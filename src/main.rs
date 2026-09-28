//! `pinakes` command-line interface: clap subcommands only; the logic lives in the library.
//!
//! Human output goes to stderr, data to stdout. Exit codes follow SPEC §4.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use pinakes::commands::Paths;

mod cli;

use cli::check::{CheckArgs, run_check};
use cli::chunks::{ChunksArgs, run_chunks};
use cli::classify::{ClassifyArgs, run_classify};
use cli::decide::{DecideArgs, run_decide};
use cli::derive::{DeriveArgs, run_derive};
use cli::diff::run_diff;
use cli::duplicates::{DuplicatesArgs, run_duplicates};
use cli::init::{InitArgs, run_init};
use cli::moved::{MovedArgs, run_moved};
use cli::report::{ReportArgs, run_report};
use cli::residue::{ResidueCommand, run_residue_list};
use cli::resolve::{ResolveArgs, run_resolve};
use cli::usage::{UsageArgs, run_usage};
use cli::verify::{VerifyArgs, run_verify};

/// Compile declared documentation sources into a reproducible corpus.
#[derive(Parser)]
#[command(name = "pinakes", version, about)]
struct Cli {
    /// Path to the source configuration.
    #[arg(long, global = true, default_value = "pinakes.yaml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scaffold pinakes.yaml, empty decisions and queries ledgers and .gitignore entries.
    ///
    /// Each repository URL becomes a source with its docs layout detected; `--workflow` also
    /// writes the weekly curation workflow. Existing files are never overwritten.
    Init(InitArgs),
    /// Fetch every source, select pages and write the artifact, manifest and residue.
    Resolve(ResolveArgs),
    /// Check that the committed manifest matches the config, the artifact and the policy.
    Verify(VerifyArgs),
    /// Inspect what was left out of the corpus.
    Residue {
        #[command(subcommand)]
        command: ResidueCommand,
    },
    /// Record a verdict on a residue page (or a page, to exclude it).
    Decide(DecideArgs),
    /// Compare two manifests: JSON on stdout, a summary on stderr, exit 3 on differences.
    Diff {
        /// The older manifest.
        old: PathBuf,
        /// The newer manifest.
        new: PathBuf,
        /// Read new page content from here for line counts (default: the artifact next to the
        /// config).
        #[arg(long, value_name = "DIR")]
        new_artifact: Option<PathBuf>,
        /// Read old page content from here before re-fetching the old commit.
        #[arg(long, value_name = "DIR")]
        old_artifact: Option<PathBuf>,
    },
    /// Render the Markdown report (PR body) on stdout.
    Report(ReportArgs),
    /// Compare report.json with the config's `gates`: exit 2 when a gate is violated.
    Check(CheckArgs),
    /// Moved to `kanon eval` (SPEC §21): prints where to go and exits 1.
    Eval(MovedArgs),
    /// Moved to `kanon queries` (SPEC §21): prints where to go and exits 1.
    Queries(MovedArgs),
    /// Find exact, mirror and near-duplicate pages: JSONL on stdout, a summary on stderr.
    Duplicates(DuplicatesArgs),
    /// Moved to `kanon embed` (SPEC §21): prints where to go and exits 1.
    Embed(MovedArgs),
    /// Emit the retrieval units of the built-in index, one JSON object per line (SPEC §2.9).
    Chunks(ChunksArgs),
    /// Ask a model to judge undecided residue and near-duplicate candidates.
    Classify(ClassifyArgs),
    /// Ask a model for the questions each page answers, kept as search text (SPEC §14.4).
    Derive(DeriveArgs),
    /// Moved to `kanon grade` (SPEC §21): prints where to go and exits 1.
    Grade(MovedArgs),
    /// Report on a served-query trail: unused pages, uncited queries and gap candidates.
    Usage(UsageArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let paths = Paths::for_config(&cli.config);
    match cli.command {
        Command::Init(args) => run_init(&paths, args),
        Command::Resolve(args) => run_resolve(paths, args),
        Command::Verify(args) => run_verify(paths, args),
        Command::Residue {
            command:
                ResidueCommand::List {
                    source,
                    reason,
                    include_excluded,
                },
        } => run_residue_list(
            &paths,
            source.as_deref(),
            reason.as_deref(),
            include_excluded,
        ),
        Command::Decide(args) => run_decide(&paths, &args),
        Command::Diff {
            old,
            new,
            new_artifact,
            old_artifact,
        } => run_diff(&paths, &old, &new, new_artifact, old_artifact),
        Command::Report(args) => run_report(&paths, args),
        Command::Check(args) => run_check(&paths, args),
        Command::Eval(_) => Ok(run_moved("eval")),
        Command::Duplicates(args) => run_duplicates(paths, args),
        Command::Queries(_) => Ok(run_moved("queries")),
        Command::Embed(_) => Ok(run_moved("embed")),
        Command::Chunks(args) => run_chunks(&paths, args),
        Command::Classify(args) => run_classify(&paths, args),
        Command::Derive(args) => run_derive(&paths, args),
        Command::Grade(_) => Ok(run_moved("grade")),
        Command::Usage(args) => run_usage(&paths, args),
    }
}
