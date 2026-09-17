//! `hybrid` (SPEC §16.3): reciprocal rank fusion of the `bm25` and `dense` page rankings.

use std::collections::HashMap;
use std::path::Path;

use super::{Backend, BackendConfig, BackendError, Bm25Backend, DenseBackend};
use crate::index::Hit;
use crate::index::bm25::float;

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

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::*;
    use crate::backend::testing::{dense_config, fixture_pages};
    use crate::embed::Embedder;
    use crate::embed::testing::FakeEmbedder;

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
}
