//! Writing what a resolve run produced: the artifact, the manifest, residue and duplicates.

use std::collections::BTreeMap;
use std::path::Path;

use crate::artifact;
use crate::config::Config;
use crate::duplicates::{self, DuplicatePair};
use crate::index::{self, Priorities};
use crate::manifest::{Manifest, split_page_id};
use crate::page::{PageRecord, PageRegistry};
use crate::residue;
use crate::sources::Checkout;
use crate::text::sha256_hex;
use crate::workspace::Paths;

use super::CommandError;

pub(super) fn write_outputs(
    paths: &Paths,
    manifest: &Manifest,
    registry: &mut PageRegistry,
    checkouts: &BTreeMap<String, Checkout>,
) -> Result<Vec<DuplicatePair>, CommandError> {
    artifact::materialise(&paths.artifact, manifest, checkouts)?;
    manifest.save(&paths.manifest)?;
    residue::write_jsonl(&paths.residue, &registry.residue_entries())?;
    let pairs = compute_duplicates(
        paths,
        Some(manifest),
        registry,
        duplicates::DEFAULT_THRESHOLD,
    )?;
    duplicates::write_jsonl(&paths.duplicates, &pairs)?;
    Ok(pairs)
}

/// A page's raw bytes, read from `<artifact>/<source>/<path>`; `None` for a malformed id or a
/// missing or unreadable file.
pub(crate) fn artifact_page_bytes(artifact: &Path, id: &str) -> Option<Vec<u8>> {
    let (source, path) = split_page_id(id)?;
    std::fs::read(artifact.join(source).join(path)).ok()
}

/// Find duplicate pairs in the artifact at `paths.artifact` (SPEC §11). `manifest`, when given,
/// supplies exact `sha256`, `selected_by` and page urls for the winner rule and the reported
/// pairs, through `registry`'s corpus records. A loaded page the registry does not already have
/// a corpus record for (a manifest-less artifact, or a page the manifest does not know about) is
/// added to `registry` as an artifact-only record, with its sha256 read from the file and its
/// url from the manifest when one is given; the winner rule then falls back to source priority
/// alone for it, since it has no `selected_by`.
pub(crate) fn compute_duplicates(
    paths: &Paths,
    manifest: Option<&Manifest>,
    registry: &mut PageRegistry,
    threshold: f64,
) -> Result<Vec<DuplicatePair>, CommandError> {
    let priorities = if paths.config.is_file() {
        Priorities::from_config(&Config::load(&paths.config)?)
    } else {
        Priorities::default()
    };
    let pages = index::load_pages(&paths.artifact, &priorities)?;
    for page in &pages {
        if registry.corpus_page(&page.id).is_some() {
            continue;
        }
        let Some(bytes) = artifact_page_bytes(&paths.artifact, &page.id) else {
            continue;
        };
        let url = manifest
            .and_then(|m| m.page_url(&page.id))
            .unwrap_or_default();
        let _ = registry.insert_artifact_only(PageRecord::artifact_only(
            &page.source,
            &page.path,
            sha256_hex(&bytes),
            url,
        ));
    }
    Ok(duplicates::find_duplicates(&pages, registry, threshold))
}
