//! `pinakes` turns a declared set of documentation sources into a reproducible,
//! measured corpus for a retrieval system (see `SPEC.md` at the repository root).
//!
//! The library is organised by file contract: [`config`] reads `pinakes.yaml`, [`sources`]
//! downloads checkouts, [`resolve`] discovers candidate pages and [`select`] applies the
//! selection policy to them, [`artifact`] and [`manifest`] materialise
//! the result, [`residue`] and [`decisions`] track what was left out and why, [`diff`] and
//! [`report`] describe changes, [`duplicates`] finds near-duplicate and mirror pages, [`index`] /
//! [`eval`] measure retrieval quality, [`queries`] grows and validates the judge
//! (`queries.jsonl`), and [`backend`] / [`embed`] give `eval` a choice of retriever shapes
//! (SPEC §16) beyond the built-in BM25 index. [`llm`] is the shared OpenAI-compatible chat
//! client; [`classify`] uses it to judge undecided residue and near-duplicate candidates.
//! [`trail`] reads `trail.jsonl`, the consumer-written record of what was actually served;
//! [`grade`] uses the judge (via [`llm`]) to grade what it retrieved, and [`usage`] turns a
//! trail into pages-never-used and gap statistics. [`corpus`] loads an artifact directory into
//! pages (source priorities, the mirror rule); [`index`] builds on it and re-exports its items.
//! [`page`] is the one description of a page, selected or residue, built from [`manifest`] and
//! [`residue`] types once per run.
//!
//! Four leaf modules depend on nothing else in the crate and may be used from anywhere: [`text`]
//! (content hashing, front matter, content cleaning, titles, path helpers), [`tokenizer`] (the
//! tokeniser shared by indexing and querying), [`layout`] (the artifact's file and directory
//! names) and [`jsonl`] (reading, writing and appending JSON Lines files with line-numbered
//! errors).

pub mod artifact;
pub mod backend;
pub mod classify;
pub mod commands;
pub mod config;
pub mod corpus;
pub mod decisions;
pub mod diff;
pub mod duplicates;
pub mod embed;
pub mod eval;
pub mod grade;
pub mod index;
pub mod jsonl;
pub mod layout;
pub mod llm;
pub mod manifest;
pub mod page;
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
