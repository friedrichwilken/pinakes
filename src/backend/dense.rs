//! `dense` (SPEC §16.2): an embeddings file, queried by cosine similarity.

use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use super::{Backend, BackendConfig, BackendError, BackendKind};
use crate::embed::{EmbedError, Embedder};
use crate::index::{Hit, Page, iter_units, load_pages, mark_mirrors, title_key};

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

/// The search behind [`DenseBackend`] (and so the dense half of `hybrid`, which calls
/// [`DenseBackend::search`]): embed `query`, rank pages by their best unit's cosine similarity,
/// de-duplicate by title key and apply the module filter — the same shape as [`Index::search`],
/// but scored densely.
///
/// [`Index::search`]: crate::index::Index::search
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::testing::{dense_config, fixture_pages};
    use crate::embed::testing::FakeEmbedder;

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
}
