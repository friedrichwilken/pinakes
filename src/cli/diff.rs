use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;

use pinakes::commands::{self, DiffOptions, Paths};
use pinakes::manifest::Manifest;
use pinakes::sources::GitHubFetcher;

use crate::cli::EXIT_DIFFERENCES;

pub(crate) fn run_diff(
    paths: &Paths,
    old: &std::path::Path,
    new: &std::path::Path,
    new_artifact: Option<PathBuf>,
    old_artifact: Option<PathBuf>,
) -> Result<ExitCode> {
    let old = Manifest::load(old)?;
    let new = Manifest::load(new)?;
    let options = DiffOptions {
        new_artifact: Some(new_artifact.unwrap_or_else(|| paths.artifact.clone())),
        old_artifact,
    };
    let diff = commands::diff(&old, &new, &options, &GitHubFetcher::new())?;
    let mut out = std::io::stdout().lock();
    out.write_all(diff.to_json()?.as_bytes())?;
    eprint!("{}", diff.summary());
    Ok(if diff.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_DIFFERENCES)
    })
}
