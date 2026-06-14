//! Error types for the xinchao core.
//!
//! Every fallible operation returns [`Result`]. The PAM layer maps any [`enum@Error`]
//! to an authentication failure, so the variants exist to aid logging and
//! diagnostics, never to widen what counts as success.

use thiserror::Error;

// Data Structures

/// All failures the core can surface.
///
/// The set is deliberately small for now and grows as capture, recognition, and
/// enrollment land in later milestones.
#[derive(Debug, Error)]
pub enum Error {
    /// No usable IR camera could be opened.
    #[error("camera error: {0}")]
    Camera(String),

    /// A configuration file or value was missing or invalid.
    #[error("configuration error: {0}")]
    Config(String),

    /// A model file could not be fetched, verified, or loaded.
    #[error("model error: {0}")]
    Model(String),

    /// A PAM service drop-in could not be read or modified.
    #[error("pam configuration error: {0}")]
    Pam(String),

    /// A face could not be recognized with enough confidence.
    #[error("recognition failed: {0}")]
    Recognition(String),

    /// An enrollment store could not be read, written, or parsed.
    #[error("enrollment store error: {0}")]
    Store(String),
}

/// Convenience alias used throughout the core.
pub type Result<T> = std::result::Result<T, Error>;

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_displays_its_context() {
        let err = Error::Config("missing device".to_string());
        assert_eq!(err.to_string(), "configuration error: missing device");
    }
}
