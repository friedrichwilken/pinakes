//! Golden fixture extension (iteration 2 §12 / §18): `tests/fixtures/golden/pinakes.yaml` now
//! declares the `mdbook` resolver for the `cookbook` source, alongside `handbook`'s `glob` and
//! `guides`' `external`, so the golden fixture demonstrates every resolver kind that mattered
//! before iteration 2.
//!
//! `cookbook`'s repository is a placeholder that is never fetched (see `tests/golden.rs`), so
//! this test exercises the resolver for real a different way: it reconstructs cookbook's own
//! checkout from the committed artifact plus its residue, adds a hand-written `SUMMARY.md`
//! naming exactly the pages `artifact/cookbook/meta.json` records, and checks that the
//! `mdbook` resolver selects that same set of pages, with `docs/recipes/enable-caching.md` (a
//! real file, present but not part of cookbook's page set) and the residue file
//! `docs/recipes/draft-plugin.md` correctly left as residue.
//!
//! This does not touch `eval` or `expected.json`, so no `UPDATE_GOLDEN` run was needed: SPEC §5
//! never scores `doc_type` or `section`, so nothing pinned there could change.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pinakes::config::{Config, compile_globs};
use pinakes::residue::Reason;
use pinakes::resolve::{self, ResolveContext};
use pinakes::sources::Checkout;

fn golden_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden")
}

/// Recursively copy every file under `src` into `dst`, creating directories as needed.
fn copy_tree(src: &Path, dst: &Path) {
    if !src.is_dir() {
        return;
    }
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

const SUMMARY: &str = "\
# Summary

[Cookbook](README.md)

# Recipes

- [Scheduled Jobs](recipes/cron-jobs.md)
- [Export to CSV](recipes/export-csv.md)
- [Rate Limiting](recipes/rate-limiting.md)
- [Webhooks](recipes/webhooks.md)
- [Notification Templates](recipes/notification-templates.md)
";

#[test]
fn cookbook_resolves_its_committed_pages_through_the_mdbook_resolver() {
    // Reconstruct cookbook's checkout: the materialised pages plus the one residue file, at
    // their original repository-relative paths.
    let checkout_dir = tempfile::tempdir().unwrap();
    copy_tree(
        &golden_fixture().join("artifact/cookbook/docs"),
        &checkout_dir.path().join("docs"),
    );
    copy_tree(
        &golden_fixture().join("artifact/_residue/cookbook/docs"),
        &checkout_dir.path().join("docs"),
    );
    std::fs::write(checkout_dir.path().join("docs/SUMMARY.md"), SUMMARY).unwrap();

    let config = Config::load(&golden_fixture().join("pinakes.yaml")).unwrap();
    let source = config.source("cookbook").expect("cookbook is configured");
    assert_eq!(source.resolver.kind(), "mdbook");

    let checkout = Checkout {
        root: checkout_dir.path().to_path_buf(),
        commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
    };
    let deny = compile_globs("deny", &[]).unwrap();
    let decisions = BTreeMap::new();
    let ctx = ResolveContext {
        deny: &deny,
        decisions: &decisions,
        config_dir: &golden_fixture(),
        is_new_source: false,
    };
    let result = resolve::resolve_source(source, &checkout, &ctx).unwrap();

    // Exactly the pages tests/fixtures/golden/artifact/cookbook/meta.json records.
    let mut pages: Vec<&str> = result.pages.keys().map(String::as_str).collect();
    pages.sort_unstable();
    assert_eq!(
        pages,
        [
            "docs/README.md",
            "docs/recipes/cron-jobs.md",
            "docs/recipes/export-csv.md",
            "docs/recipes/notification-templates.md",
            "docs/recipes/rate-limiting.md",
            "docs/recipes/webhooks.md",
        ]
    );

    let residue: Vec<&str> = result
        .residue
        .iter()
        .filter(|r| r.reason == Reason::NotSelected)
        .map(|r| r.path.as_str())
        .collect();
    assert_eq!(
        residue,
        [
            "docs/SUMMARY.md",
            "docs/recipes/draft-plugin.md",
            "docs/recipes/enable-caching.md",
        ],
        "the navigation file itself, the committed residue page and the real but unlinked \
         enable-caching.md are all unlinked Markdown in scope"
    );
    assert!(result.unresolved.is_empty());
}
