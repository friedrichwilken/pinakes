# Learning from serving: the trail, grade, usage

Everything in [Evaluation](eval.md) tunes the corpus against a judge (`queries.jsonl`) that a
person or a model wrote ahead of time. Stage **c** (serving, a consumer's own job) sees
something pinakes never does: what real users actually asked and whether the pages retrieved for
them were any good. `trail.jsonl` is the bridge back — a consumer-written log pinakes only ever
reads:

```json
{"at": "2026-09-16T12:00:00Z", "query": "how do I enable caching", "retrieved": ["handbook::docs/user/caching.md", "handbook::docs/user/quotas.md"], "ranks": [1, 2], "cited": ["handbook::docs/user/caching.md"], "outcome": "ok", "session": "s1"}
```

Every field but `at` and `query` is optional — a consumer that only logs the query text and
what it retrieved still gets useful output. See
[`examples/trail.jsonl`](../../examples/trail.jsonl) for a dozen realistic lines against the
example corpus; because it only reads the committed `manifest.json`,
`pinakes usage --trail examples/trail.jsonl` (run from `examples/`) works offline, with no
`resolve` needed first.

## Grading what was served

`pinakes grade` replays every distinct query in the trail against a backend (today, always the
built-in BM25 index — `--backend` accepts no other name until `SPEC.md` §16.1's backend trait
lands), fetches `--k` candidates (default 20) per query, and asks the same OpenAI-compatible
model endpoint as [`classify`](classify.md) to grade each candidate 0 (irrelevant) to 3 (fully
relevant):

```sh
pinakes grade --trail trail.jsonl --k 20 --out graded.jsonl
```

Each line of `graded.jsonl` is `{"query", "id", "grade", "model", "at"}`. `pinakes queries
import` turns that into rows for `queries.jsonl`, one per distinct query, `expected` being the
ids graded at or above `--min-grade` (default 2); a query with no candidate meeting the bar is
skipped and reported rather than added with an empty `expected`. `holdout` is assigned by a
seeded PRNG (`--seed`) to approximate `--holdout-share` (default `eval.holdout_min`, 0.2), and
every imported row carries `"by": "grader:<model>"` so its provenance is never confused with a
human-written or `queries add`-written row:

```sh
pinakes queries import graded.jsonl --min-grade 2 --holdout-share 0.2 --seed 1
pinakes queries check   # the imported rows are ordinary queries.jsonl rows from here on
```

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
