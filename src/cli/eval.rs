use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;

use pinakes::backend::BackendKind;
use pinakes::commands::{self, BackendEvalOptions, EvalFlags, EvalOptions, EvalPlan, Paths};
use pinakes::eval;

use crate::cli::EXIT_GATE;

#[derive(Args)]
pub(crate) struct EvalArgs {
    /// Artifact directory (default: `artifact` next to the config).
    #[arg(long)]
    artifact: Option<PathBuf>,
    /// Query file (default: `eval.queries` from the config).
    #[arg(long, value_name = "FILE")]
    queries: Option<PathBuf>,
    /// Result list length (default: `eval.k` from the config, else 10).
    #[arg(long, value_name = "N")]
    k: Option<usize>,
    /// Write the result JSON to this file instead of stdout.
    #[arg(long, value_name = "OUT")]
    json: Option<PathBuf>,
    /// Exit 2 when tuning recall@5 drops by more than `eval.max_recall_drop` below this result.
    #[arg(long, value_name = "BASELINE")]
    gate: Option<PathBuf>,
    /// Residue pages (`<source>::<path>`) to add before measuring; prints the delta.
    #[arg(long, value_name = "ID", num_args = 1..)]
    with: Vec<String>,
    /// Pages to remove before measuring; prints the delta.
    #[arg(long, value_name = "ID", num_args = 1..)]
    without: Vec<String>,
    /// Retriever backend to measure: bm25 (default), bm25-tantivy, dense, hybrid or external
    /// (SPEC §16.1).
    #[arg(long, value_name = "NAME")]
    backend: Option<String>,
    /// The consumer's search endpoint base URL (`--backend external`).
    #[arg(long, value_name = "URL")]
    backend_url: Option<String>,
    /// `embeddings.bin` path (`--backend dense`/`hybrid`; default: `embeddings.bin` next to the
    /// config).
    #[arg(long, value_name = "FILE")]
    embeddings: Option<PathBuf>,
    /// Use embeddings even when their recorded manifest hash does not match the artifact.
    #[arg(long)]
    allow_stale: bool,
    /// Run every named backend (comma-separated) over the same query set, one table each, and
    /// (with `--json`) one combined result file.
    #[arg(long, value_name = "NAME,NAME,…", value_delimiter = ',')]
    compare: Vec<String>,
}

/// The command-line flags as [`commands::EvalFlags`], before `pinakes.yaml`'s defaults are
/// applied.
fn eval_flags(args: EvalArgs) -> EvalFlags {
    EvalFlags {
        queries: args.queries,
        k: args.k,
        json: args.json,
        gate: args.gate,
        with: args.with,
        without: args.without,
        backend: args.backend,
        backend_url: args.backend_url,
        embeddings: args.embeddings,
        allow_stale: args.allow_stale,
        compare: args.compare,
    }
}

pub(crate) fn run_eval(mut paths: Paths, args: EvalArgs) -> Result<ExitCode> {
    if let Some(dir) = &args.artifact {
        paths.artifact.clone_from(dir);
    }
    let mut flags = eval_flags(args);
    commands::apply_eval_config_defaults(&paths, &mut flags)?;
    let mut plan = commands::eval_plan(flags)?;
    if plan.needs_embedder() {
        plan = plan.with_embedder(commands::eval_embedder_from_env()?);
    }
    match plan {
        EvalPlan::Plain(options) => run_eval_plain(&paths, &options),
        EvalPlan::Backend(options) => run_eval_with_backend(&paths, &options),
        EvalPlan::Compare {
            backends,
            common,
            json,
        } => run_eval_compare(&paths, &backends, &common, json.as_deref()),
    }
}

/// The plain `eval` path: no `--backend`/`--compare`/backend-only flags at all, so it goes
/// through [`commands::eval`] exactly as before backend selection existed.
fn run_eval_plain(paths: &Paths, options: &EvalOptions) -> Result<ExitCode> {
    let outcome = commands::eval(paths, options)?;
    eprintln!(
        "{}: {} pages, {} searchable, k = {}",
        paths.artifact.display(),
        outcome.page_count,
        outcome.searchable_count,
        outcome.k
    );
    eprint!("{}", eval::render_table(&outcome.summary));
    if let Some(delta) = &outcome.delta {
        eprint!("{}", eval::render_delta(delta));
    }
    if let Some(path) = &options.json {
        eprintln!("wrote {}", path.display());
    } else {
        let text = outcome
            .summary
            .to_json()
            .context("serialising the result")?;
        std::io::stdout().lock().write_all(text.as_bytes())?;
    }
    if let Some(gate) = outcome.gate {
        let verdict = if gate.passed() { "ok" } else { "FAILED" };
        eprintln!(
            "gate: tuning recall@5 {:.3} → {:.3}, drop {:+.3}, max {:.3}: {verdict}",
            gate.baseline,
            gate.current,
            gate.drop(),
            gate.max_drop
        );
        if !gate.passed() {
            return Ok(ExitCode::from(EXIT_GATE));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `eval --backend NAME` (or any backend-only flag without `--compare`): through
/// [`commands::eval_backend`].
fn run_eval_with_backend(paths: &Paths, options: &BackendEvalOptions) -> Result<ExitCode> {
    let backend = options.backend;
    let json = options.json.clone();
    let outcome = commands::eval_backend(paths, options)?;
    print_eval_outcome(
        paths,
        backend,
        outcome.page_count,
        outcome.searchable_count,
        outcome.k,
        &outcome.summary,
        outcome.delta.as_ref(),
    );
    if let Some(path) = &json {
        eprintln!("wrote {}", path.display());
    } else {
        let text = outcome
            .summary
            .to_json()
            .context("serialising the result")?;
        std::io::stdout().lock().write_all(text.as_bytes())?;
    }
    if let Some(gate) = outcome.gate {
        print_gate(backend, &gate);
        if !gate.passed() {
            return Ok(ExitCode::from(EXIT_GATE));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `eval --compare a,b,c`: one table per backend on the same query set, one combined JSON.
fn run_eval_compare(
    paths: &Paths,
    backends: &[BackendKind],
    common: &BackendEvalOptions,
    json: Option<&Path>,
) -> Result<ExitCode> {
    let results = commands::eval_compare(paths, backends, common)?;
    let mut failed_gate = false;
    let mut combined = serde_json::Map::new();
    for (backend, outcome) in &results {
        print_eval_outcome(
            paths,
            *backend,
            outcome.page_count,
            outcome.searchable_count,
            outcome.k,
            &outcome.summary,
            outcome.delta.as_ref(),
        );
        if let Some(gate) = &outcome.gate {
            print_gate(*backend, gate);
            failed_gate |= !gate.passed();
        }
        combined.insert(
            (*backend).name().to_string(),
            serde_json::to_value(&outcome.summary).context("serialising the result")?,
        );
    }
    let text =
        serde_json::to_string_pretty(&combined).context("serialising the combined result")? + "\n";
    if let Some(path) = json {
        std::fs::write(path, &text).with_context(|| format!("writing {}", path.display()))?;
        eprintln!("wrote {}", path.display());
    } else {
        std::io::stdout().lock().write_all(text.as_bytes())?;
    }
    Ok(if failed_gate {
        ExitCode::from(EXIT_GATE)
    } else {
        ExitCode::SUCCESS
    })
}

fn print_eval_outcome(
    paths: &Paths,
    backend: BackendKind,
    page_count: usize,
    searchable_count: usize,
    k: usize,
    summary: &eval::EvalSummary,
    delta: Option<&eval::Delta>,
) {
    eprintln!(
        "{} [{backend}]: {page_count} pages, {searchable_count} searchable, k = {k}",
        paths.artifact.display()
    );
    eprint!("{}", eval::render_table(summary));
    if let Some(delta) = delta {
        eprint!("{}", eval::render_delta(delta));
    }
}

fn print_gate(backend: BackendKind, gate: &eval::Gate) {
    let verdict = if gate.passed() { "ok" } else { "FAILED" };
    eprintln!(
        "gate [{backend}]: tuning recall@5 {:.3} → {:.3}, drop {:+.3}, max {:.3}: {verdict}",
        gate.baseline,
        gate.current,
        gate.drop(),
        gate.max_drop
    );
}
