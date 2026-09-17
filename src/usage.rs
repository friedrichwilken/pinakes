//! `pinakes usage`: what a trail says about a corpus that is actually being used (SPEC §15.3).
//!
//! Four things come out of a trail: pages the manifest carries that were never retrieved in the
//! window (removal candidates), pages that were retrieved but never cited, queries that got no
//! citation at all (grouped, when rendered, by their top retrieved page), and for each such
//! query the best-scoring residue page (a possible gap candidate), found with a small BM25-like
//! index built over `_residue` with the public tokeniser and content cleaning of [`crate::index`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::index::{clean_content, index_text, tokenize};
use crate::layout::RESIDUE_DIR;
use crate::manifest::{Manifest, page_id};
use crate::sources::to_slash_path;
use crate::trail::TrailEntry;

/// BM25 `k1` for the residue gap index (a fixed, generous default; this index only ranks a
/// handful of leftover pages, it does not need to be tuned).
const K1: f64 = 1.5;
/// BM25 `b` for the residue gap index.
const B: f64 = 0.75;

/// Errors raised while computing or reading/writing usage statistics.
#[derive(Debug, Error)]
pub enum UsageError {
    /// The file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The file path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The file is not valid usage JSON.
    #[error("{path}: invalid usage report: {source}")]
    Json {
        /// The file path.
        path: PathBuf,
        /// Underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// `--since` was not `<number><unit>` with unit one of `d`, `h`, `m`, `s` (or bare seconds).
    #[error("invalid --since {0:?}: expected a number optionally suffixed d, h, m or s")]
    BadSince(String),
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> UsageError + '_ {
    move |source| UsageError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// The best-scoring residue page found for an uncited query, and its score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GapCandidate {
    /// `<source>::<path>` of the residue page.
    pub id: String,
    /// Its BM25 score against the query.
    pub score: f64,
}

/// One query that received no citation across the window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UncitedQuery {
    /// The query text.
    pub query: String,
    /// The top-ranked page retrieved for it, when any trail entry recorded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_retrieved: Option<String>,
    /// The best-scoring residue page for the query text, when the residue index found one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_residue: Option<GapCandidate>,
}

/// Usage statistics over a trail window (SPEC §15.3).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// Manifest pages never retrieved in the window.
    #[serde(default)]
    pub never_retrieved: Vec<String>,
    /// Pages retrieved in the window but never cited.
    #[serde(default)]
    pub retrieved_never_cited: Vec<String>,
    /// Queries with no citation across every trail entry for that query text.
    #[serde(default)]
    pub uncited_queries: Vec<UncitedQuery>,
}

impl Usage {
    /// Read a usage report file.
    pub fn load(path: &Path) -> Result<Usage, UsageError> {
        let text = std::fs::read_to_string(path).map_err(io_err(path))?;
        serde_json::from_str(&text).map_err(|source| UsageError::Json {
            path: path.to_path_buf(),
            source,
        })
    }

    /// The report as pretty JSON with a trailing newline.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        Ok(text)
    }

    /// Write the report as pretty JSON with a trailing newline.
    pub fn save(&self, path: &Path) -> Result<(), UsageError> {
        let text = self.to_json().map_err(|source| UsageError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        std::fs::write(path, text).map_err(io_err(path))
    }
}

/// Parse `--since` (`30d`, `12h`, `45m`, `90s`, or a bare number of seconds) into seconds.
pub fn parse_since(text: &str) -> Result<i64, UsageError> {
    let bad = || UsageError::BadSince(text.to_string());
    if text.is_empty() {
        return Err(bad());
    }
    let (number, multiplier) = match text.as_bytes()[text.len() - 1] {
        b'd' => (&text[..text.len() - 1], 86_400i64),
        b'h' => (&text[..text.len() - 1], 3_600i64),
        b'm' => (&text[..text.len() - 1], 60i64),
        b's' => (&text[..text.len() - 1], 1i64),
        _ => (text, 1i64),
    };
    let count: i64 = number.parse().map_err(|_| bad())?;
    count.checked_mul(multiplier).ok_or_else(bad)
}

/// Keep only the trail entries at or after `now` minus `since_seconds`; entries whose `at`
/// cannot be parsed are kept (tolerant, per SPEC §15.1). `None` keeps every entry.
pub fn filter_since(
    entries: &[TrailEntry],
    since_seconds: Option<i64>,
    now: jiff::Timestamp,
) -> Vec<TrailEntry> {
    let Some(seconds) = since_seconds else {
        return entries.to_vec();
    };
    let duration = jiff::SignedDuration::from_secs(seconds);
    let Ok(cutoff) = now.checked_sub(duration) else {
        return entries.to_vec();
    };
    entries
        .iter()
        .filter(|e| match e.at.parse::<jiff::Timestamp>() {
            Ok(at) => at >= cutoff,
            Err(_) => true,
        })
        .cloned()
        .collect()
}

/// One residue page in the gap-candidate index.
struct ResidueDoc {
    id: String,
    term_freq: BTreeMap<String, u32>,
    len: usize,
}

/// A small BM25 index over `_residue`, used to find the best leftover page for an uncited
/// query's text (SPEC §15.3).
pub struct ResidueIndex {
    docs: Vec<ResidueDoc>,
    doc_freq: BTreeMap<String, usize>,
    avgdl: f64,
}

fn residue_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            residue_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
}

/// The page id of a residue file, given the `_residue` root it was found under.
fn residue_id(root: &Path, file: &Path) -> Option<String> {
    let rel = file.strip_prefix(root).ok()?;
    let mut components = rel.components();
    let source = components.next()?.as_os_str().to_str()?;
    let rest = to_slash_path(components.as_path());
    (!rest.is_empty()).then(|| page_id(source, &rest))
}

impl ResidueIndex {
    /// Build the index from `<artifact>/_residue`; an artifact with no residue yields an index
    /// that scores nothing.
    pub fn build(artifact: &Path) -> ResidueIndex {
        let root = artifact.join(RESIDUE_DIR);
        let mut files = Vec::new();
        residue_files(&root, &mut files);
        files.sort();
        let mut docs = Vec::new();
        let mut doc_freq: BTreeMap<String, usize> = BTreeMap::new();
        for file in files {
            let Some(id) = residue_id(&root, &file) else {
                continue;
            };
            let Ok(bytes) = std::fs::read(&file) else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            let tokens = tokenize(&index_text(&clean_content(&text)));
            if tokens.is_empty() {
                continue;
            }
            let mut term_freq: BTreeMap<String, u32> = BTreeMap::new();
            for token in &tokens {
                *term_freq.entry(token.clone()).or_insert(0) += 1;
            }
            for token in term_freq.keys() {
                *doc_freq.entry(token.clone()).or_insert(0) += 1;
            }
            docs.push(ResidueDoc {
                id,
                len: tokens.len(),
                term_freq,
            });
        }
        let avgdl = if docs.is_empty() {
            0.0
        } else {
            #[allow(clippy::cast_precision_loss)]
            let total: f64 = docs.iter().map(|d| d.len as f64).sum();
            #[allow(clippy::cast_precision_loss)]
            let n = docs.len() as f64;
            total / n
        };
        ResidueIndex {
            docs,
            doc_freq,
            avgdl,
        }
    }

    /// The residue page that scores highest against `query`'s tokens, if any scores at all.
    pub fn best(&self, query: &str) -> Option<GapCandidate> {
        if self.docs.is_empty() {
            return None;
        }
        let tokens = tokenize(query);
        if tokens.is_empty() {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        let n = self.docs.len() as f64;
        let avgdl = self.avgdl.max(1.0);
        let mut best: Option<GapCandidate> = None;
        for doc in &self.docs {
            let mut score = 0.0;
            #[allow(clippy::cast_precision_loss)]
            let len = doc.len as f64;
            for token in &tokens {
                let Some(&tf) = doc.term_freq.get(token) else {
                    continue;
                };
                let df = *self.doc_freq.get(token).unwrap_or(&0);
                if df == 0 {
                    continue;
                }
                #[allow(clippy::cast_precision_loss)]
                let df = df as f64;
                let tf = f64::from(tf);
                let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                let denom = tf + K1 * (1.0 - B + B * len / avgdl);
                score += idf * (tf * (K1 + 1.0)) / denom;
            }
            if score > 0.0 && best.as_ref().is_none_or(|b| score > b.score) {
                best = Some(GapCandidate {
                    id: doc.id.clone(),
                    score,
                });
            }
        }
        best
    }
}

/// Compute usage statistics from `entries` (already filtered to the window) against `manifest`
/// and `residue_index`.
pub fn compute(manifest: &Manifest, entries: &[TrailEntry], residue_index: &ResidueIndex) -> Usage {
    let all_pages: BTreeSet<String> = manifest.pages().map(|(id, ..)| id).collect();
    let mut retrieved: BTreeSet<String> = BTreeSet::new();
    let mut cited: BTreeSet<String> = BTreeSet::new();
    let mut by_query: BTreeMap<&str, Vec<&TrailEntry>> = BTreeMap::new();
    for entry in entries {
        retrieved.extend(entry.retrieved.iter().cloned());
        cited.extend(entry.cited.iter().cloned());
        by_query
            .entry(entry.query.as_str())
            .or_default()
            .push(entry);
    }
    let never_retrieved: Vec<String> = all_pages.difference(&retrieved).cloned().collect();
    let retrieved_never_cited: Vec<String> = retrieved.difference(&cited).cloned().collect();

    let mut uncited_queries = Vec::new();
    for (query, group) in &by_query {
        if group.iter().any(|e| !e.cited.is_empty()) {
            continue;
        }
        let top_retrieved = group
            .iter()
            .find_map(|e| e.top_retrieved())
            .map(str::to_string);
        uncited_queries.push(UncitedQuery {
            query: (*query).to_string(),
            top_retrieved,
            best_residue: residue_index.best(query),
        });
    }
    Usage {
        never_retrieved,
        retrieved_never_cited,
        uncited_queries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{ManifestSource, PageEntry, SelectedBy};
    use crate::trail::Outcome;
    use std::collections::BTreeMap as Map;

    fn manifest() -> Manifest {
        let mut m = Manifest::new("2026-09-16T12:00:00Z".to_string());
        let mut pages = Map::new();
        for path in ["a.md", "b.md", "c.md"] {
            pages.insert(
                path.to_string(),
                PageEntry {
                    sha256: "aa".repeat(32),
                    title: path.to_string(),
                    doc_type: String::new(),
                    section: String::new(),
                    selected_by: SelectedBy::Include,
                    rendered_from: None,
                },
            );
        }
        m.sources.insert(
            "h".to_string(),
            ManifestSource {
                repo: "o/h".to_string(),
                repo_url: "https://github.com/o/h.git".to_string(),
                git_ref: "main".to_string(),
                commit: "a".repeat(40),
                archived: Some(false),
                resolver: "glob".to_string(),
                pages,
                residue: vec![],
                unresolved: vec![],
                unrendered: vec![],
                render: None,
            },
        );
        m
    }

    fn entry(query: &str, retrieved: &[&str], cited: &[&str], at: &str) -> TrailEntry {
        TrailEntry {
            at: at.to_string(),
            query: query.to_string(),
            retrieved: retrieved.iter().map(|s| (*s).to_string()).collect(),
            ranks: (1..=u32::try_from(retrieved.len()).unwrap()).collect(),
            cited: cited.iter().map(|s| (*s).to_string()).collect(),
            outcome: Outcome::Unknown,
            session: String::new(),
        }
    }

    fn empty_residue_index() -> ResidueIndex {
        ResidueIndex {
            docs: Vec::new(),
            doc_freq: Map::new(),
            avgdl: 0.0,
        }
    }

    #[test]
    fn computes_never_retrieved_and_never_cited() {
        let m = manifest();
        let entries = vec![
            entry("q1", &["h::a.md"], &["h::a.md"], "t"),
            entry("q2", &["h::b.md"], &[], "t"),
        ];
        let usage = compute(&m, &entries, &empty_residue_index());
        assert_eq!(usage.never_retrieved, ["h::c.md"]);
        assert_eq!(usage.retrieved_never_cited, ["h::b.md"]);
        assert_eq!(usage.uncited_queries.len(), 1);
        assert_eq!(usage.uncited_queries[0].query, "q2");
        assert_eq!(
            usage.uncited_queries[0].top_retrieved,
            Some("h::b.md".to_string())
        );
    }

    #[test]
    fn a_query_cited_in_any_of_its_occurrences_is_not_uncited() {
        let m = manifest();
        let entries = vec![
            entry("q1", &["h::a.md"], &[], "t1"),
            entry("q1", &["h::a.md"], &["h::a.md"], "t2"),
        ];
        let usage = compute(&m, &entries, &empty_residue_index());
        assert!(usage.uncited_queries.is_empty());
    }

    #[test]
    fn parse_since_supports_every_suffix_and_bare_seconds() {
        assert_eq!(parse_since("30d").unwrap(), 30 * 86_400);
        assert_eq!(parse_since("12h").unwrap(), 12 * 3_600);
        assert_eq!(parse_since("45m").unwrap(), 45 * 60);
        assert_eq!(parse_since("90s").unwrap(), 90);
        assert_eq!(parse_since("5").unwrap(), 5);
        assert!(parse_since("").is_err());
        assert!(parse_since("d").is_err());
        assert!(parse_since("abc").is_err());
        assert!(parse_since("99999999999999999999d").is_err());
    }

    #[test]
    fn filter_since_keeps_entries_in_the_window_and_unparseable_ones() {
        let now: jiff::Timestamp = "2026-09-16T12:00:00Z".parse().unwrap();
        let entries = vec![
            entry("recent", &[], &[], "2026-09-16T00:00:00Z"),
            entry("old", &[], &[], "2026-08-01T00:00:00Z"),
            entry("garbled", &[], &[], "not-a-timestamp"),
        ];
        let kept = filter_since(&entries, Some(30 * 86_400), now);
        let queries: Vec<&str> = kept.iter().map(|e| e.query.as_str()).collect();
        assert_eq!(queries, ["recent", "garbled"]);
        assert_eq!(filter_since(&entries, None, now).len(), 3);
    }

    #[test]
    fn residue_index_finds_the_best_scoring_page_for_a_query() {
        let dir = tempfile::tempdir().unwrap();
        let artifact = dir.path();
        std::fs::create_dir_all(artifact.join("_residue/handbook/docs")).unwrap();
        std::fs::write(
            artifact.join("_residue/handbook/docs/caching.md"),
            "# Caching\n\nEnable upload caching with a label on the bucket.\n",
        )
        .unwrap();
        std::fs::write(
            artifact.join("_residue/handbook/docs/unrelated.md"),
            "# Unrelated\n\nNothing to do with the query at all.\n",
        )
        .unwrap();
        let index = ResidueIndex::build(artifact);
        let best = index.best("enable caching").unwrap();
        assert_eq!(best.id, "handbook::docs/caching.md");
        assert!(best.score > 0.0);
        assert!(index.best("completely unmatched gibberish zzy").is_none());
    }

    #[test]
    fn residue_index_over_a_missing_directory_scores_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let index = ResidueIndex::build(dir.path());
        assert!(index.best("anything").is_none());
    }

    #[test]
    fn usage_json_round_trips() {
        let usage = Usage {
            never_retrieved: vec!["h::a.md".to_string()],
            retrieved_never_cited: vec![],
            uncited_queries: vec![UncitedQuery {
                query: "q".to_string(),
                top_retrieved: None,
                best_residue: Some(GapCandidate {
                    id: "h::gap.md".to_string(),
                    score: 1.5,
                }),
            }],
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        usage.save(&path).unwrap();
        assert_eq!(Usage::load(&path).unwrap(), usage);
    }
}
