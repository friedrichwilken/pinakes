# AGENTS.md

Guidance for coding agents (and people) working in this repository.

## What this is

`pinakes` turns a declared set of documentation sources into a reproducible, measured corpus
for a retrieval system, and keeps it that way over time. Every input and output is a plain
file that belongs in git: a config, a manifest, an artifact directory, residue, decisions,
queries and a report.

## The three stages

- **a. compile** — sources → curated corpus (artifact + manifest + residue). *This tool.*
- **b. build** — corpus → an index (BM25, a vector store, …). *A consumer's job.* pinakes
  ships one in-memory BM25 backend, used only by `eval` to measure a corpus.
- **c. serve** — the index behind an agent or API. *A consumer's job.*

Do not pull stage b or c into this tool.

## Iteration 2 layout

Iteration 2 added modules on top of the SPEC §9 list, each behind its own file or directory:
`render` (the SPEC §10.1 hook) with `render::openapi` (the built-in CRD / OpenAPI renderer);
`resolve::navigation`, `resolve::vitepress`, `resolve::docusaurus`, `resolve::mdbook` and
`resolve::sitemap` (the built-in navigation resolvers, SPEC §12); `duplicates` (near-duplicate
detection, SPEC §11); and `queries` (growing and validating `queries.jsonl`, SPEC §14.3). A new
subcommand is registered in `src/main.rs` (the clap definition) and `src/commands.rs` (the
dispatch) only; its logic lives in a module of its own, following this same pattern.

## Where the contract lives

`SPEC.md` is authoritative: file formats, the artifact layout, the external resolver contract,
commands, exit codes, scoring rules. When a change alters behaviour that the spec describes,
**change the spec first, in the same commit or the one before**, then the code. If the code
and the spec disagree, the code is wrong. `README.md` explains; it never overrides the spec.

## Gates before every commit

```sh
cargo fmt                                             # applied, not just checked
cargo clippy --all-targets --all-features -- -D warnings   # pedantic is on in Cargo.toml
cargo test                                            # unit, integration, golden, snapshots
```

CI runs the same plus `cargo doc --no-deps` with `RUSTDOCFLAGS=-D warnings`, a build on the
`rust-version` from `Cargo.toml`, the end-to-end example against the network, and `cargo audit`.

Two suites pin behaviour rather than assert it:

- `tests/golden.rs` compares `eval` on `tests/fixtures/golden` with `expected.json`. Refresh
  with `UPDATE_GOLDEN=1 cargo test --test golden`.
- `tests/snapshots/` holds rendered reports. Refresh with `UPDATE_SNAPSHOTS=1 cargo test`.

Refresh only when the change is intended. The commit that updates a golden or snapshot file
must say, in its body, which rule changed and why the new numbers or text are the right ones
(for example: "the tokeniser now keeps digits, so recall@5 on `vec-alloc` rises from 0.5 to
1.0"). Never refresh to make a red test green without that justification.

## Code rules

- Rust 2024, `rustfmt` defaults, line length 100.
- Doc comments on every public item (`missing_docs` is a warning and warnings are errors).
- No panics on user input: no `unwrap`/`expect`/indexing on data from files, the network or
  the command line. Library errors are `thiserror` types; `anyhow` only in `src/main.rs`.
- Keep the module layout of SPEC §9. `src/main.rs` holds clap definitions and dispatch only.
- Human output goes to stderr, data to stdout; exit codes are part of the contract.
- `BTreeMap` for anything serialised, so output is sorted and diffs are readable.

## The tool stays generic

pinakes must not know about any particular product, company, documentation site or consumer.
No product names in code, prompts, fixtures, examples or docs; no behaviour that exists only
because one consumer wants it. Anything consumer-specific goes behind the two extension
points the spec defines: the **resolver contract** (SPEC §3, an external command that selects
pages) and the **backend contract** (a consumer's own stage-b index over the artifact
layout of SPEC §2.3). If a feature cannot be expressed through those, it does not belong here.

## Commit messages

Imperative subject under 72 characters, no trailing period. A body that says what changed and
why, wrapped at 100. One logical change per commit. End every commit with:

```text
Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

## Running the example

```sh
cargo build --release
cd examples
../target/release/pinakes resolve --artifact /tmp/pinakes-artifact   # needs the network
../target/release/pinakes verify --artifact /tmp/pinakes-artifact
../target/release/pinakes eval --artifact /tmp/pinakes-artifact
../target/release/pinakes report --old manifest.json > /tmp/report.md
```

`resolve` rewrites `examples/manifest.json` and `examples/residue.jsonl`; commit those only
when the example is meant to move. `GITHUB_TOKEN` raises the API rate limit when set.
