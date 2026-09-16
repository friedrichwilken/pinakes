# Evaluation

How `pinakes eval` measures a corpus, how to fix the evaluation method in `pinakes.yaml`, and
what the numbers it prints mean. The README's [Measuring retrieval](../README.md#measuring-retrieval)
and [Retrieval backends](../README.md#retrieval-backends) sections are the reference; this page
is the walkthrough.

## How it is set up

You write down questions and the pages that answer them in `queries.jsonl`. `pinakes eval`
builds an in-memory index over the artifact, runs every question through it and reports how
often an expected page lands in the top results and how high. Nothing leaves the machine, and
the built-in index is the ruler, not the product: it exists so a change to the corpus can be
scored before it is merged.

## The judge: `queries.jsonl`

One JSON object per line, one question each:

```jsonl
{"id": "install", "kind": "howto", "query": "how do I install the service", "expected": ["handbook::docs/install.md"]}
{"id": "slow", "kind": "troubleshooting", "query": "queries are slow", "expected": ["handbook::docs/troubleshooting/"]}
{"id": "webhooks", "kind": "howto", "query": "receive webhook events", "expected": ["cookbook::docs/recipes/webhooks.md"], "holdout": true}
```

| Field | Meaning |
|---|---|
| `id` | Unique name of the row; `--with`/`--without` deltas and `report.md` refer to it. |
| `kind` | A free label (`howto`, `reference`, `troubleshooting`, `concept`, …); every metric is also reported per kind. |
| `query` | The question, as a reader would type it. |
| `expected` | Page ids that count as a hit. `source::path` names one page; `source::dir/` means any page under that directory. Several entries mean "any of these". |
| `holdout` | `true` keeps the row out of every gate. It is still measured, in its own split. |

`pinakes queries add` appends a row after checking that every expected id exists in the
committed manifest; `pinakes queries check` re-validates the file and fails when the held-out
share drops below `eval.holdout_min` (default 0.2). Held-out rows are the guard against tuning
the corpus to the questions you happen to have written down: a change that lifts the tuning
split and leaves the held-out split flat has fitted the query set, not improved the corpus.

## Choosing the evaluation method in `pinakes.yaml`

The `eval` section fixes what a bare `pinakes eval` does. Every key has a command-line flag
that overrides it for one run.

```yaml
eval:
  queries: queries.jsonl          # the judge, relative to this file
  k: 10                           # result list length; recall@10 becomes recall@k below 10
  max_recall_drop: 0.05           # `eval --gate BASELINE` exits 2 beyond this drop in tuning recall@5
  holdout_min: 0.2                # `queries check` fails below this held-out share
  backend: bm25                   # what a bare `eval` measures; see the table below
  # backend_url: http://localhost:8080   # for backend: external
  # embeddings: embeddings.bin    # for backend: dense | hybrid, relative to this file
  # compare: [bm25, bm25-tantivy, dense, hybrid]   # one table per backend, same query set
```

| `backend` | What is measured | Needs |
|---|---|---|
| `bm25` (default) | The built-in index: pages split into intro plus one unit per H2, title ×3, heading ×2, body ×1, Okapi BM25 with `k1` 1.5 and `b` 0.75, the same formula as the `rank_bm25` Python library. | Nothing but the artifact. |
| `bm25-tantivy` | The same units scored by tantivy's own BM25 (`k1` 1.2, Lucene IDF). | Nothing. |
| `dense` | Cosine similarity over one embedding per unit, page score = best unit. | `pinakes embed` run once (writes `embeddings.bin` + `embeddings.json`), and `PINAKES_EMBED_URL`/`PINAKES_EMBED_KEY` at eval time to embed the query with the same model. |
| `hybrid` | Reciprocal rank fusion (`k` = 60) of the top 50 `bm25` and `dense` rankings. | The same as `dense`. |
| `external` | Whatever search a consumer already runs: `POST {backend_url}/search` with `{"query", "k", "module"}`, answered with `{"hits": [{"page_id", "score", "heading"}]}`. | The endpoint, reachable from where `eval` runs. |

`compare` runs every named backend over the same query set and prints one table each; with
`--json OUT` the file holds one result per backend, keyed by name. When both `compare` and
`backend` are set, a bare `eval` compares; `pinakes eval --backend NAME` on the command line
measures that one backend and ignores the configured list. `--with ID…` and `--without ID…`
(measure the corpus with a residue page added or a page removed) only work with `bm25`, the one
shape that indexes a page list directly rather than a prebuilt file or a remote store.

The embeddings file records the manifest hash it was built from; when the artifact has moved on,
`dense` and `hybrid` refuse to run unless `--allow-stale` is given, because a query embedded
against pages that no longer exist would produce a number that means nothing.

## The metrics

For every query, `eval` takes the first `k` results and finds the rank of the first result that
matches an expected entry. From those ranks:

| Metric | Definition | What it answers |
|---|---|---|
| recall@5 | Fraction of queries whose first expected hit is in the top 5. | "Would an agent that reads five pages have found it?" |
| recall@10 | The same for the top 10 (top `k` when `k` < 10). | "Is the page reachable at all?" |
| MRR | Mean of 1 ÷ rank of the first expected hit (0 when there is none). | "How far down the list is it?" 1.0 means always first; 0.5 means second on average. |
| n | Queries in the split. | How much weight to give the row. |

Each is reported for the tuning split and the held-out split separately, overall and per
`kind`. A query with several `expected` entries is a hit as soon as any of them appears; a
directory prefix (`source::dir/`) is a hit for any page below it.

This is what the golden corpus in `tests/fixtures/golden` prints (thirty pages, fourteen queries,
two held out):

```text
artifact: 33 pages, 30 searchable, k = 10
| split | kind | n | recall@5 | recall@10 | MRR |
|---|---|---|---|---|---|
| tuning | overall | 12 | 0.917 | 0.917 | 0.917 |
| tuning | concept | 1 | 1.000 | 1.000 | 1.000 |
| tuning | howto | 5 | 0.800 | 0.800 | 0.800 |
| tuning | reference | 4 | 1.000 | 1.000 | 1.000 |
| tuning | troubleshooting | 2 | 1.000 | 1.000 | 1.000 |
| held-out | overall | 2 | 1.000 | 1.000 | 1.000 |
| held-out | howto | 2 | 1.000 | 1.000 | 1.000 |
```

Reading it: one `howto` query out of five misses entirely (recall and MRR agree, so the page is
not merely low, it is absent from the top 10). The JSON on stdout says which one:

```json
{
  "tuning": { "overall": { "recall@5": 0.917, "recall@10": 0.917, "mrr": 0.917, "n": 12 }, "per_kind": { "…": "…" } },
  "holdout": { "overall": { "recall@5": 1.0, "recall@10": 1.0, "mrr": 1.0, "n": 2 }, "per_kind": { "…": "…" } },
  "queries": [
    { "id": "plugin", "kind": "howto", "holdout": false, "hit5": false, "hit10": false, "rr": 0.0,
      "top": ["handbook::docs/concepts/architecture.md"] },
    { "id": "…" }
  ]
}
```

When a backend was chosen (`--backend`, or `eval.backend` in the config) the result also carries
`"backend": "<name>"`, and `report.md` shows it next to the numbers.

`top` is the page list the query actually returned, so a miss can be read next to what won
instead. Here the expected page for `plugin` sits in `_residue`, not in the corpus; measuring
with it added is one flag away:

```sh
pinakes eval --with cookbook::docs/recipes/draft-plugin.md
```

prints the same table for the corpus with that page in, then a delta: each metric before and
after, and the queries whose reciprocal rank changed. That delta is the unit of a curation
decision: a residue page is worth admitting when it moves a query, and not otherwise.

## Where the numbers go

- `pinakes eval --json eval.json` writes the result; `pinakes eval --gate eval.json` on a later
  artifact exits 2 when tuning recall@5 drops by more than `max_recall_drop`. The weekly
  workflow runs exactly that pair and fails the PR on a regression.
- `pinakes report --eval-before A --eval-after B` renders both results into `report.md`, overall
  and per kind, held-out separately, so a reviewer sees the effect of a corpus change without
  running anything.
- `pinakes eval --compare …` answers the architecture question with the same query set instead of
  an argument: which retriever shape finds these pages, on this corpus, today.
