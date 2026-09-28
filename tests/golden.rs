//! The golden corpus check (SPEC §7.3): the built-in index over the synthetic artifact under
//! `tests/fixtures/golden` must return exactly the pinned pages for every query.
//!
//! The fixture is thirty small pages across three sources with different priorities, sidebar
//! titles in each `meta.json`, mirrored pages across sources, one frontmatter-only title, one
//! page with H2/H3 sections and a `_residue` directory, plus a fourth source (`schemas`) whose
//! one page is a CRD rendered through the built-in `openapi` renderer (SPEC §10.2), pinning the
//! identifier-compound tokeniser rule (SPEC §10.3), and a near-duplicate pair (SPEC §11) for
//! `tests/duplicates.rs`. `queries.jsonl` holds fourteen queries; their expected pages and the
//! metrics computed from them belong to `kanon`, so this test pins only what the index
//! returns: the ids of the top ten pages per query, in `expected.json`. After an intended change
//! to the index, refresh the file with `UPDATE_GOLDEN=1 cargo test --test golden` and review
//! the diff.

use std::path::{Path, PathBuf};

use pinakes::config::Config;
use pinakes::index::{Index, Priorities};
use serde_json::{Value, json};

/// Result list length the pin records.
const K: usize = 10;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden")
}

#[test]
fn golden_corpus_results_are_pinned() {
    let config = Config::load(&fixture().join("pinakes.yaml")).expect("the fixture config loads");
    let index = Index::build(
        &fixture().join("artifact"),
        &Priorities::from_config(&config),
    )
    .expect("the fixture artifact indexes");
    let queries = std::fs::read_to_string(fixture().join("queries.jsonl")).unwrap();
    let mut results = Vec::new();
    for line in queries.lines().filter(|l| !l.trim().is_empty()) {
        let query: Value = serde_json::from_str(line).unwrap();
        let text = query["query"].as_str().expect("every query has a text");
        let top: Vec<String> = index
            .search(text, K, None)
            .unwrap()
            .into_iter()
            .map(|hit| hit.page_id)
            .collect();
        results.push(json!({ "id": query["id"], "top": top }));
    }
    let actual = json!({
        "pages": index.page_count(),
        "searchable": index.searchable_count(),
        "k": K,
        "queries": results,
    });
    let mut text = serde_json::to_string_pretty(&actual).unwrap();
    text.push('\n');

    let expected_path = fixture().join("expected.json");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&expected_path, &text).unwrap();
    }
    let expected = std::fs::read_to_string(&expected_path).expect("expected.json exists");
    assert_eq!(
        text, expected,
        "golden result changed; run `UPDATE_GOLDEN=1 cargo test --test golden` if intended"
    );

    // Structural facts about the fixture that the pinned lists rest on: a near-duplicate pair
    // (SPEC §11) for `tests/duplicates.rs` to find is a normal extra page here, not a title
    // mirror.
    assert_eq!(
        index.page_count(),
        33,
        "thirty pages, one rendered CRD page, and the near-duplicate pair"
    );
    assert_eq!(
        index.searchable_count(),
        30,
        "three mirrored pages in lower-priority sources are left out"
    );
    assert_eq!(results.len(), 14);
}
