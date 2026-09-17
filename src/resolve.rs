//! Candidate discovery: the glob resolver, the external resolver (SPEC §3) and the
//! navigation-based resolvers (`vitepress`, `docusaurus`, `mdbook`, `sitemap`) report candidate
//! pages for the `select` module to apply the selection policy to.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use globset::GlobSet;
use regex::Regex;
use serde::Deserialize;
use thiserror::Error;

use crate::config::{ConfigError, Resolver, Source, compile_globs, compile_regex};
use crate::manifest::SelectedBy;
use crate::sources::{Checkout, SourceError};
pub use crate::text::{first_h1, frontmatter_title, sha256_hex, strip_frontmatter, title_of};

mod docusaurus;
mod external;
mod glob;
mod mdbook;
mod navigation;
mod sitemap;
mod vitepress;

pub use crate::select::{
    Outcome, ResidueCause, ResolveContext, ResolvedSource, archived_residue, precedence,
    resolve_source,
};
pub use external::{parse_candidates, run_external};
pub(crate) use glob::{effective_extensions, matches_extension};

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

/// What one resolver's own mechanism reports (SPEC §3): the candidate map plus, for the
/// precedence pass, the compiled `exclude` set, the residue `scope`, an external resolver's
/// `residue_mention`, the resolver's own optional `include` extra (SPEC §2.1) not yet applied to
/// `candidates`, and the navigation file path for the four navigation-based resolvers (SPEC §12),
/// used to name the mechanism behind a residue entry (SPEC §2.4).
pub(crate) struct Discovery<'a> {
    pub(crate) candidates: BTreeMap<String, Candidate>,
    pub(crate) exclude: GlobSet,
    pub(crate) scope: GlobSet,
    pub(crate) mention: Option<Regex>,
    pub(crate) extra_include: &'a [String],
    pub(crate) nav_path: Option<String>,
}

impl<'a> Discovery<'a> {
    /// The common shape of every navigation-based resolver arm of [`resolver_plan`]: take the
    /// candidate map and residue scope a `vitepress`, `docusaurus`, `mdbook` or `sitemap` plan
    /// produced, record the navigation file path, and compile `exclude`.
    fn navigation(
        found: BTreeMap<String, Candidate>,
        scope: GlobSet,
        nav_path: String,
        exclude: &[String],
        include: &'a [String],
    ) -> Result<Discovery<'a>, ResolveError> {
        Ok(Discovery {
            candidates: found,
            exclude: compile_globs("exclude", exclude)?,
            scope,
            mention: None,
            extra_include: include,
            nav_path: Some(nav_path),
        })
    }
}

fn resolver_plan<'a>(
    source: &'a Source,
    checkout: &Checkout,
    files: &[String],
    config_dir: &Path,
) -> Result<Discovery<'a>, ResolveError> {
    match &source.resolver {
        Resolver::Glob {
            include,
            exclude,
            residue_scope,
            extensions,
        } => glob::glob_plan(
            source,
            files,
            include,
            exclude,
            residue_scope,
            extensions.as_deref(),
        ),
        Resolver::External {
            command,
            args,
            residue_mention,
            residue_scope,
            include,
            exclude,
        } => {
            let mut candidates = BTreeMap::new();
            for candidate in external::run_external(source, command, args, checkout, config_dir)? {
                candidates.insert(candidate.path.clone(), candidate);
            }
            let mention = residue_mention
                .as_deref()
                .map(|p| compile_regex("residue_mention", p))
                .transpose()?;
            Ok(Discovery {
                candidates,
                exclude: compile_globs("exclude", exclude)?,
                scope: compile_globs("residue_scope", residue_scope)?,
                mention,
                extra_include: include,
                nav_path: None,
            })
        }
        Resolver::Vitepress {
            path,
            scope,
            include,
            exclude,
        } => {
            let (found, nav_scope, nav_path) =
                vitepress::plan(source, checkout, files, path.as_deref(), scope)?;
            Discovery::navigation(found, nav_scope, nav_path, exclude, include)
        }
        Resolver::Docusaurus {
            path,
            scope,
            include,
            exclude,
        } => {
            let (found, nav_scope, nav_path) =
                docusaurus::plan(source, checkout, files, path.as_deref(), scope)?;
            Discovery::navigation(found, nav_scope, nav_path, exclude, include)
        }
        Resolver::Mdbook {
            path,
            scope,
            include,
            exclude,
        } => {
            let (found, nav_scope, nav_path) =
                mdbook::plan(source, checkout, files, path.as_deref(), scope)?;
            Discovery::navigation(found, nav_scope, nav_path, exclude, include)
        }
        Resolver::Sitemap {
            path,
            scope,
            url_prefix,
            path_prefix,
            include,
            exclude,
        } => {
            let (found, nav_scope, nav_path) = sitemap::plan(
                source,
                checkout,
                files,
                path.as_deref(),
                scope,
                url_prefix,
                path_prefix,
            )?;
            Discovery::navigation(found, nav_scope, nav_path, exclude, include)
        }
    }
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
    let Discovery {
        mut candidates,
        exclude,
        scope,
        mention,
        extra_include,
        nav_path,
    } = resolver_plan(source, checkout, files, config_dir)?;
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
