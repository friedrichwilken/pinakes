//! `diff OLD.json NEW.json`: what changed between two manifests.
//!
//! The JSON on stdout has `added`, `removed`, `changed` and `sources`; the human summary goes
//! to stderr. Exit code 3 signals differences (SPEC §4). [`Diff::compute`] itself only compares
//! the two manifests; per-page `lines_added`/`lines_removed` (SPEC §13) are filled in
//! separately by [`annotate_line_counts`] once the caller has the old and new page text, since
//! getting that text (from an artifact directory or a re-fetch) is `commands`' job, not this
//! module's.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::manifest::{Manifest, page_id};

/// A page that appeared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddedPage {
    /// `<source>::<path>`.
    pub id: String,
    /// Title in the new manifest.
    pub title: String,
}

/// Why a page disappeared, as far as two manifests can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalReason {
    /// The new manifest lists the path as residue: the resolver no longer selects it.
    DroppedByResolver,
    /// The whole source is gone from the new manifest.
    SourceRemoved,
    /// The path is neither a page nor residue any more: it left the upstream checkout.
    GoneUpstream,
}

impl RemovalReason {
    /// Human wording for the report.
    pub fn describe(self) -> &'static str {
        match self {
            RemovalReason::DroppedByResolver => "dropped by resolver",
            RemovalReason::SourceRemoved => "source removed",
            RemovalReason::GoneUpstream => "gone upstream",
        }
    }
}

/// A page that disappeared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovedPage {
    /// `<source>::<path>`.
    pub id: String,
    /// Title in the old manifest.
    pub title: String,
    /// Why it is gone.
    pub reason: RemovalReason,
}

/// A page whose bytes changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedPage {
    /// `<source>::<path>`.
    pub id: String,
    /// Title in the new manifest.
    pub title: String,
    /// Hash in the old manifest.
    pub old_sha256: String,
    /// Hash in the new manifest.
    pub new_sha256: String,
    /// Lines added between the old and new text (SPEC §13); `0` until [`annotate_line_counts`]
    /// fills it in, including when the text could not be obtained.
    #[serde(default)]
    pub lines_added: usize,
    /// Lines removed between the old and new text (SPEC §13); see [`Self::lines_added`].
    #[serde(default)]
    pub lines_removed: usize,
}

/// How a source changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceStatus {
    /// Only in the new manifest.
    Added,
    /// Only in the old manifest.
    Removed,
    /// Commit or page set differs.
    Changed,
    /// Identical pages and commit.
    Unchanged,
}

/// Per-source comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceDiff {
    /// What happened to the source.
    pub status: SourceStatus,
    /// `owner/repo`.
    pub repo: String,
    /// Commit in the old manifest, if present there.
    pub old_commit: Option<String>,
    /// Commit in the new manifest, if present there.
    pub new_commit: Option<String>,
}

impl SourceDiff {
    /// The GitHub compare URL when both commits are known and differ.
    pub fn compare_url(&self) -> Option<String> {
        match (&self.old_commit, &self.new_commit) {
            (Some(old), Some(new)) if old != new => Some(format!(
                "https://github.com/{}/compare/{old}...{new}",
                self.repo
            )),
            _ => None,
        }
    }
}

/// The difference between two manifests.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diff {
    /// Pages only in the new manifest, sorted by id.
    pub added: Vec<AddedPage>,
    /// Pages only in the old manifest, sorted by id.
    pub removed: Vec<RemovedPage>,
    /// Pages in both whose hash differs, sorted by id.
    pub changed: Vec<ChangedPage>,
    /// Every source in either manifest.
    pub sources: BTreeMap<String, SourceDiff>,
}

impl Diff {
    /// Compare `old` with `new`.
    pub fn compute(old: &Manifest, new: &Manifest) -> Diff {
        let mut diff = Diff::default();
        for (name, old_source) in &old.sources {
            let Some(new_source) = new.sources.get(name) else {
                diff.sources.insert(
                    name.clone(),
                    SourceDiff {
                        status: SourceStatus::Removed,
                        repo: old_source.repo.clone(),
                        old_commit: Some(old_source.commit.clone()),
                        new_commit: None,
                    },
                );
                for (path, entry) in &old_source.pages {
                    diff.removed.push(RemovedPage {
                        id: page_id(name, path),
                        title: entry.title.clone(),
                        reason: RemovalReason::SourceRemoved,
                    });
                }
                continue;
            };
            for (path, old_entry) in &old_source.pages {
                let id = page_id(name, path);
                match new_source.pages.get(path) {
                    Some(new_entry) if new_entry.sha256 != old_entry.sha256 => {
                        diff.changed.push(ChangedPage {
                            id,
                            title: new_entry.title.clone(),
                            old_sha256: old_entry.sha256.clone(),
                            new_sha256: new_entry.sha256.clone(),
                            lines_added: 0,
                            lines_removed: 0,
                        });
                    }
                    Some(_) => {}
                    None => {
                        let reason = if new_source.residue.contains(path) {
                            RemovalReason::DroppedByResolver
                        } else {
                            RemovalReason::GoneUpstream
                        };
                        diff.removed.push(RemovedPage {
                            id,
                            title: old_entry.title.clone(),
                            reason,
                        });
                    }
                }
            }
            for (path, new_entry) in &new_source.pages {
                if !old_source.pages.contains_key(path) {
                    diff.added.push(AddedPage {
                        id: page_id(name, path),
                        title: new_entry.title.clone(),
                    });
                }
            }
            let same =
                old_source.commit == new_source.commit && old_source.pages == new_source.pages;
            diff.sources.insert(
                name.clone(),
                SourceDiff {
                    status: if same {
                        SourceStatus::Unchanged
                    } else {
                        SourceStatus::Changed
                    },
                    repo: new_source.repo.clone(),
                    old_commit: Some(old_source.commit.clone()),
                    new_commit: Some(new_source.commit.clone()),
                },
            );
        }
        for (name, new_source) in &new.sources {
            if old.sources.contains_key(name) {
                continue;
            }
            diff.sources.insert(
                name.clone(),
                SourceDiff {
                    status: SourceStatus::Added,
                    repo: new_source.repo.clone(),
                    old_commit: None,
                    new_commit: Some(new_source.commit.clone()),
                },
            );
            for (path, entry) in &new_source.pages {
                diff.added.push(AddedPage {
                    id: page_id(name, path),
                    title: entry.title.clone(),
                });
            }
        }
        diff.added.sort_by(|a, b| a.id.cmp(&b.id));
        diff.removed.sort_by(|a, b| a.id.cmp(&b.id));
        diff.changed.sort_by(|a, b| a.id.cmp(&b.id));
        diff
    }

    /// Whether the manifests describe the same corpus (source metadata aside).
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.changed.is_empty()
            && self
                .sources
                .values()
                .all(|s| s.status == SourceStatus::Unchanged)
    }

    /// Sorted, indented JSON with a trailing newline.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        crate::manifest::to_sorted_json(self)
    }

    /// The human summary printed to stderr.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{} added, {} removed, {} changed",
            self.added.len(),
            self.removed.len(),
            self.changed.len()
        );
        for (name, source) in &self.sources {
            let detail = match source.status {
                SourceStatus::Added => format!("added @ {}", short(source.new_commit.as_deref())),
                SourceStatus::Removed => {
                    format!("removed (was @ {})", short(source.old_commit.as_deref()))
                }
                SourceStatus::Changed => format!(
                    "{} -> {}",
                    short(source.old_commit.as_deref()),
                    short(source.new_commit.as_deref())
                ),
                SourceStatus::Unchanged => "unchanged".to_string(),
            };
            let _ = writeln!(out, "  {name}: {detail}");
        }
        for page in &self.added {
            let _ = writeln!(out, "  + {}", page.id);
        }
        for page in &self.removed {
            let _ = writeln!(out, "  - {} ({})", page.id, page.reason.describe());
        }
        for page in &self.changed {
            let _ = writeln!(out, "  ~ {}", page.id);
        }
        out
    }
}

fn short(commit: Option<&str>) -> &str {
    let commit = commit.unwrap_or("?");
    &commit[..commit.len().min(12)]
}

/// Fill in `lines_added`/`lines_removed` for every changed page (SPEC §13). `content(id)`
/// returns the old and new text of the page; a page for which either side is `None` keeps its
/// counts at zero, since the text could not be obtained (an unreachable re-fetch, say).
pub fn annotate_line_counts(
    diff: &mut Diff,
    mut content: impl FnMut(&str) -> (Option<String>, Option<String>),
) {
    for page in &mut diff.changed {
        if let (Some(old), Some(new)) = content(&page.id) {
            let (added, removed) = line_diff(&old, &new);
            page.lines_added = added;
            page.lines_removed = removed;
        }
    }
}

/// Lines added and removed between `old` and `new`, from the length of their longest common
/// line subsequence: lines in `new` past that subsequence are additions, lines in `old` past it
/// are removals. A simple O(n·m) LCS, not a full diff, but exact for the counts SPEC §13 wants.
pub fn line_diff(old: &str, new: &str) -> (usize, usize) {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let common = longest_common_subsequence(&old_lines, &new_lines);
    (new_lines.len() - common, old_lines.len() - common)
}

/// Length of the longest common subsequence of two line slices, in O(min) space.
fn longest_common_subsequence(a: &[&str], b: &[&str]) -> usize {
    let mut previous = vec![0usize; b.len() + 1];
    let mut current = vec![0usize; b.len() + 1];
    for a_line in a {
        for (j, b_line) in b.iter().enumerate() {
            current[j + 1] = if a_line == b_line {
                previous[j] + 1
            } else {
                previous[j + 1].max(current[j])
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{ManifestSource, PageEntry, SelectedBy};

    fn page(sha: &str, title: &str) -> PageEntry {
        PageEntry {
            sha256: sha.to_string(),
            title: title.to_string(),
            doc_type: String::new(),
            section: String::new(),
            selected_by: SelectedBy::Include,
            rendered_from: None,
        }
    }

    fn source(commit: &str, pages: &[(&str, &str, &str)], residue: &[&str]) -> ManifestSource {
        ManifestSource {
            repo: "o/r".to_string(),
            repo_url: "https://github.com/o/r.git".to_string(),
            git_ref: "main".to_string(),
            commit: commit.to_string(),
            archived: Some(false),
            resolver: "glob".to_string(),
            pages: pages
                .iter()
                .map(|(p, s, t)| ((*p).to_string(), page(s, t)))
                .collect(),
            residue: residue.iter().map(|r| (*r).to_string()).collect(),
            unresolved: vec![],
            unrendered: vec![],
            render: None,
        }
    }

    fn manifest(sources: Vec<(&str, ManifestSource)>) -> Manifest {
        let mut m = Manifest::new("t".to_string());
        for (name, source) in sources {
            m.sources.insert(name.to_string(), source);
        }
        m
    }

    #[test]
    fn identical_manifests_are_empty() {
        let m = manifest(vec![("a", source("c1", &[("x.md", "1", "X")], &[]))]);
        let diff = Diff::compute(&m, &m);
        assert!(diff.is_empty());
        assert_eq!(diff.sources["a"].status, SourceStatus::Unchanged);
        assert!(diff.summary().contains("a: unchanged"));
    }

    #[test]
    fn classifies_added_removed_and_changed_pages() {
        let old = manifest(vec![
            (
                "a",
                source(
                    "c1",
                    &[
                        ("keep.md", "1", "Keep"),
                        ("mod.md", "2", "Mod"),
                        ("drop.md", "3", "Drop"),
                        ("gone.md", "4", "Gone"),
                    ],
                    &[],
                ),
            ),
            ("dead", source("d1", &[("z.md", "9", "Z")], &[])),
        ]);
        let new = manifest(vec![
            (
                "a",
                source(
                    "c2",
                    &[
                        ("keep.md", "1", "Keep"),
                        ("mod.md", "22", "Mod v2"),
                        ("new.md", "5", "New"),
                    ],
                    &["drop.md"],
                ),
            ),
            ("fresh", source("f1", &[("n.md", "7", "N")], &[])),
        ]);
        let diff = Diff::compute(&old, &new);
        assert!(!diff.is_empty());
        assert_eq!(
            diff.added,
            [
                AddedPage {
                    id: "a::new.md".into(),
                    title: "New".into()
                },
                AddedPage {
                    id: "fresh::n.md".into(),
                    title: "N".into()
                },
            ]
        );
        assert_eq!(
            diff.removed,
            [
                RemovedPage {
                    id: "a::drop.md".into(),
                    title: "Drop".into(),
                    reason: RemovalReason::DroppedByResolver
                },
                RemovedPage {
                    id: "a::gone.md".into(),
                    title: "Gone".into(),
                    reason: RemovalReason::GoneUpstream
                },
                RemovedPage {
                    id: "dead::z.md".into(),
                    title: "Z".into(),
                    reason: RemovalReason::SourceRemoved
                },
            ]
        );
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.changed[0].id, "a::mod.md");
        assert_eq!(diff.changed[0].old_sha256, "2");
        assert_eq!(diff.changed[0].new_sha256, "22");
        assert_eq!(diff.sources["a"].status, SourceStatus::Changed);
        assert_eq!(
            diff.sources["a"].compare_url().unwrap(),
            "https://github.com/o/r/compare/c1...c2"
        );
        assert_eq!(diff.sources["dead"].status, SourceStatus::Removed);
        assert!(diff.sources["dead"].compare_url().is_none());
        assert_eq!(diff.sources["fresh"].status, SourceStatus::Added);

        let json = diff.to_json().unwrap();
        assert!(json.starts_with("{\n  \"added\": [\n"));
        assert!(json.contains("\"reason\": \"dropped_by_resolver\""));
        assert!(
            json.contains("\"status\": \"source_removed\"")
                || json.contains("\"status\": \"removed\"")
        );
        let parsed: Diff = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, diff);

        let summary = diff.summary();
        assert!(summary.starts_with("2 added, 3 removed, 1 changed\n"));
        assert!(summary.contains("  a: c1 -> c2\n"));
        assert!(summary.contains("  - a::gone.md (gone upstream)\n"));
        assert!(summary.contains("  ~ a::mod.md\n"));
    }

    #[test]
    fn a_new_commit_with_identical_pages_still_counts_as_changed() {
        let old = manifest(vec![("a", source("c1", &[("x.md", "1", "X")], &[]))]);
        let new = manifest(vec![("a", source("c2", &[("x.md", "1", "X")], &[]))]);
        let diff = Diff::compute(&old, &new);
        assert!(!diff.is_empty());
        assert!(diff.added.is_empty() && diff.removed.is_empty() && diff.changed.is_empty());
    }

    #[test]
    fn changed_pages_start_with_zero_line_counts() {
        let old = manifest(vec![("a", source("c1", &[("x.md", "1", "X")], &[]))]);
        let new = manifest(vec![("a", source("c1", &[("x.md", "2", "X")], &[]))]);
        let diff = Diff::compute(&old, &new);
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.changed[0].lines_added, 0);
        assert_eq!(diff.changed[0].lines_removed, 0);
    }

    #[test]
    fn line_diff_counts_additions_removals_and_identical_text() {
        assert_eq!(line_diff("a\nb\nc\n", "a\nb\nc\n"), (0, 0));
        assert_eq!(line_diff("a\nb\n", "a\nb\nc\n"), (1, 0));
        assert_eq!(line_diff("a\nb\nc\n", "a\nb\n"), (0, 1));
        // One line changed in the middle: one removal, one addition, shared context kept.
        assert_eq!(line_diff("a\nb\nc\n", "a\nx\nc\n"), (1, 1));
        assert_eq!(line_diff("", ""), (0, 0));
        assert_eq!(line_diff("", "a\nb\n"), (2, 0));
    }

    #[test]
    fn annotate_line_counts_fills_reachable_pages_and_leaves_others_at_zero() {
        let old = manifest(vec![(
            "a",
            source("c1", &[("x.md", "1", "X"), ("y.md", "3", "Y")], &[]),
        )]);
        let new = manifest(vec![(
            "a",
            source("c2", &[("x.md", "2", "X"), ("y.md", "4", "Y")], &[]),
        )]);
        let mut diff = Diff::compute(&old, &new);
        assert_eq!(diff.changed.len(), 2);
        annotate_line_counts(&mut diff, |id| match id {
            "a::x.md" => (Some("a\nb\n".to_string()), Some("a\nb\nc\n".to_string())),
            // "a::y.md" content is unreachable: stays at zero.
            _ => (None, None),
        });
        let x = diff.changed.iter().find(|p| p.id == "a::x.md").unwrap();
        assert_eq!((x.lines_added, x.lines_removed), (1, 0));
        let y = diff.changed.iter().find(|p| p.id == "a::y.md").unwrap();
        assert_eq!((y.lines_added, y.lines_removed), (0, 0));
    }
}
