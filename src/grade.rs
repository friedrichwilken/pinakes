//! `pinakes grade`: replay distinct trail queries against a backend and ask the model to grade
//! each candidate 0-3 for relevance (SPEC §15.2).
//!
//! [`bm25_search`] is the "small function you own" the spec asks for: a thin wrapper around the
//! built-in [`Index`] (SPEC §5) that stands in for SPEC §16.1's `Backend` trait, which does not
//! exist yet. Once it lands, this function's body becomes an adapter over it (`--backend` picks
//! the implementation); `grade`'s contract - one BM25-shaped `search(query, k) -> Vec<Hit>` call
//! per distinct query - does not change.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::index::{Hit, Index, IndexError};
use crate::jsonl::{self, KeyOrder};
use crate::llm::{self, ChatError, ChatTransport, LlmConfig};
use crate::residue;
use crate::trail::TrailEntry;

/// Default `--k` (SPEC §15.2).
pub const DEFAULT_K: usize = 20;
/// Tokens of page content shown to the model per candidate.
const EXCERPT_TOKENS: usize = 300;
/// Highest grade the model may give.
const MAX_GRADE: u8 = 3;

/// Errors raised while grading.
#[derive(Debug, Error)]
pub enum GradeError {
    /// Talking to the model failed.
    #[error(transparent)]
    Llm(#[from] ChatError),
    /// Searching the backend failed.
    #[error(transparent)]
    Index(#[from] IndexError),
    /// `--backend` named something other than `bm25`.
    #[error(
        "unknown backend {0:?}: only \"bm25\" is available until SPEC §16's backend trait lands"
    )]
    UnknownBackend(String),
}

/// The only backend name `grade` accepts today (SPEC §16.1 has not landed in this crate yet).
pub const BM25_BACKEND: &str = "bm25";

/// One graded row (SPEC §15.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GradedRow {
    /// The query text.
    pub query: String,
    /// `<source>::<path>`.
    pub id: String,
    /// Relevance grade, 0 (irrelevant) to 3 (fully relevant).
    pub grade: u8,
    /// The model that produced the grade.
    pub model: String,
    /// RFC 3339 UTC time the grade was produced.
    pub at: String,
}

/// Serialise rows as JSONL with sorted keys, one object per line.
pub fn to_jsonl(rows: &[GradedRow]) -> Result<String, serde_json::Error> {
    jsonl::to_string(rows, KeyOrder::Sorted)
}

/// Distinct `query` values from a trail, in first-occurrence order.
pub fn distinct_queries(entries: &[TrailEntry]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for entry in entries {
        if seen.insert(entry.query.clone()) {
            out.push(entry.query.clone());
        }
    }
    out
}

/// Check that `backend` is one `grade` can use today.
pub fn check_backend(backend: &str) -> Result<(), GradeError> {
    if backend == BM25_BACKEND {
        Ok(())
    } else {
        Err(GradeError::UnknownBackend(backend.to_string()))
    }
}

/// The built-in BM25 search `grade` uses for the `bm25` backend (see the module docs for why
/// this exists instead of a `Backend` trait call).
pub fn bm25_search(index: &Index, query: &str, k: usize) -> Result<Vec<Hit>, GradeError> {
    Ok(index.search(query, k, None)?)
}

/// One candidate shown to the model for grading.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct GradeCandidate {
    id: String,
    title: String,
    excerpt: String,
}

/// Turn search hits into grading candidates, looking their page up in `index`; a hit whose page
/// cannot be found (should not happen: hits come from this same index) is skipped.
fn candidates_for(index: &Index, hits: &[Hit]) -> Vec<GradeCandidate> {
    hits.iter()
        .filter_map(|hit| {
            let page = index.page(&hit.page_id)?;
            Some(GradeCandidate {
                id: hit.page_id.clone(),
                title: page.title.clone(),
                excerpt: residue::excerpt(&page.content, EXCERPT_TOKENS),
            })
        })
        .collect()
}

#[derive(Debug, Clone, Deserialize)]
struct ModelGrade {
    id: String,
    grade: u8,
}

const SYSTEM_PROMPT: &str = "You are grading search results for a documentation retrieval \
system. You will be given a query and a JSON array of candidate pages, each with an id, title \
and excerpt. For every candidate, grade how relevant it is to the query from 0 (irrelevant) to \
3 (fully relevant and directly answers the query). Respond with a JSON array only - no prose, \
no markdown code fences, one object per candidate id you were given, in this exact shape: \
[{\"id\": \"...\", \"grade\": 0}]";

fn user_prompt(query: &str, candidates: &[GradeCandidate]) -> String {
    let body = serde_json::json!({"query": query, "candidates": candidates});
    serde_json::to_string_pretty(&body).unwrap_or_default()
}

/// Ask the model to grade every hit of one query, dropping any id it returns that was not among
/// the candidates it was given.
pub fn grade_query(
    transport: &dyn ChatTransport,
    config: &LlmConfig,
    index: &Index,
    query: &str,
    hits: &[Hit],
    at: &str,
) -> Result<Vec<GradedRow>, GradeError> {
    let candidates = candidates_for(index, hits);
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let known: BTreeSet<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
    let user = user_prompt(query, &candidates);
    let grades: Vec<ModelGrade> = llm::chat(transport, config, SYSTEM_PROMPT, &user)?;
    Ok(grades
        .into_iter()
        .filter(|g| known.contains(g.id.as_str()))
        .map(|g| GradedRow {
            query: query.to_string(),
            id: g.id,
            grade: g.grade.min(MAX_GRADE),
            model: config.model.clone(),
            at: at.to_string(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Page;
    use crate::llm::testing::{Scripted, ScriptedTransport, completion};

    fn page(id: &str, source: &str, path: &str, title: &str, content: &str) -> Page {
        Page {
            id: id.to_string(),
            source: source.to_string(),
            path: path.to_string(),
            repo: source.to_string(),
            module: source.to_string(),
            title: title.to_string(),
            heading: title.to_string(),
            doc_type: String::new(),
            section: String::new(),
            priority: crate::config::DEFAULT_PRIORITY,
            content: content.to_string(),
            mirror_of: None,
        }
    }

    fn index() -> Index {
        Index::from_pages(vec![
            page(
                "handbook::docs/caching.md",
                "handbook",
                "docs/caching.md",
                "Caching",
                "Enable upload caching with a label on the bucket.",
            ),
            page(
                "handbook::docs/quotas.md",
                "handbook",
                "docs/quotas.md",
                "Quotas",
                "Quota limits apply per project.",
            ),
        ])
        .unwrap()
    }

    fn config() -> LlmConfig {
        LlmConfig {
            url: "https://example.test".to_string(),
            key: None,
            model: "grader-model".to_string(),
        }
    }

    #[test]
    fn distinct_queries_preserves_first_occurrence_order() {
        let entries = vec![
            TrailEntry {
                at: "t".to_string(),
                query: "b".to_string(),
                retrieved: vec![],
                ranks: vec![],
                cited: vec![],
                outcome: crate::trail::Outcome::Unknown,
                session: String::new(),
            },
            TrailEntry {
                query: "a".to_string(),
                ..entries_base()
            },
            TrailEntry {
                query: "b".to_string(),
                ..entries_base()
            },
        ];
        assert_eq!(distinct_queries(&entries), ["b", "a"]);
    }

    fn entries_base() -> TrailEntry {
        TrailEntry {
            at: "t".to_string(),
            query: String::new(),
            retrieved: vec![],
            ranks: vec![],
            cited: vec![],
            outcome: crate::trail::Outcome::Unknown,
            session: String::new(),
        }
    }

    #[test]
    fn check_backend_accepts_only_bm25() {
        assert!(check_backend("bm25").is_ok());
        let err = check_backend("dense").unwrap_err();
        assert!(matches!(err, GradeError::UnknownBackend(b) if b == "dense"));
    }

    #[test]
    fn bm25_search_returns_hits_from_the_index() {
        let index = index();
        let hits = bm25_search(&index, "enable caching", 10).unwrap();
        assert_eq!(hits[0].page_id, "handbook::docs/caching.md");
    }

    #[test]
    fn grade_query_asks_the_model_and_drops_unknown_ids() {
        let index = index();
        let hits = bm25_search(&index, "caching", 10).unwrap();
        let reply = completion(
            &serde_json::to_string(&serde_json::json!([
                {"id": "handbook::docs/caching.md", "grade": 3},
                {"id": "handbook::docs/not-a-candidate.md", "grade": 2},
            ]))
            .unwrap(),
        );
        let transport = ScriptedTransport::new(vec![Scripted::Ok(reply)]);
        let rows = grade_query(&transport, &config(), &index, "caching", &hits, "t").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "handbook::docs/caching.md");
        assert_eq!(rows[0].grade, 3);
        assert_eq!(rows[0].model, "grader-model");
    }

    #[test]
    fn grade_query_clamps_an_out_of_range_grade() {
        let index = index();
        let hits = bm25_search(&index, "caching", 10).unwrap();
        let reply = completion(
            &serde_json::to_string(&serde_json::json!([
                {"id": "handbook::docs/caching.md", "grade": 9},
            ]))
            .unwrap(),
        );
        let transport = ScriptedTransport::new(vec![Scripted::Ok(reply)]);
        let rows = grade_query(&transport, &config(), &index, "caching", &hits, "t").unwrap();
        assert_eq!(rows[0].grade, MAX_GRADE);
    }

    #[test]
    fn grade_query_with_no_hits_never_calls_the_model() {
        let index = index();
        let transport = ScriptedTransport::new(vec![]);
        let rows = grade_query(
            &transport,
            &config(),
            &index,
            "nothing matches at all",
            &[],
            "t",
        )
        .unwrap();
        assert!(rows.is_empty());
        assert!(transport.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn to_jsonl_round_trips() {
        let rows = vec![GradedRow {
            query: "q".to_string(),
            id: "handbook::docs/caching.md".to_string(),
            grade: 2,
            model: "m".to_string(),
            at: "t".to_string(),
        }];
        let text = to_jsonl(&rows).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.contains("\"grade\":2"));
    }
}
