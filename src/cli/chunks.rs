use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use pinakes::commands::{self, ChunksOptions, Paths};

#[derive(Args)]
pub(crate) struct ChunksArgs {
    /// Artifact directory (default: `artifact` next to the config).
    #[arg(long)]
    artifact: Option<PathBuf>,
    /// Write the JSONL here instead of stdout.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
}

pub(crate) fn run_chunks(paths: &Paths, args: ChunksArgs) -> Result<ExitCode> {
    let options = ChunksOptions {
        artifact: args.artifact,
        out: args.out.clone(),
    };
    let outcome = commands::chunks(paths, &options)?;
    if let Some(text) = outcome.jsonl {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(text.as_bytes())?;
        stdout.flush()?;
    }
    let mirrors = outcome.page_count - outcome.pages;
    match args.out {
        Some(out) => eprintln!(
            "{} chunks from {} pages ({} mirrors skipped) -> {}",
            outcome.chunks,
            outcome.pages,
            mirrors,
            out.display()
        ),
        None => eprintln!(
            "{} chunks from {} pages ({} mirrors skipped)",
            outcome.chunks, outcome.pages, mirrors
        ),
    }
    Ok(ExitCode::SUCCESS)
}
