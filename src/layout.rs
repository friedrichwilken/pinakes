//! File and directory names of the artifact layout (SPEC §2.3).
//!
//! This module depends on nothing else in the crate, so readers of an artifact (`index`, `usage`,
//! `embed`) can name its files without importing `artifact`, which re-exports these names.

/// Name of the residue directory at the artifact root.
pub const RESIDUE_DIR: &str = "_residue";
/// Name of the per-source metadata file.
pub const META_FILE: &str = "meta.json";
/// Name of the manifest at the artifact root.
pub const MANIFEST_FILE: &str = "manifest.json";
