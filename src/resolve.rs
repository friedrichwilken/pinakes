//! Candidate discovery: the glob resolver, the external resolver (SPEC §3) and the
//! navigation-based resolvers (`vitepress`, `docusaurus`, `mdbook`, `sitemap`) report candidate
//! pages for the `select` module to apply the selection policy to.
//!
//! Every resolver kind implements the crate-private `Resolver` trait. `resolver_for` is the only
//! place that matches on [`config::Resolver`] in this module or `select`; adding a resolver kind
//! means one new file implementing `Resolver` plus one arm there (and, separately, the new
//! `config::Resolver` variant itself).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use globset::GlobSet;
use regex::Regex;
use serde::Deserialize;
use thiserror::Error;

use crate::config::{self, ConfigError, Source, compile_globs};
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

/// The mechanism behind a residue entry (SPEC §2.4): a short, stable key plus a one-sentence
/// explanation for a human. This is `resolve`'s own name for the shape; `select` (which depends
/// on both `resolve` and the residue module) turns it into the residue module's `Rule` at the
/// point it builds the residue entry, since `resolve` itself does not depend on it.
///
/// `serde(rename = "Rule")` and `serde(expecting = "struct Rule")` together keep this struct's
/// deserialisation error text (e.g. "expected struct Rule") identical to what it was when the
/// external resolver's self-reported `rule` field (SPEC §3) deserialised directly into the
/// residue module's `Rule`; `rename` alone is not enough, since serde's derived `expecting()`
/// text for a JSON "invalid type" error is built from the Rust struct name, not the serde name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename = "Rule", expecting = "struct Rule")]
pub struct Mechanism {
    /// A short, stable identifier, e.g. `"policy:deny"`.
    pub key: String,
    /// One sentence explaining the decision.
    pub text: String,
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
    /// pinakes assigns a default "not selected" or "unmatched" mechanism instead (see the
    /// `external` resolver's `not_selected`). Ignored for a selected candidate.
    #[serde(default)]
    pub rule: Option<Mechanism>,
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
pub(crate) struct Plan<'a> {
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
    /// The resolver mechanism itself, for `select` to ask for its `selected_by`, its `exclude`
    /// patterns and, for a candidate it did not select, the [`Mechanism`] behind that (SPEC
    /// §2.4).
    pub(crate) resolver: Box<dyn Resolver + 'a>,
}

/// What one resolver's own mechanism reports (SPEC §3): the candidate map, the compiled
/// `exclude` set, the residue `scope`, an external resolver's `residue_mention`, and the
/// navigation file path for the four navigation-based resolvers (SPEC §12), used to name the
/// mechanism behind a residue entry (SPEC §2.4).
pub(crate) struct Discovery {
    pub(crate) candidates: BTreeMap<String, Candidate>,
    pub(crate) exclude: GlobSet,
    pub(crate) scope: GlobSet,
    pub(crate) mention: Option<Regex>,
    pub(crate) nav_path: Option<String>,
}

impl Discovery {
    /// The common shape of every navigation-based resolver's [`Resolver::discover`]: take the
    /// candidate map and residue scope a `vitepress`, `docusaurus`, `mdbook` or `sitemap` plan
    /// produced, record the navigation file path, and compile `exclude`.
    fn navigation(
        found: BTreeMap<String, Candidate>,
        scope: GlobSet,
        nav_path: String,
        exclude: &[String],
    ) -> Result<Discovery, ResolveError> {
        Ok(Discovery {
            candidates: found,
            exclude: compile_globs("exclude", exclude)?,
            scope,
            mention: None,
            nav_path: Some(nav_path),
        })
    }
}

/// A resolver kind's own discovery-and-not-selected mechanism (SPEC §3, §12). One implementation
/// per `config::Resolver` variant, constructed by [`resolver_for`]; to add a resolver kind,
/// implement this trait in a new `resolve/*.rs` file and add its arm there.
pub(crate) trait Resolver {
    /// What a page this resolver selects counts as (SPEC §2.2's `selected_by`).
    fn selected_by(&self) -> SelectedBy {
        SelectedBy::Resolver
    }
    /// This resolver's own `exclude` glob patterns (SPEC §2.1), raw; `select` compiles them to
    /// match, and names the one that matched, for a residue entry excluded this way.
    fn exclude(&self) -> &[String];
    /// This resolver's optional `include` extra (SPEC §2.1); empty when it has none.
    fn extra_include(&self) -> &[String] {
        &[]
    }
    /// Run this resolver's discovery mechanism.
    fn discover(
        &self,
        source: &Source,
        checkout: &Checkout,
        files: &[String],
        config_dir: &Path,
    ) -> Result<Discovery, ResolveError>;
    /// The mechanism (SPEC §2.4) for a candidate this resolver's own mechanism did not select.
    fn not_selected(&self, path: &str, candidate: &Candidate, plan: &Plan<'_>) -> Mechanism;
}

/// Build the [`Resolver`] implementation for `source`'s resolver kind: the only `match` on
/// [`config::Resolver`] in `resolve` or `select`.
pub(crate) fn resolver_for(source: &Source) -> Box<dyn Resolver + '_> {
    match &source.resolver {
        config::Resolver::Glob {
            include,
            exclude,
            residue_scope,
            extensions,
        } => Box::new(glob::Glob {
            include,
            exclude,
            residue_scope,
            extensions: glob::effective_extensions(extensions.as_deref(), source.render.is_some()),
        }),
        config::Resolver::External {
            command,
            args,
            residue_mention,
            residue_scope,
            include,
            exclude,
        } => Box::new(external::External {
            command,
            args,
            residue_mention: residue_mention.as_deref(),
            residue_scope,
            include,
            exclude,
        }),
        config::Resolver::Vitepress {
            path,
            scope,
            include,
            exclude,
        } => Box::new(vitepress::Vitepress {
            path: path.as_deref(),
            scope,
            include,
            exclude,
        }),
        config::Resolver::Docusaurus {
            path,
            scope,
            include,
            exclude,
        } => Box::new(docusaurus::Docusaurus {
            path: path.as_deref(),
            scope,
            include,
            exclude,
        }),
        config::Resolver::Mdbook {
            path,
            scope,
            include,
            exclude,
        } => Box::new(mdbook::Mdbook {
            path: path.as_deref(),
            scope,
            include,
            exclude,
        }),
        config::Resolver::Sitemap {
            path,
            url_prefix,
            path_prefix,
            scope,
            include,
            exclude,
        } => Box::new(sitemap::Sitemap {
            path: path.as_deref(),
            url_prefix,
            path_prefix,
            scope,
            include,
            exclude,
        }),
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

pub(crate) fn plan<'a>(
    source: &'a Source,
    checkout: &Checkout,
    files: &[String],
    config_dir: &Path,
) -> Result<Plan<'a>, ResolveError> {
    let file_set: BTreeSet<&str> = files.iter().map(String::as_str).collect();
    let resolver = resolver_for(source);
    let Discovery {
        mut candidates,
        exclude,
        scope,
        mention,
        nav_path,
    } = resolver.discover(source, checkout, files, config_dir)?;
    let mentioned: BTreeSet<String> = candidates.keys().cloned().collect();
    let include_selected = apply_extra_include(&mut candidates, files, resolver.extra_include())?;
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
        resolver,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // These two strings are what `origin/main` (before `Mechanism` existed, when
    // `Candidate.rule` deserialised straight into the residue module's `Rule`) produces for the
    // same malformed `rule` field on an external resolver's self-reported candidate line (SPEC
    // §3); pinned here so `Mechanism`'s serde attributes keep deserialising a malformed `rule`
    // byte-identical.
    #[test]
    fn malformed_candidate_rule_error_text_is_unchanged_by_the_rename() {
        let not_an_object = r#"{"path":"a.md","rule":"nope"}"#;
        let err = serde_json::from_str::<Candidate>(not_an_object).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid type: string \"nope\", expected struct Rule at line 1 column 28"
        );

        let missing_text = r#"{"path":"a.md","rule":{"key":"x"}}"#;
        let err = serde_json::from_str::<Candidate>(missing_text).unwrap_err();
        assert_eq!(err.to_string(), "missing field `text` at line 1 column 33");

        // An external resolver's self-reported rule (SPEC §3) is used verbatim, with an unknown
        // extra field silently ignored, as it always has been (no `deny_unknown_fields`).
        let extra_field = r#"{"path":"a.md","rule":{"key":"x","text":"y","extra":1}}"#;
        let candidate = serde_json::from_str::<Candidate>(extra_field).unwrap();
        assert_eq!(
            candidate.rule,
            Some(Mechanism {
                key: "x".to_string(),
                text: "y".to_string(),
            })
        );
    }
}
