//! Candidate discovery: the glob resolver, the external resolver (SPEC §3) and the
//! navigation-based resolvers (`vitepress`, `docusaurus`, `mdbook`, `sitemap`) report candidate
//! pages for the `select` module to apply the selection policy to.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use globset::GlobSet;
use regex::Regex;
use serde::Deserialize;
use thiserror::Error;

use crate::config::{ConfigError, Resolver, Source, compile_globs, compile_regex};
use crate::jsonl;
use crate::manifest::SelectedBy;
use crate::sources::{Checkout, SourceError};
use crate::text::absolutise;
pub use crate::text::{first_h1, frontmatter_title, sha256_hex, strip_frontmatter, title_of};

mod docusaurus;
mod mdbook;
mod navigation;
mod sitemap;
mod vitepress;

pub use crate::select::{
    Outcome, ResidueCause, ResolveContext, ResolvedSource, archived_residue, precedence,
    resolve_source,
};

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

/// Resolver-specific inputs collected before the precedence pass.
pub(crate) struct Plan {
    pub(crate) candidates: BTreeMap<String, Candidate>,
    pub(crate) exclude: GlobSet,
    pub(crate) scope: GlobSet,
    pub(crate) mention: Option<Regex>,
    pub(crate) unresolved: Vec<String>,
    /// Paths added by a resolver's optional `include` extra (SPEC §2.1), rather than by the
    /// resolver's own selection mechanism; these are always `selected_by: "include"`.
    pub(crate) include_selected: BTreeSet<String>,
    /// The navigation file a `vitepress`, `docusaurus`, `mdbook` or `sitemap` resolver read
    /// (SPEC §12); `None` for `glob` and `external`, which have no such file.
    pub(crate) nav_path: Option<String>,
    /// Paths the resolver's own mechanism produced a candidate for (selected or not), before
    /// the `include` extra and the scope-fill pass added anything else; used by the `external`
    /// resolver's rule (SPEC §2.4) to tell a file its own output named from one it never
    /// mentioned at all.
    pub(crate) mentioned: BTreeSet<String>,
}

/// The `glob` resolver's effective `extensions` (SPEC §2.1): `configured` verbatim when given,
/// else `["md"]`, or every file (an empty list) when the source has a `render` step.
pub(crate) fn effective_extensions(configured: Option<&[String]>, has_render: bool) -> Vec<String> {
    match configured {
        Some(extensions) => extensions.to_vec(),
        None if has_render => Vec::new(),
        None => vec!["md".to_string()],
    }
}

/// Whether `path`'s extension (case-insensitive, without the dot) is in `extensions`; an empty
/// list matches every file.
pub(crate) fn matches_extension(path: &str, extensions: &[String]) -> bool {
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

pub(crate) fn plan(
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

/// What a resolver's own selection mechanism counts as (SPEC §2.2's `selected_by`): a plain
/// `glob` include is `"include"`, everything else (including a resolver's own `include` extra,
/// handled separately) is `"resolver"`.
pub(crate) fn resolver_kind_of(resolver: &Resolver) -> SelectedBy {
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
pub(crate) fn resolver_exclude(resolver: &Resolver) -> &[String] {
    match resolver {
        Resolver::Glob { exclude, .. }
        | Resolver::External { exclude, .. }
        | Resolver::Vitepress { exclude, .. }
        | Resolver::Docusaurus { exclude, .. }
        | Resolver::Mdbook { exclude, .. }
        | Resolver::Sitemap { exclude, .. } => exclude,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
