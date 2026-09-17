use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, Subcommand};

use pinakes::commands::{self, Paths, QueriesAddOptions, QueriesImportOptions};

use crate::cli::EXIT_POLICY;

#[derive(Subcommand)]
pub(crate) enum QueriesCommand {
    /// Append a row after checking `--expected` against the committed manifest.
    Add(QueriesAddArgs),
    /// Fail (exit 4) on unknown expected ids, duplicate ids or too small a held-out share.
    Check {
        /// Query file (default: `eval.queries` from the config).
        #[arg(long, value_name = "FILE")]
        queries: Option<PathBuf>,
    },
    /// Turn `pinakes grade`'s output into query rows (SPEC §15.2).
    Import(QueriesImportArgs),
}

#[derive(Args)]
pub(crate) struct QueriesImportArgs {
    /// `graded.jsonl`, as written by `pinakes grade`.
    graded: PathBuf,
    /// Minimum grade for a candidate to enter `expected`.
    #[arg(long, default_value_t = pinakes::queries::DEFAULT_MIN_GRADE)]
    min_grade: u8,
    /// Target held-out share for the random `holdout` assignment.
    #[arg(long, default_value_t = pinakes::queries::DEFAULT_HOLDOUT_MIN)]
    holdout_share: f64,
    /// Seed for the deterministic PRNG that assigns `holdout`.
    #[arg(long, default_value_t = 0)]
    seed: u64,
    /// Query file to append to (default: `eval.queries` from the config).
    #[arg(long, value_name = "FILE")]
    queries: Option<PathBuf>,
}

#[derive(Args)]
pub(crate) struct QueriesAddArgs {
    /// Query id.
    #[arg(long)]
    id: String,
    /// The query text.
    #[arg(long)]
    query: String,
    /// Page ids, id prefixes, or the legacy `<source>/<path>` form; every one must exist.
    #[arg(long, value_name = "ID", num_args = 1..)]
    expected: Vec<String>,
    /// Query kind, e.g. `howto`.
    #[arg(long, default_value = "")]
    kind: String,
    /// Hold this row out of tuning decisions.
    #[arg(long)]
    holdout: bool,
    /// Query file (default: `eval.queries` from the config).
    #[arg(long, value_name = "FILE")]
    queries: Option<PathBuf>,
}

pub(crate) fn run_queries(paths: &Paths, command: QueriesCommand) -> Result<ExitCode> {
    match command {
        QueriesCommand::Add(args) => run_queries_add(paths, args),
        QueriesCommand::Check { queries } => run_queries_check(paths, queries.as_deref()),
        QueriesCommand::Import(args) => run_queries_import(paths, args),
    }
}

fn run_queries_import(paths: &Paths, args: QueriesImportArgs) -> Result<ExitCode> {
    let options = QueriesImportOptions {
        queries: args.queries,
        graded: args.graded,
        min_grade: args.min_grade,
        holdout_share: args.holdout_share,
        seed: args.seed,
    };
    let outcome = commands::queries_import(paths, &options)?;
    for query in &outcome.skipped {
        eprintln!("skipped {query:?}: no candidate at or above --min-grade");
    }
    eprintln!(
        "imported {} queries ({} skipped)",
        outcome.imported.len(),
        outcome.skipped.len()
    );
    Ok(ExitCode::SUCCESS)
}
fn run_queries_add(paths: &Paths, args: QueriesAddArgs) -> Result<ExitCode> {
    let options = QueriesAddOptions {
        queries: args.queries,
        id: args.id,
        query: args.query,
        expected: args.expected,
        kind: args.kind,
        holdout: args.holdout,
    };
    let query = commands::queries_add(paths, &options)?;
    eprintln!("added {} ({} expected)", query.id, query.expected.len());
    Ok(ExitCode::SUCCESS)
}

fn run_queries_check(paths: &Paths, queries: Option<&std::path::Path>) -> Result<ExitCode> {
    let report = commands::queries_check(paths, queries)?;
    for (id, expected) in &report.unknown {
        eprintln!("unknown: {id}: expected {expected:?} matches no page in the manifest");
    }
    for id in &report.duplicate_ids {
        eprintln!("duplicate: {id}");
    }
    eprintln!(
        "held-out share: {:.3} (minimum {:.3})",
        report.holdout_share, report.holdout_min
    );
    if report.ok() {
        eprintln!("ok");
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(EXIT_POLICY))
    }
}
