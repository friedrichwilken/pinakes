//! The materialised artifact directory (SPEC §2.3).
//!
//! ```text
//! <artifact>/
//!   manifest.json
//!   derived.jsonl                 # derived retrieval text (SPEC §14.4), only when there is some
//!   <source>/…/<page>.md          # selected pages, original relative paths
//!   <source>/meta.json            # {artifact_version, repo, module, base_url, commit, pages, …}
//!   _residue/<source>/…/<page>.md # leftovers, for excerpts and measurement
//! ```
//!
//! The layout is a stable contract that consumers rely on, so it is kept exact. Every file is
//! copied byte for byte from the checkout and its hash is checked against the manifest, which
//! is what makes `resolve --from-manifest` reproducible.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::RepoSlug;
use crate::jsonl::{self, KeyOrder};
use crate::layout::ARTIFACT_VERSION;
use crate::manifest::{Manifest, ManifestError, ManifestSource, page_id, to_sorted_json};
use crate::sources::Checkout;
use crate::text::sha256_hex;

pub use crate::layout::{DERIVED_FILE, MANIFEST_FILE, META_FILE, RESIDUE_DIR};

/// Errors raised while writing or checking an artifact.
#[derive(Debug, Error)]
pub enum ArtifactError {
    /// A filesystem operation failed.
    #[error("{path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The manifest could not be written.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// `meta.json` could not be serialised.
    #[error("serialising meta.json: {0}")]
    Json(#[from] serde_json::Error),
    /// The target directory exists, is not empty and holds no `manifest.json`.
    #[error("{0}: refusing to replace a directory that is not an artifact")]
    NotAnArtifact(PathBuf),
    /// No checkout was supplied for a source in the manifest.
    #[error("no checkout for source {0}")]
    MissingCheckout(String),
    /// A page's bytes do not match the hash recorded in the manifest.
    #[error("{id}: sha256 {actual} does not match the manifest ({expected})")]
    HashMismatch {
        /// Page id.
        id: String,
        /// Hash from the manifest.
        expected: String,
        /// Hash of the bytes found.
        actual: String,
    },
}

/// Per-page metadata inside `meta.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaPage {
    /// Page title.
    pub title: String,
    /// Document type, possibly empty.
    pub doc_type: String,
    /// Navigation section, possibly empty.
    pub section: String,
}

fn default_artifact_version() -> u32 {
    ARTIFACT_VERSION
}

/// `<source>/meta.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    /// The artifact contract version (SPEC §2.8) this file follows; missing means 1.
    #[serde(default = "default_artifact_version")]
    pub artifact_version: u32,
    /// `owner/repo` slug.
    pub repo: String,
    /// The source name.
    pub module: String,
    /// `https://github.com/<owner>/<repo>/blob/<commit>`.
    pub base_url: String,
    /// The resolved commit.
    pub commit: String,
    /// Pages by relative path.
    pub pages: BTreeMap<String, MetaPage>,
    /// Residue paths.
    pub residue: Vec<String>,
    /// Unresolved navigation links.
    pub unresolved: Vec<String>,
    /// Selected paths a render step (SPEC §10.1) produced no page for.
    #[serde(default)]
    pub unrendered: Vec<String>,
}

impl Meta {
    /// Derive the metadata file for a source from its manifest entry.
    pub fn from_manifest(name: &str, source: &ManifestSource) -> Meta {
        let base_url = RepoSlug::from_slug(&source.repo).map_or_else(
            || format!("https://github.com/{}/blob/{}", source.repo, source.commit),
            |slug| slug.blob_base_url(&source.commit),
        );
        Meta {
            artifact_version: ARTIFACT_VERSION,
            repo: source.repo.clone(),
            module: name.to_string(),
            base_url,
            commit: source.commit.clone(),
            pages: source
                .pages
                .iter()
                .map(|(path, entry)| {
                    let page = MetaPage {
                        title: entry.title.clone(),
                        doc_type: entry.doc_type.clone(),
                        section: entry.section.clone(),
                    };
                    (path.clone(), page)
                })
                .collect(),
            residue: source.residue.clone(),
            unresolved: source.unresolved.clone(),
            unrendered: source.unrendered.clone(),
        }
    }
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> ArtifactError + '_ {
    move |source| ArtifactError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Empty `dir`, refusing to delete anything that does not look like an artifact.
pub fn prepare_dir(dir: &Path) -> Result<(), ArtifactError> {
    if dir.exists() {
        let mut entries = std::fs::read_dir(dir).map_err(io(dir))?;
        let empty = entries.next().is_none();
        if !empty && !dir.join(MANIFEST_FILE).is_file() {
            return Err(ArtifactError::NotAnArtifact(dir.to_path_buf()));
        }
        std::fs::remove_dir_all(dir).map_err(io(dir))?;
    }
    std::fs::create_dir_all(dir).map_err(io(dir))
}

fn copy_checked(
    from: &Path,
    to: &Path,
    expected: Option<(&str, &str)>,
) -> Result<(), ArtifactError> {
    let bytes = std::fs::read(from).map_err(io(from))?;
    if let Some((id, expected)) = expected {
        let actual = sha256_hex(&bytes);
        if actual != expected {
            return Err(ArtifactError::HashMismatch {
                id: id.to_string(),
                expected: expected.to_string(),
                actual,
            });
        }
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(io(parent))?;
    }
    std::fs::write(to, bytes).map_err(io(to))
}

/// Write the artifact for `manifest` from the given checkouts (by source name).
///
/// Page bytes are checked against the manifest hashes, so materialising from a manifest whose
/// upstream content changed fails instead of silently producing a different corpus.
pub fn materialise(
    dir: &Path,
    manifest: &Manifest,
    checkouts: &BTreeMap<String, Checkout>,
) -> Result<(), ArtifactError> {
    prepare_dir(dir)?;
    for (name, source) in &manifest.sources {
        let checkout = checkouts
            .get(name)
            .ok_or_else(|| ArtifactError::MissingCheckout(name.clone()))?;
        let source_dir = dir.join(name);
        std::fs::create_dir_all(&source_dir).map_err(io(&source_dir))?;
        for (path, entry) in &source.pages {
            let id = page_id(name, path);
            copy_checked(
                &checkout.root.join(path),
                &source_dir.join(path),
                Some((&id, &entry.sha256)),
            )?;
        }
        let meta = Meta::from_manifest(name, source);
        std::fs::write(source_dir.join(META_FILE), to_sorted_json(&meta)?)
            .map_err(io(&source_dir))?;
        for path in &source.residue {
            let from = checkout.root.join(path);
            if from.is_file() {
                copy_checked(&from, &dir.join(RESIDUE_DIR).join(name).join(path), None)?;
            }
        }
    }
    manifest.save(&dir.join(MANIFEST_FILE))?;
    write_derived(dir, manifest)
}

/// The text of `derived.jsonl` for `manifest`: `None` when nothing was derived, so an artifact
/// without derived text has no such file.
fn derived_text(manifest: &Manifest) -> Option<String> {
    let lines = manifest.derived_lines();
    if lines.is_empty() {
        return None;
    }
    jsonl::to_string(&lines, KeyOrder::Sorted).ok()
}

/// Write (or, when the manifest has no derived text, remove) `<dir>/derived.jsonl` from
/// `manifest` (SPEC §14.4). It is a pure function of the manifest, so `resolve --from-manifest`
/// rebuilds it byte for byte without a model.
pub fn write_derived(dir: &Path, manifest: &Manifest) -> Result<(), ArtifactError> {
    let path = dir.join(DERIVED_FILE);
    match derived_text(manifest) {
        Some(text) => std::fs::write(&path, text).map_err(io(&path)),
        None => match std::fs::remove_file(&path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(io(&path)(err)),
            _ => Ok(()),
        },
    }
}

/// A discrepancy between an artifact directory and a manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The artifact has no `manifest.json` or it differs from the given manifest.
    ManifestDiffers,
    /// A page listed in the manifest is missing from the artifact.
    MissingPage(String),
    /// A page's bytes differ from the manifest hash.
    HashMismatch(String),
    /// A source's `meta.json` is missing or differs from what the manifest implies.
    MetaDiffers(String),
    /// `derived.jsonl` is missing, present without derived text in the manifest, or differs
    /// from what the manifest implies.
    DerivedDiffers,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::ManifestDiffers => {
                write!(f, "artifact manifest.json differs from the manifest")
            }
            Problem::MissingPage(id) => write!(f, "{id}: missing from the artifact"),
            Problem::HashMismatch(id) => {
                write!(f, "{id}: artifact bytes do not match the manifest")
            }
            Problem::MetaDiffers(name) => write!(f, "{name}/meta.json differs from the manifest"),
            Problem::DerivedDiffers => write!(f, "derived.jsonl differs from the manifest"),
        }
    }
}

/// Compare an artifact directory against `manifest`.
pub fn check(dir: &Path, manifest: &Manifest) -> Vec<Problem> {
    let mut problems = Vec::new();
    let expected_manifest = manifest.to_json().ok();
    let actual_manifest = std::fs::read_to_string(dir.join(MANIFEST_FILE)).ok();
    if expected_manifest.is_none() || expected_manifest != actual_manifest {
        problems.push(Problem::ManifestDiffers);
    }
    for (name, source) in &manifest.sources {
        for (path, entry) in &source.pages {
            let id = page_id(name, path);
            match std::fs::read(dir.join(name).join(path)) {
                Ok(bytes) if sha256_hex(&bytes) == entry.sha256 => {}
                Ok(_) => problems.push(Problem::HashMismatch(id)),
                Err(_) => problems.push(Problem::MissingPage(id)),
            }
        }
        let expected_meta = to_sorted_json(&Meta::from_manifest(name, source)).ok();
        let actual_meta = std::fs::read_to_string(dir.join(name).join(META_FILE)).ok();
        if expected_meta != actual_meta {
            problems.push(Problem::MetaDiffers(name.clone()));
        }
    }
    if derived_text(manifest) != std::fs::read_to_string(dir.join(DERIVED_FILE)).ok() {
        problems.push(Problem::DerivedDiffers);
    }
    problems
}

/// Every regular file under `dir` with its bytes, keyed by `/`-separated relative path.
pub fn snapshot(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>, ArtifactError> {
    let files = crate::sources::list_files(dir).map_err(|e| ArtifactError::Io {
        path: dir.to_path_buf(),
        source: std::io::Error::other(e.to_string()),
    })?;
    files
        .into_iter()
        .map(|rel| {
            let full = dir.join(&rel);
            std::fs::read(&full)
                .map(|bytes| (rel, bytes))
                .map_err(io(&full))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{PageEntry, SelectedBy};
    use std::fs;

    const SHA: &str = "4427d7ba863973c2cea9da74ed8675c5c74aee77";

    fn fixture() -> (tempfile::TempDir, Manifest, BTreeMap<String, Checkout>) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("checkout");
        for (path, content) in [
            ("docs/user/README.md", "# Handbook\n"),
            ("docs/user/left.md", "# Left\n"),
        ] {
            let full = root.join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, content).unwrap();
        }
        let mut manifest = Manifest::new("2026-09-16T12:00:00Z".to_string());
        let mut pages = BTreeMap::new();
        pages.insert(
            "docs/user/README.md".to_string(),
            PageEntry {
                sha256: sha256_hex(b"# Handbook\n"),
                title: "Handbook".to_string(),
                doc_type: "concept".to_string(),
                section: String::new(),
                selected_by: SelectedBy::Include,
                rendered_from: None,
            },
        );
        manifest.sources.insert(
            "handbook".to_string(),
            ManifestSource {
                repo: "example-org/handbook".to_string(),
                repo_url: "https://github.com/example-org/handbook.git".to_string(),
                git_ref: "main".to_string(),
                commit: SHA.to_string(),
                archived: Some(false),
                resolver: "glob".to_string(),
                pages,
                residue: vec!["docs/user/left.md".to_string()],
                unresolved: vec!["docs/user/ghost.md".to_string()],
                unrendered: vec![],
                render: None,
            },
        );
        let mut checkouts = BTreeMap::new();
        checkouts.insert(
            "handbook".to_string(),
            Checkout {
                root,
                commit: SHA.to_string(),
            },
        );
        (dir, manifest, checkouts)
    }

    #[test]
    fn materialises_the_spec_layout() {
        let (dir, manifest, checkouts) = fixture();
        let artifact = dir.path().join("artifact");
        materialise(&artifact, &manifest, &checkouts).unwrap();
        let files: Vec<String> = snapshot(&artifact).unwrap().into_keys().collect();
        assert_eq!(
            files,
            [
                "_residue/handbook/docs/user/left.md",
                "handbook/docs/user/README.md",
                "handbook/meta.json",
                "manifest.json",
            ]
        );
        let meta = fs::read_to_string(artifact.join("handbook/meta.json")).unwrap();
        let expected = format!(
            "{{\n  \"artifact_version\": 1,\n  \
             \"base_url\": \"https://github.com/example-org/handbook/blob/{SHA}\",\n  \
             \"commit\": \"{SHA}\",\n  \"module\": \"handbook\",\n  \"pages\": {{\n    \
             \"docs/user/README.md\": {{\n      \"doc_type\": \"concept\",\n      \
             \"section\": \"\",\n      \"title\": \"Handbook\"\n    }}\n  }},\n  \
             \"repo\": \"example-org/handbook\",\n  \"residue\": [\n    \"docs/user/left.md\"\n  ],\n  \
             \"unrendered\": [],\n  \
             \"unresolved\": [\n    \"docs/user/ghost.md\"\n  ]\n}}\n"
        );
        assert_eq!(meta, expected);
        assert_eq!(
            fs::read_to_string(artifact.join("manifest.json")).unwrap(),
            manifest.to_json().unwrap()
        );
        assert!(check(&artifact, &manifest).is_empty());
    }

    #[test]
    fn check_reports_missing_and_changed_pages() {
        let (dir, manifest, checkouts) = fixture();
        let artifact = dir.path().join("artifact");
        materialise(&artifact, &manifest, &checkouts).unwrap();
        fs::write(artifact.join("handbook/docs/user/README.md"), "changed").unwrap();
        fs::write(artifact.join("handbook/meta.json"), "{}").unwrap();
        let problems = check(&artifact, &manifest);
        assert_eq!(
            problems,
            [
                Problem::HashMismatch("handbook::docs/user/README.md".to_string()),
                Problem::MetaDiffers("handbook".to_string())
            ]
        );
        fs::remove_file(artifact.join("handbook/docs/user/README.md")).unwrap();
        fs::remove_file(artifact.join("manifest.json")).unwrap();
        let problems = check(&artifact, &manifest);
        assert_eq!(problems[0], Problem::ManifestDiffers);
        assert_eq!(
            problems[1],
            Problem::MissingPage("handbook::docs/user/README.md".to_string())
        );
        assert_eq!(
            problems[1].to_string(),
            "handbook::docs/user/README.md: missing from the artifact"
        );
    }

    #[test]
    fn derived_text_is_written_from_the_manifest_and_checked() {
        let (dir, mut manifest, checkouts) = fixture();
        let artifact = dir.path().join("artifact");
        let file = artifact.join("derived.jsonl");

        // No derived text: no file, and a stray one is a difference.
        materialise(&artifact, &manifest, &checkouts).unwrap();
        assert!(!file.exists());
        fs::write(&file, "stray\n").unwrap();
        assert_eq!(check(&artifact, &manifest), [Problem::DerivedDiffers]);
        write_derived(&artifact, &manifest).unwrap();
        assert!(!file.exists(), "removed when the manifest has none");
        write_derived(&artifact, &manifest).unwrap();

        // Derived text: the file follows the manifest, and a missing or edited one differs.
        let mut kinds = BTreeMap::new();
        kinds.insert(
            crate::manifest::DERIVED_QUESTIONS.to_string(),
            crate::manifest::DerivedEntry {
                input_sha256: "aa".to_string(),
                model: "m".to_string(),
                text: vec!["How do I install it?".to_string()],
            },
        );
        manifest
            .derived
            .insert("handbook::docs/user/README.md".to_string(), kinds);
        materialise(&artifact, &manifest, &checkouts).unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "{\"kind\":\"questions\",\"page\":\"handbook::docs/user/README.md\",\
             \"text\":[\"How do I install it?\"]}\n"
        );
        assert!(check(&artifact, &manifest).is_empty());
        fs::write(&file, "{}\n").unwrap();
        assert_eq!(check(&artifact, &manifest), [Problem::DerivedDiffers]);
        fs::remove_file(&file).unwrap();
        assert_eq!(check(&artifact, &manifest), [Problem::DerivedDiffers]);
        assert_eq!(
            Problem::DerivedDiffers.to_string(),
            "derived.jsonl differs from the manifest"
        );
    }

    #[test]
    fn refuses_hash_mismatch_and_foreign_directories() {
        let (dir, mut manifest, checkouts) = fixture();
        let artifact = dir.path().join("artifact");
        manifest
            .sources
            .get_mut("handbook")
            .unwrap()
            .pages
            .get_mut("docs/user/README.md")
            .unwrap()
            .sha256 = "00".repeat(32);
        let err = materialise(&artifact, &manifest, &checkouts).unwrap_err();
        assert!(matches!(err, ArtifactError::HashMismatch { .. }), "{err}");

        let foreign = dir.path().join("foreign");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("keep.txt"), "x").unwrap();
        let err = prepare_dir(&foreign).unwrap_err();
        assert!(matches!(err, ArtifactError::NotAnArtifact(_)), "{err}");
        assert!(foreign.join("keep.txt").exists());

        // The failed materialise left a half-written directory without a manifest.
        let err = materialise(&artifact, &manifest, &checkouts).unwrap_err();
        assert!(matches!(err, ArtifactError::NotAnArtifact(_)), "{err}");
        let fresh = dir.path().join("fresh");
        let err = materialise(&fresh, &manifest, &BTreeMap::new()).unwrap_err();
        assert!(matches!(err, ArtifactError::MissingCheckout(_)), "{err}");
    }

    #[test]
    fn rematerialising_replaces_the_previous_artifact() {
        let (dir, manifest, checkouts) = fixture();
        let artifact = dir.path().join("artifact");
        materialise(&artifact, &manifest, &checkouts).unwrap();
        fs::write(artifact.join("stale.md"), "old").unwrap();
        materialise(&artifact, &manifest, &checkouts).unwrap();
        assert!(!artifact.join("stale.md").exists());
    }
}
