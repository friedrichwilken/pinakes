//! `grade` over the golden fixture with a scripted grader (SPEC §15.2).

use std::path::{Path, PathBuf};

use pinakes::config::Config;
use pinakes::grade::{self, GradedRow};
use pinakes::index::{Index, Priorities};
use pinakes::llm::LlmConfig;
use pinakes::llm::testing::{Scripted, ScriptedTransport, completion};
use pinakes::trail;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden")
}

const TRAIL: &str = "\
{\"at\": \"t\", \"query\": \"how do I install the service\", \
 \"retrieved\": [\"handbook::docs/install.md\"], \"cited\": [\"handbook::docs/install.md\"]}
{\"at\": \"t\", \"query\": \"how do I install the service\", \
 \"retrieved\": [\"handbook::docs/install.md\"], \"cited\": []}
{\"at\": \"t\", \"query\": \"rotate the signing keys\", \"retrieved\": [], \"cited\": []}
";

fn config() -> LlmConfig {
    LlmConfig {
        url: "https://example.test".to_string(),
        key: None,
        model: "grader-model".to_string(),
    }
}

#[test]
fn grade_replays_distinct_queries_and_writes_query_id_grade_model_at() {
    let artifact = fixture().join("artifact");
    let config_file = Config::load(&fixture().join("pinakes.yaml")).unwrap();
    let priorities = Priorities::from_config(&config_file);
    let index = Index::build(&artifact, &priorities).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let trail_path = dir.path().join("trail.jsonl");
    std::fs::write(&trail_path, TRAIL).unwrap();
    let entries = trail::read_jsonl(&trail_path).unwrap();
    let queries = grade::distinct_queries(&entries);
    // The repeated "install" query collapses to one distinct query.
    assert_eq!(
        queries,
        ["how do I install the service", "rotate the signing keys"]
    );

    // One scripted reply per distinct query that has any BM25 candidates at all.
    let install_reply = completion(
        &serde_json::to_string(&serde_json::json!([
            {"id": "handbook::docs/install.md", "grade": 3},
        ]))
        .unwrap(),
    );
    let rotate_reply = completion(
        &serde_json::to_string(&serde_json::json!([
            {"id": "guides::docs/rotate-keys.md", "grade": 3},
        ]))
        .unwrap(),
    );
    let transport = ScriptedTransport::new(vec![
        Scripted::Ok(install_reply),
        Scripted::Ok(rotate_reply),
    ]);

    let llm_config = config();
    let mut rows: Vec<GradedRow> = Vec::new();
    for query in &queries {
        let hits = grade::bm25_search(&index, query, grade::DEFAULT_K).unwrap();
        rows.extend(
            grade::grade_query(&transport, &llm_config, &index, query, &hits, "t").unwrap(),
        );
    }

    // Exactly one model call per distinct query, not per trail line.
    assert_eq!(transport.requests.lock().unwrap().len(), 2);
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .any(|r| r.query == "how do I install the service"
                && r.id == "handbook::docs/install.md"
                && r.grade == 3
                && r.model == "grader-model"
                && r.at == "t")
    );
    assert!(
        rows.iter()
            .any(|r| r.query == "rotate the signing keys" && r.id == "guides::docs/rotate-keys.md")
    );

    let jsonl = grade::to_jsonl(&rows).unwrap();
    assert_eq!(jsonl.lines().count(), 2);
}
