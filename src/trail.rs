//! `trail.jsonl`: what a consumer served, read-only (SPEC §15.1).
//!
//! Consumers write this file; pinakes only reads it. One JSON object per line:
//! `{"version", "at", "query", "retrieved": [...], "ranks": [...], "cited": [...], "outcome",
//! "session"}`. Every field but `at` and `query` is optional and defaults to empty/unknown, since
//! consumers vary in how much they log; a missing `version` means 1. The shape is the trail
//! contract `kanon` publishes as a JSON Schema (SPEC §15.1), and the version rule is its: within
//! a version changes are additive, a line of a newer version is rejected before the rest of the
//! line is read, with one line naming the file, the line and both versions. Every id in
//! `retrieved` and `cited` must be `<source>::<path>` (SPEC §2.2); a line with a differently
//! shaped id is rejected rather than silently accepted.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::manifest::split_page_id;

/// The `version` of the trail contract this build reads (SPEC §15.1); a line without one is
/// version 1.
pub const TRAIL_VERSION: u32 = 1;

fn default_version() -> u32 {
    1
}

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
    /// A line is of a newer trail version than this build reads.
    #[error(
        "{path}:{line}: version {found} is newer than the version {known} this pinakes reads; \
         upgrade pinakes"
    )]
    NewerVersion {
        /// The trail file path.
        path: PathBuf,
        /// One-based line number.
        line: usize,
        /// The line's `version`.
        found: u32,
        /// The version this build reads, [`TRAIL_VERSION`].
        known: u32,
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
    /// The trail contract version the line follows; missing means 1.
    #[serde(default = "default_version")]
    pub version: u32,
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

/// The one field read before a line is parsed as a [`TrailEntry`].
#[derive(Deserialize)]
struct Versioned {
    #[serde(default = "default_version")]
    version: u32,
}

/// Read entries from `path`; blank lines are skipped. Each line's `version` is checked against
/// [`TRAIL_VERSION`] before the rest of the line is read, so a newer line is reported as such
/// and not as a parse error, and every `retrieved`/`cited` id is validated as
/// `<source>::<path>`.
pub fn read_jsonl(path: &Path) -> Result<Vec<TrailEntry>, TrailError> {
    let text = std::fs::read_to_string(path).map_err(|source| TrailError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let json_error = |line: usize| {
        move |source| TrailError::Json {
            path: path.to_path_buf(),
            line,
            source,
        }
    };
    let mut entries = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let line_no = index + 1;
        let Versioned { version } = serde_json::from_str(line).map_err(json_error(line_no))?;
        if version > TRAIL_VERSION {
            return Err(TrailError::NewerVersion {
                path: path.to_path_buf(),
                line: line_no,
                found: version,
                known: TRAIL_VERSION,
            });
        }
        let entry: TrailEntry = serde_json::from_str(line).map_err(json_error(line_no))?;
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
            version: TRAIL_VERSION,
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
        assert_eq!(e.version, 1, "a missing version means 1");
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
    fn an_explicit_version_1_line_and_unknown_fields_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trail.jsonl");
        std::fs::write(
            &path,
            "{\"version\": 1, \"at\": \"t\", \"query\": \"q\", \"added_later\": [1]}\n",
        )
        .unwrap();
        let entries = read_jsonl(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].version, 1);
    }

    #[test]
    fn a_newer_version_is_rejected_naming_the_line_and_both_versions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trail.jsonl");
        let ok = serde_json::to_string(&entry("q")).unwrap();
        std::fs::write(
            &path,
            format!("{ok}\n\n{{\"version\": 2, \"at\": \"t\", \"query\": \"q\"}}\n"),
        )
        .unwrap();
        let err = read_jsonl(&path).unwrap_err();
        assert!(
            matches!(
                err,
                TrailError::NewerVersion {
                    line: 3,
                    found: 2,
                    known: 1,
                    ..
                }
            ),
            "{err}"
        );
        assert_eq!(
            err.to_string(),
            format!(
                "{}:3: version 2 is newer than the version 1 this pinakes reads; upgrade pinakes",
                path.display()
            )
        );
    }

    #[test]
    fn the_version_is_checked_before_the_rest_of_the_line_is_parsed() {
        // A newer version may have renamed or removed `at` and `query`: it is still reported as
        // newer, not as a missing field.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trail.jsonl");
        std::fs::write(&path, "{\"version\": 7, \"served_at\": \"t\"}\n").unwrap();
        let err = read_jsonl(&path).unwrap_err();
        assert!(
            matches!(err, TrailError::NewerVersion { found: 7, .. }),
            "{err}"
        );
    }

    #[test]
    fn a_version_that_is_not_an_integer_is_an_invalid_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trail.jsonl");
        std::fs::write(
            &path,
            "{\"version\": \"two\", \"at\": \"t\", \"query\": \"q\"}\n",
        )
        .unwrap();
        let err = read_jsonl(&path).unwrap_err();
        assert!(matches!(err, TrailError::Json { line: 1, .. }), "{err}");
    }

    #[test]
    fn line_numbers_count_blank_whitespace_and_crlf_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trail.jsonl");
        let ok = serde_json::to_string(&entry("q")).unwrap();
        // Line 1 is an entry ending in CRLF, 2 is blank, 3 whitespace only, 4 is newer.
        let newer = "{\"version\": 2, \"at\": \"t\", \"query\": \"q\"}";
        std::fs::write(&path, format!("{ok}\r\n\r\n \t \r\n{newer}\r\n")).unwrap();
        let err = read_jsonl(&path).unwrap_err();
        assert!(
            matches!(err, TrailError::NewerVersion { line: 4, .. }),
            "{err}"
        );
    }

    #[test]
    fn the_first_problem_in_file_order_wins_and_a_newer_line_beats_its_own_bad_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trail.jsonl");
        let bad_id = "{\"at\": \"t\", \"query\": \"q\", \"cited\": [\"nope\"]}";
        let newer = "{\"version\": 2, \"at\": \"t\", \"query\": \"q\", \"cited\": [\"nope\"]}";
        std::fs::write(&path, format!("{bad_id}\n{newer}\n")).unwrap();
        let err = read_jsonl(&path).unwrap_err();
        assert!(matches!(err, TrailError::BadId { line: 1, .. }), "{err}");
        std::fs::write(&path, format!("{newer}\n{bad_id}\n")).unwrap();
        let err = read_jsonl(&path).unwrap_err();
        assert!(
            matches!(err, TrailError::NewerVersion { line: 1, .. }),
            "{err}"
        );
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
