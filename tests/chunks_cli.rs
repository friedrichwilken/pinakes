//! `pinakes chunks` end to end through the binary (SPEC §2.9): one JSON object per retrieval
//! unit of `tests/fixtures/golden`, on stdout or in `--out`, with the summary on stderr.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden")
}

fn pinakes(config: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_pinakes"))
        .arg("--config")
        .arg(config)
        .args(args)
        .output()
        .expect("pinakes runs")
}

/// The first two lines over the golden fixture: `cookbook` sorts first, both pages are a single
/// intro unit, and `text` is the navigation title, a blank line and the cleaned page content.
const FIRST_LINES: [&str; 2] = [
    "{\"heading\":\"\",\"id\":\"cookbook::docs/README.md#0\",\"ordinal\":0,\
     \"page\":\"cookbook::docs/README.md\",\
     \"sha256\":\"69e6b75c045b7504651d3ab08c4cbf608aa776b7ee656bb2b424207926c4b925\",\
     \"text\":\"Cookbook\\n\\n# Cookbook\\n\\nCommunity recipes for the service. \
     Each recipe is a short, tested how-to.\"}",
    "{\"heading\":\"\",\"id\":\"cookbook::docs/recipes/cron-jobs.md#0\",\"ordinal\":0,\
     \"page\":\"cookbook::docs/recipes/cron-jobs.md\",\
     \"sha256\":\"8a4e8d14d6bdcd523d93e0529b6dae04c5608a9ebbf7fe5452ec72b485eb5369\",\
     \"text\":\"Scheduled Jobs\\n\\n# Scheduled jobs\\n\\nRun `service backup create` from cron \
     every night and prune snapshots older\\nthan thirty days.\"}",
];

#[test]
fn chunks_writes_one_line_per_unit_of_the_golden_corpus() {
    let out = pinakes(&fixture().join("pinakes.yaml"), &["chunks"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    // Pinned with tests/golden.rs: 33 pages, 30 searchable, and 52 units over them (the
    // units `embed` would embed and `eval` scores).
    assert_eq!(lines.len(), 52);
    assert_eq!(lines[0], FIRST_LINES[0]);
    assert_eq!(lines[1], FIRST_LINES[1]);
    assert!(stdout.ends_with('\n'));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.trim(), "52 chunks from 30 pages (3 mirrors skipped)");

    // Every line parses, ids are page#ordinal, and no mirror page is present.
    let mut previous: Option<(String, u64)> = None;
    for line in &lines {
        let value: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
        let page = value["page"].as_str().unwrap();
        let ordinal = value["ordinal"].as_u64().unwrap();
        assert_eq!(value["id"], format!("{page}#{ordinal}"));
        let expected = match &previous {
            Some((last, n)) if last == page => n + 1,
            _ => 0,
        };
        assert_eq!(ordinal, expected, "{line}");
        assert_eq!(value["sha256"].as_str().unwrap().len(), 64);
        previous = Some((page.to_string(), ordinal));
    }
    let pages: std::collections::BTreeSet<&str> = lines
        .iter()
        .filter_map(|line| line.split("\"page\":\"").nth(1))
        .filter_map(|rest| rest.split('"').next())
        .collect();
    assert_eq!(pages.len(), 30);
}

#[test]
fn chunks_without_a_config_collapses_nothing_and_out_writes_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let out_file = dir.path().join("chunks.jsonl");
    let artifact = fixture().join("artifact");
    let out = pinakes(
        &dir.path().join("nonexistent.yaml"),
        &[
            "chunks",
            "--artifact",
            artifact.to_str().unwrap(),
            "--out",
            out_file.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("55 chunks from 33 pages (0 mirrors skipped) -> "),
        "{stderr}"
    );
    let text = fs::read_to_string(&out_file).unwrap();
    assert_eq!(text.lines().count(), 55);
    assert_eq!(text.lines().next(), Some(FIRST_LINES[0]));
}

#[test]
fn a_missing_artifact_fails_with_exit_1() {
    let dir = tempfile::tempdir().unwrap();
    let out = pinakes(&dir.path().join("nonexistent.yaml"), &["chunks"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not an artifact directory"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
