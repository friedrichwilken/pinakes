use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use pinakes::commands::{self, Paths};

use crate::cli::{EXIT_DIFFERENCES, EXIT_POLICY};

#[derive(Args)]
pub(crate) struct VerifyArgs {
    /// Artifact directory to check as well (default: `artifact` next to the config).
    #[arg(long)]
    artifact: Option<PathBuf>,
    /// Skip the artifact check; only compare the manifest with the config and policy.
    #[arg(long)]
    no_artifact: bool,
}

pub(crate) fn run_verify(mut paths: Paths, args: VerifyArgs) -> Result<ExitCode> {
    if let Some(dir) = args.artifact {
        paths.artifact = dir;
    }
    let report = commands::verify(&paths, !args.no_artifact)?;
    for line in &report.stale {
        eprintln!("stale: {line}");
    }
    for line in &report.violations {
        eprintln!("policy: {line}");
    }
    if !report.stale.is_empty() {
        Ok(ExitCode::from(EXIT_DIFFERENCES))
    } else if !report.violations.is_empty() {
        Ok(ExitCode::from(EXIT_POLICY))
    } else {
        eprintln!("ok");
        Ok(ExitCode::SUCCESS)
    }
}
