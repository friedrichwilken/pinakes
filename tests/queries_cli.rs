//! `pinakes queries add` and `pinakes queries check` end to end through the binary: appending
//! rows, rejecting unknown expected ids, and exit 4 on a `check` failure (SPEC §14.3).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use pinakes::manifest::{Manifest, ManifestSource, PageEntry, SelectedBy};

fn pinakes(dir: &Path, config: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_pinakes"))
        .current_dir(dir)
        .arg("--config")
        .arg(config)
        .args(args)
        .output()
        .expect("pinakes runs")
}

/// A workspace with a committed `manifest.json` (two pages) but no `pinakes.yaml`.
fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut manifest = Manifest::new("2026-09-16T12:00:00Z".to_string());
    let mut pages = BTreeMap::new();
    for path in ["docs/a.md", "docs/b.md"] {
        pages.insert(
            path.to_string(),
            PageEntry {
                sha256: "aa".repeat(32),
                title: path.to_string(),
                doc_type: "howto".to_string(),
                section: String::new(),
                selected_by: SelectedBy::Include,
                rendered_from: None,
            },
        );
    }
    manifest.sources.insert(
        "handbook".to_string(),
        ManifestSource {
            repo: "o/handbook".to_string(),
            repo_url: "https://github.com/o/handbook.git".to_string(),
            git_ref: "main".to_string(),
            commit: "a".repeat(40),
            archived: Some(false),
            resolver: "glob".to_string(),
            unrendered: Vec::new(),
            pages,
            residue: vec![],
            unresolved: vec![],
            render: None,
        },
    );
    manifest.save(&dir.path().join("manifest.json")).unwrap();
    dir
}

#[test]
fn add_normalises_expected_ids_and_rejects_unknown_ones() {
    let dir = workspace();
    let root = dir.path();

    let out = pinakes(
        root,
        "nonexistent.yaml",
        &[
            "queries",
            "add",
            "--id",
            "a",
            "--query",
            "what is a",
            "--expected",
            "handbook/docs/a.md",
            "--kind",
            "howto",
            "--queries",
            "queries.jsonl",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = fs::read_to_string(root.join("queries.jsonl")).unwrap();
    assert_eq!(text.lines().count(), 1);
    let row: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(row["expected"][0], "handbook::docs/a.md");

    // An unknown expected id exits 1 and nothing is appended.
    let out = pinakes(
        root,
        "nonexistent.yaml",
        &[
            "queries",
            "add",
            "--id",
            "bad",
            "--query",
            "x",
            "--expected",
            "handbook::missing.md",
            "--queries",
            "queries.jsonl",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("matches no page"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(root.join("queries.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1,
        "rejected row is not appended"
    );

    // A directory prefix in the legacy form is normalised with a trailing slash.
    let out = pinakes(
        root,
        "nonexistent.yaml",
        &[
            "queries",
            "add",
            "--id",
            "b",
            "--query",
            "what is b",
            "--expected",
            "handbook/docs",
            "--holdout",
            "--queries",
            "queries.jsonl",
        ],
    );
    assert!(out.status.success());
    let lines: Vec<String> = fs::read_to_string(root.join("queries.jsonl"))
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines.len(), 2);
    let row: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
    assert_eq!(row["expected"][0], "handbook::docs/");
    assert_eq!(row["holdout"], true);
}

#[test]
fn check_exits_4_on_an_unmet_holdout_share_and_0_once_it_is_met() {
    let dir = workspace();
    let root = dir.path();
    fs::write(
        root.join("pinakes.yaml"),
        "version: 1\nsources:\n  - name: handbook\n    repo: https://github.com/o/handbook.git\n    \
         ref: main\n    resolver:\n      type: glob\n      include: ['**/*.md']\n\
         eval:\n  queries: queries.jsonl\n  holdout_min: 0.6\n",
    )
    .unwrap();
    fs::write(
        root.join("queries.jsonl"),
        "{\"id\": \"a\", \"query\": \"a\", \"expected\": [\"handbook::docs/a.md\"]}\n\
         {\"id\": \"b\", \"query\": \"b\", \"expected\": [\"handbook::docs/b.md\"], \"holdout\": true}\n",
    )
    .unwrap();

    // 1 of 2 rows held out (0.5) is below the configured minimum (0.6).
    let out = pinakes(root, "pinakes.yaml", &["queries", "check"]);
    assert_eq!(out.status.code(), Some(4));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("held-out share: 0.500 (minimum 0.600)"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The default minimum (0.2, no config) is met by the same file.
    let out = pinakes(
        root,
        "nonexistent.yaml",
        &["queries", "check", "--queries", "queries.jsonl"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("ok"));

    // An unknown expected id and a duplicate query id both fail the check (exit 4).
    fs::write(
        root.join("queries.jsonl"),
        "{\"id\": \"a\", \"query\": \"a\", \"expected\": [\"handbook::missing.md\"]}\n\
         {\"id\": \"a\", \"query\": \"a again\", \"expected\": [\"handbook::docs/b.md\"], \"holdout\": true}\n",
    )
    .unwrap();
    let out = pinakes(
        root,
        "nonexistent.yaml",
        &["queries", "check", "--queries", "queries.jsonl"],
    );
    assert_eq!(out.status.code(), Some(4));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown: a: expected \"handbook::missing.md\""),
        "{stderr}"
    );
    assert!(stderr.contains("duplicate: a"), "{stderr}");

    // No query file at all and no config: the generic "no query file" error, exit 1.
    let out = pinakes(root, "nonexistent.yaml", &["queries", "check"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no query file"));
}
