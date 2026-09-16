//! Python bindings for `pinakes`'s built-in BM25 measurement index (SPEC §17.2).
//!
//! This crate wraps [`pinakes_lib::index::Index`] as `pinakes.Index`: the same retrieval model
//! the `pinakes eval` CLI command measures with, so a consumer importing this module searches
//! the identical corpus the curator scored. `build` and `search` release the GIL (`Python::detach`)
//! while the (CPU-bound, allocation-heavy) Rust code runs, so other Python threads keep going.
//!
//! Reading a page's `url` needs the artifact's `<source>/meta.json` (SPEC §2.3: `base_url` plus
//! the page's path), which is not part of [`pinakes_lib::index::Page`] itself, so [`Index::read`]
//! re-reads that file directly rather than widening the library type for one binding-only field.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pinakes_lib::artifact::{META_FILE, Meta};
use pinakes_lib::index::{self, Priorities};
use pyo3::exceptions::{PyOSError, PyValueError};
use pyo3::prelude::*;

/// One search result: a page id, its BM25 score and the heading of the best-scoring unit.
#[pyclass(module = "pinakes", frozen, skip_from_py_object)]
#[derive(Debug, Clone)]
struct Hit {
    /// `<source>::<path>`.
    #[pyo3(get)]
    page_id: String,
    /// BM25 score of the page's best-scoring retrieval unit.
    #[pyo3(get)]
    score: f64,
    /// Heading of the best-scoring unit, empty for the page intro.
    #[pyo3(get)]
    heading: String,
}

#[pymethods]
impl Hit {
    fn __repr__(&self) -> String {
        format!(
            "Hit(page_id={:?}, score={}, heading={:?})",
            self.page_id, self.score, self.heading
        )
    }
}

impl From<index::Hit> for Hit {
    fn from(hit: index::Hit) -> Self {
        Hit {
            page_id: hit.page_id,
            score: hit.score,
            heading: hit.heading,
        }
    }
}

/// One retrieved page: navigation title, canonical URL, owning module, document type,
/// navigation section and cleaned Markdown content.
#[pyclass(module = "pinakes", frozen, skip_from_py_object)]
#[derive(Debug, Clone)]
struct Page {
    /// The navigation title, else the page's own H1 or frontmatter title.
    #[pyo3(get)]
    title: String,
    /// The page's canonical URL: the source's `base_url` (SPEC §2.3) joined with its path.
    #[pyo3(get)]
    url: String,
    /// Module name (the source name, unless `meta.json` overrides it).
    #[pyo3(get)]
    module: String,
    /// Document type from `meta.json`, possibly empty.
    #[pyo3(get)]
    doc_type: String,
    /// Navigation section from `meta.json`, possibly empty.
    #[pyo3(get)]
    section: String,
    /// Cleaned page content (frontmatter and HTML comments removed).
    #[pyo3(get)]
    content: String,
}

#[pymethods]
impl Page {
    fn __repr__(&self) -> String {
        format!(
            "Page(title={:?}, url={:?}, module={:?}, doc_type={:?}, section={:?}, \
             content=<{} chars>)",
            self.title,
            self.url,
            self.module,
            self.doc_type,
            self.section,
            self.content.chars().count()
        )
    }
}

/// The in-memory BM25 index over an artifact directory (SPEC §5), the same index `pinakes eval`
/// measures the corpus with.
#[pyclass(module = "pinakes")]
struct Index {
    artifact: PathBuf,
    inner: index::Index,
}

/// Wrap an [`index::IndexError`] as a `ValueError`.
fn index_err(err: &index::IndexError) -> PyErr {
    PyValueError::new_err(err.to_string())
}

/// Read `<artifact>/<source>/meta.json` and join its `base_url` with `path` (SPEC §2.3).
fn page_url(artifact: &Path, source: &str, path: &str) -> PyResult<String> {
    let meta_path = artifact.join(source).join(META_FILE);
    let text = std::fs::read_to_string(&meta_path)
        .map_err(|e| PyOSError::new_err(format!("{}: {e}", meta_path.display())))?;
    let meta: Meta = serde_json::from_str(&text)
        .map_err(|e| PyValueError::new_err(format!("{}: {e}", meta_path.display())))?;
    Ok(format!("{}/{path}", meta.base_url))
}

#[pymethods]
impl Index {
    /// Build the index from an artifact directory (SPEC §2.3), optionally overriding source
    /// priorities used by the mirror rule (SPEC §5). A source not named in `priorities` (or
    /// every source, when `priorities` is `None`) gets the default priority, so unconfigured
    /// sources never collapse each other.
    #[staticmethod]
    #[pyo3(signature = (artifact_dir, priorities=None))]
    fn build(
        py: Python<'_>,
        artifact_dir: &str,
        priorities: Option<BTreeMap<String, i64>>,
    ) -> PyResult<Index> {
        let artifact = PathBuf::from(artifact_dir);
        let explicit = priorities.unwrap_or_default();
        py.detach(|| {
            let priorities = Priorities { explicit };
            let inner = index::Index::build(&artifact, &priorities).map_err(|e| index_err(&e))?;
            Ok(Index { artifact, inner })
        })
    }

    /// The best `k` pages for `query`, ranked by their best-scoring unit and de-duplicated by
    /// tokenised title. With `module`, only pages of that module (case-insensitive) are
    /// returned, unless none scores, in which case the filter is dropped.
    #[pyo3(signature = (query, k=10, module=None))]
    fn search(
        &self,
        py: Python<'_>,
        query: &str,
        k: usize,
        module: Option<&str>,
    ) -> PyResult<Vec<Hit>> {
        let inner = &self.inner;
        let hits = py.detach(|| inner.search(query, k, module).map_err(|e| index_err(&e)))?;
        Ok(hits.into_iter().map(Hit::from).collect())
    }

    /// Number of pages read from the artifact, mirrors included.
    #[getter]
    fn page_count(&self) -> usize {
        self.inner.page_count()
    }

    /// Number of pages in the search corpus, mirrors excluded.
    #[getter]
    fn searchable_count(&self) -> usize {
        self.inner.searchable_count()
    }

    /// The page with this id (`<source>::<path>`), or `None` if there is no such page.
    fn read(&self, page_id: &str) -> PyResult<Option<Page>> {
        let Some(page) = self.inner.page(page_id) else {
            return Ok(None);
        };
        let url = page_url(&self.artifact, &page.source, &page.path)?;
        Ok(Some(Page {
            title: page.title.clone(),
            url,
            module: page.module.clone(),
            doc_type: page.doc_type.clone(),
            section: page.section.clone(),
            content: page.content.clone(),
        }))
    }
}

/// The `pinakes` Python extension module.
#[pymodule]
fn pinakes(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Index>()?;
    m.add_class::<Hit>()?;
    m.add_class::<Page>()?;
    Ok(())
}
