//! `bm25` (SPEC §16.1): the SPEC §5 [`Index`], wrapped rather than rewritten.

use std::path::Path;

use super::{Backend, BackendConfig, BackendError};
use crate::index::{Hit, Index};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::testing::fixture_pages;
    use crate::index::Priorities;

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
}
