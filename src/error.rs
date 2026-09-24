//! [`CommandError`]: the error type shared by every command.

use std::path::PathBuf;

use thiserror::Error;

use crate::artifact::ArtifactError;
use crate::backend::BackendError;
use crate::classify::ClassifyError;
use crate::config::ConfigError;
use crate::corpus::CorpusError;
use crate::decisions::DecisionError;
use crate::duplicates::DuplicatesError;
use crate::embed::EmbedError;
use crate::eval::EvalError;
use crate::grade::GradeError;
use crate::index::IndexError;
use crate::llm::ChatError;
use crate::manifest::ManifestError;
use crate::queries::QueriesError;
use crate::render::RenderError;
use crate::report::ReportError;
use crate::residue::ResidueError;
use crate::resolve::ResolveError;
use crate::sources::SourceError;
use crate::trail::TrailError;
use crate::usage::UsageError;

/// Errors raised by any command.
#[derive(Debug, Error)]
pub enum CommandError {
    /// Bad or unreadable config.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// Bad or unreadable manifest.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// Bad or unreadable residue file.
    #[error(transparent)]
    Residue(#[from] ResidueError),
    /// Bad or unreadable decisions file.
    #[error(transparent)]
    Decision(#[from] DecisionError),
    /// Download or extraction failed.
    #[error("source {name}: {source}")]
    Source {
        /// Source name.
        name: String,
        /// Underlying error.
        #[source]
        source: SourceError,
    },
    /// A resolver failed.
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    /// A render step failed.
    #[error(transparent)]
    Render(#[from] RenderError),
    /// Writing the artifact failed.
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    /// A filesystem operation failed.
    #[error("{path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A manifest records a slug that is not `owner/repo`.
    #[error("source {name}: invalid repo slug {repo:?} in manifest")]
    BadSlug {
        /// Source name.
        name: String,
        /// The recorded slug.
        repo: String,
    },
    /// The tarball fetched for a recorded commit reports a different commit.
    #[error("source {name}: fetched {actual} but the manifest records {expected}")]
    CommitMismatch {
        /// Source name.
        name: String,
        /// Commit recorded in the manifest.
        expected: String,
        /// Commit the tarball reported.
        actual: String,
    },
    /// `decide` was given an id that is neither residue nor a page.
    #[error("unknown id {0}: not in residue.jsonl or manifest.json")]
    UnknownId(String),
    /// `init` was given a repository URL that is not `https://github.com/<owner>/<repo>`.
    #[error("{0:?} is not a https://github.com/<owner>/<repo> URL")]
    RepoUrl(String),
    /// A JSON value could not be produced.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Bad or unreadable eval result.
    #[error(transparent)]
    Eval(#[from] EvalError),
    /// The artifact could not be indexed.
    #[error(transparent)]
    Index(#[from] IndexError),
    /// `eval` has no query file: none given and no `eval.queries` in the config.
    #[error("no query file: pass --queries or set eval.queries in the config")]
    NoQueries,
    /// `queries add` or `queries check` failed.
    #[error(transparent)]
    Queries(#[from] QueriesError),
    /// Bad or unreadable `duplicates.jsonl`.
    #[error(transparent)]
    Duplicates(#[from] DuplicatesError),
    /// A retriever backend (SPEC §16) failed to build or search.
    #[error(transparent)]
    Backend(#[from] BackendError),
    /// Embedding, or reading/writing the embeddings file pair, failed.
    #[error(transparent)]
    Embed(#[from] EmbedError),
    /// `eval --compare` was given no backend names.
    #[error("--compare needs at least one backend name")]
    EmptyCompare,
    /// `classify` failed, including talking to the model.
    #[error(transparent)]
    Classify(#[from] ClassifyError),
    /// Building the model configuration failed (e.g. `PINAKES_LLM_URL` is not set).
    #[error(transparent)]
    Llm(#[from] ChatError),
    /// Bad or unreadable `trail.jsonl`.
    #[error(transparent)]
    Trail(#[from] TrailError),
    /// `grade` failed, including talking to the model or the backend.
    #[error(transparent)]
    Grade(#[from] GradeError),
    /// Bad or unreadable usage report, or an invalid `--since`.
    #[error(transparent)]
    Usage(#[from] UsageError),
    /// `report.json` (SPEC §2.10) could not be written or read.
    #[error(transparent)]
    Report(#[from] ReportError),
    /// `check` found no `report.json` to compare the gates against.
    #[error(
        "{}: no report to check; run `pinakes report --json {}` first",
        path.display(),
        path.display()
    )]
    NoReport {
        /// Where the report was expected.
        path: PathBuf,
    },
    /// `check` was given a `report.json` newer than this build understands (SPEC §2.10).
    #[error(
        "{}: report.json version {version} is newer than this build understands ({supported}); \
         a newer pinakes wrote it",
        path.display()
    )]
    ReportVersion {
        /// The report file.
        path: PathBuf,
        /// The document's `version`.
        version: u32,
        /// The version this build writes and reads.
        supported: u32,
    },
}

/// A page-loading failure is reported as the [`IndexError`] it has always been.
impl From<CorpusError> for CommandError {
    fn from(err: CorpusError) -> Self {
        CommandError::Index(IndexError::from(err))
    }
}
