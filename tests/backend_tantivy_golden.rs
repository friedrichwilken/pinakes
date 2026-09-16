//! Pins `eval --backend bm25-tantivy` on the golden corpus (SPEC §16.1, §18): the same fixture
//! as `tests/golden.rs`, scored by tantivy's own BM25 instead of the hand-rolled formula. The
//! plain `bm25` golden file (`tests/golden.rs`, `expected.json`) is untouched by this test.
//!
//! Refresh with `UPDATE_GOLDEN=1 cargo test --test backend_tantivy_golden` after an intended
//! change, and review the diff.

use std::path::{Path, PathBuf};

use pinakes::backend::BackendKind;
use pinakes::commands::{BackendEvalOptions, Paths, eval_backend};
use serde_json::{Value, json};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden")
}

#[test]
fn tantivy_backend_metrics_on_the_golden_corpus_are_pinned() {
    let paths = Paths::for_config(&fixture().join("pinakes.yaml"));
    let options = BackendEvalOptions {
        backend: BackendKind::Bm25Tantivy,
        ..BackendEvalOptions::default()
    };
    let outcome = eval_backend(&paths, &options).expect("eval --backend bm25-tantivy runs");

    let actual: Value = json!({
        "pages": outcome.page_count,
        "searchable": outcome.searchable_count,
        "k": outcome.k,
        "eval": serde_json::to_value(&outcome.summary).unwrap(),
    });
    let mut text = serde_json::to_string_pretty(&actual).unwrap();
    text.push('\n');

    let expected_path = fixture().join("expected-tantivy.json");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&expected_path, &text).unwrap();
    }
    let expected = std::fs::read_to_string(&expected_path).expect("expected-tantivy.json exists");
    assert_eq!(
        text, expected,
        "bm25-tantivy golden result changed; run `UPDATE_GOLDEN=1 cargo test --test backend_tantivy_golden` if intended"
    );

    // Same corpus shape as the plain bm25 golden test: only the scorer differs.
    assert_eq!(outcome.page_count, 33);
    assert_eq!(outcome.searchable_count, 30);
    assert_eq!(outcome.summary.tuning.overall.n, 12);
    assert_eq!(
        outcome.summary.holdout.as_ref().map(|h| h.overall.n),
        Some(2)
    );
    assert_eq!(outcome.summary.backend, "bm25-tantivy");
}
