use std::path::PathBuf;

use crate::duplicates::{self, DuplicatePair};
use crate::error::CommandError;
use crate::manifest::Manifest;
use crate::page::PageRegistry;
use crate::workspace::Paths;

/// Options for `duplicates`.
#[derive(Debug, Clone)]
pub struct DuplicatesOptions {
    /// Minimum Jaccard similarity for a near-duplicate pair.
    pub threshold: f64,
    /// Write the pairs there instead of returning them for the caller to print.
    pub json: Option<PathBuf>,
}

impl Default for DuplicatesOptions {
    fn default() -> DuplicatesOptions {
        DuplicatesOptions {
            threshold: duplicates::DEFAULT_THRESHOLD,
            json: None,
        }
    }
}

/// Run `duplicates`: find exact, mirror and near-duplicate pairs in the artifact (SPEC §11).
/// Uses the committed manifest when present for `selected_by` and page urls; this never touches
/// the network.
pub fn duplicates(
    paths: &Paths,
    options: &DuplicatesOptions,
) -> Result<Vec<DuplicatePair>, CommandError> {
    let manifest = if paths.manifest.is_file() {
        Some(Manifest::load(&paths.manifest)?)
    } else {
        None
    };
    // No residue: a malformed `residue.jsonl` must not be able to break `duplicates`.
    let mut registry = PageRegistry::load(manifest.as_ref(), &[]);
    let pairs = crate::pipeline::outputs::compute_duplicates(
        paths,
        manifest.as_ref(),
        &mut registry,
        options.threshold,
    )?;
    if let Some(path) = &options.json {
        duplicates::write_jsonl(path, &pairs)?;
    }
    Ok(pairs)
}
