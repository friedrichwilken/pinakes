use std::path::PathBuf;

use crate::error::CommandError;
use crate::manifest::Manifest;
use crate::trail::{self, TrailEntry};
use crate::usage::{self, Usage};
use crate::workspace::Paths;

/// Options for `usage` (SPEC §15.3).
#[derive(Debug, Clone)]
pub struct UsageOptions {
    /// `trail.jsonl` to read.
    pub trail: PathBuf,
    /// `--since` (e.g. `30d`); `None` uses the whole trail.
    pub since: Option<String>,
    /// Write the report here instead of returning it for the caller to print.
    pub json: Option<PathBuf>,
}

/// Run `usage`: pages never retrieved, retrieved-never-cited pages, and uncited queries with
/// their best residue gap candidate, over a trail window (SPEC §15.3).
pub fn usage(paths: &Paths, options: &UsageOptions) -> Result<Usage, CommandError> {
    let manifest = Manifest::load(&paths.manifest)?;
    let entries = trail::read_jsonl(&options.trail)?;
    let since_seconds = options
        .since
        .as_deref()
        .map(usage::parse_since)
        .transpose()?;
    let now = jiff::Timestamp::now();
    let filtered: Vec<TrailEntry> = usage::filter_since(&entries, since_seconds, now);
    let residue_index = usage::ResidueIndex::build(&paths.artifact);
    let report = usage::compute(&manifest, &filtered, &residue_index);
    if let Some(path) = &options.json {
        report.save(path)?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::pipeline::resolve;
    use crate::pipeline::testing::{CONFIG, fetcher, opts, workspace};
    use crate::usage::UsageError;

    #[test]
    fn usage_reports_never_retrieved_and_uncited_queries_against_the_manifest() {
        let (dir, paths) = workspace(CONFIG);
        let outcome = resolve(&paths, &opts(), &fetcher()).unwrap();
        assert!(outcome.manifest.sources["handbook"].pages.len() >= 2);

        let trail_path = dir.path().join("trail.jsonl");
        fs::write(
            &trail_path,
            "{\"at\": \"2026-09-16T12:00:00Z\", \"query\": \"a\", \
             \"retrieved\": [\"handbook::docs/a.md\"], \"cited\": [\"handbook::docs/a.md\"]}\n\
             {\"at\": \"2026-09-16T12:00:00Z\", \"query\": \"b\", \
             \"retrieved\": [\"handbook::docs/b.md\"], \"cited\": []}\n",
        )
        .unwrap();

        let options = UsageOptions {
            trail: trail_path,
            since: None,
            json: None,
        };
        let report = usage(&paths, &options).unwrap();
        assert!(
            report
                .retrieved_never_cited
                .contains(&"handbook::docs/b.md".to_string())
        );
        assert_eq!(report.uncited_queries.len(), 1);
        assert_eq!(report.uncited_queries[0].query, "b");
        assert_eq!(
            report.uncited_queries[0].top_retrieved,
            Some("handbook::docs/b.md".to_string())
        );
    }

    #[test]
    fn usage_rejects_a_malformed_since() {
        let (dir, paths) = workspace(CONFIG);
        resolve(&paths, &opts(), &fetcher()).unwrap();
        let trail_path = dir.path().join("trail.jsonl");
        fs::write(&trail_path, "").unwrap();
        let options = UsageOptions {
            trail: trail_path,
            since: Some("not-a-duration".to_string()),
            json: None,
        };
        assert!(matches!(
            usage(&paths, &options).unwrap_err(),
            CommandError::Usage(UsageError::BadSince(_))
        ));
    }
}
