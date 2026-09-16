//! Embeddings for the `dense` and `hybrid` backends (SPEC §16.2).
//!
//! [`Embedder`] is the injectable endpoint: [`HttpEmbedder`] talks to an OpenAI-compatible
//! `POST {base}/embeddings` endpoint, and [`testing::FakeEmbedder`] is a deterministic,
//! network-free stand-in used by tests. [`embed_units`] batches a list of texts through an
//! [`Embedder`]; [`write_embeddings`] and [`read_embeddings`] are the `embeddings.bin` /
//! `embeddings.json` file pair described by the spec.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors raised while embedding texts or reading/writing the embeddings file pair.
#[derive(Debug, Error)]
pub enum EmbedError {
    /// A filesystem operation failed.
    #[error("{path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A required environment variable (or `--model`) was not set.
    #[error("{0} is not set")]
    MissingConfig(String),
    /// The embeddings endpoint returned an error status or could not be reached.
    #[error("{url}: {message}")]
    Http {
        /// The requested URL.
        url: String,
        /// What went wrong.
        message: String,
    },
    /// The endpoint's response was not the expected shape.
    #[error("{url}: unexpected response: {message}")]
    BadResponse {
        /// The requested URL.
        url: String,
        /// What was wrong with it.
        message: String,
    },
    /// `embeddings.json` could not be parsed.
    #[error("{path}: invalid embeddings manifest: {source}")]
    Json {
        /// The file path.
        path: PathBuf,
        /// Underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// `embeddings.bin`'s length does not match the manifest's unit count and dimension.
    #[error(
        "{path}: expected {expected} bytes ({units} units × {dimension} f32) but found {actual}"
    )]
    BadBinSize {
        /// The `embeddings.bin` path.
        path: PathBuf,
        /// Expected byte length.
        expected: usize,
        /// Actual byte length.
        actual: usize,
        /// Unit count from the manifest.
        units: usize,
        /// Dimension from the manifest.
        dimension: usize,
    },
    /// The recorded artifact manifest hash does not match the current one.
    #[error(
        "embeddings are stale: built from manifest {expected} but the artifact is now {actual}; \
         pass --allow-stale to use them anyway"
    )]
    Stale {
        /// The hash recorded in `embeddings.json`.
        expected: String,
        /// The artifact's current hash.
        actual: String,
    },
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> EmbedError + '_ {
    move |source| EmbedError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// A source of text embeddings, injectable so tests avoid the network (SPEC §16.2).
pub trait Embedder {
    /// Embed `inputs` with `model`, one vector per input, in order.
    fn embed(&self, model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>, EmbedError>;
}

/// Texts per embeddings request, per SPEC §16.2.
pub const DEFAULT_BATCH: usize = 64;
/// Retry attempts on HTTP 429 or 5xx before giving up.
const MAX_ATTEMPTS: u32 = 3;
/// Base backoff between retries; attempt `n` waits `n * BACKOFF_UNIT`.
const BACKOFF_UNIT: Duration = Duration::from_millis(200);
/// Request timeout.
const TIMEOUT: Duration = Duration::from_secs(30);

/// An OpenAI-compatible embeddings endpoint: `POST {base_url}/embeddings`.
pub struct HttpEmbedder {
    agent: ureq::Agent,
    base_url: String,
    api_key: Option<String>,
}

impl std::fmt::Debug for HttpEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpEmbedder")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "…"))
            .finish_non_exhaustive()
    }
}

impl HttpEmbedder {
    /// A client for `base_url` (no trailing slash required), with an optional bearer token.
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> HttpEmbedder {
        let agent = ureq::Agent::config_builder()
            .user_agent(concat!("pinakes/", env!("CARGO_PKG_VERSION")))
            .build()
            .new_agent();
        HttpEmbedder {
            agent,
            base_url: base_url.into(),
            api_key,
        }
    }

    /// Build a client and resolve the model name from `PINAKES_EMBED_URL`, `PINAKES_EMBED_KEY`
    /// and `PINAKES_EMBED_MODEL`, with `model_arg` (`--model`) overriding the model env var.
    /// Errors when the URL is unset, or no model is given at all: a missing endpoint is an
    /// error, not a silent skip.
    pub fn from_env(model_arg: Option<&str>) -> Result<(HttpEmbedder, String), EmbedError> {
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let base_url = env("PINAKES_EMBED_URL")
            .ok_or_else(|| EmbedError::MissingConfig("PINAKES_EMBED_URL".to_string()))?;
        let api_key = env("PINAKES_EMBED_KEY");
        let model = model_arg
            .map(str::to_string)
            .or_else(|| env("PINAKES_EMBED_MODEL"))
            .ok_or_else(|| {
                EmbedError::MissingConfig("PINAKES_EMBED_MODEL (or --model)".to_string())
            })?;
        Ok((HttpEmbedder::new(base_url, api_key), model))
    }
}

/// Parse an OpenAI-style `{"data": [{"embedding": [...]}, ...]}` response.
fn parse_response(
    url: &str,
    value: &serde_json::Value,
    expected: usize,
) -> Result<Vec<Vec<f32>>, EmbedError> {
    let data = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| EmbedError::BadResponse {
            url: url.to_string(),
            message: "missing \"data\" array".to_string(),
        })?;
    if data.len() != expected {
        return Err(EmbedError::BadResponse {
            url: url.to_string(),
            message: format!("expected {expected} embeddings, got {}", data.len()),
        });
    }
    data.iter()
        .map(|entry| {
            entry
                .get("embedding")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| EmbedError::BadResponse {
                    url: url.to_string(),
                    message: "entry missing \"embedding\" array".to_string(),
                })?
                .iter()
                .map(|v| {
                    v.as_f64()
                        .map(|f| {
                            #[allow(clippy::cast_possible_truncation)]
                            {
                                f as f32
                            }
                        })
                        .ok_or_else(|| EmbedError::BadResponse {
                            url: url.to_string(),
                            message: "embedding value is not a number".to_string(),
                        })
                })
                .collect()
        })
        .collect()
}

impl Embedder for HttpEmbedder {
    fn embed(&self, model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let url = format!("{}/embeddings", self.base_url.trim_end_matches('/'));
        let body = serde_json::json!({ "model": model, "input": inputs });
        for attempt in 1..=MAX_ATTEMPTS {
            let mut request = self
                .agent
                .post(&url)
                .config()
                .timeout_global(Some(TIMEOUT))
                .build()
                .header("content-type", "application/json");
            if let Some(key) = &self.api_key {
                request = request.header("authorization", format!("Bearer {key}"));
            }
            match request.send_json(&body) {
                Ok(response) => {
                    let value: serde_json::Value =
                        response
                            .into_body()
                            .read_json()
                            .map_err(|e| EmbedError::BadResponse {
                                url: url.clone(),
                                message: e.to_string(),
                            })?;
                    return parse_response(&url, &value, inputs.len());
                }
                Err(ureq::Error::StatusCode(code))
                    if is_retryable(code) && attempt < MAX_ATTEMPTS =>
                {
                    std::thread::sleep(BACKOFF_UNIT * attempt);
                }
                Err(err) => {
                    return Err(EmbedError::Http {
                        url,
                        message: err.to_string(),
                    });
                }
            }
        }
        unreachable!("the loop always returns by the last attempt")
    }
}

fn is_retryable(status: u16) -> bool {
    status == 429 || (500..600).contains(&status)
}

/// Embed `texts` through `embedder`, `batch` at a time, in order.
pub fn embed_units(
    embedder: &dyn Embedder,
    model: &str,
    texts: &[String],
    batch: usize,
) -> Result<Vec<Vec<f32>>, EmbedError> {
    let batch = batch.max(1);
    let mut vectors = Vec::with_capacity(texts.len());
    for chunk in texts.chunks(batch) {
        vectors.extend(embedder.embed(model, chunk)?);
    }
    Ok(vectors)
}

/// `embeddings.json`: what `embeddings.bin` holds and how it was produced (SPEC §16.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingsManifest {
    /// The embedding model used.
    pub model: String,
    /// Vector length.
    pub dimension: usize,
    /// Retrieval unit ids (`<source>::<path>`), one per row of `embeddings.bin`, in order.
    pub unit_ids: Vec<String>,
    /// `sha256` of the artifact's `manifest.json`, or `"none"` for a manifest-less artifact.
    pub manifest_sha256: String,
}

/// Sentinel `manifest_sha256` for an artifact with no `manifest.json`.
pub const NO_MANIFEST: &str = "none";

/// The `sha256` of `<artifact>/manifest.json`, or [`NO_MANIFEST`] when it does not exist.
pub fn artifact_manifest_hash(artifact: &Path) -> Result<String, EmbedError> {
    let path = artifact.join(crate::artifact::MANIFEST_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(crate::resolve::sha256_hex(&bytes)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(NO_MANIFEST.to_string()),
        Err(err) => Err(io(&path)(err)),
    }
}

/// Write `vectors` as little-endian row-major `f32` to `bin_path`, and `manifest` as pretty
/// JSON to `json_path`.
pub fn write_embeddings(
    bin_path: &Path,
    json_path: &Path,
    manifest: &EmbeddingsManifest,
    vectors: &[Vec<f32>],
) -> Result<(), EmbedError> {
    let mut bytes = Vec::with_capacity(vectors.len() * manifest.dimension * 4);
    for vector in vectors {
        for value in vector {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    std::fs::write(bin_path, bytes).map_err(io(bin_path))?;
    let mut text = serde_json::to_string_pretty(manifest).map_err(|source| EmbedError::Json {
        path: json_path.to_path_buf(),
        source,
    })?;
    text.push('\n');
    std::fs::write(json_path, text).map_err(io(json_path))
}

/// Read the `embeddings.json` / `embeddings.bin` pair back, checking that the binary file's
/// length matches the manifest.
pub fn read_embeddings(
    bin_path: &Path,
    json_path: &Path,
) -> Result<(EmbeddingsManifest, Vec<Vec<f32>>), EmbedError> {
    let text = std::fs::read_to_string(json_path).map_err(io(json_path))?;
    let manifest: EmbeddingsManifest =
        serde_json::from_str(&text).map_err(|source| EmbedError::Json {
            path: json_path.to_path_buf(),
            source,
        })?;
    let mut file = std::fs::File::open(bin_path).map_err(io(bin_path))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(io(bin_path))?;
    let expected = manifest.unit_ids.len() * manifest.dimension * 4;
    if bytes.len() != expected {
        return Err(EmbedError::BadBinSize {
            path: bin_path.to_path_buf(),
            expected,
            actual: bytes.len(),
            units: manifest.unit_ids.len(),
            dimension: manifest.dimension,
        });
    }
    let vectors = bytes
        .chunks_exact(manifest.dimension * 4)
        .map(|row| {
            row.chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect()
        })
        .collect();
    Ok((manifest, vectors))
}

/// Cosine similarity of two equal-length vectors; `0.0` when either is the zero vector.
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;
    for (&x, &y) in a.iter().zip(b) {
        dot += f64::from(x) * f64::from(y);
        norm_a += f64::from(x) * f64::from(x);
        norm_b += f64::from(y) * f64::from(y);
    }
    if norm_a <= 0.0 || norm_b <= 0.0 {
        return 0.0;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}

/// Deterministic, network-free test doubles for [`Embedder`].
pub mod testing {
    use super::{EmbedError, Embedder};

    /// An embedder that hashes each text's tokens into an 8-dimensional bag-of-words
    /// projection, so identical or overlapping texts get similar (or identical) vectors
    /// without ever making a network call. Deterministic across runs.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct FakeEmbedder;

    /// Dimension of [`FakeEmbedder`]'s vectors.
    pub const FAKE_DIMENSION: usize = 8;

    fn hash_token(token: &str) -> usize {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in token.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
        }
        usize::try_from(hash % FAKE_DIMENSION as u64).unwrap_or(0)
    }

    fn embed_one(text: &str) -> Vec<f32> {
        let mut vector = vec![0.0f32; FAKE_DIMENSION];
        for token in crate::index::tokenize(text) {
            vector[hash_token(&token)] += 1.0;
        }
        vector
    }

    impl Embedder for FakeEmbedder {
        fn embed(&self, _model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
            Ok(inputs.iter().map(|text| embed_one(text)).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakeEmbedder;
    use super::*;

    #[test]
    fn embeddings_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("embeddings.bin");
        let json = dir.path().join("embeddings.json");
        let manifest = EmbeddingsManifest {
            model: "fake".to_string(),
            dimension: 3,
            unit_ids: vec!["a::x.md".to_string(), "a::y.md".to_string()],
            manifest_sha256: "deadbeef".to_string(),
        };
        let vectors = vec![vec![1.0, 2.0, 3.0], vec![-1.5, 0.0, 4.25]];
        write_embeddings(&bin, &json, &manifest, &vectors).unwrap();
        assert_eq!(std::fs::metadata(&bin).unwrap().len(), (2 * 3 * 4) as u64);
        let (read_manifest, read_vectors) = read_embeddings(&bin, &json).unwrap();
        assert_eq!(read_manifest, manifest);
        assert_eq!(read_vectors, vectors);
    }

    #[test]
    fn bin_size_mismatch_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("embeddings.bin");
        let json = dir.path().join("embeddings.json");
        let manifest = EmbeddingsManifest {
            model: "fake".to_string(),
            dimension: 4,
            unit_ids: vec!["a::x.md".to_string()],
            manifest_sha256: NO_MANIFEST.to_string(),
        };
        write_embeddings(&bin, &json, &manifest, &[vec![1.0, 2.0, 3.0]]).unwrap();
        assert!(matches!(
            read_embeddings(&bin, &json).unwrap_err(),
            EmbedError::BadBinSize { .. }
        ));
    }

    #[test]
    fn artifact_hash_falls_back_to_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(artifact_manifest_hash(dir.path()).unwrap(), NO_MANIFEST);
        std::fs::write(dir.path().join("manifest.json"), b"{}").unwrap();
        let hash = artifact_manifest_hash(dir.path()).unwrap();
        assert_eq!(hash, crate::resolve::sha256_hex(b"{}"));
    }

    #[test]
    fn fake_embedder_is_deterministic_and_batches() {
        let embedder = FakeEmbedder;
        let texts = vec![
            "alpha beta".to_string(),
            "alpha beta".to_string(),
            "gamma".to_string(),
        ];
        let vectors = embed_units(&embedder, "fake", &texts, 2).unwrap();
        assert_eq!(vectors.len(), 3);
        assert_eq!(vectors[0], vectors[1], "identical texts embed identically");
        assert_ne!(vectors[0], vectors[2]);
        assert!((cosine(&vectors[0], &vectors[0]) - 1.0).abs() < 1e-9);
        assert!(cosine(&vectors[0], &vectors[2]) < 1.0);
        assert!(
            cosine(&[0.0, 0.0], &[1.0, 1.0]).abs() < f64::EPSILON,
            "zero vector has no direction"
        );
    }

    #[test]
    fn missing_env_is_an_error_not_a_silent_skip() {
        // SAFETY: test-local env manipulation; no other test in this process reads these keys.
        unsafe {
            std::env::remove_var("PINAKES_EMBED_URL");
        }
        assert!(matches!(
            HttpEmbedder::from_env(None).unwrap_err(),
            EmbedError::MissingConfig(_)
        ));
    }
}
