//! `manifest.json`: the machine-written record of curated references (SPEC §2.2).
//!
//! The manifest is committed to git, so it is written with sorted keys, two-space indentation
//! and a trailing newline to keep diffs readable. Page identity everywhere is
//! `<source name>::<path>` (see [`page_id`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::Render;

/// The only manifest version written or read by this iteration.
pub const MANIFEST_VERSION: u32 = 1;

/// Errors raised while reading or writing a manifest.
#[derive(Debug, Error)]
pub enum ManifestError {
    /// The file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The manifest path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The file is not valid manifest JSON.
    #[error("{path}: invalid manifest: {source}")]
    Json {
        /// The manifest path.
        path: PathBuf,
        /// Underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// The manifest version is not supported.
    #[error("unsupported manifest version {0}; expected {MANIFEST_VERSION}")]
    Version(u32),
}

/// The curated references for every source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Manifest format version; equals [`MANIFEST_VERSION`].
    pub version: u32,
    /// RFC 3339 UTC time the manifest was generated.
    pub generated_at: String,
    /// Sources by name.
    pub sources: BTreeMap<String, ManifestSource>,
}

/// One source as recorded in the manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestSource {
    /// `owner/repo` slug.
    pub repo: String,
    /// Clone URL as configured.
    pub repo_url: String,
    /// The configured ref (branch, tag or SHA).
    #[serde(rename = "ref")]
    pub git_ref: String,
    /// The commit the ref resolved to.
    pub commit: String,
    /// Whether the repository is archived upstream; `null` when unknown.
    #[serde(default)]
    pub archived: Option<bool>,
    /// Resolver kind, `glob` or `external`.
    pub resolver: String,
    /// Selected pages by relative path.
    pub pages: BTreeMap<String, PageEntry>,
    /// Relative paths of pages left out (see `residue.jsonl` for why).
    #[serde(default)]
    pub residue: Vec<String>,
    /// Navigation links the resolver reported that have no file.
    #[serde(default)]
    pub unresolved: Vec<String>,
    /// Selected paths a render step (SPEC §10.1) produced no page for.
    #[serde(default)]
    pub unrendered: Vec<String>,
    /// The render step (SPEC §10.1) that ran over this source's selected pages, absent when
    /// none did. External command paths are recorded as they were actually run (absolutised
    /// against the config file at resolve time), so `resolve --from-manifest` can re-run the
    /// exact program regardless of where the manifest is reproduced from (SPEC §2.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render: Option<Render>,
}

/// A selected page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageEntry {
    /// Hex SHA-256 of the file bytes.
    pub sha256: String,
    /// Page title (nav title, first H1 or frontmatter `title:`; may be empty).
    #[serde(default)]
    pub title: String,
    /// Document type as reported by the resolver; may be empty.
    #[serde(default)]
    pub doc_type: String,
    /// Navigation section as reported by the resolver; may be empty.
    #[serde(default)]
    pub section: String,
    /// What selected the page.
    pub selected_by: SelectedBy,
    /// The selected path this page was rendered from (SPEC §10.1), when it is a rendered page
    /// rather than a copy of the selected file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered_from: Option<String>,
}

/// What caused a page to be selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SelectedBy {
    /// An external resolver reported `selected: true`.
    Resolver,
    /// A glob include pattern matched.
    Include,
    /// A decision with verdict `include`.
    Decision,
}

/// Build the page id `<source>::<path>`.
pub fn page_id(source: &str, path: &str) -> String {
    format!("{source}::{path}")
}

/// Split a page id into `(source, path)`.
pub fn split_page_id(id: &str) -> Option<(&str, &str)> {
    id.split_once("::")
        .filter(|(s, p)| !s.is_empty() && !p.is_empty())
}

/// Serialise any value as JSON with sorted keys, two-space indent and a trailing newline.
///
/// Going through [`serde_json::Value`] (whose objects are ordered maps) is what sorts the keys.
pub fn to_sorted_json<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let value = serde_json::to_value(value)?;
    let mut text = serde_json::to_string_pretty(&value)?;
    text.push('\n');
    Ok(text)
}

/// The current time as RFC 3339 UTC with second precision, e.g. `2026-09-16T12:00:00Z`.
pub fn now_rfc3339() -> String {
    jiff::Timestamp::now()
        .strftime("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

impl Manifest {
    /// A manifest with no sources.
    pub fn new(generated_at: String) -> Manifest {
        Manifest {
            version: MANIFEST_VERSION,
            generated_at,
            sources: BTreeMap::new(),
        }
    }

    /// Read a manifest file.
    pub fn load(path: &Path) -> Result<Manifest, ManifestError> {
        let text = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let manifest: Manifest =
            serde_json::from_str(&text).map_err(|source| ManifestError::Json {
                path: path.to_path_buf(),
                source,
            })?;
        if manifest.version != MANIFEST_VERSION {
            return Err(ManifestError::Version(manifest.version));
        }
        Ok(manifest)
    }

    /// Write the manifest with sorted keys, two-space indent and a trailing newline.
    pub fn save(&self, path: &Path) -> Result<(), ManifestError> {
        let text = self.to_json().map_err(|source| ManifestError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        std::fs::write(path, text).map_err(|source| ManifestError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    /// The canonical JSON text of this manifest.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        to_sorted_json(self)
    }

    /// Every page as `(id, source name, path, entry)` in sorted order.
    pub fn pages(&self) -> impl Iterator<Item = (String, &str, &str, &PageEntry)> {
        self.sources.iter().flat_map(|(name, source)| {
            source.pages.iter().map(move |(path, entry)| {
                (page_id(name, path), name.as_str(), path.as_str(), entry)
            })
        })
    }

    /// Look up a page by id.
    pub fn page(&self, id: &str) -> Option<&PageEntry> {
        let (source, path) = split_page_id(id)?;
        self.sources.get(source)?.pages.get(path)
    }

    /// The set of residue ids across all sources.
    pub fn residue_ids(&self) -> impl Iterator<Item = String> {
        self.sources
            .iter()
            .flat_map(|(name, source)| source.residue.iter().map(move |p| page_id(name, p)))
    }

    /// The upstream URL of page `id`, pinned to its source's fetched commit (SPEC §2.2), or
    /// `None` when the source is unknown or its `repo` does not parse as `owner/repo`.
    pub fn page_url(&self, id: &str) -> Option<String> {
        let (source, path) = split_page_id(id)?;
        self.sources.get(source)?.page_url(path)
    }
}

impl ManifestSource {
    /// The upstream URL of `path` in this source, pinned to its fetched commit: `{base_url}/
    /// {path}` where `base_url` is `https://github.com/<owner>/<repo>/blob/<commit>` (SPEC
    /// §2.3), or `None` when `repo` does not parse as `owner/repo`.
    pub fn page_url(&self, path: &str) -> Option<String> {
        crate::config::RepoSlug::from_slug(&self.repo)
            .map(|slug| format!("{}/{path}", slug.blob_base_url(&self.commit)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        let mut manifest = Manifest::new("2026-09-16T12:00:00Z".to_string());
        let mut pages = BTreeMap::new();
        pages.insert(
            "docs/user/README.md".to_string(),
            PageEntry {
                sha256: "ab".repeat(32),
                title: "Handbook".to_string(),
                doc_type: "concept".to_string(),
                section: String::new(),
                selected_by: SelectedBy::Include,
                rendered_from: None,
            },
        );
        pages.insert(
            "docs/user/00-10-overview.md".to_string(),
            PageEntry {
                sha256: "cd".repeat(32),
                title: "Overview".to_string(),
                doc_type: String::new(),
                section: "Intro".to_string(),
                selected_by: SelectedBy::Decision,
                rendered_from: None,
            },
        );
        manifest.sources.insert(
            "handbook".to_string(),
            ManifestSource {
                repo: "example-org/handbook".to_string(),
                repo_url: "https://github.com/example-org/handbook.git".to_string(),
                git_ref: "main".to_string(),
                commit: "4427d7ba863973c2cea9da74ed8675c5c74aee77".to_string(),
                archived: Some(false),
                resolver: "glob".to_string(),
                pages,
                residue: vec!["docs/user/00-15-overview-setup.md".to_string()],
                unresolved: vec![],
                unrendered: vec![],
                render: None,
            },
        );
        manifest
    }

    #[test]
    fn round_trips_through_a_file() {
        let manifest = sample();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.json");
        manifest.save(&path).unwrap();
        let loaded = Manifest::load(&path).unwrap();
        assert_eq!(loaded, manifest);
        assert_eq!(
            loaded.to_json().unwrap(),
            std::fs::read_to_string(&path).unwrap()
        );
    }

    #[test]
    fn json_is_sorted_two_space_indented_with_trailing_newline() {
        let text = sample().to_json().unwrap();
        assert!(text.ends_with("}\n"), "trailing newline");
        assert!(
            text.starts_with("{\n  \"generated_at\""),
            "top-level keys sorted: {text}"
        );
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.iter().any(|l| l.starts_with("  \"sources\": {")));
        // Page paths are sorted lexicographically within the source.
        let overview = text.find("00-10-overview.md").unwrap();
        let readme = text.find("README.md").unwrap();
        assert!(overview < readme);
        // Keys inside a page entry are sorted and the ref field is spelled "ref".
        let entry = &text[readme..];
        let doc_type = entry.find("\"doc_type\"").unwrap();
        let sha = entry.find("\"sha256\"").unwrap();
        assert!(doc_type < sha);
        assert!(text.contains("\"ref\": \"main\""));
        assert!(text.contains("\"selected_by\": \"include\""));
        assert!(text.contains("\"archived\": false"));
        assert!(!text.contains('\t'));
    }

    #[test]
    fn unknown_archived_is_null_and_missing_lists_default() {
        let mut manifest = sample();
        manifest.sources.get_mut("handbook").unwrap().archived = None;
        let text = manifest.to_json().unwrap();
        assert!(text.contains("\"archived\": null"));
        let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
        value["sources"]["handbook"]
            .as_object_mut()
            .unwrap()
            .remove("residue");
        let trimmed = value.to_string();
        let parsed: Manifest = serde_json::from_str(&trimmed).unwrap();
        assert!(parsed.sources["handbook"].residue.is_empty());
        assert_eq!(parsed.sources["handbook"].archived, None);
    }

    #[test]
    fn render_is_absent_by_default_and_present_when_recorded() {
        let mut manifest = sample();
        let text = manifest.to_json().unwrap();
        assert!(
            !text.contains("\"render\""),
            "absent when no render step ran"
        );

        manifest.sources.get_mut("handbook").unwrap().render = Some(Render::Openapi);
        let text = manifest.to_json().unwrap();
        assert!(text.contains("\"render\""));
        assert!(text.contains("\"type\": \"openapi\""));
        let round_tripped: Manifest = serde_json::from_str(&text).unwrap();
        assert_eq!(
            round_tripped.sources["handbook"].render,
            Some(Render::Openapi)
        );

        manifest.sources.get_mut("handbook").unwrap().render = Some(Render::External {
            command: vec!["/abs/render.py".to_string()],
            args: vec!["--flag".to_string()],
        });
        let text = manifest.to_json().unwrap();
        assert!(text.contains("\"type\": \"external\""));
        assert!(text.contains("\"/abs/render.py\""));
        assert!(text.contains("\"--flag\""));
        let round_tripped: Manifest = serde_json::from_str(&text).unwrap();
        assert_eq!(
            round_tripped.sources["handbook"].render,
            manifest.sources["handbook"].render
        );
    }

    #[test]
    fn page_ids_split_and_look_up() {
        assert_eq!(page_id("handbook", "docs/x.md"), "handbook::docs/x.md");
        assert_eq!(
            split_page_id("handbook::docs/x.md"),
            Some(("handbook", "docs/x.md"))
        );
        assert_eq!(split_page_id("handbook::"), None);
        assert_eq!(split_page_id("nope"), None);
        let manifest = sample();
        assert_eq!(
            manifest
                .page("handbook::docs/user/README.md")
                .unwrap()
                .title,
            "Handbook"
        );
        assert!(manifest.page("handbook::missing.md").is_none());
        let ids: Vec<String> = manifest.pages().map(|(id, ..)| id).collect();
        assert_eq!(
            ids,
            [
                "handbook::docs/user/00-10-overview.md",
                "handbook::docs/user/README.md"
            ]
        );
        let residue: Vec<String> = manifest.residue_ids().collect();
        assert_eq!(residue, ["handbook::docs/user/00-15-overview-setup.md"]);
    }

    #[test]
    fn rejects_unknown_versions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.json");
        std::fs::write(
            &path,
            "{\"version\": 2, \"generated_at\": \"\", \"sources\": {}}",
        )
        .unwrap();
        assert!(matches!(
            Manifest::load(&path).unwrap_err(),
            ManifestError::Version(2)
        ));
        std::fs::write(&path, "not json").unwrap();
        assert!(matches!(
            Manifest::load(&path).unwrap_err(),
            ManifestError::Json { .. }
        ));
        assert!(matches!(
            Manifest::load(&dir.path().join("missing.json")).unwrap_err(),
            ManifestError::Io { .. }
        ));
    }

    #[test]
    fn page_url_is_derived_from_repo_and_commit_or_none_when_the_slug_does_not_parse() {
        let manifest = sample();
        assert_eq!(
            manifest.page_url("handbook::docs/user/README.md").unwrap(),
            format!(
                "https://github.com/example-org/handbook/blob/{}/docs/user/README.md",
                "4427d7ba863973c2cea9da74ed8675c5c74aee77"
            )
        );
        assert_eq!(
            manifest.page_url("handbook::missing.md").unwrap(),
            format!(
                "https://github.com/example-org/handbook/blob/{}/missing.md",
                "4427d7ba863973c2cea9da74ed8675c5c74aee77"
            ),
            "derived for any path in a known source, not only an existing page \
             (residue, duplicate and unresolved-link urls point at paths the manifest itself \
             does not list)"
        );
        assert_eq!(manifest.page_url("nope::x.md"), None, "unknown source");

        let mut bad = manifest;
        bad.sources.get_mut("handbook").unwrap().repo = "not a slug".to_string();
        assert_eq!(bad.page_url("handbook::docs/user/README.md"), None);
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        let now = now_rfc3339();
        assert_eq!(now.len(), 20, "{now}");
        assert!(now.ends_with('Z'));
        assert_eq!(&now[10..11], "T");
    }
}
