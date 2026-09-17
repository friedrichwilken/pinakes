use crate::decisions;
use crate::error::CommandError;
use crate::residue::{self, ListFilter, ResidueEntry};
use crate::workspace::Paths;

/// Run `residue list`: entries matching the filter that have no active decision.
pub fn residue_list(
    paths: &Paths,
    filter: &ListFilter<'_>,
) -> Result<Vec<ResidueEntry>, CommandError> {
    let entries = residue::read_jsonl(&paths.residue)?;
    let effective = decisions::effective(&decisions::read_jsonl(&paths.decisions)?);
    Ok(residue::list(&entries, filter, &effective)
        .into_iter()
        .cloned()
        .collect())
}
