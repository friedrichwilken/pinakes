# Learning from serving: the trail and usage

Evaluation with [`kanon`](measuring-retrieval.md) tunes the corpus against a judge
(`queries.jsonl`) that a person or a model wrote ahead of time. Stage **c** (serving, a
consumer's own job) sees something pinakes never does: what real users actually asked and
whether the pages retrieved for them were any good. `trail.jsonl` is the bridge back — a
consumer-written log pinakes only ever reads:

```json
{"version": 1, "at": "2026-09-16T12:00:00Z", "query": "how do I enable caching", "retrieved": ["handbook::docs/user/caching.md", "handbook::docs/user/quotas.md"], "ranks": [1, 2], "cited": ["handbook::docs/user/caching.md"], "outcome": "ok", "session": "s1"}
```

Every field but `at` and `query` is optional — a consumer that only logs the query text and
what it retrieved still gets useful output. `version` is the trail contract's version, which
[`kanon`](https://github.com/friedrichwilken/kanon) defines and publishes as a JSON Schema
(`docs/schemas/trail-entry.schema.json`); a missing one means 1, and a line of a newer version
is rejected before anything else about it is read, with one line naming the file, the line and
both versions, so upgrade pinakes. See
[`examples/trail.jsonl`](../../examples/trail.jsonl) for a dozen realistic lines against the
example corpus; because it only reads the committed `manifest.json`,
`pinakes usage --trail examples/trail.jsonl` (run from `examples/`) works offline, with no
`resolve` needed first.

## Grading what was served

Turning a trail into new queries is `kanon`'s: `kanon grade` replays every distinct query in the
trail against a backend and asks a model to grade each candidate 0 (irrelevant) to 3 (fully
relevant), and `kanon queries import` turns the graded lines into rows for `queries.jsonl`. See
[Measuring retrieval](measuring-retrieval.md) for where those commands went. `pinakes grade`
and `pinakes queries` remain for one minor release as stubs that name the `kanon` command.

## Usage statistics

`pinakes usage` turns the trail into four things a judge alone cannot show: pages the manifest
carries that the trail never retrieved in the window (removal candidates), pages retrieved but
never cited, queries that got no citation at all, and — for each such query — the best-scoring
leftover page from `_residue`, found with a small BM25-like index over the tokenised, cleaned
residue text (a possible gap candidate: something worth promoting out of residue). `--since`
narrows the window (`30d`, `12h`, `45m`, `90s`; omit it to use the whole file):

```sh
pinakes usage --trail trail.jsonl --since 30d --json usage.json
pinakes report --usage usage.json   # adds a "Usage" section to the PR body
```

`report` renders the "Usage" section only when `--usage` is given, so every existing report (and
every snapshot in `tests/snapshots/`) is unaffected by a consumer that has not started logging a
trail yet.
