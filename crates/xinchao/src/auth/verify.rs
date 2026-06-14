//! High-level recognition orchestration shared by the CLI and PAM module.
//!
//! A [`Verifier`] bundles the face [`Detector`] and the [`Embedder`] so callers
//! get one entry point: detect the best face, crop to it, and embed. [`run`]
//! drives a capture/match loop against a live IR node under a hard deadline and
//! returns an accept/reject [`Outcome`]. Keeping this in the core lets the PAM
//! module stay a thin translation layer (see `docs/IMPLEMENTATION_PLAN.md` M4).
//!
//! # Security posture
//!
//! The loop fails closed: any capture or inference error aborts with an [`Error`]
//! (never a match), the deadline caps total time, and a frame with no detected
//! face is skipped rather than matched against the whole frame. Acceptance
//! requires `required_matches` separate frames within the threshold.

use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use image::DynamicImage;
use image::GrayImage;

use crate::capture::camera;
use crate::capture::camera::Frame;
use crate::config::Config;
use crate::error::Error;
use crate::error::Result;
use crate::recognition::detect;
use crate::recognition::detect::Detector;
use crate::recognition::embed::Embedder;
use crate::recognition::recognize;
use crate::recognition::recognize::Embedding;
use crate::store;
use crate::store::Enrollment;

// Constants

/// Upper bound on a single frame capture, so one stalled grab can't eat the
/// whole deadline.
const FRAME_TIMEOUT: Duration = Duration::from_secs(2);

// Data Structures

/// Parameters controlling a verification attempt.
pub struct Params {
    /// Requested capture height in pixels.
    pub frame_height: u32,
    /// Requested capture width in pixels.
    pub frame_width: u32,
    /// Distinct frames that must match within the threshold to accept.
    pub required_matches: u32,
    /// Maximum cosine distance to count a frame as a match.
    pub threshold: f32,
    /// Hard cap on the whole attempt.
    pub timeout: Duration,
}

/// The result of a verification attempt.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Outcome {
    /// Whether enough frames matched to authenticate.
    pub accepted: bool,
    /// Smallest cosine distance seen across the attempt (`INFINITY` if no face).
    pub best_distance: f32,
    /// Number of frames captured.
    pub frames: u32,
    /// Number of frames that matched within the threshold.
    pub matches: u32,
}

/// A loaded recognition pipeline: an embedder plus an optional face detector.
pub struct Verifier {
    detector: Option<Detector>,
    embedder: Embedder,
}

/// A resident verification session: the (large) recognition model, the face
/// detector, the target user's enrollment, and the IR node are all resolved once
/// so repeated [`attempt`](Session::attempt)s avoid reloading them. Built for the
/// unlock daemon, which scans many times over a single process lifetime; a
/// one-shot caller should prefer [`authenticate_user`].
pub struct Session {
    enrollment: Enrollment,
    node: PathBuf,
    params: Params,
    verifier: Verifier,
}

// Functions

/// Loads an image file for detection/embedding.
pub fn load_image(path: &Path) -> Result<DynamicImage> {
    image::open(path)
        .map_err(|e| Error::Recognition(format!("cannot read {}: {e}", path.display())))
}

/// Converts a captured greyscale frame into an image for detection/embedding.
pub fn frame_to_image(frame: &Frame) -> Result<DynamicImage> {
    let luma = frame.luma8()?;
    let gray = GrayImage::from_raw(frame.width, frame.height, luma)
        .ok_or_else(|| Error::Recognition("frame buffer too small for geometry".to_string()))?;
    Ok(DynamicImage::ImageLuma8(gray))
}

/// Authenticates `user` end to end from `config`: loads their enrollment, builds
/// the recognition pipeline, resolves the IR node, and runs the match loop.
///
/// The enrollment is loaded with [`store::load_secure`], so a non-root-owned or
/// world-writable file is refused. A detector is loaded only when
/// `config.auth.detect_faces` is set; when it is, a missing detector model is a
/// hard error (and thus a closed door) rather than a silent whole-frame match.
pub fn authenticate_user(config: &Config, store_dir: &Path, user: &str) -> Result<Outcome> {
    Session::load(config, store_dir, user)?.attempt()
}

/// Resolves the IR node from config, falling back to auto-detection.
fn resolve_node(config: &Config) -> Result<PathBuf> {
    if let Some(device) = &config.camera.device {
        return Ok(device.clone());
    }
    let devices = camera::enumerate()?;
    let ir = camera::detect_ir(&devices)
        .ok_or_else(|| Error::Camera("no IR node detected".to_string()))?;
    Ok(devices[ir].path.clone())
}

/// Runs a capture/match loop against `node` until it accepts or the deadline passes.
///
/// Captures frames, embeds the detected face in each, and counts how many land
/// within `params.threshold`. Returns as soon as `required_matches` is reached,
/// or a rejecting [`Outcome`] when the deadline elapses. Any capture or inference
/// failure propagates as an [`Error`], so the caller fails closed.
pub fn run(
    verifier: &mut Verifier,
    node: &Path,
    enrollment: &Enrollment,
    params: &Params,
) -> Result<Outcome> {
    let deadline = Instant::now() + params.timeout;
    let mut matches = 0u32;
    let mut frames = 0u32;
    let mut best_distance = f32::INFINITY;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let frame = camera::capture_frame(
            node,
            params.frame_width,
            params.frame_height,
            remaining.min(FRAME_TIMEOUT),
        )?;
        frames += 1;
        let image = frame_to_image(&frame)?;
        let face = match verifier.prepare(&image)? {
            Some(face) => face,
            None => continue,
        };
        let embedding = verifier.embed(&face)?;
        if let Some(decision) =
            recognize::decide(&embedding, enrollment.embeddings(), params.threshold)
        {
            best_distance = best_distance.min(decision.distance);
            if decision.accepted {
                matches += 1;
                if matches >= params.required_matches.max(1) {
                    return Ok(Outcome {
                        accepted: true,
                        best_distance,
                        frames,
                        matches,
                    });
                }
            }
        }
    }
    Ok(Outcome {
        accepted: false,
        best_distance,
        frames,
        matches,
    })
}

impl Verifier {
    /// Loads the recognition model and, when a path is given, the face detector.
    pub fn load(model_path: &Path, detector_path: Option<&Path>) -> Result<Self> {
        let embedder = Embedder::load(model_path)?;
        let detector = match detector_path {
            Some(path) => Some(Detector::load(path)?),
            None => None,
        };
        Ok(Verifier { detector, embedder })
    }

    /// Whether a face detector is loaded.
    pub fn has_detector(&self) -> bool {
        self.detector.is_some()
    }

    /// Selects the image region to embed: the cropped best face when a detector
    /// is loaded, the whole image when none is, or `None` when a detector ran but
    /// found no face. The caller decides what a `None` means (retry, or fall back
    /// to the whole frame for diagnostics).
    pub fn prepare(&mut self, image: &DynamicImage) -> Result<Option<DynamicImage>> {
        match self.detector.as_mut() {
            Some(detector) => Ok(detector
                .best(image)?
                .map(|detection| detect::crop_face(image, detection.bbox))),
            None => Ok(Some(image.clone())),
        }
    }

    /// Computes the embedding of a (already cropped) face image.
    pub fn embed(&mut self, image: &DynamicImage) -> Result<Embedding> {
        self.embedder.embed(image)
    }
}

impl Session {
    /// Builds a resident session for `user` from `config`, loading the model,
    /// detector, enrollment, and IR node once. Mirrors [`authenticate_user`]'s
    /// setup, including the secure (root-owned) enrollment load that fails closed.
    pub fn load(config: &Config, store_dir: &Path, user: &str) -> Result<Self> {
        let enrollment = store::load_secure(store_dir, user)?;
        let detector_path = if config.auth.detect_faces {
            Some(config.recognition.detector_path.as_path())
        } else {
            None
        };
        let verifier = Verifier::load(&config.recognition.model_file(), detector_path)?;
        let node = resolve_node(config)?;
        let params = Params {
            frame_height: config.camera.frame_height,
            frame_width: config.camera.frame_width,
            required_matches: config.recognition.required_matches,
            threshold: config.recognition.threshold as f32,
            timeout: Duration::from_secs(config.auth.timeout_secs),
        };
        Ok(Session {
            enrollment,
            node,
            params,
            verifier,
        })
    }

    /// Runs one capture/match attempt against the IR node, reusing the already
    /// loaded model and detector. Fails closed like [`run`].
    pub fn attempt(&mut self) -> Result<Outcome> {
        run(
            &mut self.verifier,
            &self.node,
            &self.enrollment,
            &self.params,
        )
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_to_image_rejects_short_buffer() {
        let frame = Frame {
            data: vec![0u8; 4],
            fourcc: "GREY".to_string(),
            height: 4,
            width: 4,
        };
        assert!(frame_to_image(&frame).is_err());
    }

    #[test]
    fn frame_to_image_accepts_matching_buffer() {
        let frame = Frame {
            data: vec![10u8; 16],
            fourcc: "GREY".to_string(),
            height: 4,
            width: 4,
        };
        let image = frame_to_image(&frame).unwrap();
        assert_eq!(image.width(), 4);
        assert_eq!(image.height(), 4);
    }
}
