//! Model-file acquisition with checksum pinning.
//!
//! Recognition models are not vendored into the repository (some carry
//! non-commercial licenses). Instead each model is described by a [`ModelSpec`]
//! and fetched on first use, verified against a pinned SHA-256, and cached under
//! a models directory. See `docs/IMPLEMENTATION_PLAN.md` section 0.
//!
//! # Security posture
//!
//! A model is only installed after its checksum matches the pinned value, so a
//! corrupted or substituted download cannot reach the recognition path.

use std::fs;
use std::fs::File;
use std::io;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

use sha2::Digest;
use sha2::Sha256;

use crate::error::Error;
use crate::error::Result;

// Constants

/// Read-buffer size used while hashing a file.
const READ_CHUNK: usize = 8192;

// Data Structures

/// A downloadable model file pinned to a known checksum.
#[derive(Clone, Copy, Debug)]
pub struct ModelSpec {
    /// File name to store the model under.
    pub file_name: &'static str,
    /// Human-readable license note, surfaced to the user on first download.
    pub license: &'static str,
    /// Expected lowercase-hex SHA-256 of the file.
    pub sha256: &'static str,
    /// Source URL to download from.
    pub url: &'static str,
}

// Functions

/// Returns the path to `spec`'s file under `dir`, downloading it if needed.
///
/// A present file with a matching checksum is reused. Otherwise the file is
/// downloaded to a temporary path, verified against the pinned SHA-256, and only
/// then moved into place, so a partial or corrupt download is never used.
pub fn ensure(spec: &ModelSpec, dir: &Path) -> Result<PathBuf> {
    let dest = dir.join(spec.file_name);
    if dest.is_file() && verify_sha256(&dest, spec.sha256).is_ok() {
        return Ok(dest);
    }
    fs::create_dir_all(dir)
        .map_err(|e| Error::Model(format!("cannot create {}: {e}", dir.display())))?;
    let tmp = dir.join(format!("{}.part", spec.file_name));
    download(spec.url, &tmp)?;
    if let Err(e) = verify_sha256(&tmp, spec.sha256) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    fs::rename(&tmp, &dest)
        .map_err(|e| Error::Model(format!("cannot install {}: {e}", dest.display())))?;
    Ok(dest)
}

/// Streams `url` to `dest`.
fn download(url: &str, dest: &Path) -> Result<()> {
    let mut response = ureq::get(url)
        .call()
        .map_err(|e| Error::Model(format!("download of {url} failed: {e}")))?;
    let mut reader = response.body_mut().as_reader();
    let mut file = File::create(dest)
        .map_err(|e| Error::Model(format!("cannot create {}: {e}", dest.display())))?;
    io::copy(&mut reader, &mut file)
        .map_err(|e| Error::Model(format!("writing {} failed: {e}", dest.display())))?;
    Ok(())
}

/// Verifies a file's SHA-256 matches `expected` (hex, case-insensitive).
fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let mut file = File::open(path)
        .map_err(|e| Error::Model(format!("cannot open {}: {e}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; READ_CHUNK];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| Error::Model(format!("cannot read {}: {e}", path.display())))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = hex_digest(&hasher.finalize());
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(Error::Model(format!(
            "checksum mismatch for {}: expected {expected}, got {actual}",
            path.display()
        )))
    }
}

/// Formats bytes as lowercase hex.
fn hex_digest(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

// Tests

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// SHA-256 of the ASCII bytes "abc".
    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn temp_file(name: &str, contents: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!("xinchao-models-{name}"));
        let mut file = File::create(&path).unwrap();
        file.write_all(contents).unwrap();
        path
    }

    #[test]
    fn hex_digest_is_lowercase_hex() {
        assert_eq!(hex_digest(&[0x00, 0x0f, 0xab, 0xff]), "000fabff");
    }

    #[test]
    fn verify_accepts_matching_checksum() {
        let path = temp_file("matching", b"abc");
        let result = verify_sha256(&path, ABC_SHA256);
        let _ = fs::remove_file(&path);
        assert!(result.is_ok());
    }

    #[test]
    fn verify_is_case_insensitive() {
        let path = temp_file("case", b"abc");
        let result = verify_sha256(&path, &ABC_SHA256.to_uppercase());
        let _ = fs::remove_file(&path);
        assert!(result.is_ok());
    }

    #[test]
    fn verify_rejects_mismatched_checksum() {
        let path = temp_file("mismatch", b"abc");
        let result = verify_sha256(&path, &"0".repeat(64));
        let _ = fs::remove_file(&path);
        assert!(result.is_err());
    }
}
