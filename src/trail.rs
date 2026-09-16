//! `trail.jsonl`: what a consumer served, read-only (SPEC §15.1).
//!
//! Consumers write this file; pinakes only reads it. One JSON object per line:
//! `{"at", "query", "retrieved": [...], "ranks": [...], "cited": [...], "outcome", "session"}`.
//! Every field but `at` and `query` is optional and defaults to empty/unknown, since consumers
//! vary in how much they log. Every id in `retrieved` and `cited` must be `<source>::<path>`
//! (SPEC §2.2); a line with a differently shaped id is rejected rather than silently accepted.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::manifest::split_page_id;

/// Errors raised while reading `trail.jsonl`.
#[derive(Debug, Error)]
pub enum TrailError {
    /// The file could not be read.
    #[error("{path}: {source}")]
    Io {
        /// The trail file path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A line is not a valid trail entry.
    #[error("{path}:{line}: invalid trail entry: {source}")]
    Json {
        /// The trail file path.
        path: PathBuf,
        /// One-based line number.
        line: usize,
        /// Underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// A line names an id that is not `<source>::<path>`.
    #[error("{path}:{line}: {field} contains {id:?}, which is not a <source>::<path> id")]
    BadId {
        /// The trail file path.
        path: PathBuf,
        /// One-based line number.
        line: usize,
        /// `retrieved` or `cited`.
        field: &'static str,
        /// The offending id.
        id: String,
    },
}

/// What the consumer recorded happened with the response, when it recorded anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The response was judged good.
    Ok,
    /// The response was judged bad.
    Bad,
    /// Not recorded, or recorded as unknown.
    #[default]
    Unknown,
}

/// One served query, as a consumer recorded it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrailEntry {
    /// RFC 3339 UTC time the query was served.
    pub at: String,
    /// The query text.
    pub query: String,
    /// Page ids returned, in retrieval order.
    #[serde(default)]
    pub retrieved: Vec<String>,
    /// Rank shown to the user for each entry of `retrieved` (same length, when given).
    #[serde(default)]
    pub ranks: Vec<u32>,
    /// Page ids the consumer says were actually cited or used.
    #[serde(default)]
    pub cited: Vec<String>,
    /// How the response fared, when the consumer recorded it.
    #[serde(default)]
    pub outcome: Outcome,
    /// An opaque session identifier, when the consumer gave one.
    #[serde(default)]
    pub session: String,
}

impl TrailEntry {
    /// The retrieved id ranked first, using `ranks` when it lines up with `retrieved`
    /// (`ranks[i]` is the rank of `retrieved[i]`), else the first id of `retrieved` as given.
    pub fn top_retrieved(&self) -> Option<&str> {
        if self.ranks.len() == self.retrieved.len() && !self.ranks.is_empty() {
            let best = self
                .ranks
                .iter()
                .enumerate()
                .min_by_key(|(_, rank)| **rank)
                .map(|(index, _)| index);
            if let Some(index) = best {
                return self.retrieved.get(index).map(String::as_str);
            }
        }
        self.retrieved.first().map(String::as_str)
    }
}

fn check_ids<'a>(
    path: &Path,
    line: usize,
    field: &'static str,
    ids: impl IntoIterator<Item = &'a String>,
) -> Result<(), TrailError> {
    for id in ids {
        if split_page_id(id).is_none() {
            return Err(TrailError::BadId {
                path: path.to_path_buf(),
                line,
                field,
                id: id.clone(),
            });
        }
    }
    Ok(())
}

/// Read entries from `path`; blank lines are skipped. Every `retrieved`/`cited` id is validated
/// as `<source>::<path>`.
pub fn read_jsonl(path: &Path) -> Result<Vec<TrailEntry>, TrailError> {
    let text = std::fs::read_to_string(path).map_err(|source| TrailError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut entries = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let line_no = index + 1;
        let entry: TrailEntry = serde_json::from_str(line).map_err(|source| TrailError::Json {
            path: path.to_path_buf(),
            line: line_no,
            source,
        })?;
        check_ids(path, line_no, "retrieved", &entry.retrieved)?;
        check_ids(path, line_no, "cited", &entry.cited)?;
        entries.push(entry);
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(query: &str) -> TrailEntry {
        TrailEntry {
            at: "2026-09-16T12:00:00Z".to_string(),
            query: query.to_string(),
            retrieved: vec![
                "handbook::docs/a.md".to_string(),
                "handbook::docs/b.md".to_string(),
            ],
            ranks: vec![1, 2],
            cited: vec!["handbook::docs/a.md".to_string()],
            outcome: Outcome::Ok,
            session: "s1".to_string(),
        }
    }

    #[test]
    fn reads_a_fully_populated_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trail.jsonl");
        let line = serde_json::to_string(&entry("q")).unwrap();
        std::fs::write(&path, format!("{line}\n\n")).unwrap();
        let entries = read_jsonl(&path).unwrap();
        assert_eq!(entries, [entry("q")]);
        assert_eq!(entries[0].top_retrieved(), Some("handbook::docs/a.md"));
    }

    #[test]
    fn tolerates_missing_optional_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trail.jsonl");
        std::fs::write(
            &path,
            "{\"at\": \"2026-09-16T12:00:00Z\", \"query\": \"q\"}\n",
        )
        .unwrap();
        let entries = read_jsonl(&path).unwrap();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert!(e.retrieved.is_empty());
        assert!(e.ranks.is_empty());
        assert!(e.cited.is_empty());
        assert_eq!(e.outcome, Outcome::Unknown);
        assert_eq!(e.session, "");
        assert_eq!(e.top_retrieved(), None);
    }

    #[test]
    fn rejects_ids_that_are_not_source_path_shaped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trail.jsonl");
        std::fs::write(
            &path,
            "{\"at\": \"t\", \"query\": \"q\", \"retrieved\": [\"not-an-id\"]}\n",
        )
        .unwrap();
        let err = read_jsonl(&path).unwrap_err();
        assert!(
            matches!(
                err,
                TrailError::BadId {
                    field: "retrieved",
                    ..
                }
            ),
            "{err}"
        );

        std::fs::write(
            &path,
            "{\"at\": \"t\", \"query\": \"q\", \"cited\": [\"handbook::\"]}\n",
        )
        .unwrap();
        let err = read_jsonl(&path).unwrap_err();
        assert!(
            matches!(err, TrailError::BadId { field: "cited", .. }),
            "{err}"
        );
    }

    #[test]
    fn top_retrieved_falls_back_when_ranks_do_not_line_up() {
        let mut e = entry("q");
        e.ranks = vec![1];
        assert_eq!(e.top_retrieved(), Some("handbook::docs/a.md"));
        e.ranks = vec![2, 1];
        assert_eq!(e.top_retrieved(), Some("handbook::docs/b.md"));
    }

    #[test]
    fn missing_file_is_an_error_not_empty() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_jsonl(&dir.path().join("nope.jsonl")).unwrap_err();
        assert!(matches!(err, TrailError::Io { .. }));
    }

    #[test]
    fn bad_json_line_reports_its_line_number() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trail.jsonl");
        let line = serde_json::to_string(&entry("q")).unwrap();
        std::fs::write(&path, format!("{line}\n{{bad\n")).unwrap();
        let err = read_jsonl(&path).unwrap_err();
        assert!(matches!(err, TrailError::Json { line: 2, .. }), "{err}");
    }
}
