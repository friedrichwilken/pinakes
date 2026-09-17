//! [`Paths`]: the file locations shared by every command.

use std::path::{Path, PathBuf};

/// File locations shared by the commands; every path is taken as given (no implicit cwd magic
/// beyond the defaults the CLI fills in).
#[derive(Debug, Clone)]
pub struct Paths {
    /// `pinakes.yaml`.
    pub config: PathBuf,
    /// The committed `manifest.json`.
    pub manifest: PathBuf,
    /// `residue.jsonl`.
    pub residue: PathBuf,
    /// `decisions.jsonl`.
    pub decisions: PathBuf,
    /// The artifact directory.
    pub artifact: PathBuf,
    /// `duplicates.jsonl`, written by `resolve` next to `residue.jsonl` (SPEC §11).
    pub duplicates: PathBuf,
    /// `embeddings.bin`, written by `embed` (SPEC §16.2); `embeddings.json` sits next to it.
    pub embeddings: PathBuf,
}

impl Paths {
    /// Defaults relative to the config file's directory.
    pub fn for_config(config: &Path) -> Paths {
        let dir = config.parent().map(Path::to_path_buf).unwrap_or_default();
        Paths {
            config: config.to_path_buf(),
            manifest: dir.join("manifest.json"),
            residue: dir.join("residue.jsonl"),
            decisions: dir.join("decisions.jsonl"),
            artifact: dir.join("artifact"),
            duplicates: dir.join("duplicates.jsonl"),
            embeddings: dir.join("embeddings.bin"),
        }
    }

    /// Directory of the config file.
    pub fn config_dir(&self) -> PathBuf {
        self.config
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }
}
