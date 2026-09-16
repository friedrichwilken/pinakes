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
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::config::{ConfigError, Resolver, Source, compile_globs, compile_regex};
use crate::decisions::{Decision, Verdict};
use crate::manifest::{PageEntry, SelectedBy, page_id};
use crate::residue::{EXCERPT_TOKENS, Reason, ResidueEntry, excerpt};
use crate::sources::{Checkout, SourceError, list_files};

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
        }
    }
}

/// Parse resolver stdout: one JSON object per non-blank line.
pub fn parse_candidates(text: &str) -> Result<Vec<Candidate>, (usize, String)> {
    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let candidate: Candidate =
            serde_json::from_str(line).map_err(|e| (index + 1, e.to_string()))?;
        if candidate.path.trim().is_empty() {
            return Err((index + 1, "path must not be empty".to_string()));
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

/// Make `arg` absolute when it names an existing path relative to `base`.
pub(crate) fn absolutise(base: &Path, arg: &str) -> String {
    let candidate = base.join(arg);
    if !Path::new(arg).is_absolute() && arg.contains(['/', '\\']) && candidate.exists() {
        std::path::absolute(&candidate)
            .unwrap_or(candidate)
            .to_string_lossy()
            .into_owned()
    } else {
        arg.to_string()
    }
}

/// Read a file from `checkout` as lossy UTF-8; used by the navigation-based resolvers to read
/// their navigation file.
fn read_file(checkout: &Checkout, path: &str) -> Result<String, ResolveError> {
    let full = checkout.root.join(path);
    let bytes = std::fs::read(&full).map_err(|source| ResolveError::Io { path: full, source })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// The text after a leading YAML frontmatter block, if any.
pub fn strip_frontmatter(content: &str) -> &str {
    let Some(rest) = content.strip_prefix("---") else {
        return content;
    };
    if !rest.starts_with('\n') && !rest.starts_with("\r\n") {
        return content;
    }
    let mut offset = 3;
    for line in rest.split_inclusive('\n') {
        offset += line.len();
        if line.trim_end() == "---" && offset > 4 {
            return &content[offset..];
        }
    }
    content
}

/// The `title:` value of a leading frontmatter block.
pub fn frontmatter_title(content: &str) -> Option<String> {
    let rest = content.strip_prefix("---")?;
    let body_start = strip_frontmatter(content);
    if std::ptr::eq(body_start, content) {
        return None;
    }
    let block = &rest[..rest.len() - body_start.len()];
    block.lines().find_map(|line| {
        let value = line.strip_prefix("title:")?.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        (!value.is_empty()).then(|| value.to_string())
    })
}

/// The first ATX H1 (`# Title`) outside frontmatter and fenced code blocks.
pub fn first_h1(content: &str) -> Option<String> {
    let mut in_fence = false;
    for line in strip_frontmatter(content).lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(title) = trimmed.strip_prefix("# ") {
            let title = title.trim().trim_end_matches('#').trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }
    None
}

/// Title fallback chain: `nav_title`, then the first H1, then frontmatter `title:`, else empty.
pub fn title_of(nav_title: &str, content: &str) -> String {
    if !nav_title.trim().is_empty() {
        return nav_title.trim().to_string();
    }
    first_h1(content)
        .or_else(|| frontmatter_title(content))
        .unwrap_or_default()
}

/// Outcome of the precedence rules for one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// `policy.deny` matched: the file is neither a page nor residue.
    Denied,
    /// `resolver.exclude` matched: the file is neither a page nor residue.
    Excluded,
    /// The file is a page, selected by the given mechanism.
    Selected(SelectedBy),
    /// The file is residue.
    Residue,
}

/// Apply `policy.deny` > `resolver.exclude` > decisions > resolver selection.
///
/// `decision` must already be checked against the file hash (an expired decision is `None`);
/// `resolver_selection` is `Some` when the resolver selected the file.
pub fn precedence(
    path: &str,
    deny: &GlobSet,
    exclude: &GlobSet,
    decision: Option<&Decision>,
    resolver_selection: Option<SelectedBy>,
) -> Outcome {
    if deny.is_match(path) {
        return Outcome::Denied;
    }
    if exclude.is_match(path) {
        return Outcome::Excluded;
    }
    match decision.map(|d| d.decision) {
        Some(Verdict::Include) => Outcome::Selected(SelectedBy::Decision),
        Some(Verdict::Exclude) => Outcome::Residue,
        Some(Verdict::Unsure) | None => match resolver_selection {
            Some(by) => Outcome::Selected(by),
            None => Outcome::Residue,
        },
    }
}

/// Inputs shared by every source in a resolve run.
pub struct ResolveContext<'a> {
    /// Compiled `policy.deny`.
    pub deny: &'a GlobSet,
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
/// `exclude` set, the residue `scope`, an external resolver's `residue_mention` and the
/// resolver's own optional `include` extra (SPEC §2.1), not yet applied to `candidates`.
type ResolverPlan<'a> = (
    BTreeMap<String, Candidate>,
    GlobSet,
    GlobSet,
    Option<Regex>,
    &'a [String],
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

fn resolver_plan<'a>(
    source: &'a Source,
    checkout: &Checkout,
    files: &[String],
    config_dir: &Path,
) -> Result<ResolverPlan<'a>, ResolveError> {
    let mut candidates: BTreeMap<String, Candidate> = BTreeMap::new();
    let (exclude, scope, mention, extra_include): (GlobSet, GlobSet, Option<Regex>, &[String]) =
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
                (exclude, scope, None, &[])
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
                )
            }
            Resolver::Vitepress {
                path,
                scope,
                include,
                exclude,
            } => {
                let (found, nav_scope) =
                    vitepress::plan(source, checkout, files, path.as_deref(), scope)?;
                candidates = found;
                (compile_globs("exclude", exclude)?, nav_scope, None, include)
            }
            Resolver::Docusaurus {
                path,
                scope,
                include,
                exclude,
            } => {
                let (found, nav_scope) =
                    docusaurus::plan(source, checkout, files, path.as_deref(), scope)?;
                candidates = found;
                (compile_globs("exclude", exclude)?, nav_scope, None, include)
            }
            Resolver::Mdbook {
                path,
                scope,
                include,
                exclude,
            } => {
                let (found, nav_scope) =
                    mdbook::plan(source, checkout, files, path.as_deref(), scope)?;
                candidates = found;
                (compile_globs("exclude", exclude)?, nav_scope, None, include)
            }
            Resolver::Sitemap {
                path,
                scope,
                url_prefix,
                path_prefix,
                include,
                exclude,
            } => {
                let (found, nav_scope) = sitemap::plan(
                    source,
                    checkout,
                    files,
                    path.as_deref(),
                    scope,
                    url_prefix,
                    path_prefix,
                )?;
                candidates = found;
                (compile_globs("exclude", exclude)?, nav_scope, None, include)
            }
        };
    Ok((candidates, exclude, scope, mention, extra_include))
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
    let (mut candidates, exclude, scope, mention, extra_include) =
        resolver_plan(source, checkout, files, config_dir)?;
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
    })
}

/// Resolve one source from its checkout.
pub fn resolve_source(
    source: &Source,
    checkout: &Checkout,
    ctx: &ResolveContext<'_>,
) -> Result<ResolvedSource, ResolveError> {
    let files = list_files(&checkout.root)?;
    let mut plan = plan(source, checkout, &files, ctx.config_dir)?;
    let mut result = ResolvedSource::default();

    for path in &plan.unresolved {
        let candidate = plan
            .candidates
            .remove(path)
            .unwrap_or_else(|| Candidate::bare(path, false));
        result.residue.push(ResidueEntry {
            id: page_id(&source.name, path),
            source: source.name.clone(),
            path: path.clone(),
            reason: Reason::UnresolvedLink,
            sha256: String::new(),
            title: candidate.title,
            excerpt: String::new(),
            context: context_of(&candidate.context, &candidate.section),
        });
    }
    result.unresolved = std::mem::take(&mut plan.unresolved);

    let prefix = format!("{}::", source.name);
    for file in &files {
        let in_scope = plan.scope.is_match(file);
        let decided = ctx.decisions.contains_key(&format!("{prefix}{file}"));
        if !plan.candidates.contains_key(file) && (in_scope || decided) {
            plan.candidates
                .insert(file.clone(), Candidate::bare(file, false));
        }
    }

    let resolver_kind = match source.resolver {
        Resolver::Glob { .. } => SelectedBy::Include,
        Resolver::External { .. }
        | Resolver::Vitepress { .. }
        | Resolver::Docusaurus { .. }
        | Resolver::Mdbook { .. }
        | Resolver::Sitemap { .. } => SelectedBy::Resolver,
    };
    let reason = if ctx.is_new_source {
        Reason::NewSource
    } else {
        Reason::NotSelected
    };
    for (path, candidate) in &plan.candidates {
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
            Outcome::Denied | Outcome::Excluded => {}
            Outcome::Selected(selected_by) => {
                result.pages.insert(
                    path.clone(),
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
            Outcome::Residue => {
                if plan.mention.as_ref().is_some_and(|re| !re.is_match(&text)) {
                    continue;
                }
                result.residue.push(ResidueEntry {
                    id,
                    source: source.name.clone(),
                    path: path.clone(),
                    reason,
                    sha256,
                    title: title_of(&candidate.title, &text),
                    excerpt: excerpt(strip_frontmatter(&text), EXCERPT_TOKENS),
                    context: context_of(&candidate.context, &candidate.section),
                });
            }
        }
    }
    result
        .residue
        .sort_by(|a, b| (a.reason, &a.path).cmp(&(b.reason, &b.path)));
    Ok(result)
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
        let deny: Vec<String> = deny.iter().map(|s| (*s).to_string()).collect();
        let deny = compile_globs("deny", &deny).unwrap();
        let decisions = crate::decisions::effective(decisions);
        let ctx = ResolveContext {
            deny: &deny,
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
        assert!(
            result.residue.is_empty(),
            "excluded and denied files are not residue"
        );
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

        let result = resolve(&source, &co, &[], &[], true);
        assert_eq!(result.residue[0].reason, Reason::NewSource);
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
            ["docs/a.md"],
            "decision-excluded page stays residue"
        );
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
            Outcome::Residue
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
            Outcome::Residue
        );
    }

    #[test]
    fn title_fallback_chain() {
        assert_eq!(title_of("Nav", "# H1\n"), "Nav");
        assert_eq!(title_of("", "---\ntitle: FM\n---\n\n# H1 #\n"), "H1");
        assert_eq!(
            title_of("", "---\ntitle: \"Quoted FM\"\n---\nbody\n"),
            "Quoted FM"
        );
        assert_eq!(title_of("", "```\n# not a title\n```\n\n## only h2\n"), "");
        assert_eq!(title_of("", "--- not frontmatter\ntitle: x\n"), "");
        assert_eq!(strip_frontmatter("---\na: 1\n---\nbody"), "body");
        assert_eq!(
            strip_frontmatter("---\nunterminated\n"),
            "---\nunterminated\n"
        );
        assert_eq!(frontmatter_title("---\n---\n# x"), None);
        assert_eq!(first_h1("#nospace\n# Real\n"), Some("Real".to_string()));
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
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

        // `exclude` keeps SUMMARY.md itself out of residue: it sits inside the default residue
        // scope (`src/**/*.md`) but is never linked from itself, which is unrelated to `include`.
        let source = glob_source(
            "      type: mdbook\n      include: ['src/orphan.md']\n      exclude: ['src/SUMMARY.md']\n",
        );
        let result = resolve(&source, &co, &[], &[], false);
        let page = &result.pages["src/orphan.md"];
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
        assert!(
            result.residue.is_empty(),
            "excluded files are never residue either"
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
