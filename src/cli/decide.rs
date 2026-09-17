use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::Args;

use pinakes::commands::{self, Paths};
use pinakes::decisions::Verdict;

#[derive(Args)]
pub(crate) struct DecideArgs {
    /// Page id, `<source>::<path>`.
    id: String,
    /// One of include, exclude, unsure.
    decision: String,
    /// One sentence of justification; defaults to "superseded by `<SUPERSEDED_BY>`" when
    /// `--superseded-by` is given.
    #[arg(long)]
    reason: Option<String>,
    /// The canonical page id this one is superseded by (SPEC §11); with no `--reason`, the
    /// reason defaults to "superseded by `<SUPERSEDED_BY>`".
    #[arg(long, value_name = "ID")]
    superseded_by: Option<String>,
    /// Who decided.
    #[arg(long, default_value = "pinakes")]
    by: String,
}

pub(crate) fn run_decide(paths: &Paths, args: &DecideArgs) -> Result<ExitCode> {
    let Some(verdict) = Verdict::parse(&args.decision) else {
        bail!(
            "decision must be include, exclude or unsure, not {:?}",
            args.decision
        );
    };
    let reason = match (&args.reason, &args.superseded_by) {
        (Some(reason), _) => reason.clone(),
        (None, Some(canonical)) => format!("superseded by {canonical}"),
        (None, None) => bail!("--reason is required unless --superseded-by is given"),
    };
    let decision = commands::decide(paths, &args.id, verdict, &reason, &args.by, None)?;
    eprintln!(
        "recorded {} for {} in {}",
        decision.decision,
        decision.id,
        paths.decisions.display()
    );
    Ok(ExitCode::SUCCESS)
}
