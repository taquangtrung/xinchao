//! IR illuminator activation via UVC extension-unit controls.
//!
//! On cameras whose IR emitter is off by default, this module discovers the
//! vendor control that turns it on and applies it, verifying success from the
//! camera's own frames (a dark frame means the emitter is still off). It builds
//! on the raw ioctl layer in [`crate::capture::uvc`] and the capture path in
//! [`crate::capture::camera`].
//!
//! # Security and risk
//!
//! Activation writes vendor payloads to the device (see [`crate::capture::uvc`]); this
//! module only writes values it first read back from the control's own range,
//! and restores the prior value when an attempt does not light the emitter.

use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use crate::capture::camera;
use crate::capture::uvc::ControlSnapshot;
use crate::capture::uvc::Handle;
use crate::error::Error;
use crate::error::Result;

// Constants

/// Highest extension-unit ID to probe (XU IDs are small and vendor-assigned).
const MAX_UNIT: u8 = 8;

/// Highest control selector to probe within each unit.
const MAX_SELECTOR: u8 = 8;

/// Resolution requested while measuring whether the emitter lit up.
const PROBE_WIDTH: u32 = 640;

/// Resolution requested while measuring whether the emitter lit up.
const PROBE_HEIGHT: u32 = 480;

/// Hard cap on each verification capture.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Mean luma above which the emitter is considered lit.
const LIT_MEAN: f64 = 12.0;

/// Maximum control length for which a full first-byte value sweep is tried.
const SWEEP_MAX_LEN: u16 = 2;

// Data Structures

/// A discovered extension-unit control and its current value range.
#[derive(Clone, Debug)]
pub struct ControlProbe {
    /// Control selector within the unit.
    pub selector: u8,
    /// The control's length and value range.
    pub snapshot: ControlSnapshot,
    /// Extension-unit ID owning the control.
    pub unit: u8,
}

/// A control payload that successfully lit the IR emitter.
#[derive(Clone, Debug)]
pub struct Activation {
    /// Mean frame luma observed after applying the payload.
    pub mean: f64,
    /// Payload written to the control (`SET_CUR`).
    pub payload: Vec<u8>,
    /// Control selector within the unit.
    pub selector: u8,
    /// Extension-unit ID owning the control.
    pub unit: u8,
}

// Functions

/// Probes every plausible extension-unit control, returning those that exist.
///
/// Uses read-only range queries only, so it is safe to run repeatedly.
pub fn probe(path: &Path) -> Result<Vec<ControlProbe>> {
    let handle = Handle::open(path)?;
    let mut found = Vec::new();
    for unit in 1..=MAX_UNIT {
        for selector in 1..=MAX_SELECTOR {
            if let Ok(snapshot) = handle.snapshot(unit, selector) {
                if snapshot.len > 0 {
                    found.push(ControlProbe {
                        selector,
                        snapshot,
                        unit,
                    });
                }
            }
        }
    }
    Ok(found)
}

/// Finds the node that exposes UVC extension-unit controls.
///
/// On many cameras the controls live on the VideoControl node (often the RGB
/// node), which differs from the IR capture node. Probes each accessible capture
/// node read-only and returns the first that exposes any control.
pub fn control_node() -> Result<Option<PathBuf>> {
    let devices = camera::enumerate()?;
    for device in &devices {
        if !device.is_capture || device.error.is_some() {
            continue;
        }
        if probe(&device.path)?.iter().any(|c| c.snapshot.len > 0) {
            return Ok(Some(device.path.clone()));
        }
    }
    Ok(None)
}

/// Attempts to light the IR emitter and returns the payload that worked.
///
/// Writes candidate payloads to controls on `control_path` and verifies each by
/// capturing from `capture_path` (these are usually different nodes). For each
/// control it tries the maximum then the default value, keeping the first that
/// raises mean brightness above [`LIT_MEAN`]. Unsuccessful writes are rolled back.
/// Returns `None` if nothing lit the emitter.
pub fn enable(control_path: &Path, capture_path: &Path) -> Result<Option<Activation>> {
    let controls = probe(control_path)?;
    let handle = Handle::open(control_path)?;
    for control in &controls {
        for payload in candidates(&control.snapshot) {
            // Many controls reject SET_CUR (read-only, or a value they dislike);
            // a failed write just means this candidate is not the activation.
            if handle
                .set_cur(control.unit, control.selector, &payload)
                .is_err()
            {
                continue;
            }
            let mean = capture_mean(capture_path)?;
            if mean >= LIT_MEAN {
                return Ok(Some(Activation {
                    mean,
                    payload,
                    selector: control.selector,
                    unit: control.unit,
                }));
            }
            restore(&handle, control);
        }
    }
    Ok(None)
}

/// Applies a previously discovered activation payload without re-probing.
pub fn apply(control_path: &Path, unit: u8, selector: u8, payload: &[u8]) -> Result<()> {
    let handle = Handle::open(control_path)?;
    handle.set_cur(unit, selector, payload)
}

/// Candidate payloads to try for a control, most likely first.
///
/// Tries the device's own notable values (max, default, min) first, then for
/// short controls sweeps every first-byte value across the declared `min..=max`
/// range, since the activation value often lies inside that range.
fn candidates(snapshot: &ControlSnapshot) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    for value in [&snapshot.max, &snapshot.def, &snapshot.min] {
        push_unique(&mut out, value.clone());
    }
    if snapshot.len <= SWEEP_MAX_LEN && !snapshot.max.is_empty() {
        let lo = snapshot.min.first().copied().unwrap_or(0);
        let hi = snapshot.max.first().copied().unwrap_or(u8::MAX);
        for first in lo..=hi {
            let mut bytes = vec![0u8; snapshot.len as usize];
            bytes[0] = first;
            push_unique(&mut out, bytes);
        }
    }
    out
}

/// Appends a non-empty payload if it is not already present.
fn push_unique(out: &mut Vec<Vec<u8>>, value: Vec<u8>) {
    if !value.is_empty() && !out.contains(&value) {
        out.push(value);
    }
}

/// Restores a control to its pre-probe current value, ignoring failures.
fn restore(handle: &Handle, control: &ControlProbe) {
    if !control.snapshot.cur.is_empty() {
        let _ = handle.set_cur(control.unit, control.selector, &control.snapshot.cur);
    }
}

/// Captures one frame from the IR node and returns its mean luma.
fn capture_mean(path: &Path) -> Result<f64> {
    let frame = camera::capture_frame(path, PROBE_WIDTH, PROBE_HEIGHT, PROBE_TIMEOUT)?;
    Ok(frame.brightness()?.mean)
}

/// Errors out if the path is not a usable capture node (kept for callers that
/// want an explicit precondition check before probing).
pub fn ensure_capture_node(path: &Path) -> Result<()> {
    let devices = camera::enumerate()?;
    let known = devices
        .iter()
        .any(|d| d.path == path && d.is_capture && d.error.is_none());
    if known {
        Ok(())
    } else {
        Err(Error::Camera(format!(
            "{} is not an accessible capture node",
            path.display()
        )))
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(cur: &[u8], def: &[u8], max: &[u8]) -> ControlSnapshot {
        ControlSnapshot {
            cur: cur.to_vec(),
            def: def.to_vec(),
            len: max.len() as u16,
            max: max.to_vec(),
            min: vec![0; max.len()],
        }
    }

    #[test]
    fn candidates_try_notable_values_before_sweep() {
        // min is vec![0, 0] from the helper; max=[3,0], def=[1,0].
        let snap = snapshot(&[0, 0], &[1, 0], &[3, 0]);
        let got = candidates(&snap);
        assert_eq!(&got[..3], &[vec![3, 0], vec![1, 0], vec![0, 0]]);
        // Sweep adds the remaining first-byte value (2) within 0..=3.
        assert!(got.contains(&vec![2, 0]));
    }

    #[test]
    fn candidates_sweep_covers_min_to_max_range() {
        let snap = snapshot(&[0], &[0], &[4]);
        let got = candidates(&snap);
        for value in 0..=4 {
            assert!(got.contains(&vec![value]), "missing {value}");
        }
    }

    #[test]
    fn candidates_deduplicate_equal_values() {
        let snap = ControlSnapshot {
            cur: vec![3],
            def: vec![3],
            len: 1,
            max: vec![3],
            min: vec![3],
        };
        assert_eq!(candidates(&snap), vec![vec![3]]);
    }

    #[test]
    fn candidates_skip_empty_ranges() {
        let snap = snapshot(&[], &[], &[]);
        assert!(candidates(&snap).is_empty());
    }
}
