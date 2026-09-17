//! `pinakes` command-line interface: clap subcommands only; the logic lives in the library.
//!
//! Human output goes to stderr, data to stdout. Exit codes follow SPEC §4.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use pinakes::commands::Paths;

mod cli;

use cli::classify::{ClassifyArgs, run_classify};
use cli::decide::{DecideArgs, run_decide};
use cli::diff::run_diff;
use cli::duplicates::{DuplicatesArgs, run_duplicates};
use cli::embed::{EmbedArgs, run_embed};
use cli::eval::{EvalArgs, run_eval};
use cli::grade::{GradeArgs, run_grade};
use cli::queries::{QueriesCommand, run_queries};
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
    /// Measure retrieval quality: table on stderr, JSON on stdout, exit 2 when the gate fails.
    Eval(EvalArgs),
    /// Grow and validate the judge, `queries.jsonl`.
    Queries {
        #[command(subcommand)]
        command: QueriesCommand,
    },
    /// Find exact, mirror and near-duplicate pages: JSONL on stdout, a summary on stderr.
    Duplicates(DuplicatesArgs),
    /// Embed every retrieval unit through an OpenAI-compatible endpoint (SPEC §16.2).
    Embed(EmbedArgs),
    /// Ask a model to judge undecided residue and near-duplicate candidates.
    Classify(ClassifyArgs),
    /// Replay a served-query trail against a backend and grade each candidate with a model.
    Grade(GradeArgs),
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
        Command::Eval(args) => run_eval(paths, args),
        Command::Duplicates(args) => run_duplicates(paths, args),
        Command::Queries { command } => run_queries(&paths, command),
        Command::Embed(args) => run_embed(paths, args),
        Command::Classify(args) => run_classify(&paths, args),
        Command::Grade(args) => run_grade(&paths, args),
        Command::Usage(args) => run_usage(&paths, args),
    }
}
