//! Per-user enrollment storage in a versioned binary format.
//!
//! An [`Enrollment`] holds the face embeddings captured for one user. It is
//! persisted as `<dir>/<user>.dat` in a compact, self-describing binary layout
//! (magic + version + dimension + count + little-endian `f32` values), so a file
//! from a future format or a different embedding size is rejected rather than
//! misread. See `docs/IMPLEMENTATION_PLAN.md` section 4 (`store`).
//!
//! # Security posture
//!
//! Enrolled embeddings are biometric-derived data the PAM module trusts during
//! authentication. [`load_secure`] refuses a file that is not root-owned or is
//! world-writable, mirroring [`crate::config::load_secure`]. Parsing is fully
//! bounds-checked and fails closed: any malformed header, length mismatch, or
//! arithmetic overflow yields an [`Error::Store`], never a partial enrollment.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;

use crate::error::Error;
use crate::error::Result;
use crate::recognition::recognize::Embedding;

// Constants

/// Canonical directory for per-user enrollment files.
pub const DEFAULT_DIR: &str = "/etc/xinchao/models";

/// File-name suffix for an enrollment file.
const FILE_EXT: &str = ".dat";

/// Magic marker at the start of every enrollment file ("XinChao ENrollment").
const MAGIC: &[u8; 4] = b"XCEN";

/// Current on-disk format version.
const FORMAT_VERSION: u8 = 1;

/// Fixed header size: magic (4) + version (1) + dim (4) + count (4).
const HEADER_LEN: usize = 13;

/// Mode enrollment files are written with: owner read/write, group/other read.
const FILE_MODE: u32 = 0o644;

/// uid that must own a security-sensitive file.
const ROOT_UID: u32 = 0;

/// Permission bit that, if set, makes a file world-writable.
const WORLD_WRITABLE: u32 = 0o002;

// Data Structures

/// The set of face embeddings enrolled for one user.
///
/// Constructed via [`Enrollment::new`], which guarantees the invariant the rest
/// of the system relies on: at least one embedding, all of the same dimension.
#[derive(Clone, Debug, PartialEq)]
pub struct Enrollment {
    embeddings: Vec<Embedding>,
}

// Functions

/// Returns the file path for `user`'s enrollment under `dir`.
///
/// Rejects a user name that could escape `dir` (empty, `.`/`..`, or containing a
/// path separator or NUL), so a crafted name cannot redirect a read or write.
pub fn user_path(dir: &Path, user: &str) -> Result<PathBuf> {
    validate_user(user)?;
    Ok(dir.join(format!("{user}{FILE_EXT}")))
}

/// Writes `enrollment` to `<dir>/<user>.dat`, creating `dir` if needed.
///
/// The file is written to a temporary path, given [`FILE_MODE`], and then renamed
/// into place, so a concurrent reader never sees a half-written enrollment.
pub fn save(dir: &Path, user: &str, enrollment: &Enrollment) -> Result<PathBuf> {
    let dest = user_path(dir, user)?;
    fs::create_dir_all(dir)
        .map_err(|e| Error::Store(format!("cannot create {}: {e}", dir.display())))?;
    let tmp = dir.join(format!("{user}{FILE_EXT}.tmp"));
    fs::write(&tmp, enrollment.to_bytes())
        .map_err(|e| Error::Store(format!("cannot write {}: {e}", tmp.display())))?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(FILE_MODE))
        .map_err(|e| Error::Store(format!("cannot set mode on {}: {e}", tmp.display())))?;
    fs::rename(&tmp, &dest)
        .map_err(|e| Error::Store(format!("cannot install {}: {e}", dest.display())))?;
    Ok(dest)
}

/// Loads `user`'s enrollment from `dir`, without permission checks.
///
/// Use this for human-facing tooling; the privileged PAM path uses
/// [`load_secure`] instead.
pub fn load(dir: &Path, user: &str) -> Result<Enrollment> {
    let path = user_path(dir, user)?;
    let bytes = fs::read(&path)
        .map_err(|e| Error::Store(format!("cannot read {}: {e}", path.display())))?;
    Enrollment::from_bytes(&bytes)
}

/// Loads `user`'s enrollment only if its file is root-owned and not world-writable.
///
/// This is the entry point for the PAM module, where a tampered enrollment would
/// be an authentication-bypass vector.
pub fn load_secure(dir: &Path, user: &str) -> Result<Enrollment> {
    let path = user_path(dir, user)?;
    check_permissions(&path)?;
    load(dir, user)
}

/// Lists the user names that have an enrollment file in `dir`, sorted.
///
/// A missing directory is treated as "no enrollments" rather than an error.
pub fn list(dir: &Path) -> Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::Store(format!("cannot list {}: {e}", dir.display()))),
    };
    let mut users = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|e| Error::Store(format!("cannot read {}: {e}", dir.display())))?;
        let name = entry.file_name();
        if let Some(user) = name.to_string_lossy().strip_suffix(FILE_EXT) {
            users.push(user.to_string());
        }
    }
    users.sort();
    Ok(users)
}

/// Removes `user`'s enrollment from `dir`, returning whether a file existed.
pub fn remove(dir: &Path, user: &str) -> Result<bool> {
    let path = user_path(dir, user)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(Error::Store(format!(
            "cannot remove {}: {e}",
            path.display()
        ))),
    }
}

/// Verifies a security-sensitive file is root-owned and not world-writable.
pub fn check_permissions(path: &Path) -> Result<()> {
    let meta = fs::metadata(path)
        .map_err(|e| Error::Store(format!("cannot stat {}: {e}", path.display())))?;
    if meta.uid() != ROOT_UID {
        return Err(Error::Store(format!(
            "{} must be owned by root",
            path.display()
        )));
    }
    if meta.mode() & WORLD_WRITABLE != 0 {
        return Err(Error::Store(format!(
            "{} must not be world-writable",
            path.display()
        )));
    }
    Ok(())
}

/// Rejects a user name that is empty, a traversal token, or holds a path separator.
fn validate_user(user: &str) -> Result<()> {
    let rejected =
        user.is_empty() || user == "." || user == ".." || user.contains('/') || user.contains('\0');
    if rejected {
        return Err(Error::Store(format!("invalid user name: {user:?}")));
    }
    Ok(())
}

impl Enrollment {
    /// Builds an enrollment, requiring at least one embedding of uniform dimension.
    pub fn new(embeddings: Vec<Embedding>) -> Result<Self> {
        let dim = match embeddings.first() {
            Some(first) => first.dim(),
            None => {
                return Err(Error::Store(
                    "enrollment needs at least one embedding".to_string(),
                ))
            }
        };
        if dim == 0 {
            return Err(Error::Store(
                "embedding dimension must be non-zero".to_string(),
            ));
        }
        if embeddings.iter().any(|e| e.dim() != dim) {
            return Err(Error::Store(
                "all enrolled embeddings must share one dimension".to_string(),
            ));
        }
        Ok(Enrollment { embeddings })
    }

    /// The enrolled embeddings.
    pub fn embeddings(&self) -> &[Embedding] {
        &self.embeddings
    }

    /// The shared dimension of every enrolled embedding.
    pub fn dim(&self) -> usize {
        self.embeddings[0].dim()
    }

    /// Serializes the enrollment to its versioned binary representation.
    fn to_bytes(&self) -> Vec<u8> {
        let dim = self.dim();
        let count = self.embeddings.len();
        let mut out = Vec::with_capacity(HEADER_LEN + count * dim * 4);
        out.extend_from_slice(MAGIC);
        out.push(FORMAT_VERSION);
        out.extend_from_slice(&(dim as u32).to_le_bytes());
        out.extend_from_slice(&(count as u32).to_le_bytes());
        for embedding in &self.embeddings {
            for value in embedding.as_slice() {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        out
    }

    /// Parses an enrollment from its versioned binary representation.
    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < HEADER_LEN {
            return Err(Error::Store("enrollment file is truncated".to_string()));
        }
        if &bytes[0..4] != MAGIC {
            return Err(Error::Store("not a xinchao enrollment file".to_string()));
        }
        let version = bytes[4];
        if version != FORMAT_VERSION {
            return Err(Error::Store(format!(
                "unsupported enrollment version {version}"
            )));
        }
        let dim = u32::from_le_bytes(bytes[5..9].try_into().unwrap()) as usize;
        let count = u32::from_le_bytes(bytes[9..13].try_into().unwrap()) as usize;
        let payload = count
            .checked_mul(dim)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| Error::Store("enrollment dimensions overflow".to_string()))?;
        let expected = HEADER_LEN
            .checked_add(payload)
            .ok_or_else(|| Error::Store("enrollment dimensions overflow".to_string()))?;
        if bytes.len() != expected {
            return Err(Error::Store(format!(
                "enrollment length {} does not match {count} x {dim} embeddings",
                bytes.len()
            )));
        }
        let mut embeddings = Vec::with_capacity(count);
        let mut offset = HEADER_LEN;
        for _ in 0..count {
            let mut values = Vec::with_capacity(dim);
            for _ in 0..dim {
                let chunk = bytes[offset..offset + 4].try_into().unwrap();
                values.push(f32::from_le_bytes(chunk));
                offset += 4;
            }
            embeddings.push(Embedding::new(values));
        }
        Enrollment::new(embeddings)
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Enrollment {
        Enrollment::new(vec![
            Embedding::new(vec![1.0, 2.0, 3.0]),
            Embedding::new(vec![-0.5, 0.0, 0.25]),
        ])
        .unwrap()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xinchao-store-{name}"));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn default_dir_is_the_canonical_path() {
        assert_eq!(DEFAULT_DIR, "/etc/xinchao/models");
    }

    #[test]
    fn new_rejects_empty() {
        assert!(Enrollment::new(Vec::new()).is_err());
    }

    #[test]
    fn new_rejects_mixed_dimensions() {
        let mixed = vec![Embedding::new(vec![1.0, 2.0]), Embedding::new(vec![1.0])];
        assert!(Enrollment::new(mixed).is_err());
    }

    #[test]
    fn round_trips_through_bytes() {
        let enrollment = sample();
        let bytes = enrollment.to_bytes();
        assert_eq!(Enrollment::from_bytes(&bytes).unwrap(), enrollment);
    }

    #[test]
    fn from_bytes_rejects_bad_magic() {
        let mut bytes = sample().to_bytes();
        bytes[0] = b'Z';
        assert!(Enrollment::from_bytes(&bytes).is_err());
    }

    #[test]
    fn from_bytes_rejects_wrong_version() {
        let mut bytes = sample().to_bytes();
        bytes[4] = FORMAT_VERSION + 1;
        assert!(Enrollment::from_bytes(&bytes).is_err());
    }

    #[test]
    fn from_bytes_rejects_truncation() {
        let bytes = sample().to_bytes();
        assert!(Enrollment::from_bytes(&bytes[..bytes.len() - 1]).is_err());
        assert!(Enrollment::from_bytes(&bytes[..5]).is_err());
    }

    #[test]
    fn validate_user_rejects_traversal() {
        assert!(validate_user("").is_err());
        assert!(validate_user(".").is_err());
        assert!(validate_user("..").is_err());
        assert!(validate_user("../root").is_err());
        assert!(validate_user("a/b").is_err());
        assert!(validate_user("alice").is_ok());
    }

    #[test]
    fn save_load_list_remove_round_trip() {
        let dir = temp_dir("round-trip");
        let enrollment = sample();
        save(&dir, "alice", &enrollment).unwrap();

        assert_eq!(load(&dir, "alice").unwrap(), enrollment);
        assert_eq!(list(&dir).unwrap(), vec!["alice".to_string()]);

        assert!(remove(&dir, "alice").unwrap());
        assert!(!remove(&dir, "alice").unwrap());
        assert!(list(&dir).unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_of_missing_dir_is_empty() {
        let dir = temp_dir("missing");
        assert!(list(&dir).unwrap().is_empty());
    }

    #[test]
    fn saved_file_is_not_world_writable() {
        let dir = temp_dir("perms");
        let path = save(&dir, "bob", &sample()).unwrap();
        let mode = fs::metadata(&path).unwrap().mode();
        assert_eq!(mode & WORLD_WRITABLE, 0);
        let _ = fs::remove_dir_all(&dir);
    }
}
