//! PAM module for **xinchao**.
//!
//! Built as a `cdylib` named `pam_xinchao.so` and loaded by PAM-aware programs
//! (`sudo`, `login`, the screen locker). It is a thin wrapper: it reads the
//! username and config, then delegates the actual recognition to
//! [`crate::auth::verify::authenticate_user`] under a hard timeout and
//! translates the result into a PAM status code. All logic lives in the core; this
//! module only does FFI and policy mapping. See `docs/IMPLEMENTATION_PLAN.md` M4.
//!
//! # Unlock-only policy
//!
//! Face auth runs only when the user **already has a graphical session** (i.e.
//! this is unlocking a lock screen, not a cold login). At a cold login the
//! password is still required, because it is what unlocks an encrypted home
//! (fscrypt/eCryptfs) and the login keyring; succeeding on a face alone there
//! would skip that and leave the session unable to start. So with no desktop
//! session, the module defers to the password (see [`session`]).
//!
//! # Fail-closed contract
//!
//! Only a verified face match for an already-logged-in user returns
//! [`PAM_SUCCESS`]. Every other path, no active session, a missing config or
//! enrollment, no camera, a load error, a timeout, or a non-match, denies.
//! Deployed with `sufficient`, a denial simply falls through to the password
//! prompt, so a bug here cannot lock a user out or grant false access.

pub mod conf;

use std::ffi::CStr;
use std::ffi::CString;
use std::os::raw::c_char;
use std::os::raw::c_int;
use std::os::raw::c_void;
use std::path::Path;
use std::path::PathBuf;
use std::ptr;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::auth::session;
use crate::auth::verify;
use crate::config;
use crate::store;

// Constants

/// PAM: authentication succeeded.
const PAM_SUCCESS: c_int = 0;

/// PAM: authentication failed (a definite non-match or timeout).
const PAM_AUTH_ERR: c_int = 7;

/// PAM: this module cannot decide (no config, camera, enrollment, or model).
const PAM_AUTHINFO_UNAVAIL: c_int = 9;

/// Extra time allowed beyond the configured attempt budget before the worker is
/// abandoned, so a stalled capture cannot hang the calling program forever.
const HARD_TIMEOUT_SLACK: Duration = Duration::from_secs(2);

// Data Structures

/// The module's verdict, before translation to a PAM status code.
enum Verdict {
    /// A face matched within the threshold.
    Authenticated,
    /// Recognition ran and did not match (or timed out).
    Rejected,
    /// The attempt could not run (setup, hardware, or enrollment problem).
    Unavailable,
}

// Functions

/// PAM service hook invoked to authenticate a user.
///
/// # Safety
///
/// Called by libpam with C ABI pointers. `pamh` must be the live PAM handle; the
/// remaining arguments are unused.
#[no_mangle]
pub extern "C" fn pam_sm_authenticate(
    pamh: *mut c_void,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    verdict_code(authenticate(pamh))
}

/// PAM service hook for credential management; a no-op success for `auth` stacks.
///
/// # Safety
///
/// Called by libpam with C ABI pointers, all unused.
#[no_mangle]
pub extern "C" fn pam_sm_setcred(
    _pamh: *mut c_void,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    PAM_SUCCESS
}

/// Runs the whole authentication attempt and returns a [`Verdict`].
///
/// Any failure to set up or run recognition maps to [`Verdict::Unavailable`]; a
/// completed non-match or a timeout maps to [`Verdict::Rejected`].
fn authenticate(pamh: *mut c_void) -> Verdict {
    let user = match current_user(pamh) {
        Some(user) => user,
        None => {
            audit(libc::LOG_WARNING, "could not determine the target user");
            return Verdict::Unavailable;
        }
    };
    if !session::user_has_graphical_session(&user) {
        audit(
            libc::LOG_NOTICE,
            &format!("no desktop session for {user}; deferring to password (cold login)"),
        );
        return Verdict::Unavailable;
    }
    let config = match config::load_secure(Path::new(config::DEFAULT_PATH)) {
        Ok(config) => config,
        Err(error) => {
            audit(libc::LOG_WARNING, &format!("config unavailable: {error}"));
            return Verdict::Unavailable;
        }
    };

    let budget = Duration::from_secs(config.auth.timeout_secs) + HARD_TIMEOUT_SLACK;
    let store_dir = PathBuf::from(store::DEFAULT_DIR);
    let worker_user = user.clone();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = verify::authenticate_user(&config, &store_dir, &worker_user);
        let _ = sender.send(result);
    });

    match receiver.recv_timeout(budget) {
        Ok(Ok(outcome)) => {
            let summary = format!(
                "user={user} distance={:.4} frames={} matches={}",
                outcome.best_distance, outcome.frames, outcome.matches
            );
            if outcome.accepted {
                audit(libc::LOG_NOTICE, &format!("ACCEPT {summary}"));
                Verdict::Authenticated
            } else {
                audit(libc::LOG_NOTICE, &format!("REJECT {summary}"));
                Verdict::Rejected
            }
        }
        Ok(Err(error)) => {
            audit(libc::LOG_WARNING, &format!("error user={user}: {error}"));
            Verdict::Unavailable
        }
        Err(_) => {
            audit(libc::LOG_WARNING, &format!("timeout user={user}"));
            Verdict::Rejected
        }
    }
}

/// Translates a [`Verdict`] into the PAM status code to return.
fn verdict_code(verdict: Verdict) -> c_int {
    match verdict {
        Verdict::Authenticated => PAM_SUCCESS,
        Verdict::Rejected => PAM_AUTH_ERR,
        Verdict::Unavailable => PAM_AUTHINFO_UNAVAIL,
    }
}

/// Reads the target user name from the PAM handle, or `None` on failure.
fn current_user(pamh: *mut c_void) -> Option<String> {
    let mut raw: *const c_char = ptr::null();
    // SAFETY: `pamh` is the live handle libpam passed us; `raw` is a valid
    // out-pointer. libpam owns the returned string; we only borrow it to copy.
    let status = unsafe { pam_get_user(pamh, &mut raw, ptr::null()) };
    if status != PAM_SUCCESS || raw.is_null() {
        return None;
    }
    // SAFETY: on success libpam set `raw` to a valid NUL-terminated string.
    let user = unsafe { CStr::from_ptr(raw) };
    user.to_str().ok().map(str::to_string)
}

/// Writes one audit line to the system log under the `authpriv` facility.
fn audit(priority: c_int, message: &str) {
    if let Ok(text) = CString::new(message) {
        // SAFETY: a literal `%s` format with one matching C-string argument.
        unsafe {
            libc::syslog(
                libc::LOG_AUTHPRIV | priority,
                c"xinchao: %s".as_ptr(),
                text.as_ptr(),
            );
        }
    }
}

extern "C" {
    /// libpam: fetch the user being authenticated (resolved at module load time).
    fn pam_get_user(pamh: *mut c_void, user: *mut *const c_char, prompt: *const c_char) -> c_int;
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_authenticated_maps_to_success() {
        assert_eq!(verdict_code(Verdict::Authenticated), PAM_SUCCESS);
        assert_eq!(verdict_code(Verdict::Rejected), PAM_AUTH_ERR);
        assert_eq!(verdict_code(Verdict::Unavailable), PAM_AUTHINFO_UNAVAIL);
    }

    #[test]
    fn setcred_is_success() {
        let code = pam_sm_setcred(ptr::null_mut(), 0, 0, ptr::null());
        assert_eq!(code, PAM_SUCCESS);
    }
}
