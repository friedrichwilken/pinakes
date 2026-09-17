use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use pinakes::commands::{self, DuplicatesOptions, Paths};
use pinakes::duplicates::{self, DuplicateKind, Suggested};

#[derive(Args)]
pub(crate) struct DuplicatesArgs {
    /// Artifact directory (default: `artifact` next to the config).
    #[arg(long)]
    artifact: Option<PathBuf>,
    /// Minimum Jaccard similarity for a near-duplicate pair.
    #[arg(long, default_value_t = duplicates::DEFAULT_THRESHOLD)]
    threshold: f64,
    /// Write the pairs to this file instead of stdout.
    #[arg(long, value_name = "OUT")]
    json: Option<PathBuf>,
}

pub(crate) fn run_duplicates(mut paths: Paths, args: DuplicatesArgs) -> Result<ExitCode> {
    if let Some(dir) = args.artifact {
        paths.artifact = dir;
    }
    let options = DuplicatesOptions {
        threshold: args.threshold,
        json: args.json.clone(),
    };
    let pairs = commands::duplicates(&paths, &options)?;
    let (mut exact, mut mirror, mut near) = (0, 0, 0);
    for pair in &pairs {
        match pair.kind {
            DuplicateKind::Exact => exact += 1,
            DuplicateKind::Mirror => mirror += 1,
            DuplicateKind::Near => near += 1,
        }
        if pair.suggested == Suggested::Review {
            eprintln!(
                "review: `{}` and `{}` tie on priority, selected_by and commit date",
                pair.canonical, pair.duplicate
            );
        }
    }
    eprintln!(
        "{} pairs: {exact} exact, {mirror} mirror, {near} near",
        pairs.len()
    );
    match &args.json {
        Some(path) => eprintln!("wrote {}", path.display()),
        None => std::io::stdout()
            .lock()
            .write_all(duplicates::to_jsonl(&pairs)?.as_bytes())?,
    }
    Ok(ExitCode::SUCCESS)
}
