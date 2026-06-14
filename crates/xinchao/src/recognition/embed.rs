//! Face embedding via an ArcFace ONNX model, run with ONNX Runtime (`ort`).
//!
//! [`Embedder`] loads a recognition model and maps a face image to an
//! [`Embedding`]. The default model is the permissively licensed ArcFace ResNet
//! from the ONNX Model Zoo ([`ARCFACE`]); it is fetched on first use via
//! [`crate::recognition::models`] rather than vendored.
//!
//! Preprocessing resizes to 112x112 RGB and feeds raw 0-255 pixel values: this
//! ONNX Model Zoo ArcFace has its input normalization built in, so pre-scaling
//! the pixels collapses the embeddings (verified empirically).

use std::path::Path;

use image::imageops::FilterType;
use image::DynamicImage;
use ort::session::Session;
use ort::value::Tensor;
use serde::Deserialize;
use serde::Serialize;

use crate::error::Error;
use crate::error::Result;
use crate::recognition::models::ModelSpec;
use crate::recognition::recognize::Embedding;

// Constants

/// ArcFace ResNet100 from the ONNX Model Zoo: most accurate, ~260MB, slow to load.
pub const ARCFACE: ModelSpec = ModelSpec {
    file_name: "arcfaceresnet100-8.onnx",
    license: "Apache-2.0 (ONNX Model Zoo)",
    sha256: "f3a6bc281e72f88862f5748b53be3d76b3b48f8f1ab1f4a537941bdc4e1b01da",
    url: "https://github.com/onnx/models/raw/main/validated/vision/body_analysis/arcface/model/arcfaceresnet100-8.onnx",
};

/// SFace from the OpenCV Model Zoo: lightweight (~38MB), fast to load, plenty
/// accurate for 1:1 unlock. Same 112x112 raw-pixel input as ArcFace.
pub const SFACE: ModelSpec = ModelSpec {
    file_name: "face_recognition_sface_2021dec.onnx",
    license: "Apache-2.0 (OpenCV Model Zoo)",
    sha256: "0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79",
    url: "https://github.com/opencv/opencv_zoo/raw/main/models/face_recognition_sface/face_recognition_sface_2021dec.onnx",
};

/// Square input edge, in pixels, expected by both models.
const INPUT_SIZE: u32 = 112;

/// Name of the models' input tensor (shared by ArcFace and SFace).
const INPUT_NAME: &str = "data";

// Data Structures

/// The recognition model to embed with.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RecognitionModel {
    /// ArcFace ResNet-100: most accurate, but ~260MB and slow to load.
    #[default]
    Arcface,
    /// SFace: lightweight and fast, recommended for face unlock.
    Sface,
}

/// A loaded recognition model that turns face images into embeddings.
pub struct Embedder {
    session: Session,
}

impl RecognitionModel {
    /// The downloadable spec (file, URL, checksum) for this model.
    pub fn spec(self) -> &'static ModelSpec {
        match self {
            RecognitionModel::Arcface => &ARCFACE,
            RecognitionModel::Sface => &SFACE,
        }
    }
}

// Functions

/// Maps an `ort` error into the core error type.
fn map_ort(error: ort::Error) -> Error {
    Error::Recognition(format!("onnx runtime: {error}"))
}

/// Builds the NCHW, normalized input tensor data for one image.
fn preprocess(image: &DynamicImage) -> Vec<f32> {
    let rgb = image
        .resize_exact(INPUT_SIZE, INPUT_SIZE, FilterType::Triangle)
        .to_rgb8();
    let plane = (INPUT_SIZE * INPUT_SIZE) as usize;
    let mut data = vec![0.0f32; plane * 3];
    for (index, pixel) in rgb.pixels().enumerate() {
        data[index] = f32::from(pixel[0]);
        data[plane + index] = f32::from(pixel[1]);
        data[2 * plane + index] = f32::from(pixel[2]);
    }
    data
}

impl Embedder {
    /// Loads a recognition model from an ONNX file.
    pub fn load(model_path: &Path) -> Result<Self> {
        let session = Session::builder()
            .map_err(map_ort)?
            .commit_from_file(model_path)
            .map_err(map_ort)?;
        Ok(Embedder { session })
    }

    /// Computes the embedding of a face image.
    ///
    /// The image is resized to the model's input size; callers that have a face
    /// bounding box should crop before calling so background does not dominate.
    pub fn embed(&mut self, image: &DynamicImage) -> Result<Embedding> {
        let data = preprocess(image);
        let shape = [1usize, 3, INPUT_SIZE as usize, INPUT_SIZE as usize];
        let tensor = Tensor::from_array((shape, data)).map_err(map_ort)?;
        let outputs = self
            .session
            .run(ort::inputs![INPUT_NAME => tensor])
            .map_err(map_ort)?;
        let (_shape, values) = outputs[0].try_extract_tensor::<f32>().map_err(map_ort)?;
        if values.is_empty() {
            return Err(Error::Recognition(
                "model returned an empty embedding".to_string(),
            ));
        }
        Ok(Embedding::new(values.to_vec()))
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preprocess_has_nchw_length_and_raw_pixel_range() {
        let image = DynamicImage::new_rgb8(64, 48);
        let data = preprocess(&image);
        assert_eq!(data.len(), 3 * (INPUT_SIZE * INPUT_SIZE) as usize);
        // Pixels are fed raw (the model normalizes internally); a black image is all zeros.
        assert!(data.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn arcface_spec_is_pinned() {
        assert_eq!(ARCFACE.sha256.len(), 64);
        assert!(ARCFACE.url.starts_with("https://"));
    }
}
