//! `pinakes eval --compare a,b,c` end to end through the binary (SPEC §16.1): one table per
//! backend on stderr, one combined JSON on stdout or in `--json`.

use std::fs;
use std::path::Path;
use std::process::Command;

fn pinakes(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_pinakes"))
        .current_dir(dir)
        .arg("--config")
        .arg("nonexistent.yaml")
        .args(args)
        .output()
        .expect("pinakes runs")
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let page = dir.path().join("artifact/handbook/docs/user/README.md");
    fs::create_dir_all(page.parent().unwrap()).unwrap();
    fs::write(&page, "# Storage\n\nEnable upload caching with a label.\n").unwrap();
    fs::write(
        dir.path().join("queries.jsonl"),
        "{\"id\": \"q\", \"kind\": \"howto\", \"query\": \"enable upload caching\", \
         \"expected\": [\"handbook/docs/user\"]}\n",
    )
    .unwrap();
    dir
}

#[test]
fn compare_prints_one_table_per_backend_and_one_combined_json() {
    let dir = workspace();
    let root = dir.path();

    let out = pinakes(
        root,
        &[
            "eval",
            "--queries",
            "queries.jsonl",
            "--compare",
            "bm25,bm25-tantivy",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("[bm25]"), "{stderr}");
    assert!(stderr.contains("[bm25-tantivy]"), "{stderr}");
    assert!(
        stderr.matches("| tuning | overall |").count() == 2,
        "one table per backend: {stderr}"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined: serde_json::Value =
        serde_json::from_str(&stdout).expect("combined JSON on stdout");
    assert_eq!(combined["bm25"]["backend"], "bm25");
    assert_eq!(combined["bm25-tantivy"]["backend"], "bm25-tantivy");
    assert_eq!(combined["bm25"]["tuning"]["overall"]["recall@5"], 1.0);
    assert_eq!(
        combined["bm25-tantivy"]["tuning"]["overall"]["recall@5"],
        1.0
    );

    // --json writes the combined file instead, and stdout stays empty.
    let out = pinakes(
        root,
        &[
            "eval",
            "--queries",
            "queries.jsonl",
            "--compare",
            "bm25,bm25-tantivy",
            "--json",
            "compare.json",
        ],
    );
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    let text = fs::read_to_string(root.join("compare.json")).unwrap();
    let combined: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(combined.get("bm25").is_some() && combined.get("bm25-tantivy").is_some());

    // An unknown backend name is an error, exit 1.
    let out = pinakes(
        root,
        &["eval", "--queries", "queries.jsonl", "--compare", "nope"],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown backend"));
}

/// `--compare a,b --gate BASELINE`: a failing gate on either backend exits 2, same as the
/// tagged single-backend path (`eval_cli.rs::eval_backend_bm25_gate_passes_and_fails`).
#[test]
fn compare_gate_failure_exits_2() {
    let dir = workspace();
    let root = dir.path();

    let out = pinakes(
        root,
        &[
            "eval",
            "--queries",
            "queries.jsonl",
            "--backend",
            "bm25",
            "--json",
            "baseline.json",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    fs::write(
        root.join("queries.jsonl"),
        "{\"id\": \"q\", \"kind\": \"howto\", \"query\": \"nothing here\", \
         \"expected\": [\"handbook/docs/user\"]}\n",
    )
    .unwrap();
    let out = pinakes(
        root,
        &[
            "eval",
            "--queries",
            "queries.jsonl",
            "--compare",
            "bm25,bm25-tantivy",
            "--gate",
            "baseline.json",
        ],
    );
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("gate [bm25]:"), "{stderr}");
    assert!(stderr.contains("gate [bm25-tantivy]:"), "{stderr}");
    assert!(stderr.contains("FAILED"), "{stderr}");
}
