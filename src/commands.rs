//! Command orchestration: the library side of every `pinakes` subcommand.
//!
//! `main.rs` only parses arguments and maps results to exit codes; everything that reads or
//! writes files lives here so it can be tested without spawning the binary.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::artifact::{self, ArtifactError, MANIFEST_FILE, Problem};
use crate::backend::{self, Backend, BackendConfig, BackendError, BackendKind};
use crate::classify::{self, ClassifyError, DuplicateLookup, PageFacts};
use crate::config::{ArchivedPolicy, Config, ConfigError, RepoSlug};
use crate::decisions::{self, Decision, DecisionError, Expired, Verdict};
use crate::diff::{self, Diff};
use crate::duplicates::{self, DuplicateContext, DuplicatePair, DuplicatesError};
use crate::embed::{self, EmbedError, Embedder};
use crate::eval::{self, Delta, EvalError, EvalSummary, Gate};
use crate::grade::{self, GradeError, GradedRow};
use crate::index::{self, Index, IndexError, Page, Priorities};
use crate::llm::{ChatError, ChatTransport, LlmConfig};
use crate::manifest::{
    Manifest, ManifestError, ManifestSource, PageEntry, SelectedBy, now_rfc3339, split_page_id,
};
use crate::queries::{self, CheckReport, GradedQuery, NewQuery, QueriesError};
use crate::render::{self, RenderError};
use crate::report::{self, ReportInput};
use crate::residue::{self, ListFilter, Reason, ResidueEntry, ResidueError};
use crate::resolve::{self, ResolveContext, ResolveError};
use crate::sources::{Checkout, Fetcher, SourceError, fetch_checkout};
use crate::text::{sha256_hex, strip_frontmatter, title_of};
use crate::trail::{self, TrailEntry, TrailError};
use crate::usage::{self, Usage, UsageError};

/// Errors raised by any command.
#[derive(Debug, Error)]
pub enum CommandError {
    /// Bad or unreadable config.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// Bad or unreadable manifest.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// Bad or unreadable residue file.
    #[error(transparent)]
    Residue(#[from] ResidueError),
    /// Bad or unreadable decisions file.
    #[error(transparent)]
    Decision(#[from] DecisionError),
    /// Download or extraction failed.
    #[error("source {name}: {source}")]
    Source {
        /// Source name.
        name: String,
        /// Underlying error.
        #[source]
        source: SourceError,
    },
    /// A resolver failed.
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    /// A render step failed.
    #[error(transparent)]
    Render(#[from] RenderError),
    /// Writing the artifact failed.
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    /// A filesystem operation failed.
    #[error("{path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A manifest records a slug that is not `owner/repo`.
    #[error("source {name}: invalid repo slug {repo:?} in manifest")]
    BadSlug {
        /// Source name.
        name: String,
        /// The recorded slug.
        repo: String,
    },
    /// The tarball fetched for a recorded commit reports a different commit.
    #[error("source {name}: fetched {actual} but the manifest records {expected}")]
    CommitMismatch {
        /// Source name.
        name: String,
        /// Commit recorded in the manifest.
        expected: String,
        /// Commit the tarball reported.
        actual: String,
    },
    /// `decide` was given an id that is neither residue nor a page.
    #[error("unknown id {0}: not in residue.jsonl or manifest.json")]
    UnknownId(String),
    /// A JSON value could not be produced.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Bad or unreadable eval result.
    #[error(transparent)]
    Eval(#[from] EvalError),
    /// The artifact could not be indexed.
    #[error(transparent)]
    Index(#[from] IndexError),
    /// `eval` has no query file: none given and no `eval.queries` in the config.
    #[error("no query file: pass --queries or set eval.queries in the config")]
    NoQueries,
    /// `queries add` or `queries check` failed.
    #[error(transparent)]
    Queries(#[from] QueriesError),
    /// Bad or unreadable `duplicates.jsonl`.
    #[error(transparent)]
    Duplicates(#[from] DuplicatesError),
    /// A retriever backend (SPEC §16) failed to build or search.
    #[error(transparent)]
    Backend(#[from] BackendError),
    /// Embedding, or reading/writing the embeddings file pair, failed.
    #[error(transparent)]
    Embed(#[from] EmbedError),
    /// `eval --compare` was given no backend names.
    #[error("--compare needs at least one backend name")]
    EmptyCompare,
    /// `classify` failed, including talking to the model.
    #[error(transparent)]
    Classify(#[from] ClassifyError),
    /// Building the model configuration failed (e.g. `PINAKES_LLM_URL` is not set).
    #[error(transparent)]
    Llm(#[from] ChatError),
    /// Bad or unreadable `trail.jsonl`.
    #[error(transparent)]
    Trail(#[from] TrailError),
    /// `grade` failed, including talking to the model or the backend.
    #[error(transparent)]
    Grade(#[from] GradeError),
    /// Bad or unreadable usage report, or an invalid `--since`.
    #[error(transparent)]
    Usage(#[from] UsageError),
}

/// File locations shared by the commands; every path is taken as given (no implicit cwd magic
/// beyond the defaults the CLI fills in).
#[derive(Debug, Clone)]
pub struct Paths {
    /// `pinakes.yaml`.
    pub config: PathBuf,
    /// The committed `manifest.json`.
    pub manifest: PathBuf,
    /// `residue.jsonl`.
    pub residue: PathBuf,
    /// `decisions.jsonl`.
    pub decisions: PathBuf,
    /// The artifact directory.
    pub artifact: PathBuf,
    /// `duplicates.jsonl`, written by `resolve` next to `residue.jsonl` (SPEC §11).
    pub duplicates: PathBuf,
    /// `embeddings.bin`, written by `embed` (SPEC §16.2); `embeddings.json` sits next to it.
    pub embeddings: PathBuf,
}

impl Paths {
    /// Defaults relative to the config file's directory.
    pub fn for_config(config: &Path) -> Paths {
        let dir = config.parent().map(Path::to_path_buf).unwrap_or_default();
        Paths {
            config: config.to_path_buf(),
            manifest: dir.join("manifest.json"),
            residue: dir.join("residue.jsonl"),
            decisions: dir.join("decisions.jsonl"),
            artifact: dir.join("artifact"),
            duplicates: dir.join("duplicates.jsonl"),
            embeddings: dir.join("embeddings.bin"),
        }
    }

    /// Directory of the config file.
    pub fn config_dir(&self) -> PathBuf {
        self.config
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }
}

/// Options for `resolve`.
#[derive(Debug, Clone)]
pub struct ResolveOptions {
    /// Reproduce this manifest instead of resolving the config.
    pub from_manifest: Option<PathBuf>,
    /// Timestamp to record; defaults to now (ignored with `from_manifest`).
    pub generated_at: Option<String>,
}

/// What `resolve` produced, for the human summary.
#[derive(Debug)]
pub struct ResolveOutcome {
    /// The manifest that was written.
    pub manifest: Manifest,
    /// The residue entries that were written.
    pub residue: Vec<ResidueEntry>,
    /// Decisions whose page hash no longer matches.
    pub expired: Vec<Expired>,
    /// Non-fatal observations (archived sources, dropped sources).
    pub warnings: Vec<String>,
    /// The duplicate pairs written to `duplicates.jsonl` (SPEC §11).
    pub duplicates: Vec<DuplicatePair>,
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> CommandError + '_ {
    move |source| CommandError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn checkout_dir(work: &Path, name: &str) -> PathBuf {
    work.join(name)
}

/// Run `resolve`: fetch every source, select pages, write the artifact, manifest and residue.
pub fn resolve(
    paths: &Paths,
    options: &ResolveOptions,
    fetcher: &dyn Fetcher,
) -> Result<ResolveOutcome, CommandError> {
    let work = tempfile::tempdir().map_err(io_err(Path::new("temp dir")))?;
    let outcome = match &options.from_manifest {
        Some(manifest_path) => reproduce(paths, manifest_path, fetcher, work.path())?,
        None => resolve_fresh(paths, options, fetcher, work.path())?,
    };
    Ok(outcome)
}

fn resolve_fresh(
    paths: &Paths,
    options: &ResolveOptions,
    fetcher: &dyn Fetcher,
    work: &Path,
) -> Result<ResolveOutcome, CommandError> {
    let config = Config::load(&paths.config)?;
    let deny = config.deny_set()?;
    let all_decisions = decisions::read_jsonl(&paths.decisions)?;
    let effective = decisions::effective(&all_decisions);
    let previous = if paths.manifest.is_file() {
        Some(Manifest::load(&paths.manifest)?)
    } else {
        None
    };
    let config_dir = paths.config_dir();

    let mut manifest = Manifest::new(options.generated_at.clone().unwrap_or_else(now_rfc3339));
    let mut all_residue = Vec::new();
    let mut warnings = Vec::new();
    let mut checkouts = BTreeMap::new();

    for source in &config.sources {
        let slug = source.slug();
        let archived = fetcher.archived(&slug);
        let dropped = archived == Some(true) && config.policy.archived == ArchivedPolicy::Drop;
        if archived == Some(true) {
            if dropped {
                warnings.push(format!("{}: repository is archived; dropped", source.name));
            } else {
                warnings.push(format!("{}: repository is archived", source.name));
            }
        }
        let dest = checkout_dir(work, &source.name);
        let checkout = fetch_checkout(fetcher, &slug, &source.git_ref, &dest).map_err(|e| {
            CommandError::Source {
                name: source.name.clone(),
                source: e,
            }
        })?;
        let ctx = ResolveContext {
            deny: &deny,
            deny_patterns: &config.policy.deny,
            decisions: &effective,
            config_dir: &config_dir,
            is_new_source: previous
                .as_ref()
                .is_some_and(|p| !p.sources.contains_key(&source.name)),
        };
        let resolved = resolve::resolve_source(source, &checkout, &ctx)?;
        if dropped {
            // The whole source is left out of the corpus; its would-be pages are still
            // accounted for, as residue with `source:archived` (SPEC §2.1, §2.4).
            all_residue.extend(resolve::archived_residue(source, &checkout, resolved));
            continue;
        }
        let residue_paths = resolved
            .residue
            .iter()
            .filter(|r| r.reason != Reason::UnresolvedLink)
            .map(|r| r.path.clone())
            .collect();
        let (pages, unrendered, render) = match &source.render {
            Some(render) => {
                let output = render::render_source(
                    &source.name,
                    render,
                    &checkout,
                    &resolved.pages,
                    &config_dir,
                )?;
                (output.pages, output.unrendered, Some(output.recorded))
            }
            None => (resolved.pages, Vec::new(), None),
        };
        manifest.sources.insert(
            source.name.clone(),
            ManifestSource {
                repo: slug.to_string(),
                repo_url: source.repo.clone(),
                git_ref: source.git_ref.clone(),
                commit: checkout.commit.clone(),
                archived,
                resolver: source.resolver.kind().to_string(),
                pages,
                residue: residue_paths,
                unresolved: resolved.unresolved,
                unrendered,
                render,
            },
        );
        all_residue.extend(resolved.residue);
        checkouts.insert(source.name.clone(), checkout);
    }

    let expired = decisions::expired(&effective, |id| {
        manifest.page(id).map(|p| p.sha256.clone()).or_else(|| {
            all_residue
                .iter()
                .find(|r| r.id == id)
                .map(|r| r.sha256.clone())
        })
    });
    let duplicates = write_outputs(paths, &manifest, &all_residue, &checkouts)?;
    Ok(ResolveOutcome {
        manifest,
        residue: all_residue,
        expired,
        warnings,
        duplicates,
    })
}

fn reproduce(
    paths: &Paths,
    manifest_path: &Path,
    fetcher: &dyn Fetcher,
    work: &Path,
) -> Result<ResolveOutcome, CommandError> {
    let manifest = Manifest::load(manifest_path)?;
    let mut checkouts = BTreeMap::new();
    let mut all_residue = Vec::new();
    for (name, source) in &manifest.sources {
        let slug = RepoSlug::from_slug(&source.repo).ok_or_else(|| CommandError::BadSlug {
            name: name.clone(),
            repo: source.repo.clone(),
        })?;
        let dest = checkout_dir(work, name);
        let checkout = fetch_checkout(fetcher, &slug, &source.commit, &dest).map_err(|e| {
            CommandError::Source {
                name: name.clone(),
                source: e,
            }
        })?;
        if checkout.commit != source.commit {
            return Err(CommandError::CommitMismatch {
                name: name.clone(),
                expected: source.commit.clone(),
                actual: checkout.commit,
            });
        }
        if let Some(render) = &source.render {
            let selected = selected_for_render(source);
            render::render_source(name, render, &checkout, &selected, &paths.config_dir())?;
        }
        all_residue.extend(recorded_residue(name, source, &checkout)?);
        checkouts.insert(name.clone(), checkout);
    }
    let duplicates = write_outputs(paths, &manifest, &all_residue, &checkouts)?;
    Ok(ResolveOutcome {
        manifest,
        residue: all_residue,
        expired: Vec::new(),
        warnings: Vec::new(),
        duplicates,
    })
}

/// Rebuild the pre-render selection [`render::render_source`] needs from a recorded source's
/// post-render pages and its `unrendered` list: one synthetic entry per distinct selected path
/// (`rendered_from`, plus every `unrendered` path), so `resolve --from-manifest` can re-run a
/// render step without the original config's resolver at hand. Only `selected_by` survives into
/// a rendered page's manifest entry, so the other fields are left blank.
fn selected_for_render(source: &ManifestSource) -> BTreeMap<String, PageEntry> {
    let mut selected = BTreeMap::new();
    let blank = |selected_by| PageEntry {
        sha256: String::new(),
        title: String::new(),
        doc_type: String::new(),
        section: String::new(),
        selected_by,
        rendered_from: None,
    };
    for entry in source.pages.values() {
        if let Some(source_path) = &entry.rendered_from {
            selected
                .entry(source_path.clone())
                .or_insert_with(|| blank(entry.selected_by));
        }
    }
    for path in &source.unrendered {
        selected
            .entry(path.clone())
            .or_insert_with(|| blank(SelectedBy::Include));
    }
    selected
}

/// Rebuild residue entries for a recorded source from the files in its checkout.
fn recorded_residue(
    name: &str,
    source: &ManifestSource,
    checkout: &Checkout,
) -> Result<Vec<ResidueEntry>, CommandError> {
    let mut entries = Vec::new();
    for path in &source.residue {
        let full = checkout.root.join(path);
        let bytes = std::fs::read(&full).map_err(io_err(&full))?;
        let text = String::from_utf8_lossy(&bytes);
        entries.push(ResidueEntry {
            id: crate::manifest::page_id(name, path),
            source: name.to_string(),
            path: path.clone(),
            reason: Reason::NotSelected,
            sha256: sha256_hex(&bytes),
            title: title_of("", &text),
            excerpt: residue::excerpt(strip_frontmatter(&text), residue::EXCERPT_TOKENS),
            context: String::new(),
            url: source.page_url(path).unwrap_or_default(),
            rule: Some(residue::Rule::reproduced()),
        });
    }
    for path in &source.unresolved {
        entries.push(ResidueEntry {
            id: crate::manifest::page_id(name, path),
            source: name.to_string(),
            path: path.clone(),
            reason: Reason::UnresolvedLink,
            sha256: String::new(),
            title: String::new(),
            excerpt: String::new(),
            context: String::new(),
            url: source.page_url(path).unwrap_or_default(),
            rule: Some(residue::Rule::reproduced()),
        });
    }
    Ok(entries)
}

fn write_outputs(
    paths: &Paths,
    manifest: &Manifest,
    all_residue: &[ResidueEntry],
    checkouts: &BTreeMap<String, Checkout>,
) -> Result<Vec<DuplicatePair>, CommandError> {
    artifact::materialise(&paths.artifact, manifest, checkouts)?;
    manifest.save(&paths.manifest)?;
    residue::write_jsonl(&paths.residue, all_residue)?;
    let pairs = compute_duplicates(paths, Some(manifest), duplicates::DEFAULT_THRESHOLD)?;
    duplicates::write_jsonl(&paths.duplicates, &pairs)?;
    Ok(pairs)
}

/// The sha256 of a page's original bytes: from the manifest when it has an entry for `id`,
/// otherwise read straight from the artifact (a manifest-less artifact, or a page the manifest
/// does not know about).
fn sha256_of_page(paths: &Paths, manifest: Option<&Manifest>, id: &str) -> Option<String> {
    if let Some(entry) = manifest.and_then(|m| m.page(id)) {
        return Some(entry.sha256.clone());
    }
    let (source, path) = split_page_id(id)?;
    let bytes = std::fs::read(paths.artifact.join(source).join(path)).ok()?;
    Some(sha256_hex(&bytes))
}

/// Find duplicate pairs in the artifact at `paths.artifact` (SPEC §11). `manifest`, when given,
/// supplies exact `sha256`, `selected_by` and page urls for the winner rule and the reported
/// pairs; without one (a manifest-less artifact) exact duplicates still work from the file
/// bytes, and the winner rule falls back to source priority alone.
fn compute_duplicates(
    paths: &Paths,
    manifest: Option<&Manifest>,
    threshold: f64,
) -> Result<Vec<DuplicatePair>, CommandError> {
    let priorities = if paths.config.is_file() {
        Priorities::from_config(&Config::load(&paths.config)?)
    } else {
        Priorities::default()
    };
    let pages = index::load_pages(&paths.artifact, &priorities)?;
    let sha256 = |id: &str| sha256_of_page(paths, manifest, id);
    let selected_by = |id: &str| manifest.and_then(|m| m.page(id)).map(|p| p.selected_by);
    let context = DuplicateContext {
        sha256: &sha256,
        selected_by: &selected_by,
    };
    let mut pairs = duplicates::find_duplicates(&pages, &context, threshold);
    if let Some(manifest) = manifest {
        for pair in &mut pairs {
            pair.canonical_url = manifest.page_url(&pair.canonical).unwrap_or_default();
            pair.duplicate_url = manifest.page_url(&pair.duplicate).unwrap_or_default();
        }
    }
    Ok(pairs)
}

/// Options for `duplicates`.
#[derive(Debug, Clone)]
pub struct DuplicatesOptions {
    /// Minimum Jaccard similarity for a near-duplicate pair.
    pub threshold: f64,
    /// Write the pairs there instead of returning them for the caller to print.
    pub json: Option<PathBuf>,
}

impl Default for DuplicatesOptions {
    fn default() -> DuplicatesOptions {
        DuplicatesOptions {
            threshold: duplicates::DEFAULT_THRESHOLD,
            json: None,
        }
    }
}

/// Run `duplicates`: find exact, mirror and near-duplicate pairs in the artifact (SPEC §11).
/// Uses the committed manifest when present for `selected_by` and page urls; this never touches
/// the network.
pub fn duplicates(
    paths: &Paths,
    options: &DuplicatesOptions,
) -> Result<Vec<DuplicatePair>, CommandError> {
    let manifest = if paths.manifest.is_file() {
        Some(Manifest::load(&paths.manifest)?)
    } else {
        None
    };
    let pairs = compute_duplicates(paths, manifest.as_ref(), options.threshold)?;
    if let Some(path) = &options.json {
        duplicates::write_jsonl(path, &pairs)?;
    }
    Ok(pairs)
}

/// The outcome of `verify`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    /// Reasons the manifest is stale (exit 3).
    pub stale: Vec<String>,
    /// Policy violations (exit 4).
    pub violations: Vec<String>,
}

impl VerifyReport {
    /// Whether verification passed.
    pub fn ok(&self) -> bool {
        self.stale.is_empty() && self.violations.is_empty()
    }
}

/// Run `verify`: the committed manifest must match the config and the artifact (when present),
/// and every policy must hold.
pub fn verify(paths: &Paths, check_artifact: bool) -> Result<VerifyReport, CommandError> {
    let config = Config::load(&paths.config)?;
    let manifest = Manifest::load(&paths.manifest)?;
    let deny = config.deny_set()?;
    let mut report = VerifyReport::default();

    for source in &config.sources {
        match manifest.sources.get(&source.name) {
            None => report
                .stale
                .push(format!("{}: in config but not in manifest", source.name)),
            Some(recorded) => {
                let slug = source.slug().to_string();
                if recorded.repo != slug || recorded.git_ref != source.git_ref {
                    report.stale.push(format!(
                        "{}: manifest records {}@{} but config says {}@{}",
                        source.name, recorded.repo, recorded.git_ref, slug, source.git_ref
                    ));
                }
                if recorded.resolver != source.resolver.kind() {
                    report.stale.push(format!(
                        "{}: manifest resolver {} differs from config {}",
                        source.name,
                        recorded.resolver,
                        source.resolver.kind()
                    ));
                }
            }
        }
    }
    for name in manifest.sources.keys() {
        if config.source(name).is_none() {
            report
                .stale
                .push(format!("{name}: in manifest but not in config"));
        }
    }
    if check_artifact {
        for problem in artifact::check(&paths.artifact, &manifest) {
            let text = match &problem {
                Problem::ManifestDiffers => {
                    format!(
                        "{}: {problem}",
                        paths.artifact.join(MANIFEST_FILE).display()
                    )
                }
                _ => problem.to_string(),
            };
            report.stale.push(text);
        }
    }

    for (name, source) in &manifest.sources {
        if source.pages.len() < config.policy.min_pages_per_source {
            report.violations.push(format!(
                "{name}: {} pages, policy requires at least {}",
                source.pages.len(),
                config.policy.min_pages_per_source
            ));
        }
        if source.archived == Some(true) && config.policy.archived == ArchivedPolicy::Drop {
            report.violations.push(format!(
                "{name}: repository is archived and policy says drop"
            ));
        }
        for path in source.pages.keys() {
            if deny.is_match(path) {
                report
                    .violations
                    .push(format!("{name}::{path}: matches policy.deny"));
            }
        }
    }
    Ok(report)
}

/// Run `residue list`: entries matching the filter that have no active decision.
pub fn residue_list(
    paths: &Paths,
    filter: &ListFilter<'_>,
) -> Result<Vec<ResidueEntry>, CommandError> {
    let entries = residue::read_jsonl(&paths.residue)?;
    let effective = decisions::effective(&decisions::read_jsonl(&paths.decisions)?);
    Ok(residue::list(&entries, filter, &effective)
        .into_iter()
        .cloned()
        .collect())
}

/// Run `decide`: append a decision for `id`, taking the hash from residue or the manifest.
pub fn decide(
    paths: &Paths,
    id: &str,
    verdict: Verdict,
    reason: &str,
    by: &str,
    at: Option<String>,
) -> Result<Decision, CommandError> {
    let from_residue = if paths.residue.is_file() {
        residue::read_jsonl(&paths.residue)?
            .into_iter()
            .find(|e| e.id == id && !e.sha256.is_empty())
            .map(|e| e.sha256)
    } else {
        None
    };
    let from_manifest = || -> Result<Option<String>, CommandError> {
        if !paths.manifest.is_file() {
            return Ok(None);
        }
        Ok(Manifest::load(&paths.manifest)?
            .page(id)
            .map(|p| p.sha256.clone()))
    };
    let sha256 = match from_residue {
        Some(hash) => hash,
        None => from_manifest()?.ok_or_else(|| CommandError::UnknownId(id.to_string()))?,
    };
    let decision = Decision {
        id: id.to_string(),
        sha256,
        decision: verdict,
        reason: reason.to_string(),
        by: by.to_string(),
        at: at.unwrap_or_else(now_rfc3339),
    };
    decisions::append(&paths.decisions, &decision)?;
    Ok(decision)
}

/// Where `diff` reads page content to compute `lines_added`/`lines_removed` (SPEC §13).
#[derive(Debug, Clone, Default)]
pub struct DiffOptions {
    /// Read the new page text from here (typically the freshly resolved artifact); `None`
    /// leaves every changed page's line counts at zero.
    pub new_artifact: Option<PathBuf>,
    /// Read the old page text from here when present; otherwise its source is re-fetched at
    /// the old commit through the fetcher passed to [`diff()`].
    pub old_artifact: Option<PathBuf>,
}

/// Run `diff`: compare two manifests and, where content is reachable, add per-page line counts
/// (SPEC §13). The old version of a changed page is read from `options.old_artifact` when
/// present there; otherwise its source is re-fetched at the old commit through `fetcher` (the
/// same trait `resolve --from-manifest` uses, so tests can supply a fake fetcher) into a
/// temporary checkout, fetched at most once per source.
pub fn diff(
    old: &Manifest,
    new: &Manifest,
    options: &DiffOptions,
    fetcher: &dyn Fetcher,
) -> Result<Diff, CommandError> {
    let mut computed = Diff::compute(old, new);
    if computed.changed.is_empty() {
        return Ok(computed);
    }
    let mut refetched: BTreeMap<String, tempfile::TempDir> = BTreeMap::new();
    let mut old_text = |source: &str, path: &str| -> Option<String> {
        if let Some(dir) = &options.old_artifact
            && let Ok(text) = std::fs::read_to_string(dir.join(source).join(path))
        {
            return Some(text);
        }
        let old_source = old.sources.get(source)?;
        let checkout_dir = match refetched.entry(source.to_string()) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                let work = tempfile::tempdir().ok()?;
                let slug = RepoSlug::from_slug(&old_source.repo)?;
                fetch_checkout(fetcher, &slug, &old_source.commit, work.path()).ok()?;
                entry.insert(work)
            }
        };
        std::fs::read_to_string(checkout_dir.path().join(path)).ok()
    };
    let new_text = |source: &str, path: &str| -> Option<String> {
        options
            .new_artifact
            .as_deref()
            .and_then(|dir| std::fs::read_to_string(dir.join(source).join(path)).ok())
    };
    diff::annotate_line_counts(&mut computed, |id| {
        let Some((source, path)) = split_page_id(id) else {
            return (None, None);
        };
        (old_text(source, path), new_text(source, path))
    });
    Ok(computed)
}

/// Inputs for `report`, as file paths; `None` falls back to the workspace defaults or nothing.
#[derive(Debug, Clone, Default)]
pub struct ReportOptions {
    /// The previous manifest.
    pub old: Option<PathBuf>,
    /// The current manifest (default: the committed one).
    pub new: Option<PathBuf>,
    /// Eval result on the previous corpus.
    pub eval_before: Option<PathBuf>,
    /// Eval result on the current corpus.
    pub eval_after: Option<PathBuf>,
    /// Read new page content from here for changed-page line counts (SPEC §13); typically the
    /// artifact next to the config.
    pub new_artifact: Option<PathBuf>,
    /// Read old page content from here before re-fetching it, as in [`DiffOptions`].
    pub old_artifact: Option<PathBuf>,
    /// A `pinakes usage --json` report to render as the "Usage" section (SPEC §15.3).
    pub usage: Option<PathBuf>,
}

/// Run `report`: render the Markdown PR body from manifests, residue, decisions, duplicates,
/// eval and usage files. `fetcher` is only used, per [`diff()`], to re-fetch a changed page's
/// old text when `options.old_artifact` does not already have it.
pub fn report(
    paths: &Paths,
    options: &ReportOptions,
    fetcher: &dyn Fetcher,
) -> Result<String, CommandError> {
    let new = Manifest::load(options.new.as_deref().unwrap_or(&paths.manifest))?;
    let old = options.old.as_deref().map(Manifest::load).transpose()?;
    let residue = if paths.residue.is_file() {
        residue::read_jsonl(&paths.residue)?
    } else {
        Vec::new()
    };
    let decisions = decisions::read_jsonl(&paths.decisions)?;
    let duplicate_pairs = duplicates::read_jsonl(&paths.duplicates)?;
    let eval_before = options
        .eval_before
        .as_deref()
        .map(EvalSummary::load)
        .transpose()?;
    let eval_after = options
        .eval_after
        .as_deref()
        .map(EvalSummary::load)
        .transpose()?;
    let usage = options.usage.as_deref().map(Usage::load).transpose()?;
    let computed_diff = old
        .as_ref()
        .map(|old| {
            let diff_options = DiffOptions {
                new_artifact: options.new_artifact.clone(),
                old_artifact: options.old_artifact.clone(),
            };
            diff(old, &new, &diff_options, fetcher)
        })
        .transpose()?;
    Ok(report::render(ReportInput {
        old: old.as_ref(),
        new: &new,
        diff: computed_diff.as_ref(),
        residue: &residue,
        decisions: &decisions,
        eval_before: eval_before.as_ref(),
        eval_after: eval_after.as_ref(),
        duplicates: &duplicate_pairs,
        usage: usage.as_ref(),
    }))
}

/// Result list length when neither `--k` nor `eval.k` is given.
pub const DEFAULT_K: usize = 10;
/// Gate tolerance when the config has no `eval` section.
pub const DEFAULT_MAX_RECALL_DROP: f64 = 0.05;

/// Options for `eval`.
#[derive(Debug, Clone, Default)]
pub struct EvalOptions {
    /// Query file (default: `eval.queries` from the config, relative to it).
    pub queries: Option<PathBuf>,
    /// Result list length (default: `eval.k` from the config, else [`DEFAULT_K`]).
    pub k: Option<usize>,
    /// Write the result JSON here.
    pub json: Option<PathBuf>,
    /// Baseline result to gate the tuning recall@5 against.
    pub gate: Option<PathBuf>,
    /// Residue pages (`<source>::<path>`) to add from `_residue` before measuring.
    pub with: Vec<String>,
    /// Pages to remove before measuring.
    pub without: Vec<String>,
}

/// What `eval` produced.
#[derive(Debug)]
pub struct EvalOutcome {
    /// The result (after `--with`/`--without` when given).
    pub summary: EvalSummary,
    /// Pages in the measured corpus, mirrors included.
    pub page_count: usize,
    /// Pages in the search corpus.
    pub searchable_count: usize,
    /// Result list length used.
    pub k: usize,
    /// The gate outcome when `--gate` was given.
    pub gate: Option<Gate>,
    /// The delta when `--with`/`--without` was given.
    pub delta: Option<Delta>,
}

/// The query file: `--queries` when given, else `eval.queries` from the config.
fn resolve_queries_path(
    paths: &Paths,
    eval_config: Option<&crate::config::EvalConfig>,
    queries: Option<&Path>,
) -> Result<PathBuf, CommandError> {
    match queries {
        Some(path) => Ok(path.to_path_buf()),
        None => eval_config
            .map(|e| paths.config_dir().join(&e.queries))
            .ok_or(CommandError::NoQueries),
    }
}

/// Run `eval`: index the artifact, run the queries, optionally gate against a baseline and
/// measure the effect of adding residue pages or removing pages.
///
/// The config is optional: without one, the query file must be given, every source gets the
/// default priority of [`Priorities`] (so no page is a mirror), `k` defaults to [`DEFAULT_K`]
/// and the gate tolerance to [`DEFAULT_MAX_RECALL_DROP`].
pub fn eval(paths: &Paths, options: &EvalOptions) -> Result<EvalOutcome, CommandError> {
    let config = if paths.config.is_file() {
        Some(Config::load(&paths.config)?)
    } else {
        None
    };
    let eval_config = config.as_ref().and_then(|c| c.eval.as_ref());
    let queries_path = resolve_queries_path(paths, eval_config, options.queries.as_deref())?;
    let k = options
        .k
        .or_else(|| eval_config.map(|e| e.k))
        .unwrap_or(DEFAULT_K);
    let max_drop = eval_config.map_or(DEFAULT_MAX_RECALL_DROP, |e| e.max_recall_drop);
    let priorities = config
        .as_ref()
        .map(Priorities::from_config)
        .unwrap_or_default();
    let queries = eval::load_queries(&queries_path)?;
    let pages = index::load_pages(&paths.artifact, &priorities)?;

    let (index, summary, delta) = if options.with.is_empty() && options.without.is_empty() {
        let index = Index::from_pages(pages)?;
        let summary = eval::evaluate(&index, &queries, k)?;
        (index, summary, None)
    } else {
        let before = eval::evaluate(&Index::from_pages(pages.clone())?, &queries, k)?;
        let index = Index::from_pages(adjust_pages(pages, &paths.artifact, &priorities, options)?)?;
        let after = eval::evaluate(&index, &queries, k)?;
        let delta = eval::delta(&before, &after);
        (index, after, Some(delta))
    };
    let gate = match &options.gate {
        Some(path) => Some(eval::gate(&summary, &EvalSummary::load(path)?, max_drop)),
        None => None,
    };
    if let Some(path) = &options.json {
        summary.save(path)?;
    }
    Ok(EvalOutcome {
        summary,
        page_count: index.page_count(),
        searchable_count: index.searchable_count(),
        k,
        gate,
        delta,
    })
}

/// Apply `--with` (add residue pages) and `--without` (remove pages) to the page list.
fn adjust_pages(
    mut pages: Vec<Page>,
    artifact: &Path,
    priorities: &Priorities,
    options: &EvalOptions,
) -> Result<Vec<Page>, CommandError> {
    for id in &options.with {
        if pages.iter().any(|p| &p.id == id) {
            return Err(IndexError::AlreadyPresent(id.clone()).into());
        }
        pages.push(index::load_residue_page(artifact, id, priorities)?);
    }
    for id in &options.without {
        let before = pages.len();
        pages.retain(|p| &p.id != id);
        if pages.len() == before {
            return Err(IndexError::UnknownPage(id.clone()).into());
        }
    }
    Ok(pages)
}

/// Options for `queries add`.
#[derive(Debug, Clone, Default)]
pub struct QueriesAddOptions {
    /// Query file (default: `eval.queries` from the config, relative to it).
    pub queries: Option<PathBuf>,
    /// Query id.
    pub id: String,
    /// The query text.
    pub query: String,
    /// Page ids, id prefixes, or the legacy `<source>/<path>` form.
    pub expected: Vec<String>,
    /// Query kind, possibly empty.
    pub kind: String,
    /// Held out from tuning decisions.
    pub holdout: bool,
}

/// Run `queries add`: append a row after checking `expected` against the committed manifest.
pub fn queries_add(
    paths: &Paths,
    options: &QueriesAddOptions,
) -> Result<eval::Query, CommandError> {
    let config = if paths.config.is_file() {
        Some(Config::load(&paths.config)?)
    } else {
        None
    };
    let eval_config = config.as_ref().and_then(|c| c.eval.as_ref());
    let queries_path = resolve_queries_path(paths, eval_config, options.queries.as_deref())?;
    let manifest = Manifest::load(&paths.manifest)?;
    let new = NewQuery {
        id: options.id.clone(),
        query: options.query.clone(),
        expected: options.expected.clone(),
        kind: options.kind.clone(),
        holdout: options.holdout,
    };
    Ok(queries::add(&queries_path, &manifest, &new)?)
}

/// Run `queries check`: validate `queries.jsonl` against the committed manifest.
pub fn queries_check(
    paths: &Paths,
    queries_override: Option<&Path>,
) -> Result<CheckReport, CommandError> {
    let config = if paths.config.is_file() {
        Some(Config::load(&paths.config)?)
    } else {
        None
    };
    let eval_config = config.as_ref().and_then(|c| c.eval.as_ref());
    let queries_path = resolve_queries_path(paths, eval_config, queries_override)?;
    let manifest = Manifest::load(&paths.manifest)?;
    let rows = eval::load_queries(&queries_path)?;
    let holdout_min = eval_config.map_or(queries::DEFAULT_HOLDOUT_MIN, |e| e.holdout_min);
    Ok(queries::check(&rows, &manifest, holdout_min))
}

/// Options for `classify` (SPEC §14.2).
#[derive(Debug, Clone, Default)]
pub struct ClassifyOptions {
    /// `--model`; falls back to `PINAKES_LLM_MODEL`.
    pub model: Option<String>,
    /// `--batch` (default [`classify::DEFAULT_BATCH`]).
    pub batch: Option<usize>,
    /// Print the proposed decisions instead of writing them.
    pub dry_run: bool,
}

/// What `classify` produced.
#[derive(Debug)]
pub struct ClassifyOutcome {
    /// One decision per candidate the model classified.
    pub decisions: Vec<Decision>,
    /// Non-fatal observations (a rule override, or an id the model invented).
    pub warnings: Vec<String>,
    /// Whether the decisions were appended to `decisions.jsonl` (`false` for `--dry-run`).
    pub written: bool,
}

/// The title, an excerpt and the `sha256` of a manifest page read from the artifact, for a
/// near-duplicate candidate that is not in `residue.jsonl`.
fn read_manifest_page(artifact: &Path, manifest: Option<&Manifest>, id: &str) -> Option<PageFacts> {
    let entry = manifest?.page(id)?;
    let (source, path) = split_page_id(id)?;
    let bytes = std::fs::read(artifact.join(source).join(path)).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let excerpt = residue::excerpt(strip_frontmatter(&text), residue::EXCERPT_TOKENS);
    Some(PageFacts {
        title: entry.title.clone(),
        excerpt,
        sha256: entry.sha256.clone(),
    })
}

/// Run `classify`: send undecided residue and near-duplicate candidates to the model in
/// batches and write (or, with `--dry-run`, print) the resulting decisions (SPEC §14.2).
pub fn classify(
    paths: &Paths,
    options: &ClassifyOptions,
    transport: &dyn ChatTransport,
) -> Result<ClassifyOutcome, CommandError> {
    let config = LlmConfig::from_env(options.model.clone())?;
    let residue = if paths.residue.is_file() {
        residue::read_jsonl(&paths.residue)?
    } else {
        Vec::new()
    };
    let duplicate_pairs = duplicates::read_jsonl(&paths.duplicates)?;
    let effective = decisions::effective(&decisions::read_jsonl(&paths.decisions)?);
    let manifest = if paths.manifest.is_file() {
        Some(Manifest::load(&paths.manifest)?)
    } else {
        None
    };
    let page_lookup = |id: &str| read_manifest_page(&paths.artifact, manifest.as_ref(), id);
    let lookup = DuplicateLookup { page: &page_lookup };
    let candidates = classify::candidates(&residue, &duplicate_pairs, &effective, &lookup);
    let batch = options.batch.unwrap_or(classify::DEFAULT_BATCH);
    let at = now_rfc3339();
    let outcome = classify::run(transport, &config, &candidates, batch, &at)?;
    let written = if options.dry_run {
        false
    } else {
        for decision in &outcome.decisions {
            decisions::append(&paths.decisions, decision)?;
        }
        true
    };
    Ok(ClassifyOutcome {
        decisions: outcome.decisions,
        warnings: outcome.warnings,
        written,
    })
}

/// Options for `queries import` (SPEC §15.2).
#[derive(Debug, Clone)]
pub struct QueriesImportOptions {
    /// Query file to append to (default: `eval.queries` from the config, relative to it).
    pub queries: Option<PathBuf>,
    /// `graded.jsonl` to read.
    pub graded: PathBuf,
    /// Minimum grade for an id to enter `expected`.
    pub min_grade: u8,
    /// Target held-out share for the random assignment.
    pub holdout_share: f64,
    /// Seed for the deterministic PRNG that assigns `holdout`.
    pub seed: u64,
}

/// What `queries import` produced.
#[derive(Debug)]
pub struct QueriesImportOutcome {
    /// Rows appended to the query file.
    pub imported: Vec<GradedQuery>,
    /// Query texts with no candidate at or above `min_grade` (not appended).
    pub skipped: Vec<String>,
}

/// Run `queries import`: turn `pinakes grade`'s output into query rows (SPEC §15.2).
pub fn queries_import(
    paths: &Paths,
    options: &QueriesImportOptions,
) -> Result<QueriesImportOutcome, CommandError> {
    let config = if paths.config.is_file() {
        Some(Config::load(&paths.config)?)
    } else {
        None
    };
    let eval_config = config.as_ref().and_then(|c| c.eval.as_ref());
    let queries_path = resolve_queries_path(paths, eval_config, options.queries.as_deref())?;
    let graded_text = std::fs::read_to_string(&options.graded).map_err(io_err(&options.graded))?;
    let mut graded = Vec::new();
    for line in graded_text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        graded.push(serde_json::from_str::<GradedRow>(line)?);
    }
    let (imported, skipped) = queries::import_graded(
        &graded,
        options.min_grade,
        options.holdout_share,
        options.seed,
    );
    queries::append_graded(&queries_path, &imported)?;
    Ok(QueriesImportOutcome { imported, skipped })
}

/// Options for `grade` (SPEC §15.2).
#[derive(Debug, Clone)]
pub struct GradeOptions {
    /// `trail.jsonl` to replay.
    pub trail: PathBuf,
    /// `--backend` (only `"bm25"` is available; see [`grade::check_backend`]).
    pub backend: Option<String>,
    /// `--k` (default [`grade::DEFAULT_K`]).
    pub k: Option<usize>,
    /// `--model`; falls back to `PINAKES_LLM_MODEL`.
    pub model: Option<String>,
    /// Write the graded rows here instead of returning them for the caller to print.
    pub out: Option<PathBuf>,
}

/// What `grade` produced.
#[derive(Debug)]
pub struct GradeOutcome {
    /// One row per (query, candidate) the model graded.
    pub rows: Vec<GradedRow>,
    /// Distinct queries replayed.
    pub queries: usize,
}

/// Run `grade`: replay every distinct trail query against the backend and ask the model to
/// grade each candidate (SPEC §15.2).
pub fn grade(
    paths: &Paths,
    options: &GradeOptions,
    transport: &dyn ChatTransport,
) -> Result<GradeOutcome, CommandError> {
    let backend = options.backend.as_deref().unwrap_or(grade::BM25_BACKEND);
    grade::check_backend(backend)?;
    let config = LlmConfig::from_env(options.model.clone())?;
    let entries = trail::read_jsonl(&options.trail)?;
    let queries = grade::distinct_queries(&entries);
    let priorities = if paths.config.is_file() {
        Priorities::from_config(&Config::load(&paths.config)?)
    } else {
        Priorities::default()
    };
    let index = Index::build(&paths.artifact, &priorities)?;
    let k = options.k.unwrap_or(grade::DEFAULT_K);
    let at = now_rfc3339();
    let mut rows = Vec::new();
    for query in &queries {
        let hits = grade::bm25_search(&index, query, k)?;
        rows.extend(grade::grade_query(
            transport, &config, &index, query, &hits, &at,
        )?);
    }
    if let Some(path) = &options.out {
        std::fs::write(path, grade::to_jsonl(&rows)?).map_err(io_err(path))?;
    }
    Ok(GradeOutcome {
        rows,
        queries: queries.len(),
    })
}

/// Options for `usage` (SPEC §15.3).
#[derive(Debug, Clone)]
pub struct UsageOptions {
    /// `trail.jsonl` to read.
    pub trail: PathBuf,
    /// `--since` (e.g. `30d`); `None` uses the whole trail.
    pub since: Option<String>,
    /// Write the report here instead of returning it for the caller to print.
    pub json: Option<PathBuf>,
}

/// Run `usage`: pages never retrieved, retrieved-never-cited pages, and uncited queries with
/// their best residue gap candidate, over a trail window (SPEC §15.3).
pub fn usage(paths: &Paths, options: &UsageOptions) -> Result<Usage, CommandError> {
    let manifest = Manifest::load(&paths.manifest)?;
    let entries = trail::read_jsonl(&options.trail)?;
    let since_seconds = options
        .since
        .as_deref()
        .map(usage::parse_since)
        .transpose()?;
    let now = jiff::Timestamp::now();
    let filtered: Vec<TrailEntry> = usage::filter_since(&entries, since_seconds, now);
    let residue_index = usage::ResidueIndex::build(&paths.artifact);
    let report = usage::compute(&manifest, &filtered, &residue_index);
    if let Some(path) = &options.json {
        report.save(path)?;
    }
    Ok(report)
}

// -------------------------------------------------------------------------------------------
// embed (SPEC §16.2)
// -------------------------------------------------------------------------------------------

/// Options for `embed`.
#[derive(Debug, Clone, Default)]
pub struct EmbedOptions {
    /// The embedding model name, recorded in `embeddings.json`.
    pub model: String,
    /// Texts per embedding request (SPEC §16.2 default: [`embed::DEFAULT_BATCH`]).
    pub batch: usize,
    /// `embeddings.bin` output path (default: `paths.embeddings`); `embeddings.json` is written
    /// next to it, with a `.json` extension.
    pub out: Option<PathBuf>,
}

/// What `embed` wrote.
#[derive(Debug)]
pub struct EmbedOutcome {
    /// Retrieval units embedded.
    pub units: usize,
    /// Vector length.
    pub dimension: usize,
    /// `embeddings.bin` path.
    pub bin_path: PathBuf,
    /// `embeddings.json` path.
    pub json_path: PathBuf,
}

/// Run `embed`: one embedding per retrieval unit of the artifact, through `embedder`.
///
/// The config is optional, exactly as for `eval`: priorities default to
/// [`Priorities::default`] without one.
pub fn embed(
    paths: &Paths,
    options: &EmbedOptions,
    embedder: &dyn Embedder,
) -> Result<EmbedOutcome, CommandError> {
    let priorities = if paths.config.is_file() {
        Priorities::from_config(&Config::load(&paths.config)?)
    } else {
        Priorities::default()
    };
    let mut pages = index::load_pages(&paths.artifact, &priorities)?;
    index::mark_mirrors(&mut pages);
    let units = index::iter_units(&pages);
    let texts: Vec<String> = units.iter().map(|u| u.text.clone()).collect();
    let batch = if options.batch == 0 {
        embed::DEFAULT_BATCH
    } else {
        options.batch
    };
    let vectors = embed::embed_units(embedder, &options.model, &texts, batch)?;
    let dimension = vectors.first().map_or(0, Vec::len);
    let manifest = embed::EmbeddingsManifest {
        model: options.model.clone(),
        dimension,
        unit_ids: units.into_iter().map(|u| u.page_id).collect(),
        manifest_sha256: embed::artifact_manifest_hash(&paths.artifact)?,
    };
    let bin_path = options
        .out
        .clone()
        .unwrap_or_else(|| paths.embeddings.clone());
    let json_path = bin_path.with_extension("json");
    embed::write_embeddings(&bin_path, &json_path, &manifest, &vectors)?;
    Ok(EmbedOutcome {
        units: manifest.unit_ids.len(),
        dimension,
        bin_path,
        json_path,
    })
}

// -------------------------------------------------------------------------------------------
// eval --backend / --compare (SPEC §16.1)
// -------------------------------------------------------------------------------------------

/// Options for `eval --backend` (any backend other than the plain default).
#[derive(Clone, Default)]
pub struct BackendEvalOptions {
    /// Query file (default: `eval.queries` from the config, relative to it).
    pub queries: Option<PathBuf>,
    /// Result list length (default: `eval.k` from the config, else [`DEFAULT_K`]).
    pub k: Option<usize>,
    /// Write the result JSON here.
    pub json: Option<PathBuf>,
    /// Baseline result to gate the tuning recall@5 against.
    pub gate: Option<PathBuf>,
    /// Residue pages to add before measuring; only supported for `bm25`.
    pub with: Vec<String>,
    /// Pages to remove before measuring; only supported for `bm25`.
    pub without: Vec<String>,
    /// The backend to measure.
    pub backend: BackendKind,
    /// The consumer's search endpoint (`external`).
    pub backend_url: Option<String>,
    /// `embeddings.bin` path (`dense`, `hybrid`); default: `paths.embeddings`.
    pub embeddings: Option<PathBuf>,
    /// Use embeddings even when their recorded manifest hash does not match the artifact.
    pub allow_stale: bool,
    /// Where query embeddings come from (`dense`, `hybrid`).
    pub embedder: Option<std::rc::Rc<dyn Embedder>>,
}

/// What `eval --backend` produced.
#[derive(Debug)]
pub struct BackendEvalOutcome {
    /// The result (after `--with`/`--without` when given), with [`EvalSummary::backend`] set.
    pub summary: EvalSummary,
    /// Pages in the measured corpus, mirrors included.
    pub page_count: usize,
    /// Pages in the search corpus.
    pub searchable_count: usize,
    /// Result list length used.
    pub k: usize,
    /// The gate outcome when `--gate` was given.
    pub gate: Option<Gate>,
    /// The delta when `--with`/`--without` was given.
    pub delta: Option<Delta>,
}

/// Run every query against `backend` with a result list of `k` pages.
fn evaluate_backend(
    backend: &dyn Backend,
    queries: &[eval::Query],
    k: usize,
) -> Result<EvalSummary, CommandError> {
    let mut rows = Vec::with_capacity(queries.len());
    for query in queries {
        let top = backend
            .search(&query.query, k, None)?
            .into_iter()
            .map(|hit| hit.page_id)
            .collect();
        rows.push(eval::QueryResult::score(query, top, k));
    }
    Ok(eval::summarise(rows))
}

fn backend_config(
    paths: &Paths,
    options: &BackendEvalOptions,
    priorities: Priorities,
) -> BackendConfig {
    let embeddings_bin = options
        .embeddings
        .clone()
        .unwrap_or_else(|| paths.embeddings.clone());
    let embeddings_json = embeddings_bin.with_extension("json");
    BackendConfig {
        priorities,
        embeddings_bin,
        embeddings_json,
        allow_stale: options.allow_stale,
        embedder: options.embedder.clone(),
        backend_url: options.backend_url.clone(),
    }
}

/// Run `eval --backend NAME`: like [`eval()`], but through the [`Backend`] trait (SPEC §16.1),
/// recording the backend name in the result. `--with`/`--without` only work for `bm25`, which
/// indexes a page list directly; every other backend rejects them with
/// [`BackendError::UnsupportedAdjustment`].
pub fn eval_backend(
    paths: &Paths,
    options: &BackendEvalOptions,
) -> Result<BackendEvalOutcome, CommandError> {
    let config = if paths.config.is_file() {
        Some(Config::load(&paths.config)?)
    } else {
        None
    };
    let eval_config = config.as_ref().and_then(|c| c.eval.as_ref());
    let queries_path = resolve_queries_path(paths, eval_config, options.queries.as_deref())?;
    let k = options
        .k
        .or_else(|| eval_config.map(|e| e.k))
        .unwrap_or(DEFAULT_K);
    let max_drop = eval_config.map_or(DEFAULT_MAX_RECALL_DROP, |e| e.max_recall_drop);
    let priorities = config
        .as_ref()
        .map(Priorities::from_config)
        .unwrap_or_default();
    let queries = eval::load_queries(&queries_path)?;
    let adjusting = !options.with.is_empty() || !options.without.is_empty();
    if adjusting && options.backend != BackendKind::Bm25 {
        return Err(BackendError::UnsupportedAdjustment(options.backend.name().to_string()).into());
    }

    let (summary, page_count, searchable_count, delta) = if adjusting {
        let pages = index::load_pages(&paths.artifact, &priorities)?;
        let before = eval::evaluate(&Index::from_pages(pages.clone())?, &queries, k)?;
        let eval_options = EvalOptions {
            with: options.with.clone(),
            without: options.without.clone(),
            ..EvalOptions::default()
        };
        let index = Index::from_pages(adjust_pages(
            pages,
            &paths.artifact,
            &priorities,
            &eval_options,
        )?)?;
        let after = eval::evaluate(&index, &queries, k)?.with_backend(options.backend.name());
        let delta = eval::delta(&before, &after);
        (
            after,
            index.page_count(),
            index.searchable_count(),
            Some(delta),
        )
    } else {
        let config = backend_config(paths, options, priorities);
        let built = backend::build(options.backend, &paths.artifact, &config)?;
        let summary =
            evaluate_backend(built.as_ref(), &queries, k)?.with_backend(options.backend.name());
        (summary, built.page_count(), built.searchable_count(), None)
    };

    let gate = match &options.gate {
        Some(path) => Some(eval::gate(&summary, &EvalSummary::load(path)?, max_drop)),
        None => None,
    };
    if let Some(path) = &options.json {
        summary.save(path)?;
    }
    Ok(BackendEvalOutcome {
        summary,
        page_count,
        searchable_count,
        k,
        gate,
        delta,
    })
}

/// Run `eval --compare a,b,c`: [`eval_backend`] once per backend, over the same query set.
pub fn eval_compare(
    paths: &Paths,
    backends: &[BackendKind],
    common: &BackendEvalOptions,
) -> Result<Vec<(BackendKind, BackendEvalOutcome)>, CommandError> {
    if backends.is_empty() {
        return Err(CommandError::EmptyCompare);
    }
    backends
        .iter()
        .map(|&kind| {
            let options = BackendEvalOptions {
                backend: kind,
                json: None,
                ..common.clone()
            };
            Ok((kind, eval_backend(paths, &options)?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::testing::{FakeFetcher, build_tarball};
    use std::fs;

    const SHA: &str = "4427d7ba863973c2cea9da74ed8675c5c74aee77";

    fn workspace(config: &str) -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("pinakes.yaml");
        fs::write(&config_path, config).unwrap();
        let paths = Paths::for_config(&config_path);
        (dir, paths)
    }

    const CONFIG: &str = "version: 1\nsources:\n  - name: handbook\n    repo: https://github.com/o/handbook.git\n    \
                          ref: main\n    resolver:\n      type: glob\n      include: ['docs/**/*.md']\n      \
                          exclude: ['**/_sidebar.md']\npolicy:\n  deny: ['**/adr/**']\n  min_pages_per_source: 2\n";

    fn fetcher() -> FakeFetcher {
        let files: [(&str, &[u8]); 4] = [
            ("docs/a.md", b"# A\n"),
            ("docs/b.md", b"# B\n"),
            ("docs/_sidebar.md", b"- a\n"),
            ("docs/adr/1.md", b"# ADR\n"),
        ];
        let mut fetcher = FakeFetcher::default();
        fetcher.add_tarball(
            "o/handbook",
            "main",
            build_tarball("handbook-main", Some(SHA), &files),
        );
        fetcher.add_tarball(
            "o/handbook",
            SHA,
            build_tarball(&format!("handbook-{SHA}"), Some(SHA), &files),
        );
        fetcher.set_archived("o/handbook", false);
        fetcher
    }

    fn opts() -> ResolveOptions {
        ResolveOptions {
            from_manifest: None,
            generated_at: Some("2026-09-16T12:00:00Z".into()),
        }
    }

    #[test]
    fn resolve_then_verify_then_decide() {
        let (_dir, paths) = workspace(CONFIG);
        let fetcher = fetcher();
        let outcome = resolve(&paths, &opts(), &fetcher).unwrap();
        let handbook = &outcome.manifest.sources["handbook"];
        assert_eq!(handbook.commit, SHA);
        assert_eq!(handbook.archived, Some(false));
        assert_eq!(handbook.pages.len(), 2);
        assert_eq!(
            outcome.residue.iter().map(|r| r.reason).collect::<Vec<_>>(),
            [Reason::Excluded, Reason::Excluded],
            "docs/_sidebar.md matches resolver.exclude and docs/adr/1.md matches policy.deny, \
             both now residue in their own right (SPEC §2.4) instead of vanishing silently"
        );
        assert!(outcome.warnings.is_empty());
        assert!(paths.manifest.is_file() && paths.residue.is_file());
        assert!(paths.artifact.join("handbook/docs/a.md").is_file());
        assert!(!paths.artifact.join("handbook/docs/adr/1.md").exists());

        assert!(verify(&paths, true).unwrap().ok());

        // Policy: raise the minimum so the source violates it.
        fs::write(
            &paths.config,
            CONFIG.replace("min_pages_per_source: 2", "min_pages_per_source: 3"),
        )
        .unwrap();
        let report = verify(&paths, true).unwrap();
        assert!(report.stale.is_empty());
        assert_eq!(report.violations.len(), 1, "{report:?}");

        // Stale: change the ref in the config and tamper with the artifact.
        fs::write(&paths.config, CONFIG.replace("ref: main", "ref: v2")).unwrap();
        fs::write(paths.artifact.join("handbook/docs/a.md"), "tampered").unwrap();
        let report = verify(&paths, true).unwrap();
        assert_eq!(report.stale.len(), 2, "{report:?}");
        assert_eq!(verify(&paths, false).unwrap().stale.len(), 1);

        fs::write(&paths.config, CONFIG).unwrap();
        let decision = decide(
            &paths,
            "handbook::docs/a.md",
            Verdict::Exclude,
            "noise",
            "me",
            Some("t".into()),
        )
        .unwrap();
        assert_eq!(decision.sha256, sha256_hex(b"# A\n"));
        assert!(matches!(
            decide(&paths, "handbook::nope.md", Verdict::Exclude, "", "", None).unwrap_err(),
            CommandError::UnknownId(_)
        ));

        // The exclude decision now removes the page and reports it as residue, alongside the
        // ongoing policy.deny and resolver.exclude residue (SPEC §2.4).
        let outcome = resolve(&paths, &opts(), &fetcher).unwrap();
        assert_eq!(outcome.manifest.sources["handbook"].pages.len(), 1);
        assert_eq!(outcome.residue.len(), 3, "{:#?}", outcome.residue);
        assert_eq!(outcome.residue[0].id, "handbook::docs/a.md");
        assert!(
            residue_list(&paths, &ListFilter::default())
                .unwrap()
                .is_empty(),
            "decided → hidden; the excluded ones are hidden by default regardless of decisions"
        );
        assert!(paths.artifact.join("_residue/handbook/docs/a.md").is_file());
        assert!(outcome.expired.is_empty());

        // A decision with a stale hash is reported as expired and ignored.
        decisions::append(
            &paths.decisions,
            &Decision {
                id: "handbook::docs/b.md".into(),
                sha256: "stale".into(),
                decision: Verdict::Exclude,
                reason: String::new(),
                by: String::new(),
                at: String::new(),
            },
        )
        .unwrap();
        let outcome = resolve(&paths, &opts(), &fetcher).unwrap();
        assert_eq!(outcome.expired.len(), 1);
        assert_eq!(outcome.expired[0].decision.id, "handbook::docs/b.md");
        assert!(
            outcome.manifest.sources["handbook"]
                .pages
                .contains_key("docs/b.md")
        );
    }

    #[test]
    fn report_reads_the_workspace_files() {
        let (_dir, paths) = workspace(CONFIG);
        resolve(&paths, &opts(), &fetcher()).unwrap();
        let text = report(&paths, &ReportOptions::default(), &fetcher()).unwrap();
        assert!(text.starts_with("# Corpus report\n"));
        assert!(text.contains("- Pages: 2\n"));
        assert!(text.contains("## Duplicates\n\n_none_\n"));
        let options = ReportOptions {
            old: Some(paths.manifest.clone()),
            eval_after: Some(paths.config_dir().join("missing-eval.json")),
            ..ReportOptions::default()
        };
        assert!(matches!(
            report(&paths, &options, &fetcher()).unwrap_err(),
            CommandError::Eval(_)
        ));
    }

    #[test]
    fn archived_sources_warn_or_drop() {
        let (_dir, paths) = workspace(CONFIG);
        let mut fetcher = fetcher();
        fetcher.set_archived("o/handbook", true);
        let outcome = resolve(&paths, &opts(), &fetcher).unwrap();
        assert_eq!(outcome.warnings, ["handbook: repository is archived"]);
        assert_eq!(outcome.manifest.sources["handbook"].archived, Some(true));

        fs::write(&paths.config, format!("{CONFIG}  archived: drop\n")).unwrap();
        let outcome = resolve(&paths, &opts(), &fetcher).unwrap();
        assert!(outcome.manifest.sources.is_empty());
        assert_eq!(
            outcome.warnings,
            ["handbook: repository is archived; dropped"]
        );
        // The source's would-be pages are still accounted for, as residue with `source:archived`
        // (SPEC §2.1, §2.4); its own residue (excluded/denied files) keeps its own rule.
        let by_path: std::collections::BTreeMap<&str, &str> = outcome
            .residue
            .iter()
            .map(|r| (r.path.as_str(), r.rule.as_ref().unwrap().key.as_str()))
            .collect();
        assert_eq!(
            by_path,
            [
                ("docs/_sidebar.md", "resolver:exclude"),
                ("docs/a.md", "source:archived"),
                ("docs/adr/1.md", "policy:deny"),
                ("docs/b.md", "source:archived"),
            ]
            .into_iter()
            .collect(),
            "{:#?}",
            outcome.residue
        );
        assert!(outcome.residue.iter().all(|r| r.reason == Reason::Excluded));
    }

    #[test]
    fn missing_tarball_is_a_source_error() {
        let (_dir, paths) = workspace(CONFIG);
        let err = resolve(&paths, &opts(), &FakeFetcher::default()).unwrap_err();
        assert!(matches!(err, CommandError::Source { .. }), "{err}");
    }

    const CRD_CONFIG: &str = "version: 1\nsources:\n  - name: crds\n    repo: https://github.com/o/crds.git\n    \
                               ref: main\n    resolver:\n      type: glob\n      \
                               include: ['config/crd/bases/*.yaml']\n    render:\n      type: openapi\n";

    const CRD_YAML: &[u8] = b"apiVersion: apiextensions.k8s.io/v1\n\
kind: CustomResourceDefinition\n\
metadata:\n  name: widgets.example.com\n\
spec:\n  group: example.com\n  names:\n    kind: Widget\n    plural: widgets\n  scope: Namespaced\n  \
versions:\n    - name: v1\n      served: true\n      storage: true\n      schema:\n        \
openAPIV3Schema:\n          type: object\n          properties:\n            spec:\n              \
type: object\n              properties:\n                size:\n                  type: string\n";

    #[test]
    fn resolve_renders_crds_and_records_unrendered_selected_files() {
        let (_dir, paths) = workspace(CRD_CONFIG);
        let files: [(&str, &[u8]); 2] = [
            ("config/crd/bases/widgets.yaml", CRD_YAML),
            ("config/crd/bases/not-a-crd.yaml", b"foo: bar\n"),
        ];
        let mut fetcher = FakeFetcher::default();
        fetcher.add_tarball(
            "o/crds",
            "main",
            build_tarball("crds-main", Some(SHA), &files),
        );
        fetcher.set_archived("o/crds", false);

        let outcome = resolve(&paths, &opts(), &fetcher).unwrap();
        let crds = &outcome.manifest.sources["crds"];
        assert_eq!(crds.pages.len(), 1, "one page per served CRD version");
        assert_eq!(crds.unrendered, ["config/crd/bases/not-a-crd.yaml"]);

        let page = &crds.pages["reference/example.com/widget-v1.md"];
        assert_eq!(page.title, "Widget (example.com/v1)");
        assert_eq!(page.doc_type, "reference");
        assert_eq!(page.section, "example.com");
        assert_eq!(page.selected_by, SelectedBy::Include);
        assert_eq!(
            page.rendered_from.as_deref(),
            Some("config/crd/bases/widgets.yaml")
        );

        let rendered = fs::read_to_string(
            paths
                .artifact
                .join("crds/reference/example.com/widget-v1.md"),
        )
        .unwrap();
        assert_eq!(sha256_hex(rendered.as_bytes()), page.sha256);
        assert!(rendered.starts_with("# Widget (example.com/v1)\n"));
        assert!(
            !paths
                .artifact
                .join("crds/config/crd/bases/widgets.yaml")
                .exists()
        );

        let meta = fs::read_to_string(paths.artifact.join("crds/meta.json")).unwrap();
        assert!(meta.contains("\"unrendered\": [\n    \"config/crd/bases/not-a-crd.yaml\"\n  ]"));
        assert!(verify(&paths, true).unwrap().ok());
    }

    #[test]
    fn new_sources_report_new_source_residue() {
        let config = CONFIG.replace(
            "include: ['docs/**/*.md']",
            "include: ['docs/a.md']\n      residue_scope: ['docs/*.md']",
        );
        let (_dir, paths) = workspace(&config);
        let fetcher = fetcher();
        let outcome = resolve(&paths, &opts(), &fetcher).unwrap();
        assert_eq!(
            outcome.residue.iter().map(|r| r.reason).collect::<Vec<_>>(),
            [Reason::NotSelected, Reason::Excluded, Reason::Excluded],
            "docs/b.md is not selected; docs/_sidebar.md matches resolver.exclude and \
             docs/adr/1.md matches policy.deny, both now residue in their own right (SPEC §2.4)"
        );
        // Rename the source: it is now new relative to the committed manifest.
        fs::write(
            &paths.config,
            config.replace("name: handbook", "name: handbook2"),
        )
        .unwrap();
        let outcome = resolve(&paths, &opts(), &fetcher).unwrap();
        assert_eq!(outcome.residue[0].reason, Reason::NewSource);
    }

    /// A workspace with a synthetic artifact, a query file and no config.
    fn eval_workspace() -> (tempfile::TempDir, Paths) {
        use crate::index::testing::{SourceSpec, write_artifact};
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::for_config(&dir.path().join("pinakes.yaml"));
        write_artifact(
            &paths.artifact,
            &[SourceSpec {
                name: "handbook",
                repo: "example-org/handbook",
                pages: &[(
                    "docs/user/README.md",
                    "Storage Module",
                    "# Storage\n\nEnable upload caching with a bucket label.\n",
                )],
                residue: &[(
                    "docs/user/quotas.md",
                    "# Configure Quotas\n\nRate limits in strict mode.\n",
                )],
            }],
        );
        fs::write(
            dir.path().join("queries.jsonl"),
            concat!(
                "{\"id\": \"caching\", \"kind\": \"howto\", \"query\": \"enable upload caching\", \"expected\": [\"handbook/docs/user\"]}\n",
                "{\"id\": \"quotas\", \"kind\": \"howto\", \"query\": \"quotas rate limits\", \"expected\": [\"handbook::docs/user/quotas.md\"]}\n",
            ),
        )
        .unwrap();
        (dir, paths)
    }

    #[test]
    fn eval_reads_defaults_from_the_config_or_the_options() {
        let (dir, paths) = eval_workspace();
        assert!(matches!(
            eval(&paths, &EvalOptions::default()).unwrap_err(),
            CommandError::NoQueries
        ));
        let options = EvalOptions {
            queries: Some(dir.path().join("queries.jsonl")),
            json: Some(dir.path().join("out.json")),
            ..EvalOptions::default()
        };
        let outcome = eval(&paths, &options).unwrap();
        assert_eq!(
            (outcome.page_count, outcome.searchable_count, outcome.k),
            (1, 1, 10)
        );
        assert!(
            (outcome.summary.tuning.overall.recall5 - 0.5).abs() < 1e-12,
            "{:?}",
            outcome.summary.queries
        );
        assert!(outcome.gate.is_none() && outcome.delta.is_none());
        assert_eq!(
            EvalSummary::load(&dir.path().join("out.json")).unwrap(),
            outcome.summary
        );

        // With a config: queries and k come from its eval section.
        fs::write(
            &paths.config,
            "version: 1\nsources:\n  - name: handbook\n    repo: https://github.com/o/handbook.git\n    \
             ref: main\n    resolver:\n      type: glob\n      include: ['**/*.md']\neval:\n  queries: queries.jsonl\n  k: 3\n  max_recall_drop: 0.1\n",
        )
        .unwrap();
        let outcome = eval(&paths, &EvalOptions::default()).unwrap();
        assert_eq!(outcome.k, 3);
        assert_eq!(outcome.summary.query("caching").unwrap().top.len(), 1);
    }

    #[test]
    fn eval_gates_and_measures_with_and_without() {
        let (dir, paths) = eval_workspace();
        let queries = dir.path().join("queries.jsonl");
        let baseline = dir.path().join("baseline.json");
        let options = EvalOptions {
            queries: Some(queries.clone()),
            json: Some(baseline.clone()),
            ..EvalOptions::default()
        };
        eval(&paths, &options).unwrap();

        // Same corpus: the gate passes even with zero tolerance from the config.
        fs::write(
            &paths.config,
            "version: 1\nsources:\n  - name: handbook\n    repo: https://github.com/o/handbook.git\n    \
             ref: main\n    resolver:\n      type: glob\n      include: ['**/*.md']\neval:\n  queries: queries.jsonl\n  max_recall_drop: 0\n",
        )
        .unwrap();
        let options = EvalOptions {
            queries: Some(queries.clone()),
            gate: Some(baseline.clone()),
            ..EvalOptions::default()
        };
        assert!(eval(&paths, &options).unwrap().gate.unwrap().passed());

        // Adding the residue page lifts recall; removing the only page drops it to zero, which
        // fails the gate.
        let options = EvalOptions {
            queries: Some(queries.clone()),
            with: vec!["handbook::docs/user/quotas.md".into()],
            ..EvalOptions::default()
        };
        let outcome = eval(&paths, &options).unwrap();
        assert_eq!((outcome.page_count, outcome.searchable_count), (2, 2));
        let delta = outcome.delta.unwrap();
        assert!((delta.tuning.1.recall5 - 1.0).abs() < 1e-12);
        assert_eq!(delta.changed.len(), 1);
        let options = EvalOptions {
            queries: Some(queries.clone()),
            gate: Some(baseline),
            without: vec!["handbook::docs/user/README.md".into()],
            ..EvalOptions::default()
        };
        let outcome = eval(&paths, &options).unwrap();
        assert!(!outcome.gate.unwrap().passed());
        assert!(outcome.delta.unwrap().tuning.1.recall5.abs() < 1e-12);

        for (with, without) in [
            (vec!["handbook::docs/user/README.md".to_string()], vec![]),
            (vec!["handbook::nope.md".to_string()], vec![]),
            (vec![], vec!["handbook::nope.md".to_string()]),
        ] {
            let options = EvalOptions {
                queries: Some(queries.clone()),
                with,
                without,
                ..EvalOptions::default()
            };
            assert!(matches!(
                eval(&paths, &options).unwrap_err(),
                CommandError::Index(_)
            ));
        }
    }

    #[test]
    fn queries_add_and_check_validate_against_the_manifest() {
        let (dir, paths) = workspace(CONFIG);
        resolve(&paths, &opts(), &fetcher()).unwrap();
        let queries_path = dir.path().join("queries.jsonl");

        let add_options = QueriesAddOptions {
            queries: Some(queries_path.clone()),
            id: "a".to_string(),
            query: "what is a".to_string(),
            expected: vec!["handbook/docs/a.md".to_string()],
            kind: "howto".to_string(),
            holdout: false,
        };
        let query = queries_add(&paths, &add_options).unwrap();
        assert_eq!(query.expected, ["handbook::docs/a.md"]);

        // An unknown expected id is rejected and nothing is appended.
        let bad = QueriesAddOptions {
            id: "bad".to_string(),
            expected: vec!["handbook::nope.md".to_string()],
            ..add_options.clone()
        };
        assert!(matches!(
            queries_add(&paths, &bad).unwrap_err(),
            CommandError::Queries(_)
        ));
        assert_eq!(eval::load_queries(&queries_path).unwrap().len(), 1);

        // A held-out row so the share check has something to pass.
        let held = QueriesAddOptions {
            id: "b".to_string(),
            query: "what is b".to_string(),
            expected: vec!["handbook::docs/b.md".to_string()],
            holdout: true,
            ..add_options
        };
        queries_add(&paths, &held).unwrap();

        let report = queries_check(&paths, Some(&queries_path)).unwrap();
        assert!(report.ok(), "{report:?}");
        assert!((report.holdout_share - 0.5).abs() < 1e-12);

        // Below the configured minimum: same rows, a stricter holdout_min.
        fs::write(
            &paths.config,
            format!("{CONFIG}eval:\n  queries: queries.jsonl\n  holdout_min: 0.6\n"),
        )
        .unwrap();
        let report = queries_check(&paths, Some(&queries_path)).unwrap();
        assert!(!report.ok());
        assert!((report.holdout_min - 0.6).abs() < 1e-12);
        fs::write(&paths.config, CONFIG).unwrap();

        // A raw duplicate id and an unknown expected id, appended straight to the file.
        let mut text = fs::read_to_string(&queries_path).unwrap();
        text.push_str(
            "{\"id\": \"a\", \"query\": \"dup\", \"expected\": [\"handbook::nope.md\"]}\n",
        );
        fs::write(&queries_path, text).unwrap();
        let report = queries_check(&paths, Some(&queries_path)).unwrap();
        assert!(!report.ok());
        assert_eq!(report.duplicate_ids, ["a"]);
        assert_eq!(
            report.unknown,
            [("a".to_string(), "handbook::nope.md".to_string())]
        );

        assert!(matches!(
            queries_check(&paths, None).unwrap_err(),
            CommandError::NoQueries
        ));
    }

    #[test]
    fn resolve_also_writes_duplicates_jsonl() {
        let (_dir, paths) = workspace(CONFIG);
        let outcome = resolve(&paths, &opts(), &fetcher()).unwrap();
        assert!(paths.duplicates.is_file());
        assert_eq!(
            outcome.duplicates,
            duplicates::read_jsonl(&paths.duplicates).unwrap()
        );
    }

    #[test]
    fn diff_adds_line_counts_by_refetching_the_old_commit() {
        const OLD_SHA: &str = "1111111111111111111111111111111111111111";
        const NEW_SHA: &str = "2222222222222222222222222222222222222222";
        let old_files: [(&str, &[u8]); 2] =
            [("docs/a.md", b"line1\nline2\n"), ("docs/b.md", b"# B\n")];
        let new_files: [(&str, &[u8]); 2] = [
            ("docs/a.md", b"line1\nline2 changed\nline3\n"),
            ("docs/b.md", b"# B\n"),
        ];
        let mut fetcher = FakeFetcher::default();
        fetcher.add_tarball(
            "o/handbook",
            "main",
            build_tarball("handbook-main", Some(NEW_SHA), &new_files),
        );
        fetcher.add_tarball(
            "o/handbook",
            OLD_SHA,
            build_tarball("handbook-old", Some(OLD_SHA), &old_files),
        );
        fetcher.set_archived("o/handbook", false);

        let (_dir, paths) = workspace(CONFIG);
        let outcome = resolve(&paths, &opts(), &fetcher).unwrap();
        let new_manifest = outcome.manifest;
        assert_eq!(new_manifest.sources["handbook"].commit, NEW_SHA);

        // An "old" manifest: same source, but at the old commit with the old page hash.
        let mut old_manifest = new_manifest.clone();
        let old_source = old_manifest.sources.get_mut("handbook").unwrap();
        old_source.commit = OLD_SHA.to_string();
        old_source.pages.get_mut("docs/a.md").unwrap().sha256 = sha256_hex(old_files[0].1);

        let options = DiffOptions {
            new_artifact: Some(paths.artifact.clone()),
            old_artifact: None,
        };
        let computed = diff(&old_manifest, &new_manifest, &options, &fetcher).unwrap();
        assert_eq!(computed.changed.len(), 1);
        assert_eq!(computed.changed[0].id, "handbook::docs/a.md");
        assert_eq!(computed.changed[0].lines_added, 2);
        assert_eq!(computed.changed[0].lines_removed, 1);
        assert!(
            fetcher
                .requests
                .lock()
                .unwrap()
                .contains(&("o/handbook".to_string(), OLD_SHA.to_string())),
            "the old commit was re-fetched"
        );

        // With no artifact to read the new text from, line counts stay at zero.
        let no_content = DiffOptions::default();
        let computed = diff(&old_manifest, &new_manifest, &no_content, &fetcher).unwrap();
        assert_eq!(computed.changed[0].lines_added, 0);
        assert_eq!(computed.changed[0].lines_removed, 0);
    }
    fn fake_embedder() -> std::rc::Rc<dyn Embedder> {
        std::rc::Rc::new(crate::embed::testing::FakeEmbedder)
    }

    #[test]
    fn embed_writes_units_for_every_searchable_page() {
        let (_dir, paths) = eval_workspace();
        let options = EmbedOptions {
            model: "fake".to_string(),
            batch: 1,
            out: None,
        };
        let outcome = embed(&paths, &options, &crate::embed::testing::FakeEmbedder).unwrap();
        assert_eq!(outcome.units, 1, "one intro unit on the single page");
        assert_eq!(outcome.dimension, crate::embed::testing::FAKE_DIMENSION);
        assert_eq!(outcome.bin_path, paths.embeddings);
        assert_eq!(outcome.json_path, paths.embeddings.with_extension("json"));
        let (manifest, vectors) =
            crate::embed::read_embeddings(&outcome.bin_path, &outcome.json_path).unwrap();
        assert_eq!(manifest.unit_ids, ["handbook::docs/user/README.md"]);
        assert_eq!(vectors.len(), 1);
    }

    #[test]
    fn eval_backend_bm25_matches_the_plain_eval_path() {
        let (dir, paths) = eval_workspace();
        let queries = dir.path().join("queries.jsonl");
        let plain = eval(
            &paths,
            &EvalOptions {
                queries: Some(queries.clone()),
                ..EvalOptions::default()
            },
        )
        .unwrap();
        let via_backend = eval_backend(
            &paths,
            &BackendEvalOptions {
                queries: Some(queries),
                backend: BackendKind::Bm25,
                ..BackendEvalOptions::default()
            },
        )
        .unwrap();
        assert_eq!(via_backend.page_count, plain.page_count);
        assert_eq!(via_backend.searchable_count, plain.searchable_count);
        assert_eq!(via_backend.summary.tuning, plain.summary.tuning);
        assert_eq!(via_backend.summary.backend, "bm25");
        assert_eq!(plain.summary.backend, "", "the legacy path never sets it");
    }

    #[test]
    fn eval_backend_rejects_with_without_for_non_bm25_backends() {
        let (dir, paths) = eval_workspace();
        let options = BackendEvalOptions {
            queries: Some(dir.path().join("queries.jsonl")),
            backend: BackendKind::Bm25Tantivy,
            with: vec!["handbook::docs/user/quotas.md".to_string()],
            ..BackendEvalOptions::default()
        };
        assert!(matches!(
            eval_backend(&paths, &options).unwrap_err(),
            CommandError::Backend(BackendError::UnsupportedAdjustment(_))
        ));
    }

    #[test]
    fn eval_compare_runs_every_backend_over_the_same_queries() {
        let (dir, paths) = eval_workspace();
        let common = BackendEvalOptions {
            queries: Some(dir.path().join("queries.jsonl")),
            ..BackendEvalOptions::default()
        };
        let results = eval_compare(
            &paths,
            &[BackendKind::Bm25, BackendKind::Bm25Tantivy],
            &common,
        )
        .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, BackendKind::Bm25);
        assert_eq!(results[0].1.summary.backend, "bm25");
        assert_eq!(results[1].0, BackendKind::Bm25Tantivy);
        assert_eq!(results[1].1.summary.backend, "bm25-tantivy");
        assert!(matches!(
            eval_compare(&paths, &[], &common).unwrap_err(),
            CommandError::EmptyCompare
        ));
    }

    #[test]
    fn eval_backend_dense_uses_the_injected_embedder() {
        let (dir, paths) = eval_workspace();
        let embed_options = EmbedOptions {
            model: "fake".to_string(),
            batch: 64,
            out: None,
        };
        embed(&paths, &embed_options, fake_embedder().as_ref()).unwrap();
        let options = BackendEvalOptions {
            queries: Some(dir.path().join("queries.jsonl")),
            backend: BackendKind::Dense,
            embedder: Some(fake_embedder()),
            ..BackendEvalOptions::default()
        };
        let outcome = eval_backend(&paths, &options).unwrap();
        assert_eq!(outcome.summary.backend, "dense");
        assert_eq!(outcome.page_count, 1);
    }

    /// Set `PINAKES_LLM_URL` for the duration of `body`, serialised against every other test
    /// that touches `PINAKES_LLM_*`, and always clean up afterwards.
    fn with_llm_url<T>(body: impl FnOnce() -> T) -> T {
        let _guard = crate::llm::ENV_LOCK.lock().unwrap();
        // SAFETY: serialised by ENV_LOCK; no other test observes this var concurrently.
        unsafe {
            std::env::set_var("PINAKES_LLM_URL", "https://example.test");
        }
        let result = body();
        // SAFETY: serialised by ENV_LOCK; no other test observes this var concurrently.
        unsafe {
            std::env::remove_var("PINAKES_LLM_URL");
        }
        result
    }

    fn classify_reply(id: &str, decision: &str) -> serde_json::Value {
        crate::llm::testing::completion(
            &serde_json::to_string(&serde_json::json!([
                {"id": id, "decision": decision, "rationale": "noise", "confidence": 0.9}
            ]))
            .unwrap(),
        )
    }

    #[test]
    fn classify_dry_run_prints_but_writes_nothing_and_a_real_run_writes_with_classifier_by() {
        let (_dir, paths) = workspace(CONFIG);
        residue::write_jsonl(
            &paths.residue,
            &[ResidueEntry {
                id: "handbook::docs/x.md".to_string(),
                source: "handbook".to_string(),
                path: "docs/x.md".to_string(),
                reason: Reason::NotSelected,
                sha256: "aa".repeat(32),
                title: "X".to_string(),
                excerpt: "some text".to_string(),
                context: String::new(),
                url: String::new(),
                rule: None,
            }],
        )
        .unwrap();

        with_llm_url(|| {
            let dry_run_transport = crate::llm::testing::ScriptedTransport::new(vec![
                crate::llm::testing::Scripted::Ok(classify_reply("handbook::docs/x.md", "exclude")),
            ]);
            let options = ClassifyOptions {
                model: Some("m".to_string()),
                batch: None,
                dry_run: true,
            };
            let outcome = classify(&paths, &options, &dry_run_transport).unwrap();
            assert!(!outcome.written);
            assert_eq!(outcome.decisions.len(), 1);
            assert!(!paths.decisions.is_file(), "dry run must not write");

            let write_transport = crate::llm::testing::ScriptedTransport::new(vec![
                crate::llm::testing::Scripted::Ok(classify_reply("handbook::docs/x.md", "exclude")),
            ]);
            let options = ClassifyOptions {
                dry_run: false,
                ..options
            };
            let outcome = classify(&paths, &options, &write_transport).unwrap();
            assert!(outcome.written);
            let written = decisions::read_jsonl(&paths.decisions).unwrap();
            assert_eq!(written.len(), 1);
            assert_eq!(written[0].id, "handbook::docs/x.md");
            assert_eq!(written[0].decision, Verdict::Exclude);
            assert_eq!(written[0].by, "classifier:m");
        });
    }

    #[test]
    fn classify_requires_the_llm_url_even_with_no_candidates() {
        let _guard = crate::llm::ENV_LOCK.lock().unwrap();
        // SAFETY: serialised by ENV_LOCK; no other test observes this var concurrently.
        unsafe {
            std::env::remove_var("PINAKES_LLM_URL");
        }
        let (_dir, paths) = workspace(CONFIG);
        let transport = crate::llm::testing::ScriptedTransport::new(vec![]);
        let options = ClassifyOptions::default();
        let err = classify(&paths, &options, &transport).unwrap_err();
        assert!(
            matches!(err, CommandError::Llm(ChatError::MissingUrl)),
            "{err}"
        );
    }

    #[test]
    fn grade_rejects_a_backend_other_than_bm25() {
        let (_dir, paths) = workspace(CONFIG);
        let trail_path = paths.config_dir().join("trail.jsonl");
        fs::write(&trail_path, "").unwrap();
        let transport = crate::llm::testing::ScriptedTransport::new(vec![]);
        let options = GradeOptions {
            trail: trail_path,
            backend: Some("dense".to_string()),
            k: None,
            model: None,
            out: None,
        };
        let err = grade(&paths, &options, &transport).unwrap_err();
        assert!(
            matches!(&err, CommandError::Grade(GradeError::UnknownBackend(b)) if b == "dense"),
            "{err}"
        );
    }

    #[test]
    fn grade_replays_distinct_trail_queries_against_bm25_and_writes_graded_rows() {
        let (dir, paths) = workspace(CONFIG);
        let artifact_page = paths.artifact.join("handbook/docs/caching.md");
        fs::create_dir_all(artifact_page.parent().unwrap()).unwrap();
        fs::write(
            &artifact_page,
            "# Caching\n\nEnable upload caching with a label on the bucket.\n",
        )
        .unwrap();
        let trail_path = dir.path().join("trail.jsonl");
        fs::write(
            &trail_path,
            "{\"at\": \"t\", \"query\": \"enable caching\", \"retrieved\": [], \"cited\": []}\n\
             {\"at\": \"t\", \"query\": \"enable caching\", \"retrieved\": [], \"cited\": []}\n",
        )
        .unwrap();

        with_llm_url(|| {
            let reply = crate::llm::testing::completion(
                &serde_json::to_string(&serde_json::json!([
                    {"id": "handbook::docs/caching.md", "grade": 3}
                ]))
                .unwrap(),
            );
            let transport = crate::llm::testing::ScriptedTransport::new(vec![
                crate::llm::testing::Scripted::Ok(reply),
            ]);
            let out_path = dir.path().join("graded.jsonl");
            let options = GradeOptions {
                trail: trail_path,
                backend: None,
                k: None,
                model: Some("grader".to_string()),
                out: Some(out_path.clone()),
            };
            let outcome = grade(&paths, &options, &transport).unwrap();
            // Only one request even though the query appears twice in the trail.
            assert_eq!(transport.requests.lock().unwrap().len(), 1);
            assert_eq!(outcome.queries, 1);
            assert_eq!(outcome.rows.len(), 1);
            assert_eq!(outcome.rows[0].id, "handbook::docs/caching.md");
            assert_eq!(outcome.rows[0].grade, 3);
            let written = fs::read_to_string(&out_path).unwrap();
            assert_eq!(written.lines().count(), 1);
        });
    }

    #[test]
    fn usage_reports_never_retrieved_and_uncited_queries_against_the_manifest() {
        let (dir, paths) = workspace(CONFIG);
        let outcome = resolve(&paths, &opts(), &fetcher()).unwrap();
        assert!(outcome.manifest.sources["handbook"].pages.len() >= 2);

        let trail_path = dir.path().join("trail.jsonl");
        fs::write(
            &trail_path,
            "{\"at\": \"2026-09-16T12:00:00Z\", \"query\": \"a\", \
             \"retrieved\": [\"handbook::docs/a.md\"], \"cited\": [\"handbook::docs/a.md\"]}\n\
             {\"at\": \"2026-09-16T12:00:00Z\", \"query\": \"b\", \
             \"retrieved\": [\"handbook::docs/b.md\"], \"cited\": []}\n",
        )
        .unwrap();

        let options = UsageOptions {
            trail: trail_path,
            since: None,
            json: None,
        };
        let report = usage(&paths, &options).unwrap();
        assert!(
            report
                .retrieved_never_cited
                .contains(&"handbook::docs/b.md".to_string())
        );
        assert_eq!(report.uncited_queries.len(), 1);
        assert_eq!(report.uncited_queries[0].query, "b");
        assert_eq!(
            report.uncited_queries[0].top_retrieved,
            Some("handbook::docs/b.md".to_string())
        );
    }

    #[test]
    fn usage_rejects_a_malformed_since() {
        let (dir, paths) = workspace(CONFIG);
        resolve(&paths, &opts(), &fetcher()).unwrap();
        let trail_path = dir.path().join("trail.jsonl");
        fs::write(&trail_path, "").unwrap();
        let options = UsageOptions {
            trail: trail_path,
            since: Some("not-a-duration".to_string()),
            json: None,
        };
        assert!(matches!(
            usage(&paths, &options).unwrap_err(),
            CommandError::Usage(UsageError::BadSince(_))
        ));
    }

    #[test]
    fn queries_import_appends_rows_from_graded_jsonl() {
        let (dir, paths) = workspace(CONFIG);
        resolve(&paths, &opts(), &fetcher()).unwrap();
        let graded_path = dir.path().join("graded.jsonl");
        fs::write(
            &graded_path,
            "{\"query\": \"a\", \"id\": \"handbook::docs/a.md\", \"grade\": 3, \
             \"model\": \"m\", \"at\": \"t\"}\n",
        )
        .unwrap();
        let queries_path = dir.path().join("queries.jsonl");
        let options = QueriesImportOptions {
            queries: Some(queries_path.clone()),
            graded: graded_path,
            min_grade: 2,
            holdout_share: 0.0,
            seed: 0,
        };
        let outcome = queries_import(&paths, &options).unwrap();
        assert_eq!(outcome.imported.len(), 1);
        assert!(outcome.skipped.is_empty());
        let loaded = eval::load_queries(&queries_path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].expected, ["handbook::docs/a.md"]);
    }
}
