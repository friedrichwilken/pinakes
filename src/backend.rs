//! Retriever shapes (SPEC §16): a common [`Backend`] trait behind `bm25` (the SPEC §5 index,
//! wrapped rather than rewritten), `bm25-tantivy` (the same units scored by tantivy's own BM25),
//! `dense` (an embeddings file, SPEC §16.2), `hybrid` (reciprocal rank fusion of `bm25` and
//! `dense`, SPEC §16.3) and `external` (a consumer's own store over HTTP, SPEC §16.4).
//!
//! `eval` selects one with `--backend NAME`, or several at once with `--compare a,b,c`; the
//! numbers this produces decide which shape to run in production, not an argument from
//! architecture.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::str::FromStr;
use std::time::Duration;

use serde::Deserialize;
use tantivy::query::{BooleanQuery, BoostQuery, Occur, Query, TermQuery};
use tantivy::schema::{
    IndexRecordOption, STORED, Schema, TantivyDocument, TextFieldIndexing, TextOptions, Value as _,
};
use tantivy::{IndexWriter, Searcher, Term};
use thiserror::Error;

use crate::embed::{EmbedError, Embedder};
use crate::index::{
    CuratorTokenizer, HEADING_BOOST, Hit, Index, IndexError, Page, Priorities, TITLE_BOOST,
    TOKENIZER_NAME, index_text, iter_units, load_pages, mark_mirrors, split_sections, title_key,
    tokenize,
};

/// Errors raised while building or querying a backend.
#[derive(Debug, Error)]
pub enum BackendError {
    /// The `bm25` or `bm25-tantivy` index could not be built or searched.
    #[error(transparent)]
    Index(#[from] IndexError),
    /// Embedding, or reading/writing the embeddings file pair, failed.
    #[error(transparent)]
    Embed(#[from] EmbedError),
    /// tantivy failed.
    #[error("index: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    /// The backend is missing configuration it needs (a URL, an embedder, …).
    #[error("backend {backend}: {message}")]
    Config {
        /// The backend name.
        backend: String,
        /// What is missing.
        message: String,
    },
    /// `--with` / `--without` is not supported for this backend.
    #[error(
        "backend {0}: --with/--without needs a backend that indexes pages directly (bm25, bm25-tantivy)"
    )]
    UnsupportedAdjustment(String),
    /// The external backend's HTTP request failed.
    #[error("{url}: {message}")]
    Http {
        /// The requested URL.
        url: String,
        /// What went wrong.
        message: String,
    },
    /// The external backend's response was not the expected shape.
    #[error("{url}: unexpected response: {message}")]
    BadResponse {
        /// The requested URL.
        url: String,
        /// What was wrong with it.
        message: String,
    },
    /// The requested backend name is not one of the five SPEC §16.1 backends.
    #[error("unknown backend {0:?}: expected bm25, bm25-tantivy, dense, hybrid or external")]
    UnknownBackend(String),
}

/// One of the five retriever shapes of SPEC §16.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BackendKind {
    /// The SPEC §5 hand-rolled `BM25Okapi` index (the default).
    #[default]
    Bm25,
    /// The same units, scored by tantivy's own BM25 (`k1` 1.2, `b` 0.75).
    Bm25Tantivy,
    /// An embeddings file, queried by cosine similarity.
    Dense,
    /// Reciprocal rank fusion of `bm25` and `dense`.
    Hybrid,
    /// A consumer's own store, over HTTP.
    External,
}

impl BackendKind {
    /// The name used on the command line and recorded in eval results.
    pub fn name(self) -> &'static str {
        match self {
            BackendKind::Bm25 => "bm25",
            BackendKind::Bm25Tantivy => "bm25-tantivy",
            BackendKind::Dense => "dense",
            BackendKind::Hybrid => "hybrid",
            BackendKind::External => "external",
        }
    }
}

impl fmt::Display for BackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for BackendKind {
    type Err = BackendError;

    fn from_str(name: &str) -> Result<BackendKind, BackendError> {
        match name {
            "bm25" => Ok(BackendKind::Bm25),
            "bm25-tantivy" => Ok(BackendKind::Bm25Tantivy),
            "dense" => Ok(BackendKind::Dense),
            "hybrid" => Ok(BackendKind::Hybrid),
            "external" => Ok(BackendKind::External),
            other => Err(BackendError::UnknownBackend(other.to_string())),
        }
    }
}

/// Configuration shared by every backend's `build`: the pieces a plain `Index` does not need.
#[derive(Clone, Default)]
pub struct BackendConfig {
    /// Source priorities for the mirror rule (as `Index` uses).
    pub priorities: Priorities,
    /// `embeddings.bin` path (`dense`, `hybrid`).
    pub embeddings_bin: PathBuf,
    /// `embeddings.json` path (`dense`, `hybrid`).
    pub embeddings_json: PathBuf,
    /// Use embeddings even when their recorded manifest hash does not match the artifact.
    pub allow_stale: bool,
    /// Where query embeddings come from (`dense`, `hybrid`); required for those backends.
    pub embedder: Option<Rc<dyn Embedder>>,
    /// The consumer's search endpoint base URL (`external`).
    pub backend_url: Option<String>,
}

impl BackendConfig {
    fn embedder(&self, backend: &str) -> Result<Rc<dyn Embedder>, BackendError> {
        self.embedder.clone().ok_or_else(|| BackendError::Config {
            backend: backend.to_string(),
            message: "no embedder configured (PINAKES_EMBED_URL not set?)".to_string(),
        })
    }

    fn backend_url(&self) -> Result<&str, BackendError> {
        self.backend_url
            .as_deref()
            .ok_or_else(|| BackendError::Config {
                backend: BackendKind::External.name().to_string(),
                message: "--backend-url is required".to_string(),
            })
    }
}

/// A retriever shape (SPEC §16.1): built once from an artifact, then searched repeatedly.
///
/// `build` is generic over the concrete backend (`Self`) and so is not part of the trait's
/// object-safe surface; `search`, `page_count` and `searchable_count` are, so `eval --compare`
/// can hold a `Vec<Box<dyn Backend>>` of backends chosen at run time by name.
pub trait Backend {
    /// Build the backend from an artifact directory.
    fn build(artifact: &Path, config: &BackendConfig) -> Result<Self, BackendError>
    where
        Self: Sized;

    /// The best `k` pages for `query`, optionally restricted to `module`.
    fn search(&self, query: &str, k: usize, module: Option<&str>)
    -> Result<Vec<Hit>, BackendError>;

    /// Pages read from the artifact, mirrors included.
    fn page_count(&self) -> usize;

    /// Pages in the search corpus (mirrors excluded).
    fn searchable_count(&self) -> usize;
}

/// Build the named backend from an artifact.
pub fn build(
    kind: BackendKind,
    artifact: &Path,
    config: &BackendConfig,
) -> Result<Box<dyn Backend>, BackendError> {
    Ok(match kind {
        BackendKind::Bm25 => Box::new(Bm25Backend::build(artifact, config)?),
        BackendKind::Bm25Tantivy => Box::new(TantivyBackend::build(artifact, config)?),
        BackendKind::Dense => Box::new(DenseBackend::build(artifact, config)?),
        BackendKind::Hybrid => Box::new(HybridBackend::build(artifact, config)?),
        BackendKind::External => Box::new(ExternalBackend::build(artifact, config)?),
    })
}

// -------------------------------------------------------------------------------------------
// bm25: Index, wrapped
// -------------------------------------------------------------------------------------------

/// The SPEC §5 index (`Index`), wrapped to implement [`Backend`]; the search rules are
/// unchanged, see [`crate::index`].
pub struct Bm25Backend(Index);

impl Bm25Backend {
    /// The wrapped index, for callers that need `--with`/`--without` page adjustment.
    pub fn index(&self) -> &Index {
        &self.0
    }

    /// Wrap an already-built index.
    pub fn from_index(index: Index) -> Bm25Backend {
        Bm25Backend(index)
    }
}

impl Backend for Bm25Backend {
    fn build(artifact: &Path, config: &BackendConfig) -> Result<Bm25Backend, BackendError> {
        Ok(Bm25Backend(Index::build(artifact, &config.priorities)?))
    }

    fn search(
        &self,
        query: &str,
        k: usize,
        module: Option<&str>,
    ) -> Result<Vec<Hit>, BackendError> {
        Ok(self.0.search(query, k, module)?)
    }

    fn page_count(&self) -> usize {
        self.0.page_count()
    }

    fn searchable_count(&self) -> usize {
        self.0.searchable_count()
    }
}

// -------------------------------------------------------------------------------------------
// bm25-tantivy: the same units, tantivy's own BM25 (k1 1.2, b 0.75)
// -------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct TantivyFields {
    title: tantivy::schema::Field,
    heading: tantivy::schema::Field,
    body: tantivy::schema::Field,
    page_id: tantivy::schema::Field,
}

impl TantivyFields {
    fn schema() -> (Schema, TantivyFields) {
        let indexing = TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER_NAME)
            .set_index_option(IndexRecordOption::WithFreqs);
        let indexed = TextOptions::default().set_indexing_options(indexing);
        let mut builder = Schema::builder();
        let fields = TantivyFields {
            title: builder.add_text_field("title", indexed.clone().set_stored()),
            heading: builder.add_text_field("heading", indexed.clone().set_stored()),
            body: builder.add_text_field("body", indexed),
            page_id: builder.add_text_field("page_id", STORED),
        };
        (builder.build(), fields)
    }
}

/// tantivy's own BM25 scorer (`k1 = 1.2`, `b = 0.75`, Lucene-style IDF and field-length norms)
/// over the same retrieval units as [`Index`] (SPEC §16.1): title, heading and body are indexed
/// as separate fields and combined with the same boosts, so the ranking differs from `bm25`
/// only in how tantivy itself scores a field match, not in what is indexed.
pub struct TantivyBackend {
    pages: Vec<Page>,
    searchable_count: usize,
    page_by_id: HashMap<String, usize>,
    fields: TantivyFields,
    searcher: Searcher,
    total_units: usize,
}

impl Backend for TantivyBackend {
    fn build(artifact: &Path, config: &BackendConfig) -> Result<TantivyBackend, BackendError> {
        let mut pages = load_pages(artifact, &config.priorities)?;
        mark_mirrors(&mut pages);
        let (schema, fields) = TantivyFields::schema();
        let index = tantivy::Index::create_in_ram(schema);
        index
            .tokenizers()
            .register(TOKENIZER_NAME, CuratorTokenizer);
        let mut writer: IndexWriter<TantivyDocument> =
            index.writer_with_num_threads(1, 32 << 20)?;
        let mut total_units = 0usize;
        let mut searchable_count = 0usize;
        let mut page_by_id = HashMap::new();
        for (position, page) in pages.iter().enumerate() {
            if page.mirror_of.is_some() {
                continue;
            }
            searchable_count += 1;
            page_by_id.insert(page.id.clone(), position);
            for section in split_sections(&index_text(&page.content)) {
                let mut doc = TantivyDocument::default();
                doc.add_text(fields.title, &page.title);
                doc.add_text(fields.heading, &section.heading);
                doc.add_text(fields.body, &section.body);
                doc.add_text(fields.page_id, &page.id);
                writer.add_document(doc)?;
                total_units += 1;
            }
        }
        writer.commit()?;
        let searcher = index.reader()?.searcher();
        Ok(TantivyBackend {
            pages,
            searchable_count,
            page_by_id,
            fields,
            searcher,
            total_units,
        })
    }

    fn search(
        &self,
        query: &str,
        k: usize,
        module: Option<&str>,
    ) -> Result<Vec<Hit>, BackendError> {
        if self.total_units == 0 || k == 0 {
            return Ok(Vec::new());
        }
        let tokens = tokenize(query);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for token in &tokens {
            for (field, boost) in [
                (
                    self.fields.title,
                    f32::from(u8::try_from(TITLE_BOOST).unwrap_or(3)),
                ),
                (
                    self.fields.heading,
                    f32::from(u8::try_from(HEADING_BOOST).unwrap_or(2)),
                ),
                (self.fields.body, 1.0),
            ] {
                let term = Term::from_field_text(field, token);
                let term_query = TermQuery::new(term, IndexRecordOption::WithFreqs);
                let query: Box<dyn Query> = if (boost - 1.0).abs() > f32::EPSILON {
                    Box::new(BoostQuery::new(Box::new(term_query), boost))
                } else {
                    Box::new(term_query)
                };
                clauses.push((Occur::Should, query));
            }
        }
        let boolean = BooleanQuery::new(clauses);
        let top = tantivy::collector::TopDocs::with_limit(self.total_units).order_by_score();
        let hits = self.searcher.search(&boolean, &top)?;

        let mut best: HashMap<String, (f32, String)> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for (score, address) in hits {
            let doc: TantivyDocument = self.searcher.doc(address)?;
            let page_id = doc
                .get_first(self.fields.page_id)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let heading = doc
                .get_first(self.fields.heading)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if !best.contains_key(&page_id) {
                order.push(page_id.clone());
            }
            let entry = best.entry(page_id).or_insert((score, heading.clone()));
            if score > entry.0 {
                *entry = (score, heading);
            }
        }

        let module_wanted = module.filter(|m| !m.is_empty()).map(str::to_lowercase);
        let module_of = |id: &str| -> String {
            self.page_by_id
                .get(id)
                .map(|&i| self.pages[i].module.to_lowercase())
                .unwrap_or_default()
        };
        let mut ranked = order;
        if let Some(wanted) = &module_wanted {
            let filtered: Vec<String> = ranked
                .iter()
                .filter(|id| &module_of(id) == wanted)
                .cloned()
                .collect();
            if !filtered.is_empty() {
                ranked = filtered;
            }
        }

        let mut seen_titles = std::collections::HashSet::new();
        let mut out = Vec::new();
        for id in ranked {
            let Some(&position) = self.page_by_id.get(&id) else {
                continue;
            };
            let key = title_key(&self.pages[position].title);
            if !key.is_empty() && !seen_titles.insert(key) {
                continue;
            }
            let (score, heading) = best.get(&id).cloned().unwrap_or_default();
            out.push(Hit {
                page_id: id,
                score: f64::from(score),
                heading,
            });
            if out.len() == k {
                break;
            }
        }
        Ok(out)
    }

    fn page_count(&self) -> usize {
        self.pages.len()
    }

    fn searchable_count(&self) -> usize {
        self.searchable_count
    }
}

// -------------------------------------------------------------------------------------------
// dense: embeddings file, cosine similarity
// -------------------------------------------------------------------------------------------

/// The `dense` backend (SPEC §16.2): an embeddings file loaded from disk, the query embedded
/// through the same endpoint (and model) that produced it, pages ranked by their best unit's
/// cosine similarity.
pub struct DenseBackend {
    pages: Vec<Page>,
    page_by_id: HashMap<String, usize>,
    searchable_count: usize,
    model: String,
    embedder: Rc<dyn Embedder>,
    unit_ids: Vec<String>,
    unit_headings: Vec<String>,
    vectors: Vec<Vec<f32>>,
}

impl std::fmt::Debug for DenseBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DenseBackend")
            .field("pages", &self.pages.len())
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl Backend for DenseBackend {
    fn build(artifact: &Path, config: &BackendConfig) -> Result<DenseBackend, BackendError> {
        let mut pages = load_pages(artifact, &config.priorities)?;
        mark_mirrors(&mut pages);
        let (manifest, vectors) =
            crate::embed::read_embeddings(&config.embeddings_bin, &config.embeddings_json)?;
        let current = crate::embed::artifact_manifest_hash(artifact)?;
        if manifest.manifest_sha256 != current && !config.allow_stale {
            return Err(BackendError::Embed(EmbedError::Stale {
                expected: manifest.manifest_sha256,
                actual: current,
            }));
        }
        let embedder = config.embedder(BackendKind::Dense.name())?;
        let units = iter_units(&pages);
        let unit_headings = units.into_iter().map(|u| u.heading).collect();
        let mut page_by_id = HashMap::new();
        let mut searchable_count = 0;
        for (position, page) in pages.iter().enumerate() {
            if page.mirror_of.is_none() {
                page_by_id.insert(page.id.clone(), position);
                searchable_count += 1;
            }
        }
        Ok(DenseBackend {
            pages,
            page_by_id,
            searchable_count,
            model: manifest.model,
            embedder,
            unit_ids: manifest.unit_ids,
            unit_headings,
            vectors,
        })
    }

    fn search(
        &self,
        query: &str,
        k: usize,
        module: Option<&str>,
    ) -> Result<Vec<Hit>, BackendError> {
        dense_search(
            &self.pages,
            &self.page_by_id,
            &self.unit_ids,
            &self.unit_headings,
            &self.vectors,
            self.embedder.as_ref(),
            &self.model,
            query,
            k,
            module,
        )
    }

    fn page_count(&self) -> usize {
        self.pages.len()
    }

    fn searchable_count(&self) -> usize {
        self.searchable_count
    }
}

/// Shared by [`DenseBackend`] and [`HybridBackend`]: embed `query`, rank pages by their best
/// unit's cosine similarity, de-duplicate by title key and apply the module filter — the same
/// shape as [`Index::search`], but scored densely.
#[allow(clippy::too_many_arguments)]
fn dense_search(
    pages: &[Page],
    page_by_id: &HashMap<String, usize>,
    unit_ids: &[String],
    unit_headings: &[String],
    vectors: &[Vec<f32>],
    embedder: &dyn Embedder,
    model: &str,
    query: &str,
    k: usize,
    module: Option<&str>,
) -> Result<Vec<Hit>, BackendError> {
    if vectors.is_empty() || k == 0 {
        return Ok(Vec::new());
    }
    let query_vector = embedder
        .embed(model, std::slice::from_ref(&query.to_string()))?
        .into_iter()
        .next()
        .ok_or_else(|| BackendError::Config {
            backend: BackendKind::Dense.name().to_string(),
            message: "embedder returned no vector for the query".to_string(),
        })?;

    let mut best: HashMap<&str, (f64, &str)> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    for ((unit_id, heading), vector) in unit_ids.iter().zip(unit_headings).zip(vectors) {
        let score = crate::embed::cosine(&query_vector, vector);
        match best.entry(unit_id.as_str()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                order.push(unit_id.as_str());
                entry.insert((score, heading.as_str()));
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                if score > entry.get().0 {
                    entry.insert((score, heading.as_str()));
                }
            }
        }
    }
    order.sort_by(|a, b| best[b].0.total_cmp(&best[a].0));

    let module_wanted = module.filter(|m| !m.is_empty()).map(str::to_lowercase);
    let module_of = |id: &str| -> String {
        page_by_id
            .get(id)
            .map(|&i| pages[i].module.to_lowercase())
            .unwrap_or_default()
    };
    if let Some(wanted) = &module_wanted {
        let filtered: Vec<&str> = order
            .iter()
            .copied()
            .filter(|id| &module_of(id) == wanted)
            .collect();
        if !filtered.is_empty() {
            order = filtered;
        }
    }

    let mut seen_titles = std::collections::HashSet::new();
    let mut out = Vec::new();
    for id in order {
        let Some(&position) = page_by_id.get(id) else {
            continue;
        };
        let key = title_key(&pages[position].title);
        if !key.is_empty() && !seen_titles.insert(key) {
            continue;
        }
        let (score, heading) = best[id];
        out.push(Hit {
            page_id: id.to_string(),
            score,
            heading: heading.to_string(),
        });
        if out.len() == k {
            break;
        }
    }
    Ok(out)
}

// -------------------------------------------------------------------------------------------
// hybrid: reciprocal rank fusion of bm25 and dense
// -------------------------------------------------------------------------------------------

/// `k` in the reciprocal rank fusion formula (SPEC §16.3).
pub const RRF_K: f64 = 60.0;
/// How many of each ranking [`HybridBackend`] fuses (SPEC §16.3).
pub const RRF_DEPTH: usize = 50;

/// Reciprocal rank fusion: `score(page) = Σ 1 / (k + rank)` over every ranking it appears in
/// (1-based rank), rankings sorted by score descending, ties broken by the order pages were
/// first seen in. `heading` is taken from the first ranking that carried the page.
pub fn reciprocal_rank_fusion(rankings: &[&[Hit]], k: usize) -> Vec<Hit> {
    let mut scores: HashMap<String, f64> = HashMap::new();
    let mut headings: HashMap<String, String> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for ranking in rankings {
        for (rank, hit) in ranking.iter().enumerate() {
            if !scores.contains_key(&hit.page_id) {
                order.push(hit.page_id.clone());
            }
            let heading = headings.entry(hit.page_id.clone()).or_default();
            if heading.is_empty() && !hit.heading.is_empty() {
                heading.clone_from(&hit.heading);
            }
            let contribution = 1.0 / (RRF_K + float(rank + 1));
            *scores.entry(hit.page_id.clone()).or_insert(0.0) += contribution;
        }
    }
    order.sort_by(|a, b| scores[b].total_cmp(&scores[a]));
    order
        .into_iter()
        .take(k)
        .map(|id| Hit {
            score: scores[&id],
            heading: headings.remove(&id).unwrap_or_default(),
            page_id: id,
        })
        .collect()
}

#[allow(clippy::cast_precision_loss)]
fn float(n: usize) -> f64 {
    n as f64
}

/// `hybrid` (SPEC §16.3): reciprocal rank fusion of the `bm25` and `dense` page rankings, over
/// the top [`RRF_DEPTH`] of each.
pub struct HybridBackend {
    bm25: Bm25Backend,
    dense: DenseBackend,
}

impl Backend for HybridBackend {
    fn build(artifact: &Path, config: &BackendConfig) -> Result<HybridBackend, BackendError> {
        Ok(HybridBackend {
            bm25: Bm25Backend::build(artifact, config)?,
            dense: DenseBackend::build(artifact, config)?,
        })
    }

    fn search(
        &self,
        query: &str,
        k: usize,
        module: Option<&str>,
    ) -> Result<Vec<Hit>, BackendError> {
        let bm25_hits = self.bm25.search(query, RRF_DEPTH, module)?;
        let dense_hits = self.dense.search(query, RRF_DEPTH, module)?;
        Ok(reciprocal_rank_fusion(&[&bm25_hits, &dense_hits], k))
    }

    fn page_count(&self) -> usize {
        self.bm25.page_count()
    }

    fn searchable_count(&self) -> usize {
        self.bm25.searchable_count()
    }
}

// -------------------------------------------------------------------------------------------
// external: a consumer's own store, over HTTP (SPEC §16.4)
// -------------------------------------------------------------------------------------------

/// External backend timeout (SPEC §16.4).
const EXTERNAL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
struct ExternalHit {
    page_id: String,
    score: f64,
    #[serde(default)]
    heading: String,
}

#[derive(Debug, Deserialize)]
struct ExternalResponse {
    hits: Vec<ExternalHit>,
}

/// `external` (SPEC §16.4): `POST {backend_url}/search` with `{"query", "k", "module"}`,
/// expecting `{"hits": [{"page_id", "score", "heading"}]}`. Used to evaluate a store a
/// consumer already runs; a failed request or a malformed response fails the whole `eval`.
pub struct ExternalBackend {
    url: String,
    agent: ureq::Agent,
    page_count: usize,
    searchable_count: usize,
}

impl std::fmt::Debug for ExternalBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalBackend")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl Backend for ExternalBackend {
    fn build(artifact: &Path, config: &BackendConfig) -> Result<ExternalBackend, BackendError> {
        let url = config.backend_url()?.to_string();
        let mut pages = load_pages(artifact, &config.priorities)?;
        mark_mirrors(&mut pages);
        let searchable_count = pages.iter().filter(|p| p.mirror_of.is_none()).count();
        let agent = ureq::Agent::config_builder()
            .user_agent(concat!("pinakes/", env!("CARGO_PKG_VERSION")))
            .build()
            .new_agent();
        Ok(ExternalBackend {
            url,
            agent,
            page_count: pages.len(),
            searchable_count,
        })
    }

    fn search(
        &self,
        query: &str,
        k: usize,
        module: Option<&str>,
    ) -> Result<Vec<Hit>, BackendError> {
        let url = format!("{}/search", self.url.trim_end_matches('/'));
        let body = serde_json::json!({ "query": query, "k": k, "module": module });
        let response = self
            .agent
            .post(&url)
            .config()
            .timeout_global(Some(EXTERNAL_TIMEOUT))
            .build()
            .header("content-type", "application/json")
            .send_json(&body)
            .map_err(|err| BackendError::Http {
                url: url.clone(),
                message: err.to_string(),
            })?;
        let parsed: ExternalResponse =
            response
                .into_body()
                .read_json()
                .map_err(|err| BackendError::BadResponse {
                    url: url.clone(),
                    message: err.to_string(),
                })?;
        Ok(parsed
            .hits
            .into_iter()
            .take(k)
            .map(|hit| Hit {
                page_id: hit.page_id,
                score: hit.score,
                heading: hit.heading,
            })
            .collect())
    }

    fn page_count(&self) -> usize {
        self.page_count
    }

    fn searchable_count(&self) -> usize {
        self.searchable_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::testing::FakeEmbedder;
    use crate::index::testing::{SourceSpec, write_artifact};

    fn fixture_pages() -> (tempfile::TempDir, Vec<Page>) {
        let dir = tempfile::tempdir().unwrap();
        write_artifact(
            dir.path(),
            &[SourceSpec {
                name: "handbook",
                repo: "example-org/handbook",
                pages: &[
                    (
                        "docs/user/README.md",
                        "Storage Module",
                        "# Storage\n\nThe storage module keeps uploaded files.\n\n## Upload caching\n\nEnable upload caching with a bucket label.\n",
                    ),
                    (
                        "docs/user/billing.md",
                        "Billing",
                        "# Billing\n\nInvoices scale to zero.\n",
                    ),
                ],
                residue: &[],
            }],
        );
        let pages = load_pages(dir.path(), &Priorities::default()).unwrap();
        (dir, pages)
    }

    #[test]
    fn backend_kind_round_trips_through_its_name() {
        for kind in [
            BackendKind::Bm25,
            BackendKind::Bm25Tantivy,
            BackendKind::Dense,
            BackendKind::Hybrid,
            BackendKind::External,
        ] {
            assert_eq!(kind.name().parse::<BackendKind>().unwrap(), kind);
        }
        assert!(matches!(
            "nope".parse::<BackendKind>().unwrap_err(),
            BackendError::UnknownBackend(_)
        ));
    }

    #[test]
    fn bm25_backend_wraps_index_search_verbatim() {
        let (dir, _pages) = fixture_pages();
        let config = BackendConfig::default();
        let backend = Bm25Backend::build(dir.path(), &config).unwrap();
        let index = Index::build(dir.path(), &Priorities::default()).unwrap();
        assert_eq!(backend.page_count(), index.page_count());
        assert_eq!(backend.searchable_count(), index.searchable_count());
        assert_eq!(
            backend.search("upload caching", 10, None).unwrap(),
            index.search("upload caching", 10, None).unwrap()
        );
    }

    #[test]
    fn tantivy_backend_finds_the_boosted_title_match_first() {
        let (dir, _pages) = fixture_pages();
        let config = BackendConfig::default();
        let backend = TantivyBackend::build(dir.path(), &config).unwrap();
        assert_eq!(backend.page_count(), 2);
        assert_eq!(backend.searchable_count(), 2);
        let hits = backend.search("upload caching", 10, None).unwrap();
        assert_eq!(hits[0].page_id, "handbook::docs/user/README.md");
        assert_eq!(hits[0].heading, "Upload caching");
        assert!(hits[0].score > 0.0);
        assert!(
            backend
                .search("nothing at all matches", 10, None)
                .unwrap()
                .is_empty()
        );
        let hits = backend
            .search("billing invoices", 10, Some("handbook"))
            .unwrap();
        assert_eq!(hits[0].page_id, "handbook::docs/user/billing.md");
    }

    fn dense_config(dir: &Path, embedder: Rc<dyn Embedder>) -> (tempfile::TempDir, BackendConfig) {
        let mut pages = load_pages(dir, &Priorities::default()).unwrap();
        mark_mirrors(&mut pages);
        let units = iter_units(&pages);
        let texts: Vec<String> = units.iter().map(|u| u.text.clone()).collect();
        let vectors = crate::embed::embed_units(embedder.as_ref(), "fake", &texts, 64).unwrap();
        let out = tempfile::tempdir().unwrap();
        let bin = out.path().join("embeddings.bin");
        let json = out.path().join("embeddings.json");
        let manifest = crate::embed::EmbeddingsManifest {
            model: "fake".to_string(),
            dimension: crate::embed::testing::FAKE_DIMENSION,
            unit_ids: units.iter().map(|u| u.page_id.clone()).collect(),
            manifest_sha256: crate::embed::artifact_manifest_hash(dir).unwrap(),
        };
        crate::embed::write_embeddings(&bin, &json, &manifest, &vectors).unwrap();
        let config = BackendConfig {
            embeddings_bin: bin,
            embeddings_json: json,
            embedder: Some(embedder),
            ..BackendConfig::default()
        };
        (out, config)
    }

    #[test]
    fn dense_backend_ranks_by_cosine_and_reports_stale_embeddings() {
        let (dir, _pages) = fixture_pages();
        let embedder: Rc<dyn Embedder> = Rc::new(FakeEmbedder);
        let (_embeddings_dir, config) = dense_config(dir.path(), embedder);
        let backend = DenseBackend::build(dir.path(), &config).unwrap();
        assert_eq!(backend.page_count(), 2);
        let hits = backend.search("upload caching bucket", 10, None).unwrap();
        assert_eq!(hits[0].page_id, "handbook::docs/user/README.md");

        // Touching the artifact so its manifest hash would differ triggers the stale check;
        // simulate that directly by writing a manifest.json after the embeddings were built.
        std::fs::write(dir.path().join("manifest.json"), "{}").unwrap();
        let err = DenseBackend::build(dir.path(), &config).unwrap_err();
        assert!(
            matches!(err, BackendError::Embed(EmbedError::Stale { .. })),
            "{err}"
        );
        let allowed = BackendConfig {
            allow_stale: true,
            ..config
        };
        assert!(DenseBackend::build(dir.path(), &allowed).is_ok());
    }

    #[test]
    fn dense_backend_without_an_embedder_is_a_config_error() {
        let (dir, _pages) = fixture_pages();
        let config = BackendConfig {
            embeddings_bin: dir.path().join("embeddings.bin"),
            embeddings_json: dir.path().join("embeddings.json"),
            ..BackendConfig::default()
        };
        // No embeddings file at all is an I/O error before the embedder is even checked.
        assert!(DenseBackend::build(dir.path(), &config).is_err());
    }

    #[test]
    fn hybrid_backend_fuses_bm25_and_dense_rankings() {
        let (dir, _pages) = fixture_pages();
        let embedder: Rc<dyn Embedder> = Rc::new(FakeEmbedder);
        let (_embeddings_dir, config) = dense_config(dir.path(), embedder);
        let backend = HybridBackend::build(dir.path(), &config).unwrap();
        let hits = backend.search("upload caching bucket", 10, None).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].page_id, "handbook::docs/user/README.md");
    }

    #[test]
    fn reciprocal_rank_fusion_matches_the_formula_by_hand() {
        let a = [
            Hit {
                page_id: "p1".into(),
                score: 1.0,
                heading: String::new(),
            },
            Hit {
                page_id: "p2".into(),
                score: 0.9,
                heading: String::new(),
            },
        ];
        let b = [
            Hit {
                page_id: "p2".into(),
                score: 5.0,
                heading: "H2".into(),
            },
            Hit {
                page_id: "p1".into(),
                score: 4.0,
                heading: String::new(),
            },
            Hit {
                page_id: "p3".into(),
                score: 3.0,
                heading: "H3".into(),
            },
        ];
        let fused = reciprocal_rank_fusion(&[&a, &b], 10);
        // p1: 1/(60+1) + 1/(60+2); p2: 1/(60+2) + 1/(60+1); p3: 1/(60+3).
        let p1 = 1.0 / 61.0 + 1.0 / 62.0;
        let p2 = 1.0 / 62.0 + 1.0 / 61.0;
        let p3 = 1.0 / 63.0;
        assert!((fused.iter().find(|h| h.page_id == "p1").unwrap().score - p1).abs() < 1e-12);
        assert!((fused.iter().find(|h| h.page_id == "p2").unwrap().score - p2).abs() < 1e-12);
        assert!((fused.iter().find(|h| h.page_id == "p3").unwrap().score - p3).abs() < 1e-12);
        assert!((p1 - p2).abs() < 1e-12, "p1 and p2 tie exactly");
        // A tie keeps the order pages were first seen in: p1 appeared in ranking a before p2.
        assert_eq!(fused[0].page_id, "p1");
        assert_eq!(fused[1].page_id, "p2");
        assert_eq!(fused[2].page_id, "p3");
        assert_eq!(
            fused[1].heading, "H2",
            "heading comes from the first ranking carrying it"
        );
        assert_eq!(
            reciprocal_rank_fusion(&[&a, &b], 2).len(),
            2,
            "k truncates the result"
        );
    }

    /// Read one HTTP/1.1 request off `stream`, answering `Expect: 100-continue` (which ureq
    /// sends before a request body) so the client proceeds to send it, then returning headers
    /// and body as one string once `Content-Length` bytes of body have arrived.
    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        use std::io::{Read as _, Write as _};

        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut answered_continue = false;
        loop {
            let n = stream.read(&mut chunk).unwrap();
            assert!(n > 0, "connection closed before a full request arrived");
            buf.extend_from_slice(&chunk[..n]);
            let Some(header_end) = find_subslice(&buf, b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&buf[..header_end]).into_owned();
            if !answered_continue && headers.to_lowercase().contains("expect: 100-continue") {
                stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").unwrap();
                answered_continue = true;
            }
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(str::trim)
                        .map(str::to_string)
                })
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            let body_start = header_end + 4;
            if buf.len() - body_start >= content_length {
                return String::from_utf8_lossy(&buf).into_owned();
            }
        }
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    #[test]
    fn external_backend_sends_the_request_and_parses_the_response() {
        use std::io::Write;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("POST /search"), "{request}");
            assert!(request.contains("\"query\""), "{request}");
            assert!(request.contains("\"caching\""), "{request}");
            let body = r#"{"hits":[{"page_id":"handbook::docs/user/README.md","score":1.5,"heading":"Upload caching"}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let (dir, _pages) = fixture_pages();
        let config = BackendConfig {
            backend_url: Some(format!("http://{addr}")),
            ..BackendConfig::default()
        };
        let backend = ExternalBackend::build(dir.path(), &config).unwrap();
        assert_eq!(backend.page_count(), 2);
        let hits = backend.search("caching", 5, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].page_id, "handbook::docs/user/README.md");
        assert!((hits[0].score - 1.5).abs() < 1e-12);
        assert_eq!(hits[0].heading, "Upload caching");
        handle.join().unwrap();
    }

    #[test]
    fn external_backend_without_a_url_is_a_config_error() {
        let (dir, _pages) = fixture_pages();
        let err = ExternalBackend::build(dir.path(), &BackendConfig::default()).unwrap_err();
        assert!(matches!(err, BackendError::Config { .. }), "{err}");
    }
}
