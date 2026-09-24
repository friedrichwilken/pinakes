use std::path::PathBuf;

use crate::chunks::{self, Chunk};
use crate::config::Config;
use crate::error::CommandError;
use crate::index::{self, Priorities};
use crate::jsonl::{self, KeyOrder};
use crate::workspace::Paths;

// -------------------------------------------------------------------------------------------
// chunks (SPEC §2.9)
// -------------------------------------------------------------------------------------------

/// Options for `chunks`.
#[derive(Debug, Clone, Default)]
pub struct ChunksOptions {
    /// Artifact directory (default: `paths.artifact`).
    pub artifact: Option<PathBuf>,
    /// Write `chunks.jsonl` here instead of returning it for stdout.
    pub out: Option<PathBuf>,
}

/// What `chunks` produced.
#[derive(Debug)]
pub struct ChunksOutcome {
    /// Retrieval units emitted.
    pub chunks: usize,
    /// Pages the units came from (mirrors excluded).
    pub pages: usize,
    /// Pages read from the artifact, mirrors included.
    pub page_count: usize,
    /// The JSONL text, sorted keys, one chunk per line; `None` when written to `options.out`.
    pub jsonl: Option<String>,
}

/// Run `chunks`: every retrieval unit of the artifact (SPEC §2.9), cut exactly as `eval`
/// indexes and `embed` embeds them.
///
/// The config is optional, exactly as for `eval`: priorities default to
/// [`Priorities::default`] without one, so nothing is a mirror.
pub fn chunks(paths: &Paths, options: &ChunksOptions) -> Result<ChunksOutcome, CommandError> {
    let priorities = if paths.config.is_file() {
        Priorities::from_config(&Config::load(&paths.config)?)
    } else {
        Priorities::default()
    };
    let artifact = options.artifact.as_ref().unwrap_or(&paths.artifact);
    let mut pages = index::load_pages(artifact, &priorities)?;
    index::mark_mirrors(&mut pages);
    let chunks: Vec<Chunk> = chunks::chunks(&pages);
    let outcome = ChunksOutcome {
        chunks: chunks.len(),
        pages: pages.iter().filter(|p| p.mirror_of.is_none()).count(),
        page_count: pages.len(),
        jsonl: None,
    };
    match &options.out {
        Some(out) => {
            jsonl::write(out, &chunks, KeyOrder::Sorted).map_err(|err| match err {
                jsonl::JsonlError::Io { path, source } => CommandError::Io { path, source },
                jsonl::JsonlError::Json { source, .. } => CommandError::Json(source),
            })?;
            Ok(outcome)
        }
        None => Ok(ChunksOutcome {
            jsonl: Some(jsonl::to_string(&chunks, KeyOrder::Sorted)?),
            ..outcome
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testing::eval_workspace;

    #[test]
    fn chunks_returns_jsonl_for_stdout_or_writes_the_file() {
        let (dir, paths) = eval_workspace();
        let outcome = chunks(&paths, &ChunksOptions::default()).unwrap();
        assert_eq!(
            (outcome.chunks, outcome.pages, outcome.page_count),
            (1, 1, 1)
        );
        let text = outcome.jsonl.expect("JSONL for stdout");
        let lines: Vec<Chunk> = jsonl::parse(&text).unwrap();
        assert_eq!(lines[0].id, "handbook::docs/user/README.md#0");
        assert!(text.starts_with("{\"heading\":\"\",\"id\":"));

        let out = dir.path().join("chunks.jsonl");
        let options = ChunksOptions {
            artifact: Some(paths.artifact.clone()),
            out: Some(out.clone()),
        };
        let outcome = chunks(&paths, &options).unwrap();
        assert!(outcome.jsonl.is_none());
        assert_eq!(std::fs::read_to_string(&out).unwrap(), text);
    }

    #[test]
    fn a_missing_artifact_is_an_error() {
        let (_dir, paths) = eval_workspace();
        let options = ChunksOptions {
            artifact: Some(paths.artifact.join("absent")),
            out: None,
        };
        assert!(matches!(
            chunks(&paths, &options).unwrap_err(),
            CommandError::Index(_)
        ));
    }
}
