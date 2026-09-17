use std::io::Write as _;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use pinakes::commands::{self, ClassifyOptions, Paths};
use pinakes::llm::UreqChatTransport;

#[derive(Args)]
pub(crate) struct ClassifyArgs {
    /// Model name; falls back to `PINAKES_LLM_MODEL`.
    #[arg(long)]
    model: Option<String>,
    /// Candidates per chat completion request.
    #[arg(long, default_value_t = pinakes::classify::DEFAULT_BATCH)]
    batch: usize,
    /// Print the proposed decisions as JSONL instead of writing them.
    #[arg(long)]
    dry_run: bool,
}

pub(crate) fn run_classify(paths: &Paths, args: ClassifyArgs) -> Result<ExitCode> {
    let dry_run = args.dry_run;
    let options = ClassifyOptions {
        model: args.model,
        batch: Some(args.batch),
        dry_run,
    };
    let transport = UreqChatTransport::new();
    let outcome = commands::classify(paths, &options, &transport)?;
    for warning in &outcome.warnings {
        eprintln!("warning: {warning}");
    }
    if dry_run {
        let mut out = std::io::stdout().lock();
        for decision in &outcome.decisions {
            out.write_all(serde_json::to_string(decision)?.as_bytes())?;
            out.write_all(b"\n")?;
        }
        eprintln!(
            "{} proposed decisions (dry run, nothing written)",
            outcome.decisions.len()
        );
    } else {
        eprintln!(
            "wrote {} decisions to {}",
            outcome.decisions.len(),
            paths.decisions.display()
        );
    }
    Ok(ExitCode::SUCCESS)
}
