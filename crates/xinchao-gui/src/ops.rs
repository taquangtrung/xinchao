//! Backend operations for the GUI: locating and running the `xinchao` CLI,
//! capturing IR preview frames, and reading enrollment, PAM, and config state.
//!
//! Everything here is read-only or shells out to the CLI (via `pkexec` for the
//! root-owned writes); the GUI never performs authentication itself.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use eframe::egui::ColorImage;
use xinchao::capture::camera;
use xinchao::config;
use xinchao::pam::conf as pamconf;
use xinchao::recognition::embed::RecognitionModel;
use xinchao::store;

use crate::app::Event;

// Constants

/// Requested preview capture width in pixels.
const PREVIEW_WIDTH: u32 = 640;

/// Requested preview capture height in pixels.
const PREVIEW_HEIGHT: u32 = 480;

/// Hard cap on a single preview frame capture.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(4);

/// Mean luma below which a preview frame is treated as "the emitter is off".
const PREVIEW_DARK_MEAN: f32 = 8.0;

// Functions

/// Resolves the `xinchao` CLI: the `--xinchao-bin <PATH>` flag if given, then the
/// first `xinchao` on `PATH`, then one next to this binary. Errors with guidance if
/// none resolve.
pub(crate) fn resolve_cli() -> Result<PathBuf, String> {
    if let Some(path) = cli_flag() {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = find_in_path("xinchao") {
        return Ok(path);
    }
    if let Some(path) = sibling_binary("xinchao") {
        return Ok(path);
    }
    Err("xinchao CLI not found in PATH or next to xinchao-gui.\n\
         Install it with 'make install', or pass its path:\n    \
         xinchao-gui --xinchao-bin /path/to/xinchao"
        .to_string())
}

/// Returns the value of the `--xinchao-bin <PATH>` (or `--xinchao-bin=PATH`) flag.
fn cli_flag() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--xinchao-bin=") {
            return Some(value.to_string());
        }
        if arg == "--xinchao-bin" {
            return args.next();
        }
    }
    None
}

/// Finds `name` in a `PATH` directory, returning the first match.
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Finds `name` in the same directory as the running binary.
fn sibling_binary(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join(name);
    candidate.is_file().then_some(candidate)
}

/// Resolves the IR node path, failing if none is detected.
pub(crate) fn resolve_ir_node() -> Result<PathBuf, String> {
    let devices = camera::enumerate().map_err(|error| error.to_string())?;
    let index = camera::detect_ir(&devices).ok_or_else(|| "no IR node detected".to_string())?;
    Ok(devices[index].path.clone())
}

/// Captures one greyscale frame as an egui image, plus whether it is near-black.
pub(crate) fn capture_color_image(node: &Path) -> Result<(ColorImage, bool), String> {
    let frame = camera::capture_frame(node, PREVIEW_WIDTH, PREVIEW_HEIGHT, CAPTURE_TIMEOUT)
        .map_err(|error| error.to_string())?;
    let luma = frame.luma8().map_err(|error| error.to_string())?;
    let (width, height) = (frame.width as usize, frame.height as usize);
    let pixels = width * height;
    if luma.len() < pixels {
        return Err("frame buffer too small for geometry".to_string());
    }
    let sum: u64 = luma.iter().take(pixels).map(|&value| value as u64).sum();
    let dark = sum as f32 / pixels as f32 <= PREVIEW_DARK_MEAN;
    let mut rgba = Vec::with_capacity(pixels * 4);
    for &value in luma.iter().take(pixels) {
        rgba.extend_from_slice(&[value, value, value, 255]);
    }
    Ok((
        ColorImage::from_rgba_unmultiplied([width, height], &rgba),
        dark,
    ))
}

/// Lists the users with an enrollment in the system store.
pub(crate) fn list_users() -> Vec<String> {
    store::list(Path::new(store::DEFAULT_DIR)).unwrap_or_default()
}

/// Reads the configured recognition model, defaulting if there is no config.
pub(crate) fn load_model() -> RecognitionModel {
    config::load(Path::new(config::DEFAULT_PATH))
        .map(|c| c.recognition.model)
        .unwrap_or_default()
}

/// The `--model` CLI value for a recognition model.
pub(crate) fn model_arg(model: RecognitionModel) -> &'static str {
    match model {
        RecognitionModel::Arcface => "arcface",
        RecognitionModel::Sface => "sface",
    }
}

/// A human-facing name for a recognition model.
pub(crate) fn model_label(model: RecognitionModel) -> &'static str {
    match model {
        RecognitionModel::Arcface => "ArcFace",
        RecognitionModel::Sface => "SFace",
    }
}

/// Reads, for each available PAM service, its label, name, and enabled state.
pub(crate) fn load_pam_status() -> Vec<(&'static str, &'static str, bool)> {
    pamconf::available()
        .iter()
        .map(|service| {
            (
                service.label,
                service.name,
                pamconf::status(service.name).unwrap_or(false),
            )
        })
        .collect()
}

/// Returns the invoking user's login name, for prefilling the enroll form.
pub(crate) fn current_user() -> String {
    std::env::var("USER").unwrap_or_default()
}

/// The last non-empty line of `text`, or the whole thing trimmed.
pub(crate) fn last_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or(text.trim())
        .to_string()
}

/// Runs the `xinchao` CLI (optionally via `pkexec`) and returns stdout or an error.
pub(crate) fn run_cli(cli: &Path, privileged: bool, args: &[&str]) -> Result<String, String> {
    let mut command = if privileged {
        let mut command = Command::new("pkexec");
        command.arg(cli);
        command
    } else {
        Command::new(cli)
    };
    command.args(args);
    let output = command
        .output()
        .map_err(|error| format!("could not run {}: {error}", cli.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    Err(if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    })
}

/// Maps a CLI result to a status event, using `ok_text` on success.
pub(crate) fn status_of(result: Result<String, String>, ok_text: String) -> Event {
    match result {
        Ok(output) => Event::Status {
            ok: true,
            text: {
                let line = last_line(&output);
                if line.is_empty() {
                    ok_text
                } else {
                    line
                }
            },
        },
        Err(error) => Event::Status {
            ok: false,
            text: last_line(&error),
        },
    }
}
