# Commands

All commands take `--config pinakes.yaml` (the default) and print human output to stderr, data
to stdout. `GITHUB_TOKEN` is used when set, to raise the GitHub API rate limit.

| command | reads | writes | exit codes |
|---|---|---|---|
| `resolve [--artifact DIR] [--from-manifest M]` | config, decisions (or a manifest to reproduce) | artifact, `manifest.json`, `residue.jsonl`, `duplicates.jsonl` | 0 ok; 1 error |
| `verify [--artifact DIR] [--no-artifact]` | config, manifest, artifact | nothing | 0; 3 stale; 4 policy violation |
| `diff OLD.json NEW.json [--new-artifact DIR] [--old-artifact DIR]` | two manifests, page content | JSON on stdout, summary on stderr | 0 same; 3 differences |
| `residue list [--source S] [--reason R] [--include-excluded]` | `residue.jsonl`, decisions | JSONL on stdout | 0 |
| `duplicates [--artifact DIR] [--threshold 0.8] [--json OUT]` | artifact, manifest and config (optional) | JSONL on stdout or in `OUT`, summary on stderr | 0 |
| `decide ID include\|exclude\|unsure --reason "…" [--by NAME] [--superseded-by ID]` | residue, manifest | appends to `decisions.jsonl` | 0; 1 unknown id |
| `report [--old M] [--new M] [--eval-before E] [--eval-after E] [--new-artifact DIR] [--old-artifact DIR] [--usage U]` | manifests, residue, decisions, duplicates, eval JSON, usage JSON | `report.md` on stdout | 0 |
| `eval [--artifact DIR] [--queries FILE] [--k N] [--json OUT] [--gate BASELINE] [--with ID…] [--without ID…] [--backend NAME] [--backend-url URL] [--embeddings FILE] [--allow-stale] [--compare NAME,NAME,…]` | artifact, queries, config (optional), embeddings (`dense`/`hybrid`) | table(s) on stderr, JSON on stdout or in `OUT` | 0; 2 gate failed |
| `embed [--artifact DIR] [--model NAME] [--out embeddings.bin] [--batch 64]` | artifact, `PINAKES_EMBED_URL`/`KEY`/`MODEL` | `embeddings.bin`, `embeddings.json` | 0; 1 error (including a missing endpoint) |
| `chunks [--artifact DIR] [--out FILE]` | artifact, config (optional, for priorities) | `chunks.jsonl` on stdout or in `FILE`, summary on stderr | 0; 1 error |
| `queries add --id ID --query TEXT --expected ID… [--kind K] [--holdout] [--queries FILE]` | manifest | appends to `queries.jsonl` | 0; 1 unknown expected id |
| `queries check [--queries FILE]` | `queries.jsonl`, manifest, config (optional) | nothing | 0; 4 unknown id, duplicate id or held-out share too low |
| `classify [--model NAME] [--batch 20] [--dry-run]` | residue, duplicates, decisions, manifest, artifact | `decisions.jsonl` (or JSONL on stdout with `--dry-run`) | 0 |
| `grade --trail FILE [--backend bm25] [--k 20] [--model NAME] [--out OUT]` | trail, artifact | `graded.jsonl` on stdout or in `OUT` | 0 |
| `queries import FILE --min-grade 2 [--holdout-share 0.2] [--seed N] [--queries FILE]` | `graded.jsonl`, manifest | appends to `queries.jsonl` | 0 |
| `usage --trail FILE [--since DURATION] [--json OUT]` | trail, manifest, artifact (for gap candidates) | report on stdout or in `OUT`, summary on stderr | 0 |

## `verify`

Treats the manifest as stale (exit 3) when it disagrees with the config (sources, refs,
resolver kinds) or with the artifact on disk (missing pages, changed bytes, a different
`meta.json`). A policy violation (exit 4) is a source with fewer than `min_pages_per_source`
pages, an archived source under `archived: drop`, or a page that matches `policy.deny`.

An artifact materialised by a release that did not write `artifact_version` (SPEC §2.8) exits 3
here, as `manifest.json` and one `meta.json` per source differ; run `resolve --from-manifest`
to rematerialise it and commit the one-line `artifact_version` diff to `manifest.json`.

## `resolve --from-manifest`

Re-fetches exactly the recorded commits and copies exactly the recorded pages, re-running any
recorded `render` step; the artifact it produces is identical byte for byte, and any hash
mismatch is an error rather than a silent drift. This is how a consumer's build step
materialises a reviewed, committed manifest without re-running discovery.

## `diff`

Computes, per changed page, `lines_added` and `lines_removed` (a small line-based LCS diff), and
per source a `compare_url` (`https://github.com/<owner>/<repo>/compare/<old>...<new>`) once it
already knows both commits:

```sh
pinakes diff /tmp/old.json manifest.json
```

```json
{"id":"handbook::docs/install.md","lines_added":4,"lines_removed":1,"new_sha256":"…","old_sha256":"…","title":"Install"}
```

The new page text comes from `--new-artifact` (default: the artifact next to the config); the
old page text comes from `--old-artifact` when given, or `diff` re-fetches that source at the
old commit itself, one checkout per source, the same way `resolve --from-manifest` does.
`report` accepts the same two flags, for the same reason, and prints a `[compare]` link next to
each changed page's line counts when both commits are known.

## `chunks`

Emits the retrieval units that [`eval`](eval.md) measures, one JSON object per line, keys
sorted, in page then unit order (`SPEC.md` §2.9): the intro plus one unit per H2 of every
searchable page, H2 sections over 1200 tokens split at H3, mirror pages left out. A consumer
building its own index can index these lines, or reimplement the split and compare, so a
recall number from `eval` describes the units it actually serves.

```sh
pinakes chunks --out chunks.jsonl
```

```json
{"heading":"Install","id":"handbook::docs/install.md#1","ordinal":1,"page":"handbook::docs/install.md","sha256":"…","text":"Install\nInstall\n\nRun the installer …"}
```

`id` is `<page id>#<ordinal>`, `ordinal` counts from 0 within the page, `heading` is empty for
the intro and `<H2> / <H3>` for a section split at H3, `text` is the unit `embed` embeds
(title, heading and body joined; the `bm25` index scores the same cut as three fields, title ×3,
heading ×2, body ×1, so `chunks` pins the units, not the scores) and `sha256` is its hex SHA-256.
The stderr summary is
`N chunks from M pages (K mirrors skipped)`.
