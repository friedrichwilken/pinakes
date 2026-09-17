//! Page selection: the glob resolver, the external resolver (SPEC §3) and the precedence rules
//! that combine policy, resolver output and decisions into pages and residue.
//!
//! Precedence for a file: `policy.deny` > `resolver.exclude` > decisions > resolver selection.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use globset::GlobSet;
use regex::Regex;
use serde::Deserialize;
use thiserror::Error;

use crate::config::{ConfigError, RepoSlug, Resolver, Source, compile_globs, compile_regex};
use crate::decisions::{Decision, Verdict};
use crate::jsonl;
use crate::manifest::{PageEntry, SelectedBy, page_id};
use crate::residue::{EXCERPT_TOKENS, Reason, ResidueEntry, Rule, excerpt};
use crate::sources::{Checkout, SourceError, list_files};
use crate::text::absolutise;
pub use crate::text::{first_h1, frontmatter_title, sha256_hex, strip_frontmatter, title_of};

mod docusaurus;
mod mdbook;
mod navigation;
mod sitemap;
mod vitepress;

/// Errors raised while resolving one source.
#[derive(Debug, Error)]
pub enum ResolveError {
    /// A pattern in the config did not compile (validation should have caught this).
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The checkout could not be listed or read.
    #[error(transparent)]
    Source(#[from] SourceError),
    /// A file under the checkout could not be read.
    #[error("{path}: {source}")]
    Io {
        /// The file path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The external resolver could not be started.
    #[error("source {name}: cannot run {command:?}: {error}")]
    ResolverSpawn {
        /// Source name.
        name: String,
        /// The program that failed to start.
        command: String,
        /// Underlying I/O error.
        error: std::io::Error,
    },
    /// The external resolver exited with a non-zero status.
    #[error("source {name}: resolver exited with {status}\n{stderr}")]
    ResolverFailed {
        /// Source name.
        name: String,
        /// Exit status as reported by the OS.
        status: String,
        /// Everything the command wrote to stderr.
        stderr: String,
    },
    /// The external resolver wrote a line that is not a valid candidate.
    #[error("source {name}: resolver output line {line}: {message}")]
    ResolverOutput {
        /// Source name.
        name: String,
        /// One-based line number of stdout.
        line: usize,
        /// What was wrong.
        message: String,
    },
    /// A navigation-based resolver (`vitepress`, `docusaurus`, `mdbook`, `sitemap`) could not
    /// find or make sense of its navigation file.
    #[error("source {name}: {message}")]
    Navigation {
        /// Source name.
        name: String,
        /// What went wrong.
        message: String,
    },
}

/// One candidate page as emitted by a resolver (the SPEC §3 line).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Candidate {
    /// Path relative to the repository root.
    pub path: String,
    /// Navigation title; empty means "derive it from the file".
    #[serde(default)]
    pub title: String,
    /// `concept|tutorial|reference|troubleshooting|release-notes|` (empty allowed).
    #[serde(default)]
    pub doc_type: String,
    /// Navigation section or TOC branch.
    #[serde(default)]
    pub section: String,
    /// Whether the resolver selects the page; defaults to `true`.
    #[serde(default = "default_true")]
    pub selected: bool,
    /// Extra context for residue; defaults to `section`.
    #[serde(default)]
    pub context: String,
    /// The mechanism the external resolver command itself wants recorded for an unselected
    /// candidate (SPEC §3), e.g. `{"key": "toc:outside-match", "text": "…"}`; when absent,
    /// pinakes assigns [`crate::residue::Rule::external_not_selected`] or
    /// [`crate::residue::Rule::external_unmatched`] instead. Ignored for a selected candidate.
    #[serde(default)]
    pub rule: Option<crate::residue::Rule>,
}

fn default_true() -> bool {
    true
}

impl Candidate {
    /// A bare candidate for `path` with no title or navigation data.
    pub fn bare(path: &str, selected: bool) -> Candidate {
        Candidate {
            path: path.to_string(),
            title: String::new(),
            doc_type: String::new(),
            section: String::new(),
            selected,
            context: String::new(),
            rule: None,
        }
    }
}

/// Parse resolver stdout: one JSON object per non-blank line.
pub fn parse_candidates(text: &str) -> Result<Vec<Candidate>, (usize, String)> {
    let mut out = Vec::new();
    for parsed in jsonl::parse_lines::<Candidate>(text) {
        let (line, candidate) = parsed.map_err(|err| (err.line, err.source.to_string()))?;
        if candidate.path.trim().is_empty() {
            return Err((line, "path must not be empty".to_string()));
        }
        out.push(candidate);
    }
    Ok(out)
}

/// Run an external resolver in `checkout` and parse its output.
///
/// Arguments that name an existing file relative to `config_dir` are made absolute so that
/// scripts kept next to `pinakes.yaml` can be referenced by relative path.
pub fn run_external(
    source: &Source,
    command: &[String],
    args: &[String],
    checkout: &Checkout,
    config_dir: &Path,
) -> Result<Vec<Candidate>, ResolveError> {
    let program = absolutise(config_dir, &command[0]);
    let rest: Vec<String> = command[1..]
        .iter()
        .chain(args)
        .map(|a| absolutise(config_dir, a))
        .collect();
    let output = Command::new(&program)
        .args(&rest)
        .current_dir(&checkout.root)
        .env("PINAKES_SOURCE", &source.name)
        .env("PINAKES_COMMIT", &checkout.commit)
        .output()
        .map_err(|error| ResolveError::ResolverSpawn {
            name: source.name.clone(),
            command: program.clone(),
            error,
        })?;
    if !output.status.success() {
        return Err(ResolveError::ResolverFailed {
            name: source.name.clone(),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr)
                .trim_end()
                .to_string(),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_candidates(&stdout).map_err(|(line, message)| ResolveError::ResolverOutput {
        name: source.name.clone(),
        line,
        message,
    })
}

/// Read a file from `checkout` as lossy UTF-8; used by the navigation-based resolvers to read
/// their navigation file.
fn read_file(checkout: &Checkout, path: &str) -> Result<String, ResolveError> {
    let full = checkout.root.join(path);
    let bytes = std::fs::read(&full).map_err(|source| ResolveError::Io { path: full, source })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Why [`precedence`] decided a file is residue (SPEC §2.4's `rule`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidueCause<'a> {
    /// An active decision excludes the page, regardless of resolver selection.
    Decision(&'a Decision),
    /// The resolver (or the scope-fill pass) did not select the page.
    NotSelected,
}

/// Outcome of the precedence rules for one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome<'a> {
    /// `policy.deny` matched: the file is neither a page nor residue.
    Denied,
    /// `resolver.exclude` matched: the file is neither a page nor residue.
    Excluded,
    /// The file is a page, selected by the given mechanism.
    Selected(SelectedBy),
    /// The file is residue, for the given reason.
    Residue(ResidueCause<'a>),
}

/// Apply `policy.deny` > `resolver.exclude` > decisions > resolver selection.
///
/// `decision` must already be checked against the file hash (an expired decision is `None`);
/// `resolver_selection` is `Some` when the resolver selected the file.
pub fn precedence<'a>(
    path: &str,
    deny: &GlobSet,
    exclude: &GlobSet,
    decision: Option<&'a Decision>,
    resolver_selection: Option<SelectedBy>,
) -> Outcome<'a> {
    if deny.is_match(path) {
        return Outcome::Denied;
    }
    if exclude.is_match(path) {
        return Outcome::Excluded;
    }
    match decision.map(|d| (d, d.decision)) {
        Some((_, Verdict::Include)) => Outcome::Selected(SelectedBy::Decision),
        Some((d, Verdict::Exclude)) => Outcome::Residue(ResidueCause::Decision(d)),
        Some((_, Verdict::Unsure)) | None => match resolver_selection {
            Some(by) => Outcome::Selected(by),
            None => Outcome::Residue(ResidueCause::NotSelected),
        },
    }
}

/// Inputs shared by every source in a resolve run.
pub struct ResolveContext<'a> {
    /// Compiled `policy.deny`.
    pub deny: &'a GlobSet,
    /// Raw `policy.deny` patterns, to name the one that matched in a residue entry's `rule`
    /// (SPEC §2.1, §2.4).
    pub deny_patterns: &'a [String],
    /// Effective decisions by page id.
    pub decisions: &'a BTreeMap<String, Decision>,
    /// Directory of `pinakes.yaml`, for relative resolver script paths.
    pub config_dir: &'a Path,
    /// Whether the source is absent from the previous manifest (residue reason `new_source`).
    pub is_new_source: bool,
}

/// The pages, residue and unresolved links of one source.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ResolvedSource {
    /// Selected pages by relative path.
    pub pages: BTreeMap<String, PageEntry>,
    /// Residue entries sorted by path (unresolved links last).
    pub residue: Vec<ResidueEntry>,
    /// Navigation links with no file, sorted.
    pub unresolved: Vec<String>,
}

/// Resolver-specific inputs collected before the precedence pass.
struct Plan {
    candidates: BTreeMap<String, Candidate>,
    exclude: GlobSet,
    scope: GlobSet,
    mention: Option<Regex>,
    unresolved: Vec<String>,
    /// Paths added by a resolver's optional `include` extra (SPEC §2.1), rather than by the
    /// resolver's own selection mechanism; these are always `selected_by: "include"`.
    include_selected: BTreeSet<String>,
    /// The navigation file a `vitepress`, `docusaurus`, `mdbook` or `sitemap` resolver read
    /// (SPEC §12); `None` for `glob` and `external`, which have no such file.
    nav_path: Option<String>,
    /// Paths the resolver's own mechanism produced a candidate for (selected or not), before
    /// the `include` extra and the scope-fill pass added anything else; used by the `external`
    /// resolver's rule (SPEC §2.4) to tell a file its own output named from one it never
    /// mentioned at all.
    mentioned: BTreeSet<String>,
}

/// The `glob` resolver's effective `extensions` (SPEC §2.1): `configured` verbatim when given,
/// else `["md"]`, or every file (an empty list) when the source has a `render` step.
fn effective_extensions(configured: Option<&[String]>, has_render: bool) -> Vec<String> {
    match configured {
        Some(extensions) => extensions.to_vec(),
        None if has_render => Vec::new(),
        None => vec!["md".to_string()],
    }
}

/// Whether `path`'s extension (case-insensitive, without the dot) is in `extensions`; an empty
/// list matches every file.
fn matches_extension(path: &str, extensions: &[String]) -> bool {
    if extensions.is_empty() {
        return true;
    }
    let Some(ext) = Path::new(path).extension().and_then(|e| e.to_str()) else {
        return false;
    };
    extensions.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

/// Resolver-specific selection: the candidate map plus, for the precedence pass, the compiled
/// `exclude` set, the residue `scope`, an external resolver's `residue_mention`, the resolver's
/// own optional `include` extra (SPEC §2.1) not yet applied to `candidates`, and the navigation
/// file path for the four navigation-based resolvers (SPEC §12), used to name the mechanism
/// behind a residue entry (SPEC §2.4).
type ResolverPlan<'a> = (
    BTreeMap<String, Candidate>,
    GlobSet,
    GlobSet,
    Option<Regex>,
    &'a [String],
    Option<String>,
);

/// Everything [`resolver_plan`] returns besides the candidate map: see [`ResolverPlan`].
type ResolverPlanTail<'a> = (
    GlobSet,
    GlobSet,
    Option<Regex>,
    &'a [String],
    Option<String>,
);

/// The `glob` resolver arm of [`resolver_plan`]: populate `candidates` from `include` (filtered
/// by `extensions`) and return the compiled residue `scope` and `exclude` set.
#[allow(clippy::too_many_arguments)]
fn glob_plan(
    source: &Source,
    files: &[String],
    include: &[String],
    exclude: &[String],
    residue_scope: &[String],
    extensions: Option<&[String]>,
    candidates: &mut BTreeMap<String, Candidate>,
) -> Result<(GlobSet, GlobSet), ResolveError> {
    let include = compile_globs("include", include)?;
    let extensions = effective_extensions(extensions, source.render.is_some());
    for file in files {
        if include.is_match(file) && matches_extension(file, &extensions) {
            candidates.insert(file.clone(), Candidate::bare(file, true));
        }
    }
    let scope = if residue_scope.is_empty() {
        include
    } else {
        compile_globs("residue_scope", residue_scope)?
    };
    Ok((scope, compile_globs("exclude", exclude)?))
}

/// The common shape of every navigation-based resolver arm of [`resolver_plan`]: take the
/// candidate map, residue scope and navigation file path a `vitepress`, `docusaurus`, `mdbook`
/// or `sitemap` plan produced, install the candidates, and compile `exclude`.
fn navigation_plan<'a>(
    planned: Result<(BTreeMap<String, Candidate>, GlobSet, String), ResolveError>,
    exclude: &[String],
    include: &'a [String],
    candidates: &mut BTreeMap<String, Candidate>,
) -> Result<ResolverPlanTail<'a>, ResolveError> {
    let (found, nav_scope, nav_path) = planned?;
    *candidates = found;
    Ok((
        compile_globs("exclude", exclude)?,
        nav_scope,
        None,
        include,
        Some(nav_path),
    ))
}

fn resolver_plan<'a>(
    source: &'a Source,
    checkout: &Checkout,
    files: &[String],
    config_dir: &Path,
) -> Result<ResolverPlan<'a>, ResolveError> {
    let mut candidates: BTreeMap<String, Candidate> = BTreeMap::new();
    let (exclude, scope, mention, extra_include, nav_path): ResolverPlanTail<'a> =
        match &source.resolver {
            Resolver::Glob {
                include,
                exclude,
                residue_scope,
                extensions,
            } => {
                let (scope, exclude) = glob_plan(
                    source,
                    files,
                    include,
                    exclude,
                    residue_scope,
                    extensions.as_deref(),
                    &mut candidates,
                )?;
                (exclude, scope, None, &[], None)
            }
            Resolver::External {
                command,
                args,
                residue_mention,
                residue_scope,
                include,
                exclude,
            } => {
                for candidate in run_external(source, command, args, checkout, config_dir)? {
                    candidates.insert(candidate.path.clone(), candidate);
                }
                let mention = residue_mention
                    .as_deref()
                    .map(|p| compile_regex("residue_mention", p))
                    .transpose()?;
                (
                    compile_globs("exclude", exclude)?,
                    compile_globs("residue_scope", residue_scope)?,
                    mention,
                    include,
                    None,
                )
            }
            Resolver::Vitepress {
                path,
                scope,
                include,
                exclude,
            } => navigation_plan(
                vitepress::plan(source, checkout, files, path.as_deref(), scope),
                exclude,
                include,
                &mut candidates,
            )?,
            Resolver::Docusaurus {
                path,
                scope,
                include,
                exclude,
            } => navigation_plan(
                docusaurus::plan(source, checkout, files, path.as_deref(), scope),
                exclude,
                include,
                &mut candidates,
            )?,
            Resolver::Mdbook {
                path,
                scope,
                include,
                exclude,
            } => navigation_plan(
                mdbook::plan(source, checkout, files, path.as_deref(), scope),
                exclude,
                include,
                &mut candidates,
            )?,
            Resolver::Sitemap {
                path,
                scope,
                url_prefix,
                path_prefix,
                include,
                exclude,
            } => navigation_plan(
                sitemap::plan(
                    source,
                    checkout,
                    files,
                    path.as_deref(),
                    scope,
                    url_prefix,
                    path_prefix,
                ),
                exclude,
                include,
                &mut candidates,
            )?,
        };
    Ok((candidates, exclude, scope, mention, extra_include, nav_path))
}

/// Add files matching a resolver's optional `include` extra (SPEC §2.1) to `candidates`, unless
/// the resolver's own mechanism already selected them; returns the paths added this way, which
/// are always `selected_by: "include"`.
fn apply_extra_include(
    candidates: &mut BTreeMap<String, Candidate>,
    files: &[String],
    extra_include: &[String],
) -> Result<BTreeSet<String>, ResolveError> {
    let mut include_selected = BTreeSet::new();
    if !extra_include.is_empty() {
        let include_set = compile_globs("include", extra_include)?;
        for file in files {
            if include_set.is_match(file) && !candidates.contains_key(file) {
                candidates.insert(file.clone(), Candidate::bare(file, true));
                include_selected.insert(file.clone());
            }
        }
    }
    Ok(include_selected)
}

fn plan(
    source: &Source,
    checkout: &Checkout,
    files: &[String],
    config_dir: &Path,
) -> Result<Plan, ResolveError> {
    let file_set: BTreeSet<&str> = files.iter().map(String::as_str).collect();
    let (mut candidates, exclude, scope, mention, extra_include, nav_path) =
        resolver_plan(source, checkout, files, config_dir)?;
    let mentioned: BTreeSet<String> = candidates.keys().cloned().collect();
    let include_selected = apply_extra_include(&mut candidates, files, extra_include)?;
    let unresolved: Vec<String> = candidates
        .keys()
        .filter(|p| !file_set.contains(p.as_str()))
        .cloned()
        .collect();
    Ok(Plan {
        candidates,
        exclude,
        scope,
        mention,
        unresolved,
        include_selected,
        nav_path,
        mentioned,
    })
}

/// The upstream URL of `path` pinned to `checkout.commit` (SPEC §2.4): `{base_url}/{path}`
/// where `base_url` is `https://github.com/<owner>/<repo>/blob/<commit>`, or empty when
/// `source.repo` does not parse as a GitHub URL.
fn page_url(source: &Source, checkout: &Checkout, path: &str) -> String {
    RepoSlug::parse(&source.repo)
        .map(|slug| format!("{}/{path}", slug.blob_base_url(&checkout.commit)))
        .unwrap_or_default()
}

/// Resolve one source from its checkout.
pub fn resolve_source(
    source: &Source,
    checkout: &Checkout,
    ctx: &ResolveContext<'_>,
) -> Result<ResolvedSource, ResolveError> {
    let files = list_files(&checkout.root)?;
    let mut plan = plan(source, checkout, &files, ctx.config_dir)?;
    let mut result = ResolvedSource {
        residue: unresolved_entries(source, checkout, &mut plan),
        unresolved: std::mem::take(&mut plan.unresolved),
        ..ResolvedSource::default()
    };
    fill_scope_candidates(&mut plan, &files, source, ctx);

    let resolver_kind = resolver_kind_of(&source.resolver);
    let reason = if ctx.is_new_source {
        Reason::NewSource
    } else {
        Reason::NotSelected
    };
    for (path, candidate) in &plan.candidates {
        classify_candidate(
            source,
            checkout,
            ctx,
            &plan,
            resolver_kind,
            reason,
            path,
            candidate,
            &mut result,
        )?;
    }
    result
        .residue
        .sort_by(|a, b| (a.reason, &a.path).cmp(&(b.reason, &b.path)));
    Ok(result)
}

/// Turn each of `plan.unresolved`'s dangling links into a residue entry, removing the
/// placeholder candidate pinakes created for it (SPEC §2.4's `unresolved_link` reason).
///
/// A dangling link has no file of its own to point at, so its url is the navigation file that
/// linked it, at the fetched commit; the external resolver has no navigation file, so it falls
/// back to the (nonexistent) target path itself, with a rule of its own
/// ([`Rule::external_dangling_link`]) instead of [`Rule::nav_dangling_link`].
fn unresolved_entries(source: &Source, checkout: &Checkout, plan: &mut Plan) -> Vec<ResidueEntry> {
    let unresolved_url = plan
        .nav_path
        .as_deref()
        .map(|nav| page_url(source, checkout, nav));
    let rule = match &plan.nav_path {
        Some(nav) => Rule::nav_dangling_link(nav),
        None => Rule::external_dangling_link(),
    };
    plan.unresolved
        .clone()
        .iter()
        .map(|path| {
            let candidate = plan
                .candidates
                .remove(path)
                .unwrap_or_else(|| Candidate::bare(path, false));
            ResidueEntry {
                id: page_id(&source.name, path),
                source: source.name.clone(),
                path: path.clone(),
                reason: Reason::UnresolvedLink,
                sha256: String::new(),
                title: candidate.title,
                excerpt: String::new(),
                context: context_of(&candidate.context, &candidate.section),
                url: unresolved_url
                    .clone()
                    .unwrap_or_else(|| page_url(source, checkout, path)),
                rule: Some(rule.clone()),
            }
        })
        .collect()
}

/// Add a bare, unselected candidate for every file in `plan.scope` (or with a decision on
/// record) that no resolver mechanism already produced a candidate for, so the precedence pass
/// sees it and can report it as residue.
fn fill_scope_candidates(
    plan: &mut Plan,
    files: &[String],
    source: &Source,
    ctx: &ResolveContext<'_>,
) {
    let prefix = format!("{}::", source.name);
    for file in files {
        let in_scope = plan.scope.is_match(file);
        let decided = ctx.decisions.contains_key(&format!("{prefix}{file}"));
        if !plan.candidates.contains_key(file) && (in_scope || decided) {
            plan.candidates
                .insert(file.clone(), Candidate::bare(file, false));
        }
    }
}

/// What a resolver's own selection mechanism counts as (SPEC §2.2's `selected_by`): a plain
/// `glob` include is `"include"`, everything else (including a resolver's own `include` extra,
/// handled separately) is `"resolver"`.
fn resolver_kind_of(resolver: &Resolver) -> SelectedBy {
    match resolver {
        Resolver::Glob { .. } => SelectedBy::Include,
        Resolver::External { .. }
        | Resolver::Vitepress { .. }
        | Resolver::Docusaurus { .. }
        | Resolver::Mdbook { .. }
        | Resolver::Sitemap { .. } => SelectedBy::Resolver,
    }
}

/// A resolver's own `exclude` glob patterns (SPEC §2.1); every resolver kind has one.
fn resolver_exclude(resolver: &Resolver) -> &[String] {
    match resolver {
        Resolver::Glob { exclude, .. }
        | Resolver::External { exclude, .. }
        | Resolver::Vitepress { exclude, .. }
        | Resolver::Docusaurus { exclude, .. }
        | Resolver::Mdbook { exclude, .. }
        | Resolver::Sitemap { exclude, .. } => exclude,
    }
}

/// The first pattern in `patterns` that matches `path`, to name in a residue entry's `rule`
/// (SPEC §2.4); a pattern that fails to compile (validation should have caught this already) is
/// skipped rather than panicking.
fn first_matching_pattern(patterns: &[String], path: &str) -> Option<String> {
    patterns
        .iter()
        .find(|pattern| {
            globset::Glob::new(pattern).is_ok_and(|g| g.compile_matcher().is_match(path))
        })
        .cloned()
}

/// The rule (SPEC §2.4) for a candidate the resolver's own mechanism did not select: which one
/// depends on the resolver kind and, for `glob` and `external`, on why exactly.
fn not_selected_rule(source: &Source, plan: &Plan, path: &str, candidate: &Candidate) -> Rule {
    match &source.resolver {
        Resolver::Glob {
            include,
            extensions,
            ..
        } => {
            let include_set =
                compile_globs("include", include).unwrap_or_else(|_| GlobSet::empty());
            let configured = effective_extensions(extensions.as_deref(), source.render.is_some());
            if include_set.is_match(path) && !matches_extension(path, &configured) {
                Rule::glob_extension(&configured)
            } else {
                Rule::glob_outside_include()
            }
        }
        Resolver::External { .. } => candidate.rule.clone().unwrap_or_else(|| {
            if plan.mentioned.contains(path) {
                Rule::external_not_selected()
            } else {
                Rule::external_unmatched()
            }
        }),
        Resolver::Vitepress { .. } => {
            Rule::sidebar_unlinked(plan.nav_path.as_deref().unwrap_or_default())
        }
        Resolver::Docusaurus { .. } => {
            Rule::docusaurus_unlinked(plan.nav_path.as_deref().unwrap_or_default())
        }
        Resolver::Mdbook { .. } => {
            Rule::mdbook_unlinked(plan.nav_path.as_deref().unwrap_or_default())
        }
        Resolver::Sitemap { .. } => {
            Rule::sitemap_unlisted(plan.nav_path.as_deref().unwrap_or_default())
        }
    }
}

/// Apply the precedence rules to one candidate, inserting it into `result.pages` or
/// `result.residue` as appropriate.
#[allow(clippy::too_many_arguments)]
fn classify_candidate(
    source: &Source,
    checkout: &Checkout,
    ctx: &ResolveContext<'_>,
    plan: &Plan,
    resolver_kind: SelectedBy,
    reason: Reason,
    path: &str,
    candidate: &Candidate,
    result: &mut ResolvedSource,
) -> Result<(), ResolveError> {
    let id = page_id(&source.name, path);
    let full = checkout.root.join(path);
    let bytes = std::fs::read(&full).map_err(|source| ResolveError::Io {
        path: full.clone(),
        source,
    })?;
    let sha256 = sha256_hex(&bytes);
    let text = String::from_utf8_lossy(&bytes);
    let decision = ctx.decisions.get(&id).filter(|d| d.applies_to(&sha256));
    let by = if plan.include_selected.contains(path) {
        SelectedBy::Include
    } else {
        resolver_kind
    };
    let selection = candidate.selected.then_some(by);
    match precedence(path, ctx.deny, &plan.exclude, decision, selection) {
        Outcome::Denied => excluded_residue(
            source,
            checkout,
            path,
            &sha256,
            &text,
            Rule::policy_deny(&first_matching_pattern(ctx.deny_patterns, path).unwrap_or_default()),
            result,
        ),
        Outcome::Excluded => excluded_residue(
            source,
            checkout,
            path,
            &sha256,
            &text,
            Rule::resolver_exclude(
                &first_matching_pattern(resolver_exclude(&source.resolver), path)
                    .unwrap_or_default(),
            ),
            result,
        ),
        Outcome::Selected(selected_by) => {
            result.pages.insert(
                path.to_string(),
                PageEntry {
                    sha256,
                    title: title_of(&candidate.title, &text),
                    doc_type: candidate.doc_type.clone(),
                    section: candidate.section.clone(),
                    selected_by,
                    rendered_from: None,
                },
            );
        }
        Outcome::Residue(cause) => {
            if plan.mention.as_ref().is_some_and(|re| !re.is_match(&text)) {
                return Ok(());
            }
            let rule = match cause {
                ResidueCause::Decision(d) => Rule::decision_exclude(&d.by, &d.reason),
                // A brand new source's residue is uniformly unreviewed (SPEC §2.4's
                // `source:new`), regardless of which resolver mechanism did not select it.
                ResidueCause::NotSelected if reason == Reason::NewSource => Rule::source_new(),
                ResidueCause::NotSelected => not_selected_rule(source, plan, path, candidate),
            };
            result.residue.push(ResidueEntry {
                id,
                source: source.name.clone(),
                path: path.to_string(),
                reason,
                sha256,
                title: title_of(&candidate.title, &text),
                excerpt: excerpt(strip_frontmatter(&text), EXCERPT_TOKENS),
                context: context_of(&candidate.context, &candidate.section),
                url: page_url(source, checkout, path),
                rule: Some(rule),
            });
        }
    }
    Ok(())
}

/// Record a file `policy.deny` or a resolver's `exclude` kept out of both the corpus and the
/// ordinary residue pass, as residue in its own right (reason [`Reason::Excluded`], SPEC §2.4)
/// so nothing disappears from view without a trace.
#[allow(clippy::too_many_arguments)]
fn excluded_residue(
    source: &Source,
    checkout: &Checkout,
    path: &str,
    sha256: &str,
    text: &str,
    rule: Rule,
    result: &mut ResolvedSource,
) {
    result.residue.push(ResidueEntry {
        id: page_id(&source.name, path),
        source: source.name.clone(),
        path: path.to_string(),
        reason: Reason::Excluded,
        sha256: sha256.to_string(),
        title: title_of("", text),
        excerpt: excerpt(strip_frontmatter(text), EXCERPT_TOKENS),
        context: String::new(),
        url: page_url(source, checkout, path),
        rule: Some(rule),
    });
}

/// Turn `resolved`'s would-be pages into residue (reason [`Reason::Excluded`], rule
/// [`Rule::source_archived`], SPEC §2.4) because `policy.archived` dropped the whole source;
/// `resolved`'s own residue entries are kept as they are, since those files were never going to
/// join the corpus regardless of the source's archived state.
pub fn archived_residue(
    source: &Source,
    checkout: &Checkout,
    resolved: ResolvedSource,
) -> Vec<ResidueEntry> {
    let mut entries = resolved.residue;
    for (path, page) in resolved.pages {
        let full = checkout.root.join(&path);
        let excerpt_text = std::fs::read(&full)
            .map(|bytes| {
                excerpt(
                    strip_frontmatter(&String::from_utf8_lossy(&bytes)),
                    EXCERPT_TOKENS,
                )
            })
            .unwrap_or_default();
        entries.push(ResidueEntry {
            id: page_id(&source.name, &path),
            source: source.name.clone(),
            path: path.clone(),
            reason: Reason::Excluded,
            sha256: page.sha256,
            title: page.title,
            excerpt: excerpt_text,
            context: String::new(),
            url: page_url(source, checkout, &path),
            rule: Some(Rule::source_archived()),
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries
}

fn context_of(context: &str, section: &str) -> String {
    if context.is_empty() {
        section.to_string()
    } else {
        context.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::fs;

    const SHA: &str = "4427d7ba863973c2cea9da74ed8675c5c74aee77";

    /// A checkout on disk with the given files.
    fn checkout(files: &[(&str, &str)]) -> (tempfile::TempDir, Checkout) {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let full = dir.path().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, content).unwrap();
        }
        let checkout = Checkout {
            root: dir.path().to_path_buf(),
            commit: SHA.to_string(),
        };
        (dir, checkout)
    }

    fn glob_source(yaml_resolver: &str) -> Source {
        let text = format!(
            "version: 1\nsources:\n  - name: s\n    repo: https://github.com/o/r\n    ref: main\n    \
             resolver:\n{yaml_resolver}"
        );
        Config::from_yaml(&text).unwrap().sources.remove(0)
    }

    fn glob_source_with_render(yaml_resolver: &str, yaml_render: &str) -> Source {
        let text = format!(
            "version: 1\nsources:\n  - name: s\n    repo: https://github.com/o/r\n    ref: main\n    \
             resolver:\n{yaml_resolver}    render:\n{yaml_render}"
        );
        Config::from_yaml(&text).unwrap().sources.remove(0)
    }

    fn resolve(
        source: &Source,
        checkout: &Checkout,
        deny: &[&str],
        decisions: &[Decision],
        is_new_source: bool,
    ) -> ResolvedSource {
        let deny_patterns: Vec<String> = deny.iter().map(|s| (*s).to_string()).collect();
        let deny = compile_globs("deny", &deny_patterns).unwrap();
        let decisions = crate::decisions::effective(decisions);
        let ctx = ResolveContext {
            deny: &deny,
            deny_patterns: &deny_patterns,
            decisions: &decisions,
            config_dir: Path::new("."),
            is_new_source,
        };
        resolve_source(source, checkout, &ctx).unwrap()
    }

    fn decision(path: &str, sha256: &str, verdict: Verdict) -> Decision {
        Decision {
            id: format!("s::{path}"),
            sha256: sha256.to_string(),
            decision: verdict,
            reason: String::new(),
            by: String::new(),
            at: String::new(),
        }
    }

    #[test]
    fn glob_resolver_applies_include_exclude_and_deny() {
        let (_dir, co) = checkout(&[
            ("docs/user/README.md", "# Handbook\n\nIntro.\n"),
            ("docs/user/_sidebar.md", "* [x](x.md)\n"),
            ("docs/user/adr/001.md", "# ADR\n"),
            (
                "docs/user/sub/deep.md",
                "---\ntitle: Deep Page\n---\nno h1\n",
            ),
            ("docs/contributor/x.md", "# Contributor\n"),
            ("README.md", "# Top\n"),
        ]);
        let source = glob_source(
            "      type: glob\n      include: ['docs/user/**/*.md']\n      exclude: ['**/_sidebar.md']\n",
        );
        let result = resolve(&source, &co, &["**/adr/**"], &[], false);
        let paths: Vec<&String> = result.pages.keys().collect();
        assert_eq!(paths, ["docs/user/README.md", "docs/user/sub/deep.md"]);
        let readme = &result.pages["docs/user/README.md"];
        assert_eq!(readme.title, "Handbook");
        assert_eq!(readme.selected_by, SelectedBy::Include);
        assert_eq!(readme.sha256, sha256_hex(b"# Handbook\n\nIntro.\n"));
        assert_eq!(result.pages["docs/user/sub/deep.md"].title, "Deep Page");
        let excluded: Vec<(&str, &str)> = result
            .residue
            .iter()
            .map(|r| (r.path.as_str(), r.rule.as_ref().unwrap().key.as_str()))
            .collect();
        assert_eq!(
            excluded,
            [
                ("docs/user/_sidebar.md", "resolver:exclude"),
                ("docs/user/adr/001.md", "policy:deny"),
            ],
            "excluded and denied files are residue in their own right (SPEC §2.4)"
        );
        assert!(result.residue.iter().all(|r| r.reason == Reason::Excluded));
        assert!(result.unresolved.is_empty());
    }

    #[test]
    fn glob_residue_scope_reports_files_outside_include() {
        let (_dir, co) = checkout(&[
            ("docs/user/a.md", "# A\n"),
            ("docs/user/b.txt", "not markdown"),
            ("docs/internal/c.md", "# C\n\nsecret storage text\n"),
        ]);
        let source = glob_source(
            "      type: glob\n      include: ['docs/user/**/*.md']\n      residue_scope: ['docs/**/*.md']\n",
        );
        let result = resolve(&source, &co, &[], &[], false);
        assert_eq!(result.pages.len(), 1);
        assert_eq!(result.residue.len(), 1);
        let residue = &result.residue[0];
        assert_eq!(residue.id, "s::docs/internal/c.md");
        assert_eq!(residue.reason, Reason::NotSelected);
        assert_eq!(residue.title, "C");
        assert_eq!(residue.excerpt, "# C secret storage text");
        assert_eq!(residue.sha256, sha256_hex(b"# C\n\nsecret storage text\n"));
        assert_eq!(
            residue.rule.as_ref().unwrap().key,
            "glob:outside-include",
            "outside include, not an extension mismatch"
        );

        let result = resolve(&source, &co, &[], &[], true);
        assert_eq!(result.residue[0].reason, Reason::NewSource);
        assert_eq!(result.residue[0].rule.as_ref().unwrap().key, "source:new");
    }

    #[test]
    fn decisions_override_resolver_selection_but_not_deny_or_exclude() {
        let (_dir, co) = checkout(&[
            ("docs/a.md", "# A\n"),
            ("docs/b.md", "# B\n"),
            ("docs/c.md", "# C\n"),
            ("other/d.md", "# D\n"),
            ("docs/x.md", "# X\n"),
        ]);
        let source = glob_source(
            "      type: glob\n      include: ['docs/*.md']\n      exclude: ['docs/x.md']\n",
        );
        let sha = |s: &str| sha256_hex(s.as_bytes());
        let decisions = vec![
            decision("docs/a.md", &sha("# A\n"), Verdict::Exclude),
            decision("docs/b.md", "stale", Verdict::Exclude),
            decision("docs/c.md", &sha("# C\n"), Verdict::Unsure),
            decision("other/d.md", &sha("# D\n"), Verdict::Include),
            decision("docs/x.md", &sha("# X\n"), Verdict::Include),
        ];
        let result = resolve(&source, &co, &["**/x.md"], &decisions, false);
        let paths: Vec<&String> = result.pages.keys().collect();
        assert_eq!(paths, ["docs/b.md", "docs/c.md", "other/d.md"]);
        assert_eq!(
            result.pages["docs/b.md"].selected_by,
            SelectedBy::Include,
            "stale decision"
        );
        assert_eq!(
            result.pages["docs/c.md"].selected_by,
            SelectedBy::Include,
            "unsure"
        );
        assert_eq!(result.pages["other/d.md"].selected_by, SelectedBy::Decision);
        let residue: Vec<&str> = result.residue.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            residue,
            ["docs/a.md", "docs/x.md"],
            "decision-excluded page stays residue; policy.deny beats the include decision too, \
             now as residue in its own right (SPEC §2.4)"
        );
        let a = &result.residue[0];
        assert_eq!(a.reason, Reason::NotSelected);
        assert_eq!(a.rule.as_ref().unwrap().key, "decision:exclude");
        let x = &result.residue[1];
        assert_eq!(x.reason, Reason::Excluded);
        assert_eq!(x.rule.as_ref().unwrap().key, "policy:deny");
    }

    #[test]
    fn precedence_table() {
        let deny = compile_globs("d", &["deny/**".to_string()]).unwrap();
        let exclude = compile_globs("e", &["ex/**".to_string()]).unwrap();
        let inc = decision("p", "h", Verdict::Include);
        let exc = decision("p", "h", Verdict::Exclude);
        let uns = decision("p", "h", Verdict::Unsure);
        let sel = Some(SelectedBy::Resolver);
        assert_eq!(
            precedence("deny/a", &deny, &exclude, Some(&inc), sel),
            Outcome::Denied
        );
        assert_eq!(
            precedence("ex/a", &deny, &exclude, Some(&inc), sel),
            Outcome::Excluded
        );
        assert_eq!(
            precedence("a", &deny, &exclude, Some(&inc), None),
            Outcome::Selected(SelectedBy::Decision)
        );
        assert_eq!(
            precedence("a", &deny, &exclude, Some(&exc), sel),
            Outcome::Residue(ResidueCause::Decision(&exc))
        );
        assert_eq!(
            precedence("a", &deny, &exclude, Some(&uns), sel),
            Outcome::Selected(SelectedBy::Resolver)
        );
        assert_eq!(
            precedence("a", &deny, &exclude, None, sel),
            Outcome::Selected(SelectedBy::Resolver)
        );
        assert_eq!(
            precedence("a", &deny, &exclude, None, None),
            Outcome::Residue(ResidueCause::NotSelected)
        );
    }

    /// An external resolver written as a shell script so the test needs nothing but `sh`.
    fn external_source(dir: &Path, script: &str) -> Source {
        let path = dir.join("resolver.sh");
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        glob_source(&format!(
            "      type: external\n      command: ['sh', '{}']\n      args: ['--flag']\n      \
             residue_scope: ['docs/**/*.md']\n      residue_mention: '(?i)storage'\n",
            path.display()
        ))
    }

    #[test]
    fn external_resolver_contract() {
        let (_dir, co) = checkout(&[
            ("docs/a.md", "# A\n\ntext\n"),
            (
                "docs/b.md",
                "---\ntitle: B Front\n---\n\nmentions storage\n",
            ),
            ("docs/c.md", "# C\n\nno mention\n"),
            ("docs/d.md", "# D\n\nstorage again\n"),
            ("docs/e.md", "# E\n\nunmentioned storage page\n"),
            ("docs/f.md", "# F\n\nunmentioned quiet page\n"),
        ]);
        let script_dir = tempfile::tempdir().unwrap();
        let source = external_source(
            script_dir.path(),
            r#"[ "$1" = "--flag" ] || exit 9
printf '{"path":"docs/a.md","title":"Nav A","doc_type":"concept","section":"%s@%s","selected":true}\n' "$PINAKES_SOURCE" "$PINAKES_COMMIT"
printf '{"path":"docs/b.md","doc_type":"tutorial"}\n'
printf '\n{"path":"docs/c.md","selected":false,"section":"Left"}\n'
printf '{"path":"docs/d.md","selected":false,"context":"TOC > D"}\n'
printf '{"path":"docs/missing.md","title":"Ghost","selected":true,"section":"Nav"}\n'
[ -f docs/a.md ] || exit 7
"#,
        );
        let result = resolve(&source, &co, &[], &[], false);
        let a = &result.pages["docs/a.md"];
        assert_eq!(a.title, "Nav A");
        assert_eq!(a.doc_type, "concept");
        assert_eq!(a.section, format!("s@{SHA}"), "env vars reach the resolver");
        assert_eq!(a.selected_by, SelectedBy::Resolver);
        assert_eq!(
            result.pages["docs/b.md"].title, "B Front",
            "frontmatter fallback"
        );
        assert_eq!(result.pages.len(), 2);
        assert_eq!(result.unresolved, ["docs/missing.md"]);
        let residue: Vec<(&str, Reason, &str)> = result
            .residue
            .iter()
            .map(|r| (r.path.as_str(), r.reason, r.context.as_str()))
            .collect();
        assert_eq!(
            residue,
            [
                ("docs/d.md", Reason::NotSelected, "TOC > D"),
                ("docs/e.md", Reason::NotSelected, ""),
                ("docs/missing.md", Reason::UnresolvedLink, "Nav"),
            ],
            "c and f lack the residue_mention text; e is in scope but never mentioned"
        );
        assert_eq!(result.residue[2].title, "Ghost");
        assert!(result.residue[2].sha256.is_empty());
        let rule_keys: Vec<&str> = result
            .residue
            .iter()
            .map(|r| r.rule.as_ref().unwrap().key.as_str())
            .collect();
        assert_eq!(
            rule_keys,
            [
                "external:not-selected",
                "external:unmatched",
                "external:dangling-link",
            ],
            "d.md was reported unselected; e.md was never mentioned at all; missing.md has no \
             navigation file to point at (SPEC §2.4)"
        );
    }

    #[test]
    fn external_resolver_candidate_supplied_rule_is_used_verbatim() {
        let (_dir, co) = checkout(&[("docs/a.md", "# A\n"), ("docs/b.md", "# B\n")]);
        let script_dir = tempfile::tempdir().unwrap();
        let path = script_dir.path().join("resolver.sh");
        fs::write(
            &path,
            "#!/bin/sh\n\
             printf '{\"path\":\"docs/a.md\",\"selected\":false,\"rule\":{\"key\":\"toc:outside-match\",\"text\":\"outside the table-of-contents subtrees matching docs/guide/**\"}}\\n'\n\
             printf '{\"path\":\"docs/b.md\",\"selected\":false}\\n'\n",
        )
        .unwrap();
        let source = glob_source(&format!(
            "      type: external\n      command: ['sh', '{}']\n",
            path.display()
        ));
        let result = resolve(&source, &co, &[], &[], false);
        let a = result
            .residue
            .iter()
            .find(|r| r.path == "docs/a.md")
            .unwrap();
        let rule = a.rule.as_ref().unwrap();
        assert_eq!(rule.key, "toc:outside-match");
        assert_eq!(
            rule.text,
            "outside the table-of-contents subtrees matching docs/guide/**"
        );
        let b = result
            .residue
            .iter()
            .find(|r| r.path == "docs/b.md")
            .unwrap();
        assert_eq!(
            b.rule.as_ref().unwrap().key,
            "external:not-selected",
            "no candidate-supplied rule falls back to the default"
        );
    }

    #[test]
    fn external_resolver_failures_carry_stderr_and_line_numbers() {
        let (_dir, co) = checkout(&[("docs/a.md", "# A\n")]);
        let script_dir = tempfile::tempdir().unwrap();
        let source = external_source(script_dir.path(), "echo 'boom: bad toc' >&2; exit 3");
        let deny = GlobSet::empty();
        let decisions = BTreeMap::new();
        let ctx = ResolveContext {
            deny: &deny,
            deny_patterns: &[],
            decisions: &decisions,
            config_dir: Path::new("."),
            is_new_source: false,
        };
        let err = resolve_source(&source, &co, &ctx).unwrap_err();
        match err {
            ResolveError::ResolverFailed {
                name,
                status,
                stderr,
            } => {
                assert_eq!(name, "s");
                assert!(status.contains('3'), "{status}");
                assert_eq!(stderr, "boom: bad toc");
            }
            other => panic!("unexpected {other}"),
        }

        let source = external_source(
            script_dir.path(),
            "echo '{\"path\":\"docs/a.md\"}'; echo '{oops'",
        );
        let err = resolve_source(&source, &co, &ctx).unwrap_err();
        assert!(
            matches!(err, ResolveError::ResolverOutput { line: 2, .. }),
            "{err}"
        );

        let source = external_source(script_dir.path(), "echo '{\"title\":\"no path\"}'");
        let err = resolve_source(&source, &co, &ctx).unwrap_err();
        assert!(
            matches!(err, ResolveError::ResolverOutput { line: 1, .. }),
            "{err}"
        );

        let mut source = source;
        source.resolver = Resolver::External {
            command: vec!["definitely-not-a-program-xyz".to_string()],
            args: vec![],
            residue_mention: None,
            residue_scope: vec![],
            include: vec![],
            exclude: vec![],
        };
        let err = resolve_source(&source, &co, &ctx).unwrap_err();
        assert!(matches!(err, ResolveError::ResolverSpawn { .. }), "{err}");
    }

    #[test]
    fn relative_script_paths_resolve_against_the_config_dir() {
        let config_dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(config_dir.path().join("resolvers")).unwrap();
        fs::write(
            config_dir.path().join("resolvers/r.sh"),
            "#!/bin/sh\necho '{\"path\":\"docs/a.md\"}'\n",
        )
        .unwrap();
        let (_dir, co) = checkout(&[("docs/a.md", "# A\n")]);
        let source = glob_source("      type: external\n      command: ['sh', 'resolvers/r.sh']\n");
        let deny = GlobSet::empty();
        let decisions = BTreeMap::new();
        let ctx = ResolveContext {
            deny: &deny,
            deny_patterns: &[],
            decisions: &decisions,
            config_dir: config_dir.path(),
            is_new_source: false,
        };
        let result = resolve_source(&source, &co, &ctx).unwrap();
        assert_eq!(result.pages.len(), 1);
        assert_eq!(absolutise(config_dir.path(), "python3"), "python3");
        assert_eq!(
            absolutise(config_dir.path(), "--title-match"),
            "--title-match"
        );
    }

    #[test]
    fn parse_candidates_defaults_selected_to_true() {
        let parsed =
            parse_candidates("{\"path\":\"a.md\"}\n\n{\"path\":\"b.md\",\"selected\":false}\n")
                .unwrap();
        assert!(parsed[0].selected);
        assert!(!parsed[1].selected);
        assert_eq!(parse_candidates("{\"path\":\"\"}").unwrap_err().0, 1);
    }

    #[test]
    fn external_resolver_include_selects_a_file_the_command_never_mentions() {
        let (_dir, co) = checkout(&[
            ("docs/a.md", "# A\n"),
            ("README.md", "# Landing\n\nNot in the TOC.\n"),
        ]);
        let script_dir = tempfile::tempdir().unwrap();
        let path = script_dir.path().join("resolver.sh");
        fs::write(
            &path,
            "#!/bin/sh\nprintf '{\"path\":\"docs/a.md\",\"title\":\"A\"}\\n'\n",
        )
        .unwrap();
        let source = glob_source(&format!(
            "      type: external\n      command: ['sh', '{}']\n      include: ['README.md']\n",
            path.display()
        ));
        let result = resolve(&source, &co, &[], &[], false);
        assert_eq!(result.pages["docs/a.md"].selected_by, SelectedBy::Resolver);
        let landing = &result.pages["README.md"];
        assert_eq!(landing.title, "Landing");
        assert_eq!(landing.doc_type, "");
        assert_eq!(landing.section, "");
        assert_eq!(landing.selected_by, SelectedBy::Include);
        assert!(
            result.residue.is_empty(),
            "include selection is never residue"
        );
    }

    #[test]
    fn vitepress_include_selects_an_unlinked_landing_readme() {
        const CONFIG: &str = "export default { themeConfig: { sidebar: [] } }";
        let (_dir, co) = checkout(&[
            ("docs/.vitepress/config.ts", CONFIG),
            ("docs/README.md", "# Docs Home\n\nLanding page.\n"),
        ]);

        let source = glob_source("      type: vitepress\n");
        let result = resolve(&source, &co, &[], &[], false);
        assert!(result.pages.is_empty());
        assert_eq!(
            result.residue[0].path, "docs/README.md",
            "unselected but in scope"
        );

        let source = glob_source("      type: vitepress\n      include: ['docs/README.md']\n");
        let result = resolve(&source, &co, &[], &[], false);
        let page = &result.pages["docs/README.md"];
        assert_eq!(page.title, "Docs Home");
        assert_eq!(page.doc_type, "");
        assert_eq!(page.section, "");
        assert_eq!(page.selected_by, SelectedBy::Include);
        assert!(
            result.residue.is_empty(),
            "include selection is never residue"
        );
    }

    #[test]
    fn docusaurus_include_selects_an_unlinked_landing_readme() {
        const SIDEBARS: &str = "module.exports = { tutorialSidebar: [] };";
        let (_dir, co) = checkout(&[
            ("sidebars.js", SIDEBARS),
            ("docs/orphan.md", "# Orphan Page\n\nNot in any sidebar.\n"),
        ]);

        let source = glob_source("      type: docusaurus\n      include: ['docs/orphan.md']\n");
        let result = resolve(&source, &co, &[], &[], false);
        let page = &result.pages["docs/orphan.md"];
        assert_eq!(page.title, "Orphan Page");
        assert_eq!(page.doc_type, "");
        assert_eq!(page.section, "");
        assert_eq!(page.selected_by, SelectedBy::Include);
        assert!(
            result.residue.is_empty(),
            "include selection is never residue"
        );
    }

    #[test]
    fn mdbook_include_selects_an_unlinked_landing_readme() {
        const SUMMARY: &str = "# Summary\n";
        let (_dir, co) = checkout(&[
            ("src/SUMMARY.md", SUMMARY),
            ("src/orphan.md", "# Orphan Page\n\nNot in SUMMARY.md.\n"),
        ]);

        // `exclude` keeps SUMMARY.md itself out of the ordinary residue pass: it sits inside the
        // default residue scope (`src/**/*.md`) but is never linked from itself, which is
        // unrelated to `include`; it is still residue in its own right (SPEC §2.4), as `excluded`.
        let source = glob_source(
            "      type: mdbook\n      include: ['src/orphan.md']\n      exclude: ['src/SUMMARY.md']\n",
        );
        let result = resolve(&source, &co, &[], &[], false);
        let page = &result.pages["src/orphan.md"];
        assert_eq!(page.title, "Orphan Page");
        assert_eq!(page.doc_type, "");
        assert_eq!(page.section, "");
        assert_eq!(page.selected_by, SelectedBy::Include);
        assert_eq!(
            result.residue.len(),
            1,
            "include selection is never residue, but the excluded SUMMARY.md is"
        );
        assert_eq!(result.residue[0].path, "src/SUMMARY.md");
        assert_eq!(result.residue[0].reason, Reason::Excluded);
    }

    #[test]
    fn sitemap_include_selects_an_unlinked_landing_readme() {
        const SITEMAP: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
</urlset>
"#;
        let (_dir, co) = checkout(&[
            ("sitemap.xml", SITEMAP),
            ("docs/orphan.md", "# Orphan Page\n\nNot in the sitemap.\n"),
        ]);

        let source = glob_source(
            "      type: sitemap\n      url_prefix: 'https://example.com/docs/'\n      \
             path_prefix: 'docs/'\n      include: ['docs/orphan.md']\n",
        );
        let result = resolve(&source, &co, &[], &[], false);
        let page = &result.pages["docs/orphan.md"];
        assert_eq!(page.title, "Orphan Page");
        assert_eq!(page.doc_type, "");
        assert_eq!(page.section, "");
        assert_eq!(page.selected_by, SelectedBy::Include);
        assert!(
            result.residue.is_empty(),
            "include selection is never residue"
        );
    }

    #[test]
    fn exclude_removes_a_file_from_navigation_and_external_selection() {
        const CONFIG: &str = "export default { themeConfig: { sidebar: [] } }";
        let (_dir, co) = checkout(&[
            ("docs/.vitepress/config.ts", CONFIG),
            ("docs/README.md", "# Docs Home\n"),
        ]);
        let source = glob_source(
            "      type: vitepress\n      include: ['docs/README.md']\n      exclude: ['docs/README.md']\n",
        );
        let result = resolve(&source, &co, &[], &[], false);
        assert!(result.pages.is_empty(), "exclude beats include");
        assert_eq!(
            result.residue.len(),
            1,
            "excluded files are residue in their own right (SPEC §2.4), not silently dropped"
        );
        assert_eq!(result.residue[0].path, "docs/README.md");
        assert_eq!(result.residue[0].reason, Reason::Excluded);
        assert_eq!(
            result.residue[0].rule.as_ref().unwrap().key,
            "resolver:exclude"
        );
    }

    #[test]
    fn effective_extensions_defaults_to_markdown_or_every_file_with_render() {
        assert_eq!(effective_extensions(None, false), vec!["md".to_string()]);
        assert_eq!(effective_extensions(None, true), Vec::<String>::new());
        assert_eq!(
            effective_extensions(Some(&["yaml".to_string()]), true),
            vec!["yaml".to_string()],
            "an explicit list wins over the render default"
        );
        assert_eq!(
            effective_extensions(Some(&[]), false),
            Vec::<String>::new(),
            "an explicit empty list means every file even without render"
        );
    }

    #[test]
    fn matches_extension_is_case_insensitive_and_empty_means_everything() {
        assert!(matches_extension("a/b.MD", &["md".to_string()]));
        assert!(!matches_extension("a/b.txt", &["md".to_string()]));
        assert!(matches_extension("a/b.txt", &[]));
        assert!(!matches_extension("a/b", &["md".to_string()]));
    }

    #[test]
    fn glob_include_defaults_to_markdown_only() {
        let (_dir, co) = checkout(&[("docs/a.md", "# A\n"), ("docs/a.json", "{}\n")]);
        let source = glob_source("      type: glob\n      include: ['docs/*']\n");
        let result = resolve(&source, &co, &[], &[], false);
        assert_eq!(result.pages.keys().collect::<Vec<_>>(), vec!["docs/a.md"]);
        assert_eq!(
            result
                .residue
                .iter()
                .map(|r| r.path.as_str())
                .collect::<Vec<_>>(),
            vec!["docs/a.json"],
            "non-markdown files still count against the (unfiltered) residue scope"
        );
        assert_eq!(
            result.residue[0].rule.as_ref().unwrap().key,
            "glob:extension",
            "matches include, but not the (default) md extension"
        );
        assert!(result.residue[0].rule.as_ref().unwrap().text.contains("md"));
    }

    #[test]
    fn glob_extensions_explicit_list_overrides_the_default() {
        let (_dir, co) = checkout(&[("docs/a.md", "# A\n"), ("docs/a.yaml", "key: value\n")]);

        let source = glob_source(
            "      type: glob\n      include: ['docs/*']\n      extensions: ['yaml']\n",
        );
        let result = resolve(&source, &co, &[], &[], false);
        assert_eq!(result.pages.keys().collect::<Vec<_>>(), vec!["docs/a.yaml"]);

        let source =
            glob_source("      type: glob\n      include: ['docs/*']\n      extensions: []\n");
        let result = resolve(&source, &co, &[], &[], false);
        assert_eq!(result.pages.len(), 2, "an empty list means every file");
    }

    #[test]
    fn glob_extensions_default_to_every_file_when_the_source_has_a_render_step() {
        let (_dir, co) = checkout(&[
            ("crds/widget.yaml", "kind: Other\n"),
            ("README.md", "# Readme\n"),
        ]);

        let plain = glob_source("      type: glob\n      include: ['**/*']\n");
        let result = resolve(&plain, &co, &[], &[], false);
        assert_eq!(
            result.pages.keys().collect::<Vec<_>>(),
            vec!["README.md"],
            "without a render step, only markdown is selected by default"
        );

        let rendered = glob_source_with_render(
            "      type: glob\n      include: ['**/*']\n",
            "      type: openapi\n",
        );
        let result = resolve(&rendered, &co, &[], &[], false);
        assert_eq!(
            result.pages.keys().collect::<Vec<_>>(),
            vec!["README.md", "crds/widget.yaml"],
            "a render step defaults to every file"
        );

        let rendered_explicit = glob_source_with_render(
            "      type: glob\n      include: ['**/*']\n      extensions: ['md']\n",
            "      type: openapi\n",
        );
        let result = resolve(&rendered_explicit, &co, &[], &[], false);
        assert_eq!(
            result.pages.keys().collect::<Vec<_>>(),
            vec!["README.md"],
            "an explicit extensions list still wins over the render default"
        );
    }
}
