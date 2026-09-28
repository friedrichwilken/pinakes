//! Command orchestration: the library side of every `pinakes` subcommand.
//!
//! `main.rs` only parses arguments and maps results to exit codes; everything that reads or
//! writes files lives here so it can be tested without spawning the binary.
//!
//! Each subcommand's options, outcome and implementation live in their own file below; this
//! module re-exports them by name so every `pinakes::commands::<name>` path stays as it was
//! when this was a single file.

mod check;
mod chunks;
mod classify;
mod decide;
mod diff;
mod duplicates;
mod init;
mod report;
mod residue;
#[cfg(test)]
mod testing;
mod usage;
mod verify;

pub use crate::error::CommandError;
pub use crate::pipeline::{ResolveOptions, ResolveOutcome, resolve};
pub use crate::workspace::Paths;

pub use check::{
    CHECK_VERSION, CheckOptions, CheckOutcome, Violation, check, check_gates, check_warnings,
};
pub use chunks::{ChunksOptions, ChunksOutcome, chunks};
pub use classify::{ClassifyOptions, ClassifyOutcome, classify};
pub use decide::decide;
pub use diff::{DiffOptions, diff};
pub use duplicates::{DuplicatesOptions, duplicates};
pub use init::{
    DetectedLayout, FALLBACK_REF, GITIGNORE_LINES, InitOptions, InitOutcome, WORKFLOW,
    WORKFLOW_PATH, detect_layout, init, source_name,
};
pub use report::{ReportOptions, report};
pub use residue::residue_list;
pub use usage::{UsageOptions, usage};
pub use verify::{VerifyReport, verify};
