//! `init` (SPEC §4): scaffold a commented `pinakes.yaml`, empty `decisions.jsonl` and
//! `queries.jsonl`, the `.gitignore` entries for work directories and, on request, the weekly
//! workflow. Never overwrites; a source's resolver is detected from one fetch of its repository.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::config::{CONFIG_VERSION, RepoSlug};
use crate::error::CommandError;
use crate::sources::{Fetcher, fetch_checkout, list_files};

/// The consumer side of the weekly workflow (SPEC §17.1), embedded so `init --workflow` cannot
/// drift from `examples/curate-weekly.yml`.
pub const WORKFLOW: &str = include_str!("../../examples/curate-weekly.yml");

/// Where `--workflow` writes, relative to the workspace directory.
pub const WORKFLOW_PATH: &str = ".github/workflows/curate.yml";

/// The `.gitignore` lines `init` adds: materialised corpora and the rendered report.
pub const GITIGNORE_LINES: [&str; 3] = ["/artifact", "/artifact-*", "/report.md"];

/// The branch assumed when the fetcher cannot name the repository's default branch.
pub const FALLBACK_REF: &str = "main";

/// The placeholder source written when `init` is given no repository URL.
const PLACEHOLDER_REPO: &str = "https://github.com/example-org/handbook.git";

/// What `init` scaffolds.
#[derive(Debug, Clone)]
pub struct InitOptions {
    /// The config file to write; every other file goes into its directory, created when
    /// missing.
    pub config: PathBuf,
    /// GitHub repository URLs to declare as sources, each fetched once to detect its layout.
    pub repos: Vec<String>,
    /// Also write [`WORKFLOW_PATH`].
    pub workflow: bool,
}

/// What `init` did, file by file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitOutcome {
    /// Files created.
    pub written: Vec<PathBuf>,
    /// Existing files left alone.
    pub skipped: Vec<PathBuf>,
    /// Existing files that only gained lines (`.gitignore`).
    pub updated: Vec<PathBuf>,
    /// Detection problems, one line per source; each is also a comment in the config.
    pub warnings: Vec<String>,
}

/// The resolver `init` picks for a repository from its file list alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectedLayout {
    /// A `.vitepress/config.*` was found at `nav`.
    Vitepress {
        /// The navigation file, relative to the repository root.
        nav: String,
    },
    /// A `sidebars.js` or `sidebars.ts` was found at `nav`.
    Docusaurus {
        /// The navigation file, relative to the repository root.
        nav: String,
    },
    /// A `SUMMARY.md` was found at `nav`.
    Mdbook {
        /// The navigation file, relative to the repository root.
        nav: String,
    },
    /// No navigation file: select Markdown by one glob.
    Glob {
        /// `docs/**/*.md` when the repository has a `docs/` directory, else `**/*.md`.
        include: String,
    },
}

impl DetectedLayout {
    /// The `resolver.type` to write.
    pub fn kind(&self) -> &'static str {
        match self {
            DetectedLayout::Vitepress { .. } => "vitepress",
            DetectedLayout::Docusaurus { .. } => "docusaurus",
            DetectedLayout::Mdbook { .. } => "mdbook",
            DetectedLayout::Glob { .. } => "glob",
        }
    }

    /// The `path` to write: the navigation file when it is not where the resolver looks by
    /// default (SPEC §12), so the generated config resolves without editing.
    pub fn explicit_path(&self) -> Option<&str> {
        let (nav, is_default) = match self {
            DetectedLayout::Vitepress { nav } => (nav, nav.starts_with("docs/.vitepress/config.")),
            DetectedLayout::Docusaurus { nav } => {
                (nav, nav == "sidebars.js" || nav == "sidebars.ts")
            }
            DetectedLayout::Mdbook { nav } => (nav, nav == "src/SUMMARY.md"),
            DetectedLayout::Glob { .. } => return None,
        };
        (!is_default).then_some(nav.as_str())
    }

    /// What the detection saw, for the comment next to `type`.
    fn evidence(&self) -> String {
        match self {
            DetectedLayout::Vitepress { nav }
            | DetectedLayout::Docusaurus { nav }
            | DetectedLayout::Mdbook { nav } => format!("detected: {nav}"),
            DetectedLayout::Glob { include } if include == "docs/**/*.md" => {
                "no navigation file found; docs/ exists".to_string()
            }
            DetectedLayout::Glob { .. } => "no navigation file found".to_string(),
        }
    }
}

/// Pick the resolver for a checkout from its sorted file list (SPEC §4), first rule that
/// matches: a `.vitepress/config.*` anywhere, a `sidebars.js`/`sidebars.ts` anywhere, a
/// `SUMMARY.md` anywhere, otherwise a glob on `docs/**/*.md` when `docs/` exists, else
/// `**/*.md`. A navigation file at the resolver's default location is preferred over one
/// elsewhere.
pub fn detect_layout(files: &[String]) -> DetectedLayout {
    fn dir_of(f: &str) -> &str {
        f.rsplit_once('/').map_or("", |(dir, _)| dir)
    }
    fn name_of(f: &str) -> &str {
        f.rsplit_once('/').map_or(f, |(_, name)| name)
    }
    if let Some(nav) = first_match(files, |f| {
        dir_of(f).ends_with(".vitepress") && name_of(f).starts_with("config.")
    }) {
        return DetectedLayout::Vitepress { nav };
    }
    if let Some(nav) = first_match(files, |f| {
        matches!(name_of(f), "sidebars.js" | "sidebars.ts")
    }) {
        return DetectedLayout::Docusaurus { nav };
    }
    if let Some(nav) = first_match(files, |f| name_of(f) == "SUMMARY.md") {
        return DetectedLayout::Mdbook { nav };
    }
    let has_docs = files.iter().any(|f| f.starts_with("docs/"));
    DetectedLayout::Glob {
        include: if has_docs { "docs/**/*.md" } else { "**/*.md" }.to_string(),
    }
}

/// The first file (in sorted order) satisfying `is_nav`, preferring one whose layout needs no
/// explicit `path`.
fn first_match(files: &[String], is_nav: impl Fn(&str) -> bool) -> Option<String> {
    let mut matches = files.iter().filter(|f| is_nav(f)).cloned();
    let first = matches.next()?;
    let at_default = |nav: &String| {
        nav.starts_with("docs/.vitepress/config.")
            || nav == "sidebars.js"
            || nav == "sidebars.ts"
            || nav == "src/SUMMARY.md"
    };
    if at_default(&first) {
        return Some(first);
    }
    Some(matches.find(at_default).unwrap_or(first))
}

/// A source name from a repository name: `[A-Za-z0-9_-]+`, everything else becomes `-`.
pub fn source_name(repo: &str) -> String {
    repo.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// One source of the generated config.
struct SourceDraft {
    name: String,
    repo: String,
    git_ref: String,
    ref_comment: String,
    layout: DetectedLayout,
    /// Why detection failed, when it did; also the reason `layout` is a plain glob.
    failure: Option<String>,
}

/// Run `init`: write what is missing next to `options.config`, detecting each repository's
/// layout through `fetcher`. A fetch that fails still yields a source (a `glob` resolver plus a
/// comment saying why) rather than an error; only a malformed URL or an I/O failure is one.
pub fn init(options: &InitOptions, fetcher: &dyn Fetcher) -> Result<InitOutcome, CommandError> {
    let dir = options
        .config
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    if !dir.as_os_str().is_empty() {
        std::fs::create_dir_all(&dir).map_err(|source| CommandError::Io {
            path: dir.clone(),
            source,
        })?;
    }
    let mut outcome = InitOutcome::default();

    if options.config.exists() {
        outcome.skipped.push(options.config.clone());
    } else {
        let drafts = draft_sources(&options.repos, fetcher, &mut outcome.warnings)?;
        let yaml = render_config(&drafts)?;
        record(
            &mut outcome,
            &options.config,
            write_new(&options.config, &yaml)?,
        );
    }
    for ledger in ["decisions.jsonl", "queries.jsonl"] {
        let path = dir.join(ledger);
        let written = write_new(&path, "")?;
        record(&mut outcome, &path, written);
    }
    let gitignore = dir.join(".gitignore");
    match merge_gitignore(&gitignore)? {
        Merge::Created => outcome.written.push(gitignore),
        Merge::Appended => outcome.updated.push(gitignore),
        Merge::Unchanged => outcome.skipped.push(gitignore),
    }
    if options.workflow {
        let path = dir.join(WORKFLOW_PATH);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| CommandError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let written = write_new(&path, WORKFLOW)?;
        record(&mut outcome, &path, written);
    }
    Ok(outcome)
}

fn record(outcome: &mut InitOutcome, path: &Path, written: bool) {
    if written {
        outcome.written.push(path.to_path_buf());
    } else {
        outcome.skipped.push(path.to_path_buf());
    }
}

/// Create `path` with `content`; `Ok(false)` when it already exists (left untouched).
fn write_new(path: &Path, content: &str) -> Result<bool, CommandError> {
    let io = |source| CommandError::Io {
        path: path.to_path_buf(),
        source,
    };
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            std::io::Write::write_all(&mut file, content.as_bytes()).map_err(io)?;
            Ok(true)
        }
        Err(err) if err.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(err) => Err(io(err)),
    }
}

enum Merge {
    Created,
    Appended,
    Unchanged,
}

/// Add the [`GITIGNORE_LINES`] an existing `.gitignore` lacks, or create the file.
fn merge_gitignore(path: &Path) -> Result<Merge, CommandError> {
    let io = |source| CommandError::Io {
        path: path.to_path_buf(),
        source,
    };
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == ErrorKind::NotFound => None,
        Err(err) => return Err(io(err)),
    };
    let present: BTreeSet<&str> = existing
        .as_deref()
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .collect();
    let missing: Vec<&str> = GITIGNORE_LINES
        .iter()
        .copied()
        .filter(|line| !present.contains(line))
        .collect();
    if missing.is_empty() {
        return Ok(Merge::Unchanged);
    }
    let mut block = String::new();
    if let Some(text) = &existing
        && !text.is_empty()
    {
        block.push_str(if text.ends_with('\n') { "\n" } else { "\n\n" });
    }
    block.push_str("# pinakes: rebuilt from the manifest and the report, never committed\n");
    for line in &missing {
        block.push_str(line);
        block.push('\n');
    }
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .map_err(io)?;
    std::io::Write::write_all(&mut file, block.as_bytes()).map_err(io)?;
    Ok(if existing.is_some() {
        Merge::Appended
    } else {
        Merge::Created
    })
}

/// One draft per URL, in order; the placeholder source when there is none.
fn draft_sources(
    repos: &[String],
    fetcher: &dyn Fetcher,
    warnings: &mut Vec<String>,
) -> Result<Vec<SourceDraft>, CommandError> {
    if repos.is_empty() {
        return Ok(vec![SourceDraft {
            name: "handbook".to_string(),
            repo: PLACEHOLDER_REPO.to_string(),
            git_ref: FALLBACK_REF.to_string(),
            ref_comment: "branch, tag or SHA".to_string(),
            layout: DetectedLayout::Glob {
                include: "docs/**/*.md".to_string(),
            },
            failure: None,
        }]);
    }
    let mut taken = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut drafts = Vec::with_capacity(repos.len());
    for url in repos {
        let slug = RepoSlug::parse(url).ok_or_else(|| CommandError::RepoUrl(url.clone()))?;
        if !seen.insert(slug.to_string()) {
            warnings.push(format!("{slug}: given more than once; kept the first"));
            continue;
        }
        let name = unique_name(&source_name(&slug.repo), &mut taken);
        let (git_ref, ref_comment) = if let Some(branch) = fetcher.default_branch(&slug) {
            (branch, "the repository's default branch".to_string())
        } else {
            warnings.push(format!(
                "{name}: default branch unknown; assumed {FALLBACK_REF}"
            ));
            (
                FALLBACK_REF.to_string(),
                "assumed; the default branch could not be determined, check it".to_string(),
            )
        };
        let (layout, failure) = match detect_in_checkout(fetcher, &slug, &git_ref) {
            Ok(layout) => (layout, None),
            Err(err) => {
                let reason = err.to_string().replace(['\n', '\r'], " ");
                warnings.push(format!(
                    "{name}: layout not detected ({reason}); wrote a glob resolver"
                ));
                (
                    DetectedLayout::Glob {
                        include: "**/*.md".to_string(),
                    },
                    Some(reason),
                )
            }
        };
        drafts.push(SourceDraft {
            name,
            repo: slug.url(),
            git_ref,
            ref_comment,
            layout,
            failure,
        });
    }
    Ok(drafts)
}

/// `name`, or `name-2`, `name-3`, … when `taken` already holds it.
fn unique_name(name: &str, taken: &mut BTreeSet<String>) -> String {
    let mut candidate = name.to_string();
    let mut n = 1;
    while taken.contains(&candidate) {
        n += 1;
        candidate = format!("{name}-{n}");
    }
    taken.insert(candidate.clone());
    candidate
}

/// Fetch `slug` at `git_ref` into a temporary directory and detect its layout.
fn detect_in_checkout(
    fetcher: &dyn Fetcher,
    slug: &RepoSlug,
    git_ref: &str,
) -> Result<DetectedLayout, CommandError> {
    let work = tempfile::tempdir().map_err(|source| CommandError::Io {
        path: std::env::temp_dir(),
        source,
    })?;
    let checkout = fetch_checkout(fetcher, slug, git_ref, work.path()).map_err(|source| {
        CommandError::Source {
            name: slug.to_string(),
            source,
        }
    })?;
    let files = list_files(&checkout.root).map_err(|source| CommandError::Source {
        name: slug.to_string(),
        source,
    })?;
    Ok(detect_layout(&files))
}

/// The commented config text for `drafts`; it parses and validates as a [`Config`]
/// (`crate::config::Config`) because every value written is one the schema accepts.
fn render_config(drafts: &[SourceDraft]) -> Result<String, CommandError> {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# pinakes.yaml, written by `pinakes init`. Keys: SPEC.md section 2.1; resolver `path`: section 12."
    );
    let _ = writeln!(out, "version: {CONFIG_VERSION}");
    let _ = writeln!(out, "sources:");
    for draft in drafts {
        let name_comment = if draft.repo == PLACEHOLDER_REPO {
            "placeholder: replace with your repository"
        } else {
            "directory name in the artifact; [A-Za-z0-9_-]+"
        };
        let _ = writeln!(out, "  - name: {}  # {name_comment}", draft.name);
        let _ = writeln!(out, "    repo: {}", draft.repo);
        // A JSON string is a valid YAML double-quoted scalar, whatever the branch is called.
        let quoted_ref = serde_json::to_string(&draft.git_ref)?;
        let _ = writeln!(out, "    ref: {quoted_ref}  # {}", draft.ref_comment);
        let _ = writeln!(out, "    resolver:");
        let type_comment = match &draft.failure {
            Some(reason) => format!("layout not detected ({reason}); adjust include"),
            None if draft.repo == PLACEHOLDER_REPO => {
                "glob | vitepress | docusaurus | mdbook | sitemap | external".to_string()
            }
            None => draft.layout.evidence(),
        };
        let _ = writeln!(out, "      type: {}  # {type_comment}", draft.layout.kind());
        if let Some(path) = draft.layout.explicit_path() {
            let quoted = serde_json::to_string(path)?;
            let _ = writeln!(out, "      path: {quoted}  # not at the default location");
        }
        if let DetectedLayout::Glob { include } = &draft.layout {
            let quoted = serde_json::to_string(include)?;
            let _ = writeln!(
                out,
                "      include: [{quoted}]  # Markdown only, by default"
            );
        }
    }
    out.push_str(
        "policy:\n\
         \x20 deny: [\"**/CLAUDE.md\", \"**/adr/**\", \"**/CHANGELOG.md\"]  # beats everything\n\
         \x20 archived: warn  # warn | drop\n\
         \x20 min_pages_per_source: 1  # verify fails below this\n\
         # eval:  # enable once queries.jsonl has entries\n\
         #   queries: queries.jsonl\n\
         #   k: 10\n\
         #   max_recall_drop: 0.05  # eval --gate exits 2 beyond this\n",
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Resolver};
    use crate::sources::testing::{FakeFetcher, build_tarball};

    const SHA: &str = "4427d7ba863973c2cea9da74ed8675c5c74aee77";
    const HANDBOOK: &str = "https://github.com/example-org/handbook.git";

    fn strings(files: &[&str]) -> Vec<String> {
        let mut files: Vec<String> = files.iter().map(ToString::to_string).collect();
        files.sort();
        files
    }

    #[test]
    fn detects_each_layout_in_precedence_order() {
        let vitepress = detect_layout(&strings(&[
            "docs/.vitepress/config.ts",
            "sidebars.js",
            "src/SUMMARY.md",
        ]));
        assert_eq!(
            vitepress,
            DetectedLayout::Vitepress {
                nav: "docs/.vitepress/config.ts".to_string()
            }
        );
        assert_eq!(vitepress.explicit_path(), None);

        let elsewhere = detect_layout(&strings(&["site/.vitepress/config.mts", "docs/a.md"]));
        assert_eq!(elsewhere.kind(), "vitepress");
        assert_eq!(
            elsewhere.explicit_path(),
            Some("site/.vitepress/config.mts")
        );

        let docusaurus = detect_layout(&strings(&["sidebars.ts", "docs/a.md", "src/SUMMARY.md"]));
        assert_eq!(docusaurus.kind(), "docusaurus");
        assert_eq!(docusaurus.explicit_path(), None);
        let nested = detect_layout(&strings(&["website/sidebars.js", "a/sidebars.ts"]));
        assert_eq!(
            nested.explicit_path(),
            Some("a/sidebars.ts"),
            "sorted first"
        );
        let root_wins = detect_layout(&strings(&["a/sidebars.ts", "sidebars.js"]));
        assert_eq!(root_wins.explicit_path(), None, "the default location wins");

        let mdbook = detect_layout(&strings(&["src/SUMMARY.md", "src/intro.md"]));
        assert_eq!(mdbook.kind(), "mdbook");
        assert_eq!(mdbook.explicit_path(), None);
        let book = detect_layout(&strings(&["book/SUMMARY.md", "README.md"]));
        assert_eq!(book.explicit_path(), Some("book/SUMMARY.md"));

        assert_eq!(
            detect_layout(&strings(&["docs/a.md", "README.md"])),
            DetectedLayout::Glob {
                include: "docs/**/*.md".to_string()
            }
        );
        assert_eq!(
            detect_layout(&strings(&["README.md", "guide/a.md"])),
            DetectedLayout::Glob {
                include: "**/*.md".to_string()
            }
        );
        assert_eq!(detect_layout(&[]).kind(), "glob");
    }

    #[test]
    fn source_names_are_sanitised_and_made_unique() {
        assert_eq!(source_name("my.repo"), "my-repo");
        assert_eq!(source_name("Hand_book-1"), "Hand_book-1");
        let mut taken = BTreeSet::new();
        assert_eq!(unique_name("a", &mut taken), "a");
        assert_eq!(unique_name("a", &mut taken), "a-2");
        assert_eq!(unique_name("a", &mut taken), "a-3");
    }

    fn fetcher_with(slug: &str, wrapper: &str, files: &[(&str, &[u8])]) -> FakeFetcher {
        let mut fetcher = FakeFetcher::default();
        fetcher.add_tarball(slug, "trunk", build_tarball(wrapper, Some(SHA), files));
        fetcher.set_default_branch(slug, "trunk");
        fetcher
    }

    fn run(dir: &Path, repos: &[&str], workflow: bool, fetcher: &FakeFetcher) -> InitOutcome {
        init(
            &InitOptions {
                config: dir.join("pinakes.yaml"),
                repos: repos.iter().map(ToString::to_string).collect(),
                workflow,
            },
            fetcher,
        )
        .unwrap()
    }

    #[test]
    fn writes_a_loadable_config_with_the_detected_resolver_and_default_branch() {
        type Case<'a> = (&'a [(&'a str, &'a [u8])], &'a str, Option<&'a str>);
        let cases: [Case<'_>; 4] = [
            (&[("docs/.vitepress/config.ts", b"{}")], "vitepress", None),
            (
                &[("www/sidebars.js", b"{}")],
                "docusaurus",
                Some("www/sidebars.js"),
            ),
            (&[("src/SUMMARY.md", b"- [a](a.md)")], "mdbook", None),
            (&[("docs/a.md", b"# A")], "glob", None),
        ];
        for (files, kind, path) in cases {
            let dir = tempfile::tempdir().unwrap();
            let fetcher = fetcher_with("example-org/handbook", "handbook-trunk", files);
            let outcome = run(dir.path(), &[HANDBOOK], false, &fetcher);
            assert!(
                outcome.warnings.is_empty(),
                "{kind}: {:?}",
                outcome.warnings
            );
            assert_eq!(outcome.written.len(), 4, "{kind}: {outcome:?}");
            let config = Config::load(&dir.path().join("pinakes.yaml"))
                .unwrap_or_else(|e| panic!("{kind}: generated config must load: {e}"));
            let source = &config.sources[0];
            assert_eq!(source.name, "handbook");
            assert_eq!(source.repo, HANDBOOK);
            assert_eq!(source.git_ref, "trunk");
            assert_eq!(source.resolver.kind(), kind);
            let written_path = match &source.resolver {
                Resolver::Vitepress { path, .. }
                | Resolver::Docusaurus { path, .. }
                | Resolver::Mdbook { path, .. } => path.as_deref(),
                Resolver::Glob { include, .. } => {
                    assert_eq!(include, &["docs/**/*.md"]);
                    None
                }
                other => panic!("{other:?}"),
            };
            assert_eq!(written_path, path, "{kind}");
            let text = std::fs::read_to_string(dir.path().join("pinakes.yaml")).unwrap();
            assert!(
                text.contains("# eval:"),
                "eval block is commented out:\n{text}"
            );
            assert_eq!(fetcher.requests.lock().unwrap().len(), 1, "fetched once");
        }
    }

    #[test]
    fn a_failed_fetch_still_writes_a_glob_source_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let fetcher = FakeFetcher::default();
        let outcome = run(dir.path(), &[HANDBOOK], false, &fetcher);
        assert_eq!(outcome.warnings.len(), 2, "{:?}", outcome.warnings);
        assert!(outcome.warnings[0].contains("assumed main"));
        assert!(outcome.warnings[1].contains("layout not detected"));
        let text = std::fs::read_to_string(dir.path().join("pinakes.yaml")).unwrap();
        assert!(text.contains("layout not detected (source example-org/handbook: no tarball"));
        assert!(text.contains("assumed; the default branch could not be determined"));
        let config = Config::from_yaml(&text).expect("still loads");
        assert_eq!(config.sources[0].git_ref, "main");
        match &config.sources[0].resolver {
            Resolver::Glob { include, .. } => assert_eq!(include, &["**/*.md"]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn no_urls_gives_a_placeholder_source_that_loads() {
        let dir = tempfile::tempdir().unwrap();
        let fetcher = FakeFetcher::default();
        let outcome = run(dir.path(), &[], false, &fetcher);
        assert!(outcome.warnings.is_empty());
        assert!(
            fetcher.requests.lock().unwrap().is_empty(),
            "nothing fetched"
        );
        let config = Config::load(&dir.path().join("pinakes.yaml")).unwrap();
        assert_eq!(config.sources[0].repo, PLACEHOLDER_REPO);
        assert_eq!(config.policy.deny.len(), 3);
        assert_eq!(config.policy.min_pages_per_source, 1);
        assert!(config.eval.is_none());
        for ledger in ["decisions.jsonl", "queries.jsonl"] {
            assert_eq!(std::fs::read(dir.path().join(ledger)).unwrap(), b"");
        }
    }

    #[test]
    fn two_repositories_with_the_same_name_get_distinct_sources() {
        let dir = tempfile::tempdir().unwrap();
        let mut fetcher = fetcher_with("a/handbook", "handbook-trunk", &[("docs/a.md", b"a")]);
        fetcher.add_tarball(
            "b/handbook",
            "trunk",
            build_tarball("handbook-trunk", Some(SHA), &[("src/SUMMARY.md", b"")]),
        );
        fetcher.set_default_branch("b/handbook", "trunk");
        run(
            dir.path(),
            &[
                "https://github.com/a/handbook",
                "https://github.com/b/handbook.git",
            ],
            false,
            &fetcher,
        );
        let config = Config::load(&dir.path().join("pinakes.yaml")).unwrap();
        let names: Vec<&str> = config.sources.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["handbook", "handbook-2"]);
        assert_eq!(config.sources[1].resolver.kind(), "mdbook");
    }

    #[test]
    fn a_repository_given_twice_is_fetched_and_written_once() {
        let dir = tempfile::tempdir().unwrap();
        let fetcher = fetcher_with(
            "example-org/handbook",
            "handbook-trunk",
            &[("docs/a.md", b"a")],
        );
        let outcome = run(
            dir.path(),
            &[HANDBOOK, "https://github.com/example-org/handbook"],
            false,
            &fetcher,
        );
        assert_eq!(
            outcome.warnings,
            ["example-org/handbook: given more than once; kept the first"]
        );
        assert_eq!(fetcher.requests.lock().unwrap().len(), 1, "fetched once");
        let config = Config::load(&dir.path().join("pinakes.yaml")).unwrap();
        assert_eq!(config.sources.len(), 1);
        assert_eq!(config.sources[0].name, "handbook");
    }

    #[test]
    fn rejects_a_url_that_is_not_a_github_repository() {
        let dir = tempfile::tempdir().unwrap();
        let err = init(
            &InitOptions {
                config: dir.path().join("pinakes.yaml"),
                repos: vec!["https://gitlab.example/o/r".to_string()],
                workflow: false,
            },
            &FakeFetcher::default(),
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::RepoUrl(_)), "{err}");
        assert!(!dir.path().join("pinakes.yaml").exists());
    }

    #[test]
    fn never_overwrites_and_reports_what_it_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let fetcher = FakeFetcher::default();
        std::fs::write(dir.path().join("pinakes.yaml"), "version: 1\n").unwrap();
        std::fs::write(dir.path().join("queries.jsonl"), "{}\n").unwrap();
        let outcome = run(dir.path(), &[HANDBOOK], true, &fetcher);
        assert!(
            fetcher.requests.lock().unwrap().is_empty(),
            "no fetch for a kept config"
        );
        assert_eq!(
            outcome.skipped,
            [
                dir.path().join("pinakes.yaml"),
                dir.path().join("queries.jsonl")
            ]
        );
        assert_eq!(
            outcome.written,
            [
                dir.path().join("decisions.jsonl"),
                dir.path().join(".gitignore"),
                dir.path().join(WORKFLOW_PATH),
            ]
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("pinakes.yaml")).unwrap(),
            "version: 1\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("queries.jsonl")).unwrap(),
            "{}\n"
        );

        let again = run(dir.path(), &[], true, &fetcher);
        assert!(again.written.is_empty(), "{again:?}");
        assert!(again.updated.is_empty(), "{again:?}");
        assert_eq!(again.skipped.len(), 5, "{again:?}");
    }

    #[test]
    fn gitignore_only_gains_the_lines_it_lacks() {
        let dir = tempfile::tempdir().unwrap();
        let gitignore = dir.path().join(".gitignore");
        std::fs::write(&gitignore, "/target\n/artifact").unwrap();
        let outcome = run(dir.path(), &[], false, &FakeFetcher::default());
        assert_eq!(outcome.updated, std::slice::from_ref(&gitignore));
        let text = std::fs::read_to_string(&gitignore).unwrap();
        assert_eq!(
            text,
            "/target\n/artifact\n\n# pinakes: rebuilt from the manifest and the report, never committed\n\
             /artifact-*\n/report.md\n"
        );
        let again = run(dir.path(), &[], false, &FakeFetcher::default());
        assert!(again.updated.is_empty());
        assert_eq!(std::fs::read_to_string(&gitignore).unwrap(), text);
    }

    #[test]
    fn workflow_is_the_example_file_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("corpus");
        let outcome = run(&sub, &[], true, &FakeFetcher::default());
        let path = sub.join(WORKFLOW_PATH);
        assert!(outcome.written.contains(&path));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/curate-weekly.yml")
            )
            .unwrap()
        );
        let without = run(
            &dir.path().join("plain"),
            &[],
            false,
            &FakeFetcher::default(),
        );
        assert!(!without.written.iter().any(|p| p.ends_with("curate.yml")));
    }
}
