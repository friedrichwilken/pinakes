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

/// A config that fixes `eval.backend` (SPEC §16.1) makes a bare `eval` measure that backend,
/// and `--backend` on the command line still overrides it.
#[test]
fn eval_takes_the_backend_from_the_config() {
    let dir = workspace();
    let root = dir.path();
    fs::write(
        root.join("pinakes.yaml"),
        "version: 1\nsources:\n  - name: handbook\n    repo: https://github.com/example-org/handbook.git\n    \
         ref: main\n    resolver:\n      type: glob\n      include: ['docs/**/*.md']\n\
         eval:\n  queries: queries.jsonl\n  backend: bm25-tantivy\n",
    )
    .unwrap();
    let run = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_pinakes"))
            .current_dir(root)
            .args(["--config", "pinakes.yaml", "eval"])
            .args(extra)
            .output()
            .expect("pinakes runs")
    };

    let out = run(&[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["backend"], "bm25-tantivy");

    let out = run(&["--backend", "bm25"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["backend"], "bm25", "the flag wins over the config");
}

/// `eval.compare` in the config turns a bare `eval` into a comparison, one result per backend.
#[test]
fn eval_takes_the_comparison_from_the_config() {
    let dir = workspace();
    let root = dir.path();
    fs::write(
        root.join("pinakes.yaml"),
        "version: 1\nsources:\n  - name: handbook\n    repo: https://github.com/example-org/handbook.git\n    \
         ref: main\n    resolver:\n      type: glob\n      include: ['docs/**/*.md']\n\
         eval:\n  queries: queries.jsonl\n  compare: [bm25, bm25-tantivy]\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_pinakes"))
        .current_dir(root)
        .args(["--config", "pinakes.yaml", "eval"])
        .output()
        .expect("pinakes runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["bm25"]["backend"], "bm25");
    assert_eq!(json["bm25-tantivy"]["backend"], "bm25-tantivy");
}

/// `--backend bm25`, given explicitly with no config, goes through the tagged backend path: the
/// JSON result carries `"backend":"bm25"` and the stderr counts line is bracketed `[bm25]` —
/// unlike the plain path, which has neither (see `eval_plain_has_no_backend_key_or_bracket_tag`
/// below and `eval_writes_json_and_exits_2_when_the_gate_fails` above).
#[test]
fn eval_backend_bm25_explicit_flag_tags_the_output() {
    let dir = workspace();
    let root = dir.path();
    let out = pinakes(
        root,
        &["eval", "--queries", "queries.jsonl", "--backend", "bm25"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["backend"], "bm25");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[bm25]: 1 pages, 1 searchable, k = 10"),
        "{stderr}"
    );
}

/// `--backend bm25 --gate BASELINE`: exit 0 and a `"gate [bm25]:"` line when it passes, exit 2
/// and `"gate [bm25]:"` plus `"FAILED"` when it doesn't — the tagged-path mirror of
/// `eval_writes_json_and_exits_2_when_the_gate_fails`'s plain-path gate coverage.
#[test]
fn eval_backend_bm25_gate_passes_and_fails() {
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

    let out = pinakes(
        root,
        &[
            "eval",
            "--queries",
            "queries.jsonl",
            "--backend",
            "bm25",
            "--gate",
            "baseline.json",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("gate [bm25]:"));

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
            "--backend",
            "bm25",
            "--gate",
            "baseline.json",
        ],
    );
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("gate [bm25]:"), "{stderr}");
    assert!(stderr.contains("FAILED"), "{stderr}");
}

/// A bare `--allow-stale` (no `--backend`) still routes through the tagged backend path, at
/// `bm25` — a quirk of the current dispatch predicate (`allow_stale` only means anything for
/// `dense`/`hybrid`), pinned so a refactor doesn't silently change it.
#[test]
fn eval_allow_stale_alone_goes_through_the_backend_path() {
    let dir = workspace();
    let root = dir.path();
    let out = pinakes(
        root,
        &["eval", "--queries", "queries.jsonl", "--allow-stale"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["backend"], "bm25");
    assert!(String::from_utf8_lossy(&out.stderr).contains("[bm25]"));
}

/// The plain path (no backend-selecting flag at all) never tags its output — the mirror image
/// of the `--backend`/`--allow-stale` pins above.
#[test]
fn eval_plain_has_no_backend_key_or_bracket_tag() {
    let dir = workspace();
    let root = dir.path();
    let out = pinakes(root, &["eval", "--queries", "queries.jsonl"]);
    assert!(out.status.success());
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(json.get("backend").is_none(), "{json}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains('['), "{stderr}");
}

/// A configured `eval.compare` only turns a *bare* `eval` into a comparison; `--backend` on the
/// command line still measures just that one backend, ignoring the configured comparison.
#[test]
fn eval_config_compare_is_ignored_when_backend_flag_given() {
    let dir = workspace();
    let root = dir.path();
    fs::write(
        root.join("pinakes.yaml"),
        "version: 1\nsources:\n  - name: handbook\n    repo: https://github.com/example-org/handbook.git\n    \
         ref: main\n    resolver:\n      type: glob\n      include: ['docs/**/*.md']\n\
         eval:\n  queries: queries.jsonl\n  compare: [bm25, bm25-tantivy]\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_pinakes"))
        .current_dir(root)
        .args(["--config", "pinakes.yaml", "eval", "--backend", "bm25"])
        .output()
        .expect("pinakes runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["backend"], "bm25");
    assert!(json.get("bm25").is_none(), "{json}");
    assert!(json.get("bm25-tantivy").is_none(), "{json}");
}

/// `eval.embeddings`, when relative, resolves against the config file's directory, not the
/// process's cwd. Pinned through the error `dense` raises when the resolved file is missing,
/// since exercising a real read needs no network: the config lives in `sub/`, so a correct
/// resolution reports `sub/custom/embeddings.json`, not `custom/embeddings.json` off the cwd.
#[test]
fn eval_config_relative_embeddings_path_resolves_against_config_dir() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let sub = root.join("sub");
    let page = sub.join("artifact/handbook/docs/user/README.md");
    fs::create_dir_all(page.parent().unwrap()).unwrap();
    fs::write(&page, "# Storage\n\nEnable upload caching with a label.\n").unwrap();
    fs::write(
        sub.join("queries.jsonl"),
        "{\"id\": \"q\", \"kind\": \"howto\", \"query\": \"enable upload caching\", \
         \"expected\": [\"handbook/docs/user\"]}\n",
    )
    .unwrap();
    fs::write(
        sub.join("pinakes.yaml"),
        "version: 1\nsources:\n  - name: handbook\n    repo: https://github.com/example-org/handbook.git\n    \
         ref: main\n    resolver:\n      type: glob\n      include: ['docs/**/*.md']\n\
         eval:\n  queries: queries.jsonl\n  embeddings: custom/embeddings.bin\n",
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_pinakes"))
        .current_dir(root)
        .env("PINAKES_EMBED_URL", "http://127.0.0.1:1")
        .args(["--config", "sub/pinakes.yaml", "eval", "--backend", "dense"])
        .output()
        .expect("pinakes runs");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    // `--config sub/pinakes.yaml` is relative, so the resolved path stays relative too (joined
    // against the config's directory, "sub", not the process's cwd, which would omit "sub/").
    assert!(stderr.contains("sub/custom/embeddings.json"), "{stderr}");
}

/// `--backend dense` needs `PINAKES_EMBED_URL`; without it, the exact original wording (about to
/// move from main.rs's `embedder_from_env` into the library) must survive byte for byte.
#[test]
fn eval_backend_dense_without_embed_url_errors() {
    let dir = workspace();
    let root = dir.path();
    let out = Command::new(env!("CARGO_BIN_EXE_pinakes"))
        .current_dir(root)
        .env_remove("PINAKES_EMBED_URL")
        .args([
            "--config",
            "nonexistent.yaml",
            "eval",
            "--queries",
            "queries.jsonl",
            "--backend",
            "dense",
        ])
        .output()
        .expect("pinakes runs");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("PINAKES_EMBED_URL is not set (needed for --backend dense/hybrid)"),
        "{stderr}"
    );
}
