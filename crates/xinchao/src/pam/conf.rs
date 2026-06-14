//! Enabling and disabling face unlock in a PAM service's drop-in.
//!
//! Wiring face auth into a service means adding one line to its
//! `/etc/pam.d/<service>` stack:
//!
//! ```text
//! auth    sufficient    pam_xinchao.so
//! ```
//!
//! This module only ever **adds** that line at the top of the auth stack and
//! removes exactly that line again; it never touches the rest of the file, so the
//! password fallback always remains. `sufficient` (not `required`) means a failed
//! or missing module simply falls through to the next line, so a bug here cannot
//! lock anyone out. Before any change the original file is backed up.
//!
//! # Security posture
//!
//! Only an allowlist of services may be modified ([`KNOWN_SERVICES`]). Writes are
//! atomic (temp + rename) and the original is preserved as a `.xinchao-bak`
//! sibling, so a change is always reversible. Login screens are included, but if
//! one ever refuses your password, recover from a text console
//! (`Ctrl`+`Alt`+`F3`) and disable it or restore the backup. The bare text-console
//! `login` and the screen-locker-less display path are deliberately omitted.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;

use crate::error::Error;
use crate::error::Result;

// Constants

/// Directory holding per-service PAM stacks.
const PAM_DIR: &str = "/etc/pam.d";

/// The line inserted to enable face unlock.
const PAM_LINE: &str = "auth    sufficient    pam_xinchao.so";

/// Token identifying our module in a PAM line.
const MODULE: &str = "pam_xinchao.so";

/// Mode PAM drop-ins are written with.
const FILE_MODE: u32 = 0o644;

// Data Structures

/// A PAM service face unlock may be toggled for.
#[derive(Clone, Copy, Debug)]
pub struct Service {
    /// Human-readable description of what enabling it covers.
    pub label: &'static str,
    /// The `/etc/pam.d` file name.
    pub name: &'static str,
}

// Constants (continued)

/// Every service this module is allowed to modify. Only those whose file exists
/// are offered (see [`available`]). The bare console `login` is intentionally
/// absent, as a mistake there is the hardest to recover from.
pub const KNOWN_SERVICES: [Service; 5] = [
    Service {
        label: "Login + lock screen (LightDM)",
        name: "lightdm",
    },
    Service {
        label: "Login + lock screen (GDM)",
        name: "gdm-password",
    },
    Service {
        label: "Login + lock screen (SDDM)",
        name: "sddm",
    },
    Service {
        label: "sudo (terminal)",
        name: "sudo",
    },
    Service {
        label: "Desktop admin dialogs (polkit)",
        name: "polkit-1",
    },
];

// Functions

/// The known services whose PAM file exists on this system.
pub fn available() -> Vec<Service> {
    KNOWN_SERVICES
        .iter()
        .copied()
        .filter(|service| Path::new(PAM_DIR).join(service.name).exists())
        .collect()
}

/// Reports whether face unlock is currently enabled for `service`.
///
/// A service whose file is missing is reported as disabled rather than an error.
pub fn status(service: &str) -> Result<bool> {
    let path = service_path(service)?;
    match fs::read_to_string(&path) {
        Ok(content) => Ok(has_line(&content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(Error::Pam(format!(
            "cannot read {}: {error}",
            path.display()
        ))),
    }
}

/// Enables face unlock for `service`, returning whether the file changed.
///
/// Backs up the original first; a no-op if the line is already present.
pub fn enable(service: &str) -> Result<bool> {
    let path = service_path(service)?;
    let content = read_service(&path)?;
    if has_line(&content) {
        return Ok(false);
    }
    backup(&path, &content)?;
    write_atomic(&path, &with_line(&content))?;
    Ok(true)
}

/// Disables face unlock for `service`, returning whether the file changed.
pub fn disable(service: &str) -> Result<bool> {
    let path = service_path(service)?;
    let content = read_service(&path)?;
    if !has_line(&content) {
        return Ok(false);
    }
    backup(&path, &content)?;
    write_atomic(&path, &without_line(&content))?;
    Ok(true)
}

/// Resolves the path for an allowed `service`, rejecting anything else.
fn service_path(service: &str) -> Result<PathBuf> {
    if !KNOWN_SERVICES.iter().any(|known| known.name == service) {
        return Err(Error::Pam(format!("unsupported service {service:?}")));
    }
    Ok(Path::new(PAM_DIR).join(service))
}

/// Reads a service file, mapping a missing file to a clear error.
fn read_service(path: &Path) -> Result<String> {
    fs::read_to_string(path)
        .map_err(|error| Error::Pam(format!("cannot read {}: {error}", path.display())))
}

/// Preserves the original file as a `.xinchao-bak` sibling, once.
fn backup(path: &Path, content: &str) -> Result<()> {
    let backup = backup_path(path);
    if backup.exists() {
        return Ok(());
    }
    fs::write(&backup, content)
        .map_err(|error| Error::Pam(format!("cannot write backup {}: {error}", backup.display())))
}

/// Writes `content` to `path` atomically, with [`FILE_MODE`] permissions.
fn write_atomic(path: &Path, content: &str) -> Result<()> {
    let tmp = PathBuf::from(format!("{}.xinchao-tmp", path.display()));
    fs::write(&tmp, content)
        .map_err(|error| Error::Pam(format!("cannot write {}: {error}", tmp.display())))?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(FILE_MODE))
        .map_err(|error| Error::Pam(format!("cannot set mode on {}: {error}", tmp.display())))?;
    fs::rename(&tmp, path)
        .map_err(|error| Error::Pam(format!("cannot install {}: {error}", path.display())))
}

/// The `.xinchao-bak` backup path for a service file.
fn backup_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.xinchao-bak", path.display()))
}

/// Whether an enabling line is present (a non-comment line naming the module).
fn has_line(content: &str) -> bool {
    content.lines().any(is_our_line)
}

/// Whether `line` is our active (non-comment) PAM line.
fn is_our_line(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.starts_with('#') && trimmed.contains(MODULE)
}

/// Returns `content` with the enabling line inserted before the first directive.
fn with_line(content: &str) -> String {
    if has_line(content) {
        return ensure_trailing_newline(content);
    }
    let mut out = Vec::new();
    let mut inserted = false;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if !inserted && !trimmed.is_empty() && !trimmed.starts_with('#') {
            out.push(PAM_LINE.to_string());
            inserted = true;
        }
        out.push(line.to_string());
    }
    if !inserted {
        out.push(PAM_LINE.to_string());
    }
    let mut result = out.join("\n");
    result.push('\n');
    result
}

/// Returns `content` with our active line(s) removed.
fn without_line(content: &str) -> String {
    let kept: Vec<&str> = content.lines().filter(|line| !is_our_line(line)).collect();
    let mut result = kept.join("\n");
    if !result.is_empty() {
        result.push('\n');
    }
    result
}

/// Ensures `content` ends in exactly one newline.
fn ensure_trailing_newline(content: &str) -> String {
    let mut result = content.trim_end_matches('\n').to_string();
    result.push('\n');
    result
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    const SUDO: &str = "#%PAM-1.0\n@include common-auth\n@include common-account\n";

    #[test]
    fn only_known_services_are_allowed() {
        assert!(service_path("sudo").is_ok());
        assert!(service_path("lightdm").is_ok());
        assert!(service_path("login").is_err());
        assert!(service_path("../etc/shadow").is_err());
    }

    #[test]
    fn detects_active_line_but_not_comment() {
        assert!(!has_line(SUDO));
        assert!(has_line("auth sufficient pam_xinchao.so\n"));
        assert!(!has_line("# auth sufficient pam_xinchao.so\n"));
    }

    #[test]
    fn enable_inserts_before_first_directive() {
        let enabled = with_line(SUDO);
        let lines: Vec<&str> = enabled.lines().collect();
        assert_eq!(lines[0], "#%PAM-1.0");
        assert_eq!(lines[1], PAM_LINE);
        assert_eq!(lines[2], "@include common-auth");
        assert!(enabled.ends_with('\n'));
    }

    #[test]
    fn enable_keeps_the_rest_of_the_stack() {
        let enabled = with_line(SUDO);
        assert!(enabled.contains("@include common-auth"));
        assert!(enabled.contains("@include common-account"));
    }

    #[test]
    fn enable_is_idempotent() {
        let once = with_line(SUDO);
        let twice = with_line(&once);
        assert_eq!(once, twice);
        assert_eq!(once.matches(MODULE).count(), 1);
    }

    #[test]
    fn enable_then_disable_restores_original() {
        let enabled = with_line(SUDO);
        let disabled = without_line(&enabled);
        assert_eq!(disabled, SUDO);
    }

    #[test]
    fn disable_without_line_is_noop() {
        assert_eq!(without_line(SUDO), SUDO);
    }

    #[test]
    fn enable_handles_comment_only_file() {
        let enabled = with_line("#%PAM-1.0\n");
        assert!(has_line(&enabled));
        assert!(enabled.ends_with('\n'));
    }
}
