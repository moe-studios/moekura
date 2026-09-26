//! The tagger: suggests tags and a rating for posts with a WD-tagger
//! ONNX model, run by `moekura tagger` (the app's `tagger` feature).
//!
//! ONNX Runtime is loaded when the tagger starts ([`load_runtime`]), so
//! building this crate needs neither the library nor a download.

mod download;
mod job;
mod model;
#[cfg(test)]
mod tiny;

pub use crate::download::{ModelFiles, ensure};
pub use crate::job::TaggerJobs;
pub use crate::model::{Model, Predict, input_pixels, load_runtime};

#[derive(Debug, thiserror::Error)]
pub enum TaggerError {
    #[error("ONNX Runtime could not be loaded: {0}")]
    Runtime(String),
    #[error("downloading the model: {0}")]
    Download(String),
    #[error("loading the model: {0}")]
    Model(String),
    #[error("running the model: {0}")]
    Inference(String),
}
