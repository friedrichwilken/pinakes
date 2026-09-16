//! `duplicates` on the golden corpus (SPEC §11): the near-duplicate pair added to the fixture
//! for iteration 2 (`handbook::docs/concepts/notifications.md` and a lightly edited, distinctly
//! titled copy in the lower-priority `cookbook` source) must be found, with the higher-priority
//! `handbook` page as canonical. The fixture carries no `manifest.json` (SPEC §7.3 requires
//! `eval` to work from `meta.json` alone), so this also exercises the manifest-less fallback:
//! `sha256` read straight from the artifact and no `selected_by`, leaving priority alone to
//! decide the winner.

use std::path::{Path, PathBuf};

use pinakes::commands::{DuplicatesOptions, Paths, duplicates};
use pinakes::duplicates::{DEFAULT_THRESHOLD, DuplicateKind, Suggested};

const CANONICAL: &str = "handbook::docs/concepts/notifications.md";
const DUPLICATE: &str = "cookbook::docs/recipes/notification-templates.md";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden")
}

#[test]
fn finds_the_near_duplicate_pair_with_the_higher_priority_page_as_canonical() {
    let paths = Paths::for_config(&fixture().join("pinakes.yaml"));
    let pairs = duplicates(&paths, &DuplicatesOptions::default())
        .expect("duplicates runs on the manifest-less golden fixture");

    let pair = pairs
        .iter()
        .find(|p| p.canonical == CANONICAL || p.duplicate == CANONICAL)
        .unwrap_or_else(|| panic!("no pair mentions {CANONICAL} in {pairs:#?}"));

    assert_eq!(pair.kind, DuplicateKind::Near, "{pair:?}");
    assert_eq!(pair.canonical, CANONICAL, "handbook (priority 10) wins");
    assert_eq!(pair.duplicate, DUPLICATE);
    assert_eq!(pair.suggested, Suggested::Exclude);
    assert!(pair.similarity >= DEFAULT_THRESHOLD, "{pair:?}");
    assert!(pair.why.contains("priority 10 > 1"), "{}", pair.why);
}

#[test]
fn a_stricter_threshold_still_finds_it_but_an_impossible_one_does_not() {
    let paths = Paths::for_config(&fixture().join("pinakes.yaml"));

    let strict = duplicates(
        &paths,
        &DuplicatesOptions {
            threshold: 0.85,
            json: None,
        },
    )
    .unwrap();
    assert!(
        strict
            .iter()
            .any(|p| p.canonical == CANONICAL || p.duplicate == CANONICAL)
    );

    let impossible = duplicates(
        &paths,
        &DuplicatesOptions {
            threshold: 1.01,
            json: None,
        },
    )
    .unwrap();
    assert!(
        impossible.iter().all(|p| p.kind != DuplicateKind::Near),
        "{impossible:#?}"
    );
}
