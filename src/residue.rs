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
use crate::jsonl::{self, JsonlError, KeyOrder};

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
    /// Kept out by `policy.deny`, a resolver's `exclude`, a decision, or an archived source
    /// dropped by `policy.archived` (SPEC §2.4). Never a candidate for `decide`: hidden from
    /// `residue list` unless `--include-excluded` is given, and shown in `report` as a
    /// collapsed, always-present count so nothing disappears without a trace.
    Excluded,
}

impl Reason {
    /// The `snake_case` name used in files and on the command line.
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::NotSelected => "not_selected",
            Reason::UnresolvedLink => "unresolved_link",
            Reason::NewSource => "new_source",
            Reason::Excluded => "excluded",
        }
    }

    /// Parse the `snake_case` name.
    pub fn parse(text: &str) -> Option<Reason> {
        match text {
            "not_selected" => Some(Reason::NotSelected),
            "unresolved_link" => Some(Reason::UnresolvedLink),
            "new_source" => Some(Reason::NewSource),
            "excluded" => Some(Reason::Excluded),
            _ => None,
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The mechanism that decided a residue entry's placement (SPEC §2.4): a short, stable key plus
/// a one-sentence explanation for a human. Constructors name every key this tool assigns on its
/// own; an external resolver may report its own key and text instead (SPEC §3), in which case it
/// is used verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// A short, stable identifier, e.g. `"policy:deny"`.
    pub key: String,
    /// One sentence explaining the decision.
    pub text: String,
}

impl Rule {
    fn new(key: &str, text: String) -> Rule {
        Rule {
            key: key.to_string(),
            text,
        }
    }

    /// A `vitepress` sidebar links other pages but not this one.
    pub fn sidebar_unlinked(nav_file: &str) -> Rule {
        Rule::new("sidebar:unlinked", format!("not linked from `{nav_file}`"))
    }

    /// A `docusaurus` sidebar links other pages but not this one.
    pub fn docusaurus_unlinked(nav_file: &str) -> Rule {
        Rule::new(
            "docusaurus:unlinked",
            format!("not linked from `{nav_file}`"),
        )
    }

    /// An `mdbook` `SUMMARY.md` links other pages but not this one.
    pub fn mdbook_unlinked(nav_file: &str) -> Rule {
        Rule::new("mdbook:unlinked", format!("not linked from `{nav_file}`"))
    }

    /// A sitemap lists other pages but not this one.
    pub fn sitemap_unlisted(nav_file: &str) -> Rule {
        Rule::new("sitemap:unlisted", format!("not listed in `{nav_file}`"))
    }

    /// A `glob` resolver's `include` (or `residue_scope`) matched, but the file's extension is
    /// not one of the configured ones (SPEC §2.1).
    pub fn glob_extension(extensions: &[String]) -> Rule {
        let list = if extensions.is_empty() {
            "none configured".to_string()
        } else {
            extensions.join(", ")
        };
        Rule::new(
            "glob:extension",
            format!("not one of the configured extensions ({list})"),
        )
    }

    /// A `glob` resolver's `residue_scope` matched, but `include` did not.
    pub fn glob_outside_include() -> Rule {
        Rule::new(
            "glob:outside-include",
            "outside the configured include patterns".to_string(),
        )
    }

    /// The external resolver command (SPEC §3) reported this candidate with `selected: false`,
    /// and did not supply its own `rule`.
    pub fn external_not_selected() -> Rule {
        Rule::new(
            "external:not-selected",
            "the resolver command reported it unselected".to_string(),
        )
    }

    /// A file inside the external resolver's `residue_scope` (SPEC §3) that its command's
    /// output never mentioned at all, selected or not.
    pub fn external_unmatched() -> Rule {
        Rule::new(
            "external:unmatched",
            "outside the paths the resolver command's output covers".to_string(),
        )
    }

    /// A navigation file links a page that does not exist in the checkout (residue reason
    /// [`Reason::UnresolvedLink`]).
    pub fn nav_dangling_link(nav_file: &str) -> Rule {
        Rule::new(
            "nav:dangling-link",
            format!("linked from `{nav_file}` but the file does not exist"),
        )
    }

    /// The external resolver command (SPEC §3) reported a candidate path that does not exist in
    /// the checkout; unlike the navigation-based resolvers, there is no navigation file to name.
    pub fn external_dangling_link() -> Rule {
        Rule::new(
            "external:dangling-link",
            "the resolver command reported a page that does not exist in the checkout".to_string(),
        )
    }

    /// `policy.deny` matched (SPEC §2.1); beats everything else.
    pub fn policy_deny(pattern: &str) -> Rule {
        Rule::new(
            "policy:deny",
            format!("matches `policy.deny` (`{pattern}`)"),
        )
    }

    /// A resolver's own `exclude` glob matched (SPEC §2.1).
    pub fn resolver_exclude(pattern: &str) -> Rule {
        Rule::new(
            "resolver:exclude",
            format!("matches `resolver.exclude` (`{pattern}`)"),
        )
    }

    /// A decision with verdict `exclude` is in effect for this page (SPEC §2.5).
    pub fn decision_exclude(by: &str, reason: &str) -> Rule {
        let by = if by.is_empty() { "a decision" } else { by };
        let reason = if reason.is_empty() {
            "no reason given"
        } else {
            reason
        };
        Rule::new("decision:exclude", format!("excluded by {by}: {reason}"))
    }

    /// The page's source is archived upstream and `policy.archived` is `drop` (SPEC §2.1).
    pub fn source_archived() -> Rule {
        Rule::new(
            "source:archived",
            "the source is archived upstream and policy.archived is drop".to_string(),
        )
    }

    /// The page's source is new since the previous manifest, so nothing in it has been reviewed
    /// yet (residue reason [`Reason::NewSource`]).
    pub fn source_new() -> Rule {
        Rule::new(
            "source:new",
            "the source appeared after the previous manifest, so nothing in it has been \
             reviewed yet"
                .to_string(),
        )
    }

    /// `resolve --from-manifest` (SPEC §2.2) rebuilds residue from the manifest's recorded
    /// paths alone, with no resolver plan to recover the original mechanism from.
    pub fn reproduced() -> Rule {
        Rule::new(
            "reproduced:from-manifest",
            "recomputed by `resolve --from-manifest`; the original resolver did not run"
                .to_string(),
        )
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
    /// The page's upstream URL pinned to the fetched commit (SPEC §2.4): for
    /// [`Reason::UnresolvedLink`] this points at the navigation file itself, since the linked
    /// page does not exist; empty when it could not be derived (e.g. the source's repo does not
    /// parse as a GitHub URL).
    #[serde(default)]
    pub url: String,
    /// The mechanism that decided this entry (SPEC §2.4); absent only for entries written by an
    /// older version of this tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<Rule>,
}

/// The first `max_tokens` whitespace-separated tokens of `text`, joined by single spaces.
pub fn excerpt(text: &str, max_tokens: usize) -> String {
    text.split_whitespace()
        .take(max_tokens)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Serialise entries as JSONL with sorted keys, one object per line, entries themselves sorted
/// by `(source, path)` (SPEC §2.4) so the file is byte-for-byte stable across runs regardless of
/// the order they were found in.
pub fn to_jsonl(entries: &[ResidueEntry]) -> Result<String, serde_json::Error> {
    jsonl::to_string(&sorted(entries), KeyOrder::Sorted)
}

fn sorted(entries: &[ResidueEntry]) -> Vec<&ResidueEntry> {
    let mut sorted: Vec<&ResidueEntry> = entries.iter().collect();
    sorted.sort_by(|a, b| {
        (a.source.as_str(), a.path.as_str()).cmp(&(b.source.as_str(), b.path.as_str()))
    });
    sorted
}

impl From<JsonlError> for ResidueError {
    fn from(err: JsonlError) -> Self {
        match err {
            JsonlError::Io { path, source } => ResidueError::Io { path, source },
            JsonlError::Json { path, line, source } => ResidueError::Json { path, line, source },
        }
    }
}

/// Write entries to `path` as JSONL.
pub fn write_jsonl(path: &Path, entries: &[ResidueEntry]) -> Result<(), ResidueError> {
    Ok(jsonl::write(path, &sorted(entries), KeyOrder::Sorted)?)
}

/// Read entries from `path`; blank lines are skipped.
pub fn read_jsonl(path: &Path) -> Result<Vec<ResidueEntry>, ResidueError> {
    Ok(jsonl::read(path)?)
}

/// Filter for `residue list`.
#[derive(Debug, Default, Clone)]
pub struct ListFilter<'a> {
    /// Only entries of this source.
    pub source: Option<&'a str>,
    /// Only entries with this reason.
    pub reason: Option<Reason>,
    /// Show [`Reason::Excluded`] entries too; they are never candidates for `decide` (SPEC
    /// §2.4), so they are hidden by default even when `reason` does not otherwise exclude them.
    /// Explicitly asking for `reason: Some(Reason::Excluded)` shows them regardless.
    pub include_excluded: bool,
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
            e.reason != Reason::Excluded
                || filter.include_excluded
                || filter.reason == Some(Reason::Excluded)
        })
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
            url: format!("https://github.com/o/r/blob/{}/{path}", "aa".repeat(20)),
            rule: None,
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
        // Entries are sorted by (source, path); "guides" comes before "handbook".
        assert!(text.starts_with(
            "{\"context\":\"\",\"excerpt\":\"some text\",\"id\":\"guides::docs/b.md\""
        ));
        assert!(text.contains("\"reason\":\"unresolved_link\""));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("residue.jsonl");
        write_jsonl(&path, &entries).unwrap();
        assert_eq!(
            read_jsonl(&path).unwrap(),
            [entries[1].clone(), entries[0].clone()],
            "the file is sorted regardless of the order entries were given in"
        );
        std::fs::write(&path, "\n{bad\n").unwrap();
        let err = read_jsonl(&path).unwrap_err();
        assert!(matches!(err, ResidueError::Json { line: 2, .. }), "{err}");
    }

    #[test]
    fn jsonl_is_byte_for_byte_stable_across_writes_and_input_order() {
        let a = entry("handbook", "docs/a.md", Reason::NotSelected);
        let b = entry("guides", "docs/b.md", Reason::UnresolvedLink);
        let c = entry("guides", "docs/a.md", Reason::NotSelected);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("residue.jsonl");

        write_jsonl(&path, &[a.clone(), b.clone(), c.clone()]).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        // Same entries, different input order and a second write: byte for byte identical.
        write_jsonl(&path, &[c.clone(), a.clone(), b.clone()]).unwrap();
        let second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first, second);

        let ids: Vec<&str> = first
            .lines()
            .map(|l| {
                let rest = &l[l.find("\"id\":\"").unwrap() + 6..];
                &rest[..rest.find('"').unwrap()]
            })
            .collect();
        assert_eq!(
            ids,
            [
                "guides::docs/a.md",
                "guides::docs/b.md",
                "handbook::docs/a.md"
            ],
            "sorted by (source, path)"
        );
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
                include_excluded: false,
            },
            &decisions,
        );
        assert_eq!(handbook.len(), 1);
        let by_reason = list(
            &entries,
            &ListFilter {
                source: None,
                reason: Some(Reason::NotSelected),
                include_excluded: false,
            },
            &decisions,
        );
        assert_eq!(by_reason[0].id, "guides::docs/c.md");
        assert_eq!(Reason::parse("new_source"), Some(Reason::NewSource));
        assert_eq!(Reason::parse("excluded"), Some(Reason::Excluded));
        assert_eq!(Reason::parse("nope"), None);
        assert_eq!(Reason::UnresolvedLink.to_string(), "unresolved_link");
    }

    #[test]
    fn excluded_entries_are_hidden_unless_included_or_explicitly_asked_for() {
        let entries = vec![
            entry("handbook", "docs/a.md", Reason::NotSelected),
            entry("handbook", "CHANGELOG.md", Reason::Excluded),
        ];
        let decisions = BTreeMap::new();

        let default = list(&entries, &ListFilter::default(), &decisions);
        assert_eq!(
            default.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["handbook::docs/a.md"],
            "excluded residue is hidden by default"
        );

        let included = list(
            &entries,
            &ListFilter {
                source: None,
                reason: None,
                include_excluded: true,
            },
            &decisions,
        );
        assert_eq!(included.len(), 2, "--include-excluded shows it");

        let asked_for = list(
            &entries,
            &ListFilter {
                source: None,
                reason: Some(Reason::Excluded),
                include_excluded: false,
            },
            &decisions,
        );
        assert_eq!(
            asked_for.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["handbook::CHANGELOG.md"],
            "asking for --reason excluded shows it even without --include-excluded"
        );
    }

    #[test]
    fn every_rule_constructor_names_a_stable_key_and_sentence() {
        let cases: Vec<(Rule, &str)> = vec![
            (
                Rule::sidebar_unlinked("docs/.vitepress/config.ts"),
                "sidebar:unlinked",
            ),
            (
                Rule::docusaurus_unlinked("sidebars.js"),
                "docusaurus:unlinked",
            ),
            (Rule::mdbook_unlinked("src/SUMMARY.md"), "mdbook:unlinked"),
            (Rule::sitemap_unlisted("sitemap.xml"), "sitemap:unlisted"),
            (Rule::glob_extension(&["md".to_string()]), "glob:extension"),
            (Rule::glob_outside_include(), "glob:outside-include"),
            (Rule::external_not_selected(), "external:not-selected"),
            (Rule::external_unmatched(), "external:unmatched"),
            (
                Rule::nav_dangling_link("src/SUMMARY.md"),
                "nav:dangling-link",
            ),
            (Rule::external_dangling_link(), "external:dangling-link"),
            (Rule::policy_deny("**/CHANGELOG.md"), "policy:deny"),
            (Rule::resolver_exclude("**/_sidebar.md"), "resolver:exclude"),
            (Rule::decision_exclude("alice", "noise"), "decision:exclude"),
            (Rule::source_archived(), "source:archived"),
            (Rule::source_new(), "source:new"),
        ];
        for (rule, key) in cases {
            assert_eq!(rule.key, key);
            assert!(!rule.text.is_empty(), "{key} has a sentence");
            assert!(
                rule.text.chars().next().is_some_and(char::is_lowercase),
                "{key}: {:?} reads as a clause, not a heading",
                rule.text
            );
        }
        assert!(
            Rule::policy_deny("**/CHANGELOG.md")
                .text
                .contains("**/CHANGELOG.md")
        );
        assert!(Rule::decision_exclude("", "").text.contains("a decision"));
        assert!(
            Rule::decision_exclude("", "")
                .text
                .contains("no reason given")
        );
        assert!(Rule::glob_extension(&[]).text.contains("none configured"));
    }
}
