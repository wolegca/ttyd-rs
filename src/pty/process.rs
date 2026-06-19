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

        // Set FD_CLOEXEC on both PTY fds so they are closed on exec
        Self::set_cloexec(master_fd);
        Self::set_cloexec(slave_fd);

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

        // slave_fd is now duplicated onto fd 0/1/2, close the original
        drop(slave_owned);

        // Close all inherited file descriptors above stderr to prevent the
        // child from holding references to the parent's sockets, PTY masters,
        // tokio runtime internals, etc.
        Self::close_fds_above(libc::STDERR_FILENO);

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

    /// Set `FD_CLOEXEC` on a file descriptor so it is closed on `exec`.
    fn set_cloexec(fd: RawFd) {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags >= 0 {
            let _ = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
        }
    }

    /// Close all file descriptors greater than `min_fd`.
    ///
    /// Uses `close_range(2)` on Linux ≥ 5.9, falls back to iterating
    /// `/proc/self/fd` on older kernels, and `fcntl(F_MAXFD)` on macOS.
    fn close_fds_above(min_fd: RawFd) {
        let first = (min_fd + 1) as libc::c_uint;

        // Fast path: close_range(2) — atomic, no race with other threads
        #[cfg(target_os = "linux")]
        {
            // close_range(first, ~0u, 0) — close all fds >= first
            if unsafe { libc::close_range(first, libc::c_uint::MAX, 0) } == 0 {
                return;
            }
        }

        // Fallback: enumerate open fds via /proc/self/fd (Linux)
        #[cfg(target_os = "linux")]
        {
            if let Ok(dir) = std::fs::read_dir("/proc/self/fd") {
                for entry in dir.flatten() {
                    if let Ok(fd) = entry.file_name().to_string_lossy().parse::<RawFd>()
                        && fd > min_fd
                    {
                        let _ = close(fd);
                    }
                }
            }
        }

        // Fallback for macOS: use fcntl(F_MAXFD) to find the highest open fd
        #[cfg(target_os = "macos")]
        {
            let max_fd = unsafe { libc::fcntl(0, libc::F_MAXFD) };
            if max_fd >= 0 {
                for fd in (min_fd + 1)..=max_fd {
                    let _ = close(fd);
                }
            }
        }
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

        // Send SIGHUP first — interactive shells use this to run cleanup
        // (history flush, EXIT trap) before exiting.  SIGKILL follows as
        // a fallback in case the child ignores SIGHUP.
        let _ = nix::sys::signal::kill(self.child_pid, nix::sys::signal::Signal::SIGHUP);
        let _ = nix::sys::signal::kill(self.child_pid, nix::sys::signal::Signal::SIGKILL);

        // First try a non-blocking reap in case the child has already exited.
        match nix::sys::wait::waitpid(self.child_pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG)) {
            Ok(_) => return,
            Err(nix::errno::Errno::ECHILD) => return, // already reaped
            _ => {}
        }

        // The child has not exited yet (signal delivery is async).
        // Spawn a detached thread that does a blocking waitpid so we
        // never leave a zombie, but also never block a tokio worker.
        let pid = self.child_pid;
        let _ = std::thread::Builder::new()
            .name("pty-reaper".into())
            .spawn(move || {
                let _ = nix::sys::wait::waitpid(pid, None);
            });
    }
}
