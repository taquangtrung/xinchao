//! The ONNX face-recognition pipeline: face detection, embedding, the
//! accept/reject comparison, and acquisition of the pinned model files.

pub mod detect;
pub mod embed;
pub mod models;
pub mod recognize;
