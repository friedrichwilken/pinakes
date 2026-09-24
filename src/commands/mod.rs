//! Command orchestration: the library side of every `pinakes` subcommand.
//!
//! `main.rs` only parses arguments and maps results to exit codes; everything that reads or
//! writes files lives here so it can be tested without spawning the binary.
//!
//! Each subcommand's options, outcome and implementation live in their own file below; this
//! module re-exports them by name so every `pinakes::commands::<name>` path stays as it was
//! when this was a single file.

mod chunks;
mod classify;
mod decide;
mod diff;
mod duplicates;
mod embed;
mod eval;
mod grade;
mod queries;
mod report;
mod residue;
#[cfg(test)]
mod testing;
mod usage;
mod verify;

pub use crate::error::CommandError;
pub use crate::pipeline::{ResolveOptions, ResolveOutcome, resolve};
pub use crate::workspace::Paths;

pub use chunks::{ChunksOptions, ChunksOutcome, chunks};
pub use classify::{ClassifyOptions, ClassifyOutcome, classify};
pub use decide::decide;
pub use diff::{DiffOptions, diff};
pub use duplicates::{DuplicatesOptions, duplicates};
pub use embed::{EmbedOptions, EmbedOutcome, embed};
pub use eval::{
    BackendEvalOptions, BackendEvalOutcome, DEFAULT_K, DEFAULT_MAX_RECALL_DROP, EvalFlags,
    EvalOptions, EvalOutcome, EvalPlan, apply_eval_config_defaults, eval, eval_backend,
    eval_compare, eval_embedder_from_env, eval_plan,
};
pub use grade::{GradeOptions, GradeOutcome, grade};
pub use queries::{
    QueriesAddOptions, QueriesImportOptions, QueriesImportOutcome, queries_add, queries_check,
    queries_import,
};
pub use report::{ReportOptions, report};
pub use residue::residue_list;
pub use usage::{UsageOptions, usage};
pub use verify::{VerifyReport, verify};
