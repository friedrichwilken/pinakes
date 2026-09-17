use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::config::RepoSlug;
use crate::diff::{self, Diff};
use crate::error::CommandError;
use crate::manifest::{Manifest, split_page_id};
use crate::sources::{Fetcher, fetch_checkout};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::resolve;
    use crate::pipeline::testing::{CONFIG, opts, workspace};
    use crate::sources::testing::{FakeFetcher, build_tarball};
    use crate::text::sha256_hex;

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
}
