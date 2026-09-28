//! The subcommands that moved to `kanon` (SPEC §21) end to end through the binary: each prints
//! one line naming the `kanon` command and exits 1, whatever arguments an old invocation carries.

use std::process::Command;

fn pinakes(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_pinakes"))
        .args(args)
        .output()
        .expect("pinakes runs")
}

#[test]
fn moved_subcommands_point_at_kanon_and_exit_1() {
    for (args, command) in [
        (&["eval"][..], "eval"),
        (
            &["eval", "--gate", "baseline.json", "--queries", "q.jsonl"],
            "eval",
        ),
        (&["eval", "--help"], "eval"),
        (&["queries", "add", "--id", "x"], "queries"),
        (&["embed", "--endpoint", "http://localhost"], "embed"),
        (&["grade", "--trail", "trail.jsonl"], "grade"),
    ] {
        let out = pinakes(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}: nothing on stdout");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!("pinakes {command} moved to kanon: run `kanon {command}` instead (SPEC §21)\n"),
            "{args:?}"
        );
    }
}

#[test]
fn report_no_longer_takes_eval_files() {
    let out = pinakes(&["report", "--eval-after", "eval.json"]);
    assert_eq!(out.status.code(), Some(2), "clap's usage error");
    assert!(String::from_utf8_lossy(&out.stderr).contains("--eval-after"));
}
