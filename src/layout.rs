//! File and directory names of the artifact layout (SPEC §2.3) and the artifact contract
//! version (SPEC §2.8).
//!
//! This module depends on nothing else in the crate, so readers of an artifact (`index`, `usage`,
//! `embed`) can name its files without importing `artifact`, which re-exports these names.

/// Name of the residue directory at the artifact root.
pub const RESIDUE_DIR: &str = "_residue";
/// Name of the per-source metadata file.
pub const META_FILE: &str = "meta.json";
/// Name of the manifest at the artifact root.
pub const MANIFEST_FILE: &str = "manifest.json";

/// The artifact contract version this build writes and the highest one it reads (SPEC §2.8):
/// the directory layout, the `meta.json` fields and the `manifest.json` fields. A missing
/// `artifact_version` field means 1; additive changes stay within a major; a removal or rename
/// is a new major, which older readers reject.
pub const ARTIFACT_VERSION: u32 = 1;

/// An artifact or manifest written by a newer contract version than this build reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "artifact version {0} is newer than this pinakes supports ({ARTIFACT_VERSION}); upgrade pinakes"
)]
pub struct NewerArtifactVersion(pub u32);

/// Accept `found` when it is [`ARTIFACT_VERSION`] or lower, else name the newer version.
pub fn check_artifact_version(found: u32) -> Result<(), NewerArtifactVersion> {
    if found > ARTIFACT_VERSION {
        return Err(NewerArtifactVersion(found));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_current_and_older_versions_and_rejects_newer_ones() {
        assert_eq!(check_artifact_version(ARTIFACT_VERSION), Ok(()));
        assert_eq!(check_artifact_version(0), Ok(()));
        let err = check_artifact_version(ARTIFACT_VERSION + 1).unwrap_err();
        assert_eq!(err, NewerArtifactVersion(ARTIFACT_VERSION + 1));
        assert_eq!(
            err.to_string(),
            "artifact version 2 is newer than this pinakes supports (1); upgrade pinakes"
        );
    }
}
