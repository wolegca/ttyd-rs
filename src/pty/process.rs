/// PTY process management using nix
use nix::pty::{Winsize, openpty};
use nix::unistd::{ForkResult, Pid, close, dup2, execvp, fork, setsid};
use std::ffi::CString;
use std::os::unix::io::{AsRawFd, IntoRawFd, RawFd};
use thiserror::Error;
use tracing::debug;

#[derive(Debug, Error)]
pub enum PtyError {
    #[error("Failed to open PTY: {0}")]
    OpenPtyFailed(String),

    #[error("Fork failed: {0}")]
    ForkFailed(String),

    #[error("Failed to execute command: {0}")]
    ExecFailed(String),

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Nix error: {0}")]
    NixError(#[from] nix::Error),
}

pub struct PtyProcess {
    /// File descriptor for the master side of the PTY
    pub master_fd: RawFd,

    /// Process ID of the child process
    pub child_pid: Pid,
}

impl PtyProcess {
    /// Create a new PTY process with the given command and window size
    pub fn spawn(command: &[String], cols: u16, rows: u16) -> Result<Self, PtyError> {
        if command.is_empty() {
            return Err(PtyError::ExecFailed("Command cannot be empty".to_string()));
        }

        let winsize = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        // Open a new PTY
        let pty =
            openpty(Some(&winsize), None).map_err(|e| PtyError::OpenPtyFailed(e.to_string()))?;

        let master_fd = pty.master.as_raw_fd();
        let slave_fd = pty.slave.as_raw_fd();

        debug!("PTY opened: master_fd={}, slave_fd={}", master_fd, slave_fd);

        // Fork the process
        match unsafe { fork() } {
            Ok(ForkResult::Parent { child }) => {
                // Parent process
                // Take ownership of master_fd to prevent double-close
                let master_fd = pty.master.into_raw_fd();

                // Explicitly drop slave to close it in parent
                drop(pty.slave);

                debug!("Forked child process: pid={}", child);

                Ok(PtyProcess {
                    master_fd,
                    child_pid: child,
                })
            }
            Ok(ForkResult::Child) => {
                // Child process - pty will be dropped but we don't care since exec replaces the process
                Self::setup_child(slave_fd, command)?;
                // setup_child will exec, so this should never be reached
                unreachable!("execvp should not return");
            }
            Err(e) => {
                // On error, let pty.master and pty.slave drop naturally to close fds
                Err(PtyError::ForkFailed(e.to_string()))
            }
        }
    }

    /// Setup the child process to use the PTY slave
    fn setup_child(slave_fd: RawFd, command: &[String]) -> Result<(), PtyError> {
        // Create a new session
        setsid().map_err(|e| PtyError::ExecFailed(format!("setsid failed: {}", e)))?;

        // Redirect stdin, stdout, stderr to the slave PTY
        use std::os::unix::io::{FromRawFd, OwnedFd};
        let slave_owned = unsafe { OwnedFd::from_raw_fd(slave_fd) };
        let mut stdin_owned = unsafe { OwnedFd::from_raw_fd(0) };
        let mut stdout_owned = unsafe { OwnedFd::from_raw_fd(1) };
        let mut stderr_owned = unsafe { OwnedFd::from_raw_fd(2) };

        dup2(&slave_owned, &mut stdin_owned)
            .map_err(|e| PtyError::ExecFailed(format!("dup2 stdin failed: {}", e)))?;
        dup2(&slave_owned, &mut stdout_owned)
            .map_err(|e| PtyError::ExecFailed(format!("dup2 stdout failed: {}", e)))?;
        dup2(&slave_owned, &mut stderr_owned)
            .map_err(|e| PtyError::ExecFailed(format!("dup2 stderr failed: {}", e)))?;

        // Don't close slave_fd since it's now owned by slave_owned

        // Convert command to CString
        let program = CString::new(command[0].as_str())
            .map_err(|e| PtyError::ExecFailed(format!("Invalid command: {}", e)))?;

        let args: Result<Vec<CString>, _> =
            command.iter().map(|s| CString::new(s.as_str())).collect();

        let args = args.map_err(|e| PtyError::ExecFailed(format!("Invalid arguments: {}", e)))?;

        // Execute the command
        execvp(&program, &args).map_err(|e| PtyError::ExecFailed(e.to_string()))?;

        Ok(())
    }

    /// Resize the PTY window
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), PtyError> {
        let winsize = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        nix::ioctl_write_ptr_bad!(tiocswinsz, libc::TIOCSWINSZ, Winsize);

        unsafe {
            tiocswinsz(self.master_fd, &winsize as *const Winsize).map_err(PtyError::NixError)?;
        }

        debug!("PTY resized to {}x{}", cols, rows);
        Ok(())
    }
}

impl Drop for PtyProcess {
    fn drop(&mut self) {
        debug!(
            "Cleaning up PTY process: pid={}, fd={}",
            self.child_pid, self.master_fd
        );

        // Close the master fd first so the child's controlling terminal hangs
        // up and its foreground process group receives SIGHUP.
        let _ = close(self.master_fd);

        // An interactive shell ignores SIGTERM, so send SIGHUP (which
        // terminates it) and follow with SIGKILL to guarantee the child exits.
        // Until the child dies its slave fd stays open and any PTY reader
        // blocks forever on read(), which previously hung shutdown.
        let _ = nix::sys::signal::kill(self.child_pid, nix::sys::signal::Signal::SIGHUP);
        let _ = nix::sys::signal::kill(self.child_pid, nix::sys::signal::Signal::SIGKILL);

        // Reap the child process to prevent zombies (blocking; the child has
        // already been killed so this returns promptly).
        let _ = nix::sys::wait::waitpid(self.child_pid, None);
    }
}
