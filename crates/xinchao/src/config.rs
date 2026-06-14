//! Configuration loading and validation for `/etc/xinchao/config.toml`.
//!
//! The config is deserialized with serde and validated before use. Every field
//! has a safe default, so a missing file or partial table still yields a usable
//! configuration. See `docs/IMPLEMENTATION_PLAN.md` section 7 for the schema.
//!
//! # Security posture
//!
//! The PAM module runs as root during authentication, so it loads via
//! [`load_secure`], which refuses a config that is not root-owned or is
//! world-writable. Validation rejects nonsensical values (zero timeout, negative
//! threshold) rather than silently degrading, keeping the auth path fail-closed.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;

use crate::error::Error;
use crate::error::Result;
use crate::recognition::embed::RecognitionModel;

// Constants

/// Canonical location of the system configuration file.
pub const DEFAULT_PATH: &str = "/etc/xinchao/config.toml";

/// uid that must own security-sensitive files.
const ROOT_UID: u32 = 0;

/// Permission bit that, if set, makes a file world-writable.
const WORLD_WRITABLE: u32 = 0o002;

// Data Structures

/// Effective xinchao configuration.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Authentication timing and behavior.
    pub auth: Auth,
    /// Camera selection and capture geometry.
    pub camera: Camera,
    /// Opt-in debug capture settings.
    pub debug: Debug,
    /// Persisted IR-emitter activation, replayed at boot; `None` until
    /// `xinchao enable-ir --apply` discovers and saves one.
    pub emitter: Option<Emitter>,
    /// Recognition backend and thresholds.
    pub recognition: Recognition,
}

/// Authentication timing and behavior.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Auth {
    /// Whether to require a detected face before matching.
    pub detect_faces: bool,
    /// Hard cap on a single authentication attempt, in seconds.
    pub timeout_secs: u64,
}

/// Camera selection and capture geometry.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Camera {
    /// IR node path; `None` means auto-detect the IR node.
    pub device: Option<PathBuf>,
    /// Requested capture height in pixels.
    pub frame_height: u32,
    /// Requested capture width in pixels.
    pub frame_width: u32,
    /// Frames discarded before use, to let the sensor settle.
    pub warmup_frames: u32,
}

/// Opt-in debug capture settings.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Debug {
    /// Where to write debug frames; `None` (the default) disables capture.
    pub capture_path: Option<PathBuf>,
}

/// A persisted IR-emitter activation: the vendor UVC control write that lights
/// the illuminator. The emitter resets to dark on power-cycle and on resume, so
/// `xinchao enable-ir --boot` replays this before any face auth runs. All fields
/// are required when the table is present, since they are written together.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Emitter {
    /// Control node the payload is written to (often the RGB/control node, not
    /// the IR capture node).
    pub control_node: PathBuf,
    /// Payload bytes written via UVC `SET_CUR` to light the emitter.
    pub payload: Vec<u8>,
    /// Control selector within the extension unit.
    pub selector: u8,
    /// Extension-unit id owning the control.
    pub unit: u8,
}

/// Recognition backend and thresholds.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Recognition {
    /// Which inference backend to use.
    pub backend: Backend,
    /// Path to the face-detector model file.
    pub detector_path: PathBuf,
    /// Which recognition model to embed with; the file is found next to
    /// `model_path`. Switching models requires re-enrolling.
    pub model: RecognitionModel,
    /// Directory anchor for the model files (its parent holds the models).
    pub model_path: PathBuf,
    /// Frames that must match within one attempt to accept.
    pub required_matches: u32,
    /// Maximum embedding distance to accept; lower is stricter.
    pub threshold: f64,
}

/// Recognition backend selection.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// dlib ResNet face recognition.
    Dlib,
    /// ONNX Runtime with an ArcFace-style model (the default).
    #[default]
    Onnx,
}

// Functions

/// Loads and validates a config from `path`, without permission checks.
///
/// Use this for human-facing tooling; the privileged PAM path uses
/// [`load_secure`] instead.
pub fn load(path: &Path) -> Result<Config> {
    let text = fs::read_to_string(path)
        .map_err(|e| Error::Config(format!("cannot read {}: {e}", path.display())))?;
    let config: Config = toml::from_str(&text)
        .map_err(|e| Error::Config(format!("invalid config {}: {e}", path.display())))?;
    config.validate()?;
    Ok(config)
}

/// Loads a config only if `path` is root-owned and not world-writable.
///
/// This is the entry point for the PAM module, where a tampered or
/// world-writable config would be an authentication-bypass vector.
pub fn load_secure(path: &Path) -> Result<Config> {
    check_permissions(path)?;
    load(path)
}

/// Verifies a security-sensitive file is root-owned and not world-writable.
pub fn check_permissions(path: &Path) -> Result<()> {
    let meta = fs::metadata(path)
        .map_err(|e| Error::Config(format!("cannot stat {}: {e}", path.display())))?;
    if meta.uid() != ROOT_UID {
        return Err(Error::Config(format!(
            "{} must be owned by root",
            path.display()
        )));
    }
    if meta.mode() & WORLD_WRITABLE != 0 {
        return Err(Error::Config(format!(
            "{} must not be world-writable",
            path.display()
        )));
    }
    Ok(())
}

/// Sets `recognition.model` in the config file at `path`, preserving everything
/// else. Written atomically (temp + rename).
pub fn set_model(path: &Path, model: RecognitionModel) -> Result<()> {
    let value = match model {
        RecognitionModel::Arcface => "arcface",
        RecognitionModel::Sface => "sface",
    };
    let content = fs::read_to_string(path)
        .map_err(|e| Error::Config(format!("cannot read {}: {e}", path.display())))?;
    let updated = with_recognition_model(&content, value);
    let tmp = PathBuf::from(format!("{}.xinchao-tmp", path.display()));
    fs::write(&tmp, updated)
        .map_err(|e| Error::Config(format!("cannot write {}: {e}", tmp.display())))?;
    fs::rename(&tmp, path)
        .map_err(|e| Error::Config(format!("cannot install {}: {e}", path.display())))
}

/// Returns `content` with `model = "<value>"` set inside the `[recognition]`
/// table, replacing an existing entry or inserting one (adding the table if
/// absent).
fn with_recognition_model(content: &str, value: &str) -> String {
    let line = format!("model = \"{value}\"");
    let mut out: Vec<String> = Vec::new();
    let mut in_recognition = false;
    let mut done = false;
    for raw in content.lines() {
        let trimmed = raw.trim();
        let is_header = trimmed.starts_with('[') && trimmed.ends_with(']');
        if is_header {
            if in_recognition && !done {
                out.push(line.clone());
                done = true;
            }
            in_recognition = trimmed == "[recognition]";
            out.push(raw.to_string());
            continue;
        }
        if in_recognition && !done && is_key(trimmed, "model") {
            out.push(line.clone());
            done = true;
            continue;
        }
        out.push(raw.to_string());
    }
    if in_recognition && !done {
        out.push(line.clone());
        done = true;
    }
    if !done {
        out.push("[recognition]".to_string());
        out.push(line);
    }
    let mut result = out.join("\n");
    result.push('\n');
    result
}

/// Whether `line` assigns the exact TOML key `key` (not a longer name like
/// `model_path`).
fn is_key(line: &str, key: &str) -> bool {
    line.split('=').next().map(str::trim) == Some(key)
}

/// Persists `emitter` as the `[emitter]` table in the config file at `path`,
/// replacing any existing one and leaving every other table untouched. Written
/// atomically (temp + rename). The block is machine-managed, so it is rendered
/// wholesale rather than edited key by key like [`set_model`].
pub fn set_emitter(path: &Path, emitter: &Emitter) -> Result<()> {
    let content = fs::read_to_string(path)
        .map_err(|e| Error::Config(format!("cannot read {}: {e}", path.display())))?;
    let updated = with_emitter_block(&content, emitter);
    let tmp = PathBuf::from(format!("{}.xinchao-tmp", path.display()));
    fs::write(&tmp, updated)
        .map_err(|e| Error::Config(format!("cannot write {}: {e}", tmp.display())))?;
    fs::rename(&tmp, path)
        .map_err(|e| Error::Config(format!("cannot install {}: {e}", path.display())))
}

/// Returns `content` with the `[emitter]` table replaced by one rendered from
/// `emitter`: any previous `[emitter]` lines are dropped and a fresh block is
/// appended at the end. Other tables and their comments are preserved verbatim.
fn with_emitter_block(content: &str, emitter: &Emitter) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_emitter = false;
    for raw in content.lines() {
        let trimmed = raw.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_emitter = trimmed == "[emitter]";
        }
        if !in_emitter {
            out.push(raw.to_string());
        }
    }
    while out.last().is_some_and(|line| line.trim().is_empty()) {
        out.pop();
    }
    out.push(String::new());
    out.push(render_emitter_block(emitter));
    let mut result = out.join("\n");
    result.push('\n');
    result
}

/// Renders an `[emitter]` table from `emitter`, keys sorted to match the struct.
fn render_emitter_block(emitter: &Emitter) -> String {
    let payload = emitter
        .payload
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "[emitter]\ncontrol_node = \"{}\"\npayload = [{payload}]\nselector = {}\nunit = {}",
        emitter.control_node.display(),
        emitter.selector,
        emitter.unit,
    )
}

impl Config {
    /// Rejects values that would make authentication unsafe or impossible.
    pub fn validate(&self) -> Result<()> {
        if self.recognition.backend == Backend::Dlib {
            return Err(Error::Config(
                "recognition.backend \"dlib\" is not implemented yet; use \"onnx\"".to_string(),
            ));
        }
        if self.camera.frame_width == 0 || self.camera.frame_height == 0 {
            return Err(Error::Config(
                "camera frame size must be non-zero".to_string(),
            ));
        }
        if self.auth.timeout_secs == 0 {
            return Err(Error::Config(
                "auth.timeout_secs must be at least 1".to_string(),
            ));
        }
        if !self.recognition.threshold.is_finite() || self.recognition.threshold <= 0.0 {
            return Err(Error::Config(
                "recognition.threshold must be a positive number".to_string(),
            ));
        }
        if self.recognition.required_matches == 0 {
            return Err(Error::Config(
                "recognition.required_matches must be at least 1".to_string(),
            ));
        }
        if self.recognition.model_path.as_os_str().is_empty() {
            return Err(Error::Config(
                "recognition.model_path must be set".to_string(),
            ));
        }
        Ok(())
    }
}

impl Default for Auth {
    fn default() -> Self {
        Auth {
            detect_faces: true,
            timeout_secs: 2,
        }
    }
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            device: None,
            frame_height: 480,
            frame_width: 640,
            warmup_frames: 3,
        }
    }
}

impl Recognition {
    /// Path to the configured recognition model: its file name in the same
    /// directory as `model_path`.
    pub fn model_file(&self) -> PathBuf {
        let dir = self
            .model_path
            .parent()
            .unwrap_or_else(|| Path::new("/etc/xinchao/models"));
        dir.join(self.model.spec().file_name)
    }
}

impl Default for Recognition {
    fn default() -> Self {
        Recognition {
            backend: Backend::Onnx,
            detector_path: PathBuf::from("/etc/xinchao/models/version-RFB-320.onnx"),
            model: RecognitionModel::default(),
            model_path: PathBuf::from("/etc/xinchao/models/arcfaceresnet100-8.onnx"),
            required_matches: 2,
            threshold: 0.45,
        }
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        assert!(Config::default().validate().is_ok());
    }

    #[test]
    fn defaults_match_the_plan_sketch() {
        let config = Config::default();
        assert_eq!(config.recognition.backend, Backend::Onnx);
        assert_eq!(config.recognition.threshold, 0.45);
        assert_eq!(config.camera.frame_width, 640);
        assert!(config.auth.detect_faces);
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let toml = r#"
            [recognition]
            threshold = 0.30
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.recognition.threshold, 0.30);
        // Untouched fields keep their defaults.
        assert_eq!(config.recognition.backend, Backend::Onnx);
        assert_eq!(config.camera.frame_width, 640);
    }

    #[test]
    fn backend_parses_lowercase() {
        let toml = r#"
            [recognition]
            backend = "dlib"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.recognition.backend, Backend::Dlib);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let toml = r#"
            [camera]
            frame_widht = 640
        "#;
        assert!(toml::from_str::<Config>(toml).is_err());
    }

    #[test]
    fn set_model_replaces_existing_entry() {
        let toml = "[recognition]\nmodel = \"arcface\"\nthreshold = 0.45\n";
        let out = with_recognition_model(toml, "sface");
        assert!(out.contains("model = \"sface\""));
        assert!(!out.contains("arcface"));
        assert!(out.contains("threshold = 0.45"));
        assert_eq!(out.matches("model = ").count(), 1);
    }

    #[test]
    fn set_model_inserts_when_absent_without_touching_model_path() {
        let toml = "[recognition]\nmodel_path = \"/x/arc.onnx\"\nthreshold = 0.45\n";
        let out = with_recognition_model(toml, "sface");
        assert!(out.contains("model = \"sface\""));
        assert!(out.contains("model_path = \"/x/arc.onnx\""));
        let parsed: Config = toml::from_str(&out).unwrap();
        assert_eq!(parsed.recognition.model, RecognitionModel::Sface);
    }

    #[test]
    fn set_model_round_trips_through_the_example_config() {
        let example = include_str!("../../../packaging/xinchao.toml");
        let out = with_recognition_model(example, "sface");
        let parsed: Config = toml::from_str(&out).unwrap();
        assert_eq!(parsed.recognition.model, RecognitionModel::Sface);
    }

    #[test]
    fn set_emitter_appends_a_parseable_block() {
        let toml = "[camera]\ndevice = \"/dev/video2\"\n";
        let emitter = Emitter {
            control_node: PathBuf::from("/dev/video0"),
            payload: vec![1, 255],
            selector: 6,
            unit: 3,
        };
        let out = with_emitter_block(toml, &emitter);
        let parsed: Config = toml::from_str(&out).unwrap();
        assert_eq!(parsed.emitter, Some(emitter));
        // The pre-existing table is untouched.
        assert_eq!(parsed.camera.device, Some(PathBuf::from("/dev/video2")));
    }

    #[test]
    fn set_emitter_replaces_an_existing_block() {
        let toml = "[emitter]\ncontrol_node = \"/dev/video9\"\npayload = [0]\nselector = 1\nunit = 1\n\n[auth]\ntimeout_secs = 4\n";
        let emitter = Emitter {
            control_node: PathBuf::from("/dev/video0"),
            payload: vec![2],
            selector: 6,
            unit: 3,
        };
        let out = with_emitter_block(toml, &emitter);
        assert_eq!(out.matches("[emitter]").count(), 1);
        assert!(!out.contains("/dev/video9"));
        let parsed: Config = toml::from_str(&out).unwrap();
        assert_eq!(parsed.emitter, Some(emitter));
        // A table that followed the old block survives.
        assert_eq!(parsed.auth.timeout_secs, 4);
    }

    #[test]
    fn emitter_defaults_to_none() {
        assert_eq!(Config::default().emitter, None);
    }

    #[test]
    fn dlib_backend_is_rejected() {
        let mut config = Config::default();
        config.recognition.backend = Backend::Dlib;
        assert!(config.validate().is_err());
    }

    #[test]
    fn zero_threshold_is_rejected() {
        let mut config = Config::default();
        config.recognition.threshold = 0.0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn zero_timeout_is_rejected() {
        let mut config = Config::default();
        config.auth.timeout_secs = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn world_writable_config_is_refused() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir();
        let path = dir.join("xinchao-test-config.toml");
        let mut file = fs::File::create(&path).unwrap();
        writeln!(file, "[auth]\ntimeout_secs = 4").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();

        let result = check_permissions(&path);
        let _ = fs::remove_file(&path);
        assert!(result.is_err());
    }
}
