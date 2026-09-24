//! `report.md`: the rendered PR body (SPEC §2.7), and `report.json`, its facts (SPEC §2.10).
//!
//! Sections, in order: summary counts; eval before/after (overall and per kind, held-out
//! separately); added pages; removed pages (with reason); changed pages (with line counts and
//! an upstream compare link, SPEC §13); new residue grouped by reason with excerpt; expired
//! decisions; unresolved links; archived sources; duplicates (SPEC §11); usage (SPEC §15.3),
//! only when a usage report is given.
//!
//! [`prepare`] computes everything the sections need once, into a [`Prepared`]; the Markdown
//! ([`Prepared::markdown`]) and the JSON facts ([`Prepared::facts`]) are both rendered from
//! it, so the two cannot drift.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::decisions::{self, Decision, Expired, Verdict};
use crate::diff::{Diff, RemovalReason, RemovedPage, SourceDiff};
use crate::duplicates::{DuplicateKind, DuplicatePair, Suggested};
use crate::eval::{EvalSummary, Metrics, Split};
use crate::manifest::Manifest;
use crate::page::PageRegistry;
use crate::residue::{Reason, ResidueEntry};
use crate::usage::{UncitedQuery, Usage};

/// Words of excerpt shown per residue entry.
const EXCERPT_WORDS: usize = 60;

/// The `version` of the `report.json` document this code writes (SPEC §2.10).
pub const FACTS_VERSION: u32 = 1;

/// Errors raised while reading or writing `report.json`.
#[derive(Debug, Error)]
pub enum ReportError {
    /// The file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The file path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The file is not a valid report document.
    #[error("{path}: invalid report facts: {source}")]
    Json {
        /// The file path.
        path: PathBuf,
        /// Underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
}

/// Everything the report is rendered from.
#[derive(Debug, Clone, Copy)]
pub struct ReportInput<'a> {
    /// The previous manifest, when there is one.
    pub old: Option<&'a Manifest>,
    /// The current manifest.
    pub new: &'a Manifest,
    /// A pre-computed diff, when the caller has one (SPEC §13's `lines_added`/`lines_removed`
    /// and `compare_url` need the artifacts or a re-fetch, which is `commands`' job). When
    /// `None` and `old` is given, one is computed from the manifests alone, with zero line
    /// counts.
    pub diff: Option<&'a Diff>,
    /// The page registry (selected and residue records) built from `new` and the current
    /// residue file.
    pub registry: &'a PageRegistry,
    /// Every decision line, in file order.
    pub decisions: &'a [Decision],
    /// Evaluation on the previous corpus.
    pub eval_before: Option<&'a EvalSummary>,
    /// Evaluation on the current corpus.
    pub eval_after: Option<&'a EvalSummary>,
    /// Current duplicate pairs (SPEC §11), empty when `duplicates.jsonl` was not supplied.
    pub duplicates: &'a [DuplicatePair],
    /// Usage statistics (SPEC §15.3); the "Usage" section is rendered only when this is given.
    pub usage: Option<&'a Usage>,
}

/// Residue entries that share one rule (SPEC §2.4), in registry order.
#[derive(Debug, Clone)]
struct RuleGroup {
    /// The rule's key, e.g. `"policy:deny"`.
    key: String,
    /// The rule's sentence, the group's heading.
    text: String,
    /// The entries in the group, as positions in [`Prepared::residue`].
    entries: Vec<usize>,
}

/// Why a removed page is reported as removed: the diff's reason (SPEC §13), or "excluded by
/// decision" when an effective `exclude` decision covers the page (SPEC §2.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovedReason {
    /// The path left the upstream checkout.
    GoneUpstream,
    /// The resolver no longer selects the path; it is residue now.
    DroppedByResolver,
    /// The whole source is gone from the new manifest.
    SourceRemoved,
    /// A decision excludes the page.
    ExcludedByDecision,
}

impl RemovedReason {
    /// Human wording for the Markdown report.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            RemovedReason::GoneUpstream => RemovalReason::GoneUpstream.describe(),
            RemovedReason::DroppedByResolver => RemovalReason::DroppedByResolver.describe(),
            RemovedReason::SourceRemoved => RemovalReason::SourceRemoved.describe(),
            RemovedReason::ExcludedByDecision => "excluded by decision",
        }
    }
}

/// Why a decision expired (SPEC §2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpiredWhy {
    /// The page still exists, with a different hash.
    PageChanged,
    /// The page is neither in the corpus nor in the residue any more.
    PageGone,
}

impl ExpiredWhy {
    fn of(item: &Expired) -> ExpiredWhy {
        if item.current_sha256.is_some() {
            ExpiredWhy::PageChanged
        } else {
            ExpiredWhy::PageGone
        }
    }

    /// Human wording for the Markdown report.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            ExpiredWhy::PageChanged => "page changed",
            ExpiredWhy::PageGone => "page gone",
        }
    }
}

/// Everything the report's sections need, computed once from a [`ReportInput`]; the Markdown
/// and the JSON facts are both rendered from this.
#[derive(Debug, Clone)]
pub struct Prepared<'a> {
    input: ReportInput<'a>,
    /// Sources in the current manifest.
    sources: usize,
    /// Pages in the current manifest.
    pages: usize,
    /// What "Changes since" names: the previous manifest's `generated_at`.
    since: String,
    /// The last decision per id (SPEC §2.5).
    effective: BTreeMap<String, Decision>,
    /// Every residue entry, in registry order.
    residue: Vec<ResidueEntry>,
    /// Positions in `residue` of the entries no effective decision covers at their current
    /// hash, in registry order.
    undecided: Vec<usize>,
    /// Residue ids an effective `exclude` decision covers at their current hash.
    active_excludes: BTreeSet<String>,
    /// Decisions whose page changed or went away.
    expired: Vec<Expired>,
    /// The diff against the previous manifest: the caller's, or one computed from the two
    /// manifests; `None` without a previous manifest.
    diff: Option<Cow<'a, Diff>>,
    /// "New residue": undecided, non-excluded entries the previous manifest did not list,
    /// grouped by rule in heading order.
    new_residue: Vec<RuleGroup>,
    /// Every reason `excluded` entry, grouped by rule in heading order.
    excluded: Vec<RuleGroup>,
    /// Positions in `residue` of the dangling navigation links, sorted by id.
    unresolved: Vec<usize>,
    /// `(name, repo)` of every source recorded as archived, by name.
    archived: Vec<(String, String)>,
    /// The duplicate pairs by kind, exact then mirror then near, kinds without pairs left out.
    duplicates: Vec<(DuplicateKind, Vec<&'a DuplicatePair>)>,
}

/// Compute everything the report's sections need.
#[must_use]
pub fn prepare(input: ReportInput<'_>) -> Prepared<'_> {
    let effective = decisions::effective(input.decisions);
    let residue = input.registry.residue_entries();
    let expired = decisions::expired(&effective, |id| {
        input.registry.get(id).map(|r| r.sha256.clone())
    });
    let diff = match (input.diff, input.old) {
        (Some(diff), _) => Some(Cow::Borrowed(diff)),
        (None, Some(old)) => Some(Cow::Owned(Diff::compute(old, input.new))),
        (None, None) => None,
    };
    let undecided: Vec<usize> = residue
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            !effective
                .get(&r.id)
                .is_some_and(|d| d.applies_to(&r.sha256))
        })
        .map(|(i, _)| i)
        .collect();
    let active_excludes: BTreeSet<String> = residue
        .iter()
        .filter(|r| {
            effective
                .get(&r.id)
                .is_some_and(|d| d.decision == Verdict::Exclude && d.applies_to(&r.sha256))
        })
        .map(|r| r.id.clone())
        .collect();
    let previous: BTreeSet<String> = input
        .old
        .map(|old| {
            old.residue_ids()
                .chain(old.sources.iter().flat_map(unresolved_ids))
                .collect()
        })
        .unwrap_or_default();
    let new_residue = group_by_rule(
        &residue,
        undecided.iter().copied().filter(|&i| {
            residue
                .get(i)
                .is_some_and(|r| !previous.contains(&r.id) && r.reason != Reason::Excluded)
        }),
    );
    let excluded = group_by_rule(&residue, positions(&residue, Reason::Excluded));
    let mut unresolved: Vec<usize> = positions(&residue, Reason::UnresolvedLink).collect();
    unresolved.sort_by_key(|&i| residue.get(i).map(|r| r.id.as_str()));
    let archived = input
        .new
        .sources
        .iter()
        .filter(|(_, source)| source.archived == Some(true))
        .map(|(name, source)| (name.clone(), source.repo.clone()))
        .collect();
    let duplicates = [
        DuplicateKind::Exact,
        DuplicateKind::Mirror,
        DuplicateKind::Near,
    ]
    .into_iter()
    .map(|kind| {
        let pairs: Vec<&DuplicatePair> =
            input.duplicates.iter().filter(|p| p.kind == kind).collect();
        (kind, pairs)
    })
    .filter(|(_, pairs)| !pairs.is_empty())
    .collect();
    Prepared {
        input,
        sources: input.new.sources.len(),
        pages: input.new.sources.values().map(|s| s.pages.len()).sum(),
        since: input
            .old
            .map_or("previous", |m| m.generated_at.as_str())
            .to_string(),
        effective,
        residue,
        undecided,
        active_excludes,
        expired,
        diff,
        new_residue,
        excluded,
        unresolved,
        archived,
        duplicates,
    }
}

/// The positions in `residue` of the entries with `reason`, in registry order.
fn positions(residue: &[ResidueEntry], reason: Reason) -> impl Iterator<Item = usize> + '_ {
    residue
        .iter()
        .enumerate()
        .filter(move |(_, r)| r.reason == reason)
        .map(|(i, _)| i)
}

/// Render the report as Markdown: [`prepare`], then [`Prepared::markdown`].
#[must_use]
pub fn render(input: ReportInput<'_>) -> String {
    prepare(input).markdown()
}

impl Prepared<'_> {
    /// The residue entries at `positions`, in that order.
    fn entries<'p>(&'p self, positions: &'p [usize]) -> impl Iterator<Item = &'p ResidueEntry> {
        positions.iter().filter_map(|&i| self.residue.get(i))
    }

    /// The reason "Removed pages" gives for `page` (SPEC §2.7).
    fn removed_reason(&self, page: &RemovedPage) -> RemovedReason {
        if self.active_excludes.contains(page.id.as_str())
            && page.reason != RemovalReason::SourceRemoved
        {
            return RemovedReason::ExcludedByDecision;
        }
        match page.reason {
            RemovalReason::DroppedByResolver => RemovedReason::DroppedByResolver,
            RemovalReason::SourceRemoved => RemovedReason::SourceRemoved,
            RemovalReason::GoneUpstream => RemovedReason::GoneUpstream,
        }
    }

    /// The Markdown report (SPEC §2.7).
    #[must_use]
    pub fn markdown(&self) -> String {
        let mut out = String::from("# Corpus report\n\n");
        summary(&mut out, self);
        eval_section(&mut out, self.input.eval_before, self.input.eval_after);
        pages_section(&mut out, self);
        residue_section(&mut out, self);
        expired_section(&mut out, &self.expired);
        unresolved_section(&mut out, self);
        archived_section(&mut out, &self.archived);
        duplicates_section(&mut out, self);
        if let Some(usage) = self.input.usage {
            // `duplicates_section`'s empty branch has no trailing blank line (it used to be the
            // last section); restore one so "Usage" is not glued to it.
            if !out.ends_with("\n\n") {
                out.push('\n');
            }
            usage_section(&mut out, usage);
        }
        out
    }

    /// The report's facts (SPEC §2.10): the counts and ids the Markdown shows, per section.
    #[must_use]
    pub fn facts(&self) -> ReportFacts {
        let input = self.input;
        let eval_side = |summary: &EvalSummary| EvalSideFacts {
            tuning: summary.tuning.clone(),
            holdout: summary.holdout.clone(),
        };
        let mut unresolved_links: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for entry in self.entries(&self.unresolved) {
            unresolved_links
                .entry(entry.source.clone())
                .or_default()
                .push(entry.id.clone());
        }
        ReportFacts {
            version: FACTS_VERSION,
            summary: self.summary_facts(),
            eval: (input.eval_before.is_some() || input.eval_after.is_some()).then(|| EvalFacts {
                before: input.eval_before.map(eval_side),
                after: input.eval_after.map(eval_side),
            }),
            pages: self.pages_facts(),
            residue: self.residue_facts(),
            expired_decisions: self
                .expired
                .iter()
                .map(|item| ExpiredFacts {
                    id: item.decision.id.clone(),
                    decision: item.decision.decision,
                    why: ExpiredWhy::of(item),
                })
                .collect(),
            unresolved_links,
            archived_sources: self.archived.iter().map(|(name, _)| name.clone()).collect(),
            duplicates: self.duplicates_facts(),
            usage: input.usage.cloned(),
        }
    }

    fn summary_facts(&self) -> SummaryFacts {
        SummaryFacts {
            sources: self.sources,
            pages: self.pages,
            residue: self.residue.len(),
            undecided: self.undecided.len(),
            excluded: self.excluded.iter().map(|g| g.entries.len()).sum(),
            decisions: self.effective.len(),
            changes: self.diff.as_deref().map(|diff| ChangesFacts {
                since: self.since.clone(),
                added: diff.added.len(),
                removed: diff.removed.len(),
                changed: diff.changed.len(),
            }),
        }
    }

    fn pages_facts(&self) -> PagesFacts {
        let Some(diff) = self.diff.as_deref() else {
            return PagesFacts::default();
        };
        PagesFacts {
            added: diff.added.iter().map(|p| p.id.clone()).collect(),
            removed: diff
                .removed
                .iter()
                .map(|p| RemovedPageFacts {
                    id: p.id.clone(),
                    reason: self.removed_reason(p),
                })
                .collect(),
            changed: diff
                .changed
                .iter()
                .map(|p| ChangedPageFacts {
                    id: p.id.clone(),
                    lines_added: p.lines_added,
                    lines_removed: p.lines_removed,
                })
                .collect(),
        }
    }

    fn residue_facts(&self) -> ResidueFacts {
        let group_facts = |groups: &[RuleGroup]| -> Vec<RuleGroupFacts> {
            groups
                .iter()
                .map(|group| RuleGroupFacts {
                    rule: group.key.clone(),
                    text: group.text.clone(),
                    ids: self.entries(&group.entries).map(|e| e.id.clone()).collect(),
                })
                .collect()
        };
        ResidueFacts {
            new: group_facts(&self.new_residue),
            undecided: self
                .entries(&self.undecided)
                .map(|r| r.id.clone())
                .collect(),
            excluded: group_facts(&self.excluded),
        }
    }

    fn duplicates_facts(&self) -> DuplicatesFacts {
        let pairs: Vec<DuplicatePairFacts> = self
            .duplicates
            .iter()
            .flat_map(|(_, pairs)| pairs.iter())
            .map(|pair| DuplicatePairFacts {
                kind: pair.kind,
                canonical: pair.canonical.clone(),
                duplicate: pair.duplicate.clone(),
                similarity: pair.similarity,
                suggested: pair.suggested,
            })
            .collect();
        DuplicatesFacts {
            count: pairs.len(),
            pairs,
        }
    }
}

/// The `report.json` document (SPEC §2.10): every count and id `report.md` shows, per section,
/// plus the id lists and counts a gate needs. Rendered from the same [`Prepared`] as the Markdown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportFacts {
    /// The document's major version; missing means `1`. Additive within a major version.
    #[serde(default = "default_version")]
    pub version: u32,
    /// The "Summary" counts.
    pub summary: SummaryFacts,
    /// The "Eval before/after" numbers; `None` when no eval result was given.
    #[serde(default)]
    pub eval: Option<EvalFacts>,
    /// The "Added pages", "Removed pages" and "Changed pages" sections.
    pub pages: PagesFacts,
    /// The "New residue" section and the undecided residue behind it.
    pub residue: ResidueFacts,
    /// The "Expired decisions" section.
    #[serde(default)]
    pub expired_decisions: Vec<ExpiredFacts>,
    /// The "Unresolved links" section: residue ids by source name, in the Markdown's order.
    #[serde(default)]
    pub unresolved_links: BTreeMap<String, Vec<String>>,
    /// The "Archived sources" section: source names.
    #[serde(default)]
    pub archived_sources: Vec<String>,
    /// The "Duplicates" section (SPEC §11).
    pub duplicates: DuplicatesFacts,
    /// The "Usage" section (SPEC §15.3); `None` when no usage report was given.
    #[serde(default)]
    pub usage: Option<Usage>,
}

fn default_version() -> u32 {
    FACTS_VERSION
}

impl ReportFacts {
    /// Read a `report.json`.
    pub fn load(path: &Path) -> Result<ReportFacts, ReportError> {
        let text = std::fs::read_to_string(path).map_err(io_err(path))?;
        serde_json::from_str(&text).map_err(|source| ReportError::Json {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Write the document as pretty JSON with a trailing newline.
    pub fn save(&self, path: &Path) -> Result<(), ReportError> {
        let text = self.to_json().map_err(|source| ReportError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        std::fs::write(path, text).map_err(io_err(path))
    }

    /// The document as pretty JSON (two-space indent) with a trailing newline.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        Ok(text)
    }
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> ReportError + '_ {
    move |source| ReportError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// The "Summary" counts (SPEC §2.10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryFacts {
    /// Sources in the current manifest.
    pub sources: usize,
    /// Pages in the current manifest.
    pub pages: usize,
    /// Residue entries.
    pub residue: usize,
    /// Residue entries no effective decision covers at their current hash.
    pub undecided: usize,
    /// Residue entries with reason `excluded` (SPEC §2.4).
    pub excluded: usize,
    /// Decisions in effect: the last line per id (SPEC §2.5).
    pub decisions: usize,
    /// The diff's counts; `None` without a previous manifest.
    #[serde(default)]
    pub changes: Option<ChangesFacts>,
}

/// The "Changes since" line of the summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangesFacts {
    /// The previous manifest's `generated_at`.
    pub since: String,
    /// Pages added since then.
    pub added: usize,
    /// Pages removed since then.
    pub removed: usize,
    /// Pages whose hash changed since then.
    pub changed: usize,
}

/// The "Eval before/after" numbers: each side as `eval --json` writes its splits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalFacts {
    /// The previous corpus's result, when given.
    #[serde(default)]
    pub before: Option<EvalSideFacts>,
    /// The current corpus's result, when given.
    #[serde(default)]
    pub after: Option<EvalSideFacts>,
}

/// One side of the eval table: the tuning split and, when there are held-out queries, the
/// held-out split (SPEC §2.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalSideFacts {
    /// Queries with `holdout: false`.
    pub tuning: Split,
    /// Queries with `holdout: true`, when there are any.
    #[serde(default)]
    pub holdout: Option<Split>,
}

/// The page-diff sections; all empty without a previous manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PagesFacts {
    /// Ids of pages in the current manifest only.
    #[serde(default)]
    pub added: Vec<String>,
    /// Pages in the previous manifest only, each with its reason.
    #[serde(default)]
    pub removed: Vec<RemovedPageFacts>,
    /// Pages in both manifests with a different hash, each with its line counts (SPEC §13).
    #[serde(default)]
    pub changed: Vec<ChangedPageFacts>,
}

/// One entry of "Removed pages".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovedPageFacts {
    /// The page id.
    pub id: String,
    /// Why it is gone.
    pub reason: RemovedReason,
}

/// One entry of "Changed pages".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedPageFacts {
    /// The page id.
    pub id: String,
    /// Lines added between the two versions (SPEC §13).
    pub lines_added: usize,
    /// Lines removed between the two versions (SPEC §13).
    pub lines_removed: usize,
}

/// The residue sections.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResidueFacts {
    /// The "New residue" groups, in the Markdown's order.
    #[serde(default)]
    pub new: Vec<RuleGroupFacts>,
    /// Every undecided residue id, in registry order (the order the Markdown lists residue in).
    #[serde(default)]
    pub undecided: Vec<String>,
    /// The groups of the collapsed "excluded by policy or resolver rules" block.
    #[serde(default)]
    pub excluded: Vec<RuleGroupFacts>,
}

/// Residue ids that share one rule (SPEC §2.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleGroupFacts {
    /// The rule's key, e.g. `"policy:deny"`.
    pub rule: String,
    /// The rule's sentence, the group's heading in the Markdown.
    pub text: String,
    /// The ids in the group, in the Markdown's order.
    #[serde(default)]
    pub ids: Vec<String>,
}

/// One entry of "Expired decisions".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpiredFacts {
    /// The decision's page id.
    pub id: String,
    /// The verdict that expired.
    pub decision: Verdict,
    /// Why it expired.
    pub why: ExpiredWhy,
}

/// The "Duplicates" section (SPEC §11).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DuplicatesFacts {
    /// The number of pairs.
    pub count: usize,
    /// Every pair, by kind (exact, mirror, near) then in `duplicates.jsonl` order.
    #[serde(default)]
    pub pairs: Vec<DuplicatePairFacts>,
}

/// One duplicate pair, canonical first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DuplicatePairFacts {
    /// How the two pages are related.
    pub kind: DuplicateKind,
    /// The page the winner rule kept.
    pub canonical: String,
    /// The page the winner rule would drop.
    pub duplicate: String,
    /// Jaccard similarity of the two (`1.0` for an exact duplicate).
    pub similarity: f64,
    /// The default verdict for `decide`.
    pub suggested: Suggested,
}

fn summary(out: &mut String, prepared: &Prepared<'_>) {
    out.push_str("## Summary\n\n");
    let _ = writeln!(out, "- Sources: {}", prepared.sources);
    let _ = writeln!(out, "- Pages: {}", prepared.pages);
    let _ = writeln!(
        out,
        "- Residue: {} entries, {} undecided",
        prepared.residue.len(),
        prepared.undecided.len()
    );
    let _ = writeln!(out, "- Decisions: {}", prepared.effective.len());
    match prepared.diff.as_deref() {
        Some(diff) => {
            let _ = writeln!(
                out,
                "- Changes since {}: {} added, {} removed, {} changed",
                prepared.since,
                diff.added.len(),
                diff.removed.len(),
                diff.changed.len()
            );
        }
        None => out.push_str("- Changes: no previous manifest to compare with\n"),
    }
    out.push('\n');
}

fn eval_section(out: &mut String, before: Option<&EvalSummary>, after: Option<&EvalSummary>) {
    out.push_str("## Eval before/after\n\n");
    if before.is_none() && after.is_none() {
        out.push_str("_No evaluation results supplied._\n\n");
        return;
    }
    out.push_str("### Tuning queries\n\n");
    eval_table(out, before.map(|e| &e.tuning), after.map(|e| &e.tuning));
    let holdout_before = before.and_then(|e| e.holdout.as_ref());
    let holdout_after = after.and_then(|e| e.holdout.as_ref());
    if holdout_before.is_some() || holdout_after.is_some() {
        out.push_str("### Held-out queries\n\n");
        eval_table(out, holdout_before, holdout_after);
    }
}

fn eval_table(out: &mut String, before: Option<&Split>, after: Option<&Split>) {
    out.push_str("| kind | recall@5 | recall@10 | MRR | n |\n|---|---|---|---|---|\n");
    let mut kinds: BTreeSet<&str> = BTreeSet::new();
    for split in [before, after].into_iter().flatten() {
        kinds.extend(split.per_kind.keys().map(String::as_str));
    }
    eval_row(
        out,
        "overall",
        before.map(|s| &s.overall),
        after.map(|s| &s.overall),
    );
    for kind in kinds {
        eval_row(
            out,
            kind,
            before.and_then(|s| s.per_kind.get(kind)),
            after.and_then(|s| s.per_kind.get(kind)),
        );
    }
    out.push('\n');
}

fn eval_row(out: &mut String, label: &str, before: Option<&Metrics>, after: Option<&Metrics>) {
    let cell = |f: fn(&Metrics) -> f64| match (before, after) {
        (Some(b), Some(a)) => format!("{:.3} → {:.3}", f(b), f(a)),
        (Some(b), None) => format!("{:.3} → –", f(b)),
        (None, Some(a)) => format!("– → {:.3}", f(a)),
        (None, None) => "–".to_string(),
    };
    let n = after
        .or(before)
        .map_or_else(|| "–".to_string(), |m| m.n.to_string());
    let _ = writeln!(
        out,
        "| {label} | {} | {} | {} | {n} |",
        cell(|m| m.recall5),
        cell(|m| m.recall10),
        cell(|m| m.mrr)
    );
}

/// Render one page mention as a Markdown link `[title](url)`, or `` `id` `` when there is no
/// title or no URL could be derived (SPEC §2.7).
fn page_mention(title: &str, id: &str, url: &str) -> String {
    if title.is_empty() || url.is_empty() {
        format!("`{id}`")
    } else {
        format!("[{title}]({url})")
    }
}

fn pages_section(out: &mut String, prepared: &Prepared<'_>) {
    let registry = prepared.input.registry;
    let old = prepared.input.old;
    let diff = prepared.diff.as_deref();
    let url_of = |id: &str| {
        registry
            .corpus_page(id)
            .map(|r| r.url.clone())
            .unwrap_or_default()
    };
    out.push_str("## Added pages\n\n");
    match diff {
        Some(diff) if !diff.added.is_empty() => {
            for page in &diff.added {
                let mention = page_mention(&page.title, &page.id, &url_of(&page.id));
                let _ = writeln!(out, "- {mention}");
            }
        }
        _ => out.push_str("_none_\n"),
    }
    out.push_str("\n## Removed pages\n\n");
    match diff {
        Some(diff) if !diff.removed.is_empty() => {
            for page in &diff.removed {
                let reason = prepared.removed_reason(page).describe();
                let url = old.and_then(|m| m.page_url(&page.id)).unwrap_or_default();
                let mention = page_mention(&page.title, &page.id, &url);
                let _ = writeln!(out, "- {mention} ({reason})");
            }
        }
        _ => out.push_str("_none_\n"),
    }
    out.push_str("\n## Changed pages\n\n");
    match diff {
        Some(diff) if !diff.changed.is_empty() => {
            for page in &diff.changed {
                let source = page.id.split("::").next().unwrap_or_default();
                let compare = diff.sources.get(source).and_then(SourceDiff::compare_url);
                let lines = format!("+{}/-{}", page.lines_added, page.lines_removed);
                let mention = page_mention(&page.title, &page.id, &url_of(&page.id));
                match compare {
                    Some(compare_url) => {
                        let _ = writeln!(out, "- {mention} ({lines}) ([compare]({compare_url}))");
                    }
                    None => {
                        let _ = writeln!(out, "- {mention} ({lines})");
                    }
                }
            }
        }
        _ => out.push_str("_none_\n"),
    }
    out.push('\n');
}

/// The `(rule key, rule text)` a residue entry groups under (SPEC §2.4, §2.7); entries written
/// before this tool recorded a rule fall back to a single catch-all group.
fn rule_key_text(entry: &ResidueEntry) -> (String, String) {
    match &entry.rule {
        Some(rule) => (rule.key.clone(), rule.text.clone()),
        None => ("unknown".to_string(), "no rule recorded".to_string()),
    }
}

/// Group the entries of `residue` at `positions` by rule, in the stable order the rule's own
/// sentence sorts to; within a group the positions keep their order.
fn group_by_rule(
    residue: &[ResidueEntry],
    positions: impl Iterator<Item = usize>,
) -> Vec<RuleGroup> {
    let mut by_rule: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (i, entry) in positions.filter_map(|i| residue.get(i).map(|e| (i, e))) {
        let (key, text) = rule_key_text(entry);
        by_rule.entry((text, key)).or_default().push(i);
    }
    by_rule
        .into_iter()
        .map(|((text, key), entries)| RuleGroup { key, text, entries })
        .collect()
}

/// One residue entry's mention, context and excerpt (SPEC §2.7).
fn render_residue_entry(out: &mut String, entry: &ResidueEntry) {
    let mention = page_mention(&entry.title, &entry.id, &entry.url);
    let _ = writeln!(out, "- {mention}");
    if !entry.context.is_empty() {
        let _ = writeln!(out, "  - context: {}", entry.context);
    }
    let excerpt: Vec<&str> = entry
        .excerpt
        .split_whitespace()
        .take(EXCERPT_WORDS)
        .collect();
    if !excerpt.is_empty() {
        let ellipsis = if entry
            .excerpt
            .split_whitespace()
            .nth(EXCERPT_WORDS)
            .is_some()
        {
            " …"
        } else {
            ""
        };
        let _ = writeln!(out, "  > {}{ellipsis}", excerpt.join(" "));
    }
}

/// One rule group: its heading, then each entry.
fn render_rule_group(out: &mut String, prepared: &Prepared<'_>, group: &RuleGroup) {
    let _ = writeln!(out, "### {}\n", group.text);
    for entry in prepared.entries(&group.entries) {
        render_residue_entry(out, entry);
    }
    out.push('\n');
}

/// The "New residue" section (SPEC §2.7): non-excluded entries new since the previous report,
/// grouped by rule (SPEC §2.4), each group headed by the rule's sentence; excluded entries are
/// never "new" in this sense, but are always accounted for in a collapsed, always-present count
/// (SPEC §2.4) regardless of report history.
fn residue_section(out: &mut String, prepared: &Prepared<'_>) {
    out.push_str("## New residue\n\n");
    if prepared.new_residue.is_empty() {
        out.push_str("_none_\n\n");
    } else {
        for group in &prepared.new_residue {
            render_rule_group(out, prepared, group);
        }
    }
    excluded_details(out, prepared);
}

/// A collapsed `<details>` block naming every currently excluded page (SPEC §2.4), grouped by
/// rule; omitted entirely when there are none.
fn excluded_details(out: &mut String, prepared: &Prepared<'_>) {
    let count: usize = prepared.excluded.iter().map(|g| g.entries.len()).sum();
    if count == 0 {
        return;
    }
    let plural = if count == 1 { "" } else { "s" };
    let _ = writeln!(
        out,
        "<details>\n<summary>{count} page{plural} excluded by policy or resolver rules</summary>\n"
    );
    for group in &prepared.excluded {
        render_rule_group(out, prepared, group);
    }
    out.push_str("</details>\n\n");
}

fn unresolved_ids((name, source): (&String, &crate::manifest::ManifestSource)) -> Vec<String> {
    source
        .unresolved
        .iter()
        .map(|p| crate::manifest::page_id(name, p))
        .collect()
}

fn expired_section(out: &mut String, expired: &[Expired]) {
    out.push_str("## Expired decisions\n\n");
    if expired.is_empty() {
        out.push_str("_none_\n\n");
        return;
    }
    for item in expired {
        let d = &item.decision;
        let why = ExpiredWhy::of(item).describe();
        let _ = writeln!(
            out,
            "- `{}` — {} by {} at {} ({why}): {}",
            d.id, d.decision, d.by, d.at, d.reason
        );
    }
    out.push('\n');
}

/// The "Unresolved links" section: dangling navigation links (SPEC §2.4's `unresolved_link`
/// reason), linking the navigation file itself since the target does not exist (SPEC §2.7).
fn unresolved_section(out: &mut String, prepared: &Prepared<'_>) {
    out.push_str("## Unresolved links\n\n");
    if prepared.unresolved.is_empty() {
        out.push_str("_none_\n");
    } else {
        for entry in prepared.entries(&prepared.unresolved) {
            let mention = page_mention(&entry.title, &entry.id, &entry.url);
            let _ = writeln!(out, "- {mention}");
        }
    }
    out.push('\n');
}

fn archived_section(out: &mut String, archived: &[(String, String)]) {
    out.push_str("## Archived sources\n\n");
    if archived.is_empty() {
        out.push_str("_none_\n");
    }
    for (name, repo) in archived {
        let _ = writeln!(out, "- `{name}` ({repo})");
    }
    out.push('\n');
}

/// The "Duplicates" section (SPEC §11): exact, mirror and near-duplicate pairs, grouped by kind
/// in that order, each page mentioned as a Markdown link (SPEC §2.7).
fn duplicates_section(out: &mut String, prepared: &Prepared<'_>) {
    out.push_str("## Duplicates\n\n");
    if prepared.duplicates.is_empty() {
        out.push_str("_none_\n");
        return;
    }
    let registry = prepared.input.registry;
    let title_of = |id: &str| {
        registry
            .corpus_page(id)
            .map(|p| p.title.clone())
            .unwrap_or_default()
    };
    for (kind, pairs) in &prepared.duplicates {
        let _ = writeln!(out, "### {}\n", kind_label(*kind));
        for pair in pairs {
            let canonical = page_mention(
                &title_of(&pair.canonical),
                &pair.canonical,
                &pair.canonical_url,
            );
            let duplicate = page_mention(
                &title_of(&pair.duplicate),
                &pair.duplicate,
                &pair.duplicate_url,
            );
            let _ = writeln!(
                out,
                "- {canonical} ← {duplicate} (similarity {:.3}, suggested: {}) — {}",
                pair.similarity,
                suggested_label(pair.suggested),
                pair.why
            );
        }
        out.push('\n');
    }
}
fn kind_label(kind: DuplicateKind) -> &'static str {
    match kind {
        DuplicateKind::Exact => "exact",
        DuplicateKind::Mirror => "mirror",
        DuplicateKind::Near => "near",
    }
}

fn suggested_label(suggested: Suggested) -> &'static str {
    match suggested {
        Suggested::Exclude => "exclude",
        Suggested::Review => "review",
    }
}

/// The "Usage" section (SPEC §15.3): pages never retrieved, pages retrieved but never cited,
/// and uncited queries grouped by their top retrieved page, each with its best residue gap
/// candidate when the index found one. Rendered only when `report` was given a usage JSON.
fn usage_section(out: &mut String, usage: &Usage) {
    out.push_str("## Usage\n\n");
    out.push_str("### Never retrieved\n\n");
    if usage.never_retrieved.is_empty() {
        out.push_str("_none_\n\n");
    } else {
        for id in &usage.never_retrieved {
            let _ = writeln!(out, "- `{id}`");
        }
        out.push('\n');
    }
    out.push_str("### Retrieved, never cited\n\n");
    if usage.retrieved_never_cited.is_empty() {
        out.push_str("_none_\n\n");
    } else {
        for id in &usage.retrieved_never_cited {
            let _ = writeln!(out, "- `{id}`");
        }
        out.push('\n');
    }
    out.push_str("### Uncited queries\n\n");
    if usage.uncited_queries.is_empty() {
        out.push_str("_none_\n");
        return;
    }
    let mut by_top: BTreeMap<Option<&str>, Vec<&UncitedQuery>> = BTreeMap::new();
    for query in &usage.uncited_queries {
        by_top
            .entry(query.top_retrieved.as_deref())
            .or_default()
            .push(query);
    }
    for (top, queries) in &by_top {
        let heading = top.map_or_else(|| "_no page retrieved_".to_string(), |id| format!("`{id}`"));
        let _ = writeln!(out, "- {heading}");
        for query in queries {
            match &query.best_residue {
                Some(gap) => {
                    let _ = writeln!(
                        out,
                        "  - {:?} — gap candidate: `{}` (score {:.3})",
                        query.query, gap.id, gap.score
                    );
                }
                None => {
                    let _ = writeln!(out, "  - {:?}", query.query);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{ManifestSource, PageEntry, SelectedBy};

    fn page(sha: &str, title: &str) -> PageEntry {
        PageEntry {
            sha256: sha.to_string(),
            title: title.to_string(),
            doc_type: "concept".to_string(),
            section: String::new(),
            selected_by: SelectedBy::Include,
            rendered_from: None,
        }
    }

    fn source(
        commit: &str,
        archived: Option<bool>,
        pages: &[(&str, &str, &str)],
        residue: &[&str],
        unresolved: &[&str],
    ) -> ManifestSource {
        ManifestSource {
            repo: "example-org/handbook".to_string(),
            repo_url: "https://github.com/example-org/handbook.git".to_string(),
            git_ref: "main".to_string(),
            commit: commit.to_string(),
            archived,
            resolver: "glob".to_string(),
            pages: pages
                .iter()
                .map(|(p, s, t)| ((*p).to_string(), page(s, t)))
                .collect(),
            residue: residue.iter().map(|r| (*r).to_string()).collect(),
            unresolved: unresolved.iter().map(|u| (*u).to_string()).collect(),
            unrendered: Vec::new(),
            render: None,
        }
    }

    fn residue(
        id: &str,
        reason: Reason,
        title: &str,
        excerpt: &str,
        context: &str,
    ) -> ResidueEntry {
        let (source, path) = id.split_once("::").unwrap();
        let rule = match reason {
            Reason::UnresolvedLink => crate::residue::Rule::nav_dangling_link("docs/_sidebar.md"),
            Reason::Excluded => crate::residue::Rule::policy_deny("**/CHANGELOG.md"),
            Reason::NotSelected | Reason::NewSource => crate::residue::Rule {
                key: "glob:outside-include".to_string(),
                text: "outside the configured include patterns".to_string(),
            },
        };
        ResidueEntry {
            id: id.to_string(),
            source: source.to_string(),
            path: path.to_string(),
            reason,
            sha256: if reason == Reason::UnresolvedLink {
                String::new()
            } else {
                "r1".to_string()
            },
            title: title.to_string(),
            excerpt: excerpt.to_string(),
            context: context.to_string(),
            url: format!(
                "https://github.com/example-org/handbook/blob/2222222222222222222222222222222222222222/{path}"
            ),
            rule: Some(rule),
        }
    }

    fn decision(id: &str, sha: &str, verdict: Verdict) -> Decision {
        Decision {
            id: id.to_string(),
            sha256: sha.to_string(),
            decision: verdict,
            reason: "reviewed".to_string(),
            by: "alice".to_string(),
            at: "2026-09-15T08:00:00Z".to_string(),
        }
    }

    fn metrics(r5: f64, r10: f64, mrr: f64, n: usize) -> Metrics {
        Metrics {
            recall5: r5,
            recall10: r10,
            mrr,
            n,
        }
    }

    fn manifests() -> (Manifest, Manifest, Vec<ResidueEntry>, Vec<Decision>) {
        let mut old = Manifest::new("2026-09-01T00:00:00Z".to_string());
        old.sources.insert(
            "handbook".to_string(),
            source(
                "1111111111111111111111111111111111111111",
                Some(false),
                &[
                    ("docs/getting-started.md", "a1", "Getting Started"),
                    ("docs/old.md", "b1", "Old Page"),
                    ("docs/changed.md", "c1", "Changed Page"),
                    ("docs/dropped.md", "d1", "Dropped Page"),
                ],
                &["docs/known-residue.md"],
                &[],
            ),
        );
        old.sources.insert(
            "removed-src".to_string(),
            source(
                "9999999999999999999999999999999999999999",
                None,
                &[("docs/x.md", "x1", "X")],
                &[],
                &[],
            ),
        );
        let mut new = Manifest::new("2026-09-16T12:00:00Z".to_string());
        new.sources.insert(
            "handbook".to_string(),
            source(
                "2222222222222222222222222222222222222222",
                Some(true),
                &[
                    ("docs/getting-started.md", "a1", "Getting Started"),
                    ("docs/changed.md", "c2", "Changed Page"),
                    ("docs/new.md", "e1", "New Page"),
                ],
                &[
                    "docs/known-residue.md",
                    "docs/dropped.md",
                    "docs/fresh.md",
                    "docs/decided.md",
                ],
                &["docs/ghost.md"],
            ),
        );
        let residue = vec![
            residue(
                "handbook::docs/known-residue.md",
                Reason::NotSelected,
                "Known",
                "seen before",
                "",
            ),
            residue(
                "handbook::docs/dropped.md",
                Reason::NotSelected,
                "Dropped Page",
                "was a page, now residue",
                "Sidebar > Old",
            ),
            residue(
                "handbook::docs/fresh.md",
                Reason::NotSelected,
                "",
                &"word ".repeat(70),
                "",
            ),
            residue(
                "handbook::docs/decided.md",
                Reason::NotSelected,
                "Decided",
                "hidden by decision",
                "",
            ),
            residue(
                "handbook::docs/ghost.md",
                Reason::UnresolvedLink,
                "Ghost",
                "",
                "Sidebar > Ghost",
            ),
        ];
        let decisions = vec![
            decision("handbook::docs/decided.md", "r1", Verdict::Exclude),
            decision("handbook::docs/dropped.md", "r1", Verdict::Exclude),
            decision("handbook::docs/old.md", "b1", Verdict::Include),
            decision("handbook::docs/changed.md", "c1", Verdict::Unsure),
        ];
        (old, new, residue, decisions)
    }

    fn evals() -> (EvalSummary, EvalSummary) {
        let before = EvalSummary {
            tuning: Split {
                overall: metrics(0.80, 0.85, 0.66, 40),
                per_kind: [("howto".to_string(), metrics(0.9, 0.95, 0.8, 10))].into(),
            },
            holdout: Some(Split {
                overall: metrics(0.5, 0.5, 0.4, 4),
                per_kind: BTreeMap::new(),
            }),
            queries: vec![],
            backend: String::new(),
        };
        let after = EvalSummary {
            tuning: Split {
                overall: metrics(0.85, 0.9, 0.7, 40),
                per_kind: [
                    ("howto".to_string(), metrics(0.9, 1.0, 0.85, 10)),
                    ("concept".to_string(), metrics(0.7, 0.7, 0.5, 5)),
                ]
                .into(),
            },
            holdout: Some(Split {
                overall: metrics(0.75, 0.75, 0.6, 4),
                per_kind: BTreeMap::new(),
            }),
            queries: vec![],
            backend: String::new(),
        };
        (before, after)
    }

    /// Write `rendered` over the snapshot when `UPDATE_SNAPSHOTS` is set, then return the
    /// expected text so a refreshed snapshot passes in the same run.
    fn snapshot(name: &str, rendered: &str, expected: &'static str) -> String {
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
            let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/snapshots/");
            std::fs::write(format!("{path}{name}"), rendered).unwrap();
            return rendered.to_string();
        }
        expected.to_string()
    }

    fn sample_duplicates() -> Vec<DuplicatePair> {
        vec![
            DuplicatePair {
                kind: DuplicateKind::Near,
                similarity: 0.93,
                canonical: "handbook::docs/getting-started.md".to_string(),
                duplicate: "removed-src::docs/x.md".to_string(),
                why: "priority 10 > 1".to_string(),
                suggested: Suggested::Exclude,
                canonical_url: "https://github.com/example-org/handbook/blob/2222222222222222222222222222222222222222/docs/getting-started.md".to_string(),
                duplicate_url: "https://github.com/example-org/handbook/blob/9999999999999999999999999999999999999999/docs/x.md".to_string(),
            },
            DuplicatePair {
                kind: DuplicateKind::Mirror,
                similarity: 0.71,
                canonical: "handbook::docs/new.md".to_string(),
                duplicate: "handbook::docs/getting-started.md".to_string(),
                why: "priority and selected_by tie; handbook::docs/new.md sorts first"
                    .to_string(),
                suggested: Suggested::Review,
                canonical_url: "https://github.com/example-org/handbook/blob/2222222222222222222222222222222222222222/docs/new.md".to_string(),
                duplicate_url: "https://github.com/example-org/handbook/blob/2222222222222222222222222222222222222222/docs/getting-started.md".to_string(),
            },
        ]
    }

    /// The full fixture: an old manifest, an annotated diff, both eval sides and duplicates.
    struct FullFixture {
        old: Manifest,
        new: Manifest,
        diff: Diff,
        registry: PageRegistry,
        decisions: Vec<Decision>,
        before: EvalSummary,
        after: EvalSummary,
        duplicates: Vec<DuplicatePair>,
    }

    impl FullFixture {
        fn new() -> FullFixture {
            let (old, new, residue, decisions) = manifests();
            let (before, after) = evals();
            let diff = annotated_diff(&old, &new);
            let registry = PageRegistry::load(Some(&new), &residue);
            FullFixture {
                old,
                new,
                diff,
                registry,
                decisions,
                before,
                after,
                duplicates: sample_duplicates(),
            }
        }

        fn input(&self) -> ReportInput<'_> {
            ReportInput {
                old: Some(&self.old),
                new: &self.new,
                diff: Some(&self.diff),
                registry: &self.registry,
                decisions: &self.decisions,
                eval_before: Some(&self.before),
                eval_after: Some(&self.after),
                duplicates: &self.duplicates,
                usage: None,
            }
        }
    }

    fn annotated_diff(old: &Manifest, new: &Manifest) -> Diff {
        let mut diff = Diff::compute(old, new);
        crate::diff::annotate_line_counts(&mut diff, |id| {
            if id == "handbook::docs/changed.md" {
                (
                    Some("intro\nold line\n".to_string()),
                    Some("intro\nnew line\nextra line\n".to_string()),
                )
            } else {
                (None, None)
            }
        });
        diff
    }

    #[test]
    fn full_report_matches_snapshot() {
        let (old, new, residue, decisions) = manifests();
        let (before, after) = evals();
        let mut diff = Diff::compute(&old, &new);
        crate::diff::annotate_line_counts(&mut diff, |id| {
            if id == "handbook::docs/changed.md" {
                (
                    Some("intro\nold line\n".to_string()),
                    Some("intro\nnew line\nextra line\n".to_string()),
                )
            } else {
                (None, None)
            }
        });
        let duplicates = sample_duplicates();
        let registry = PageRegistry::load(Some(&new), &residue);
        let rendered = render(ReportInput {
            old: Some(&old),
            new: &new,
            diff: Some(&diff),
            registry: &registry,
            decisions: &decisions,
            eval_before: Some(&before),
            eval_after: Some(&after),
            duplicates: &duplicates,
            usage: None,
        });
        let expected = include_str!("../tests/snapshots/report_full.md");
        let expected = snapshot("report_full.md", &rendered, expected);
        assert_eq!(rendered, expected, "rendered report:\n{rendered}");
    }

    #[test]
    fn minimal_report_without_old_manifest_or_eval() {
        let (_, new, residue, decisions) = manifests();
        let registry = PageRegistry::load(Some(&new), &residue);
        let rendered = render(ReportInput {
            old: None,
            new: &new,
            diff: None,
            registry: &registry,
            decisions: &decisions,
            eval_before: None,
            eval_after: None,
            duplicates: &[],
            usage: None,
        });
        let expected = include_str!("../tests/snapshots/report_minimal.md");
        let expected = snapshot("report_minimal.md", &rendered, expected);
        assert_eq!(rendered, expected, "rendered report:\n{rendered}");
    }

    #[test]
    fn eval_with_only_after_side() {
        let (_, new, _, _) = manifests();
        let (_, after) = evals();
        let registry = PageRegistry::load(Some(&new), &[]);
        let rendered = render(ReportInput {
            old: None,
            new: &new,
            diff: None,
            registry: &registry,
            decisions: &[],
            eval_before: None,
            eval_after: Some(&after),
            duplicates: &[],
            usage: None,
        });
        assert!(rendered.contains("| overall | – → 0.850 | – → 0.900 | – → 0.700 | 40 |"));
        assert!(rendered.contains("### Held-out queries"));
    }

    #[test]
    fn excluded_residue_is_a_collapsed_always_present_count_grouped_by_rule() {
        let (_, new, mut entries, decisions) = manifests();
        entries.push(residue(
            "handbook::CHANGELOG.md",
            Reason::Excluded,
            "Changelog",
            "release notes",
            "",
        ));
        entries.push(residue(
            "handbook::docs/_sidebar.md",
            Reason::Excluded,
            "Sidebar",
            "nav",
            "",
        ));
        let registry = PageRegistry::load(Some(&new), &entries);
        let rendered = render(ReportInput {
            old: None,
            new: &new,
            diff: None,
            registry: &registry,
            decisions: &decisions,
            eval_before: None,
            eval_after: None,
            duplicates: &[],
            usage: None,
        });
        let (before_details, details) = rendered.split_once("<details>").unwrap();
        assert!(
            !before_details.contains("Changelog") && !before_details.contains("[Sidebar]"),
            "excluded residue never renders as ordinary \"New residue\", only in the details \
             block: {before_details}"
        );
        assert!(
            details
                .starts_with("\n<summary>2 pages excluded by policy or resolver rules</summary>\n")
        );
        let (block, after) = details.split_once("</details>").unwrap();
        assert!(block.contains("### matches `policy.deny` (`**/CHANGELOG.md`)"));
        assert!(block.contains("[Changelog]"));
        assert!(block.contains("[Sidebar]"));
        assert!(after.trim_start().starts_with("## Expired decisions"));

        // Omitting excluded residue entirely omits the block, unlike the always-rendered
        // "New residue" heading itself.
        let (_, new, entries, decisions) = manifests();
        let registry = PageRegistry::load(Some(&new), &entries);
        let without_excluded = render(ReportInput {
            old: None,
            new: &new,
            diff: None,
            registry: &registry,
            decisions: &decisions,
            eval_before: None,
            eval_after: None,
            duplicates: &[],
            usage: None,
        });
        assert!(!without_excluded.contains("<details>"));
    }

    #[test]
    fn duplicates_section_groups_pairs_by_kind() {
        let (_, new, _, _) = manifests();
        let duplicates = sample_duplicates();
        let registry = PageRegistry::load(Some(&new), &[]);
        let rendered = render(ReportInput {
            old: None,
            new: &new,
            diff: None,
            registry: &registry,
            decisions: &[],
            eval_before: None,
            eval_after: None,
            duplicates: &duplicates,
            usage: None,
        });
        // Kind order is exact, mirror, near, regardless of the input order.
        assert!(rendered.contains("## Duplicates\n\n### mirror\n\n"));
        assert!(rendered.contains("\n\n### near\n\n"));
        assert!(rendered.contains(
            "- [Getting Started](https://github.com/example-org/handbook/blob/2222222222222222222222222222222222222222/docs/getting-started.md) \
             ← `removed-src::docs/x.md` (similarity 0.930, suggested: exclude) — priority 10 > 1\n"
        ), "{rendered}");
        assert!(rendered.contains("suggested: review"));

        let rendered_empty = render(ReportInput {
            old: None,
            new: &new,
            diff: None,
            registry: &registry,
            decisions: &[],
            eval_before: None,
            eval_after: None,
            duplicates: &[],
            usage: None,
        });
        assert!(rendered_empty.ends_with("## Duplicates\n\n_none_\n"));
    }

    #[test]
    fn duplicates_section_titles_come_from_the_corpus_record_only_never_residue() {
        let (_, new, _, _) = manifests();
        // A residue record at this id, titled differently from any corpus page.
        let entries = vec![residue(
            "handbook::docs/known-residue.md",
            Reason::NotSelected,
            "Residue Title",
            "text",
            "",
        )];
        let registry = PageRegistry::load(Some(&new), &entries);
        let duplicates = vec![DuplicatePair {
            kind: DuplicateKind::Mirror,
            similarity: 1.0,
            canonical: "handbook::docs/getting-started.md".to_string(),
            duplicate: "handbook::docs/known-residue.md".to_string(),
            why: "priority".to_string(),
            suggested: Suggested::Exclude,
            canonical_url: String::new(),
            duplicate_url: String::new(),
        }];
        let rendered = render(ReportInput {
            old: None,
            new: &new,
            diff: None,
            registry: &registry,
            decisions: &[],
            eval_before: None,
            eval_after: None,
            duplicates: &duplicates,
            usage: None,
        });
        // `known-residue.md` is not a corpus page (only residue), so in the Duplicates section
        // its mention falls back to the bare id, never the residue entry's title (which does
        // appear, correctly, over in "New residue").
        let duplicates_section = rendered.split("## Duplicates").nth(1).unwrap();
        assert!(!duplicates_section.contains("Residue Title"), "{rendered}");
        assert!(
            duplicates_section.contains("`handbook::docs/known-residue.md`"),
            "{rendered}"
        );
    }

    fn sample_usage() -> Usage {
        Usage {
            never_retrieved: vec!["handbook::docs/uninstall.md".to_string()],
            retrieved_never_cited: vec!["handbook::docs/getting-started.md".to_string()],
            uncited_queries: vec![
                UncitedQuery {
                    query: "how do I write a plugin".to_string(),
                    top_retrieved: Some("cookbook::docs/README.md".to_string()),
                    best_residue: Some(crate::usage::GapCandidate {
                        id: "cookbook::docs/recipes/draft-plugin.md".to_string(),
                        score: 1.42,
                    }),
                },
                UncitedQuery {
                    query: "totally unmatched question".to_string(),
                    top_retrieved: None,
                    best_residue: None,
                },
            ],
        }
    }

    #[test]
    fn usage_section_is_rendered_only_when_given_and_matches_snapshot() {
        let (_, new, residue, decisions) = manifests();
        let usage = sample_usage();
        let registry = PageRegistry::load(Some(&new), &residue);
        let rendered = render(ReportInput {
            old: None,
            new: &new,
            diff: None,
            registry: &registry,
            decisions: &decisions,
            eval_before: None,
            eval_after: None,
            duplicates: &[],
            usage: Some(&usage),
        });
        let expected = include_str!("../tests/snapshots/report_usage.md");
        let expected = snapshot("report_usage.md", &rendered, expected);
        assert_eq!(rendered, expected, "rendered report:\n{rendered}");

        // Omitting `usage` omits the section entirely (existing reports are unaffected).
        let without_usage = render(ReportInput {
            old: None,
            new: &new,
            diff: None,
            registry: &registry,
            decisions: &decisions,
            eval_before: None,
            eval_after: None,
            duplicates: &[],
            usage: None,
        });
        assert!(!without_usage.contains("## Usage"));
    }

    /// Serialise `facts`, pin it against `tests/snapshots/<name>` the way the Markdown snapshots
    /// are pinned, and check the document reads back equal.
    fn assert_facts_snapshot(name: &str, facts: &ReportFacts, expected: &'static str) {
        let rendered = facts.to_json().unwrap();
        let expected = snapshot(name, &rendered, expected);
        assert_eq!(rendered, expected, "rendered facts:\n{rendered}");
        let parsed: ReportFacts = serde_json::from_str(&rendered).unwrap();
        assert_eq!(&parsed, facts);
    }

    #[test]
    fn full_report_facts_match_snapshot() {
        let fixture = FullFixture::new();
        let prepared = prepare(fixture.input());
        // The Markdown from the same `Prepared` is the pinned one: the two share their facts.
        assert_eq!(
            prepared.markdown(),
            include_str!("../tests/snapshots/report_full.md")
        );
        let facts = prepared.facts();
        assert_facts_snapshot(
            "report_full.json",
            &facts,
            include_str!("../tests/snapshots/report_full.json"),
        );
        // Spot checks against what the Markdown shows.
        assert_eq!(facts.summary.residue, 5);
        assert_eq!(facts.summary.undecided, 3);
        assert_eq!(facts.summary.decisions, 4);
        let changes = facts.summary.changes.as_ref().unwrap();
        assert_eq!((changes.added, changes.removed, changes.changed), (1, 3, 1));
        assert_eq!(
            facts
                .pages
                .removed
                .iter()
                .map(|p| (p.id.as_str(), p.reason))
                .collect::<Vec<_>>(),
            vec![
                (
                    "handbook::docs/dropped.md",
                    RemovedReason::ExcludedByDecision
                ),
                ("handbook::docs/old.md", RemovedReason::GoneUpstream),
                ("removed-src::docs/x.md", RemovedReason::SourceRemoved),
            ]
        );
        assert_eq!(facts.pages.changed[0].lines_added, 2);
        assert_eq!(facts.pages.changed[0].lines_removed, 1);
        assert_eq!(facts.residue.new.len(), 2);
        assert_eq!(facts.residue.new[1].ids, vec!["handbook::docs/fresh.md"]);
        // Registry order, as the Markdown lists residue: not sorted by id.
        assert_eq!(
            facts.residue.undecided,
            vec![
                "handbook::docs/known-residue.md",
                "handbook::docs/fresh.md",
                "handbook::docs/ghost.md",
            ]
        );
        assert_eq!(facts.expired_decisions.len(), 2);
        assert_eq!(facts.expired_decisions[0].why, ExpiredWhy::PageChanged);
        assert_eq!(facts.expired_decisions[1].why, ExpiredWhy::PageGone);
        assert_eq!(
            facts.unresolved_links["handbook"],
            vec!["handbook::docs/ghost.md"]
        );
        assert_eq!(facts.archived_sources, vec!["handbook"]);
        assert_eq!(facts.duplicates.count, 2);
        assert_eq!(facts.duplicates.pairs[0].kind, DuplicateKind::Mirror);
        assert!(facts.usage.is_none());
        assert_eq!(
            facts.eval.as_ref().unwrap().after.as_ref().unwrap().tuning,
            fixture.after.tuning
        );
    }

    #[test]
    fn minimal_report_facts_match_snapshot() {
        let (_, new, residue, decisions) = manifests();
        let registry = PageRegistry::load(Some(&new), &residue);
        let prepared = prepare(ReportInput {
            old: None,
            new: &new,
            diff: None,
            registry: &registry,
            decisions: &decisions,
            eval_before: None,
            eval_after: None,
            duplicates: &[],
            usage: None,
        });
        assert_eq!(
            prepared.markdown(),
            include_str!("../tests/snapshots/report_minimal.md")
        );
        let facts = prepared.facts();
        assert_facts_snapshot(
            "report_minimal.json",
            &facts,
            include_str!("../tests/snapshots/report_minimal.json"),
        );
        assert!(facts.summary.changes.is_none());
        assert!(facts.eval.is_none());
        assert_eq!(facts.pages, PagesFacts::default());
        assert_eq!(facts.duplicates.count, 0);
    }

    #[test]
    fn usage_report_facts_match_snapshot() {
        let (_, new, residue, decisions) = manifests();
        let usage = sample_usage();
        let registry = PageRegistry::load(Some(&new), &residue);
        let prepared = prepare(ReportInput {
            old: None,
            new: &new,
            diff: None,
            registry: &registry,
            decisions: &decisions,
            eval_before: None,
            eval_after: None,
            duplicates: &[],
            usage: Some(&usage),
        });
        assert_eq!(
            prepared.markdown(),
            include_str!("../tests/snapshots/report_usage.md")
        );
        let facts = prepared.facts();
        assert_facts_snapshot(
            "report_usage.json",
            &facts,
            include_str!("../tests/snapshots/report_usage.json"),
        );
        assert_eq!(facts.usage.as_ref(), Some(&usage));
    }

    #[test]
    fn excluded_residue_is_counted_and_grouped_in_the_facts() {
        let (_, new, mut entries, decisions) = manifests();
        entries.push(residue(
            "handbook::CHANGELOG.md",
            Reason::Excluded,
            "Changelog",
            "release notes",
            "",
        ));
        let registry = PageRegistry::load(Some(&new), &entries);
        let facts = prepare(ReportInput {
            old: None,
            new: &new,
            diff: None,
            registry: &registry,
            decisions: &decisions,
            eval_before: None,
            eval_after: None,
            duplicates: &[],
            usage: None,
        })
        .facts();
        assert_eq!(facts.summary.excluded, 1);
        assert_eq!(facts.residue.excluded.len(), 1);
        assert_eq!(facts.residue.excluded[0].rule, "policy:deny");
        assert_eq!(
            facts.residue.excluded[0].ids,
            vec!["handbook::CHANGELOG.md"]
        );
        // Excluded entries are never "new residue".
        assert!(
            facts
                .residue
                .new
                .iter()
                .all(|g| !g.ids.iter().any(|id| id == "handbook::CHANGELOG.md"))
        );
    }

    #[test]
    fn facts_round_trip_through_serde_and_default_the_version() {
        let fixture = FullFixture::new();
        let facts = prepare(fixture.input()).facts();
        let json = facts.to_json().unwrap();
        assert!(json.ends_with("}\n"));
        let back: ReportFacts = serde_json::from_str(&json).unwrap();
        assert_eq!(back, facts);

        // `version` defaults to 1 when missing (SPEC §2.10), and `load` reads what `save` wrote.
        let without_version = json.replacen("  \"version\": 1,\n", "", 1);
        assert_ne!(without_version, json);
        let back: ReportFacts = serde_json::from_str(&without_version).unwrap();
        assert_eq!(back.version, FACTS_VERSION);
        assert_eq!(back, facts);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        facts.save(&path).unwrap();
        assert_eq!(ReportFacts::load(&path).unwrap(), facts);
        assert!(matches!(
            ReportFacts::load(&dir.path().join("missing.json")).unwrap_err(),
            ReportError::Io { .. }
        ));
        std::fs::write(&path, "{").unwrap();
        assert!(matches!(
            ReportFacts::load(&path).unwrap_err(),
            ReportError::Json { .. }
        ));
    }
}
