//! Pins `dense` and `hybrid` (SPEC §16.2, §16.3) on the golden corpus, scored with the
//! deterministic, network-free `FakeEmbedder` (SPEC §16.1's numbers-not-architecture point
//! applies here too: these pins exist so a change to the fusion or cosine-scoring code shows up
//! as a diff, not to claim anything about real embedding quality).
//!
//! Refresh with `UPDATE_GOLDEN=1 cargo test --test backend_dense_hybrid_golden` after an
//! intended change, and review the diff.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use pinakes::backend::BackendKind;
use pinakes::commands::{BackendEvalOptions, EmbedOptions, Paths, embed, eval_backend};
use pinakes::embed::Embedder;
use pinakes::embed::testing::FakeEmbedder;
use serde_json::{Value, json};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden")
}

fn pin(name: &str, actual: &Value) {
    let mut text = serde_json::to_string_pretty(actual).unwrap();
    text.push('\n');
    let expected_path = fixture().join(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&expected_path, &text).unwrap();
    }
    let expected = std::fs::read_to_string(&expected_path)
        .unwrap_or_else(|_| panic!("{} exists", expected_path.display()));
    assert_eq!(
        text, expected,
        "{name} changed; run `UPDATE_GOLDEN=1 cargo test --test backend_dense_hybrid_golden` if intended"
    );
}

#[test]
fn dense_and_hybrid_metrics_on_the_golden_corpus_are_pinned() {
    let paths = Paths::for_config(&fixture().join("pinakes.yaml"));
    let embeddings_dir = tempfile::tempdir().unwrap();
    let embedder: &dyn Embedder = &FakeEmbedder;

    // Build embeddings.bin/.json for the golden artifact with the fake embedder, over to a
    // scratch directory so this test never writes into the committed fixture.
    let embed_options = EmbedOptions {
        model: "fake".to_string(),
        batch: 8,
        out: Some(embeddings_dir.path().join("embeddings.bin")),
    };
    embed(&paths, &embed_options, embedder).expect("embed runs on the golden fixture");

    let embedder_rc: Rc<dyn Embedder> = Rc::new(FakeEmbedder);
    let common = BackendEvalOptions {
        embeddings: Some(embeddings_dir.path().join("embeddings.bin")),
        embedder: Some(embedder_rc),
        ..BackendEvalOptions::default()
    };

    let dense = eval_backend(
        &paths,
        &BackendEvalOptions {
            backend: BackendKind::Dense,
            ..common.clone()
        },
    )
    .expect("dense eval runs");
    pin(
        "expected-dense.json",
        &json!({
            "pages": dense.page_count,
            "searchable": dense.searchable_count,
            "k": dense.k,
            "eval": serde_json::to_value(&dense.summary).unwrap(),
        }),
    );
    assert_eq!(dense.summary.backend, "dense");
    assert_eq!(dense.page_count, 33);
    assert_eq!(dense.searchable_count, 30);

    let hybrid = eval_backend(
        &paths,
        &BackendEvalOptions {
            backend: BackendKind::Hybrid,
            ..common
        },
    )
    .expect("hybrid eval runs");
    pin(
        "expected-hybrid.json",
        &json!({
            "pages": hybrid.page_count,
            "searchable": hybrid.searchable_count,
            "k": hybrid.k,
            "eval": serde_json::to_value(&hybrid.summary).unwrap(),
        }),
    );
    assert_eq!(hybrid.summary.backend, "hybrid");
    assert_eq!(hybrid.page_count, 33);
    assert_eq!(hybrid.searchable_count, 30);
}
