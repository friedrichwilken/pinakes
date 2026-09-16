//! `pinakes.yaml`: the human-written source configuration (SPEC §2.1).
//!
//! Parsing is strict: unknown keys, malformed source names, non-GitHub repositories, invalid
//! globs and invalid regexes are all rejected with a [`ConfigError`] so that mistakes surface
//! before any network access happens.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The only configuration version understood by this iteration.
pub const CONFIG_VERSION: u32 = 1;

/// Errors raised while reading or validating `pinakes.yaml`.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("cannot read config {path}: {source}")]
    Io {
        /// Path that failed to open.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The YAML did not parse or did not match the expected shape.
    #[error("invalid config: {0}")]
    Yaml(#[from] serde_yaml_ng::Error),
    /// The `version` field is not supported.
    #[error("unsupported config version {0}; expected {CONFIG_VERSION}")]
    Version(u32),
    /// The config declares no sources.
    #[error("config declares no sources")]
    NoSources,
    /// A source name does not match `[A-Za-z0-9_-]+`.
    #[error("invalid source name {0:?}: expected [A-Za-z0-9_-]+")]
    SourceName(String),
    /// Two sources share a name.
    #[error("duplicate source name {0:?}")]
    DuplicateSource(String),
    /// A source `repo` is not a `https://github.com/<owner>/<repo>[.git]` URL.
    #[error("source {name}: repo {repo:?} is not a github.com repository URL")]
    RepoUrl {
        /// Source whose URL is invalid.
        name: String,
        /// The offending URL.
        repo: String,
    },
    /// A source `ref` is empty.
    #[error("source {0}: ref must not be empty")]
    EmptyRef(String),
    /// A glob pattern does not compile.
    #[error("{context}: invalid glob {pattern:?}: {source}")]
    Glob {
        /// Where the pattern came from, e.g. `policy.deny`.
        context: String,
        /// The offending pattern.
        pattern: String,
        /// Underlying globset error.
        #[source]
        source: globset::Error,
    },
    /// A regular expression does not compile.
    #[error("{context}: invalid regex {pattern:?}: {source}")]
    Regex {
        /// Where the pattern came from.
        context: String,
        /// The offending pattern.
        pattern: String,
        /// Underlying regex error.
        #[source]
        source: regex::Error,
    },
    /// An external resolver has an empty command.
    #[error("source {0}: external resolver command must not be empty")]
    EmptyCommand(String),
    /// A glob resolver has no include patterns.
    #[error("source {0}: glob resolver needs at least one include pattern")]
    NoInclude(String),
    /// A `sitemap` resolver has an empty `url_prefix`.
    #[error("source {0}: sitemap resolver needs a non-empty url_prefix")]
    EmptyUrlPrefix(String),
    /// A `sitemap` resolver has an empty `path_prefix`.
    #[error("source {0}: sitemap resolver needs a non-empty path_prefix")]
    EmptyPathPrefix(String),
    /// An external render command is empty.
    #[error("source {0}: external render command must not be empty")]
    EmptyRenderCommand(String),
}

/// The parsed and validated configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Config format version; must equal [`CONFIG_VERSION`].
    pub version: u32,
    /// Documentation sources, in declaration order.
    pub sources: Vec<Source>,
    /// Corpus-wide policy.
    #[serde(default)]
    pub policy: Policy,
    /// Evaluation settings (used by `eval`, part 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eval: Option<EvalConfig>,
}

/// One documentation source: a GitHub repository at a ref, with a resolver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// Directory name in the artifact; `[A-Za-z0-9_-]+`.
    pub name: String,
    /// Repository URL; GitHub only in v1.
    pub repo: String,
    /// Branch, tag or SHA; resolved to a SHA at resolve time.
    #[serde(rename = "ref")]
    pub git_ref: String,
    /// Higher wins when two sources carry the same title (mirrors); default
    /// [`DEFAULT_PRIORITY`], so sources that do not set it never collapse each other.
    #[serde(default = "default_priority")]
    pub priority: i64,
    /// How pages are selected from the checkout.
    pub resolver: Resolver,
    /// How selected pages become Markdown, when they are not already Markdown (SPEC §10.1);
    /// absent for sources whose selected files are already Markdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render: Option<Render>,
}

/// Priority of a source that does not set one.
pub const DEFAULT_PRIORITY: i64 = 1;

fn default_priority() -> i64 {
    DEFAULT_PRIORITY
}

/// Page selection strategy for a source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Resolver {
    /// Select files by glob patterns.
    Glob {
        /// Files matching any of these are selected.
        include: Vec<String>,
        /// Files matching any of these are never selected.
        #[serde(default)]
        exclude: Vec<String>,
        /// Files matching these but not selected are residue; defaults to `include`.
        #[serde(default)]
        residue_scope: Vec<String>,
        /// Case-insensitive extensions (without the dot) `include` matches are restricted to.
        /// `None` (not given in config) defaults to `["md"]`, or to every file when the source
        /// has a `render` step (SPEC §10.1); `Some(vec![])` explicitly means every file.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extensions: Option<Vec<String>>,
    },
    /// Run an external command that emits the SPEC §3 JSONL contract.
    External {
        /// Program and leading arguments.
        command: Vec<String>,
        /// Trailing arguments.
        #[serde(default)]
        args: Vec<String>,
        /// Only leftovers whose text matches this regex are reported.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        residue_mention: Option<String>,
        /// Files under these globs that the command never mentions are residue.
        #[serde(default)]
        residue_scope: Vec<String>,
        /// Files matching these globs are selected in addition to what the command selects
        /// (SPEC §3), with `selected_by: "include"`; never residue.
        #[serde(default)]
        include: Vec<String>,
        /// Files matching these globs are never selected, even when the command selects them.
        #[serde(default)]
        exclude: Vec<String>,
    },
    /// Select the pages a `VitePress` sidebar links (SPEC §12): a tolerant scan of a full
    /// `.vitepress/config.*` or a standalone sidebar file such as `_sidebar.ts`.
    Vitepress {
        /// Navigation file, relative to the repository root; defaults to
        /// `docs/.vitepress/config.*`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// Globs used to compute residue; defaults to the navigation file's content
        /// directory, `**/*.md`.
        #[serde(default)]
        scope: Vec<String>,
        /// Files matching these globs are selected in addition to what the sidebar links
        /// (SPEC §12), with `selected_by: "include"`; never residue.
        #[serde(default)]
        include: Vec<String>,
        /// Files matching these globs are never selected, even when the sidebar links them.
        #[serde(default)]
        exclude: Vec<String>,
    },
    /// Select the pages a Docusaurus sidebar links (SPEC §12): `sidebars.js` or `sidebars.ts`.
    Docusaurus {
        /// Navigation file, relative to the repository root; defaults to `sidebars.js` or
        /// `sidebars.ts`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// Globs used to compute residue; defaults to `docs/**/*.md`.
        #[serde(default)]
        scope: Vec<String>,
        /// Files matching these globs are selected in addition to what the sidebar links
        /// (SPEC §12), with `selected_by: "include"`; never residue.
        #[serde(default)]
        include: Vec<String>,
        /// Files matching these globs are never selected, even when the sidebar links them.
        #[serde(default)]
        exclude: Vec<String>,
    },
    /// Select the pages an mdBook `SUMMARY.md` links (SPEC §12).
    Mdbook {
        /// Navigation file, relative to the repository root; defaults to `src/SUMMARY.md`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// Globs used to compute residue; defaults to the navigation file's directory,
        /// `**/*.md`.
        #[serde(default)]
        scope: Vec<String>,
        /// Files matching these globs are selected in addition to what `SUMMARY.md` links
        /// (SPEC §12), with `selected_by: "include"`; never residue.
        #[serde(default)]
        include: Vec<String>,
        /// Files matching these globs are never selected, even when `SUMMARY.md` links them.
        #[serde(default)]
        exclude: Vec<String>,
    },
    /// Select the pages a sitemap links (SPEC §12): `sitemap.xml`, or a plain URL list file,
    /// mapped to repository paths via `url_prefix` → `path_prefix`.
    Sitemap {
        /// Navigation file, relative to the repository root; defaults to `sitemap.xml`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// URL prefix stripped from each sitemap entry, e.g. `https://example.com/docs/`.
        url_prefix: String,
        /// Repository path prefix substituted for `url_prefix`, e.g. `docs/`.
        path_prefix: String,
        /// Globs used to compute residue; defaults to `path_prefix` joined with `**/*.md`.
        #[serde(default)]
        scope: Vec<String>,
        /// Files matching these globs are selected in addition to what the sitemap links
        /// (SPEC §12), with `selected_by: "include"`; never residue.
        #[serde(default)]
        include: Vec<String>,
        /// Files matching these globs are never selected, even when the sitemap links them.
        #[serde(default)]
        exclude: Vec<String>,
    },
}

impl Resolver {
    /// Short name recorded in the manifest (`glob`, `external`, `vitepress`, `docusaurus`,
    /// `mdbook` or `sitemap`).
    pub fn kind(&self) -> &'static str {
        match self {
            Resolver::Glob { .. } => "glob",
            Resolver::External { .. } => "external",
            Resolver::Vitepress { .. } => "vitepress",
            Resolver::Docusaurus { .. } => "docusaurus",
            Resolver::Mdbook { .. } => "mdbook",
            Resolver::Sitemap { .. } => "sitemap",
        }
    }
}

/// How a source's selected pages become Markdown pages, when they are not already Markdown
/// (SPEC §10.1). The render step runs after selection and before the artifact is written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Render {
    /// Run an external command that emits the SPEC §10.1 JSONL contract.
    External {
        /// Program and leading arguments; relative paths are resolved against the config file.
        command: Vec<String>,
        /// Trailing arguments.
        #[serde(default)]
        args: Vec<String>,
    },
    /// The built-in CRD / `OpenAPI` schema renderer (SPEC §10.2).
    Openapi,
}

/// What to do with a source whose repository is archived upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArchivedPolicy {
    /// Keep the source and warn.
    #[default]
    Warn,
    /// Drop the source from the corpus.
    Drop,
}

/// Corpus-wide policy (SPEC §2.1 `policy`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Globs that beat everything: matching files are never part of the corpus.
    #[serde(default)]
    pub deny: Vec<String>,
    /// Handling of archived repositories.
    #[serde(default)]
    pub archived: ArchivedPolicy,
    /// `verify` fails when a source has fewer pages than this.
    #[serde(default = "default_min_pages")]
    pub min_pages_per_source: usize,
}

fn default_min_pages() -> usize {
    1
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            deny: Vec::new(),
            archived: ArchivedPolicy::default(),
            min_pages_per_source: 1,
        }
    }
}

/// Evaluation settings (SPEC §2.1 `eval`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalConfig {
    /// Path to `queries.jsonl`, relative to the config file.
    pub queries: PathBuf,
    /// Cut-off for the result list.
    #[serde(default = "default_k")]
    pub k: usize,
    /// `eval --gate` exits 2 when recall drops by more than this.
    #[serde(default)]
    pub max_recall_drop: f64,
    /// `queries check` fails when the held-out share of `queries.jsonl` falls below this.
    #[serde(default = "default_holdout_min")]
    pub holdout_min: f64,
}

fn default_k() -> usize {
    10
}

fn default_holdout_min() -> f64 {
    crate::queries::DEFAULT_HOLDOUT_MIN
}

/// `owner/repo` of a GitHub repository, derived from a source URL.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepoSlug {
    /// Repository owner (user or organisation).
    pub owner: String,
    /// Repository name without the `.git` suffix.
    pub repo: String,
}

impl RepoSlug {
    /// Parse `https://github.com/<owner>/<repo>[.git][/]`; returns `None` for anything else.
    pub fn parse(url: &str) -> Option<RepoSlug> {
        let rest = url.strip_prefix("https://github.com/")?;
        let rest = rest.trim_end_matches('/');
        let rest = rest.strip_suffix(".git").unwrap_or(rest);
        let (owner, repo) = rest.split_once('/')?;
        let ok = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                && s != "."
                && s != ".."
        };
        if ok(owner) && ok(repo) {
            Some(RepoSlug {
                owner: owner.to_string(),
                repo: repo.to_string(),
            })
        } else {
            None
        }
    }

    /// Parse an `owner/repo` slug as recorded in a manifest.
    pub fn from_slug(slug: &str) -> Option<RepoSlug> {
        RepoSlug::parse(&format!("https://github.com/{slug}"))
    }

    /// The canonical clone URL, `https://github.com/<owner>/<repo>.git`.
    pub fn url(&self) -> String {
        format!("https://github.com/{self}.git")
    }

    /// The `blob` base URL for a commit, as recorded in `meta.json`.
    pub fn blob_base_url(&self, commit: &str) -> String {
        format!("https://github.com/{self}/blob/{commit}")
    }
}

impl fmt::Display for RepoSlug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)
    }
}

impl Source {
    /// The `owner/repo` slug of this source; validation guarantees it parses.
    pub fn slug(&self) -> RepoSlug {
        RepoSlug::parse(&self.repo).unwrap_or_else(|| RepoSlug {
            owner: String::new(),
            repo: self.repo.clone(),
        })
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if !valid_source_name(&self.name) {
            return Err(ConfigError::SourceName(self.name.clone()));
        }
        if RepoSlug::parse(&self.repo).is_none() {
            return Err(ConfigError::RepoUrl {
                name: self.name.clone(),
                repo: self.repo.clone(),
            });
        }
        if self.git_ref.trim().is_empty() {
            return Err(ConfigError::EmptyRef(self.name.clone()));
        }
        let ctx = |field: &str| format!("source {}: resolver.{field}", self.name);
        match &self.resolver {
            Resolver::Glob {
                include,
                exclude,
                residue_scope,
                extensions: _,
            } => {
                if include.is_empty() {
                    return Err(ConfigError::NoInclude(self.name.clone()));
                }
                compile_globs(&ctx("include"), include)?;
                compile_globs(&ctx("exclude"), exclude)?;
                compile_globs(&ctx("residue_scope"), residue_scope)?;
            }
            Resolver::External {
                command,
                residue_mention,
                residue_scope,
                include,
                exclude,
                ..
            } => {
                if command.is_empty() || command[0].trim().is_empty() {
                    return Err(ConfigError::EmptyCommand(self.name.clone()));
                }
                if let Some(pattern) = residue_mention {
                    compile_regex(&ctx("residue_mention"), pattern)?;
                }
                compile_globs(&ctx("residue_scope"), residue_scope)?;
                compile_globs(&ctx("include"), include)?;
                compile_globs(&ctx("exclude"), exclude)?;
            }
            Resolver::Vitepress {
                scope,
                include,
                exclude,
                ..
            }
            | Resolver::Docusaurus {
                scope,
                include,
                exclude,
                ..
            }
            | Resolver::Mdbook {
                scope,
                include,
                exclude,
                ..
            } => {
                compile_globs(&ctx("scope"), scope)?;
                compile_globs(&ctx("include"), include)?;
                compile_globs(&ctx("exclude"), exclude)?;
            }
            Resolver::Sitemap {
                scope,
                url_prefix,
                path_prefix,
                include,
                exclude,
                ..
            } => {
                compile_globs(&ctx("scope"), scope)?;
                compile_globs(&ctx("include"), include)?;
                compile_globs(&ctx("exclude"), exclude)?;
                if url_prefix.trim().is_empty() {
                    return Err(ConfigError::EmptyUrlPrefix(self.name.clone()));
                }
                if path_prefix.trim().is_empty() {
                    return Err(ConfigError::EmptyPathPrefix(self.name.clone()));
                }
            }
        }
        if let Some(Render::External { command, .. }) = &self.render
            && (command.is_empty() || command[0].trim().is_empty())
        {
            return Err(ConfigError::EmptyRenderCommand(self.name.clone()));
        }
        Ok(())
    }
}

/// Compile a list of glob patterns into a [`GlobSet`]; `context` labels errors.
pub fn compile_globs(context: &str, patterns: &[String]) -> Result<GlobSet, ConfigError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern).map_err(|source| ConfigError::Glob {
            context: context.to_string(),
            pattern: pattern.clone(),
            source,
        })?;
        builder.add(glob);
    }
    builder.build().map_err(|source| ConfigError::Glob {
        context: context.to_string(),
        pattern: patterns.join(", "),
        source,
    })
}

/// Compile a regular expression; `context` labels errors.
pub fn compile_regex(context: &str, pattern: &str) -> Result<Regex, ConfigError> {
    Regex::new(pattern).map_err(|source| ConfigError::Regex {
        context: context.to_string(),
        pattern: pattern.to_string(),
        source,
    })
}

fn valid_source_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

impl Config {
    /// Read and validate a config file.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Config::from_yaml(&text)
    }

    /// Parse and validate config text.
    pub fn from_yaml(text: &str) -> Result<Config, ConfigError> {
        let config: Config = serde_yaml_ng::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    /// Check every invariant that parsing alone cannot enforce.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.version != CONFIG_VERSION {
            return Err(ConfigError::Version(self.version));
        }
        if self.sources.is_empty() {
            return Err(ConfigError::NoSources);
        }
        let mut seen = BTreeSet::new();
        for source in &self.sources {
            source.validate()?;
            if !seen.insert(source.name.as_str()) {
                return Err(ConfigError::DuplicateSource(source.name.clone()));
            }
        }
        compile_globs("policy.deny", &self.policy.deny)?;
        Ok(())
    }

    /// The compiled `policy.deny` set.
    pub fn deny_set(&self) -> Result<GlobSet, ConfigError> {
        compile_globs("policy.deny", &self.policy.deny)
    }

    /// Look up a source by name.
    pub fn source(&self, name: &str) -> Option<&Source> {
        self.sources.iter().find(|s| s.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
version: 1
sources:
  - name: handbook
    repo: https://github.com/example-org/handbook.git
    ref: main
    priority: 10
    resolver:
      type: glob
      include: ["docs/user/**/*.md"]
      exclude: ["**/_sidebar.md"]
  - name: guides
    repo: https://github.com/example-org/guides.git
    ref: main
    priority: 1
    resolver:
      type: external
      command: ["python3", "resolvers/toc.py"]
      args: ["--title-match", "(?i)handbook"]
      residue_mention: "(?i)handbook"
policy:
  deny: ["**/CLAUDE.md", "**/adr/**", "**/CHANGELOG.md"]
  archived: warn
  min_pages_per_source: 1
eval:
  queries: queries.jsonl
  k: 10
  max_recall_drop: 0.05
"#;

    fn minimal(name: &str, repo: &str) -> String {
        format!(
            "version: 1\nsources:\n  - name: {name}\n    repo: {repo}\n    ref: main\n    \
             resolver:\n      type: glob\n      include: ['**/*.md']\n"
        )
    }

    #[test]
    fn parses_the_spec_example() {
        let config = Config::from_yaml(FULL).expect("valid config");
        assert_eq!(config.version, 1);
        assert_eq!(config.sources.len(), 2);
        let handbook = &config.sources[0];
        assert_eq!(handbook.name, "handbook");
        assert_eq!(handbook.git_ref, "main");
        assert_eq!(handbook.priority, 10);
        assert_eq!(handbook.slug().to_string(), "example-org/handbook");
        assert_eq!(handbook.resolver.kind(), "glob");
        let guides = &config.sources[1];
        match &guides.resolver {
            Resolver::External {
                command,
                args,
                residue_mention,
                residue_scope,
                include,
                exclude,
            } => {
                assert_eq!(command, &["python3", "resolvers/toc.py"]);
                assert_eq!(args, &["--title-match", "(?i)handbook"]);
                assert_eq!(residue_mention.as_deref(), Some("(?i)handbook"));
                assert!(residue_scope.is_empty());
                assert!(include.is_empty());
                assert!(exclude.is_empty());
            }
            other => panic!("expected external resolver, got {other:?}"),
        }
        assert_eq!(config.policy.archived, ArchivedPolicy::Warn);
        assert_eq!(config.policy.min_pages_per_source, 1);
        assert_eq!(config.policy.deny.len(), 3);
        let eval = config.eval.expect("eval section");
        assert_eq!(eval.k, 10);
        assert!((eval.max_recall_drop - 0.05).abs() < f64::EPSILON);
        assert!(
            (eval.holdout_min - crate::queries::DEFAULT_HOLDOUT_MIN).abs() < f64::EPSILON,
            "holdout_min defaults when the config omits it"
        );
    }

    #[test]
    fn holdout_min_can_be_set_explicitly() {
        let text = format!(
            "{}{}",
            minimal("a", "https://github.com/o/r"),
            "eval:\n  queries: queries.jsonl\n  holdout_min: 0.3\n"
        );
        let config = Config::from_yaml(&text).unwrap();
        assert!((config.eval.unwrap().holdout_min - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn defaults_apply_when_sections_are_missing() {
        let config = Config::from_yaml(&minimal("a", "https://github.com/o/r")).expect("valid");
        assert_eq!(config.policy, Policy::default());
        assert!(config.eval.is_none());
        assert_eq!(config.sources[0].priority, DEFAULT_PRIORITY);
        assert_eq!(config.sources[0].slug().url(), "https://github.com/o/r.git");
        assert!(config.source("a").is_some());
        assert!(config.source("b").is_none());
    }

    #[test]
    fn rejects_bad_source_names() {
        for bad in ["", "with space", "slash/name", "dot.name", "handbook::x"] {
            let err = Config::from_yaml(&minimal(bad, "https://github.com/o/r")).unwrap_err();
            assert!(matches!(err, ConfigError::SourceName(_)), "{bad:?}: {err}");
        }
        assert!(Config::from_yaml(&minimal("ok_name-1", "https://github.com/o/r")).is_ok());
    }

    #[test]
    fn rejects_non_github_repos() {
        for bad in [
            "https://gitlab.com/o/r.git",
            "git@github.com:o/r.git",
            "http://github.com/o/r",
            "https://github.com/o",
            "https://github.com/o/r/extra",
        ] {
            let err = Config::from_yaml(&minimal("a", bad)).unwrap_err();
            assert!(matches!(err, ConfigError::RepoUrl { .. }), "{bad:?}: {err}");
        }
    }

    #[test]
    fn rejects_unknown_resolver_type_and_unknown_keys() {
        let text = "version: 1\nsources:\n  - name: a\n    repo: https://github.com/o/r\n    \
                    ref: main\n    resolver:\n      type: sidebar\n";
        assert!(matches!(
            Config::from_yaml(text).unwrap_err(),
            ConfigError::Yaml(_)
        ));
        let text = "version: 1\nsources:\n  - name: a\n    repo: https://github.com/o/r\n    \
                    ref: main\n    typo: 1\n    resolver:\n      type: glob\n      include: ['*']\n";
        assert!(matches!(
            Config::from_yaml(text).unwrap_err(),
            ConfigError::Yaml(_)
        ));
    }

    #[test]
    fn rejects_version_duplicates_globs_and_regexes() {
        let base = minimal("a", "https://github.com/o/r");
        let err = Config::from_yaml(&base.replace("version: 1", "version: 2")).unwrap_err();
        assert!(matches!(err, ConfigError::Version(2)));

        let dup = format!("{base}{}", &base["version: 1\nsources:\n".len()..]);
        assert!(matches!(
            Config::from_yaml(&dup).unwrap_err(),
            ConfigError::DuplicateSource(_)
        ));

        let bad_glob = base.replace("'**/*.md'", "'docs/[a'");
        assert!(matches!(
            Config::from_yaml(&bad_glob).unwrap_err(),
            ConfigError::Glob { .. }
        ));

        let bad_regex = "version: 1\nsources:\n  - name: a\n    repo: https://github.com/o/r\n    \
                         ref: main\n    resolver:\n      type: external\n      command: [x]\n      \
                         residue_mention: '('\n";
        assert!(matches!(
            Config::from_yaml(bad_regex).unwrap_err(),
            ConfigError::Regex { .. }
        ));

        let empty_cmd = bad_regex.replace("command: [x]", "command: []");
        let empty_cmd = empty_cmd.replace("residue_mention: '('", "");
        assert!(matches!(
            Config::from_yaml(&empty_cmd).unwrap_err(),
            ConfigError::EmptyCommand(_)
        ));

        assert!(matches!(
            Config::from_yaml("version: 1\nsources: []\n").unwrap_err(),
            ConfigError::NoSources
        ));
        let bad_deny = format!("{base}policy:\n  deny: ['[']\n");
        assert!(matches!(
            Config::from_yaml(&bad_deny).unwrap_err(),
            ConfigError::Glob { .. }
        ));
    }

    #[test]
    fn repo_slug_round_trips() {
        let slug = RepoSlug::parse("https://github.com/example-org/handbook.git").unwrap();
        assert_eq!(slug.url(), "https://github.com/example-org/handbook.git");
        assert_eq!(
            slug.blob_base_url("abc"),
            "https://github.com/example-org/handbook/blob/abc"
        );
        assert_eq!(
            RepoSlug::parse("https://github.com/o/r/").unwrap().repo,
            "r"
        );
        assert!(RepoSlug::parse("https://github.com/../r").is_none());
        assert_eq!(RepoSlug::from_slug("example-org/handbook").unwrap(), slug);
    }

    #[test]
    fn config_survives_a_yaml_round_trip() {
        let config = Config::from_yaml(FULL).unwrap();
        let text = serde_yaml_ng::to_string(&config).unwrap();
        assert_eq!(Config::from_yaml(&text).unwrap(), config);
    }

    #[test]
    fn parses_the_navigation_resolver_types() {
        let text = "version: 1\nsources:\n  - name: a\n    repo: https://github.com/o/r\n    ref: main\n    \
                     resolver:\n      type: vitepress\n";
        let config = Config::from_yaml(text).unwrap();
        match &config.sources[0].resolver {
            Resolver::Vitepress {
                path,
                scope,
                include,
                exclude,
            } => {
                assert!(path.is_none());
                assert!(scope.is_empty());
                assert!(include.is_empty());
                assert!(exclude.is_empty());
            }
            other => panic!("expected vitepress, got {other:?}"),
        }
        assert_eq!(config.sources[0].resolver.kind(), "vitepress");

        let text = "version: 1\nsources:\n  - name: a\n    repo: https://github.com/o/r\n    ref: main\n    \
                     resolver:\n      type: docusaurus\n      path: sidebars.ts\n      scope: ['docs/**/*.md']\n      \
                     include: ['README.md']\n      exclude: ['docs/internal/**']\n";
        let config = Config::from_yaml(text).unwrap();
        match &config.sources[0].resolver {
            Resolver::Docusaurus {
                path,
                scope,
                include,
                exclude,
            } => {
                assert_eq!(path.as_deref(), Some("sidebars.ts"));
                assert_eq!(scope, &["docs/**/*.md"]);
                assert_eq!(include, &["README.md"]);
                assert_eq!(exclude, &["docs/internal/**"]);
            }
            other => panic!("expected docusaurus, got {other:?}"),
        }
        assert_eq!(config.sources[0].resolver.kind(), "docusaurus");

        let text = "version: 1\nsources:\n  - name: a\n    repo: https://github.com/o/r\n    ref: main\n    \
                     resolver:\n      type: mdbook\n";
        let config = Config::from_yaml(text).unwrap();
        assert_eq!(config.sources[0].resolver.kind(), "mdbook");

        let text = "version: 1\nsources:\n  - name: a\n    repo: https://github.com/o/r\n    ref: main\n    \
                     resolver:\n      type: sitemap\n      url_prefix: 'https://example.com/docs/'\n      \
                     path_prefix: 'docs/'\n";
        let config = Config::from_yaml(text).unwrap();
        match &config.sources[0].resolver {
            Resolver::Sitemap {
                path,
                url_prefix,
                path_prefix,
                scope,
                include,
                exclude,
            } => {
                assert!(path.is_none());
                assert_eq!(url_prefix, "https://example.com/docs/");
                assert_eq!(path_prefix, "docs/");
                assert!(scope.is_empty());
                assert!(include.is_empty());
                assert!(exclude.is_empty());
            }
            other => panic!("expected sitemap, got {other:?}"),
        }
        assert_eq!(config.sources[0].resolver.kind(), "sitemap");

        let text = serde_yaml_ng::to_string(&config).unwrap();
        assert_eq!(Config::from_yaml(&text).unwrap(), config, "round trips");
    }

    #[test]
    fn rejects_bad_sitemap_and_scope_options() {
        let base = "version: 1\nsources:\n  - name: a\n    repo: https://github.com/o/r\n    ref: main\n    \
                     resolver:\n      type: sitemap\n";
        let empty_url_prefix = format!("{base}      url_prefix: ''\n      path_prefix: 'docs/'\n");
        assert!(matches!(
            Config::from_yaml(&empty_url_prefix).unwrap_err(),
            ConfigError::EmptyUrlPrefix(_)
        ));

        let empty_path_prefix =
            format!("{base}      url_prefix: 'https://example.com/'\n      path_prefix: ''\n");
        assert!(matches!(
            Config::from_yaml(&empty_path_prefix).unwrap_err(),
            ConfigError::EmptyPathPrefix(_)
        ));

        let bad_scope = "version: 1\nsources:\n  - name: a\n    repo: https://github.com/o/r\n    ref: main\n    \
                          resolver:\n      type: mdbook\n      scope: ['[']\n";
        assert!(matches!(
            Config::from_yaml(bad_scope).unwrap_err(),
            ConfigError::Glob { .. }
        ));
    }

    #[test]
    fn every_resolver_but_glob_accepts_include_and_exclude() {
        let cases = [
            "type: external\n      command: ['x']\n      include: ['README.md']\n      exclude: ['docs/x.md']\n",
            "type: vitepress\n      include: ['README.md']\n      exclude: ['docs/x.md']\n",
            "type: docusaurus\n      include: ['README.md']\n      exclude: ['docs/x.md']\n",
            "type: mdbook\n      include: ['README.md']\n      exclude: ['docs/x.md']\n",
            "type: sitemap\n      url_prefix: 'https://example.com/'\n      path_prefix: 'docs/'\n      \
             include: ['README.md']\n      exclude: ['docs/x.md']\n",
        ];
        for resolver in cases {
            let text = format!(
                "version: 1\nsources:\n  - name: a\n    repo: https://github.com/o/r\n    ref: main\n    \
                 resolver:\n      {resolver}"
            );
            let config = Config::from_yaml(&text).unwrap_or_else(|e| panic!("{resolver}: {e}"));
            let text = serde_yaml_ng::to_string(&config).unwrap();
            assert_eq!(
                Config::from_yaml(&text).unwrap(),
                config,
                "{resolver}: round trips"
            );
        }
    }

    #[test]
    fn rejects_bad_include_and_exclude_globs() {
        let base = "version: 1\nsources:\n  - name: a\n    repo: https://github.com/o/r\n    ref: main\n    \
                     resolver:\n      type: mdbook\n";
        let bad_include = format!("{base}      include: ['[']\n");
        assert!(matches!(
            Config::from_yaml(&bad_include).unwrap_err(),
            ConfigError::Glob { .. }
        ));
        let bad_exclude = format!("{base}      exclude: ['[']\n");
        assert!(matches!(
            Config::from_yaml(&bad_exclude).unwrap_err(),
            ConfigError::Glob { .. }
        ));
    }

    #[test]
    fn glob_extensions_default_is_unset_and_an_explicit_list_or_empty_list_parses() {
        let config = Config::from_yaml(&minimal("a", "https://github.com/o/r")).unwrap();
        match &config.sources[0].resolver {
            Resolver::Glob { extensions, .. } => assert!(extensions.is_none()),
            other => panic!("expected glob, got {other:?}"),
        }

        let text = "version: 1\nsources:\n  - name: a\n    repo: https://github.com/o/r\n    ref: main\n    \
                     resolver:\n      type: glob\n      include: ['**/*']\n      \
                     extensions: ['yaml', 'json']\n";
        let config = Config::from_yaml(text).unwrap();
        match &config.sources[0].resolver {
            Resolver::Glob { extensions, .. } => {
                assert_eq!(
                    extensions.as_deref(),
                    Some(&["yaml".to_string(), "json".to_string()][..])
                );
            }
            other => panic!("expected glob, got {other:?}"),
        }
        let round = serde_yaml_ng::to_string(&config).unwrap();
        assert_eq!(Config::from_yaml(&round).unwrap(), config, "round trips");

        let text = "version: 1\nsources:\n  - name: a\n    repo: https://github.com/o/r\n    ref: main\n    \
                     resolver:\n      type: glob\n      include: ['**/*']\n      extensions: []\n";
        let config = Config::from_yaml(text).unwrap();
        match &config.sources[0].resolver {
            Resolver::Glob { extensions, .. } => {
                assert_eq!(
                    extensions.as_deref(),
                    Some(&[][..]),
                    "explicit empty list is kept, not None"
                );
            }
            other => panic!("expected glob, got {other:?}"),
        }
    }
}
