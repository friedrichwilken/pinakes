//! `pinakes classify`: batch LLM classification of undecided residue and near-duplicate
//! candidates (SPEC §14.2).
//!
//! This module is pure: it turns the page registry's residue records, duplicate pairs and their
//! corpus records into [`ClassifyItem`]s, sends them to the model in batches through
//! [`crate::llm`], and turns the model's verdicts into [`Decision`]s. Two rules are enforced
//! regardless of what the model says: a `new_source` residue page is never written as `include`,
//! and an id the model did not name in the batch it was given is ignored rather than guessed at.
//! All file I/O (reading `residue.jsonl`/`duplicates.jsonl`/the artifact, appending to
//! `decisions.jsonl`) is the caller's job (`commands::classify`).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::decisions::{Decision, Verdict};
use crate::duplicates::DuplicatePair;
use crate::llm::{self, ChatError, ChatTransport, LlmConfig};
use crate::page::{PageRegistry, PageStatus};
use crate::residue::Reason;

/// Default `--batch` size (SPEC §14.2).
pub const DEFAULT_BATCH: usize = 20;

/// Errors raised while classifying.
#[derive(Debug, Error)]
pub enum ClassifyError {
    /// Talking to the model failed.
    #[error(transparent)]
    Llm(#[from] ChatError),
}

/// One residue or near-duplicate item offered to the model.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClassifyItem {
    /// `<source>::<path>`.
    pub id: String,
    /// Hash of the page bytes the candidate reflects, for the decision that is written.
    #[serde(skip)]
    pub sha256: String,
    /// Page title, possibly empty.
    pub title: String,
    /// An excerpt of the page body.
    pub excerpt: String,
    /// The residue reason (`not_selected`, `unresolved_link`, `new_source`) or `"duplicate"`.
    pub reason: String,
    /// Sidebar section/TOC branch, or (for a duplicate) the canonical id and similarity.
    pub context: String,
}

/// Whether `decision` still applies to a candidate at `sha256` (mirrors [`Decision::applies_to`]
/// without requiring a residue entry).
fn already_decided(effective: &BTreeMap<String, Decision>, id: &str, sha256: &str) -> bool {
    effective.get(id).is_some_and(|d| d.applies_to(sha256))
}

/// Build the candidate list: undecided residue, then near-duplicate pairs' `duplicate` id not
/// already covered by residue or an active decision (SPEC §14.2).
pub fn candidates(
    registry: &PageRegistry,
    duplicates: &[DuplicatePair],
    effective: &BTreeMap<String, Decision>,
) -> Vec<ClassifyItem> {
    let mut out = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for record in registry.residue() {
        let PageStatus::Residue {
            reason, context, ..
        } = &record.status
        else {
            continue;
        };
        if already_decided(effective, &record.id, &record.sha256) {
            continue;
        }
        seen.insert(record.id.clone());
        out.push(ClassifyItem {
            id: record.id.clone(),
            sha256: record.sha256.clone(),
            title: record.title.clone(),
            excerpt: record.excerpt.clone().unwrap_or_default(),
            reason: reason.as_str().to_string(),
            context: context.clone(),
        });
    }
    for pair in duplicates {
        let id = &pair.duplicate;
        if seen.contains(id) {
            continue;
        }
        let Some(record) = registry.corpus_page(id).filter(|record| {
            matches!(record.status, PageStatus::Selected { .. }) && record.excerpt.is_some()
        }) else {
            continue;
        };
        if already_decided(effective, id, &record.sha256) {
            continue;
        }
        seen.insert(id.clone());
        out.push(ClassifyItem {
            id: id.clone(),
            sha256: record.sha256.clone(),
            title: record.title.clone(),
            excerpt: record.excerpt.clone().unwrap_or_default(),
            reason: "duplicate".to_string(),
            context: format!(
                "canonical {} (similarity {:.3})",
                pair.canonical, pair.similarity
            ),
        });
    }
    out
}

/// The model's raw verdict for one candidate, before the `new_source` rule is applied.
#[derive(Debug, Clone, Deserialize)]
struct ModelVerdict {
    id: String,
    decision: String,
    #[serde(default)]
    rationale: String,
    #[serde(default)]
    #[allow(dead_code)]
    confidence: f64,
}

/// What `run` produced.
#[derive(Debug, Default)]
pub struct Outcome {
    /// One decision per candidate the model named that was actually in the batch it was given.
    pub decisions: Vec<Decision>,
    /// Non-fatal observations: an id the model invented, or a rule override.
    pub warnings: Vec<String>,
}

const SYSTEM_PROMPT: &str = "You are curating a documentation corpus for a retrieval system. \
You will be given a JSON array of candidate pages, each with an id, title, excerpt, the reason \
it is a candidate, and context. For every candidate, decide whether it belongs in the corpus. \
Respond with a JSON array only - no prose, no markdown code fences, one object per candidate id \
you were given, in this exact shape: \
[{\"id\": \"...\", \"decision\": \"include\"|\"exclude\"|\"unsure\", \"rationale\": \"one \
sentence\", \"confidence\": 0.0}]";

fn user_prompt(batch: &[ClassifyItem]) -> String {
    serde_json::to_string_pretty(batch).unwrap_or_default()
}

/// Apply the `new_source` rule and stamp `by`/`at`; returns the decision and, when the rule
/// overrode the model, a warning describing it.
fn apply_rules(
    candidate: &ClassifyItem,
    verdict: &ModelVerdict,
    model: &str,
    at: &str,
) -> (Decision, Option<String>) {
    let mut decision = Verdict::parse(&verdict.decision).unwrap_or(Verdict::Unsure);
    let mut warning = None;
    if candidate.reason == Reason::NewSource.as_str() && decision == Verdict::Include {
        warning = Some(format!(
            "{}: model proposed include for a new_source candidate; recorded as unsure instead",
            candidate.id
        ));
        decision = Verdict::Unsure;
    }
    let reason = if verdict.rationale.trim().is_empty() {
        "no rationale given".to_string()
    } else {
        verdict.rationale.clone()
    };
    (
        Decision {
            id: candidate.id.clone(),
            sha256: candidate.sha256.clone(),
            decision,
            reason,
            by: format!("classifier:{model}"),
            at: at.to_string(),
        },
        warning,
    )
}

/// Send `candidates` to the model in batches of `batch_size` and turn the verdicts into
/// decisions stamped `by: "classifier:<model>"` at `at`.
pub fn run(
    transport: &dyn ChatTransport,
    config: &LlmConfig,
    candidates: &[ClassifyItem],
    batch_size: usize,
    at: &str,
) -> Result<Outcome, ClassifyError> {
    let batch_size = batch_size.max(1);
    let mut outcome = Outcome::default();
    for batch in candidates.chunks(batch_size) {
        let user = user_prompt(batch);
        let verdicts: Vec<ModelVerdict> = llm::chat(transport, config, SYSTEM_PROMPT, &user)?;
        let by_id: BTreeMap<&str, &ClassifyItem> =
            batch.iter().map(|c| (c.id.as_str(), c)).collect();
        for verdict in &verdicts {
            let Some(candidate) = by_id.get(verdict.id.as_str()) else {
                outcome
                    .warnings
                    .push(format!("model returned unknown id {:?}", verdict.id));
                continue;
            };
            let (decision, warning) = apply_rules(candidate, verdict, &config.model, at);
            if let Some(warning) = warning {
                outcome.warnings.push(warning);
            }
            outcome.decisions.push(decision);
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duplicates::{DuplicateKind, Suggested};
    use crate::llm::testing::{Scripted, ScriptedTransport, completion};
    use crate::manifest::SelectedBy;
    use crate::page::PageRecord;

    /// A residue registry record for `id`, matching what `residue_entry` used to build.
    fn residue_record(id: &str, reason: Reason) -> PageRecord {
        let (source, path) = id.split_once("::").unwrap();
        PageRecord {
            id: id.to_string(),
            source: source.to_string(),
            path: path.to_string(),
            repo: String::new(),
            commit: String::new(),
            title: "Title".to_string(),
            doc_type: String::new(),
            section: String::new(),
            url: String::new(),
            sha256: "aa".repeat(32),
            excerpt: Some("some excerpt text".to_string()),
            status: PageStatus::Residue {
                reason,
                rule: None,
                context: "sidebar".to_string(),
            },
        }
    }

    /// A selected corpus registry record for `id`, with an optional excerpt.
    fn selected_record(id: &str, title: &str, excerpt: Option<&str>, sha256: &str) -> PageRecord {
        let (source, path) = id.split_once("::").unwrap();
        PageRecord {
            id: id.to_string(),
            source: source.to_string(),
            path: path.to_string(),
            repo: String::new(),
            commit: String::new(),
            title: title.to_string(),
            doc_type: String::new(),
            section: String::new(),
            url: String::new(),
            sha256: sha256.to_string(),
            excerpt: excerpt.map(str::to_string),
            status: PageStatus::Selected {
                by: SelectedBy::Include,
                rendered_from: None,
            },
        }
    }

    fn config() -> LlmConfig {
        LlmConfig {
            url: "https://example.test".to_string(),
            key: None,
            model: "test-model".to_string(),
        }
    }

    #[test]
    fn candidates_skip_residue_already_decided() {
        let r1 = residue_record("h::a.md", Reason::NotSelected);
        let r2 = residue_record("h::b.md", Reason::NotSelected);
        let mut decisions = BTreeMap::new();
        decisions.insert(
            r1.id.clone(),
            Decision {
                id: r1.id.clone(),
                sha256: r1.sha256.clone(),
                decision: Verdict::Exclude,
                reason: "noise".to_string(),
                by: "me".to_string(),
                at: "t".to_string(),
            },
        );
        let registry = PageRegistry::from_records([r1, r2.clone()]);
        let cands = candidates(&registry, &[], &decisions);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].id, r2.id);
    }

    #[test]
    fn candidates_include_undecided_near_duplicates_with_canonical_context() {
        let pair = DuplicatePair {
            kind: DuplicateKind::Near,
            similarity: 0.876,
            canonical: "h::a.md".to_string(),
            duplicate: "h::b.md".to_string(),
            why: "priority".to_string(),
            suggested: Suggested::Exclude,
            canonical_url: String::new(),
            duplicate_url: String::new(),
        };
        let registry = PageRegistry::from_records([selected_record(
            "h::b.md",
            "B",
            Some("excerpt"),
            &"bb".repeat(32),
        )]);
        let cands = candidates(&registry, &[pair], &BTreeMap::new());
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].reason, "duplicate");
        assert!(cands[0].context.contains("h::a.md"));
        assert!(cands[0].context.contains("0.876"));
    }

    #[test]
    fn candidates_skip_a_duplicate_with_no_excerpt_or_no_selected_corpus_record() {
        let mut registry = PageRegistry::from_records([
            selected_record("h::no-excerpt.md", "No excerpt", None, &"cc".repeat(32)),
            PageRecord::artifact_only("h", "artifact-only.md", "dd".repeat(32), String::new()),
            residue_record("h::residue-only.md", Reason::NotSelected),
        ]);
        // An artifact-only record can have an excerpt too, but it is still not `Selected`.
        assert!(registry.set_excerpt("h::artifact-only.md", "has an excerpt".to_string()));
        // Already decided so the residue loop does not itself add it to `seen`, isolating what
        // the duplicate loop's lookup does with a residue-only id.
        let mut already_decided = BTreeMap::new();
        already_decided.insert(
            "h::residue-only.md".to_string(),
            Decision {
                id: "h::residue-only.md".to_string(),
                sha256: "aa".repeat(32),
                decision: Verdict::Exclude,
                reason: "reviewed separately".to_string(),
                by: "me".to_string(),
                at: "t".to_string(),
            },
        );

        let pair = |duplicate: &str| DuplicatePair {
            kind: DuplicateKind::Mirror,
            similarity: 1.0,
            canonical: "h::a.md".to_string(),
            duplicate: duplicate.to_string(),
            why: "priority".to_string(),
            suggested: Suggested::Exclude,
            canonical_url: String::new(),
            duplicate_url: String::new(),
        };
        let pairs = vec![
            pair("h::no-excerpt.md"),
            pair("h::artifact-only.md"),
            pair("h::residue-only.md"),
            pair("h::missing.md"),
        ];
        let cands = candidates(&registry, &pairs, &already_decided);
        assert!(cands.is_empty(), "{cands:?}");
    }

    #[test]
    fn candidates_preserve_residue_order_across_sources_a_and_a_b() {
        // By id string, "a-b::r.md" sorts before "a::r.md" (`-` is 0x2D, `:` is 0x3A): inserting
        // them the other way round pins that `candidates` follows the registry's insertion
        // order, not a re-sort by id.
        let registry = PageRegistry::from_records([
            residue_record("a::r.md", Reason::NotSelected),
            residue_record("a-b::r.md", Reason::NotSelected),
        ]);
        let cands = candidates(&registry, &[], &BTreeMap::new());
        assert_eq!(
            cands.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["a::r.md", "a-b::r.md"],
            "residue candidates follow the registry's residue() order, not id order"
        );
    }

    #[test]
    fn run_batches_at_the_given_size() {
        let cands: Vec<ClassifyItem> = (0..5)
            .map(|i| ClassifyItem {
                id: format!("h::{i}.md"),
                sha256: "aa".repeat(32),
                title: format!("T{i}"),
                excerpt: String::new(),
                reason: "not_selected".to_string(),
                context: String::new(),
            })
            .collect();
        let reply = |ids: &[&str]| {
            let items: Vec<_> = ids
                .iter()
                .map(|id| {
                    serde_json::json!({"id": id, "decision": "exclude", "rationale": "noise", "confidence": 0.9})
                })
                .collect();
            completion(&serde_json::to_string(&items).unwrap())
        };
        let transport = ScriptedTransport::new(vec![
            Scripted::Ok(reply(&["h::0.md", "h::1.md"])),
            Scripted::Ok(reply(&["h::2.md", "h::3.md"])),
            Scripted::Ok(reply(&["h::4.md"])),
        ]);
        let outcome = run(&transport, &config(), &cands, 2, "2026-09-16T12:00:00Z").unwrap();
        assert_eq!(outcome.decisions.len(), 5);
        assert_eq!(transport.requests.lock().unwrap().len(), 3);
        assert!(outcome.warnings.is_empty());
        assert_eq!(outcome.decisions[0].by, "classifier:test-model");
    }

    #[test]
    fn new_source_include_is_overridden_to_unsure() {
        let candidate = ClassifyItem {
            id: "h::new.md".to_string(),
            sha256: "aa".repeat(32),
            title: "New".to_string(),
            excerpt: String::new(),
            reason: Reason::NewSource.as_str().to_string(),
            context: String::new(),
        };
        let reply = completion(
            &serde_json::to_string(&serde_json::json!([
                {"id": "h::new.md", "decision": "include", "rationale": "looks fine", "confidence": 0.5}
            ]))
            .unwrap(),
        );
        let transport = ScriptedTransport::new(vec![Scripted::Ok(reply)]);
        let outcome = run(&transport, &config(), &[candidate], 20, "t").unwrap();
        assert_eq!(outcome.decisions.len(), 1);
        assert_eq!(outcome.decisions[0].decision, Verdict::Unsure);
        assert_eq!(outcome.warnings.len(), 1);
        assert!(outcome.warnings[0].contains("new_source"));
    }

    #[test]
    fn unknown_id_from_the_model_is_a_warning_not_a_decision() {
        let candidate = ClassifyItem {
            id: "h::a.md".to_string(),
            sha256: "aa".repeat(32),
            title: "A".to_string(),
            excerpt: String::new(),
            reason: "not_selected".to_string(),
            context: String::new(),
        };
        let reply = completion(
            &serde_json::to_string(&serde_json::json!([
                {"id": "h::not-in-batch.md", "decision": "exclude", "rationale": "x", "confidence": 0.5}
            ]))
            .unwrap(),
        );
        let transport = ScriptedTransport::new(vec![Scripted::Ok(reply)]);
        let outcome = run(&transport, &config(), &[candidate], 20, "t").unwrap();
        assert!(outcome.decisions.is_empty());
        assert_eq!(outcome.warnings.len(), 1);
        assert!(outcome.warnings[0].contains("unknown id"));
    }

    #[test]
    fn unparseable_decision_defaults_to_unsure() {
        let candidate = ClassifyItem {
            id: "h::a.md".to_string(),
            sha256: "aa".repeat(32),
            title: "A".to_string(),
            excerpt: String::new(),
            reason: "not_selected".to_string(),
            context: String::new(),
        };
        let reply = completion(
            &serde_json::to_string(&serde_json::json!([
                {"id": "h::a.md", "decision": "maybe-ish", "rationale": "", "confidence": 0.5}
            ]))
            .unwrap(),
        );
        let transport = ScriptedTransport::new(vec![Scripted::Ok(reply)]);
        let outcome = run(&transport, &config(), &[candidate], 20, "t").unwrap();
        assert_eq!(outcome.decisions[0].decision, Verdict::Unsure);
        assert_eq!(outcome.decisions[0].reason, "no rationale given");
    }
}
