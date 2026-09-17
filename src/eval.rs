//! Retrieval evaluation against `queries.jsonl` (SPEC §6).
//!
//! [`load_queries`] reads the judge, [`evaluate`] runs every query through an [`Index`] and
//! [`summarise`] folds the per-query rows into recall@5, recall@10 (or @k when `k < 10`), MRR
//! and n, overall and per `kind`, for tuning rows and `holdout: true` rows separately.
//! [`gate`] compares the tuning recall@5 with a baseline and [`delta`] compares two results for
//! `--with` / `--without`. The result types are what `report` renders (SPEC §2.7).
//!
//! `expected` entries come in two forms. `<source>::<path>` names a page and `<source>::<dir>/`
//! any page under a directory. The legacy `<source>/<path>` form (no `::`) matches the page or
//! any page under that directory prefix, on a path-segment boundary, so `handbook/docs/user` does
//! not match `handbook/docs/super-user/x.md`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::index::{Index, IndexError};
use crate::jsonl::{self, JsonlError};

/// Errors raised while reading queries or evaluation results.
#[derive(Debug, Error)]
pub enum EvalError {
    /// The file could not be read.
    #[error("{path}: {source}")]
    Io {
        /// The file path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The file is not a valid result.
    #[error("{path}: invalid eval result: {source}")]
    Json {
        /// The result file path.
        path: PathBuf,
        /// Underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// A query line is not valid JSON.
    #[error("{path}:{line}: invalid query: {source}")]
    Query {
        /// The query file path.
        path: PathBuf,
        /// 1-based line number.
        line: usize,
        /// Underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// Searching failed.
    #[error(transparent)]
    Index(#[from] IndexError),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> EvalError + '_ {
    move |source| EvalError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// One row of `queries.jsonl` (SPEC §2.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Query {
    /// Query id.
    pub id: String,
    /// The query text.
    pub query: String,
    /// Page ids or id prefixes that count as a hit.
    #[serde(default)]
    pub expected: Vec<String>,
    /// Query kind, possibly empty.
    #[serde(default)]
    pub kind: String,
    /// Held out from tuning decisions.
    #[serde(default)]
    pub holdout: bool,
}

/// Read `queries.jsonl`; blank lines are skipped.
pub fn load_queries(path: &Path) -> Result<Vec<Query>, EvalError> {
    jsonl::read(path).map_err(|err| match err {
        JsonlError::Io { path, source } => EvalError::Io { path, source },
        JsonlError::Json { path, line, source } => EvalError::Query { path, line, source },
    })
}

/// Whether `page_id` (`<source>::<path>`) satisfies an `expected` entry.
pub fn matches(page_id: &str, expected: &str) -> bool {
    let page = page_id.replacen("::", "/", 1);
    let (prefix, any_below) = match expected.split_once("::") {
        Some((source, rest)) => (format!("{source}/{rest}"), rest.ends_with('/')),
        None => (expected.to_string(), true),
    };
    let prefix = prefix.trim_end_matches('/');
    if prefix.is_empty() {
        return false;
    }
    page == prefix
        || (any_below && page.starts_with(prefix) && page[prefix.len()..].starts_with('/'))
}

/// The retrieval metrics over a set of queries.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    /// Fraction of queries with an expected page in the top 5.
    #[serde(rename = "recall@5")]
    pub recall5: f64,
    /// Fraction of queries with an expected page in the top 10.
    #[serde(rename = "recall@10")]
    pub recall10: f64,
    /// Mean reciprocal rank of the first expected hit.
    pub mrr: f64,
    /// Number of queries.
    pub n: usize,
}

impl Metrics {
    /// Aggregate per-query rows; all zero for no rows.
    pub fn of(rows: &[&QueryResult]) -> Metrics {
        let n = rows.len();
        if n == 0 {
            return Metrics {
                recall5: 0.0,
                recall10: 0.0,
                mrr: 0.0,
                n: 0,
            };
        }
        let count = |pred: fn(&QueryResult) -> bool| float(rows.iter().filter(|r| pred(r)).count());
        Metrics {
            recall5: count(|r| r.hit5) / float(n),
            recall10: count(|r| r.hit10) / float(n),
            mrr: rows.iter().map(|r| r.rr).sum::<f64>() / float(n),
            n,
        }
    }
}

#[allow(clippy::cast_precision_loss)]
fn float(n: usize) -> f64 {
    n as f64
}

/// Metrics for one split (tuning or held-out), overall and per query kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Split {
    /// All queries in the split.
    pub overall: Metrics,
    /// Queries grouped by `kind`.
    #[serde(default)]
    pub per_kind: BTreeMap<String, Metrics>,
}

impl Split {
    /// Aggregate per-query rows overall and per kind.
    pub fn of(rows: &[&QueryResult]) -> Split {
        let mut by_kind: BTreeMap<String, Vec<&QueryResult>> = BTreeMap::new();
        for row in rows {
            by_kind.entry(row.kind.clone()).or_default().push(row);
        }
        Split {
            overall: Metrics::of(rows),
            per_kind: by_kind
                .into_iter()
                .map(|(kind, rows)| (kind, Metrics::of(&rows)))
                .collect(),
        }
    }
}

/// One query's outcome (SPEC §6 per-query row).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryResult {
    /// Query id.
    pub id: String,
    /// Query kind.
    #[serde(default)]
    pub kind: String,
    /// Whether the query is held out from tuning.
    #[serde(default)]
    pub holdout: bool,
    /// An expected page was in the top 5.
    pub hit5: bool,
    /// An expected page was in the top 10.
    pub hit10: bool,
    /// Reciprocal rank of the first expected hit (0 when none).
    pub rr: f64,
    /// The page ids returned, best first.
    #[serde(default)]
    pub top: Vec<String>,
}

impl QueryResult {
    /// Score `top` (the `k` page ids returned, best first) against the query's expectations.
    pub fn score(query: &Query, top: Vec<String>, k: usize) -> QueryResult {
        let rank = top
            .iter()
            .position(|id| query.expected.iter().any(|exp| matches(id, exp)));
        QueryResult {
            id: query.id.clone(),
            kind: query.kind.clone(),
            holdout: query.holdout,
            hit5: rank.is_some_and(|r| r < 5),
            hit10: rank.is_some_and(|r| r < k.min(10)),
            rr: rank.map_or(0.0, |r| 1.0 / float(r + 1)),
            top,
        }
    }
}

/// The JSON `eval` writes with `--json` and `report` reads with `--eval-before/--eval-after`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalSummary {
    /// Queries with `holdout: false`.
    pub tuning: Split,
    /// Queries with `holdout: true`, when there are any.
    #[serde(default)]
    pub holdout: Option<Split>,
    /// Per-query rows.
    #[serde(default)]
    pub queries: Vec<QueryResult>,
    /// The backend that produced this result (SPEC §16.1), e.g. `"bm25"` or `"dense"`. Left
    /// empty (and omitted from the JSON) by the plain BM25 `eval` path, which predates backend
    /// selection; only `eval --backend`/`--compare` set it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub backend: String,
}

impl EvalSummary {
    /// Record which backend produced this result (SPEC §16.1).
    #[must_use]
    pub fn with_backend(mut self, name: &str) -> EvalSummary {
        self.backend = name.to_string();
        self
    }
}

impl EvalSummary {
    /// Read a result file.
    pub fn load(path: &Path) -> Result<EvalSummary, EvalError> {
        let text = std::fs::read_to_string(path).map_err(io(path))?;
        serde_json::from_str(&text).map_err(|source| EvalError::Json {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Write the result as pretty JSON with a trailing newline.
    pub fn save(&self, path: &Path) -> Result<(), EvalError> {
        let text = self.to_json().map_err(|source| EvalError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        std::fs::write(path, text).map_err(io(path))
    }

    /// The result as pretty JSON with a trailing newline.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        Ok(text)
    }

    /// The row for a query id.
    pub fn query(&self, id: &str) -> Option<&QueryResult> {
        self.queries.iter().find(|q| q.id == id)
    }
}

/// Run every query against `index` with a result list of `k` pages.
pub fn evaluate(index: &Index, queries: &[Query], k: usize) -> Result<EvalSummary, EvalError> {
    let mut rows = Vec::with_capacity(queries.len());
    for query in queries {
        let top = index
            .search(&query.query, k, None)?
            .into_iter()
            .map(|hit| hit.page_id)
            .collect();
        rows.push(QueryResult::score(query, top, k));
    }
    Ok(summarise(rows))
}

/// Fold per-query rows into tuning and held-out splits.
pub fn summarise(rows: Vec<QueryResult>) -> EvalSummary {
    let tuning: Vec<&QueryResult> = rows.iter().filter(|r| !r.holdout).collect();
    let holdout: Vec<&QueryResult> = rows.iter().filter(|r| r.holdout).collect();
    EvalSummary {
        tuning: Split::of(&tuning),
        holdout: (!holdout.is_empty()).then(|| Split::of(&holdout)),
        queries: rows,
        backend: String::new(),
    }
}

/// The outcome of `--gate`: the tuning recall@5 now and in the baseline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gate {
    /// Tuning recall@5 of the baseline.
    pub baseline: f64,
    /// Tuning recall@5 now.
    pub current: f64,
    /// Largest tolerated drop (`eval.max_recall_drop`).
    pub max_drop: f64,
}

impl Gate {
    /// How much recall@5 dropped (negative when it rose).
    pub fn drop(&self) -> f64 {
        self.baseline - self.current
    }

    /// Whether the drop is within tolerance.
    pub fn passed(&self) -> bool {
        self.drop() <= self.max_drop
    }
}

/// Compare the tuning recall@5 of `current` with `baseline`.
pub fn gate(current: &EvalSummary, baseline: &EvalSummary, max_drop: f64) -> Gate {
    Gate {
        baseline: baseline.tuning.overall.recall5,
        current: current.tuning.overall.recall5,
        max_drop,
    }
}

/// A query whose reciprocal rank changed between two results.
#[derive(Debug, Clone, PartialEq)]
pub struct RankChange {
    /// Query id.
    pub id: String,
    /// Reciprocal rank before (`None` when the query was not evaluated).
    pub before: Option<f64>,
    /// Reciprocal rank after (`None` when the query was not evaluated).
    pub after: Option<f64>,
}

/// The difference between two results (`--with` / `--without`).
#[derive(Debug, Clone, PartialEq)]
pub struct Delta {
    /// Tuning metrics before and after.
    pub tuning: (Metrics, Metrics),
    /// Held-out metrics before and after, when either side has them.
    pub holdout: Option<(Metrics, Metrics)>,
    /// Queries whose reciprocal rank changed, in `after` order.
    pub changed: Vec<RankChange>,
}

/// Compare two results.
pub fn delta(before: &EvalSummary, after: &EvalSummary) -> Delta {
    let holdout = match (&before.holdout, &after.holdout) {
        (None, None) => None,
        (b, a) => Some((
            b.as_ref().map_or_else(|| Metrics::of(&[]), |s| s.overall),
            a.as_ref().map_or_else(|| Metrics::of(&[]), |s| s.overall),
        )),
    };
    let mut changed = Vec::new();
    for row in &after.queries {
        let old = before.query(&row.id).map(|q| q.rr);
        if old != Some(row.rr) {
            changed.push(RankChange {
                id: row.id.clone(),
                before: old,
                after: Some(row.rr),
            });
        }
    }
    for row in &before.queries {
        if after.query(&row.id).is_none() {
            changed.push(RankChange {
                id: row.id.clone(),
                before: Some(row.rr),
                after: None,
            });
        }
    }
    Delta {
        tuning: (before.tuning.overall, after.tuning.overall),
        holdout,
        changed,
    }
}

fn table_rows(out: &mut String, label: &str, split: &Split) {
    let row = |out: &mut String, kind: &str, m: &Metrics| {
        let _ = writeln!(
            out,
            "| {label} | {kind} | {} | {:.3} | {:.3} | {:.3} |",
            m.n, m.recall5, m.recall10, m.mrr
        );
    };
    row(out, "overall", &split.overall);
    for (kind, metrics) in &split.per_kind {
        row(out, kind, metrics);
    }
}

/// The Markdown table `eval` prints on stderr.
pub fn render_table(summary: &EvalSummary) -> String {
    let mut out = String::from(
        "| split | kind | n | recall@5 | recall@10 | MRR |\n|---|---|---|---|---|---|\n",
    );
    table_rows(&mut out, "tuning", &summary.tuning);
    if let Some(holdout) = &summary.holdout {
        table_rows(&mut out, "held-out", holdout);
    }
    out
}

fn delta_line(out: &mut String, label: &str, (before, after): (Metrics, Metrics)) {
    let cell = |name: &str, b: f64, a: f64| format!("{name} {b:.3} → {a:.3} ({:+.3})", a - b);
    let _ = writeln!(
        out,
        "{label}: {}; {}; {}; n {} → {}",
        cell("recall@5", before.recall5, after.recall5),
        cell("recall@10", before.recall10, after.recall10),
        cell("MRR", before.mrr, after.mrr),
        before.n,
        after.n
    );
}

/// The human-readable delta `eval --with/--without` prints on stderr.
pub fn render_delta(delta: &Delta) -> String {
    let mut out = String::new();
    delta_line(&mut out, "tuning", delta.tuning);
    if let Some(holdout) = delta.holdout {
        delta_line(&mut out, "held-out", holdout);
    }
    if delta.changed.is_empty() {
        out.push_str("no query changed rank\n");
    } else {
        let _ = writeln!(out, "{} queries changed rank:", delta.changed.len());
        for change in &delta.changed {
            let fmt = |rr: Option<f64>| rr.map_or_else(|| "–".to_string(), |v| format!("{v:.3}"));
            let _ = writeln!(
                out,
                "  {}: rr {} → {}",
                change.id,
                fmt(change.before),
                fmt(change.after)
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Priorities;
    use crate::index::testing::{SourceSpec, write_artifact};

    #[test]
    fn result_json_round_trips() {
        let text = r#"{"tuning":{"overall":{"recall@5":0.75,"recall@10":0.875,"mrr":0.6,"n":8},
            "per_kind":{"howto":{"recall@5":1.0,"recall@10":1.0,"mrr":0.9,"n":3}}},
            "queries":[{"id":"q1","kind":"howto","holdout":false,"hit5":true,"hit10":true,"rr":1.0,"top":["a::b.md"]}]}"#;
        let summary: EvalSummary = serde_json::from_str(text).unwrap();
        assert!((summary.tuning.overall.recall5 - 0.75).abs() < 1e-9);
        assert_eq!(summary.tuning.per_kind["howto"].n, 3);
        assert!(summary.holdout.is_none());
        assert_eq!(summary.queries[0].top, ["a::b.md"]);
        let again: EvalSummary =
            serde_json::from_str(&serde_json::to_string(&summary).unwrap()).unwrap();
        assert_eq!(again, summary);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("eval.json");
        std::fs::write(&path, text).unwrap();
        assert_eq!(EvalSummary::load(&path).unwrap(), summary);
        assert!(matches!(
            EvalSummary::load(&dir.path().join("nope.json")).unwrap_err(),
            EvalError::Io { .. }
        ));
        let saved = dir.path().join("saved.json");
        summary.save(&saved).unwrap();
        assert!(std::fs::read_to_string(&saved).unwrap().ends_with("}\n"));
        assert_eq!(EvalSummary::load(&saved).unwrap(), summary);
    }

    #[test]
    fn expected_matches_both_id_forms() {
        let page = "handbook::docs/user/tutorials/01-40-caching.md";
        assert!(matches(
            page,
            "handbook::docs/user/tutorials/01-40-caching.md"
        ));
        assert!(matches(page, "handbook::docs/user/"));
        assert!(matches(page, "handbook::docs/"));
        assert!(
            !matches(page, "handbook::docs/user"),
            "no trailing slash: exact only"
        );
        assert!(!matches(
            page,
            "handbook::docs/user/tutorials/01-40-caching"
        ));
        assert!(matches(
            page,
            "handbook/docs/user/tutorials/01-40-caching.md"
        ));
        assert!(matches(page, "handbook/docs/user"));
        assert!(matches(page, "handbook/docs/user/"));
        assert!(matches(page, "handbook"));
        assert!(!matches(page, "handbook/docs/use"));
        assert!(!matches(
            "handbook::docs/super-user/x.md",
            "handbook/docs/user"
        ));
        assert!(!matches(page, "guides/docs/user"));
        assert!(!matches(page, ""));
        assert!(!matches(page, "::"));
        assert!(
            matches("handbook/docs/user/a.md", "handbook/docs/user"),
            "legacy ids too"
        );
    }

    #[test]
    fn queries_load_with_defaults_and_report_bad_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queries.jsonl");
        std::fs::write(
            &path,
            "{\"id\": \"q1\", \"query\": \"x\", \"expected\": [\"a/b\"]}\n\n\
             {\"id\": \"q2\", \"kind\": \"howto\", \"query\": \"y\", \"expected\": [], \"holdout\": true}\n",
        )
        .unwrap();
        let queries = load_queries(&path).unwrap();
        assert_eq!(queries.len(), 2);
        assert_eq!(queries[0].kind, "");
        assert!(!queries[0].holdout);
        assert!(queries[1].holdout);
        std::fs::write(&path, "{\"id\": \"q1\"}\nnot json\n").unwrap();
        let err = load_queries(&path).unwrap_err();
        assert!(matches!(err, EvalError::Query { line: 1, .. }), "{err}");
        assert!(matches!(
            load_queries(&dir.path().join("missing.jsonl")).unwrap_err(),
            EvalError::Io { .. }
        ));
    }

    #[test]
    fn scoring_uses_rank_and_k() {
        let query = Query {
            id: "q".into(),
            query: String::new(),
            expected: vec!["s::docs/".into()],
            kind: "howto".into(),
            holdout: true,
        };
        let top: Vec<String> = (0..12).map(|i| format!("s::other/{i}.md")).collect();
        let miss = QueryResult::score(&query, top.clone(), 12);
        assert!(!miss.hit5 && !miss.hit10 && miss.rr == 0.0);
        let mut hit = top.clone();
        hit[7] = "s::docs/x.md".into();
        let row = QueryResult::score(&query, hit.clone(), 12);
        assert!(!row.hit5 && row.hit10 && (row.rr - 0.125).abs() < 1e-12);
        assert!(row.holdout && row.kind == "howto" && row.top.len() == 12);
        let mut late = top;
        late[10] = "s::docs/x.md".into();
        let row = QueryResult::score(&query, late, 12);
        assert!(!row.hit10 && (row.rr - 1.0 / 11.0).abs() < 1e-12);
        // With k below 10, recall@10 is recall@k.
        let mut short: Vec<String> = (0..3).map(|i| format!("s::other/{i}.md")).collect();
        short[2] = "s::docs/x.md".into();
        assert!(QueryResult::score(&query, short, 3).hit10);
    }

    fn artifact() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        write_artifact(
            dir.path(),
            &[
                SourceSpec {
                    name: "handbook",
                    repo: "example-org/handbook",
                    pages: &[
                        (
                            "docs/user/README.md",
                            "Storage Module",
                            "# Storage\n\nThe storage module keeps uploaded files.\n\n## Upload caching\n\nEnable upload caching with a bucket label.\n",
                        ),
                        (
                            "docs/user/quotas.md",
                            "",
                            "# Configure Quotas\n\nRate limits in strict mode.\n",
                        ),
                    ],
                    residue: &[(
                        "docs/user/billing-note.md",
                        "# Billing invoices scale\n\nScale billing invoices.\n",
                    )],
                },
                SourceSpec {
                    name: "billing",
                    repo: "example-org/billing",
                    pages: &[(
                        "docs/user/README.md",
                        "Billing",
                        "# Billing\n\nInvoices scale to zero.\n",
                    )],
                    residue: &[],
                },
            ],
        );
        let queries = dir.path().join("queries.jsonl");
        std::fs::write(
            &queries,
            concat!(
                "{\"id\": \"caching\", \"kind\": \"howto\", \"query\": \"enable upload caching\", \"expected\": [\"handbook/docs/user\"]}\n",
                "{\"id\": \"quotas\", \"kind\": \"howto\", \"query\": \"quotas rate limits\", \"expected\": [\"handbook::docs/user/quotas.md\"]}\n",
                "{\"id\": \"invoices\", \"kind\": \"concept\", \"query\": \"billing invoices scale\", \"expected\": [\"billing::docs/user/\"]}\n",
                "{\"id\": \"held\", \"kind\": \"concept\", \"query\": \"uploaded files\", \"expected\": [\"billing/docs/user\"], \"holdout\": true}\n",
            ),
        )
        .unwrap();
        (dir, queries)
    }

    #[test]
    fn metrics_on_a_synthetic_artifact_separate_holdout_rows() {
        let (dir, queries) = artifact();
        let index = Index::build(dir.path(), &Priorities::default()).unwrap();
        let queries = load_queries(&queries).unwrap();
        let summary = evaluate(&index, &queries, 10).unwrap();
        let tuning = summary.tuning.overall;
        assert_eq!(tuning.n, 3);
        assert!((tuning.recall5 - 1.0).abs() < 1e-12 && (tuning.mrr - 1.0).abs() < 1e-12);
        assert_eq!(summary.tuning.per_kind["howto"].n, 2);
        assert_eq!(summary.tuning.per_kind["concept"].n, 1);
        let holdout = summary.holdout.as_ref().expect("held-out split");
        assert_eq!(holdout.overall.n, 1);
        assert!(holdout.overall.recall5 == 0.0 && holdout.overall.mrr == 0.0);
        assert_eq!(summary.queries.len(), 4);
        assert_eq!(
            summary.query("caching").unwrap().top[0],
            "handbook::docs/user/README.md"
        );
        let table = render_table(&summary);
        assert!(table.contains("| tuning | overall | 3 | 1.000 | 1.000 | 1.000 |"));
        assert!(table.contains("| held-out | overall | 1 | 0.000 | 0.000 | 0.000 |"));
        assert!(table.contains("| tuning | concept | 1 |"));

        let none = summarise(Vec::new());
        assert_eq!(none.tuning.overall.n, 0);
        assert!(none.holdout.is_none());
        assert!(!render_table(&none).contains("held-out"));
    }

    #[test]
    fn with_and_without_change_the_delta() {
        let (dir, queries) = artifact();
        let queries = load_queries(&queries).unwrap();
        let priorities = Priorities::default();
        let before = evaluate(
            &Index::build(dir.path(), &priorities).unwrap(),
            &queries,
            10,
        )
        .unwrap();

        // --with: the residue page outscores the billing page for the "invoices" query.
        let mut pages = crate::index::load_pages(dir.path(), &priorities).unwrap();
        pages.push(
            crate::index::load_residue_page(
                dir.path(),
                "handbook::docs/user/billing-note.md",
                &priorities,
            )
            .unwrap(),
        );
        let after = evaluate(&Index::from_pages(pages).unwrap(), &queries, 10).unwrap();
        let d = delta(&before, &after);
        assert!(d.tuning.1.mrr < d.tuning.0.mrr, "{d:?}");
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.changed[0].id, "invoices");
        assert_eq!(d.changed[0].before, Some(1.0));
        assert_eq!(d.changed[0].after, Some(0.5));
        let text = render_delta(&d);
        assert!(
            text.contains("tuning: recall@5 1.000 → 1.000 (+0.000)"),
            "{text}"
        );
        assert!(text.contains("MRR 1.000 → 0.833 (-0.167)"), "{text}");
        assert!(text.contains("  invoices: rr 1.000 → 0.500"), "{text}");
        assert!(text.contains("held-out: "), "{text}");

        // --without: dropping the quotas page loses that query entirely.
        let mut pages = crate::index::load_pages(dir.path(), &priorities).unwrap();
        pages.retain(|p| p.id != "handbook::docs/user/quotas.md");
        let after = evaluate(&Index::from_pages(pages).unwrap(), &queries, 10).unwrap();
        let d = delta(&before, &after);
        assert!((d.tuning.1.recall5 - 2.0 / 3.0).abs() < 1e-12);
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.changed[0].id, "quotas");
        assert_eq!(d.changed[0].after, Some(0.0));

        assert_eq!(delta(&before, &before).changed, []);
        assert!(render_delta(&delta(&before, &before)).contains("no query changed rank"));
        // A query missing on one side is reported too.
        let mut fewer = before.clone();
        fewer.queries.retain(|q| q.id != "quotas");
        assert_eq!(delta(&before, &fewer).changed[0].after, None);
        assert_eq!(delta(&fewer, &before).changed[0].before, None);
    }

    #[test]
    fn gate_compares_tuning_recall_at_5() {
        let (dir, queries) = artifact();
        let queries = load_queries(&queries).unwrap();
        let index = Index::build(dir.path(), &Priorities::default()).unwrap();
        let summary = evaluate(&index, &queries, 10).unwrap();
        let mut baseline = summary.clone();
        assert!(gate(&summary, &baseline, 0.0).passed());
        baseline.tuning.overall.recall5 = 1.0;
        let mut current = summary;
        current.tuning.overall.recall5 = 0.9;
        let g = gate(&current, &baseline, 0.05);
        assert!(!g.passed());
        assert!((g.drop() - 0.1).abs() < 1e-12);
        assert!(gate(&current, &baseline, 0.1).passed());
        // Held-out rows never gate.
        current.holdout = Some(Split::of(&[]));
        current.tuning.overall.recall5 = 1.0;
        assert!(gate(&current, &baseline, 0.0).passed());
    }
}
