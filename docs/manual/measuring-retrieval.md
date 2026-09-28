# Measuring retrieval

Measuring a retriever against the corpus is [`kanon`](https://github.com/friedrichwilken/kanon)'s
job, not `pinakes`'s. `pinakes` compiles the corpus; `kanon` scores a retriever on it, with the
same flags and file formats these commands had here. Install it with
`cargo install --git https://github.com/friedrichwilken/kanon --locked`.

## Where each command went

| was | is now |
|---|---|
| `pinakes eval` | `kanon eval` (`--gate`, `--json`, `--with`, `--compare`, `--backend` and the rest) |
| `pinakes embed` | `kanon embed` |
| `pinakes grade` | `kanon grade` |
| `pinakes queries add`, `check`, `import` | `kanon queries add`, `check`, `import` |
| `pinakes report --eval-before E --eval-after E` | `kanon report --eval-before E --eval-after E` |
| the `bm25-tantivy`, `dense`, `hybrid` and `external` backends | the same names in `kanon eval --backend` |

The four subcommands stay in `pinakes` for one minor release as stubs: each prints one line
naming the `kanon` command to run instead, whatever arguments it was given, and exits 1. The
release after removes them. `pinakes report` no longer takes `--eval-before` or `--eval-after`,
and its `report.json` is now version 2, without the `eval` key; a version 1 `report.json` written
by an earlier release still loads, and `check` ignores the key.

## What did not move

- **`queries.jsonl` and the `eval:` block** of `pinakes.yaml` keep their formats. `kanon` reads
  both, so an existing `pinakes.yaml` works as `kanon`'s config unchanged, and `pinakes` still
  parses the block (see [Configuration](config.md)).
- **The built-in index** (`SPEC.md` §5) stays as the reference index `kanon` measures by
  default, and as the library and Python `Index` a consumer searches with.
  [`pinakes chunks`](commands.md#chunks) emits its retrieval units, so a consumer's own index can
  be checked against what `kanon` measured.
- **`usage`** stays: it reads a served-query trail and reports on the corpus (see
  [Learning from serving](serving-feedback.md)).
- **The weekly workflow** measures with `kanon eval --gate` and appends `kanon report`'s
  evaluation sections to the pull request body (see [Weekly curation workflow](weekly-workflow.md)).

## How the built-in index scores a page

Pages are cleaned of frontmatter, HTML comments, link and image targets and HTML tags, split
into the intro plus one unit per H2 (H2 sections over 1200 tokens split at H3), scored by title
(×3), heading (×2) and body, ranked by their best unit and de-duplicated by tokenised title. The
tokeniser lowercases, keeps `[a-z0-9]+` runs and drops a small stopword list; no stemming.
`pinakes chunks` emits exactly these units.

**Scoring.** The score is Okapi BM25 with `k1` 1.5, `b` 0.75 and negative IDFs floored at a
quarter of the average IDF, computed with exact unit lengths, and field boosts act as
term-frequency multipliers. This is the common `rank_bm25` BM25Okapi formula, so results are
comparable with that library.

**Mirror rule.** When two sources carry a page with the same title key (navigation title or H1),
only the page from the source with the higher `priority` is indexed; the other is still a page,
just not searchable. Priorities come from `pinakes.yaml` alone (`priority`, default 1). A source
the config does not list, and every source when there is no config, gets that same default, and
equal priorities never collapse a page, so a manifest-less artifact with only `meta.json` files
is indexed with no mirrors at all. Same-title results are still de-duplicated at search time,
whatever the priorities. This is a different mechanism from
[near-duplicate detection](duplicates.md): the mirror rule only affects which page is
searchable, never the manifest or the artifact.
