//! Authentication orchestration: the high-level verification pipeline shared by
//! the CLI and PAM module, and the systemd-logind session queries that gate it.

pub mod session;
pub mod verify;
