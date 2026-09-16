//! The golden corpus check (SPEC §7.3): `eval` on the synthetic artifact under
//! `tests/fixtures/golden` must yield exactly the pinned result.
//!
//! The fixture is thirty small pages across three sources with different priorities, sidebar
//! titles in each `meta.json`, mirrored pages across sources, one frontmatter-only title, one
//! page with H2/H3 sections and a `_residue` directory, plus a fourth source (`schemas`) whose
//! one page is a CRD rendered through the built-in `openapi` renderer (SPEC §10.2), pinning the
//! identifier-compound tokeniser rule (SPEC §10.3), and a near-duplicate pair (SPEC §11) for
//! `tests/duplicates.rs`; `queries.jsonl` holds fourteen queries, two of them held out. The
//! expected metrics are whatever the implementation yields, pinned in
//! `expected.json`. After an intended change to the index, refresh the file with
//! `UPDATE_GOLDEN=1 cargo test --test golden` and review the diff.

use std::path::{Path, PathBuf};

use pinakes::commands::{EvalOptions, EvalOutcome, Paths, eval};
use serde_json::{Value, json};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden")
}

/// The residue page `--with` adds in the second run.
const WITH_RESIDUE: &str = "cookbook::docs/recipes/draft-plugin.md";

fn outcome_json(outcome: &EvalOutcome) -> Value {
    json!({
        "pages": outcome.page_count,
        "searchable": outcome.searchable_count,
        "k": outcome.k,
        "eval": serde_json::to_value(&outcome.summary).unwrap(),
    })
}

#[test]
fn golden_corpus_metrics_are_pinned() {
    let paths = Paths::for_config(&fixture().join("pinakes.yaml"));
    let plain = eval(&paths, &EvalOptions::default()).expect("eval runs on the fixture");
    let with = eval(
        &paths,
        &EvalOptions {
            with: vec![WITH_RESIDUE.to_string()],
            ..EvalOptions::default()
        },
    )
    .expect("eval --with runs on the fixture");
    let delta = with.delta.as_ref().expect("--with yields a delta");
    let actual = json!({
        "plain": outcome_json(&plain),
        "with_residue": {
            "page": WITH_RESIDUE,
            "changed_queries": delta.changed.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            "result": outcome_json(&with),
        },
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

    // Structural facts about the fixture that the pinned numbers rest on. Iteration 2 added a
    // near-duplicate pair (SPEC §11) for `tests/duplicates.rs` to find: a distinctly titled copy
    // of `handbook::docs/concepts/notifications.md` in the lower-priority `cookbook` source, so
    // it is a normal extra page here, not a title mirror.
    assert_eq!(
        plain.page_count, 33,
        "thirty pages, one rendered CRD page, and the near-duplicate pair"
    );
    assert_eq!(
        plain.searchable_count, 30,
        "three mirrored pages in lower-priority sources are left out"
    );
    assert_eq!(plain.summary.tuning.overall.n, 12);
    assert_eq!(plain.summary.holdout.as_ref().map(|h| h.overall.n), Some(2));
    assert_eq!(with.page_count, 34);
    assert_eq!(delta.changed.len(), 1, "only the plugin query changes rank");
    assert_eq!(delta.changed[0].id, "plugin");
}
