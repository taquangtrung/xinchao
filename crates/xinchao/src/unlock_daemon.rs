//! Resident IR face-unlock daemon.
//!
//! Watches systemd-logind (via `loginctl`) for the target user's graphical
//! session becoming locked and, on a face match, calls `loginctl unlock-session`
//! so the lock screen dismisses with no keystroke (Windows Hello-style). The
//! recognition model is loaded once into a [`Session`] and kept resident, which
//! both removes the per-unlock load latency and lets the scan start instantly.
//!
//! # Security posture
//!
//! It only ever unlocks sessions **owned by `user`** (filtered from `loginctl
//! list-sessions`) and only after a face match against **that user's** enrollment
//! (the `Session` is built for `user`). Both conditions must hold, so a stranger's
//! face cannot unlock and a match cannot unlock someone else's session. Every
//! `loginctl` or scan error is treated as "stay locked"; the door fails closed.

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use anyhow::Result;
use xinchao::auth::session;
use xinchao::auth::verify::Session;
use xinchao::config;

// Constants

/// Backoff before retrying to load the model/enrollment when the daemon starts
/// before the user has enrolled (or before models are installed).
const NOT_READY_BACKOFF: Duration = Duration::from_secs(5);

// Data Structures

/// Runtime options for the daemon, resolved from CLI flags and system defaults.
pub struct Options {
    /// Config file to read (root-owned; loaded with [`config::load_secure`]).
    pub config_path: PathBuf,
    /// How long to wait between scan attempts after a non-matching scan.
    pub retry: Duration,
    /// How often to poll the session lock state while idle.
    pub poll: Duration,
    /// Directory holding the enrollments (root-owned).
    pub store_dir: PathBuf,
    /// User whose locked graphical session to unlock on a match.
    pub user: String,
}

// Functions

/// Runs the daemon loop forever, returning only on an unrecoverable error.
///
/// The recognition [`Session`] is loaded lazily and retried on failure, so the
/// service can be enabled before the user has enrolled and will start working
/// once enrollment and models are in place, with no restart needed.
pub fn serve(opts: Options) -> Result<()> {
    log(&format!(
        "starting; user={} config={} store={}",
        opts.user,
        opts.config_path.display(),
        opts.store_dir.display(),
    ));
    let mut session: Option<Session> = None;
    let mut cooldown: HashMap<String, Instant> = HashMap::new();
    loop {
        if session.is_none() {
            match load_session(&opts) {
                Ok(loaded) => {
                    log("recognition model and enrollment loaded; ready");
                    session = Some(loaded);
                }
                Err(error) => {
                    log(&format!("not ready ({error:#}); retrying shortly"));
                    sleep(NOT_READY_BACKOFF);
                    continue;
                }
            }
        }

        let locked: Vec<String> = session::graphical_session_ids(&opts.user)
            .into_iter()
            .filter(|id| session::locked_hint(id) == Some(true))
            .collect();
        forget_unlocked(&mut cooldown, &locked);
        if locked.is_empty() {
            sleep(opts.poll);
            continue;
        }

        let scanner = session.as_mut().expect("session loaded above");
        for id in locked {
            if let Some(until) = cooldown.get(&id) {
                if Instant::now() < *until {
                    continue;
                }
            }
            match scanner.attempt() {
                Ok(outcome) if outcome.accepted => {
                    match unlock_session(&id) {
                        Ok(()) => log(&format!(
                            "unlocked session {id} (distance {:.3})",
                            outcome.best_distance
                        )),
                        Err(error) => {
                            log(&format!("matched but unlock failed for {id}: {error:#}"))
                        }
                    }
                    cooldown.remove(&id);
                }
                Ok(_) => {
                    cooldown.insert(id, Instant::now() + opts.retry);
                }
                Err(error) => {
                    log(&format!("scan error on {id}: {error}"));
                    cooldown.insert(id, Instant::now() + opts.retry);
                }
            }
        }
        sleep(opts.poll);
    }
}

/// Loads the config and a resident recognition [`Session`] for the target user.
fn load_session(opts: &Options) -> Result<Session> {
    let config = config::load_secure(&opts.config_path)
        .with_context(|| format!("loading config {}", opts.config_path.display()))?;
    let session = Session::load(&config, &opts.store_dir, &opts.user)
        .context("loading recognition model and enrollment")?;
    Ok(session)
}

/// Asks logind to unlock `id`, which dismisses a logind-aware lock screen.
fn unlock_session(id: &str) -> Result<()> {
    let status = Command::new("loginctl")
        .arg("unlock-session")
        .arg(id)
        .status()
        .context("running loginctl unlock-session")?;
    if !status.success() {
        anyhow::bail!("loginctl unlock-session {id} exited with {status}");
    }
    Ok(())
}

/// Drops cooldown entries for sessions that are no longer locked, so a fresh lock
/// scans immediately rather than waiting out a stale backoff.
fn forget_unlocked(cooldown: &mut HashMap<String, Instant>, locked: &[String]) {
    let still: HashSet<&str> = locked.iter().map(String::as_str).collect();
    cooldown.retain(|id, _| still.contains(id.as_str()));
}

/// Emits a daemon log line to stderr, which systemd routes to the journal.
fn log(message: &str) {
    eprintln!("xinchao-unlockd: {message}");
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forgets_cooldowns_for_unlocked_sessions() {
        let now = Instant::now();
        let mut cooldown = HashMap::new();
        cooldown.insert("c1".to_string(), now);
        cooldown.insert("c2".to_string(), now);
        forget_unlocked(&mut cooldown, &["c2".to_string()]);
        assert!(!cooldown.contains_key("c1"));
        assert!(cooldown.contains_key("c2"));
    }
}
