//! The `Embedder` trait and its ONNX Runtime implementation (Spec 04, Chunking, embedding and
//! indexing). The `Reranker` arrives with retrieval in Spec 05.
//!
//! Embedding input is passage text and nothing else; queries are embedded the same way, with no
//! instruction prefix. Vectors are L2-normalized for cosine distance.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;
use sha2::{Digest, Sha256};
use tokenizers::{Tokenizer, TruncationParams};

/// The dimension of bge-m3's dense vectors.
pub const EMBED_DIM: usize = 1024;

/// The version of how vectors are computed from text, beyond the model itself. It is part of
/// a talk's index hash and is stored on each text unit, so raising it makes `wt reindex --all`
/// and `wt text reembed` re-embed everything once. 2: one text per run, so a vector depends on
/// its text alone (decision of October 9, 2026).
pub const VECTOR_VERSION: i32 = 2;

/// Why a model failed.
#[derive(Debug, thiserror::Error)]
pub enum MlError {
    /// The model directory or one of its files is missing.
    #[error("model files: {0}")]
    Files(String),
    /// ONNX Runtime could not be loaded or refused.
    #[error("ONNX Runtime: {0}")]
    Runtime(String),
    /// The tokenizer refused.
    #[error("tokenizer: {0}")]
    Tokenizer(String),
    /// The model returned something other than what an embedder returns.
    #[error("model output: {0}")]
    Output(String),
}

/// Turns passage or query text into unit vectors.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// The name stored in `passage.embedding_model`.
    fn model_id(&self) -> &str;
    /// The model and [`VECTOR_VERSION`]: what an index hash records, so a change to either
    /// makes stored vectors stale.
    fn vector_id(&self) -> String {
        format!("{}#v{VECTOR_VERSION}", self.model_id())
    }
    /// Vector length: 1024 for bge-m3.
    fn dim(&self) -> usize;
    /// One L2-normalized vector per text, in order.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, MlError>;
}

/// Scales `v` to unit length; a zero vector stays zero.
pub fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Where the embedder runs.
#[derive(Debug, Clone)]
pub struct OnnxSettings {
    /// `WT_MODEL_DIR`.
    pub model_dir: PathBuf,
    /// The model's directory name under `model_dir`, such as `bge-m3-int8` (`WT_EMBED_MODEL`).
    pub model: String,
    /// The ONNX Runtime library; when `None`, `ORT_DYLIB_PATH` or the system search path.
    pub runtime: Option<PathBuf>,
    /// Tokens per text; longer texts are truncated.
    pub max_tokens: usize,
    /// Threads inside one run.
    pub threads: usize,
}

impl OnnxSettings {
    /// Spec 04's CPU defaults for `model` under `model_dir`.
    pub fn new(model_dir: impl Into<PathBuf>, model: impl Into<String>) -> Self {
        Self {
            model_dir: model_dir.into(),
            model: model.into(),
            runtime: None,
            max_tokens: 512,
            threads: std::thread::available_parallelism().map_or(4, |n| n.get().min(8)),
        }
    }
}

static RUNTIME: OnceLock<Result<(), String>> = OnceLock::new();

/// Loads ONNX Runtime once per process.
fn load_runtime(path: Option<&Path>) -> Result<(), MlError> {
    RUNTIME
        .get_or_init(|| match path {
            Some(p) => ort::init_from(p)
                .map(|b| {
                    b.with_name("wt").commit();
                })
                .map_err(|e| format!("loading {}: {e}", p.display())),
            None => {
                ort::init().with_name("wt").commit();
                Ok(())
            }
        })
        .clone()
        .map_err(MlError::Runtime)
}

fn runtime_err(e: impl std::fmt::Display) -> MlError {
    MlError::Runtime(e.to_string())
}

/// bge-m3 exported to ONNX by `scripts/export_models.py`: `model.onnx` and `tokenizer.json` in
/// one directory. The dense vector is the first token's hidden state, normalized.
pub struct OnnxEmbedder {
    model_id: String,
    session: Arc<Mutex<Session>>,
    tokenizer: Arc<Tokenizer>,
}

impl std::fmt::Debug for OnnxEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnnxEmbedder")
            .field("model_id", &self.model_id)
            .finish_non_exhaustive()
    }
}

impl OnnxEmbedder {
    /// Loads the model and its tokenizer, failing at startup when either is missing.
    pub fn load(settings: &OnnxSettings) -> Result<Self, MlError> {
        let dir = settings.model_dir.join(&settings.model);
        let model = dir.join("model.onnx");
        let tokenizer_file = dir.join("tokenizer.json");
        for f in [&model, &tokenizer_file] {
            if !f.is_file() {
                return Err(MlError::Files(format!(
                    "{} is missing; run scripts/export_models.py",
                    f.display()
                )));
            }
        }
        load_runtime(settings.runtime.as_deref())?;
        let session = Session::builder()
            .map_err(runtime_err)?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(runtime_err)?
            .with_intra_threads(settings.threads.max(1))
            .map_err(runtime_err)?
            .commit_from_file(&model)
            .map_err(runtime_err)?;
        let mut tokenizer =
            Tokenizer::from_file(&tokenizer_file).map_err(|e| MlError::Tokenizer(e.to_string()))?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: settings.max_tokens,
                ..TruncationParams::default()
            }))
            .map_err(|e| MlError::Tokenizer(e.to_string()))?;
        Ok(Self {
            model_id: settings.model.clone(),
            session: Arc::new(Mutex::new(session)),
            tokenizer: Arc::new(tokenizer),
        })
    }
}

/// Embeds one text in its own run on the calling thread. Every text gets a run of its own:
/// the int8 export quantizes activations dynamically, with one scale per tensor across the whole
/// run, padding included, so beside other texts a vector moved (cosine 0.97 to 0.99 against the
/// same text alone). Queries are embedded alone, so passages are too, and there is no batch
/// setting to change that (decision of October 9, 2026). It is slower than batching: on a
/// random sample of passages on the owner's CPU, about 1.5 against 2.3 passages a second.
fn embed_one(
    session: &Mutex<Session>,
    tokenizer: &Tokenizer,
    text: &str,
) -> Result<Vec<f32>, MlError> {
    let encoding = tokenizer
        .encode(text, true)
        .map_err(|e| MlError::Tokenizer(e.to_string()))?;
    let ids: Vec<i64> = encoding.get_ids().iter().map(|&i| i64::from(i)).collect();
    let mask: Vec<i64> = encoding
        .get_attention_mask()
        .iter()
        .map(|&m| i64::from(m))
        .collect();
    let shape = [1, ids.len()];
    let ids = Tensor::from_array((shape, ids)).map_err(runtime_err)?;
    let mask = Tensor::from_array((shape, mask)).map_err(runtime_err)?;
    let mut session = session
        .lock()
        .map_err(|_| MlError::Runtime("a previous run panicked".into()))?;
    let output_name = session
        .outputs()
        .first()
        .map(|o| o.name().to_owned())
        .ok_or_else(|| MlError::Output("the model has no outputs".into()))?;
    let outputs = session
        .run(ort::inputs!["input_ids" => ids, "attention_mask" => mask])
        .map_err(runtime_err)?;
    let (shape, data) = outputs[output_name.as_str()]
        .try_extract_tensor::<f32>()
        .map_err(runtime_err)?;
    let dims: Vec<usize> = shape
        .iter()
        .map(|&d| usize::try_from(d).unwrap_or(0))
        .collect();
    // Hidden states [1, tokens, dim] give the first token's; a pooled [1, dim] is used as is.
    let dim = match dims.as_slice() {
        [1, _, d] | [1, d] => *d,
        other => return Err(MlError::Output(format!("unexpected shape {other:?}"))),
    };
    if dim != EMBED_DIM {
        return Err(MlError::Output(format!(
            "vectors have {dim} dimensions, not {EMBED_DIM}"
        )));
    }
    let mut v = data[..dim].to_vec();
    l2_normalize(&mut v);
    Ok(v)
}

#[async_trait]
impl Embedder for OnnxEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        EMBED_DIM
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, MlError> {
        let mut out = Vec::with_capacity(texts.len());
        for text in texts {
            let (session, tokenizer, text) = (
                Arc::clone(&self.session),
                Arc::clone(&self.tokenizer),
                text.clone(),
            );
            let vector =
                tokio::task::spawn_blocking(move || embed_one(&session, &tokenizer, &text))
                    .await
                    .map_err(|e| MlError::Runtime(format!("embedding task: {e}")))??;
            out.push(vector);
        }
        Ok(out)
    }
}

/// A deterministic stand-in for tests: a hashed bag of lowercase words, normalized, so texts
/// that share words are near each other.
#[derive(Debug, Clone)]
pub struct FakeEmbedder {
    model_id: String,
}

impl FakeEmbedder {
    /// A fake that records `model_id` on passages.
    pub fn new(model_id: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
        }
    }

    /// The vector for one text.
    pub fn vector(text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; EMBED_DIM];
        for word in text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
        {
            let h = Sha256::digest(word.to_lowercase().as_bytes());
            let i = usize::from(u16::from_be_bytes([h[0], h[1]])) % EMBED_DIM;
            v[i] += if h[2] & 1 == 0 { 1.0 } else { -1.0 };
        }
        l2_normalize(&mut v);
        v
    }
}

#[async_trait]
impl Embedder for FakeEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        EMBED_DIM
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, MlError> {
        Ok(texts.iter().map(|t| Self::vector(t)).collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[tokio::test]
    async fn the_fake_returns_unit_vectors_that_repeat() {
        let e = FakeEmbedder::new("fake");
        let texts = vec![
            "Sitting is enough.".to_owned(),
            "sitting IS enough".to_owned(),
        ];
        let a = e.embed(&texts).await.unwrap();
        let b = e.embed(&texts).await.unwrap();
        assert_eq!(a, b);
        assert_eq!(a[0].len(), EMBED_DIM);
        let norm: f32 = a[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
        assert_eq!(
            a[0], a[1],
            "case and punctuation do not change the fake's vector"
        );
    }

    #[test]
    fn a_missing_model_fails_at_load_naming_the_file() {
        let s = OnnxSettings::new(std::env::temp_dir().join("wt-no-models"), "bge-m3-int8");
        let err = OnnxEmbedder::load(&s).unwrap_err().to_string();
        assert!(
            err.contains("model.onnx") && err.contains("export_models.py"),
            "{err}"
        );
    }
}
