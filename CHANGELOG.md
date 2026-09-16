# Changelog

All notable changes to this project are documented in this file. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [1.0.2] - 2026-09-16

### Fixed

- The composite setup action failed to load on every runner: its input description contained a
  `${{ github.token }}` example, which the runner evaluates and rejects. Releases 1.0.0 and 1.0.1
  install fine when downloaded directly; only `uses: friedrichwilken/pinakes@v1` was affected.

### Changed

- CI actions updated: checkout 7, upload-artifact 7, download-artifact 8, create-pull-request 8,
  action-gh-release 3.

## [1.0.1] - 2026-09-16

### Added

- An optional `include` (and `exclude`) glob list on the `external`, `vitepress`, `docusaurus`,
  `mdbook` and `sitemap` resolvers, matching the `glob` resolver's own: a file matching
  `include` that the resolver's own mechanism does not already select is added with
  `selected_by: "include"`, title from the first H1 then frontmatter, empty `doc_type` and
  `section`, and is never residue; `exclude` removes a file from selection regardless of what
  the resolver reports. Lets a landing page outside a site's navigation (a top-level `README.md`,
  say) into the corpus without touching the navigation file itself.
- A composite GitHub Action, [`action.yml`](action.yml) ("Set up pinakes") at the repository
  root: downloads a release for the runner's platform, verifies it against the release's
  `SHA256SUMS`, and adds it to `PATH`. `curate.yml` uses it in place of its own inline download
  when running outside this repository; any consumer workflow can use it directly
  (`uses: friedrichwilken/pinakes@v1`). A moving `v1` tag is force-updated to each `v1.x.y`
  release so `@v1` always tracks the latest compatible one.

### Changed

- The `glob` resolver now defaults to selecting only Markdown files (`extensions: ["md"]`),
  restricting what a broad `include` pattern like `docs/**/*` picks up; an explicit
  `extensions: []` still means every file, and sources with a `render` step (SPEC §10.1) default
  to every file automatically, since a renderer typically consumes YAML or JSON rather than
  Markdown.

## [1.0.0] - 2026-09-16

First release: iterations 1 and 2 together. The file contracts in SPEC.md (config, manifest,
residue, decisions, queries, trail, the external resolver and render protocols) are considered
stable from this version on; changes to them bump the major version.

Iteration 2 (SPEC §10 onwards): content that is not prose, better selection, judgement on top,
and delivery.

### Added

- A per-source `render` hook that turns selected files into Markdown pages after selection and
  before the artifact is written, plus a built-in `openapi` renderer that turns a Kubernetes
  `CustomResourceDefinition` or an OpenAPI 3.x document into one reference page per served CRD
  version or schema, with Fields, Status and Conditions tables.
- `resolve --from-manifest` now records a source's render configuration in the manifest and
  re-runs it, so a rendered source reproduces byte for byte the same as any other; an external
  render command's path is recorded as it was actually run, so reproduction finds it regardless
  of where the manifest is later reproduced from.
- Four built-in resolvers — `vitepress`, `docusaurus`, `mdbook` and `sitemap` — that select the
  pages a documentation site's own navigation file links, and report unlinked Markdown as
  residue.
- A `pinakes duplicates` command and `duplicates.jsonl`, reporting exact, mirror and
  near-duplicate page pairs (MinHash and LSH over shingled text) with a suggested verdict for
  `decide`; run automatically by `resolve` and shown in a new "Duplicates" section of `report`.
- `pinakes queries add` and `pinakes queries check`, for appending validated rows to
  `queries.jsonl` and guarding against a held-out share that has drifted too low.
- `skills/curate/SKILL.md`, a Claude Code skill that drives a curation session through the CLI
  only (`residue list`, `duplicates`, `diff`, `decide`, `report`), for use in a project that
  installs it from this repository.
- `.github/workflows/curate.yml`, a reusable weekly workflow (resolve, diff, eval, duplicates,
  report, then a pull request on branch `pinakes/weekly`), with `examples/curate-weekly.yml`
  showing how a consumer repository calls it.
- `pinakes-py`, a PyO3 wheel exposing `pinakes.Index` (`build`, `search`, `read`) for Python
  consumers, built for Linux x86_64/aarch64 and macOS arm64 in CI on release tags.
- A `Backend` trait behind `eval --backend NAME`, with five retriever shapes (SPEC §16): the
  built-in `bm25`, `bm25-tantivy` (the same retrieval units scored by tantivy's own BM25),
  `dense` (cosine similarity over an embeddings file), `hybrid` (reciprocal rank fusion of the
  bm25 and dense rankings) and `external` (a consumer's own search endpoint, over HTTP). Every
  eval result records its backend name, and `eval --compare NAME,NAME,…` runs several backends
  over the same query set and prints one table per backend.
- `pinakes embed`, computing one embedding per retrieval unit through an OpenAI-compatible
  endpoint and writing `embeddings.bin`/`embeddings.json` (model, dimension, unit ids and the
  artifact's manifest hash) for the `dense` and `hybrid` backends to read.
- `pinakes classify`, sending undecided residue and near-duplicate candidates to an
  OpenAI-compatible chat endpoint in batches and writing the model's verdicts to
  `decisions.jsonl` as ordinary, provenance-stamped decisions.
- `trail.jsonl`, a consumer-written record of what was actually served, and `pinakes grade`,
  which replays its distinct queries against a backend and asks a model to grade each
  candidate 0-3 for relevance.
- `pinakes queries import`, turning `pinakes grade`'s `graded.jsonl` output into `queries.jsonl`
  rows with a seeded, reproducible `holdout` split.
- `pinakes usage`, reporting pages a trail never retrieved, pages retrieved but never cited,
  uncited queries and their best-scoring residue gap candidate; surfaced in `report` as a new
  "Usage" section when `--usage` is given.

### Changed

- `diff` now also computes `lines_added`/`lines_removed` per changed page and a `compare_url`
  per source; `report` prints both next to the pages and sources they describe.
- The tokeniser now indexes an identifier that contains `.`, `/`, `_` or `-` between
  alphanumerics as both its split parts and the joined form (`spec.sink` → `spec`, `sink`,
  `specsink`), so a query for the whole path matches directly; the golden corpus metrics were
  re-pinned for this change.

## [0.1.0] - 2026-09-16 (iteration 1, not published)

Iteration 1: pinakes compiles a documentation corpus and measures it, end to end.

### Added

- `pinakes.yaml`, the source configuration: GitHub sources with a `glob` or `external`
  resolver, a `priority`, and corpus-wide `policy` (`deny`, `archived`, `min_pages_per_source`).
- `resolve`, which downloads each source as a codeload tarball (no git needed), selects pages
  with its resolver, materialises the artifact directory (`<source>/…`, `meta.json`,
  `_residue/…`), and writes `manifest.json` and `residue.jsonl`.
- `manifest.json`, the sorted, two-space-indented, git-friendly record of each source's
  resolved commit, archived flag, and selected pages (hash, title, doc type, section, what
  selected it), plus residue and unresolved paths.
- `residue.jsonl` and `decisions.jsonl`: leftover pages with a reason, title and excerpt, and
  append-only `include`/`exclude`/`unsure` verdicts tied to a page's hash that expire once the
  page's content changes.
- `diff`, comparing two manifests (added, removed and changed pages) as JSON on stdout with a
  human summary on stderr.
- `report`, rendering the Markdown pull-request body from manifests, residue and decisions.
- `verify`, checking a committed manifest against the config and the artifact on disk, and
  enforcing `policy`.
- A built-in, in-memory BM25 backend (tantivy) used only by `eval`: page-intro-plus-H2-section
  units, title/heading/body field boosts, and same-title mirror de-duplication by source
  priority.
- `eval`, reporting recall@5, recall@10 and MRR overall and per query `kind`, split into tuning
  and held-out rows, with `--gate` against a baseline and `--with`/`--without` deltas for a
  residue page.
- `resolve --from-manifest`, reproducing a committed manifest's artifact byte for byte by
  re-fetching exactly the recorded commits and pages.
- A runnable two-source example in `examples/` (the Rustonomicon and the Rust API Guidelines)
  and the golden corpus fixture in `tests/fixtures/golden`, pinning `eval`'s BM25 result.
- CI (lint, unit and integration tests on Ubuntu and macOS, an MSRV build, an end-to-end run of
  the example, `cargo audit`), a release workflow building binaries for Linux and Apple
  silicon, and Dependabot for Cargo and Actions updates.

[Unreleased]: https://github.com/OWNER/pinakes/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/OWNER/pinakes/releases/tag/v0.1.0
