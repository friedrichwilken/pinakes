//! `pinakes` command-line interface: clap subcommands only; the logic lives in the library.
//!
//! Human output goes to stderr, data to stdout. Exit codes follow SPEC §4.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};

use pinakes::backend::BackendKind;
use pinakes::commands::{
    self, BackendEvalOptions, ClassifyOptions, DiffOptions, DuplicatesOptions, EmbedOptions,
    EvalOptions, GradeOptions, Paths, QueriesAddOptions, QueriesImportOptions, ReportOptions,
    ResolveOptions, UsageOptions,
};
use pinakes::decisions::Verdict;
use pinakes::duplicates::{self, DuplicateKind, Suggested};
use pinakes::embed::{Embedder, HttpEmbedder};
use pinakes::eval;
use pinakes::llm::UreqChatTransport;
use pinakes::manifest::Manifest;
use pinakes::residue::{ListFilter, Reason};
use pinakes::sources::GitHubFetcher;

/// Exit code for a failed `eval --gate`.
const EXIT_GATE: u8 = 2;
/// Exit code for `diff` differences and a stale manifest in `verify`.
const EXIT_DIFFERENCES: u8 = 3;
/// Exit code for a policy violation in `verify`, or a failed `queries check`.
const EXIT_POLICY: u8 = 4;

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

#[derive(Subcommand)]
enum QueriesCommand {
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
struct QueriesImportArgs {
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
struct QueriesAddArgs {
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

#[derive(Args)]
struct ResolveArgs {
    /// Artifact directory (default: `artifact` next to the config).
    #[arg(long)]
    artifact: Option<PathBuf>,
    /// Reproduce this manifest exactly instead of resolving the config.
    #[arg(long, value_name = "MANIFEST")]
    from_manifest: Option<PathBuf>,
}

#[derive(Args)]
struct VerifyArgs {
    /// Artifact directory to check as well (default: `artifact` next to the config).
    #[arg(long)]
    artifact: Option<PathBuf>,
    /// Skip the artifact check; only compare the manifest with the config and policy.
    #[arg(long)]
    no_artifact: bool,
}

#[derive(Subcommand)]
enum ResidueCommand {
    /// Print undecided residue entries as JSONL.
    List {
        /// Only entries from this source.
        #[arg(long)]
        source: Option<String>,
        /// Only entries with this reason.
        #[arg(long, value_name = "not_selected|unresolved_link|new_source|excluded")]
        reason: Option<String>,
        /// Also show `excluded` entries (kept out by `policy.deny`, a resolver's `exclude`, a
        /// decision, or an archived source): hidden by default since they are never candidates
        /// for `decide` (SPEC §2.4).
        #[arg(long)]
        include_excluded: bool,
    },
}

#[derive(Args)]
struct ReportArgs {
    /// The previous manifest, for added/removed/changed pages.
    #[arg(long, value_name = "MANIFEST")]
    old: Option<PathBuf>,
    /// The current manifest (default: manifest.json next to the config).
    #[arg(long, value_name = "MANIFEST")]
    new: Option<PathBuf>,
    /// Eval JSON for the previous corpus.
    #[arg(long, value_name = "EVAL")]
    eval_before: Option<PathBuf>,
    /// Eval JSON for the current corpus.
    #[arg(long, value_name = "EVAL")]
    eval_after: Option<PathBuf>,
    /// Read new page content from here for changed-page line counts (default: the artifact
    /// next to the config).
    #[arg(long, value_name = "DIR")]
    new_artifact: Option<PathBuf>,
    /// Read old page content from here before re-fetching the old commit.
    #[arg(long, value_name = "DIR")]
    old_artifact: Option<PathBuf>,
    /// A `pinakes usage --json` report to render as the "Usage" section (SPEC §15.3).
    #[arg(long, value_name = "USAGE")]
    usage: Option<PathBuf>,
}

#[derive(Args)]
struct DecideArgs {
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

#[derive(Args)]
struct EvalArgs {
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

#[derive(Args)]
struct EmbedArgs {
    /// Artifact directory (default: `artifact` next to the config).
    #[arg(long)]
    artifact: Option<PathBuf>,
    /// The embedding model, sent to the endpoint and recorded in `embeddings.json` (default:
    /// `PINAKES_EMBED_MODEL`).
    #[arg(long, value_name = "NAME")]
    model: Option<String>,
    /// `embeddings.bin` output path (default: `embeddings.bin` next to the config);
    /// `embeddings.json` is written next to it.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
    /// Texts per embeddings request.
    #[arg(long, default_value_t = pinakes::embed::DEFAULT_BATCH)]
    batch: usize,
}

#[derive(Args)]
struct DuplicatesArgs {
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

#[derive(Args)]
struct ClassifyArgs {
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

#[derive(Args)]
struct GradeArgs {
    /// Trail file to replay.
    #[arg(long, value_name = "FILE")]
    trail: PathBuf,
    /// Backend to fetch candidates from; only `bm25` is available today (SPEC §16.1 is not
    /// wired into `grade` yet).
    #[arg(long, default_value = "bm25")]
    backend: String,
    /// Candidates fetched per query.
    #[arg(long, value_name = "N")]
    k: Option<usize>,
    /// Model name; falls back to `PINAKES_LLM_MODEL`.
    #[arg(long)]
    model: Option<String>,
    /// Write graded rows to this file instead of stdout.
    #[arg(long, value_name = "OUT")]
    out: Option<PathBuf>,
}

#[derive(Args)]
struct UsageArgs {
    /// Trail file to read.
    #[arg(long, value_name = "FILE")]
    trail: PathBuf,
    /// Only trail entries in this window, e.g. `30d`, `12h`, `45m`, `90s`.
    #[arg(long, value_name = "DURATION")]
    since: Option<String>,
    /// Write the report as JSON to this file (in addition to the stderr summary).
    #[arg(long, value_name = "OUT")]
    json: Option<PathBuf>,
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

fn run_queries(paths: &Paths, command: QueriesCommand) -> Result<ExitCode> {
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

fn run_resolve(mut paths: Paths, args: ResolveArgs) -> Result<ExitCode> {
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

fn run_verify(mut paths: Paths, args: VerifyArgs) -> Result<ExitCode> {
    if let Some(dir) = args.artifact {
        paths.artifact = dir;
    }
    let report = commands::verify(&paths, !args.no_artifact)?;
    for line in &report.stale {
        eprintln!("stale: {line}");
    }
    for line in &report.violations {
        eprintln!("policy: {line}");
    }
    if !report.stale.is_empty() {
        Ok(ExitCode::from(EXIT_DIFFERENCES))
    } else if !report.violations.is_empty() {
        Ok(ExitCode::from(EXIT_POLICY))
    } else {
        eprintln!("ok");
        Ok(ExitCode::SUCCESS)
    }
}

fn run_residue_list(
    paths: &Paths,
    source: Option<&str>,
    reason: Option<&str>,
    include_excluded: bool,
) -> Result<ExitCode> {
    let reason = match reason {
        Some(text) => Some(Reason::parse(text).with_context(|| {
            format!(
                "unknown reason {text:?}: expected not_selected, unresolved_link, new_source or excluded"
            )
        })?),
        None => None,
    };
    let filter = ListFilter {
        source,
        reason,
        include_excluded,
    };
    let entries = commands::residue_list(paths, &filter)?;
    let mut out = std::io::stdout().lock();
    out.write_all(pinakes::residue::to_jsonl(&entries)?.as_bytes())?;
    Ok(ExitCode::SUCCESS)
}

fn run_decide(paths: &Paths, args: &DecideArgs) -> Result<ExitCode> {
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

fn run_diff(
    paths: &Paths,
    old: &std::path::Path,
    new: &std::path::Path,
    new_artifact: Option<PathBuf>,
    old_artifact: Option<PathBuf>,
) -> Result<ExitCode> {
    let old = Manifest::load(old)?;
    let new = Manifest::load(new)?;
    let options = DiffOptions {
        new_artifact: Some(new_artifact.unwrap_or_else(|| paths.artifact.clone())),
        old_artifact,
    };
    let diff = commands::diff(&old, &new, &options, &GitHubFetcher::new())?;
    let mut out = std::io::stdout().lock();
    out.write_all(diff.to_json()?.as_bytes())?;
    eprint!("{}", diff.summary());
    Ok(if diff.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_DIFFERENCES)
    })
}

fn run_report(paths: &Paths, args: ReportArgs) -> Result<ExitCode> {
    let options = ReportOptions {
        old: args.old,
        new: args.new,
        eval_before: args.eval_before,
        eval_after: args.eval_after,
        new_artifact: Some(args.new_artifact.unwrap_or_else(|| paths.artifact.clone())),
        old_artifact: args.old_artifact,
        usage: args.usage,
    };
    let text = commands::report(paths, &options, &GitHubFetcher::new())?;
    std::io::stdout().lock().write_all(text.as_bytes())?;
    Ok(ExitCode::SUCCESS)
}

fn run_duplicates(mut paths: Paths, args: DuplicatesArgs) -> Result<ExitCode> {
    if let Some(dir) = args.artifact {
        paths.artifact = dir;
    }
    let options = DuplicatesOptions {
        threshold: args.threshold,
        json: args.json.clone(),
    };
    let pairs = commands::duplicates(&paths, &options, &GitHubFetcher::new())?;
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

fn run_eval(mut paths: Paths, args: EvalArgs) -> Result<ExitCode> {
    if let Some(dir) = &args.artifact {
        paths.artifact.clone_from(dir);
    }
    if !args.compare.is_empty() {
        return run_eval_compare(&paths, args);
    }
    if args.backend.is_none()
        && args.backend_url.is_none()
        && args.embeddings.is_none()
        && !args.allow_stale
    {
        return run_eval_plain(&paths, args);
    }
    run_eval_with_backend(&paths, args)
}

/// The plain `eval` path: no `--backend`/`--compare`/backend-only flags at all, so it goes
/// through [`commands::eval`] exactly as before backend selection existed.
fn run_eval_plain(paths: &Paths, args: EvalArgs) -> Result<ExitCode> {
    let options = EvalOptions {
        queries: args.queries,
        k: args.k,
        json: args.json,
        gate: args.gate,
        with: args.with,
        without: args.without,
    };
    let outcome = commands::eval(paths, &options)?;
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

/// An embedder for `PINAKES_EMBED_URL`/`PINAKES_EMBED_KEY`, for backends that embed queries
/// (`dense`, `hybrid`). The model itself comes from `embeddings.json`, not from here.
fn embedder_from_env() -> Result<std::rc::Rc<dyn Embedder>> {
    let base = std::env::var("PINAKES_EMBED_URL")
        .context("PINAKES_EMBED_URL is not set (needed for --backend dense/hybrid)")?;
    let key = std::env::var("PINAKES_EMBED_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty());
    Ok(std::rc::Rc::new(HttpEmbedder::new(base, key)))
}

fn needs_embedder(kind: BackendKind) -> bool {
    matches!(kind, BackendKind::Dense | BackendKind::Hybrid)
}

/// `eval --backend NAME` (or any backend-only flag without `--compare`): through
/// [`commands::eval_backend`].
fn run_eval_with_backend(paths: &Paths, args: EvalArgs) -> Result<ExitCode> {
    let backend: BackendKind = args.backend.as_deref().unwrap_or("bm25").parse()?;
    let embedder = needs_embedder(backend)
        .then(embedder_from_env)
        .transpose()?;
    let json = args.json.clone();
    let options = BackendEvalOptions {
        queries: args.queries,
        k: args.k,
        json: json.clone(),
        gate: args.gate,
        with: args.with,
        without: args.without,
        backend,
        backend_url: args.backend_url,
        embeddings: args.embeddings,
        allow_stale: args.allow_stale,
        embedder,
    };
    let outcome = commands::eval_backend(paths, &options)?;
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
fn run_eval_compare(paths: &Paths, args: EvalArgs) -> Result<ExitCode> {
    let backends: Vec<BackendKind> = args
        .compare
        .iter()
        .map(|name| name.trim().parse())
        .collect::<std::result::Result<_, _>>()?;
    let embedder = backends
        .iter()
        .copied()
        .any(needs_embedder)
        .then(embedder_from_env)
        .transpose()?;
    let common = BackendEvalOptions {
        queries: args.queries,
        k: args.k,
        json: None,
        gate: args.gate,
        with: args.with,
        without: args.without,
        backend: BackendKind::default(),
        backend_url: args.backend_url,
        embeddings: args.embeddings,
        allow_stale: args.allow_stale,
        embedder,
    };
    let results = commands::eval_compare(paths, &backends, &common)?;
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
    if let Some(path) = &args.json {
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

fn run_embed(mut paths: Paths, args: EmbedArgs) -> Result<ExitCode> {
    if let Some(dir) = args.artifact {
        paths.artifact = dir;
    }
    let (embedder, model) = HttpEmbedder::from_env(args.model.as_deref())?;
    let options = EmbedOptions {
        model,
        batch: args.batch,
        out: args.out,
    };
    let outcome = commands::embed(&paths, &options, &embedder)?;
    eprintln!(
        "{}: embedded {} units ({} dims) -> {} and {}",
        paths.artifact.display(),
        outcome.units,
        outcome.dimension,
        outcome.bin_path.display(),
        outcome.json_path.display()
    );
    Ok(ExitCode::SUCCESS)
}

fn run_classify(paths: &Paths, args: ClassifyArgs) -> Result<ExitCode> {
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

fn run_grade(paths: &Paths, args: GradeArgs) -> Result<ExitCode> {
    let options = GradeOptions {
        trail: args.trail,
        backend: Some(args.backend),
        k: args.k,
        model: args.model,
        out: args.out.clone(),
    };
    let transport = UreqChatTransport::new();
    let outcome = commands::grade(paths, &options, &transport)?;
    eprintln!(
        "{} queries, {} graded rows",
        outcome.queries,
        outcome.rows.len()
    );
    if let Some(path) = &args.out {
        eprintln!("wrote {}", path.display());
    } else {
        let text = pinakes::grade::to_jsonl(&outcome.rows).context("serialising graded rows")?;
        std::io::stdout().lock().write_all(text.as_bytes())?;
    }
    Ok(ExitCode::SUCCESS)
}

fn run_usage(paths: &Paths, args: UsageArgs) -> Result<ExitCode> {
    let options = UsageOptions {
        trail: args.trail,
        since: args.since,
        json: args.json.clone(),
    };
    let usage = commands::usage(paths, &options)?;
    eprintln!(
        "{} never retrieved, {} retrieved but never cited, {} uncited queries",
        usage.never_retrieved.len(),
        usage.retrieved_never_cited.len(),
        usage.uncited_queries.len()
    );
    if let Some(path) = &args.json {
        eprintln!("wrote {}", path.display());
    } else {
        let text = usage.to_json().context("serialising the usage report")?;
        std::io::stdout().lock().write_all(text.as_bytes())?;
    }
    Ok(ExitCode::SUCCESS)
}
