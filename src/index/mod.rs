//! Built-in BM25 measurement backend (SPEC §5): tokeniser, section splitting and the tantivy
//! index over an artifact directory.
//!
//! The rules are this tool's retrieval model, pinned by the golden corpus (SPEC §7.3):
//!
//! - every `<source>/…/*.md` under the artifact is a page (directories starting with `_` and
//!   `manifest.json` are skipped; a `manifest.json` is not required, `<source>/meta.json` is
//!   read for the repo slug, module name and navigation titles);
//! - the page is split into retrieval units: the intro before the first H2 and one unit per H2
//!   section, H2 sections over [`SECTION_SPLIT_TOKENS`] tokens split again at H3;
//! - a unit is scored over three fields, `title` (boost [`TITLE_BOOST`]), `heading` (boost
//!   [`HEADING_BOOST`]) and `body`; the page score is its best unit; results are de-duplicated
//!   by tokenised title;
//! - the mirror rule: when two sources carry a page with the same title key (navigation title
//!   or H1), only the page from the source with the higher `priority` is indexed; priorities
//!   come from `pinakes.yaml` alone and equal priorities never collapse anything.
//!
//! Scoring is Okapi BM25 with `k1 = 1.5`, `b = 0.75` and negative IDFs floored at a quarter of
//! the average IDF, computed from the tantivy postings and an exact per-unit length. Field
//! boosts multiply the term frequency and the unit length, as repeating the title and heading
//! tokens would. This is the common `rank_bm25` `BM25Okapi` formula, so results are comparable
//! with that library; tantivy's own scorer (`k1 = 1.2`, Lucene IDF, quantised lengths) would
//! rank differently and is not used.
//!
//! The code lives in three submodules, `tokenizer`, `sections` and `bm25`, re-exported here;
//! loading an artifact into pages is [`crate::corpus`], re-exported here as well.

use std::path::{Path, PathBuf};

use thiserror::Error;

pub(crate) mod bm25;
pub(crate) mod sections;
pub(crate) mod tokenizer;

pub use crate::corpus::{
    DEFAULT_PRIORITY, Page, Priorities, load_pages, load_residue_page, mark_mirrors,
};
pub use bm25::{HEADING_BOOST, Hit, Index, TITLE_BOOST, Unit, iter_units};
pub use sections::{
    SECTION_SPLIT_TOKENS, Section, clean_content, extract_title, index_text, split_sections,
};
// Old name, still used by backend; remove once backend is updated.
pub use tokenizer::PinakesTokenizer as CuratorTokenizer;
pub use tokenizer::{
    PinakesTokenStream, PinakesTokenizer, STOPWORDS, TOKENIZER_NAME, is_stopword, title_key,
    tokenize,
};

/// Errors raised while loading pages or building the index.
#[derive(Debug, Error)]
pub enum IndexError {
    /// A filesystem operation failed.
    #[error("{path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The artifact path is not a directory.
    #[error("{0}: not an artifact directory")]
    NotADirectory(PathBuf),
    /// tantivy failed.
    #[error("index: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    /// Reading postings failed.
    #[error("index: reading postings: {0}")]
    Postings(#[from] std::io::Error),
    /// A page id is not `<source>::<path>`.
    #[error("invalid page id {0:?}: expected <source>::<path>")]
    BadPageId(String),
    /// `--with` named a page that is not in `_residue`.
    #[error("{id}: no residue page at {path}")]
    MissingResidue {
        /// The page id.
        id: String,
        /// Where the page was expected.
        path: PathBuf,
    },
    /// `--with` named a page that is already in the corpus.
    #[error("{0}: already in the corpus")]
    AlreadyPresent(String),
    /// `--without` named a page that is not in the corpus.
    #[error("{0}: not in the corpus")]
    UnknownPage(String),
}

pub(crate) fn io(path: &Path) -> impl FnOnce(std::io::Error) -> IndexError + '_ {
    move |source| IndexError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Helpers for building synthetic artifacts in tests.
#[cfg(test)]
pub(crate) mod testing {
    use std::path::Path;

    /// A source to write: directory name, repo slug, `(path, nav title, content)` pages and
    /// `(path, content)` residue pages.
    pub struct SourceSpec<'a> {
        pub name: &'a str,
        pub repo: &'a str,
        pub pages: &'a [(&'a str, &'a str, &'a str)],
        pub residue: &'a [(&'a str, &'a str)],
    }

    /// Write `sources` as an artifact directory with a `meta.json` per source (no manifest).
    pub fn write_artifact(dir: &Path, sources: &[SourceSpec<'_>]) {
        for source in sources {
            let root = dir.join(source.name);
            let mut pages = serde_json::Map::new();
            for (path, title, content) in source.pages {
                let file = root.join(path);
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(&file, content).unwrap();
                if !title.is_empty() {
                    pages.insert(
                        (*path).to_string(),
                        serde_json::json!({"title": title, "doc_type": "", "section": ""}),
                    );
                }
            }
            std::fs::create_dir_all(&root).unwrap();
            let meta = serde_json::json!({
                "repo": source.repo,
                "module": source.name,
                "base_url": format!("https://github.com/{}/blob/abc", source.repo),
                "commit": "abc",
                "pages": pages,
            });
            std::fs::write(root.join("meta.json"), meta.to_string()).unwrap();
            for (path, content) in source.residue {
                let file = dir.join("_residue").join(source.name).join(path);
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(&file, content).unwrap();
            }
        }
    }
}
