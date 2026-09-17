//! The compile pipeline (SPEC stage a): fetch every configured source, resolve and select its
//! pages, render where configured, and write the artifact, manifest, residue and duplicates
//! files. [`resolve`] is the entry point; internally, `fresh` resolves a config from scratch and
//! `reproduce` replays a committed manifest instead; both end by writing the outputs (`outputs`).

mod fresh;
pub(crate) mod outputs;
mod reproduce;

use std::path::{Path, PathBuf};

use crate::decisions::Expired;
use crate::duplicates::DuplicatePair;
use crate::error::CommandError;
use crate::manifest::Manifest;
use crate::page::PageRegistry;
use crate::residue::ResidueEntry;
use crate::sources::Fetcher;
use crate::workspace::Paths;

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
    /// Every page of the run, selected or residue, as one registry. It equals
    /// [`PageRegistry::load`] over the manifest and residue files that were written.
    pub registry: PageRegistry,
}

/// Wrap an I/O error with the path it happened on.
pub(crate) fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> CommandError + '_ {
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
        Some(manifest_path) => reproduce::reproduce(paths, manifest_path, fetcher, work.path())?,
        None => fresh::resolve_fresh(paths, options, fetcher, work.path())?,
    };
    Ok(outcome)
}

#[cfg(test)]
pub(crate) mod testing {
    //! Fixtures shared by the pipeline's own tests and `commands`'s remaining tests: every one
    //! of them sets up a workspace and resolves it before exercising the command under test.

    use std::fs;

    use crate::sources::testing::{FakeFetcher, build_tarball};
    use crate::workspace::Paths;

    use super::ResolveOptions;

    pub(crate) const SHA: &str = "4427d7ba863973c2cea9da74ed8675c5c74aee77";

    pub(crate) const CONFIG: &str = "version: 1\nsources:\n  - name: handbook\n    repo: https://github.com/o/handbook.git\n    \
         ref: main\n    resolver:\n      type: glob\n      include: ['docs/**/*.md']\n      \
         exclude: ['**/_sidebar.md']\npolicy:\n  deny: ['**/adr/**']\n  min_pages_per_source: 2\n";

    pub(crate) fn workspace(config: &str) -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("pinakes.yaml");
        fs::write(&config_path, config).unwrap();
        let paths = Paths::for_config(&config_path);
        (dir, paths)
    }

    pub(crate) fn fetcher() -> FakeFetcher {
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

    pub(crate) fn opts() -> ResolveOptions {
        ResolveOptions {
            from_manifest: None,
            generated_at: Some("2026-09-16T12:00:00Z".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::duplicates;
    use crate::manifest::Manifest;
    use crate::page::PageRegistry;
    use crate::residue::{self, Reason};
    use crate::sources::testing::FakeFetcher;

    use super::testing::{CONFIG, SHA, fetcher, opts, workspace};
    use super::*;

    /// The registry `resolve` returns is the one a later command gets by loading the files
    /// `resolve` wrote.
    fn assert_registry_matches_the_written_files(paths: &Paths, outcome: &ResolveOutcome) {
        let manifest = Manifest::load(&paths.manifest).unwrap();
        let residue = residue::read_jsonl(&paths.residue).unwrap();
        assert_eq!(
            outcome.registry,
            PageRegistry::load(Some(&manifest), &residue)
        );
        assert_eq!(
            outcome.registry.selected().count(),
            outcome.manifest.pages().count()
        );
        assert_eq!(outcome.registry.residue().count(), outcome.residue.len());
    }

    #[test]
    fn resolve_returns_the_registry_of_the_files_it_wrote() {
        // A fresh resolve.
        let (_dir, paths) = workspace(CONFIG);
        let mut fetcher = fetcher();
        let fresh = resolve(&paths, &opts(), &fetcher).unwrap();
        assert_registry_matches_the_written_files(&paths, &fresh);
        // The fixture's 4 files: docs/a.md and docs/b.md are selected; docs/_sidebar.md is
        // residue (resolver:exclude) and docs/adr/1.md is residue (policy:deny).
        assert_eq!(fresh.registry.len(), 4);
        let excluded = fresh.registry.get("handbook::docs/adr/1.md").unwrap();
        assert!(excluded.is_excluded());
        assert_eq!(excluded.commit, SHA);

        // `--from-manifest` over the manifest just written.
        let reproduced = resolve(
            &paths,
            &ResolveOptions {
                from_manifest: Some(paths.manifest.clone()),
                generated_at: None,
            },
            &fetcher,
        )
        .unwrap();
        assert_registry_matches_the_written_files(&paths, &reproduced);
        assert_eq!(reproduced.registry.len(), 4);

        // A dropped archived source: residue only, from a source the manifest does not list.
        fetcher.set_archived("o/handbook", true);
        fs::write(&paths.config, format!("{CONFIG}  archived: drop\n")).unwrap();
        let dropped = resolve(&paths, &opts(), &fetcher).unwrap();
        assert_registry_matches_the_written_files(&paths, &dropped);
        assert_eq!(dropped.registry.selected().count(), 0);
        let page = dropped.registry.get("handbook::docs/a.md").unwrap();
        assert_eq!((page.repo.as_str(), page.commit.as_str()), ("", ""));
        assert!(page.url.ends_with("/docs/a.md"), "{}", page.url);
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
}
