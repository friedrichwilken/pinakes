use std::path::{Path, PathBuf};

use crate::backend::{self, Backend, BackendConfig, BackendError, BackendKind};
use crate::config::Config;
use crate::embed::Embedder;
use crate::error::CommandError;
use crate::eval::{self, Delta, EvalSummary, Gate};
use crate::index::{self, Index, IndexError, Page, Priorities};
use crate::workspace::Paths;

/// Result list length when neither `--k` nor `eval.k` is given.
pub const DEFAULT_K: usize = 10;
/// Gate tolerance when the config has no `eval` section.
pub const DEFAULT_MAX_RECALL_DROP: f64 = 0.05;

/// Options for `eval`.
#[derive(Debug, Clone, Default)]
pub struct EvalOptions {
    /// Query file (default: `eval.queries` from the config, relative to it).
    pub queries: Option<PathBuf>,
    /// Result list length (default: `eval.k` from the config, else [`DEFAULT_K`]).
    pub k: Option<usize>,
    /// Write the result JSON here.
    pub json: Option<PathBuf>,
    /// Baseline result to gate the tuning recall@5 against.
    pub gate: Option<PathBuf>,
    /// Residue pages (`<source>::<path>`) to add from `_residue` before measuring.
    pub with: Vec<String>,
    /// Pages to remove before measuring.
    pub without: Vec<String>,
}

/// What `eval` produced.
#[derive(Debug)]
pub struct EvalOutcome {
    /// The result (after `--with`/`--without` when given).
    pub summary: EvalSummary,
    /// Pages in the measured corpus, mirrors included.
    pub page_count: usize,
    /// Pages in the search corpus.
    pub searchable_count: usize,
    /// Result list length used.
    pub k: usize,
    /// The gate outcome when `--gate` was given.
    pub gate: Option<Gate>,
    /// The delta when `--with`/`--without` was given.
    pub delta: Option<Delta>,
}

/// The query file: `--queries` when given, else `eval.queries` from the config.
pub(super) fn resolve_queries_path(
    paths: &Paths,
    eval_config: Option<&crate::config::EvalConfig>,
    queries: Option<&Path>,
) -> Result<PathBuf, CommandError> {
    match queries {
        Some(path) => Ok(path.to_path_buf()),
        None => eval_config
            .map(|e| paths.config_dir().join(&e.queries))
            .ok_or(CommandError::NoQueries),
    }
}

/// Run `eval`: index the artifact, run the queries, optionally gate against a baseline and
/// measure the effect of adding residue pages or removing pages.
///
/// The config is optional: without one, the query file must be given, every source gets the
/// default priority of [`Priorities`] (so no page is a mirror), `k` defaults to [`DEFAULT_K`]
/// and the gate tolerance to [`DEFAULT_MAX_RECALL_DROP`].
pub fn eval(paths: &Paths, options: &EvalOptions) -> Result<EvalOutcome, CommandError> {
    let config = if paths.config.is_file() {
        Some(Config::load(&paths.config)?)
    } else {
        None
    };
    let eval_config = config.as_ref().and_then(|c| c.eval.as_ref());
    let queries_path = resolve_queries_path(paths, eval_config, options.queries.as_deref())?;
    let k = options
        .k
        .or_else(|| eval_config.map(|e| e.k))
        .unwrap_or(DEFAULT_K);
    let max_drop = eval_config.map_or(DEFAULT_MAX_RECALL_DROP, |e| e.max_recall_drop);
    let priorities = config
        .as_ref()
        .map(Priorities::from_config)
        .unwrap_or_default();
    let queries = eval::load_queries(&queries_path)?;
    let pages = index::load_pages(&paths.artifact, &priorities)?;

    let (summary, page_count, searchable_count, delta) =
        if options.with.is_empty() && options.without.is_empty() {
            let index = Index::from_pages(pages)?;
            let summary = eval::evaluate(&index, &queries, k)?;
            (summary, index.page_count(), index.searchable_count(), None)
        } else {
            let (summary, page_count, searchable_count, delta) = evaluate_adjusted(
                pages,
                &paths.artifact,
                &priorities,
                &queries,
                k,
                &options.with,
                &options.without,
            )?;
            (summary, page_count, searchable_count, Some(delta))
        };
    let gate = match &options.gate {
        Some(path) => Some(eval::gate(&summary, &EvalSummary::load(path)?, max_drop)),
        None => None,
    };
    if let Some(path) = &options.json {
        summary.save(path)?;
    }
    Ok(EvalOutcome {
        summary,
        page_count,
        searchable_count,
        k,
        gate,
        delta,
    })
}

/// Adjust `pages` by `with`/`without`, index it, and return the before/after summaries' delta
/// alongside the after index's counts — the `--with`/`--without` computation shared by [`eval`]
/// and [`eval_backend`]'s `bm25` adjusting branch.
fn evaluate_adjusted(
    pages: Vec<Page>,
    artifact: &Path,
    priorities: &Priorities,
    queries: &[eval::Query],
    k: usize,
    with: &[String],
    without: &[String],
) -> Result<(EvalSummary, usize, usize, Delta), CommandError> {
    let before = eval::evaluate(&Index::from_pages(pages.clone())?, queries, k)?;
    let index = Index::from_pages(adjust_pages(pages, artifact, priorities, with, without)?)?;
    let after = eval::evaluate(&index, queries, k)?;
    let delta = eval::delta(&before, &after);
    Ok((after, index.page_count(), index.searchable_count(), delta))
}

/// Apply `--with` (add residue pages) and `--without` (remove pages) to the page list.
fn adjust_pages(
    mut pages: Vec<Page>,
    artifact: &Path,
    priorities: &Priorities,
    with: &[String],
    without: &[String],
) -> Result<Vec<Page>, CommandError> {
    for id in with {
        if pages.iter().any(|p| &p.id == id) {
            return Err(IndexError::AlreadyPresent(id.clone()).into());
        }
        pages.push(index::load_residue_page(artifact, id, priorities)?);
    }
    for id in without {
        let before = pages.len();
        pages.retain(|p| &p.id != id);
        if pages.len() == before {
            return Err(IndexError::UnknownPage(id.clone()).into());
        }
    }
    Ok(pages)
}

// -------------------------------------------------------------------------------------------
// eval --backend / --compare (SPEC §16.1)
// -------------------------------------------------------------------------------------------

/// Options for `eval --backend` (any backend other than the plain default).
#[derive(Clone, Default)]
pub struct BackendEvalOptions {
    /// Query file (default: `eval.queries` from the config, relative to it).
    pub queries: Option<PathBuf>,
    /// Result list length (default: `eval.k` from the config, else [`DEFAULT_K`]).
    pub k: Option<usize>,
    /// Write the result JSON here.
    pub json: Option<PathBuf>,
    /// Baseline result to gate the tuning recall@5 against.
    pub gate: Option<PathBuf>,
    /// Residue pages to add before measuring; only supported for `bm25`.
    pub with: Vec<String>,
    /// Pages to remove before measuring; only supported for `bm25`.
    pub without: Vec<String>,
    /// The backend to measure.
    pub backend: BackendKind,
    /// The consumer's search endpoint (`external`).
    pub backend_url: Option<String>,
    /// `embeddings.bin` path (`dense`, `hybrid`); default: `paths.embeddings`.
    pub embeddings: Option<PathBuf>,
    /// Use embeddings even when their recorded manifest hash does not match the artifact.
    pub allow_stale: bool,
    /// Where query embeddings come from (`dense`, `hybrid`).
    pub embedder: Option<std::rc::Rc<dyn Embedder>>,
}

/// What `eval --backend` produced.
#[derive(Debug)]
pub struct BackendEvalOutcome {
    /// The result (after `--with`/`--without` when given), with [`EvalSummary::backend`] set.
    pub summary: EvalSummary,
    /// Pages in the measured corpus, mirrors included.
    pub page_count: usize,
    /// Pages in the search corpus.
    pub searchable_count: usize,
    /// Result list length used.
    pub k: usize,
    /// The gate outcome when `--gate` was given.
    pub gate: Option<Gate>,
    /// The delta when `--with`/`--without` was given.
    pub delta: Option<Delta>,
}

/// Run every query against `backend` with a result list of `k` pages.
fn evaluate_backend(
    backend: &dyn Backend,
    queries: &[eval::Query],
    k: usize,
) -> Result<EvalSummary, CommandError> {
    let mut rows = Vec::with_capacity(queries.len());
    for query in queries {
        let top = backend
            .search(&query.query, k, None)?
            .into_iter()
            .map(|hit| hit.page_id)
            .collect();
        rows.push(eval::QueryResult::score(query, top, k));
    }
    Ok(eval::summarise(rows))
}

fn backend_config(
    paths: &Paths,
    options: &BackendEvalOptions,
    priorities: Priorities,
) -> BackendConfig {
    let embeddings_bin = options
        .embeddings
        .clone()
        .unwrap_or_else(|| paths.embeddings.clone());
    let embeddings_json = embeddings_bin.with_extension("json");
    BackendConfig {
        priorities,
        embeddings_bin,
        embeddings_json,
        allow_stale: options.allow_stale,
        embedder: options.embedder.clone(),
        backend_url: options.backend_url.clone(),
    }
}

/// Run `eval --backend NAME`: like [`eval()`], but through the [`Backend`] trait (SPEC §16.1),
/// recording the backend name in the result. `--with`/`--without` only work for `bm25`, which
/// indexes a page list directly; every other backend rejects them with
/// [`BackendError::UnsupportedAdjustment`].
pub fn eval_backend(
    paths: &Paths,
    options: &BackendEvalOptions,
) -> Result<BackendEvalOutcome, CommandError> {
    let config = if paths.config.is_file() {
        Some(Config::load(&paths.config)?)
    } else {
        None
    };
    let eval_config = config.as_ref().and_then(|c| c.eval.as_ref());
    let queries_path = resolve_queries_path(paths, eval_config, options.queries.as_deref())?;
    let k = options
        .k
        .or_else(|| eval_config.map(|e| e.k))
        .unwrap_or(DEFAULT_K);
    let max_drop = eval_config.map_or(DEFAULT_MAX_RECALL_DROP, |e| e.max_recall_drop);
    let priorities = config
        .as_ref()
        .map(Priorities::from_config)
        .unwrap_or_default();
    let queries = eval::load_queries(&queries_path)?;
    let adjusting = !options.with.is_empty() || !options.without.is_empty();
    if adjusting && options.backend != BackendKind::Bm25 {
        return Err(BackendError::UnsupportedAdjustment(options.backend.name().to_string()).into());
    }

    let (summary, page_count, searchable_count, delta) = if adjusting {
        let pages = index::load_pages(&paths.artifact, &priorities)?;
        let (summary, page_count, searchable_count, delta) = evaluate_adjusted(
            pages,
            &paths.artifact,
            &priorities,
            &queries,
            k,
            &options.with,
            &options.without,
        )?;
        (
            summary.with_backend(options.backend.name()),
            page_count,
            searchable_count,
            Some(delta),
        )
    } else {
        let config = backend_config(paths, options, priorities);
        let built = backend::build(options.backend, &paths.artifact, &config)?;
        let summary =
            evaluate_backend(built.as_ref(), &queries, k)?.with_backend(options.backend.name());
        (summary, built.page_count(), built.searchable_count(), None)
    };

    let gate = match &options.gate {
        Some(path) => Some(eval::gate(&summary, &EvalSummary::load(path)?, max_drop)),
        None => None,
    };
    if let Some(path) = &options.json {
        summary.save(path)?;
    }
    Ok(BackendEvalOutcome {
        summary,
        page_count,
        searchable_count,
        k,
        gate,
        delta,
    })
}

/// Run `eval --compare a,b,c`: [`eval_backend`] once per backend, over the same query set.
pub fn eval_compare(
    paths: &Paths,
    backends: &[BackendKind],
    common: &BackendEvalOptions,
) -> Result<Vec<(BackendKind, BackendEvalOutcome)>, CommandError> {
    if backends.is_empty() {
        return Err(CommandError::EmptyCompare);
    }
    backends
        .iter()
        .map(|&kind| {
            let options = BackendEvalOptions {
                backend: kind,
                json: None,
                ..common.clone()
            };
            Ok((kind, eval_backend(paths, &options)?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::commands::embed::{EmbedOptions, embed};
    use crate::commands::testing::{eval_workspace, fake_embedder};

    #[test]
    fn eval_reads_defaults_from_the_config_or_the_options() {
        let (dir, paths) = eval_workspace();
        assert!(matches!(
            eval(&paths, &EvalOptions::default()).unwrap_err(),
            CommandError::NoQueries
        ));
        let options = EvalOptions {
            queries: Some(dir.path().join("queries.jsonl")),
            json: Some(dir.path().join("out.json")),
            ..EvalOptions::default()
        };
        let outcome = eval(&paths, &options).unwrap();
        assert_eq!(
            (outcome.page_count, outcome.searchable_count, outcome.k),
            (1, 1, 10)
        );
        assert!(
            (outcome.summary.tuning.overall.recall5 - 0.5).abs() < 1e-12,
            "{:?}",
            outcome.summary.queries
        );
        assert!(outcome.gate.is_none() && outcome.delta.is_none());
        assert_eq!(
            EvalSummary::load(&dir.path().join("out.json")).unwrap(),
            outcome.summary
        );

        // With a config: queries and k come from its eval section.
        fs::write(
            &paths.config,
            "version: 1\nsources:\n  - name: handbook\n    repo: https://github.com/o/handbook.git\n    \
             ref: main\n    resolver:\n      type: glob\n      include: ['**/*.md']\neval:\n  queries: queries.jsonl\n  k: 3\n  max_recall_drop: 0.1\n",
        )
        .unwrap();
        let outcome = eval(&paths, &EvalOptions::default()).unwrap();
        assert_eq!(outcome.k, 3);
        assert_eq!(outcome.summary.query("caching").unwrap().top.len(), 1);
    }

    #[test]
    fn eval_gates_and_measures_with_and_without() {
        let (dir, paths) = eval_workspace();
        let queries = dir.path().join("queries.jsonl");
        let baseline = dir.path().join("baseline.json");
        let options = EvalOptions {
            queries: Some(queries.clone()),
            json: Some(baseline.clone()),
            ..EvalOptions::default()
        };
        eval(&paths, &options).unwrap();

        // Same corpus: the gate passes even with zero tolerance from the config.
        fs::write(
            &paths.config,
            "version: 1\nsources:\n  - name: handbook\n    repo: https://github.com/o/handbook.git\n    \
             ref: main\n    resolver:\n      type: glob\n      include: ['**/*.md']\neval:\n  queries: queries.jsonl\n  max_recall_drop: 0\n",
        )
        .unwrap();
        let options = EvalOptions {
            queries: Some(queries.clone()),
            gate: Some(baseline.clone()),
            ..EvalOptions::default()
        };
        assert!(eval(&paths, &options).unwrap().gate.unwrap().passed());

        // Adding the residue page lifts recall; removing the only page drops it to zero, which
        // fails the gate.
        let options = EvalOptions {
            queries: Some(queries.clone()),
            with: vec!["handbook::docs/user/quotas.md".into()],
            ..EvalOptions::default()
        };
        let outcome = eval(&paths, &options).unwrap();
        assert_eq!((outcome.page_count, outcome.searchable_count), (2, 2));
        let delta = outcome.delta.unwrap();
        assert!((delta.tuning.1.recall5 - 1.0).abs() < 1e-12);
        assert_eq!(delta.changed.len(), 1);
        let options = EvalOptions {
            queries: Some(queries.clone()),
            gate: Some(baseline),
            without: vec!["handbook::docs/user/README.md".into()],
            ..EvalOptions::default()
        };
        let outcome = eval(&paths, &options).unwrap();
        assert!(!outcome.gate.unwrap().passed());
        assert!(outcome.delta.unwrap().tuning.1.recall5.abs() < 1e-12);

        for (with, without) in [
            (vec!["handbook::docs/user/README.md".to_string()], vec![]),
            (vec!["handbook::nope.md".to_string()], vec![]),
            (vec![], vec!["handbook::nope.md".to_string()]),
        ] {
            let options = EvalOptions {
                queries: Some(queries.clone()),
                with,
                without,
                ..EvalOptions::default()
            };
            assert!(matches!(
                eval(&paths, &options).unwrap_err(),
                CommandError::Index(_)
            ));
        }
    }

    #[test]
    fn eval_backend_bm25_matches_the_plain_eval_path() {
        let (dir, paths) = eval_workspace();
        let queries = dir.path().join("queries.jsonl");
        let plain = eval(
            &paths,
            &EvalOptions {
                queries: Some(queries.clone()),
                ..EvalOptions::default()
            },
        )
        .unwrap();
        let via_backend = eval_backend(
            &paths,
            &BackendEvalOptions {
                queries: Some(queries),
                backend: BackendKind::Bm25,
                ..BackendEvalOptions::default()
            },
        )
        .unwrap();
        assert_eq!(via_backend.page_count, plain.page_count);
        assert_eq!(via_backend.searchable_count, plain.searchable_count);
        assert_eq!(via_backend.summary.tuning, plain.summary.tuning);
        assert_eq!(via_backend.summary.backend, "bm25");
        assert_eq!(plain.summary.backend, "", "the legacy path never sets it");
    }

    #[test]
    fn eval_backend_rejects_with_without_for_non_bm25_backends() {
        let (dir, paths) = eval_workspace();
        let options = BackendEvalOptions {
            queries: Some(dir.path().join("queries.jsonl")),
            backend: BackendKind::Bm25Tantivy,
            with: vec!["handbook::docs/user/quotas.md".to_string()],
            ..BackendEvalOptions::default()
        };
        assert!(matches!(
            eval_backend(&paths, &options).unwrap_err(),
            CommandError::Backend(BackendError::UnsupportedAdjustment(_))
        ));
    }

    #[test]
    fn eval_compare_runs_every_backend_over_the_same_queries() {
        let (dir, paths) = eval_workspace();
        let common = BackendEvalOptions {
            queries: Some(dir.path().join("queries.jsonl")),
            ..BackendEvalOptions::default()
        };
        let results = eval_compare(
            &paths,
            &[BackendKind::Bm25, BackendKind::Bm25Tantivy],
            &common,
        )
        .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, BackendKind::Bm25);
        assert_eq!(results[0].1.summary.backend, "bm25");
        assert_eq!(results[1].0, BackendKind::Bm25Tantivy);
        assert_eq!(results[1].1.summary.backend, "bm25-tantivy");
        assert!(matches!(
            eval_compare(&paths, &[], &common).unwrap_err(),
            CommandError::EmptyCompare
        ));
    }

    #[test]
    fn eval_backend_dense_uses_the_injected_embedder() {
        let (dir, paths) = eval_workspace();
        let embed_options = EmbedOptions {
            model: "fake".to_string(),
            batch: 64,
            out: None,
        };
        embed(&paths, &embed_options, fake_embedder().as_ref()).unwrap();
        let options = BackendEvalOptions {
            queries: Some(dir.path().join("queries.jsonl")),
            backend: BackendKind::Dense,
            embedder: Some(fake_embedder()),
            ..BackendEvalOptions::default()
        };
        let outcome = eval_backend(&paths, &options).unwrap();
        assert_eq!(outcome.summary.backend, "dense");
        assert_eq!(outcome.page_count, 1);
    }
}
