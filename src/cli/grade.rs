use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;

use pinakes::commands::{self, GradeOptions, Paths};
use pinakes::llm::UreqChatTransport;

#[derive(Args)]
pub(crate) struct GradeArgs {
    /// Trail file to replay.
    #[arg(long, value_name = "FILE")]
    trail: PathBuf,
    /// Backend to fetch candidates from; only `bm25` is available today (SPEC §16.1 is not
    /// wired into `grade` yet).
    #[arg(long, default_value = "bm25")]
    backend: String,
    /// Candidates fetched per query.
    #[arg(long, value_name = "N")]
    k: Option<usize>,
    /// Model name; falls back to `PINAKES_LLM_MODEL`.
    #[arg(long)]
    model: Option<String>,
    /// Write graded rows to this file instead of stdout.
    #[arg(long, value_name = "OUT")]
    out: Option<PathBuf>,
}

pub(crate) fn run_grade(paths: &Paths, args: GradeArgs) -> Result<ExitCode> {
    let options = GradeOptions {
        trail: args.trail,
        backend: Some(args.backend),
        k: args.k,
        model: args.model,
        out: args.out.clone(),
    };
    let transport = UreqChatTransport::new();
    let outcome = commands::grade(paths, &options, &transport)?;
    eprintln!(
        "{} queries, {} graded rows",
        outcome.queries,
        outcome.rows.len()
    );
    if let Some(path) = &args.out {
        eprintln!("wrote {}", path.display());
    } else {
        let text = pinakes::grade::to_jsonl(&outcome.rows).context("serialising graded rows")?;
        std::io::stdout().lock().write_all(text.as_bytes())?;
    }
    Ok(ExitCode::SUCCESS)
}
