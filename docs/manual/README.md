# Manual

Reference documentation: what's there, precisely. Looking for a guided walkthrough instead? See
the [tutorial](../tutorials/curate-a-corpus.md).

- [Configuration and files](config.md) — `pinakes.yaml`'s schema, page identity, and every file
  the tool reads or writes.
- [Commands](commands.md) — every subcommand: what it reads, writes, and its exit codes.
- [Resolvers](resolvers.md) — the six ways to select "which files are documentation".
- [Rendering](rendering.md) — turning CRDs, OpenAPI schemas or your own formats into pages.
- [Handlers](handlers.md) — a render step and an external resolver, before and after, on real
  files.
- [Near-duplicate detection](duplicates.md) — exact, mirror and near-duplicate pages, and the
  winner rule.
- [Evaluation](eval.md) — the query set, the metrics, and how to grow `queries.jsonl`.
- [Retrieval backends](backends.md) — measuring bm25, tantivy, dense, hybrid or an external
  search endpoint with the same query set.
- [Classifying residue with a model](classify.md) — an LLM proposes include/exclude/unsure at
  scale.
- [Curating with an agent](curating-with-an-agent.md) — a Claude Code skill that runs a curation
  session through the CLI.
- [Weekly curation workflow](weekly-workflow.md) — the reusable GitHub Actions workflow that
  opens a PR on a schedule.
- [Setup action](setup-action.md) — putting the `pinakes` binary on `PATH` in any workflow.
- [Learning from serving](serving-feedback.md) — the trail, grading real queries, and usage
  statistics.
- [Development](development.md) — building, testing, and CI.
- [Releases](releases.md) — tags, the release build, and the moving major tag.
- [Python bindings](python-bindings.md) — the `pinakes` wheel.

For the full, authoritative contract (file formats, exit codes, scoring rules), see
[`SPEC.md`](../../SPEC.md).
