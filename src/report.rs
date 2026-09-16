//! `report.md`: the rendered PR body (SPEC §2.7).
//!
//! Sections, in order: summary counts; eval before/after (overall and per kind, held-out
//! separately); added pages; removed pages (with reason); changed pages (with line counts and
//! an upstream compare link, SPEC §13); new residue grouped by reason with excerpt; expired
//! decisions; unresolved links; archived sources; duplicates (SPEC §11); usage (SPEC §15.3),
//! only when a usage report is given.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::decisions::{self, Decision, Expired, Verdict};
use crate::diff::{Diff, RemovalReason, SourceDiff};
use crate::duplicates::{DuplicateKind, DuplicatePair, Suggested};
use crate::eval::{EvalSummary, Metrics, Split};
use crate::manifest::Manifest;
use crate::residue::{Reason, ResidueEntry};
use crate::usage::{UncitedQuery, Usage};

/// Words of excerpt shown per residue entry.
const EXCERPT_WORDS: usize = 60;

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
    /// Current residue entries.
    pub residue: &'a [ResidueEntry],
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

/// Render the report as Markdown.
pub fn render(input: ReportInput<'_>) -> String {
    let effective = decisions::effective(input.decisions);
    let expired = decisions::expired(&effective, |id| {
        input.new.page(id).map(|p| p.sha256.clone()).or_else(|| {
            input
                .residue
                .iter()
                .find(|r| r.id == id)
                .map(|r| r.sha256.clone())
        })
    });
    let computed;
    let diff = match input.diff {
        Some(diff) => Some(diff),
        None => match input.old {
            Some(old) => {
                computed = Diff::compute(old, input.new);
                Some(&computed)
            }
            None => None,
        },
    };
    let undecided: Vec<&ResidueEntry> = input
        .residue
        .iter()
        .filter(|r| {
            !effective
                .get(&r.id)
                .is_some_and(|d| d.applies_to(&r.sha256))
        })
        .collect();
    let active_excludes: BTreeSet<&str> = input
        .residue
        .iter()
        .filter(|r| {
            effective
                .get(&r.id)
                .is_some_and(|d| d.decision == Verdict::Exclude && d.applies_to(&r.sha256))
        })
        .map(|r| r.id.as_str())
        .collect();

    let mut out = String::from("# Corpus report\n\n");
    summary(&mut out, input, diff, &undecided, &effective);
    eval_section(&mut out, input.eval_before, input.eval_after);
    pages_section(&mut out, diff, &active_excludes, input.old, input.new);
    residue_section(&mut out, input, &undecided);
    expired_section(&mut out, &expired);
    unresolved_section(&mut out, input.residue);
    archived_section(&mut out, input.new);
    duplicates_section(&mut out, input.duplicates, input.new);
    if let Some(usage) = input.usage {
        // `duplicates_section`'s empty branch has no trailing blank line (it used to be the
        // last section); restore one so "Usage" is not glued to it.
        if !out.ends_with("\n\n") {
            out.push('\n');
        }
        usage_section(&mut out, usage);
    }
    out
}

fn summary(
    out: &mut String,
    input: ReportInput<'_>,
    diff: Option<&Diff>,
    undecided: &[&ResidueEntry],
    effective: &BTreeMap<String, Decision>,
) {
    let pages: usize = input.new.sources.values().map(|s| s.pages.len()).sum();
    out.push_str("## Summary\n\n");
    let _ = writeln!(out, "- Sources: {}", input.new.sources.len());
    let _ = writeln!(out, "- Pages: {pages}");
    let _ = writeln!(
        out,
        "- Residue: {} entries, {} undecided",
        input.residue.len(),
        undecided.len()
    );
    let _ = writeln!(out, "- Decisions: {}", effective.len());
    match diff {
        Some(diff) => {
            let _ = writeln!(
                out,
                "- Changes since {}: {} added, {} removed, {} changed",
                input.old.map_or("previous", |m| m.generated_at.as_str()),
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

fn pages_section(
    out: &mut String,
    diff: Option<&Diff>,
    active_excludes: &BTreeSet<&str>,
    old: Option<&Manifest>,
    new: &Manifest,
) {
    out.push_str("## Added pages\n\n");
    match diff {
        Some(diff) if !diff.added.is_empty() => {
            for page in &diff.added {
                let url = new.page_url(&page.id).unwrap_or_default();
                let mention = page_mention(&page.title, &page.id, &url);
                let _ = writeln!(out, "- {mention}");
            }
        }
        _ => out.push_str("_none_\n"),
    }
    out.push_str("\n## Removed pages\n\n");
    match diff {
        Some(diff) if !diff.removed.is_empty() => {
            for page in &diff.removed {
                let excluded = active_excludes.contains(page.id.as_str());
                let reason = if excluded && page.reason != RemovalReason::SourceRemoved {
                    "excluded by decision"
                } else {
                    page.reason.describe()
                };
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
                let url = new.page_url(&page.id).unwrap_or_default();
                let mention = page_mention(&page.title, &page.id, &url);
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

/// Group `entries` by rule, in the stable order the rule's own sentence sorts to.
fn group_by_rule<'a>(
    entries: impl Iterator<Item = &'a ResidueEntry>,
) -> BTreeMap<(String, String), Vec<&'a ResidueEntry>> {
    let mut by_rule: BTreeMap<(String, String), Vec<&ResidueEntry>> = BTreeMap::new();
    for entry in entries {
        let (key, text) = rule_key_text(entry);
        by_rule.entry((text, key)).or_default().push(entry);
    }
    by_rule
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

/// The "New residue" section (SPEC §2.7): non-excluded entries new since the previous report,
/// grouped by rule (SPEC §2.4), each group headed by the rule's sentence; excluded entries are
/// never "new" in this sense, but are always accounted for in a collapsed, always-present count
/// (SPEC §2.4) regardless of report history.
fn residue_section(out: &mut String, input: ReportInput<'_>, undecided: &[&ResidueEntry]) {
    out.push_str("## New residue\n\n");
    let previous: BTreeSet<String> = input
        .old
        .map(|old| {
            old.residue_ids()
                .chain(old.sources.iter().flat_map(unresolved_ids))
                .collect()
        })
        .unwrap_or_default();
    let by_rule = group_by_rule(
        undecided
            .iter()
            .copied()
            .filter(|r| !previous.contains(&r.id) && r.reason != Reason::Excluded),
    );
    if by_rule.is_empty() {
        out.push_str("_none_\n\n");
    } else {
        for ((text, _key), entries) in by_rule {
            let _ = writeln!(out, "### {text}\n");
            for entry in entries {
                render_residue_entry(out, entry);
            }
            out.push('\n');
        }
    }
    excluded_details(out, input.residue);
}

/// A collapsed `<details>` block naming every currently excluded page (SPEC §2.4), grouped by
/// rule; omitted entirely when there are none.
fn excluded_details(out: &mut String, residue: &[ResidueEntry]) {
    let excluded: Vec<&ResidueEntry> = residue
        .iter()
        .filter(|r| r.reason == Reason::Excluded)
        .collect();
    if excluded.is_empty() {
        return;
    }
    let plural = if excluded.len() == 1 { "" } else { "s" };
    let _ = writeln!(
        out,
        "<details>\n<summary>{} page{plural} excluded by policy or resolver rules</summary>\n",
        excluded.len()
    );
    for ((text, _key), entries) in group_by_rule(excluded.into_iter()) {
        let _ = writeln!(out, "### {text}\n");
        for entry in entries {
            render_residue_entry(out, entry);
        }
        out.push('\n');
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
        let why = if item.current_sha256.is_some() {
            "page changed"
        } else {
            "page gone"
        };
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
fn unresolved_section(out: &mut String, residue: &[ResidueEntry]) {
    out.push_str("## Unresolved links\n\n");
    let mut entries: Vec<&ResidueEntry> = residue
        .iter()
        .filter(|r| r.reason == Reason::UnresolvedLink)
        .collect();
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    if entries.is_empty() {
        out.push_str("_none_\n");
    } else {
        for entry in entries {
            let mention = page_mention(&entry.title, &entry.id, &entry.url);
            let _ = writeln!(out, "- {mention}");
        }
    }
    out.push('\n');
}

fn archived_section(out: &mut String, manifest: &Manifest) {
    out.push_str("## Archived sources\n\n");
    let mut any = false;
    for (name, source) in &manifest.sources {
        if source.archived == Some(true) {
            let _ = writeln!(out, "- `{name}` ({})", source.repo);
            any = true;
        }
    }
    if !any {
        out.push_str("_none_\n");
    }
    out.push('\n');
}

/// The "Duplicates" section (SPEC §11): exact, mirror and near-duplicate pairs, grouped by kind
/// in that order, each page mentioned as a Markdown link (SPEC §2.7).
fn duplicates_section(out: &mut String, duplicates: &[DuplicatePair], new: &Manifest) {
    out.push_str("## Duplicates\n\n");
    if duplicates.is_empty() {
        out.push_str("_none_\n");
        return;
    }
    let title_of = |id: &str| new.page(id).map(|p| p.title.clone()).unwrap_or_default();
    for kind in [
        DuplicateKind::Exact,
        DuplicateKind::Mirror,
        DuplicateKind::Near,
    ] {
        let pairs: Vec<&DuplicatePair> = duplicates.iter().filter(|p| p.kind == kind).collect();
        if pairs.is_empty() {
            continue;
        }
        let _ = writeln!(out, "### {}\n", kind_label(kind));
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
            Reason::NotSelected | Reason::NewSource => crate::residue::Rule::glob_outside_include(),
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
                why: "same priority, selected_by and commit date".to_string(),
                suggested: Suggested::Review,
                canonical_url: "https://github.com/example-org/handbook/blob/2222222222222222222222222222222222222222/docs/new.md".to_string(),
                duplicate_url: "https://github.com/example-org/handbook/blob/2222222222222222222222222222222222222222/docs/getting-started.md".to_string(),
            },
        ]
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
        let rendered = render(ReportInput {
            old: Some(&old),
            new: &new,
            diff: Some(&diff),
            residue: &residue,
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
        let rendered = render(ReportInput {
            old: None,
            new: &new,
            diff: None,
            residue: &residue,
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
        let rendered = render(ReportInput {
            old: None,
            new: &new,
            diff: None,
            residue: &[],
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
        let rendered = render(ReportInput {
            old: None,
            new: &new,
            diff: None,
            residue: &entries,
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
        let without_excluded = render(ReportInput {
            old: None,
            new: &new,
            diff: None,
            residue: &entries,
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
        let rendered = render(ReportInput {
            old: None,
            new: &new,
            diff: None,
            residue: &[],
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
            residue: &[],
            decisions: &[],
            eval_before: None,
            eval_after: None,
            duplicates: &[],
            usage: None,
        });
        assert!(rendered_empty.ends_with("## Duplicates\n\n_none_\n"));
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
        let rendered = render(ReportInput {
            old: None,
            new: &new,
            diff: None,
            residue: &residue,
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
            residue: &residue,
            decisions: &decisions,
            eval_before: None,
            eval_after: None,
            duplicates: &[],
            usage: None,
        });
        assert!(!without_usage.contains("## Usage"));
    }
}
