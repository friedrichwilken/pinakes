//! `usage` over a hand-written trail against the golden fixture (SPEC §15.3).
//!
//! The golden fixture (`tests/fixtures/golden`) is intentionally manifest-less (SPEC §7.3), so
//! this test builds a manifest from the artifact's actual pages (`index::load_pages`) rather
//! than reading a committed one, then runs the pure `usage::compute` and `usage::ResidueIndex`
//! functions directly against a small trail written inline.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pinakes::index::{self, Priorities};
use pinakes::manifest::{Manifest, ManifestSource, PageEntry, SelectedBy};
use pinakes::trail;
use pinakes::usage::{self, ResidueIndex};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden")
}

/// A manifest whose page list matches the golden artifact's actual pages; only `pages` matters
/// for `usage::compute` (it only reads the manifest's page ids).
fn manifest_from_artifact(artifact: &Path) -> Manifest {
    let pages = index::load_pages(artifact, &Priorities::default()).unwrap();
    let mut manifest = Manifest::new("2026-09-16T12:00:00Z".to_string());
    for page in pages {
        let source = manifest
            .sources
            .entry(page.source.clone())
            .or_insert_with(|| ManifestSource {
                repo: format!("example-org/{}", page.source),
                repo_url: format!("https://github.com/example-org/{}.git", page.source),
                git_ref: "main".to_string(),
                commit: "0".repeat(40),
                archived: Some(false),
                resolver: "glob".to_string(),
                pages: BTreeMap::new(),
                residue: vec![],
                unresolved: vec![],
                unrendered: vec![],
                render: None,
            });
        source.pages.insert(
            page.path.clone(),
            PageEntry {
                sha256: "0".repeat(64),
                title: page.title.clone(),
                doc_type: page.doc_type.clone(),
                section: page.section.clone(),
                selected_by: SelectedBy::Resolver,
                rendered_from: None,
            },
        );
    }
    manifest
}

const TRAIL: &str = "\
{\"at\": \"2026-09-16T12:00:00Z\", \"query\": \"how do I install the service\", \
 \"retrieved\": [\"handbook::docs/install.md\"], \"ranks\": [1], \
 \"cited\": [\"handbook::docs/install.md\"], \"outcome\": \"ok\"}
{\"at\": \"2026-09-16T12:00:00Z\", \"query\": \"back up and restore data\", \
 \"retrieved\": [\"handbook::docs/tutorials/backup-restore.md\"], \"ranks\": [1], \
 \"cited\": [], \"outcome\": \"bad\"}
{\"at\": \"2026-09-16T12:00:00Z\", \"query\": \"write a plugin\", \
 \"retrieved\": [\"cookbook::docs/README.md\"], \"ranks\": [1], \"cited\": []}
";

#[test]
fn usage_finds_unused_pages_uncited_queries_and_a_residue_gap_candidate() {
    let artifact = fixture().join("artifact");
    let manifest = manifest_from_artifact(&artifact);

    let dir = tempfile::tempdir().unwrap();
    let trail_path = dir.path().join("trail.jsonl");
    std::fs::write(&trail_path, TRAIL).unwrap();
    let entries = trail::read_jsonl(&trail_path).unwrap();
    assert_eq!(entries.len(), 3);

    let residue_index = ResidueIndex::build(&artifact);
    let report = usage::compute(&manifest, &entries, &residue_index);

    // A page nobody retrieved: any handbook page not named in the trail.
    assert!(
        report
            .never_retrieved
            .contains(&"handbook::docs/configuration.md".to_string())
    );
    // Retrieved but never cited: the backup-restore tutorial and the cookbook README.
    assert!(
        report
            .retrieved_never_cited
            .contains(&"handbook::docs/tutorials/backup-restore.md".to_string())
    );
    assert!(
        report
            .retrieved_never_cited
            .contains(&"cookbook::docs/README.md".to_string())
    );
    // The installed query was cited, so it must not appear among uncited queries.
    assert!(
        !report
            .uncited_queries
            .iter()
            .any(|q| q.query == "how do I install the service")
    );

    let plugin_query = report
        .uncited_queries
        .iter()
        .find(|q| q.query == "write a plugin")
        .expect("the plugin query is uncited");
    assert_eq!(
        plugin_query.top_retrieved,
        Some("cookbook::docs/README.md".to_string())
    );
    // The residue index finds the leftover "write a plugin" recipe as the gap candidate.
    let gap = plugin_query
        .best_residue
        .as_ref()
        .expect("a residue page scores against this query");
    assert_eq!(gap.id, "cookbook::docs/recipes/draft-plugin.md");
    assert!(gap.score > 0.0);
}

#[test]
fn usage_json_round_trips_through_report_rendering() {
    let artifact = fixture().join("artifact");
    let manifest = manifest_from_artifact(&artifact);
    let dir = tempfile::tempdir().unwrap();
    let trail_path = dir.path().join("trail.jsonl");
    std::fs::write(&trail_path, TRAIL).unwrap();
    let entries = trail::read_jsonl(&trail_path).unwrap();
    let residue_index = ResidueIndex::build(&artifact);
    let report = usage::compute(&manifest, &entries, &residue_index);

    let json_path = dir.path().join("usage.json");
    report.save(&json_path).unwrap();
    let loaded = pinakes::usage::Usage::load(&json_path).unwrap();
    assert_eq!(loaded, report);
}
