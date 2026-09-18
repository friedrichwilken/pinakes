# pinakes

<!-- Badges: replace OWNER with the GitHub owner once the repository has a remote.
[![ci](https://github.com/OWNER/pinakes/actions/workflows/ci.yml/badge.svg)](https://github.com/OWNER/pinakes/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/OWNER/pinakes)](https://github.com/OWNER/pinakes/releases)
[![licence](https://img.shields.io/badge/licence-Apache--2.0-blue.svg)](LICENSE)
-->

> It matters not how many books you have, but how good they are. (Seneca, *Letters to Lucilius* 45)

Named after the *Pinakes*, the catalogue Callimachus wrote for the Library of Alexandria: the
first known record of what a library held, and the first curated corpus.

`pinakes` turns a declared set of documentation sources into a reproducible, measured corpus for
a retrieval system, and keeps it that way over time.

## Why

A retrieval system is only as good as what it was given to retrieve. That corpus usually starts
as a hand-picked file list that nobody revisits, with no record of what was left out or why, no
way to tell whether a change to it helped or hurt, and no way to reproduce last week's version
when this week's breaks something.

`pinakes` makes the corpus a reviewable artifact instead: a config you write, a manifest you
commit, and every input and output a plain file that belongs in git. No database, no daemon, no
hidden state — and it is retriever-agnostic, so it fits in front of whatever index or agent you
already have.

## Quick start

```sh
cargo install --git https://github.com/friedrichwilken/pinakes --tag v1
```

```yaml
# pinakes.yaml
version: 1
sources:
  - name: nomicon
    repo: https://github.com/rust-lang/nomicon.git
    ref: master
    resolver:
      type: glob
      include: ["src/**/*.md"]
```

```sh
pinakes resolve   # <- fetches the source, selects pages, writes the artifact and manifest
```

Full walkthrough, growing this into a second source, a query set and a scheduled workflow:
[tutorial](docs/tutorials/curate-a-corpus.md).

## What you get

```text
nomicon: 63 pages, 0 residue, 0 unresolved @ 5791ca9f5d67
wrote manifest.json and residue.jsonl (0 residue entries) and artifact
```

`manifest.json` — the file you commit, and the file `diff` and `verify` compare against — records
the resolved commit and every selected page with its hash and title. What a resolver saw but did
not select lands in `residue.jsonl` with an excerpt, so a reviewer (or an agent) can decide on it
without opening the file, and `pinakes eval` measures a query set against the corpus so a change
can be scored before it is merged.

## Going further

- [Six resolvers](docs/manual/resolvers.md) for however your sources organise their docs
  (VitePress, Docusaurus, mdBook, a sitemap, a glob, or a script of your own).
- [Rendering](docs/manual/rendering.md) — turn Kubernetes CRDs, OpenAPI schemas, or your own
  formats into indexable pages.
- [Near-duplicate detection](docs/manual/duplicates.md) — find and prune mirrored or
  near-identical pages across sources.
- [Retrieval backends](docs/manual/backends.md) — measure bm25, tantivy, dense, hybrid or your
  own search endpoint against the same query set.
- [Classifying residue with a model](docs/manual/classify.md) and
  [curating with an agent](docs/manual/curating-with-an-agent.md) — review leftovers at scale
  instead of one at a time.
- [A weekly GitHub Actions workflow](docs/manual/weekly-workflow.md) that reproduces the corpus,
  measures it, and opens a pull request when a source changes.
- [Learning from serving](docs/manual/serving-feedback.md) — feed real user queries back in to
  grow the query set and spot removal candidates.

## Learn more

- [Tutorial: curate a corpus and wire up a weekly workflow](docs/tutorials/curate-a-corpus.md)
- [Manual](docs/manual/README.md) — full reference for every command, config key and file.
- [`SPEC.md`](SPEC.md) — the authoritative contract: file formats, exit codes, scoring rules.
- [`CHANGELOG.md`](CHANGELOG.md) — what has landed.
- [`AGENTS.md`](AGENTS.md) and [`CONTRIBUTING.md`](CONTRIBUTING.md) — working in this repository.

## Licence

Apache-2.0.
