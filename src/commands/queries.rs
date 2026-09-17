use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::CommandError;
use crate::eval;
use crate::grade::GradedRow;
use crate::jsonl;
use crate::manifest::Manifest;
use crate::pipeline::io_err;
use crate::queries::{self, CheckReport, GradedQuery, NewQuery};
use crate::workspace::Paths;

use super::eval::resolve_queries_path;

/// Options for `queries add`.
#[derive(Debug, Clone, Default)]
pub struct QueriesAddOptions {
    /// Query file (default: `eval.queries` from the config, relative to it).
    pub queries: Option<PathBuf>,
    /// Query id.
    pub id: String,
    /// The query text.
    pub query: String,
    /// Page ids, id prefixes, or the legacy `<source>/<path>` form.
    pub expected: Vec<String>,
    /// Query kind, possibly empty.
    pub kind: String,
    /// Held out from tuning decisions.
    pub holdout: bool,
}

/// Run `queries add`: append a row after checking `expected` against the committed manifest.
pub fn queries_add(
    paths: &Paths,
    options: &QueriesAddOptions,
) -> Result<eval::Query, CommandError> {
    let config = if paths.config.is_file() {
        Some(Config::load(&paths.config)?)
    } else {
        None
    };
    let eval_config = config.as_ref().and_then(|c| c.eval.as_ref());
    let queries_path = resolve_queries_path(paths, eval_config, options.queries.as_deref())?;
    let manifest = Manifest::load(&paths.manifest)?;
    let new = NewQuery {
        id: options.id.clone(),
        query: options.query.clone(),
        expected: options.expected.clone(),
        kind: options.kind.clone(),
        holdout: options.holdout,
    };
    Ok(queries::add(&queries_path, &manifest, &new)?)
}

/// Run `queries check`: validate `queries.jsonl` against the committed manifest.
pub fn queries_check(
    paths: &Paths,
    queries_override: Option<&Path>,
) -> Result<CheckReport, CommandError> {
    let config = if paths.config.is_file() {
        Some(Config::load(&paths.config)?)
    } else {
        None
    };
    let eval_config = config.as_ref().and_then(|c| c.eval.as_ref());
    let queries_path = resolve_queries_path(paths, eval_config, queries_override)?;
    let manifest = Manifest::load(&paths.manifest)?;
    let rows = eval::load_queries(&queries_path)?;
    let holdout_min = eval_config.map_or(queries::DEFAULT_HOLDOUT_MIN, |e| e.holdout_min);
    Ok(queries::check(&rows, &manifest, holdout_min))
}

/// Options for `queries import` (SPEC §15.2).
#[derive(Debug, Clone)]
pub struct QueriesImportOptions {
    /// Query file to append to (default: `eval.queries` from the config, relative to it).
    pub queries: Option<PathBuf>,
    /// `graded.jsonl` to read.
    pub graded: PathBuf,
    /// Minimum grade for an id to enter `expected`.
    pub min_grade: u8,
    /// Target held-out share for the random assignment.
    pub holdout_share: f64,
    /// Seed for the deterministic PRNG that assigns `holdout`.
    pub seed: u64,
}

/// What `queries import` produced.
#[derive(Debug)]
pub struct QueriesImportOutcome {
    /// Rows appended to the query file.
    pub imported: Vec<GradedQuery>,
    /// Query texts with no candidate at or above `min_grade` (not appended).
    pub skipped: Vec<String>,
}

/// Run `queries import`: turn `pinakes grade`'s output into query rows (SPEC §15.2).
pub fn queries_import(
    paths: &Paths,
    options: &QueriesImportOptions,
) -> Result<QueriesImportOutcome, CommandError> {
    let config = if paths.config.is_file() {
        Some(Config::load(&paths.config)?)
    } else {
        None
    };
    let eval_config = config.as_ref().and_then(|c| c.eval.as_ref());
    let queries_path = resolve_queries_path(paths, eval_config, options.queries.as_deref())?;
    let graded_text = std::fs::read_to_string(&options.graded).map_err(io_err(&options.graded))?;
    let graded: Vec<GradedRow> = jsonl::parse(&graded_text).map_err(|err| err.source)?;
    let (imported, skipped) = queries::import_graded(
        &graded,
        options.min_grade,
        options.holdout_share,
        options.seed,
    );
    queries::append_graded(&queries_path, &imported)?;
    Ok(QueriesImportOutcome { imported, skipped })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::pipeline::resolve;
    use crate::pipeline::testing::{CONFIG, fetcher, opts, workspace};

    #[test]
    fn queries_add_and_check_validate_against_the_manifest() {
        let (dir, paths) = workspace(CONFIG);
        resolve(&paths, &opts(), &fetcher()).unwrap();
        let queries_path = dir.path().join("queries.jsonl");

        let add_options = QueriesAddOptions {
            queries: Some(queries_path.clone()),
            id: "a".to_string(),
            query: "what is a".to_string(),
            expected: vec!["handbook/docs/a.md".to_string()],
            kind: "howto".to_string(),
            holdout: false,
        };
        let query = queries_add(&paths, &add_options).unwrap();
        assert_eq!(query.expected, ["handbook::docs/a.md"]);

        // An unknown expected id is rejected and nothing is appended.
        let bad = QueriesAddOptions {
            id: "bad".to_string(),
            expected: vec!["handbook::nope.md".to_string()],
            ..add_options.clone()
        };
        assert!(matches!(
            queries_add(&paths, &bad).unwrap_err(),
            CommandError::Queries(_)
        ));
        assert_eq!(eval::load_queries(&queries_path).unwrap().len(), 1);

        // A held-out row so the share check has something to pass.
        let held = QueriesAddOptions {
            id: "b".to_string(),
            query: "what is b".to_string(),
            expected: vec!["handbook::docs/b.md".to_string()],
            holdout: true,
            ..add_options
        };
        queries_add(&paths, &held).unwrap();

        let report = queries_check(&paths, Some(&queries_path)).unwrap();
        assert!(report.ok(), "{report:?}");
        assert!((report.holdout_share - 0.5).abs() < 1e-12);

        // Below the configured minimum: same rows, a stricter holdout_min.
        fs::write(
            &paths.config,
            format!("{CONFIG}eval:\n  queries: queries.jsonl\n  holdout_min: 0.6\n"),
        )
        .unwrap();
        let report = queries_check(&paths, Some(&queries_path)).unwrap();
        assert!(!report.ok());
        assert!((report.holdout_min - 0.6).abs() < 1e-12);
        fs::write(&paths.config, CONFIG).unwrap();

        // A raw duplicate id and an unknown expected id, appended straight to the file.
        let mut text = fs::read_to_string(&queries_path).unwrap();
        text.push_str(
            "{\"id\": \"a\", \"query\": \"dup\", \"expected\": [\"handbook::nope.md\"]}\n",
        );
        fs::write(&queries_path, text).unwrap();
        let report = queries_check(&paths, Some(&queries_path)).unwrap();
        assert!(!report.ok());
        assert_eq!(report.duplicate_ids, ["a"]);
        assert_eq!(
            report.unknown,
            [("a".to_string(), "handbook::nope.md".to_string())]
        );

        assert!(matches!(
            queries_check(&paths, None).unwrap_err(),
            CommandError::NoQueries
        ));
    }

    #[test]
    fn queries_import_appends_rows_from_graded_jsonl() {
        let (dir, paths) = workspace(CONFIG);
        resolve(&paths, &opts(), &fetcher()).unwrap();
        let graded_path = dir.path().join("graded.jsonl");
        fs::write(
            &graded_path,
            "{\"query\": \"a\", \"id\": \"handbook::docs/a.md\", \"grade\": 3, \
             \"model\": \"m\", \"at\": \"t\"}\n",
        )
        .unwrap();
        let queries_path = dir.path().join("queries.jsonl");
        let options = QueriesImportOptions {
            queries: Some(queries_path.clone()),
            graded: graded_path,
            min_grade: 2,
            holdout_share: 0.0,
            seed: 0,
        };
        let outcome = queries_import(&paths, &options).unwrap();
        assert_eq!(outcome.imported.len(), 1);
        assert!(outcome.skipped.is_empty());
        let loaded = eval::load_queries(&queries_path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].expected, ["handbook::docs/a.md"]);
    }
}
