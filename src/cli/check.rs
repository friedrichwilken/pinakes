use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;

use pinakes::commands::{self, CheckOptions, Paths};

use crate::cli::EXIT_GATE;

#[derive(Args)]
pub(crate) struct CheckArgs {
    /// The report.json written by `report --json` (default: report.json next to the config).
    #[arg(long, value_name = "FILE")]
    report: Option<PathBuf>,
}

pub(crate) fn run_check(paths: &Paths, args: CheckArgs) -> Result<ExitCode> {
    let options = CheckOptions {
        report: args.report,
    };
    let outcome = commands::check(paths, &options)?;
    for warning in &outcome.warnings {
        eprintln!("warning: {warning}");
    }
    for violation in &outcome.violations {
        eprintln!(
            "gate {}: {} > {}",
            violation.gate, violation.actual, violation.limit
        );
    }
    if outcome.checked == 0 {
        eprintln!("gates: none configured");
    } else if outcome.passed() {
        eprintln!("gates: ok ({} checked)", outcome.checked);
    } else {
        eprintln!(
            "gates: FAILED ({} of {} violated)",
            outcome.violations.len(),
            outcome.checked
        );
    }
    let text = outcome.to_json().context("serialising the outcome")?;
    std::io::stdout().lock().write_all(text.as_bytes())?;
    Ok(if outcome.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_GATE)
    })
}
