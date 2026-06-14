//! Querying a user's graphical login sessions via systemd-logind.
//!
//! A user's *graphical session* is the desktop they logged into (an `x11` or
//! `wayland` session of class `user`). Two callers need this:
//!
//! * the PAM module, to tell an **unlock** (the user is already logged in, so a
//!   face match is safe) apart from a **cold login** (no session yet, where the
//!   password is still required to unlock an encrypted home or keyring), and
//! * the unlock daemon, to know which sessions a lock screen guards.
//!
//! All queries go through `loginctl`; any failure is reported as "no session",
//! so callers fail closed (treat the user as not-yet-logged-in).

use std::collections::HashMap;
use std::process::Command;

// Functions

/// Whether `user` already has at least one graphical (desktop) session.
///
/// Used as a gate: a face match should unlock an existing desktop, but must not
/// stand in for the password at a cold login, where that password is what
/// unlocks the user's encrypted home and keyring.
pub fn user_has_graphical_session(user: &str) -> bool {
    !graphical_session_ids(user).is_empty()
}

/// The ids of `user`'s graphical sessions (the ones a lock screen guards).
pub fn graphical_session_ids(user: &str) -> Vec<String> {
    let listing = match loginctl(&["list-sessions", "--no-legend"]) {
        Some(output) => output,
        None => return Vec::new(),
    };
    listing
        .lines()
        .filter_map(parse_session_row)
        .filter(|(_, owner)| *owner == user)
        .map(|(id, _)| id.to_string())
        .filter(|id| session_is_graphical(id))
        .collect()
}

/// Reads `id`'s logind `LockedHint`: `Some(true)` locked, `Some(false)` unlocked,
/// `None` if it could not be read (callers treat that as "do not act").
pub fn locked_hint(id: &str) -> Option<bool> {
    let props = show_session(id, &["LockedHint"])?;
    parse_locked(props.get("LockedHint")?)
}

/// Whether `id` is an interactive graphical session (x11/wayland), not a TTY/service.
fn session_is_graphical(id: &str) -> bool {
    let props = match show_session(id, &["Type", "Class"]) {
        Some(props) => props,
        None => return false,
    };
    let kind = props.get("Type").map(String::as_str).unwrap_or_default();
    let class = props.get("Class").map(String::as_str).unwrap_or_default();
    class == "user" && matches!(kind, "x11" | "wayland" | "mir")
}

/// Reads selected properties of a session via `loginctl show-session`.
fn show_session(id: &str, properties: &[&str]) -> Option<HashMap<String, String>> {
    let mut args = vec!["show-session", id];
    for property in properties {
        args.push("-p");
        args.push(property);
    }
    loginctl(&args).map(|output| parse_props(&output))
}

/// Runs `loginctl` with `args`, returning its stdout on success.
fn loginctl(args: &[&str]) -> Option<String> {
    let output = Command::new("loginctl").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// Parses one `loginctl list-sessions --no-legend` row into (session id, owner).
///
/// The columns are `SESSION UID USER SEAT TTY`; only the first and third matter.
fn parse_session_row(line: &str) -> Option<(&str, &str)> {
    let mut fields = line.split_whitespace();
    let id = fields.next()?;
    let _uid = fields.next()?;
    let owner = fields.next()?;
    Some((id, owner))
}

/// Parses `KEY=VALUE` lines from `loginctl show-session` into a map.
fn parse_props(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

/// Interprets a logind `LockedHint` value (`yes`/`no`).
fn parse_locked(value: &str) -> Option<bool> {
    match value {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_session_row() {
        assert_eq!(
            parse_session_row("c2  1000 trung    seat0 tty2"),
            Some(("c2", "trung"))
        );
    }

    #[test]
    fn rejects_a_short_session_row() {
        assert_eq!(parse_session_row("c2  1000"), None);
    }

    #[test]
    fn parses_show_session_properties() {
        let props = parse_props("Type=x11\nClass=user\nLockedHint=yes\n");
        assert_eq!(props.get("Type").map(String::as_str), Some("x11"));
        assert_eq!(props.get("LockedHint").map(String::as_str), Some("yes"));
    }

    #[test]
    fn parses_the_locked_hint() {
        assert_eq!(parse_locked("yes"), Some(true));
        assert_eq!(parse_locked("no"), Some(false));
        assert_eq!(parse_locked(""), None);
    }
}
