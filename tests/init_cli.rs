//! `pinakes init` end to end through the binary, with no repository URL so nothing touches the
//! network (SPEC §4): the scaffold is written once, loads as a config, and is never overwritten.

use std::fs;
use std::path::Path;
use std::process::Command;

use pinakes::config::Config;

fn pinakes(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_pinakes"))
        .current_dir(dir)
        .arg("--config")
        .arg("pinakes.yaml")
        .args(args)
        .output()
        .expect("pinakes runs")
}

#[test]
fn init_scaffolds_once_and_skips_on_the_second_run() {
    let dir = tempfile::tempdir().unwrap();
    let out = pinakes(dir.path(), &["init", "--workflow"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty(), "human output goes to stderr");
    let stderr = String::from_utf8_lossy(&out.stderr);
    for file in [
        "pinakes.yaml",
        "decisions.jsonl",
        "queries.jsonl",
        ".gitignore",
        ".github/workflows/curate.yml",
    ] {
        assert!(stderr.contains(&format!("wrote {file}")), "{stderr}");
        assert!(dir.path().join(file).is_file(), "{file}");
    }
    assert!(!stderr.contains("warning"), "{stderr}");
    assert!(
        stderr.contains("hint: edit the placeholder source in pinakes.yaml before `resolve`"),
        "{stderr}"
    );

    let config = Config::load(&dir.path().join("pinakes.yaml")).expect("generated config loads");
    assert_eq!(config.sources.len(), 1);
    assert_eq!(fs::read(dir.path().join("decisions.jsonl")).unwrap(), b"");
    assert_eq!(fs::read(dir.path().join("queries.jsonl")).unwrap(), b"");
    let gitignore = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    for line in ["/artifact", "/artifact-*", "/report.md"] {
        assert!(gitignore.lines().any(|l| l == line), "{gitignore}");
    }
    let workflow = fs::read_to_string(dir.path().join(".github/workflows/curate.yml")).unwrap();
    assert!(workflow.contains("workflow_dispatch"), "{workflow}");

    fs::write(dir.path().join("pinakes.yaml"), "edited\n").unwrap();
    let again = pinakes(dir.path(), &["init", "--workflow"]);
    assert!(again.status.success());
    let stderr = String::from_utf8_lossy(&again.stderr);
    assert!(stderr.contains("skipped pinakes.yaml (exists)"), "{stderr}");
    assert!(!stderr.contains("wrote"), "{stderr}");
    assert_eq!(
        fs::read_to_string(dir.path().join("pinakes.yaml")).unwrap(),
        "edited\n",
        "an existing file is never overwritten"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join(".gitignore")).unwrap(),
        gitignore,
        "no duplicate .gitignore lines"
    );
}

#[test]
fn init_dir_creates_the_directory_and_a_bad_url_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_pinakes"))
        .current_dir(dir.path())
        .args(["--config", "sources.yaml", "init", "--dir", "corpus"])
        .output()
        .expect("pinakes runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        dir.path().join("corpus/sources.yaml").is_file(),
        "--dir keeps the --config file name"
    );
    assert!(dir.path().join("corpus/decisions.jsonl").is_file());
    assert!(
        !dir.path().join("corpus/.github").exists(),
        "no workflow without --workflow"
    );

    let bad = pinakes(dir.path(), &["init", "--dir", "other", "not-a-url"]);
    assert_eq!(bad.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&bad.stderr);
    assert!(stderr.contains("not-a-url"), "{stderr}");
    assert!(!dir.path().join("other/pinakes.yaml").exists());
}
