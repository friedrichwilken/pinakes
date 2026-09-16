//! `pinakes eval` end to end through the binary: JSON on stdout or in `--json`, the table on
//! stderr, exit 2 when `--gate` fails (SPEC §4).

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
fn eval_writes_json_and_exits_2_when_the_gate_fails() {
    let dir = workspace();
    let root = dir.path();

    // No manifest, no meta.json, no config: the artifact is still measurable.
    let out = pinakes(root, &["eval", "--queries", "queries.jsonl"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let summary: serde_json::Value = serde_json::from_str(&stdout).expect("JSON on stdout");
    assert_eq!(summary["tuning"]["overall"]["recall@5"], 1.0);
    assert_eq!(
        summary["queries"][0]["top"][0],
        "handbook::docs/user/README.md"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("| tuning | overall | 1 | 1.000 | 1.000 | 1.000 |"),
        "{stderr}"
    );
    assert!(stderr.contains("1 pages, 1 searchable, k = 10"), "{stderr}");

    // --json writes the file and keeps stdout empty; a matching baseline passes the gate.
    let out = pinakes(
        root,
        &[
            "eval",
            "--queries",
            "queries.jsonl",
            "--json",
            "baseline.json",
        ],
    );
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert!(root.join("baseline.json").is_file());
    let out = pinakes(
        root,
        &[
            "eval",
            "--queries",
            "queries.jsonl",
            "--gate",
            "baseline.json",
        ],
    );
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("gate: tuning recall@5 1.000 → 1.000"));

    // A query that misses drops recall@5 from 1.0 to 0.0: beyond the default 0.05 tolerance.
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
            "--gate",
            "baseline.json",
        ],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("FAILED"));

    // Errors exit 1: an unknown page for --without, and a missing query file.
    let out = pinakes(
        root,
        &[
            "eval",
            "--queries",
            "queries.jsonl",
            "--without",
            "handbook::nope.md",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not in the corpus"));
    let out = pinakes(root, &["eval"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no query file"));
}
