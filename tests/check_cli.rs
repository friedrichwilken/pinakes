//! `pinakes check` end to end through the binary: the violated gates on stderr, the JSON
//! summary on stdout, exit 2 when a gate is violated and 1 without a report (SPEC §2.11, §4).

use std::fs;
use std::path::Path;
use std::process::Command;

fn pinakes(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_pinakes"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("pinakes runs")
}

const CONFIG: &str = "version: 1\nsources:\n  - name: handbook\n    \
                      repo: https://github.com/example-org/handbook.git\n    ref: main\n    \
                      resolver:\n      type: glob\n      include: ['docs/**/*.md']\n";

/// A `report.json` with three undecided residue ids, one removed page, no expired decision, one
/// archived source, one unresolved link and two duplicate pairs (compacted; `report --json`
/// pretty-prints, and `check` reads either).
const REPORT: &str = r#"{
  "version": 1,
  "summary": {"sources": 1, "pages": 3, "residue": 3, "undecided": 3, "excluded": 0,
              "decisions": 0, "changes": {"since": "2026-09-01T00:00:00Z", "added": 0,
              "removed": 1, "changed": 0}},
  "eval": null,
  "pages": {"added": [], "removed": [{"id": "handbook::docs/old.md", "reason": "gone_upstream"}],
            "changed": []},
  "residue": {"new": [], "undecided": ["handbook::docs/a.md", "handbook::docs/b.md",
              "handbook::docs/c.md"], "excluded": []},
  "expired_decisions": [],
  "unresolved_links": {"handbook": ["handbook::docs/ghost.md"]},
  "archived_sources": ["handbook"],
  "duplicates": {"count": 2, "pairs": []},
  "usage": null
}
"#;

fn workspace(gates: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("pinakes.yaml"), format!("{CONFIG}{gates}")).unwrap();
    dir
}

#[test]
fn check_passes_with_the_ok_line_and_the_summary_json() {
    let dir = workspace("gates:\n  undecided_residue_max: 3\n  duplicates_max: 5\n");
    fs::write(dir.path().join("report.json"), REPORT).unwrap();
    let out = pinakes(dir.path(), &["check"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(stderr, "gates: ok (2 checked)\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "{\"version\":1,\"checked\":2,\"violations\":[]}\n"
    );
}

#[test]
fn check_exits_2_with_one_line_per_violated_gate() {
    let dir = workspace(
        "gates:\n  undecided_residue_max: 0\n  removed_pages_max: 1\n  \
         expired_decisions_max: 0\n  unresolved_links_max: 0\n",
    );
    // `--report` reads the file from wherever it was written.
    let report = dir.path().join("out").join("facts.json");
    fs::create_dir_all(report.parent().unwrap()).unwrap();
    fs::write(&report, REPORT).unwrap();
    let out = pinakes(dir.path(), &["check", "--report", "out/facts.json"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert_eq!(
        stderr,
        "gate undecided_residue_max: 3 > 0\n\
         gate unresolved_links_max: 1 > 0\n\
         gates: FAILED (2 of 4 violated)\n"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout,
        "{\"version\":1,\"checked\":4,\"violations\":[\
         {\"gate\":\"undecided_residue_max\",\"limit\":0,\"actual\":3},\
         {\"gate\":\"unresolved_links_max\",\"limit\":0,\"actual\":1}]}\n"
    );
    let summary: serde_json::Value = serde_json::from_str(&stdout).expect("JSON on stdout");
    assert_eq!(summary["violations"][1]["gate"], "unresolved_links_max");
}

#[test]
fn removed_pages_max_warns_when_the_report_has_no_previous_manifest() {
    let dir = workspace("gates:\n  removed_pages_max: 0\n  duplicates_max: 1\n");
    let no_old = REPORT.replacen(
        "\"changes\": {\"since\": \"2026-09-01T00:00:00Z\", \"added\": 0,\n              \
         \"removed\": 1, \"changed\": 0}",
        "\"changes\": null",
        1,
    );
    assert!(no_old.contains("\"changes\": null"));
    let no_old = no_old.replacen(
        "[{\"id\": \"handbook::docs/old.md\", \"reason\": \"gone_upstream\"}]",
        "[]",
        1,
    );
    fs::write(dir.path().join("report.json"), no_old).unwrap();
    let out = pinakes(dir.path(), &["check"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The warning is one line; the other gate still decides the exit code.
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert_eq!(
        stderr,
        "warning: removed_pages_max is set but the report has no previous manifest \
         (summary.changes is null): the gate passes vacuously; run report with --old\n\
         gate duplicates_max: 2 > 1\n\
         gates: FAILED (1 of 2 violated)\n"
    );
}

#[test]
fn check_exits_1_without_a_report_and_0_without_gates() {
    let dir = workspace("gates:\n  duplicates_max: 0\n");
    let out = pinakes(dir.path(), &["check"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("report.json: no report to check; run `pinakes report --json"),
        "{stderr}"
    );
    assert!(out.stdout.is_empty());

    // A report from a newer pinakes: refused with exit 1 and a line naming the version.
    let newer = REPORT.replacen("\"version\": 1", "\"version\": 2", 1);
    fs::write(dir.path().join("report.json"), newer).unwrap();
    let out = pinakes(dir.path(), &["check"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("report.json version 2 is newer than this build understands (1)"),
        "{stderr}"
    );
    assert!(out.stdout.is_empty());

    // No `gates:` block: nothing to check, and no report is needed.
    let dir = workspace("");
    let out = pinakes(dir.path(), &["check"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "gates: none configured\n"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "{\"version\":1,\"checked\":0,\"violations\":[]}\n"
    );
}
