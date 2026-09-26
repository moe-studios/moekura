//! Running a WD-tagger model with ONNX Runtime.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use moekura_core::tagger::{Label, Prediction, interpret, parse_labels};
use moekura_media::RgbImage;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::{Tensor, ValueType};

use crate::TaggerError;

/// Loads ONNX Runtime from `library`, or else from `ORT_DYLIB_PATH` or
/// the system's library path, and returns where it looked. Only the first
/// call in a process loads anything; it logs the version found.
pub fn load_runtime(library: Option<&Path>) -> Result<PathBuf, TaggerError> {
    let path = library.map(Path::to_path_buf).unwrap_or_else(|| {
        std::env::var_os("ORT_DYLIB_PATH")
            .filter(|p| !p.is_empty())
            .map_or_else(|| PathBuf::from("libonnxruntime.so"), PathBuf::from)
    });
    let environment = ort::init_from(&path).map_err(|e| {
        TaggerError::Runtime(format!(
            "{e}. The -tagger image includes it; elsewhere, install ONNX Runtime \
             and point tagger.runtime or ORT_DYLIB_PATH at libonnxruntime.so"
        ))
    })?;
    environment.with_name("moekura").commit();
    Ok(path)
}

/// Something that suggests tags for images: the real model, or a stand-in
/// in tests.
pub trait Predict: Send + Sync + 'static {
    /// Recorded with the suggestions.
    fn model_name(&self) -> &str;
    /// Images are given as squares of this size.
    fn input_size(&self) -> u32;
    /// Tags scoring at least `floor`, and the rating. Blocks while the
    /// model runs.
    fn predict(&self, image: &RgbImage, floor: f32) -> Result<Prediction, TaggerError>;
}

/// A loaded model and its tag list.
pub struct Model {
    name: String,
    session: Mutex<Session>,
    input_size: u32,
    labels: Vec<Label>,
}

impl Model {
    /// Loads `model` (ONNX) and `tags` (its `selected_tags.csv`), using
    /// `threads` threads per image (`0`: one per core). Call
    /// [`load_runtime`] first.
    pub fn load(
        name: &str,
        model: &Path,
        tags: &Path,
        threads: usize,
    ) -> Result<Self, TaggerError> {
        let invalid =
            |message: String| TaggerError::Model(format!("{}: {message}", model.display()));
        let csv = std::fs::read_to_string(tags)
            .map_err(|e| TaggerError::Model(format!("{}: {e}", tags.display())))?;
        let labels = parse_labels(&csv)
            .map_err(|e| TaggerError::Model(format!("{}: {e}", tags.display())))?;
        let ort_error = |e: ort::Error| invalid(e.to_string());
        let session = Session::builder()
            .map_err(ort_error)?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| invalid(e.to_string()))?
            .with_intra_threads(threads)
            .map_err(|e| invalid(e.to_string()))?
            .commit_from_file(model)
            .map_err(ort_error)?;

        // WD taggers take a batch of square BGR images (NHWC) and give a
        // score per tag.
        let [input] = session.inputs() else {
            return Err(invalid("expected a model with one input".into()));
        };
        let input_size = match input.dtype() {
            ValueType::Tensor { shape, .. }
                if shape.len() == 4 && shape[3] == 3 && shape[1] == shape[2] && shape[1] > 0 =>
            {
                u32::try_from(shape[1]).map_err(|_| invalid("input too large".into()))?
            }
            other => {
                return Err(invalid(format!(
                    "expected an input of [batch, size, size, 3], not {other}"
                )));
            }
        };
        let [output] = session.outputs() else {
            return Err(invalid("expected a model with one output".into()));
        };
        if let ValueType::Tensor { shape, .. } = output.dtype()
            && let Some(&scores) = shape.last()
            && scores > 0
            && usize::try_from(scores).ok() != Some(labels.len())
        {
            return Err(invalid(format!(
                "the model scores {scores} tags, but its tag list names {}",
                labels.len()
            )));
        }
        Ok(Self {
            name: name.to_owned(),
            session: Mutex::new(session),
            input_size,
            labels,
        })
    }

    /// The model's raw scores for `image`, one per label.
    pub fn scores(&self, image: &RgbImage) -> Result<Vec<f32>, TaggerError> {
        let size = self.input_size;
        let side = i64::from(size);
        let input = Tensor::from_array(([1, side, side, 3], input_pixels(image, size)))
            .map_err(|e| TaggerError::Inference(e.to_string()))?;
        let mut session = self
            .session
            .lock()
            .map_err(|_| TaggerError::Inference("an earlier run panicked".into()))?;
        let outputs = session
            .run(ort::inputs![input])
            .map_err(|e| TaggerError::Inference(e.to_string()))?;
        let (_, scores) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| TaggerError::Inference(e.to_string()))?;
        if scores.len() != self.labels.len() {
            return Err(TaggerError::Inference(format!(
                "got {} scores for {} tags",
                scores.len(),
                self.labels.len()
            )));
        }
        Ok(scores.to_vec())
    }
}

impl Predict for Model {
    fn model_name(&self) -> &str {
        &self.name
    }

    fn input_size(&self) -> u32 {
        self.input_size
    }

    fn predict(&self, image: &RgbImage, floor: f32) -> Result<Prediction, TaggerError> {
        let scores = self.scores(image)?;
        Ok(interpret(&self.labels, &scores, floor))
    }
}

/// `image` centred on a white `size`×`size` square, as BGR values from 0
/// to 255, the way the WD taggers were trained. `image` must fit.
pub fn input_pixels(image: &RgbImage, size: u32) -> Vec<f32> {
    let size = size as usize;
    let (width, height) = (
        (image.width as usize).min(size),
        (image.height as usize).min(size),
    );
    let (left, top) = ((size - width) / 2, (size - height) / 2);
    let mut out = vec![255.0; size * size * 3];
    for y in 0..height {
        for x in 0..width {
            let from = (y * image.width as usize + x) * 3;
            let to = ((top + y) * size + left + x) * 3;
            let [r, g, b] = [0, 1, 2].map(|c| f32::from(image.pixels[from + c]));
            out[to..to + 3].copy_from_slice(&[b, g, r]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pads_to_a_white_square_in_bgr() {
        // One red pixel over one blue one.
        let image = RgbImage {
            width: 1,
            height: 2,
            pixels: vec![255, 0, 0, 0, 0, 255],
        };
        let input = input_pixels(&image, 2);
        #[rustfmt::skip]
        assert_eq!(input, [
            0.0, 0.0, 255.0,   255.0, 255.0, 255.0,
            255.0, 0.0, 0.0,   255.0, 255.0, 255.0,
        ]);
        // Centred.
        let input = input_pixels(&image, 4);
        assert_eq!(&input[(4 + 1) * 3..(4 + 1) * 3 + 3], [0.0, 0.0, 255.0]);
    }
}
