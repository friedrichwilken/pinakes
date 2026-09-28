use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use pinakes::commands::{self, DeriveOptions, Paths};
use pinakes::llm::UreqChatTransport;

#[derive(Args)]
pub(crate) struct DeriveArgs {
    /// Model name; falls back to `PINAKES_LLM_MODEL`.
    #[arg(long)]
    model: Option<String>,
    /// List the pages that would be asked about, without asking or writing.
    #[arg(long)]
    dry_run: bool,
}

pub(crate) fn run_derive(paths: &Paths, args: DeriveArgs) -> Result<ExitCode> {
    let options = DeriveOptions {
        model: args.model,
        dry_run: args.dry_run,
    };
    let transport = UreqChatTransport::new();
    let outcome = commands::derive(paths, &options, &transport)?;
    for warning in &outcome.warnings {
        eprintln!("warning: {warning}");
    }
    if options.dry_run {
        for id in &outcome.pending {
            println!("{id}");
        }
        eprintln!(
            "{} of {} pages would get questions ({} fresh; dry run, nothing written)",
            outcome.pending.len(),
            outcome.considered,
            outcome.fresh
        );
    } else {
        if outcome.written {
            eprintln!(
                "questions for {} pages ({} fresh, {} dropped); wrote {} and the artifact's \
                 derived.jsonl",
                outcome.derived,
                outcome.fresh,
                outcome.dropped,
                paths.manifest.display()
            );
        } else if outcome.pending.is_empty() {
            eprintln!(
                "nothing to do: {} of {} pages are fresh",
                outcome.fresh, outcome.considered
            );
        }
        if !outcome.pending.is_empty() {
            eprintln!(
                "{} pages got no usable questions and stay pending: run derive again to retry",
                outcome.pending.len()
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}
