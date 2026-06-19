/// PTY session management
use super::process::{PtyError, PtyProcess};
use nix::unistd::Pid;

pub struct PtySession {
    /// The underlying PTY process
    process: PtyProcess,

    /// Terminal dimensions
    cols: u16,
    rows: u16,
}

impl PtySession {
    /// Create a new PTY session
    pub fn new(command: &[String], cols: u16, rows: u16) -> Result<Self, PtyError> {
        let process = PtyProcess::spawn(command, cols, rows)?;

        Ok(Self {
            process,
            cols,
            rows,
        })
    }

    /// Get the master file descriptor for reading/writing
    pub fn master_fd(&self) -> i32 {
        self.process.master_fd
    }

    /// Get the child process ID
    pub fn child_pid(&self) -> Pid {
        self.process.child_pid
    }

    /// Resize the terminal
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), PtyError> {
        self.process.resize(cols, rows)?;
        self.cols = cols;
        self.rows = rows;
        Ok(())
    }

    /// Get current terminal dimensions
    pub fn dimensions(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_pty_session_new() {
        let session = PtySession::new(&["true".to_string()], 80, 24);
        assert!(session.is_ok());
        let session = session.unwrap();
        assert_eq!(session.dimensions(), (80, 24));
        assert!(session.master_fd() >= 0);
        assert!(session.child_pid().as_raw() > 0);
    }

    #[test]
    fn test_pty_session_new_with_dimensions() {
        let session = PtySession::new(&["true".to_string()], 120, 40).unwrap();
        assert_eq!(session.dimensions(), (120, 40));
    }

    #[test]
    fn test_pty_session_resize() {
        let mut session =
            PtySession::new(&["sleep".to_string(), "0.5".to_string()], 80, 24).unwrap();
        assert_eq!(session.dimensions(), (80, 24));

        let result = session.resize(120, 40);
        assert!(result.is_ok());
        assert_eq!(session.dimensions(), (120, 40));
    }

    #[test]
    fn test_pty_session_resize_updates_dimensions() {
        let mut session =
            PtySession::new(&["sleep".to_string(), "0.5".to_string()], 80, 24).unwrap();

        session.resize(100, 50).unwrap();
        assert_eq!(session.dimensions(), (100, 50));

        session.resize(200, 60).unwrap();
        assert_eq!(session.dimensions(), (200, 60));
    }

    #[test]
    fn test_pty_session_master_fd_valid() {
        let session = PtySession::new(&["true".to_string()], 80, 24).unwrap();
        let fd = session.master_fd();
        assert!(fd >= 0);
    }

    #[test]
    fn test_pty_session_child_pid_valid() {
        let session = PtySession::new(&["true".to_string()], 80, 24).unwrap();
        let pid = session.child_pid();
        assert!(pid.as_raw() > 0);
    }

    #[test]
    fn test_pty_session_drop_runs_without_panic() {
        // Verify that Drop impl runs without panicking.
        // The actual cleanup (SIGHUP, SIGKILL, reaper thread) is tested
        // indirectly through the session manager tests.
        let session = PtySession::new(&["true".to_string()], 80, 24).unwrap();
        // Drop should run without panic
        drop(session);
    }
}
