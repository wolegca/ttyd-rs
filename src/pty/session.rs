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
    #[allow(dead_code)]
    pub fn dimensions(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }
}
