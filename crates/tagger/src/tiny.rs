//! A tiny WD-tagger-shaped model, for tests that run ONNX Runtime for
//! real (when `ORT_DYLIB_PATH` points at it; CI sets it).
//!
//! It takes `[batch, 8, 8, 3]` BGR images like the real ones and scores
//! seven labels from the image's mean colour: the four ratings (general
//! always wins), `red_theme` and `blue_theme` for red and blue images, and
//! the character `red_character` for images with a lot of red. The ONNX
//! file is written here, protobuf by hand, so the repository needs no
//! binary fixture.

use std::path::Path;
use std::sync::Arc;

use moekura_core::posts::Rating;
use moekura_db::tag_suggestions;
use sqlx::PgPool;

use crate::job::tests::processed_post;
use crate::{Model, Predict, load_runtime};

pub const TAGS_CSV: &str = "tag_id,name,category,count
1,general,9,0
2,sensitive,9,0
3,questionable,9,0
4,explicit,9,0
5,red_theme,0,0
6,blue_theme,0,0
7,red_character,4,0
";

/// Per label: weights for the mean blue, green and red (0–255), and a
/// bias; the score is the sigmoid of their sum.
const WEIGHTS: [([f32; 3], f32); 7] = [
    ([0.0, 0.0, 0.0], 2.0),
    ([0.0, 0.0, 0.0], 0.0),
    ([0.0, 0.0, 0.0], -2.0),
    ([0.0, 0.0, 0.0], -4.0),
    ([-8.0 / 255.0, 0.0, 8.0 / 255.0], -2.0),
    ([8.0 / 255.0, 0.0, -8.0 / 255.0], -2.0),
    ([0.0, 0.0, 6.0 / 255.0], -2.0),
];

/// The model as ONNX: ReduceMean over height and width, MatMul, Add,
/// Sigmoid.
pub fn model_onnx() -> Vec<u8> {
    let labels = WEIGHTS.len() as i64;
    let mut weights = Vec::new();
    for channel in 0..3 {
        for (w, _) in WEIGHTS {
            weights.push(w[channel]);
        }
    }
    let biases: Vec<f32> = WEIGHTS.iter().map(|(_, b)| *b).collect();

    let mut graph = Vec::new();
    node(
        &mut graph,
        "ReduceMean",
        &["input"],
        "mean",
        Some(("axes", &[1, 2])),
        Some(("keepdims", 0)),
    );
    node(&mut graph, "MatMul", &["mean", "w"], "scaled", None, None);
    node(&mut graph, "Add", &["scaled", "b"], "logits", None, None);
    node(&mut graph, "Sigmoid", &["logits"], "output", None, None);
    string(&mut graph, 2, "tiny");
    tensor(&mut graph, "w", &[3, labels], &weights);
    tensor(&mut graph, "b", &[labels], &biases);
    value_info(
        &mut graph,
        11,
        "input",
        &[
            Dim::Param("batch"),
            Dim::Value(8),
            Dim::Value(8),
            Dim::Value(3),
        ],
    );
    value_info(
        &mut graph,
        12,
        "output",
        &[Dim::Param("batch"), Dim::Value(labels)],
    );

    let mut model = Vec::new();
    int(&mut model, 1, 8); // ir_version
    string(&mut model, 2, "moekura-tests"); // producer_name
    message(&mut model, 8, |opset| {
        string(opset, 1, "");
        int(opset, 2, 13);
    });
    message(&mut model, 7, |g| g.extend_from_slice(&graph));
    model
}

enum Dim<'a> {
    Value(i64),
    Param(&'a str),
}

fn varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn int(out: &mut Vec<u8>, field: u64, value: i64) {
    varint(out, field << 3);
    varint(out, value as u64);
}

fn bytes(out: &mut Vec<u8>, field: u64, data: &[u8]) {
    varint(out, field << 3 | 2);
    varint(out, data.len() as u64);
    out.extend_from_slice(data);
}

fn string(out: &mut Vec<u8>, field: u64, text: &str) {
    bytes(out, field, text.as_bytes());
}

fn message(out: &mut Vec<u8>, field: u64, build: impl FnOnce(&mut Vec<u8>)) {
    let mut inner = Vec::new();
    build(&mut inner);
    bytes(out, field, &inner);
}

/// A NodeProto (graph field 1), with at most one ints and one int
/// attribute.
fn node(
    graph: &mut Vec<u8>,
    op: &str,
    inputs: &[&str],
    output: &str,
    ints: Option<(&str, &[i64])>,
    single: Option<(&str, i64)>,
) {
    message(graph, 1, |n| {
        for input in inputs {
            string(n, 1, input);
        }
        string(n, 2, output);
        string(n, 3, output);
        string(n, 4, op);
        if let Some((name, values)) = ints {
            message(n, 5, |a| {
                string(a, 1, name);
                for &v in values {
                    int(a, 8, v);
                }
                int(a, 20, 7); // INTS
            });
        }
        if let Some((name, value)) = single {
            message(n, 5, |a| {
                string(a, 1, name);
                int(a, 3, value);
                int(a, 20, 2); // INT
            });
        }
    });
}

/// A float TensorProto initializer (graph field 5).
fn tensor(graph: &mut Vec<u8>, name: &str, dims: &[i64], values: &[f32]) {
    message(graph, 5, |t| {
        for &d in dims {
            int(t, 1, d);
        }
        int(t, 2, 1); // FLOAT
        string(t, 8, name);
        let raw: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        bytes(t, 9, &raw);
    });
}

/// A float tensor ValueInfoProto (graph input 11 or output 12).
fn value_info(graph: &mut Vec<u8>, field: u64, name: &str, dims: &[Dim<'_>]) {
    message(graph, field, |v| {
        string(v, 1, name);
        message(v, 2, |ty| {
            message(ty, 1, |tensor| {
                int(tensor, 1, 1); // FLOAT
                message(tensor, 2, |shape| {
                    for dim in dims {
                        message(shape, 1, |d| match dim {
                            Dim::Value(n) => int(d, 1, *n),
                            Dim::Param(p) => string(d, 2, p),
                        });
                    }
                });
            });
        });
    });
}

/// Writes the model and its tag list to `dir`, returning their paths.
pub fn write(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    std::fs::create_dir_all(dir).unwrap();
    let (model, tags) = (dir.join("model.onnx"), dir.join("selected_tags.csv"));
    std::fs::write(&model, model_onnx()).unwrap();
    std::fs::write(&tags, TAGS_CSV).unwrap();
    (model, tags)
}

/// ONNX Runtime, if the environment says where it is.
fn runtime() -> bool {
    if std::env::var_os("ORT_DYLIB_PATH").is_none_or(|p| p.is_empty()) {
        eprintln!("skipped: set ORT_DYLIB_PATH to an ONNX Runtime library to run this test");
        return false;
    }
    load_runtime(None).expect("ONNX Runtime loads");
    true
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("moekura-tiny-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn runs_the_tiny_model() {
    if !runtime() {
        return;
    }
    let dir = scratch("run");
    let (model, tags) = write(&dir);
    let model = Model::load("tiny", &model, &tags, 1).unwrap();
    assert_eq!(model.input_size(), 8);

    let solid = |rgb: [u8; 3]| moekura_media::RgbImage {
        width: 8,
        height: 8,
        pixels: rgb.repeat(64),
    };
    let red = model.predict(&solid([255, 0, 0]), 0.5).unwrap();
    assert_eq!(red.rating.map(|r| r.0), Some(Rating::General));
    let names: Vec<&str> = red.tags.iter().map(|t| t.0.as_str()).collect();
    assert_eq!(names, ["red_theme", "red_character"]);
    let blue = model.predict(&solid([0, 0, 255]), 0.5).unwrap();
    let names: Vec<&str> = blue.tags.iter().map(|t| t.0.as_str()).collect();
    assert_eq!(names, ["blue_theme"]);
}

#[test]
fn refuses_a_tag_list_that_doesnt_fit() {
    if !runtime() {
        return;
    }
    let dir = scratch("mismatch");
    let (model, tags) = write(&dir);
    std::fs::write(&tags, "tag_id,name,category,count\n1,general,9,0\n").unwrap();
    let error = Model::load("tiny", &model, &tags, 1)
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("scores 7 tags"), "{error}");
}

#[sqlx::test(migrator = "moekura_db::MIGRATOR")]
async fn tags_a_post_with_the_tiny_model(pool: PgPool) {
    if !runtime() {
        return;
    }
    let dir = scratch("post");
    let (model, tags) = write(&dir.join("model"));
    let model = Arc::new(Model::load("tiny", &model, &tags, 1).unwrap());
    let (jobs, post_id) = processed_post(&pool, &dir, model).await;
    jobs.tag(post_id).await.unwrap();

    let suggestions: Vec<(String, i16)> = tag_suggestions::for_post(&pool, post_id)
        .await
        .unwrap()
        .into_iter()
        .map(|s| (s.name, s.category_id))
        .collect();
    assert_eq!(
        suggestions,
        [("red_character".to_owned(), 4), ("red_theme".to_owned(), 0)]
    );
    let result = tag_suggestions::result(&pool, post_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (result.model.as_str(), result.rating),
        ("tiny", Rating::General)
    );
}
