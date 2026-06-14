//! Face detection via the UltraFace ONNX model, run with ONNX Runtime (`ort`).
//!
//! [`Detector`] loads a lightweight detector and returns the face bounding boxes
//! in a frame. Recognition crops to the best box before embedding so background
//! does not dilute the embedding. The default model is the permissively licensed
//! UltraFace RFB-320 ([`ULTRAFACE`]), fetched on first use via [`crate::recognition::models`].
//!
//! The detector input is 320x240 RGB normalized as `(pixel - 127) / 128`; the two
//! outputs are per-anchor face scores and normalized boxes, post-processed with a
//! confidence cutoff and non-maximum suppression. The pure geometry helpers
//! (scaling, IoU, NMS, cropping) are unit-tested without the model.

use image::imageops::FilterType;
use image::DynamicImage;
use image::GenericImageView;
use ort::session::Session;
use ort::value::Tensor;

use crate::error::Error;
use crate::error::Result;
use crate::recognition::models::ModelSpec;

// Constants

/// The default detector model: UltraFace RFB-320 from the ONNX Model Zoo.
pub const ULTRAFACE: ModelSpec = ModelSpec {
    file_name: "version-RFB-320.onnx",
    license: "MIT (ONNX Model Zoo / Ultra-Light-Fast-Generic-Face-Detector)",
    sha256: "34cd7e60aeff28744c657de7a3dc64e872d506741de66987f3426f2b79f88017",
    url: "https://github.com/onnx/models/raw/main/validated/vision/body_analysis/ultraface/models/version-RFB-320.onnx",
};

/// Input width, in pixels, expected by the model.
const INPUT_WIDTH: u32 = 320;

/// Input height, in pixels, expected by the model.
const INPUT_HEIGHT: u32 = 240;

/// Name of the model's input tensor.
const INPUT_NAME: &str = "input";

/// Name of the per-anchor class-score output (`[1, N, 2]`: background, face).
const SCORES_NAME: &str = "scores";

/// Name of the per-anchor box output (`[1, N, 4]`: normalized x1, y1, x2, y2).
const BOXES_NAME: &str = "boxes";

/// Pixel value subtracted during input normalization.
const NORM_MEAN: f32 = 127.0;

/// Divisor applied during input normalization.
const NORM_SCALE: f32 = 128.0;

/// Minimum face probability for a detection to be kept.
const DEFAULT_CONFIDENCE: f32 = 0.7;

/// IoU above which the lower-scoring of two boxes is suppressed.
const DEFAULT_IOU: f32 = 0.5;

/// Fraction each side of the best box is expanded before cropping, so the
/// embedder sees the whole face plus a little context.
const CROP_MARGIN: f32 = 0.2;

// Data Structures

/// A pixel-space, axis-aligned bounding box within an image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoundingBox {
    /// Box height in pixels.
    pub height: u32,
    /// Box width in pixels.
    pub width: u32,
    /// Left edge in pixels.
    pub x: u32,
    /// Top edge in pixels.
    pub y: u32,
}

/// A detected face: its box and the model's confidence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Detection {
    /// Pixel-space bounding box in the source image.
    pub bbox: BoundingBox,
    /// Face probability in `0.0..=1.0`.
    pub confidence: f32,
}

/// A loaded face detector.
pub struct Detector {
    session: Session,
}

// Functions

/// Maps an `ort` error into the core error type.
fn map_ort(error: ort::Error) -> Error {
    Error::Recognition(format!("onnx runtime (detector): {error}"))
}

/// Builds the NCHW, normalized detector input for one image.
fn preprocess(image: &DynamicImage) -> Vec<f32> {
    let rgb = image
        .resize_exact(INPUT_WIDTH, INPUT_HEIGHT, FilterType::Triangle)
        .to_rgb8();
    let plane = (INPUT_WIDTH * INPUT_HEIGHT) as usize;
    let mut data = vec![0.0f32; plane * 3];
    for (index, pixel) in rgb.pixels().enumerate() {
        data[index] = (f32::from(pixel[0]) - NORM_MEAN) / NORM_SCALE;
        data[plane + index] = (f32::from(pixel[1]) - NORM_MEAN) / NORM_SCALE;
        data[2 * plane + index] = (f32::from(pixel[2]) - NORM_MEAN) / NORM_SCALE;
    }
    data
}

/// Converts a normalized `[x1, y1, x2, y2]` box to a pixel box in a `w`x`h` image.
///
/// Returns `None` for a degenerate or out-of-order box, so a malformed model
/// output can never yield a zero-area crop.
fn scale_box(norm: [f32; 4], width: u32, height: u32) -> Option<BoundingBox> {
    let x1 = (norm[0].clamp(0.0, 1.0) * width as f32).round() as u32;
    let y1 = (norm[1].clamp(0.0, 1.0) * height as f32).round() as u32;
    let x2 = (norm[2].clamp(0.0, 1.0) * width as f32).round() as u32;
    let y2 = (norm[3].clamp(0.0, 1.0) * height as f32).round() as u32;
    if x2 <= x1 || y2 <= y1 {
        return None;
    }
    Some(BoundingBox {
        height: y2 - y1,
        width: x2 - x1,
        x: x1,
        y: y1,
    })
}

/// Intersection-over-union of two boxes, in `0.0..=1.0`.
fn iou(a: BoundingBox, b: BoundingBox) -> f32 {
    let ax2 = a.x + a.width;
    let ay2 = a.y + a.height;
    let bx2 = b.x + b.width;
    let by2 = b.y + b.height;
    let ix1 = a.x.max(b.x);
    let iy1 = a.y.max(b.y);
    let ix2 = ax2.min(bx2);
    let iy2 = ay2.min(by2);
    if ix2 <= ix1 || iy2 <= iy1 {
        return 0.0;
    }
    let intersection = (ix2 - ix1) as f32 * (iy2 - iy1) as f32;
    let union = (a.width * a.height) as f32 + (b.width * b.height) as f32 - intersection;
    if union <= 0.0 {
        return 0.0;
    }
    intersection / union
}

/// Greedy non-maximum suppression: keeps the highest-scoring box, drops any box
/// overlapping it beyond `iou_threshold`, and repeats.
fn nms(mut detections: Vec<Detection>, iou_threshold: f32) -> Vec<Detection> {
    detections.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
    let mut kept: Vec<Detection> = Vec::new();
    for candidate in detections {
        if kept
            .iter()
            .all(|k| iou(k.bbox, candidate.bbox) <= iou_threshold)
        {
            kept.push(candidate);
        }
    }
    kept
}

/// Expands a box by [`CROP_MARGIN`] on each side, clamped to a `w`x`h` image.
fn expand(bbox: BoundingBox, width: u32, height: u32) -> BoundingBox {
    let margin_x = (bbox.width as f32 * CROP_MARGIN).round() as u32;
    let margin_y = (bbox.height as f32 * CROP_MARGIN).round() as u32;
    let x = bbox.x.saturating_sub(margin_x);
    let y = bbox.y.saturating_sub(margin_y);
    let x2 = (bbox.x + bbox.width + margin_x).min(width);
    let y2 = (bbox.y + bbox.height + margin_y).min(height);
    BoundingBox {
        height: y2 - y,
        width: x2 - x,
        x,
        y,
    }
}

/// Crops `image` to a face box expanded by [`CROP_MARGIN`].
pub fn crop_face(image: &DynamicImage, bbox: BoundingBox) -> DynamicImage {
    let (width, height) = image.dimensions();
    let region = expand(bbox, width, height);
    image.crop_imm(region.x, region.y, region.width, region.height)
}

impl Detector {
    /// Loads a detector from an ONNX file.
    pub fn load(model_path: &std::path::Path) -> Result<Self> {
        let session = Session::builder()
            .map_err(map_ort)?
            .commit_from_file(model_path)
            .map_err(map_ort)?;
        Ok(Detector { session })
    }

    /// Returns every face detected above [`DEFAULT_CONFIDENCE`], suppressed by NMS.
    pub fn detect(&mut self, image: &DynamicImage) -> Result<Vec<Detection>> {
        let (width, height) = image.dimensions();
        let data = preprocess(image);
        let shape = [1usize, 3, INPUT_HEIGHT as usize, INPUT_WIDTH as usize];
        let tensor = Tensor::from_array((shape, data)).map_err(map_ort)?;
        let outputs = self
            .session
            .run(ort::inputs![INPUT_NAME => tensor])
            .map_err(map_ort)?;
        let (_scores_shape, scores) = outputs[SCORES_NAME]
            .try_extract_tensor::<f32>()
            .map_err(map_ort)?;
        let (_boxes_shape, boxes) = outputs[BOXES_NAME]
            .try_extract_tensor::<f32>()
            .map_err(map_ort)?;
        Ok(postprocess(scores, boxes, width, height))
    }

    /// Returns the highest-confidence face, or `None` if none clears the cutoff.
    pub fn best(&mut self, image: &DynamicImage) -> Result<Option<Detection>> {
        let mut detections = self.detect(image)?;
        detections.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
        Ok(detections.into_iter().next())
    }
}

/// Turns raw `scores`/`boxes` tensors into pixel-space detections after NMS.
fn postprocess(scores: &[f32], boxes: &[f32], width: u32, height: u32) -> Vec<Detection> {
    let anchors = (scores.len() / 2).min(boxes.len() / 4);
    let mut detections = Vec::new();
    for anchor in 0..anchors {
        let confidence = scores[anchor * 2 + 1];
        if confidence < DEFAULT_CONFIDENCE {
            continue;
        }
        let raw = [
            boxes[anchor * 4],
            boxes[anchor * 4 + 1],
            boxes[anchor * 4 + 2],
            boxes[anchor * 4 + 3],
        ];
        if let Some(bbox) = scale_box(raw, width, height) {
            detections.push(Detection { bbox, confidence });
        }
    }
    nms(detections, DEFAULT_IOU)
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    fn bbox(x: u32, y: u32, width: u32, height: u32) -> BoundingBox {
        BoundingBox {
            height,
            width,
            x,
            y,
        }
    }

    #[test]
    fn ultraface_spec_is_pinned() {
        assert_eq!(ULTRAFACE.sha256.len(), 64);
        assert!(ULTRAFACE.url.starts_with("https://"));
    }

    #[test]
    fn preprocess_has_nchw_length_and_normalized_black() {
        let image = DynamicImage::new_rgb8(50, 70);
        let data = preprocess(&image);
        assert_eq!(data.len(), 3 * (INPUT_WIDTH * INPUT_HEIGHT) as usize);
        let expected = -NORM_MEAN / NORM_SCALE;
        assert!(data.iter().all(|&v| (v - expected).abs() < 1e-6));
    }

    #[test]
    fn scale_box_maps_to_pixels() {
        let b = scale_box([0.25, 0.5, 0.75, 1.0], 320, 240).unwrap();
        assert_eq!(b, bbox(80, 120, 160, 120));
    }

    #[test]
    fn scale_box_rejects_degenerate() {
        assert_eq!(scale_box([0.5, 0.5, 0.5, 0.9], 100, 100), None);
        assert_eq!(scale_box([0.9, 0.1, 0.2, 0.9], 100, 100), None);
    }

    #[test]
    fn iou_of_identical_boxes_is_one() {
        let b = bbox(0, 0, 10, 10);
        assert!((iou(b, b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn iou_of_disjoint_boxes_is_zero() {
        assert_eq!(iou(bbox(0, 0, 10, 10), bbox(20, 20, 10, 10)), 0.0);
    }

    #[test]
    fn iou_of_half_overlap() {
        // Two 10x10 boxes overlapping in a 5x10 strip: intersection 50, union 150.
        let value = iou(bbox(0, 0, 10, 10), bbox(5, 0, 10, 10));
        assert!((value - (50.0 / 150.0)).abs() < 1e-6);
    }

    #[test]
    fn nms_suppresses_overlapping_lower_score() {
        let dets = vec![
            Detection {
                bbox: bbox(0, 0, 10, 10),
                confidence: 0.9,
            },
            Detection {
                bbox: bbox(1, 1, 10, 10),
                confidence: 0.8,
            },
            Detection {
                bbox: bbox(50, 50, 10, 10),
                confidence: 0.7,
            },
        ];
        let kept = nms(dets, DEFAULT_IOU);
        assert_eq!(kept.len(), 2);
        assert!((kept[0].confidence - 0.9).abs() < 1e-6);
    }

    #[test]
    fn expand_clamps_to_image_bounds() {
        let region = expand(bbox(2, 2, 4, 4), 8, 8);
        // 20% of 4 rounds to 1px each side, clamped within 8x8.
        assert_eq!(region, bbox(1, 1, 6, 6));
    }

    #[test]
    fn postprocess_keeps_confident_boxes() {
        // Two anchors: one face above cutoff, one below.
        let scores = [0.9, 0.1, 0.2, 0.95];
        let boxes = [0.0, 0.0, 0.5, 0.5, 0.1, 0.1, 0.4, 0.4];
        let dets = postprocess(&scores, &boxes, 100, 100);
        assert_eq!(dets.len(), 1);
        assert_eq!(dets[0].bbox, bbox(10, 10, 30, 30));
    }
}
