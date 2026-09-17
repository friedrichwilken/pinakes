use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use pinakes::commands::{self, Paths, ReportOptions};
use pinakes::sources::GitHubFetcher;

#[derive(Args)]
pub(crate) struct ReportArgs {
    /// The previous manifest, for added/removed/changed pages.
    #[arg(long, value_name = "MANIFEST")]
    old: Option<PathBuf>,
    /// The current manifest (default: manifest.json next to the config).
    #[arg(long, value_name = "MANIFEST")]
    new: Option<PathBuf>,
    /// Eval JSON for the previous corpus.
    #[arg(long, value_name = "EVAL")]
    eval_before: Option<PathBuf>,
    /// Eval JSON for the current corpus.
    #[arg(long, value_name = "EVAL")]
    eval_after: Option<PathBuf>,
    /// Read new page content from here for changed-page line counts (default: the artifact
    /// next to the config).
    #[arg(long, value_name = "DIR")]
    new_artifact: Option<PathBuf>,
    /// Read old page content from here before re-fetching the old commit.
    #[arg(long, value_name = "DIR")]
    old_artifact: Option<PathBuf>,
    /// A `pinakes usage --json` report to render as the "Usage" section (SPEC §15.3).
    #[arg(long, value_name = "USAGE")]
    usage: Option<PathBuf>,
}

pub(crate) fn run_report(paths: &Paths, args: ReportArgs) -> Result<ExitCode> {
    let options = ReportOptions {
        old: args.old,
        new: args.new,
        eval_before: args.eval_before,
        eval_after: args.eval_after,
        new_artifact: Some(args.new_artifact.unwrap_or_else(|| paths.artifact.clone())),
        old_artifact: args.old_artifact,
        usage: args.usage,
    };
    let text = commands::report(paths, &options, &GitHubFetcher::new())?;
    std::io::stdout().lock().write_all(text.as_bytes())?;
    Ok(ExitCode::SUCCESS)
}
