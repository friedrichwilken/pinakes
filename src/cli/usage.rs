use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;

use pinakes::commands::{self, Paths, UsageOptions};

#[derive(Args)]
pub(crate) struct UsageArgs {
    /// Trail file to read.
    #[arg(long, value_name = "FILE")]
    trail: PathBuf,
    /// Only trail entries in this window, e.g. `30d`, `12h`, `45m`, `90s`.
    #[arg(long, value_name = "DURATION")]
    since: Option<String>,
    /// Write the report as JSON to this file (in addition to the stderr summary).
    #[arg(long, value_name = "OUT")]
    json: Option<PathBuf>,
}

pub(crate) fn run_usage(paths: &Paths, args: UsageArgs) -> Result<ExitCode> {
    let options = UsageOptions {
        trail: args.trail,
        since: args.since,
        json: args.json.clone(),
    };
    let usage = commands::usage(paths, &options)?;
    eprintln!(
        "{} never retrieved, {} retrieved but never cited, {} uncited queries",
        usage.never_retrieved.len(),
        usage.retrieved_never_cited.len(),
        usage.uncited_queries.len()
    );
    if let Some(path) = &args.json {
        eprintln!("wrote {}", path.display());
    } else {
        let text = usage.to_json().context("serialising the usage report")?;
        std::io::stdout().lock().write_all(text.as_bytes())?;
    }
    Ok(ExitCode::SUCCESS)
}
