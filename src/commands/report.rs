use std::path::PathBuf;

use crate::decisions;
use crate::duplicates;
use crate::error::CommandError;
use crate::eval::EvalSummary;
use crate::manifest::Manifest;
use crate::page::PageRegistry;
use crate::report::{self, ReportInput};
use crate::residue;
use crate::sources::Fetcher;
use crate::usage::Usage;
use crate::workspace::Paths;

use super::diff::{DiffOptions, diff};

/// Inputs for `report`, as file paths; `None` falls back to the workspace defaults or nothing.
#[derive(Debug, Clone, Default)]
pub struct ReportOptions {
    /// The previous manifest.
    pub old: Option<PathBuf>,
    /// The current manifest (default: the committed one).
    pub new: Option<PathBuf>,
    /// Eval result on the previous corpus.
    pub eval_before: Option<PathBuf>,
    /// Eval result on the current corpus.
    pub eval_after: Option<PathBuf>,
    /// Read new page content from here for changed-page line counts (SPEC §13); typically the
    /// artifact next to the config.
    pub new_artifact: Option<PathBuf>,
    /// Read old page content from here before re-fetching it, as in [`DiffOptions`].
    pub old_artifact: Option<PathBuf>,
    /// A `pinakes usage --json` report to render as the "Usage" section (SPEC §15.3).
    pub usage: Option<PathBuf>,
}

/// Run `report`: render the Markdown PR body from manifests, residue, decisions, duplicates,
/// eval and usage files. `fetcher` is only used, per [`diff()`], to re-fetch a changed page's
/// old text when `options.old_artifact` does not already have it.
pub fn report(
    paths: &Paths,
    options: &ReportOptions,
    fetcher: &dyn Fetcher,
) -> Result<String, CommandError> {
    let new = Manifest::load(options.new.as_deref().unwrap_or(&paths.manifest))?;
    let old = options.old.as_deref().map(Manifest::load).transpose()?;
    let residue = if paths.residue.is_file() {
        residue::read_jsonl(&paths.residue)?
    } else {
        Vec::new()
    };
    let decisions = decisions::read_jsonl(&paths.decisions)?;
    let duplicate_pairs = duplicates::read_jsonl(&paths.duplicates)?;
    let eval_before = options
        .eval_before
        .as_deref()
        .map(EvalSummary::load)
        .transpose()?;
    let eval_after = options
        .eval_after
        .as_deref()
        .map(EvalSummary::load)
        .transpose()?;
    let usage = options.usage.as_deref().map(Usage::load).transpose()?;
    let computed_diff = old
        .as_ref()
        .map(|old| {
            let diff_options = DiffOptions {
                new_artifact: options.new_artifact.clone(),
                old_artifact: options.old_artifact.clone(),
            };
            diff(old, &new, &diff_options, fetcher)
        })
        .transpose()?;
    let registry = PageRegistry::load(Some(&new), &residue);
    Ok(report::render(ReportInput {
        old: old.as_ref(),
        new: &new,
        diff: computed_diff.as_ref(),
        registry: &registry,
        decisions: &decisions,
        eval_before: eval_before.as_ref(),
        eval_after: eval_after.as_ref(),
        duplicates: &duplicate_pairs,
        usage: usage.as_ref(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::resolve;
    use crate::pipeline::testing::{CONFIG, fetcher, opts, workspace};

    #[test]
    fn report_reads_the_workspace_files() {
        let (_dir, paths) = workspace(CONFIG);
        resolve(&paths, &opts(), &fetcher()).unwrap();
        let text = report(&paths, &ReportOptions::default(), &fetcher()).unwrap();
        assert!(text.starts_with("# Corpus report\n"));
        assert!(text.contains("- Pages: 2\n"));
        assert!(text.contains("## Duplicates\n\n_none_\n"));
        let options = ReportOptions {
            old: Some(paths.manifest.clone()),
            eval_after: Some(paths.config_dir().join("missing-eval.json")),
            ..ReportOptions::default()
        };
        assert!(matches!(
            report(&paths, &options, &fetcher()).unwrap_err(),
            CommandError::Eval(_)
        ));
    }
}
