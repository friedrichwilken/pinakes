//! `decisions.jsonl`: human- or agent-written verdicts on residue (SPEC §2.5).
//!
//! A decision applies only while the page's `sha256` matches; otherwise it is expired and the
//! page is residue again. Later lines override earlier ones for the same id, so the file is
//! append-only.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors raised while reading or writing `decisions.jsonl`.
#[derive(Debug, Error)]
pub enum DecisionError {
    /// The file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The decisions file path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A line is not a valid decision.
    #[error("{path}:{line}: invalid decision: {source}")]
    Json {
        /// The decisions file path.
        path: PathBuf,
        /// One-based line number.
        line: usize,
        /// Underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
}

/// The verdict on a residue page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// Add the page to the corpus.
    Include,
    /// Keep the page out and stop reporting it.
    Exclude,
    /// Looked at, undecided; keeps the page out of `residue list`.
    Unsure,
}

impl Verdict {
    /// The lowercase name used in files and on the command line.
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Include => "include",
            Verdict::Exclude => "exclude",
            Verdict::Unsure => "unsure",
        }
    }

    /// Parse the lowercase name.
    pub fn parse(text: &str) -> Option<Verdict> {
        match text {
            "include" => Some(Verdict::Include),
            "exclude" => Some(Verdict::Exclude),
            "unsure" => Some(Verdict::Unsure),
            _ => None,
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One decision line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    /// `<source>::<path>`.
    pub id: String,
    /// Hash of the page bytes the decision was made for.
    pub sha256: String,
    /// The verdict.
    pub decision: Verdict,
    /// One sentence of justification.
    #[serde(default)]
    pub reason: String,
    /// Who decided: a name or an agent.
    #[serde(default)]
    pub by: String,
    /// RFC 3339 UTC time of the decision.
    #[serde(default)]
    pub at: String,
}

impl Decision {
    /// Whether this decision still applies to a page with hash `sha256`.
    pub fn applies_to(&self, sha256: &str) -> bool {
        !self.sha256.is_empty() && self.sha256 == sha256
    }
}

/// A decision whose page hash no longer matches (or whose page is gone).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expired {
    /// The decision that expired.
    pub decision: Decision,
    /// The page's current hash, or `None` when the page no longer exists.
    pub current_sha256: Option<String>,
}

/// Read every decision line in file order; a missing file yields no decisions.
pub fn read_jsonl(path: &Path) -> Result<Vec<Decision>, DecisionError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(DecisionError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let mut decisions = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let decision = serde_json::from_str(line).map_err(|source| DecisionError::Json {
            path: path.to_path_buf(),
            line: index + 1,
            source,
        })?;
        decisions.push(decision);
    }
    Ok(decisions)
}

/// Append one decision line, creating the file when needed.
pub fn append(path: &Path, decision: &Decision) -> Result<(), DecisionError> {
    let io = |source| DecisionError::Io {
        path: path.to_path_buf(),
        source,
    };
    let value = serde_json::to_value(decision).map_err(|source| DecisionError::Json {
        path: path.to_path_buf(),
        line: 0,
        source,
    })?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(io)?;
    let mut line = value.to_string();
    line.push('\n');
    file.write_all(line.as_bytes()).map_err(io)
}

/// The effective decision per id: later lines override earlier ones.
pub fn effective(decisions: &[Decision]) -> BTreeMap<String, Decision> {
    decisions
        .iter()
        .map(|d| (d.id.clone(), d.clone()))
        .collect()
}

/// Which effective decisions no longer apply, given a lookup of the current hash per id.
pub fn expired(
    effective: &BTreeMap<String, Decision>,
    current_hash: impl Fn(&str) -> Option<String>,
) -> Vec<Expired> {
    effective
        .values()
        .filter_map(|decision| {
            let current = current_hash(&decision.id);
            let still_valid = current.as_deref().is_some_and(|h| decision.applies_to(h));
            (!still_valid).then(|| Expired {
                decision: decision.clone(),
                current_sha256: current,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(id: &str, sha: &str, verdict: Verdict) -> Decision {
        Decision {
            id: id.to_string(),
            sha256: sha.to_string(),
            decision: verdict,
            reason: "because".to_string(),
            by: "tester".to_string(),
            at: "2026-09-16T12:00:00Z".to_string(),
        }
    }

    #[test]
    fn later_lines_override_earlier_ones() {
        let lines = vec![
            decision("a::x.md", "1", Verdict::Include),
            decision("a::y.md", "2", Verdict::Unsure),
            decision("a::x.md", "1", Verdict::Exclude),
        ];
        let eff = effective(&lines);
        assert_eq!(eff.len(), 2);
        assert_eq!(eff["a::x.md"].decision, Verdict::Exclude);
        assert_eq!(eff["a::y.md"].decision, Verdict::Unsure);
    }

    #[test]
    fn decisions_expire_when_the_hash_changes_or_the_page_is_gone() {
        let eff = effective(&[
            decision("a::same.md", "1", Verdict::Include),
            decision("a::changed.md", "2", Verdict::Exclude),
            decision("a::gone.md", "3", Verdict::Unsure),
        ]);
        let hashes: BTreeMap<&str, &str> = [("a::same.md", "1"), ("a::changed.md", "9")].into();
        let expired = expired(&eff, |id| hashes.get(id).map(|h| (*h).to_string()));
        let ids: Vec<(&str, Option<&str>)> = expired
            .iter()
            .map(|e| (e.decision.id.as_str(), e.current_sha256.as_deref()))
            .collect();
        assert_eq!(ids, [("a::changed.md", Some("9")), ("a::gone.md", None)]);
        assert!(eff["a::same.md"].applies_to("1"));
        assert!(!eff["a::same.md"].applies_to("2"));
        assert!(!decision("a::x", "", Verdict::Include).applies_to(""));
    }

    #[test]
    fn append_and_read_round_trip_and_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("decisions.jsonl");
        assert!(read_jsonl(&path).unwrap().is_empty());
        let first = decision("a::x.md", "1", Verdict::Include);
        let second = decision("a::x.md", "1", Verdict::Exclude);
        append(&path, &first).unwrap();
        append(&path, &second).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.starts_with("{\"at\":\"2026-09-16T12:00:00Z\",\"by\":\"tester\""));
        assert_eq!(read_jsonl(&path).unwrap(), vec![first, second]);
        std::fs::write(&path, "{\"id\": 1}\n").unwrap();
        let err = read_jsonl(&path).unwrap_err();
        assert!(matches!(err, DecisionError::Json { line: 1, .. }), "{err}");
        assert_eq!(Verdict::parse("include"), Some(Verdict::Include));
        assert_eq!(Verdict::parse("maybe"), None);
        assert_eq!(Verdict::Unsure.to_string(), "unsure");
    }
}
