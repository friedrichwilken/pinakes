//! `pinakes classify`: batch LLM classification of undecided residue and near-duplicate
//! candidates (SPEC §14.2).
//!
//! This module is pure: it turns residue entries, duplicate pairs and a page lookup into
//! [`Candidate`]s, sends them to the model in batches through [`crate::llm`], and turns the
//! model's verdicts into [`Decision`]s. Two rules are enforced regardless of what the model
//! says: a `new_source` residue page is never written as `include`, and an id the model did not
//! name in the batch it was given is ignored rather than guessed at. All file I/O (reading
//! `residue.jsonl`/`duplicates.jsonl`/the artifact, appending to `decisions.jsonl`) is the
//! caller's job (`commands::classify`).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::decisions::{Decision, Verdict};
use crate::duplicates::DuplicatePair;
use crate::llm::{self, ChatError, ChatTransport, LlmConfig};
use crate::residue::{Reason, ResidueEntry};

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
pub struct Candidate {
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

/// Title, excerpt and `sha256` of a manifest page, as read from the artifact.
#[derive(Debug, Clone, PartialEq)]
pub struct PageFacts {
    /// Page title.
    pub title: String,
    /// An excerpt of the page body.
    pub excerpt: String,
    /// Hex SHA-256 of the file bytes.
    pub sha256: String,
}

/// External facts needed to build a candidate from a near-duplicate pair, so this module never
/// touches the filesystem (mirrors [`crate::duplicates::DuplicateContext`]).
pub struct DuplicateLookup<'a> {
    /// Facts about a manifest page by id; `None` when it cannot be read.
    pub page: &'a dyn Fn(&str) -> Option<PageFacts>,
}

/// Whether `decision` still applies to a candidate at `sha256` (mirrors [`Decision::applies_to`]
/// without requiring a residue entry).
fn already_decided(effective: &BTreeMap<String, Decision>, id: &str, sha256: &str) -> bool {
    effective.get(id).is_some_and(|d| d.applies_to(sha256))
}

/// Build the candidate list: undecided residue, then near-duplicate pairs' `duplicate` id not
/// already covered by residue or an active decision (SPEC §14.2).
pub fn candidates(
    residue: &[ResidueEntry],
    duplicates: &[DuplicatePair],
    effective: &BTreeMap<String, Decision>,
    lookup: &DuplicateLookup<'_>,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for entry in residue {
        if already_decided(effective, &entry.id, &entry.sha256) {
            continue;
        }
        seen.insert(entry.id.clone());
        out.push(Candidate {
            id: entry.id.clone(),
            sha256: entry.sha256.clone(),
            title: entry.title.clone(),
            excerpt: entry.excerpt.clone(),
            reason: entry.reason.as_str().to_string(),
            context: entry.context.clone(),
        });
    }
    for pair in duplicates {
        let id = &pair.duplicate;
        if seen.contains(id) {
            continue;
        }
        let Some(facts) = (lookup.page)(id) else {
            continue;
        };
        if already_decided(effective, id, &facts.sha256) {
            continue;
        }
        seen.insert(id.clone());
        out.push(Candidate {
            id: id.clone(),
            sha256: facts.sha256,
            title: facts.title,
            excerpt: facts.excerpt,
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

fn user_prompt(batch: &[Candidate]) -> String {
    serde_json::to_string_pretty(batch).unwrap_or_default()
}

/// Apply the `new_source` rule and stamp `by`/`at`; returns the decision and, when the rule
/// overrode the model, a warning describing it.
fn apply_rules(
    candidate: &Candidate,
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
    candidates: &[Candidate],
    batch_size: usize,
    at: &str,
) -> Result<Outcome, ClassifyError> {
    let batch_size = batch_size.max(1);
    let mut outcome = Outcome::default();
    for batch in candidates.chunks(batch_size) {
        let user = user_prompt(batch);
        let verdicts: Vec<ModelVerdict> = llm::chat(transport, config, SYSTEM_PROMPT, &user)?;
        let by_id: BTreeMap<&str, &Candidate> = batch.iter().map(|c| (c.id.as_str(), c)).collect();
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

    fn residue_entry(id: &str, reason: Reason) -> ResidueEntry {
        ResidueEntry {
            id: id.to_string(),
            source: id.split("::").next().unwrap().to_string(),
            path: id.split("::").nth(1).unwrap().to_string(),
            reason,
            sha256: "aa".repeat(32),
            title: "Title".to_string(),
            excerpt: "some excerpt text".to_string(),
            context: "sidebar".to_string(),
            url: String::new(),
            rule: None,
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
        let mut r1 = residue_entry("h::a.md", Reason::NotSelected);
        r1.sha256 = "aa".repeat(32);
        let r2 = residue_entry("h::b.md", Reason::NotSelected);
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
        let lookup = DuplicateLookup { page: &|_| None };
        let cands = candidates(&[r1, r2.clone()], &[], &decisions, &lookup);
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
        let page = |id: &str| -> Option<PageFacts> {
            (id == "h::b.md").then(|| PageFacts {
                title: "B".to_string(),
                excerpt: "excerpt".to_string(),
                sha256: "bb".repeat(32),
            })
        };
        let lookup = DuplicateLookup { page: &page };
        let cands = candidates(&[], &[pair], &BTreeMap::new(), &lookup);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].reason, "duplicate");
        assert!(cands[0].context.contains("h::a.md"));
        assert!(cands[0].context.contains("0.876"));
    }

    #[test]
    fn candidates_skip_a_duplicate_the_lookup_cannot_read() {
        let pair = DuplicatePair {
            kind: DuplicateKind::Mirror,
            similarity: 1.0,
            canonical: "h::a.md".to_string(),
            duplicate: "h::gone.md".to_string(),
            why: "priority".to_string(),
            suggested: Suggested::Exclude,
            canonical_url: String::new(),
            duplicate_url: String::new(),
        };
        let lookup = DuplicateLookup { page: &|_| None };
        let cands = candidates(&[], &[pair], &BTreeMap::new(), &lookup);
        assert!(cands.is_empty());
    }

    #[test]
    fn run_batches_at_the_given_size() {
        let cands: Vec<Candidate> = (0..5)
            .map(|i| Candidate {
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
        let candidate = Candidate {
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
        let candidate = Candidate {
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
        let candidate = Candidate {
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
