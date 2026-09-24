use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use pinakes::commands::{self, InitOptions, Paths};
use pinakes::sources::GitHubFetcher;

#[derive(Args)]
pub(crate) struct InitArgs {
    /// GitHub repository URLs to declare as sources; each is fetched once to detect its docs
    /// layout (vitepress, docusaurus, mdbook or a glob). None gives a placeholder source.
    #[arg(value_name = "REPO_URL")]
    repos: Vec<String>,
    /// Also write `.github/workflows/curate.yml`, the weekly curation workflow.
    #[arg(long)]
    workflow: bool,
    /// Directory to scaffold into, created when missing (default: the directory of `--config`;
    /// the config keeps its file name either way).
    #[arg(long, value_name = "DIR")]
    dir: Option<PathBuf>,
}

pub(crate) fn run_init(paths: &Paths, args: InitArgs) -> Result<ExitCode> {
    let file_name = paths
        .config
        .file_name()
        .map_or_else(|| "pinakes.yaml".into(), ToOwned::to_owned);
    let config = match args.dir {
        Some(dir) => dir.join(file_name),
        None => paths.config.clone(),
    };
    let options = InitOptions {
        config,
        repos: args.repos,
        workflow: args.workflow,
    };
    let outcome = commands::init(&options, &GitHubFetcher::new())?;
    for warning in &outcome.warnings {
        eprintln!("warning: {warning}");
    }
    if options.repos.is_empty() && outcome.written.contains(&options.config) {
        eprintln!(
            "hint: edit the placeholder source in {} before `resolve`",
            options.config.display()
        );
    }
    for path in &outcome.written {
        eprintln!("wrote {}", path.display());
    }
    for path in &outcome.updated {
        eprintln!("updated {}", path.display());
    }
    for path in &outcome.skipped {
        eprintln!("skipped {} (exists)", path.display());
    }
    Ok(ExitCode::SUCCESS)
}
