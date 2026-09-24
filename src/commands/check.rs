//! `check`: compare a `report.json` with the `gates:` in `pinakes.yaml` (SPEC §2.11).

use std::path::PathBuf;

use serde::Serialize;

use crate::config::{Config, Gates};
use crate::error::CommandError;
use crate::report::{FACTS_VERSION, ReportFacts};
use crate::workspace::Paths;

/// The `version` of the JSON summary `check` prints (SPEC §2.11).
pub const CHECK_VERSION: u32 = 1;

/// Inputs for `check`.
#[derive(Debug, Clone, Default)]
pub struct CheckOptions {
    /// The `report.json` to check (default: `report.json` next to the config).
    pub report: Option<PathBuf>,
}

/// One gate whose count exceeds its maximum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Violation {
    /// The gate's key in `pinakes.yaml`, e.g. `undecided_residue_max`.
    pub gate: &'static str,
    /// The configured maximum.
    pub limit: usize,
    /// The count `report.json` shows.
    pub actual: usize,
}

/// What `check` found: how many gates were set and which of them are violated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckOutcome {
    /// Gates that are set in the config; `0` means none is configured.
    pub checked: usize,
    /// The violated gates, in the order of SPEC §2.11's table.
    pub violations: Vec<Violation>,
    /// Gates that were set but could not tell anything, one sentence each, for stderr: today
    /// only `removed_pages_max` on a report made without a previous manifest.
    pub warnings: Vec<String>,
}

/// The JSON document on stdout (SPEC §2.11).
#[derive(Serialize)]
struct CheckDocument<'a> {
    version: u32,
    checked: usize,
    violations: &'a [Violation],
}

impl CheckOutcome {
    /// `true` when no gate is violated.
    pub fn passed(&self) -> bool {
        self.violations.is_empty()
    }

    /// The outcome as one JSON line with a trailing newline (SPEC §2.11).
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        let mut text = serde_json::to_string(&CheckDocument {
            version: CHECK_VERSION,
            checked: self.checked,
            violations: &self.violations,
        })?;
        text.push('\n');
        Ok(text)
    }
}

/// The gates that are set but vacuous on this report (SPEC §2.11): `removed_pages_max` reads
/// `pages.removed`, which is empty when `report` ran without `--old`.
pub fn check_warnings(gates: &Gates, facts: &ReportFacts) -> Vec<String> {
    let mut warnings = Vec::new();
    if gates.removed_pages_max.is_some() && facts.summary.changes.is_none() {
        warnings.push(
            "removed_pages_max is set but the report has no previous manifest \
             (summary.changes is null): the gate passes vacuously; run report with --old"
                .to_string(),
        );
    }
    warnings
}

/// Compare every configured gate with the count it reads in `facts`; a count strictly above
/// the maximum is a violation, and an absent gate is skipped.
pub fn check_gates(gates: &Gates, facts: &ReportFacts) -> Vec<Violation> {
    let unresolved: usize = facts.unresolved_links.values().map(Vec::len).sum();
    let candidates = [
        (
            "undecided_residue_max",
            gates.undecided_residue_max,
            facts.residue.undecided.len(),
        ),
        (
            "removed_pages_max",
            gates.removed_pages_max,
            facts.pages.removed.len(),
        ),
        (
            "expired_decisions_max",
            gates.expired_decisions_max,
            facts.expired_decisions.len(),
        ),
        (
            "archived_sources_max",
            gates.archived_sources_max,
            facts.archived_sources.len(),
        ),
        (
            "unresolved_links_max",
            gates.unresolved_links_max,
            unresolved,
        ),
        (
            "duplicates_max",
            gates.duplicates_max,
            facts.duplicates.count,
        ),
    ];
    candidates
        .into_iter()
        .filter_map(|(gate, limit, actual)| {
            let limit = limit?;
            (actual > limit).then_some(Violation {
                gate,
                limit,
                actual,
            })
        })
        .collect()
}

/// Run `check`: load the config's `gates`, read the report and compare. Without any gate set
/// the report is not read at all, so a workspace that never asked for gates cannot fail here.
pub fn check(paths: &Paths, options: &CheckOptions) -> Result<CheckOutcome, CommandError> {
    let config = Config::load(&paths.config)?;
    let gates = config.gates.unwrap_or_default();
    let checked = gates.configured();
    if checked == 0 {
        return Ok(CheckOutcome {
            checked,
            violations: Vec::new(),
            warnings: Vec::new(),
        });
    }
    let report = options
        .report
        .clone()
        .unwrap_or_else(|| paths.config_dir().join("report.json"));
    if !report.is_file() {
        return Err(CommandError::NoReport { path: report });
    }
    let facts = ReportFacts::load(&report)?;
    if facts.version > FACTS_VERSION {
        return Err(CommandError::ReportVersion {
            path: report,
            version: facts.version,
            supported: FACTS_VERSION,
        });
    }
    Ok(CheckOutcome {
        checked,
        violations: check_gates(&gates, &facts),
        warnings: check_warnings(&gates, &facts),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::decisions::Verdict;
    use crate::report::{
        ChangesFacts, DuplicatesFacts, ExpiredFacts, ExpiredWhy, PagesFacts, RemovedPageFacts,
        RemovedReason, ResidueFacts, SummaryFacts,
    };

    /// A report with three undecided residue ids, two removed pages, one expired decision, one
    /// archived source, three unresolved links over two sources and four duplicate pairs.
    fn facts() -> ReportFacts {
        let id = |n: usize| format!("handbook::docs/{n}.md");
        ReportFacts {
            version: crate::report::FACTS_VERSION,
            summary: SummaryFacts {
                sources: 2,
                pages: 10,
                residue: 3,
                undecided: 3,
                excluded: 0,
                decisions: 1,
                changes: None,
            },
            eval: None,
            pages: PagesFacts {
                added: vec![],
                removed: (1..=2)
                    .map(|n| RemovedPageFacts {
                        id: id(n),
                        reason: RemovedReason::GoneUpstream,
                    })
                    .collect(),
                changed: vec![],
            },
            residue: ResidueFacts {
                new: vec![],
                undecided: (1..=3).map(id).collect(),
                excluded: vec![],
            },
            expired_decisions: vec![ExpiredFacts {
                id: id(1),
                decision: Verdict::Include,
                why: ExpiredWhy::PageGone,
            }],
            unresolved_links: BTreeMap::from([
                ("guides".to_string(), vec![id(4), id(5)]),
                ("handbook".to_string(), vec![id(6)]),
            ]),
            archived_sources: vec!["guides".to_string()],
            duplicates: DuplicatesFacts {
                count: 4,
                pairs: vec![],
            },
            usage: None,
        }
    }

    #[test]
    fn every_gate_reads_its_own_count() {
        let facts = facts();
        let all = Gates {
            undecided_residue_max: Some(0),
            removed_pages_max: Some(0),
            expired_decisions_max: Some(0),
            archived_sources_max: Some(0),
            unresolved_links_max: Some(0),
            duplicates_max: Some(0),
        };
        let violations = check_gates(&all, &facts);
        let expected = [
            ("undecided_residue_max", 3),
            ("removed_pages_max", 2),
            ("expired_decisions_max", 1),
            ("archived_sources_max", 1),
            ("unresolved_links_max", 3),
            ("duplicates_max", 4),
        ];
        assert_eq!(
            violations
                .iter()
                .map(|v| (v.gate, v.actual))
                .collect::<Vec<_>>(),
            expected
        );
        assert!(violations.iter().all(|v| v.limit == 0));
    }

    #[test]
    fn an_absent_gate_is_off_and_equal_to_the_limit_passes() {
        let facts = facts();
        assert!(check_gates(&Gates::default(), &facts).is_empty());

        // Every count sits exactly on its limit: nothing is violated.
        let exact = Gates {
            undecided_residue_max: Some(3),
            removed_pages_max: Some(2),
            expired_decisions_max: Some(1),
            archived_sources_max: Some(1),
            unresolved_links_max: Some(3),
            duplicates_max: Some(4),
        };
        assert!(check_gates(&exact, &facts).is_empty());

        // One below on a single gate: only that gate is reported, the others stay off.
        let one = Gates {
            duplicates_max: Some(3),
            ..Gates::default()
        };
        assert_eq!(
            check_gates(&one, &facts),
            vec![Violation {
                gate: "duplicates_max",
                limit: 3,
                actual: 4,
            }]
        );
    }

    #[test]
    fn the_outcome_serialises_as_the_spec_document() {
        let outcome = CheckOutcome {
            checked: 2,
            violations: vec![Violation {
                gate: "undecided_residue_max",
                limit: 0,
                actual: 3,
            }],
            warnings: vec![],
        };
        assert!(!outcome.passed());
        assert_eq!(
            outcome.to_json().unwrap(),
            "{\"version\":1,\"checked\":2,\"violations\":[{\"gate\":\"undecided_residue_max\",\
             \"limit\":0,\"actual\":3}]}\n"
        );
        let clean = CheckOutcome {
            checked: 0,
            violations: vec![],
            warnings: vec![],
        };
        assert!(clean.passed());
        assert_eq!(
            clean.to_json().unwrap(),
            "{\"version\":1,\"checked\":0,\"violations\":[]}\n"
        );
    }

    #[test]
    fn check_reads_the_config_and_the_report() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("pinakes.yaml");
        let paths = Paths::for_config(&config);
        let base = "version: 1\nsources:\n  - name: handbook\n    \
                    repo: https://github.com/example-org/handbook.git\n    ref: main\n    \
                    resolver:\n      type: glob\n      include: ['**/*.md']\n";

        // No gates: nothing is checked and the missing report is not an error.
        std::fs::write(&config, base).unwrap();
        let outcome = check(&paths, &CheckOptions::default()).unwrap();
        assert_eq!(outcome.checked, 0);
        assert!(outcome.passed());

        // Gates but no report: the error names the file and the command that writes it.
        std::fs::write(&config, format!("{base}gates:\n  duplicates_max: 3\n")).unwrap();
        let err = check(&paths, &CheckOptions::default()).unwrap_err();
        assert!(matches!(err, CommandError::NoReport { .. }));
        assert!(
            err.to_string().contains("run `pinakes report --json"),
            "{err}"
        );

        // A report, by default next to the config, else where --report says.
        let report = dir.path().join("report.json");
        facts().save(&report).unwrap();
        let outcome = check(&paths, &CheckOptions::default()).unwrap();
        assert_eq!(outcome.checked, 1);
        assert_eq!(outcome.violations.len(), 1);
        let elsewhere = dir.path().join("elsewhere.json");
        let mut fewer = facts();
        fewer.duplicates.count = 3;
        fewer.save(&elsewhere).unwrap();
        let options = CheckOptions {
            report: Some(elsewhere.clone()),
        };
        assert!(check(&paths, &options).unwrap().passed());

        // A report that is not a report document is the report module's error.
        std::fs::write(&elsewhere, "{\"version\": 1}\n").unwrap();
        assert!(matches!(
            check(&paths, &options).unwrap_err(),
            CommandError::Report(_)
        ));

        // A report from a newer pinakes is refused, not read with the wrong meaning.
        let mut newer = facts();
        newer.version = FACTS_VERSION + 1;
        newer.save(&elsewhere).unwrap();
        let err = check(&paths, &options).unwrap_err();
        assert!(matches!(err, CommandError::ReportVersion { version, .. } if version == 2));
        assert!(err.to_string().contains("version 2"), "{err}");
    }

    #[test]
    fn removed_pages_max_warns_without_a_previous_manifest() {
        let facts = facts();
        assert!(facts.summary.changes.is_none());
        let gates = Gates {
            removed_pages_max: Some(0),
            duplicates_max: Some(0),
            ..Gates::default()
        };
        let warnings = check_warnings(&gates, &facts);
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].starts_with("removed_pages_max is set but"),
            "{warnings:?}"
        );
        // The other gates still count: two removed pages exceed 0 by the facts alone.
        assert_eq!(check_gates(&gates, &facts).len(), 2);

        // Nothing to warn about without the gate, or with a previous manifest.
        assert!(check_warnings(&Gates::default(), &facts).is_empty());
        let mut with_old = facts.clone();
        with_old.summary.changes = Some(ChangesFacts {
            since: "2026-09-01T00:00:00Z".to_string(),
            added: 0,
            removed: 2,
            changed: 0,
        });
        assert!(check_warnings(&gates, &with_old).is_empty());
    }
}
