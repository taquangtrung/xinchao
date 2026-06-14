//! Core logic for **xinchao**, a Windows Hello-style IR facial-authentication
//! system for Linux.
//!
//! This crate holds the privilege-agnostic building blocks: configuration,
//! camera capture, face recognition, and enrollment storage. The [`pam`] module
//! (built into the cdylib as `pam_xinchao.so`) and the `xinchao` binary are thin
//! clients on top of them. See `docs/IMPLEMENTATION_PLAN.md` for the milestone
//! roadmap. Camera enumeration and capture (M1), face detection and embedding
//! (M2), per-user enrollment storage (M3), and the shared verification pipeline
//! behind the PAM module (M4) are implemented.
//!
//! # Security posture
//!
//! Everything fails closed: any [`Error`] is treated as "not authenticated" by
//! callers, never as success.

pub mod auth;
pub mod capture;
pub mod config;
pub mod error;
pub mod pam;
pub mod recognition;
pub mod store;

pub use error::{Error, Result};

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_alias_is_reexported() {
        let ok: Result<u8> = Ok(1);
        assert!(matches!(ok, Ok(1)));
    }
}
