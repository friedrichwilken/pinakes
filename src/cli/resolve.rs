use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use pinakes::commands::{self, Paths, ResolveOptions};
use pinakes::sources::GitHubFetcher;

#[derive(Args)]
pub(crate) struct ResolveArgs {
    /// Artifact directory (default: `artifact` next to the config).
    #[arg(long)]
    artifact: Option<PathBuf>,
    /// Reproduce this manifest exactly instead of resolving the config.
    #[arg(long, value_name = "MANIFEST")]
    from_manifest: Option<PathBuf>,
}

pub(crate) fn run_resolve(mut paths: Paths, args: ResolveArgs) -> Result<ExitCode> {
    if let Some(dir) = args.artifact {
        paths.artifact = dir;
    }
    let options = ResolveOptions {
        from_manifest: args.from_manifest,
        generated_at: None,
    };
    let outcome = commands::resolve(&paths, &options, &GitHubFetcher::new())?;
    for warning in &outcome.warnings {
        eprintln!("warning: {warning}");
    }
    for expired in &outcome.expired {
        eprintln!(
            "expired decision: {} ({}) no longer matches the page",
            expired.decision.id, expired.decision.decision
        );
    }
    for (name, source) in &outcome.manifest.sources {
        eprintln!(
            "{name}: {} pages, {} residue, {} unresolved @ {}",
            source.pages.len(),
            source.residue.len(),
            source.unresolved.len(),
            &source.commit[..source.commit.len().min(12)]
        );
    }
    eprintln!(
        "wrote {} and {} ({} residue entries) and {}",
        paths.manifest.display(),
        paths.residue.display(),
        outcome.residue.len(),
        paths.artifact.display()
    );
    Ok(ExitCode::SUCCESS)
}
