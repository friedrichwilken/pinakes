//! `residue.jsonl`: what was left out of the corpus, and why (SPEC §2.4).
//!
//! One JSON object per line. The file is machine-written by `resolve` and read by
//! `residue list`, `decide` and `report`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::decisions::Decision;

/// Roughly how many whitespace-separated tokens an excerpt keeps.
pub const EXCERPT_TOKENS: usize = 600;

/// Errors raised while reading or writing `residue.jsonl`.
#[derive(Debug, Error)]
pub enum ResidueError {
    /// The file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The residue file path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A line is not a valid residue entry.
    #[error("{path}:{line}: invalid residue entry: {source}")]
    Json {
        /// The residue file path.
        path: PathBuf,
        /// One-based line number.
        line: usize,
        /// Underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
}

/// Why a page is residue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// In scope but not selected by the resolver.
    NotSelected,
    /// A navigation link the resolver reported that has no file.
    UnresolvedLink,
    /// Not selected, in a source that is new since the previous manifest.
    NewSource,
}

impl Reason {
    /// The `snake_case` name used in files and on the command line.
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::NotSelected => "not_selected",
            Reason::UnresolvedLink => "unresolved_link",
            Reason::NewSource => "new_source",
        }
    }

    /// Parse the `snake_case` name.
    pub fn parse(text: &str) -> Option<Reason> {
        match text {
            "not_selected" => Some(Reason::NotSelected),
            "unresolved_link" => Some(Reason::UnresolvedLink),
            "new_source" => Some(Reason::NewSource),
            _ => None,
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One leftover page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResidueEntry {
    /// `<source>::<path>`.
    pub id: String,
    /// Source name.
    pub source: String,
    /// Path relative to the repository root.
    pub path: String,
    /// Why the page is residue.
    pub reason: Reason,
    /// Hex SHA-256 of the file bytes; empty for unresolved links.
    #[serde(default)]
    pub sha256: String,
    /// Page title, if one could be determined.
    #[serde(default)]
    pub title: String,
    /// The first ~600 tokens of the page body.
    #[serde(default)]
    pub excerpt: String,
    /// Sidebar section or TOC branch when the resolver gave one.
    #[serde(default)]
    pub context: String,
}

/// The first `max_tokens` whitespace-separated tokens of `text`, joined by single spaces.
pub fn excerpt(text: &str, max_tokens: usize) -> String {
    text.split_whitespace()
        .take(max_tokens)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Serialise entries as JSONL with sorted keys, one object per line.
pub fn to_jsonl(entries: &[ResidueEntry]) -> Result<String, serde_json::Error> {
    let mut out = String::new();
    for entry in entries {
        out.push_str(&serde_json::to_string(&serde_json::to_value(entry)?)?);
        out.push('\n');
    }
    Ok(out)
}

/// Write entries to `path` as JSONL.
pub fn write_jsonl(path: &Path, entries: &[ResidueEntry]) -> Result<(), ResidueError> {
    let text = to_jsonl(entries).map_err(|source| ResidueError::Json {
        path: path.to_path_buf(),
        line: 0,
        source,
    })?;
    std::fs::write(path, text).map_err(|source| ResidueError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Read entries from `path`; blank lines are skipped.
pub fn read_jsonl(path: &Path) -> Result<Vec<ResidueEntry>, ResidueError> {
    let text = std::fs::read_to_string(path).map_err(|source| ResidueError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut entries = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry = serde_json::from_str(line).map_err(|source| ResidueError::Json {
            path: path.to_path_buf(),
            line: index + 1,
            source,
        })?;
        entries.push(entry);
    }
    Ok(entries)
}

/// Filter for `residue list`.
#[derive(Debug, Default, Clone)]
pub struct ListFilter<'a> {
    /// Only entries of this source.
    pub source: Option<&'a str>,
    /// Only entries with this reason.
    pub reason: Option<Reason>,
}

/// Entries matching `filter` that have no active decision (one whose hash still matches).
pub fn list<'a>(
    entries: &'a [ResidueEntry],
    filter: &ListFilter<'_>,
    decisions: &BTreeMap<String, Decision>,
) -> Vec<&'a ResidueEntry> {
    entries
        .iter()
        .filter(|e| filter.source.is_none_or(|s| s == e.source))
        .filter(|e| filter.reason.is_none_or(|r| r == e.reason))
        .filter(|e| {
            !decisions
                .get(&e.id)
                .is_some_and(|d| d.applies_to(&e.sha256))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decisions::{Decision, Verdict};

    fn entry(source: &str, path: &str, reason: Reason) -> ResidueEntry {
        ResidueEntry {
            id: format!("{source}::{path}"),
            source: source.to_string(),
            path: path.to_string(),
            reason,
            sha256: "aa".repeat(32),
            title: "T".to_string(),
            excerpt: "some text".to_string(),
            context: String::new(),
        }
    }

    #[test]
    fn jsonl_round_trips_with_sorted_keys() {
        let entries = vec![
            entry("handbook", "docs/a.md", Reason::NotSelected),
            entry("guides", "docs/b.md", Reason::UnresolvedLink),
        ];
        let text = to_jsonl(&entries).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.starts_with(
            "{\"context\":\"\",\"excerpt\":\"some text\",\"id\":\"handbook::docs/a.md\""
        ));
        assert!(text.contains("\"reason\":\"unresolved_link\""));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("residue.jsonl");
        write_jsonl(&path, &entries).unwrap();
        assert_eq!(read_jsonl(&path).unwrap(), entries);
        std::fs::write(&path, "\n{bad\n").unwrap();
        let err = read_jsonl(&path).unwrap_err();
        assert!(matches!(err, ResidueError::Json { line: 2, .. }), "{err}");
    }

    #[test]
    fn excerpt_keeps_the_first_tokens() {
        assert_eq!(excerpt("a  b\nc d e", 3), "a b c");
        assert_eq!(excerpt("", 3), "");
        let long = "word ".repeat(1000);
        assert_eq!(
            excerpt(&long, EXCERPT_TOKENS).split(' ').count(),
            EXCERPT_TOKENS
        );
    }

    #[test]
    fn list_filters_and_hides_decided_entries() {
        let entries = vec![
            entry("handbook", "docs/a.md", Reason::NotSelected),
            entry("handbook", "docs/b.md", Reason::NewSource),
            entry("guides", "docs/c.md", Reason::NotSelected),
        ];
        let mut decisions = BTreeMap::new();
        let decided = Decision {
            id: "handbook::docs/a.md".to_string(),
            sha256: "aa".repeat(32),
            decision: Verdict::Exclude,
            reason: "noise".to_string(),
            by: "me".to_string(),
            at: "2026-09-16T12:00:00Z".to_string(),
        };
        decisions.insert(decided.id.clone(), decided.clone());
        let mut stale = decided;
        stale.id = "guides::docs/c.md".to_string();
        stale.sha256 = "bb".repeat(32);
        decisions.insert(stale.id.clone(), stale);

        let all = list(&entries, &ListFilter::default(), &decisions);
        let ids: Vec<&str> = all.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            ["handbook::docs/b.md", "guides::docs/c.md"],
            "decided hidden, stale shown"
        );

        let handbook = list(
            &entries,
            &ListFilter {
                source: Some("handbook"),
                reason: None,
            },
            &decisions,
        );
        assert_eq!(handbook.len(), 1);
        let by_reason = list(
            &entries,
            &ListFilter {
                source: None,
                reason: Some(Reason::NotSelected),
            },
            &decisions,
        );
        assert_eq!(by_reason[0].id, "guides::docs/c.md");
        assert_eq!(Reason::parse("new_source"), Some(Reason::NewSource));
        assert_eq!(Reason::parse("nope"), None);
        assert_eq!(Reason::UnresolvedLink.to_string(), "unresolved_link");
    }
}
