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

## Layout

Module map, grouped bottom-up by what depends on what. One line per module: what it owns.

**Depend on nothing else in the crate:** `text` (hashing, front matter, titles, path helpers),
`tokenizer` (the tantivy `PinakesTokenizer`, stopwords, title keys), `layout` (the artifact's
file and directory names and its contract version), `jsonl` (read/write/append JSON Lines with
line-numbered errors), `num` (the one `usize -> f64` cast), `workspace` (`Paths`, the file
locations every command uses), `config` (`pinakes.yaml`'s schema) and `llm` (the shared
OpenAI-compatible chat client).

**Sources and their recorded history:** `sources` (fetch a checkout) and `manifest`
(`manifest.json`'s schema) depend on `config`; `decisions` (`decisions.jsonl`) depends on
`jsonl`; `residue` (`residue.jsonl`, `Rule`, `residue list`'s filter) depends on `decisions` and
`jsonl`; `trail` (`trail.jsonl`) depends on `jsonl` and `manifest`. `page` (`PageRecord` /
`PageStatus` / `PageRegistry`, the one description of a page, built once per run) depends on
`manifest` and `residue` — a deliberate exception to "depends on nothing": a page record has to
read both files' shapes to describe a page either way.

**Discovery and selection:** `resolve` + one file per resolver under `resolve/` (`glob`,
`external`, `vitepress`, `docusaurus`, `mdbook`, `sitemap`) implement a `pub(crate) trait
Resolver`; `resolver_for` is the only `match` on `config::Resolver`. `resolve` imports neither
`residue` nor `decisions` — a resolver only names a `Mechanism` (`{key, text}`). `select` turns
that into stored residue (`impl From<Mechanism> for Rule`) and applies precedence (`policy.deny`
> `resolver.exclude` > decisions > resolver selection); it depends on `resolve` for
`Candidate`/`Mechanism`/`Plan`, plus `decisions`, `manifest`, `residue`, `config`, `sources`,
`text`. `resolve.rs` keeps one exception in the other direction: a `pub use crate::select::{...}`
shim so paths used before the split (`pinakes::resolve::precedence`, and so on) still resolve.
`pipeline` (`fresh`, `reproduce`, `outputs`) runs discovery and selection, renders (`render` /
`render::openapi`) and writes the artifact, manifest, residue and duplicates files.

**Corpus and retrieval:** `corpus` loads an artifact directory into pages (source priorities, the
mirror rule); `index` (`tokenizer.rs`... `index/bm25.rs`, `index/sections.rs`) builds the BM25
index on it and re-exports its and `tokenizer`'s items, so `pinakes::index::...` paths did not
move; `backend` (`bm25`, `tantivy`, `dense`, `hybrid`, `external`) depends on `corpus`, `index`
and `embed`; `chunks` (`Chunk`, `chunks.jsonl`'s schema, built on `index::iter_units`) depends
on `corpus`, `index` and `text`; `eval` measures a `Backend` and depends only on `index`,
`jsonl`, `num` (not `backend` itself — the code that picks a backend for `eval` lives in
`commands::eval`).

**Everything else that reports on the corpus** (peers; each may use the others): `duplicates`
and `classify` (`ClassifyItem`, LLM judging of undecided residue/near-duplicates) both read a
`&PageRegistry`; `report` (`ReportInput`, also `&PageRegistry`) depends on `page`, `duplicates`,
`usage`, `eval`, `diff`, `decisions`, `manifest`, `residue`; `grade` and `usage` read a trail;
`diff` compares two manifests; `artifact` writes the artifact directory; `queries` grows and
validates `queries.jsonl` — and is the one **known exception** left over: it depends on `eval`
and `grade` for their types, where the target layout has it as their peer instead. That
direction was never revisited in this series.

**Command layer:** `error` holds `CommandError`, the error type every command returns (it wraps
nearly every module's error type — by design, not by drift). `commands/` is one file per
subcommand, each owning its `Options`/`Outcome`; `commands/mod.rs` re-exports them by name, plus
`workspace::Paths`, `error::CommandError` and `pipeline`'s entry point, so every
`pinakes::commands::X` path is unchanged from before the split. Like `cli` below, `commands`
depends on effectively everything beneath it — that is its job as the library's dispatch layer.

**The binary:** `main.rs` holds only `Cli`, the `Command` enum (clap derives `--help` text and
subcommand order from its doc comments — do not reorder without checking), `main` and the `run`
dispatch match. `src/cli/` (binary-only; not declared in `lib.rs`) has one file per subcommand:
`*Args` struct(s) and `run_*` function(s) that build an `Options`, call `commands::...`, and
print the result; `src/cli/mod.rs` also holds `EXIT_GATE`/`EXIT_DIFFERENCES`/`EXIT_POLICY`. `cli`
depends on `commands` plus a handful of library types needed only for printing (e.g.
`residue::to_jsonl`, `duplicates::DuplicateKind`); nothing depends on `cli`.

Check any module's own imports with:

```sh
grep -rho 'crate::[a-z_]*' src/<module>.rs src/<module>/ 2>/dev/null | sort -u
```

(read past any `#[cfg(test)]` block by eye; a test-only import does not count against the rule.)

- **Adding a resolver:** a new `config::Resolver` variant with its arms in
  `config::Resolver::kind()` and `Source::validate()` (both match exhaustively), one file under
  `src/resolve/` implementing `Resolver`, one arm in `resolver_for`. The four navigation
  resolvers share their scanning code in `resolve/navigation.rs`. Name every mechanism a
  resolver reports as a `Mechanism { key, text }`; `select` turns that into a stored `Rule`.
- **Adding a backend:** one file under `src/backend/`, a `BackendKind` variant plus its
  `name()`/`FromStr` arms, one arm in `backend::build`, `needs_embedder()` if it embeds, and
  the list of valid names in `BackendError::UnknownBackend`'s message.
- **Adding a command:** library side, `src/commands/<name>.rs` plus a named re-export in
  `commands/mod.rs`; binary side, `src/cli/<name>.rs` plus its `mod` line in
  `src/cli/mod.rs`, a `Command` variant and a dispatch arm in `main.rs`'s `run`. Write the
  SPEC section first.

A new subcommand is registered in `src/main.rs` (the `Command` enum and the `run` dispatch),
`src/cli/mod.rs` (the `mod` line) and `src/cli/<name>.rs` (its arguments and printing) only; its
logic lives in `src/commands/<name>.rs`, following this same pattern.

## Where the contract lives

`SPEC.md` is authoritative: file formats, the artifact layout, the external resolver contract,
commands, exit codes, scoring rules. When a change alters behaviour that the spec describes,
**change the spec first, in the same commit or the one before**, then the code. If the code
and the spec disagree, the code is wrong. `README.md` explains; it never overrides the spec.

## Docs

Three kinds, never mixed: `README.md` answers "what is it, and is it usable in 60 seconds?" —
one screen, code before prose, why before how, a copy-paste quick start with zero optional
settings, then links out. `docs/tutorials/` answers "walk me through it" — one tutorial builds
one artefact (currently: a corpus plus its weekly workflow), each step adds exactly one
capability, every step shows the complete file so far, new or changed lines carry a trailing
`# <-` comment saying what the line does. `docs/manual/` answers "what exactly does X do?" — one
question per page, filename is the topic, the page opens with the answer before the detail.

Facts that mirror a source of truth (a command's flags, a config key, an exit code) belong in
exactly one manual page; link to it rather than repeating it elsewhere. A caveat that would make
the quick start need an explanation means the quick start is wrong, not that the caveat needs a
footnote. This is not yet test-enforced (no README length check, no docs-example linter) — treat
that as a known gap, not as license to let a page drift from what the code does.

## Gates before every commit

```sh
just check                                            # the three lines below, plus rustdoc, as CI runs them
cargo fmt                                             # applied, not just checked
cargo clippy --all-targets --all-features -- -D warnings   # pedantic is on in Cargo.toml
cargo test                                            # unit, integration, golden, snapshots
```

CI runs the same plus `cargo doc --no-deps` with `RUSTDOCFLAGS=-D warnings`, a build on the
`rust-version` from `Cargo.toml`, the end-to-end example against the network, and `cargo audit`.

Five suites pin behaviour rather than assert it:

- `tests/golden.rs` compares `eval` on `tests/fixtures/golden` with `expected.json`. Refresh
  with `UPDATE_GOLDEN=1 cargo test --test golden`. The other golden suites
  (`tests/golden_mdbook.rs`, `tests/backend_tantivy_golden.rs`,
  `tests/backend_dense_hybrid_golden.rs`) follow the same rule.
- `tests/snapshots/` holds rendered reports. Refresh with `UPDATE_SNAPSHOTS=1 cargo test`.
- `tests/pipeline_pin.rs` pins the bytes of `manifest.json`, `residue.jsonl`,
  `duplicates.jsonl`, `report.md` and `residue list` written by a fresh resolve and a
  `--from-manifest` one, under `tests/snapshots/pipeline_pin/`. Refresh with
  `UPDATE_SNAPSHOTS=1 cargo test --test pipeline_pin`.
- `tests/schema.rs` pins the JSON Schema of `manifest.json`, generated from the `manifest`
  types, at `docs/schemas/manifest.schema.json`. Refresh with
  `UPDATE_SCHEMAS=1 cargo test --test schema`.
- `tests/eval_cli.rs` and `tests/eval_compare_cli.rs` pin the CLI's JSON and table output for
  `eval` through the binary. They have no refresh flag; update the hand-written assertions
  directly when a change is intended.

Refresh only when the change is intended. The commit that updates a golden or snapshot file, or
the assertions in the two eval CLI pin tests, must say, in its body, which rule changed and why
the new numbers or text are the right ones (for example: "the tokeniser now keeps digits, so
recall@5 on `vec-alloc` rises from 0.5 to 1.0"). Never refresh, or edit a pinned assertion, just
to make a red test green without that justification.

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
