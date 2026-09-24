//! `pinakes` turns a declared set of documentation sources into a reproducible,
//! measured corpus for a retrieval system (see `SPEC.md` at the repository root).
//!
//! Eight modules depend on nothing else in the crate and may be used from anywhere: [`text`]
//! (content hashing, front matter, content cleaning, titles, path helpers), [`tokenizer`] (the
//! tokeniser shared by indexing and querying), [`layout`] (the artifact's file and directory
//! names), [`jsonl`] (reading, writing and appending JSON Lines files with line-numbered
//! errors), [`num`] (the one `usize -> f64` cast used by ratios and averages), [`workspace`]
//! (holds [`workspace::Paths`], the file locations shared by every command), [`config`] (reads
//! `pinakes.yaml`) and [`llm`], the shared OpenAI-compatible chat client.
//!
//! [`sources`] downloads checkouts and [`manifest`] is `manifest.json`'s schema, both on top of
//! [`config`]; [`decisions`] is `decisions.jsonl`; [`residue`] is `residue.jsonl`, on top of
//! [`decisions`]; [`trail`] reads `trail.jsonl`, the consumer-written record of what was
//! actually served. [`page`] is the one description of a page, selected or residue, built from
//! a [`manifest`] and a slice of [`residue`] entries once per run (a deliberate exception to
//! the no-dependency rule above: describing a page means reading both shapes).
//!
//! [`resolve`] discovers candidate pages (one file per built-in mechanism under `resolve/`:
//! glob, external, vitepress, docusaurus, mdbook, sitemap) and [`select`] applies the selection
//! policy to them — precedence, `policy.deny`, `resolver.exclude` and decisions — to decide what
//! becomes a page and what becomes residue; [`pipeline`] is the compile pipeline (SPEC stage a)
//! that runs the two together, renders where configured and writes the artifact, manifest,
//! residue and duplicates files.
//!
//! [`corpus`] loads an artifact directory into pages (source priorities, the mirror rule);
//! [`index`] builds the built-in BM25 index on top of it and re-exports its items, so
//! `pinakes::index::...` paths did not move; [`chunks`] emits those units as `chunks.jsonl`, the
//! contract a consumer's own index can be checked against; [`backend`] / [`embed`] give [`eval`]
//! a choice of retriever shapes (SPEC §16) beyond that built-in index.
//!
//! [`duplicates`] finds near-duplicate and mirror pages; [`classify`] uses [`llm`] to judge
//! undecided residue and near-duplicate candidates; [`grade`] uses the judge to grade what a
//! trail retrieved; [`usage`] turns a trail into pages-never-used and gap statistics; [`queries`]
//! grows and validates the judge (`queries.jsonl`); [`diff`] and [`report`] describe changes;
//! [`artifact`] materialises a compiled corpus; [`render`] is the SPEC §10.1 render hook.
//!
//! Consumers of an artifact (an evaluation tool, a serving system) depend on the modules SPEC
//! §20 lists — [`corpus`], [`index`], [`chunks`], [`tokenizer`], [`manifest`], [`layout`],
//! [`text`], [`jsonl`], [`num`], [`trail`], [`config`] and [`llm`] — and those follow the
//! compatibility rule stated there; every other module is an implementation detail of the
//! commands. SPEC §21 says which commands are moving out to a separate evaluation tool.
//!
//! [`error`] holds [`error::CommandError`], the error type every command returns. [`commands`]
//! is one file per subcommand, each owning its options and outcome, re-exported by name from
//! [`commands`] itself, along with [`workspace::Paths`], [`error::CommandError`] and
//! [`pipeline`]'s entry point, so every `pinakes::commands::...` path stays as it was before the
//! split. The `pinakes` binary's own `src/cli/` (one file per subcommand: arguments, dispatch
//! and printing) is not part of this library.

pub mod artifact;
pub mod backend;
pub mod chunks;
pub mod classify;
pub mod commands;
pub mod config;
pub mod corpus;
pub mod decisions;
pub mod diff;
pub mod duplicates;
pub mod embed;
pub mod error;
pub mod eval;
pub mod grade;
pub mod index;
pub mod jsonl;
pub mod layout;
pub mod llm;
pub mod manifest;
pub mod num;
pub mod page;
pub mod pipeline;
pub mod queries;
pub mod render;
pub mod report;
pub mod residue;
pub mod resolve;
pub mod select;
pub mod sources;
pub mod text;
pub mod tokenizer;
pub mod trail;
pub mod usage;
pub mod workspace;
