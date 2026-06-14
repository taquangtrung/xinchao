//! V4L2 camera enumeration, IR-node identification, and frame capture.
//!
//! This module backs `xinchao diagnose` (milestone M1): it lists the V4L2
//! capture nodes on the system, guesses which one is the infrared sensor, and
//! can grab a single frame to prove capture works on the target hardware.
//!
//! Devices are identified by their stable `/dev/v4l/by-id/...` path where one
//! exists, because the numeric `/dev/videoN` index can change across reboots and
//! docking (see the security model in `docs/IMPLEMENTATION_PLAN.md`).
//!
//! # Security posture
//!
//! Capture is bounded by a caller-supplied timeout so authentication can never
//! hang on a stalled sensor, and every failure surfaces as an [`Error`] so
//! callers fail closed.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use image::GrayImage;
use v4l::buffer::Type;
use v4l::io::mmap;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::Device;
use v4l::Format;

use crate::error::Error;
use crate::error::Result;

// Constants

/// Pixel-format four-character codes (trimmed of padding) that mark a greyscale
/// or depth stream, i.e. the kind an IR sensor reports.
const IR_FOURCCS: &[&str] = &["GREY", "Y8", "Y8I", "Y10", "Y12", "Y12I", "Y16", "Z16"];

/// Pixel-format codes (trimmed) that mark a colour stream, i.e. the regular RGB
/// webcam node rather than the IR sensor.
const COLOR_FOURCCS: &[&str] = &[
    "MJPG", "YUYV", "YVYU", "UYVY", "NV12", "NV21", "YU12", "YV12", "RGB3", "BGR3", "JPEG", "H264",
];

/// Directory of stable by-id symlinks to V4L2 nodes.
const BY_ID_DIR: &str = "/dev/v4l/by-id";

/// Number of buffers to map for a capture stream.
const BUFFER_COUNT: u32 = 4;

/// Frames streamed in one capture burst. We keep the brightest, because IR
/// Windows-Hello cameras strobe the illuminator (only some frames are lit) and
/// take a dozen-plus frames for auto-exposure to settle; a fixed-index grab
/// almost always lands on a dark frame.
const BURST_FRAMES: usize = 32;

// Data Structures

/// A discovered V4L2 device node and what we could learn about it.
///
/// When the node could not be opened (commonly a permission problem, since
/// `/dev/video*` is `root:video`), [`error`](Self::error) is set and the
/// capability fields are left at their defaults.
#[derive(Debug)]
pub struct DeviceInfo {
    /// Stable `usb-...` name from `/dev/v4l/by-id`, if the node has one.
    pub by_id: Option<String>,
    /// Human-readable device name reported by the driver (the V4L2 "card").
    pub card: String,
    /// Kernel driver backing the node.
    pub driver: String,
    /// Why the node could not be queried, if it could not.
    pub error: Option<String>,
    /// Pixel formats the node advertises for capture.
    pub formats: Vec<FormatInfo>,
    /// Whether the node supports video capture (vs. metadata/output only).
    pub is_capture: bool,
    /// The `/dev/videoN` path of the node.
    pub path: PathBuf,
}

/// One pixel format advertised by a capture node.
#[derive(Debug)]
pub struct FormatInfo {
    /// Driver's human-readable description (e.g. "Greyscale 8-bit").
    pub description: String,
    /// Four-character code as advertised, padding trimmed (e.g. "GREY").
    pub fourcc: String,
}

/// A single captured frame and the geometry the driver negotiated.
#[derive(Debug)]
pub struct Frame {
    /// Raw pixel bytes exactly as the device delivered them.
    pub data: Vec<u8>,
    /// Pixel format the frame was captured in, padding trimmed.
    pub fourcc: String,
    /// Frame height in pixels.
    pub height: u32,
    /// Frame width in pixels.
    pub width: u32,
}

/// Summary statistics of a frame's luma, for spotting an unlit IR illuminator.
#[derive(Debug)]
pub struct Brightness {
    /// Brightest pixel value (0-255).
    pub max: u8,
    /// Mean pixel value (0-255).
    pub mean: f64,
    /// Darkest pixel value (0-255).
    pub min: u8,
}

// Functions

/// Enumerates every `/dev/video*` node, annotating each with its by-id name,
/// driver, card, and advertised capture formats.
///
/// Nodes that cannot be opened or queried are still returned, with
/// [`DeviceInfo::error`] describing why, so `diagnose` can show the user a
/// permission problem rather than silently dropping a device.
pub fn enumerate() -> Result<Vec<DeviceInfo>> {
    let by_id = by_id_map();
    let mut paths = video_node_paths()?;
    paths.sort();

    let devices = paths
        .into_iter()
        .map(|path| describe_node(path, &by_id))
        .collect();
    Ok(devices)
}

/// Picks the most likely IR node from an enumerated set, returning its index.
///
/// A node qualifies as IR when it can capture and advertises a greyscale/depth
/// format. Nodes that advertise no colour format are strongly preferred, since
/// the RGB webcam node typically also offers MJPG/YUYV.
pub fn detect_ir(devices: &[DeviceInfo]) -> Option<usize> {
    let mut fallback = None;
    for (index, device) in devices.iter().enumerate() {
        if !device.is_capture || !has_ir_format(device) {
            continue;
        }
        if !has_color_format(device) {
            return Some(index);
        }
        fallback.get_or_insert(index);
    }
    fallback
}

/// Captures the brightest of a short frame burst from the node at `path`.
///
/// The device's first greyscale/IR format is requested at the given resolution;
/// the driver may negotiate a nearby size, which is reflected in the returned
/// [`Frame`]. We stream up to [`BURST_FRAMES`] and keep the brightest, because IR
/// illuminators strobe and auto-exposure needs time to settle. Capture is bounded
/// by `timeout` so it can never hang.
pub fn capture_frame(path: &Path, width: u32, height: u32, timeout: Duration) -> Result<Frame> {
    let device = Device::with_path(path)
        .map_err(|e| Error::Camera(format!("cannot open {}: {e}", path.display())))?;

    let fourcc = ir_fourcc(&device).ok_or_else(|| {
        Error::Camera(format!("{} advertises no greyscale format", path.display()))
    })?;

    let requested = Format::new(width, height, fourcc);
    let negotiated = device
        .set_format(&requested)
        .map_err(|e| Error::Camera(format!("cannot set capture format: {e}")))?;

    let mut stream = mmap::Stream::with_buffers(&device, Type::VideoCapture, BUFFER_COUNT)
        .map_err(|e| Error::Camera(format!("cannot start capture stream: {e}")))?;
    stream.set_timeout(timeout);

    // Stream a burst and keep the brightest frame, bounded by the deadline so we
    // never exceed the caller's timeout. This rides over the IR strobe and the
    // auto-exposure ramp instead of grabbing a fixed (usually dark) frame.
    let deadline = Instant::now() + timeout;
    let mut brightest = Vec::new();
    let mut brightest_mean = -1.0f64;
    let mut frames = 0;
    while frames < BURST_FRAMES && Instant::now() < deadline {
        let (buffer, _meta) = stream
            .next()
            .map_err(|e| Error::Camera(format!("frame capture failed: {e}")))?;
        frames += 1;
        let mean = mean_luma(buffer);
        if mean > brightest_mean {
            brightest_mean = mean;
            brightest.clear();
            brightest.extend_from_slice(buffer);
        }
    }
    if brightest.is_empty() {
        return Err(Error::Camera(format!(
            "no frames captured from {} within the timeout",
            path.display()
        )));
    }

    Ok(Frame {
        data: brightest,
        fourcc: trim_fourcc(negotiated.fourcc.str().unwrap_or("")),
        height: negotiated.height,
        width: negotiated.width,
    })
}

/// Mean byte value of a greyscale frame buffer, a cheap brightness proxy.
fn mean_luma(buffer: &[u8]) -> f64 {
    if buffer.is_empty() {
        return 0.0;
    }
    let sum: u64 = buffer.iter().map(|&b| b as u64).sum();
    sum as f64 / buffer.len() as f64
}

/// Writes a greyscale frame to `path` as a PNG for visual inspection.
pub fn save_debug_png(frame: &Frame, path: &Path) -> Result<()> {
    let gray = frame.luma8()?;
    let image = GrayImage::from_raw(frame.width, frame.height, gray)
        .ok_or_else(|| Error::Camera("frame buffer too small for its geometry".to_string()))?;
    image
        .save(path)
        .map_err(|e| Error::Camera(format!("cannot write {}: {e}", path.display())))
}

/// Describes a single node, capturing any open/query failure into the result.
fn describe_node(path: PathBuf, by_id: &HashMap<PathBuf, String>) -> DeviceInfo {
    let by_id = by_id.get(&path).cloned();
    let device = match Device::with_path(&path) {
        Ok(device) => device,
        Err(e) => return failed_node(path, by_id, format!("cannot open: {e}")),
    };

    let caps = match device.query_caps() {
        Ok(caps) => caps,
        Err(e) => return failed_node(path, by_id, format!("cannot query capabilities: {e}")),
    };
    let is_capture = caps
        .capabilities
        .contains(v4l::capability::Flags::VIDEO_CAPTURE);

    let formats = if is_capture {
        device
            .enum_formats()
            .map(|descs| descs.into_iter().map(FormatInfo::from).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    DeviceInfo {
        by_id,
        card: caps.card,
        driver: caps.driver,
        error: None,
        formats,
        is_capture,
        path,
    }
}

/// Builds a [`DeviceInfo`] for a node we could not fully query.
fn failed_node(path: PathBuf, by_id: Option<String>, error: String) -> DeviceInfo {
    DeviceInfo {
        by_id,
        card: String::new(),
        driver: String::new(),
        error: Some(error),
        formats: Vec::new(),
        is_capture: false,
        path,
    }
}

/// Lists `/dev/video*` paths present on the system.
fn video_node_paths() -> Result<Vec<PathBuf>> {
    let entries =
        fs::read_dir("/dev").map_err(|e| Error::Camera(format!("cannot read /dev: {e}")))?;
    let mut paths = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("video") {
            paths.push(entry.path());
        }
    }
    Ok(paths)
}

/// Maps each resolved `/dev/videoN` path to its stable by-id name.
fn by_id_map() -> HashMap<PathBuf, String> {
    let mut map = HashMap::new();
    let entries = match fs::read_dir(BY_ID_DIR) {
        Ok(entries) => entries,
        Err(_) => return map,
    };
    for entry in entries.flatten() {
        let link = entry.path();
        if let Ok(target) = fs::canonicalize(&link) {
            let name = entry.file_name().to_string_lossy().into_owned();
            map.insert(target, name);
        }
    }
    map
}

/// Returns the first IR/greyscale [`FourCC`](v4l::FourCC) the live device offers.
fn ir_fourcc(device: &Device) -> Option<v4l::FourCC> {
    let formats = device.enum_formats().ok()?;
    formats
        .into_iter()
        .find(|desc| is_ir_fourcc(&trim_fourcc(desc.fourcc.str().unwrap_or(""))))
        .map(|desc| desc.fourcc)
}

/// Whether any advertised format on the node is greyscale/IR.
fn has_ir_format(device: &DeviceInfo) -> bool {
    device.formats.iter().any(|f| is_ir_fourcc(&f.fourcc))
}

/// Whether any advertised format on the node is a colour format.
fn has_color_format(device: &DeviceInfo) -> bool {
    device.formats.iter().any(|f| is_color_fourcc(&f.fourcc))
}

/// Whether a trimmed four-character code denotes a greyscale/IR format.
fn is_ir_fourcc(fourcc: &str) -> bool {
    IR_FOURCCS.contains(&fourcc)
}

/// Whether a trimmed four-character code denotes a colour format.
fn is_color_fourcc(fourcc: &str) -> bool {
    COLOR_FOURCCS.contains(&fourcc)
}

/// Strips the space padding V4L2 uses to pad short codes to four bytes.
fn trim_fourcc(fourcc: &str) -> String {
    fourcc
        .trim_matches(|c: char| c == ' ' || c == '\0')
        .to_string()
}

impl Frame {
    /// Returns frame brightness statistics, used to detect an unlit IR sensor.
    pub fn brightness(&self) -> Result<Brightness> {
        let luma = self.luma8()?;
        let max = luma.iter().copied().max().unwrap_or(0);
        let min = luma.iter().copied().min().unwrap_or(0);
        let sum: u64 = luma.iter().map(|&p| u64::from(p)).sum();
        let mean = sum as f64 / luma.len().max(1) as f64;
        Ok(Brightness { max, mean, min })
    }

    /// Converts the frame to 8-bit luma, failing if the byte layout is unknown.
    ///
    /// Handles 8-bit (one byte per pixel) and 16-bit little-endian (two bytes
    /// per pixel, high byte kept) greyscale, covering the recognised IR formats.
    pub fn luma8(&self) -> Result<Vec<u8>> {
        let pixels = (self.width * self.height) as usize;
        if self.data.len() == pixels {
            Ok(self.data.clone())
        } else if self.data.len() == pixels * 2 {
            Ok(self.data.chunks_exact(2).map(|p| p[1]).collect())
        } else {
            Err(Error::Camera(format!(
                "cannot interpret {} frame of {} bytes at {}x{}",
                self.fourcc,
                self.data.len(),
                self.width,
                self.height
            )))
        }
    }
}

impl From<v4l::format::Description> for FormatInfo {
    fn from(desc: v4l::format::Description) -> Self {
        FormatInfo {
            description: desc.description,
            fourcc: trim_fourcc(desc.fourcc.str().unwrap_or("")),
        }
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    fn device(formats: &[&str]) -> DeviceInfo {
        DeviceInfo {
            by_id: None,
            card: "test".to_string(),
            driver: "test".to_string(),
            error: None,
            formats: formats
                .iter()
                .map(|f| FormatInfo {
                    description: f.to_string(),
                    fourcc: f.to_string(),
                })
                .collect(),
            is_capture: true,
            path: PathBuf::from("/dev/videoX"),
        }
    }

    #[test]
    fn greyscale_codes_are_ir() {
        assert!(is_ir_fourcc("GREY"));
        assert!(is_ir_fourcc("Y16"));
        assert!(!is_ir_fourcc("MJPG"));
    }

    #[test]
    fn color_codes_are_not_ir() {
        assert!(is_color_fourcc("YUYV"));
        assert!(!is_color_fourcc("GREY"));
    }

    #[test]
    fn fourcc_padding_is_trimmed() {
        assert_eq!(trim_fourcc("Y16 "), "Y16");
        assert_eq!(trim_fourcc("GREY"), "GREY");
    }

    #[test]
    fn detect_ir_prefers_pure_greyscale_node() {
        let devices = vec![device(&["MJPG", "YUYV"]), device(&["GREY"])];
        assert_eq!(detect_ir(&devices), Some(1));
    }

    #[test]
    fn detect_ir_falls_back_to_mixed_node() {
        let devices = vec![device(&["MJPG", "YUYV"]), device(&["MJPG", "GREY"])];
        assert_eq!(detect_ir(&devices), Some(1));
    }

    #[test]
    fn detect_ir_ignores_color_only_nodes() {
        let devices = vec![device(&["MJPG", "YUYV"])];
        assert_eq!(detect_ir(&devices), None);
    }

    #[test]
    fn detect_ir_skips_non_capture_nodes() {
        let mut ir = device(&["GREY"]);
        ir.is_capture = false;
        assert_eq!(detect_ir(&[ir]), None);
    }

    #[test]
    fn luma8_passes_through_8bit_frame() {
        let frame = Frame {
            data: vec![0, 64, 128, 255],
            fourcc: "GREY".to_string(),
            height: 2,
            width: 2,
        };
        assert_eq!(frame.luma8().unwrap(), vec![0, 64, 128, 255]);
    }

    #[test]
    fn luma8_keeps_high_byte_of_16bit_frame() {
        let frame = Frame {
            data: vec![0x00, 0x10, 0xFF, 0x20],
            fourcc: "Y16".to_string(),
            height: 1,
            width: 2,
        };
        assert_eq!(frame.luma8().unwrap(), vec![0x10, 0x20]);
    }

    #[test]
    fn luma8_rejects_unknown_layout() {
        let frame = Frame {
            data: vec![1, 2, 3],
            fourcc: "GREY".to_string(),
            height: 2,
            width: 2,
        };
        assert!(frame.luma8().is_err());
    }

    #[test]
    fn brightness_summarises_luma() {
        let frame = Frame {
            data: vec![0, 100, 200, 0],
            fourcc: "GREY".to_string(),
            height: 2,
            width: 2,
        };
        let stats = frame.brightness().unwrap();
        assert_eq!(stats.min, 0);
        assert_eq!(stats.max, 200);
        assert_eq!(stats.mean, 75.0);
    }
}
