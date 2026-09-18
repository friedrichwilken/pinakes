# Retrieval backends

`eval` measures the built-in BM25 index by default, but stage b (the index itself) is a
consumer's choice, so `eval` can measure five retriever shapes behind the same `Backend`
trait and the same query set, with `--backend NAME`:

```text
--backend bm25            the built-in index (SPEC §5), default
--backend bm25-tantivy    the same retrieval units, scored by tantivy's own BM25 (k1 1.2, b 0.75)
                          instead of the hand-rolled BM25Okapi formula
--backend dense           an embeddings file (see `pinakes embed` below), cosine similarity
--backend hybrid          reciprocal rank fusion (k = 60) of the top 50 bm25 and dense rankings
--backend external        a consumer's own search endpoint, over HTTP
```

The same choice can be fixed in `pinakes.yaml` so that a bare `pinakes eval` measures the shape a
project has settled on; every key has a flag that overrides it for one run:

```yaml
eval:
  queries: queries.jsonl
  backend: hybrid                 # what a bare `eval` measures (--backend overrides)
  embeddings: embeddings.bin      # for dense/hybrid, relative to this file (--embeddings)
  # backend_url: http://localhost:8080          # for external (--backend-url)
  # compare: [bm25, bm25-tantivy, dense, hybrid]  # one table per backend (--compare)
```

A configured `compare` applies to a bare `eval` only; `--backend NAME` on the command line
measures that one backend and ignores the list.

`--backend dense`/`hybrid` read `embeddings.bin`/`embeddings.json` (`--embeddings PATH`,
default `embeddings.bin` next to the config) and embed the query through
`PINAKES_EMBED_URL`/`PINAKES_EMBED_KEY`, using the model recorded in `embeddings.json` — the
same one the file was built with, since comparing a query embedded by a different model would
be meaningless. A manifest hash mismatch (the artifact moved on since `embed` ran) is an error
unless `--allow-stale` is given. `--backend external --backend-url URL` posts `{"query", "k",
"module"}` to `URL/search` and expects `{"hits": [{"page_id", "score", "heading"}]}` back,
30 second timeout; a failed request fails the whole `eval`. `--with`/`--without` only work with
`--backend bm25`, the one shape that indexes a page list directly rather than a prebuilt file
or an external store.

`eval --compare bm25,bm25-tantivy,dense` runs every named backend over the same query set,
prints one recall/MRR table per backend on stderr, and (with `--json OUT`) writes one JSON
object keyed by backend name instead of a single result. Every backend's result records its
name in `"backend"`.

```sh
pinakes embed --model text-embedding-3-small --out embeddings.bin
pinakes eval --backend dense
pinakes eval --compare bm25,bm25-tantivy,dense,hybrid
```

## `pinakes embed`

```text
pinakes embed [--artifact DIR] [--model NAME] [--out embeddings.bin] [--batch 64]
```

Computes one embedding per retrieval unit — the same intro-plus-H2-sections split `eval`
scores — through an OpenAI-compatible endpoint (`POST {PINAKES_EMBED_URL}/embeddings`,
bearer `PINAKES_EMBED_KEY`, model from `PINAKES_EMBED_MODEL` or `--model`), 64 texts per
request with retries and backoff on HTTP 429 and 5xx. It writes `embeddings.bin`
(little-endian `f32`, row-major) and `embeddings.json` (the model, the vector dimension, the
unit ids in row order, and the artifact's `manifest.json` hash, or `"none"` for a
manifest-less artifact). A missing endpoint is an error, not a silent skip.

None of this says which shape to run in production — that is what the numbers `eval
--compare` prints are for, not an argument about architecture.
