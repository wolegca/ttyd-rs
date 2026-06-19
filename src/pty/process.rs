/// PTY process management using nix
use nix::pty::{Winsize, openpty};
use nix::unistd::{ForkResult, Pid, close, execvp, fork, setsid};
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

        // Set FD_CLOEXEC on the master fd only.
        // The slave fd must NOT have FD_CLOEXEC here: the child needs it open
        // until after dup2, and we close it manually in setup_child.
        Self::set_cloexec(master_fd);

        debug!("PTY opened: master_fd={}, slave_fd={}", master_fd, slave_fd);

        // Fork the process
        match unsafe { fork() } {
            Ok(ForkResult::Parent { child }) => {
                // Parent process: take ownership of master_fd to prevent double-close
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
                // Child process — pty will be dropped but exec replaces the process image anyway.
                Self::setup_child(slave_fd, command)?;
                // setup_child calls execvp which never returns on success.
                unreachable!("execvp should not return");
            }
            Err(e) => {
                // On error, let pty.master and pty.slave drop naturally to close fds
                Err(PtyError::ForkFailed(e.to_string()))
            }
        }
    }

    /// Setup the child process to use the PTY slave.
    fn setup_child(slave_fd: RawFd, command: &[String]) -> Result<(), PtyError> {
        // Create a new session so this process becomes a session leader with no
        // controlling terminal yet.
        setsid().map_err(|e| PtyError::ExecFailed(format!("setsid failed: {}", e)))?;

        // Make the slave PTY the controlling terminal for this session.
        // This is required for login(1) and any program that opens /dev/tty or
        // calls tcgetpgrp(). Without it, login exits silently on Debian 13.
        // SAFETY: slave_fd is a valid PTY slave fd obtained from openpty().
        if unsafe { libc::ioctl(slave_fd, libc::TIOCSCTTY as _, 0i32) } < 0 {
            return Err(PtyError::ExecFailed(format!(
                "TIOCSCTTY failed: {}",
                std::io::Error::last_os_error()
            )));
        }

        // Set TERM environment variable so programs know we're a capable terminal.
        // Without this, systemd-launched processes have no TERM and colors are disabled.
        // SAFETY: we are in a forked child before exec, so no threads to race.
        unsafe {
            libc::setenv(
                CString::new("TERM")
                    .map_err(|e| PtyError::ExecFailed(format!("Invalid TERM name: {}", e)))?
                    .as_ptr(),
                CString::new("xterm-256color")
                    .map_err(|e| PtyError::ExecFailed(format!("Invalid TERM value: {}", e)))?
                    .as_ptr(),
                1, // overwrite = true
            );
        }

        // Redirect stdin, stdout, stderr to the slave PTY using libc::dup2
        // directly. Wrapping fd 0/1/2 in OwnedFd before the dup2 calls creates
        // aliased ownership: drop() would close the real stdin/stdout/stderr
        // after exec replaces the image, causing subtle fd leaks or double-closes
        // depending on nix version behaviour.
        for dst in [0i32, 1, 2] {
            if unsafe { libc::dup2(slave_fd, dst) } < 0 {
                return Err(PtyError::ExecFailed(format!(
                    "dup2({} -> {}) failed: {}",
                    slave_fd,
                    dst,
                    std::io::Error::last_os_error()
                )));
            }
        }

        // Close the original slave fd now that it has been duplicated onto 0/1/2.
        // Skip if slave_fd happens to be one of 0/1/2 (unlikely but possible).
        if slave_fd > 2 {
            unsafe { libc::close(slave_fd) };
        }

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

        // Execute the command — never returns on success
        execvp(&program, &args).map_err(|e| PtyError::ExecFailed(e.to_string()))?;

        Ok(())
    }

    /// Set `FD_CLOEXEC` on a file descriptor so it is closed on `exec`.
    fn set_cloexec(fd: RawFd) {
        // SAFETY: fcntl is a POSIX syscall; fd is valid (from openpty).
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags >= 0 {
            let rc = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
            if rc < 0 {
                debug!(
                    "Failed to set FD_CLOEXEC on fd {}: {}",
                    fd,
                    std::io::Error::last_os_error()
                );
            }
        }
    }

    /// Close all file descriptors greater than `min_fd`.
    ///
    /// Uses `close_range(2)` on Linux ≥ 5.9, falls back to iterating
    /// `/proc/self/fd` on older kernels.
    fn close_fds_above(min_fd: RawFd) {
        let first = (min_fd + 1) as libc::c_uint;

        // Fast path: close_range(2) — atomic, no race with other threads
        // close_range(first, ~0u, 0) — close all fds >= first
        if unsafe { libc::close_range(first, libc::c_uint::MAX, 0) } == 0 {
            return;
        }

        // Fallback: enumerate open fds via /proc/self/fd
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_spawn_empty_command_errors() {
        let result = PtyProcess::spawn(&[], 80, 24);
        assert!(result.is_err());
        let err = result.err().unwrap();
        let msg = err.to_string();
        assert!(msg.contains("empty"), "Expected 'empty' in error: {}", msg);
    }

    #[test]
    fn test_spawn_valid_command() {
        let result = PtyProcess::spawn(&["true".to_string()], 80, 24);
        assert!(result.is_ok());
        let proc = result.unwrap();
        assert!(proc.master_fd >= 0);
        // child_pid should be positive
        assert!(proc.child_pid.as_raw() > 0);
    }

    #[test]
    fn test_spawn_sets_dimensions() {
        let proc = PtyProcess::spawn(&["sleep".to_string(), "0.1".to_string()], 120, 40).unwrap();
        // Just verify it succeeds — dimensions are set via openpty
        assert!(proc.master_fd >= 0);
    }

    #[test]
    fn test_resize() {
        let proc = PtyProcess::spawn(&["sleep".to_string(), "0.5".to_string()], 80, 24).unwrap();
        let result = proc.resize(120, 40);
        assert!(result.is_ok());
    }

    #[test]
    fn test_resize_invalid_fd() {
        // Use an invalid fd to trigger an error
        let proc = PtyProcess {
            master_fd: -1,
            child_pid: nix::unistd::Pid::from_raw(1),
        };
        let result = proc.resize(80, 24);
        assert!(result.is_err());
    }

    #[test]
    fn test_drop_runs_without_panic() {
        // Verify that Drop impl runs without panicking.
        // The actual cleanup (SIGHUP, SIGKILL, reaper thread) is tested
        // indirectly through the session tests.
        let proc = PtyProcess::spawn(&["true".to_string()], 80, 24).unwrap();
        // Drop should run without panic
        drop(proc);
    }

    #[test]
    fn test_spawn_and_drop_multiple() {
        // Verify that spawning and dropping multiple processes works correctly
        for _ in 0..3 {
            let proc = PtyProcess::spawn(&["true".to_string()], 80, 24).unwrap();
            assert!(proc.master_fd >= 0);
        }
    }

    #[test]
    fn test_spawn_invalid_command() {
        let result = PtyProcess::spawn(&["/nonexistent/binary".to_string()], 80, 24);
        // The child process will fail to exec, parent gets an Ok because fork succeeded
        // The child's exec failure happens after fork returns to parent
        // So spawn should succeed (parent side), but the child will exit with error
        assert!(result.is_ok());
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
        // (history flush, EXIT trap) before exiting.  Poll for up to 500 ms
        // to let the child exit cleanly, then fall back to SIGKILL.
        let _ = nix::sys::signal::kill(self.child_pid, nix::sys::signal::Signal::SIGHUP);

        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while std::time::Instant::now() < deadline {
            match nix::sys::wait::waitpid(
                self.child_pid,
                Some(nix::sys::wait::WaitPidFlag::WNOHANG),
            ) {
                Ok(_) | Err(nix::errno::Errno::ECHILD) => {
                    // Child exited or was already reaped — no SIGKILL needed.
                    // The outer waitpid/reaper below will be a harmless no-op.
                    return;
                }
                _ => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }

        // Child still alive after grace period — force kill.
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
