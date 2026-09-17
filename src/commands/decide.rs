use crate::decisions::{self, Decision, Verdict};
use crate::error::CommandError;
use crate::manifest::{Manifest, now_rfc3339};
use crate::residue;
use crate::workspace::Paths;

/// Run `decide`: append a decision for `id`, taking the hash from residue or the manifest.
pub fn decide(
    paths: &Paths,
    id: &str,
    verdict: Verdict,
    reason: &str,
    by: &str,
    at: Option<String>,
) -> Result<Decision, CommandError> {
    let from_residue = if paths.residue.is_file() {
        residue::read_jsonl(&paths.residue)?
            .into_iter()
            .find(|e| e.id == id && !e.sha256.is_empty())
            .map(|e| e.sha256)
    } else {
        None
    };
    let from_manifest = || -> Result<Option<String>, CommandError> {
        if !paths.manifest.is_file() {
            return Ok(None);
        }
        Ok(Manifest::load(&paths.manifest)?
            .page(id)
            .map(|p| p.sha256.clone()))
    };
    let sha256 = match from_residue {
        Some(hash) => hash,
        None => from_manifest()?.ok_or_else(|| CommandError::UnknownId(id.to_string()))?,
    };
    let decision = Decision {
        id: id.to_string(),
        sha256,
        decision: verdict,
        reason: reason.to_string(),
        by: by.to_string(),
        at: at.unwrap_or_else(now_rfc3339),
    };
    decisions::append(&paths.decisions, &decision)?;
    Ok(decision)
}
