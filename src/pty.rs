/// PTY (Pseudo-Terminal) management module
pub(crate) mod process;
mod session;

pub use process::PtyError;
pub use session::PtySession;
