# pinakes

<!-- Badges: replace OWNER with the GitHub owner once the repository has a remote.
[![ci](https://github.com/OWNER/pinakes/actions/workflows/ci.yml/badge.svg)](https://github.com/OWNER/pinakes/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/OWNER/pinakes)](https://github.com/OWNER/pinakes/releases)
[![licence](https://img.shields.io/badge/licence-Apache--2.0-blue.svg)](LICENSE)
-->

> Non refert quam multos sed quam bonos habeas. — It matters not how many books you have, but
> how good they are. (Seneca, *Letters to Lucilius* 45)

Named after the *Pinakes*, the catalogue Callimachus wrote for the Library of Alexandria: the
first known record of what a library held, and the first curated corpus.

`pinakes` turns a declared set of documentation sources into a reproducible, measured corpus
for a retrieval system, and keeps it that way over time. It is retriever-agnostic: the output
is a directory of Markdown files plus a manifest, and every input and output is a plain file
that belongs in git. No database, no daemon, no hidden state.

The full contract is in [`SPEC.md`](SPEC.md). Iteration 1 implements all of it, including the
built-in BM25 index that `eval` uses to measure a corpus against a query set; iteration 2 (SPEC
§10 onward) is under way and already covers rendering non-Markdown sources, near-duplicate
detection, the query-set commands, the curation skill and the weekly workflow described below.
[`CHANGELOG.md`](CHANGELOG.md) tracks what has landed.

Two guides go deeper than this file: [Evaluation](docs/evaluation.md) (how measuring is set up,
how to fix the retriever shape in `pinakes.yaml`, what the metrics mean) and
[Handlers](docs/handlers.md) (what a render step and an external resolver do to a document,
before and after, on a runnable example).

## The three stages

```text
a. compile   sources  ──►  curated corpus (artifact + manifest + residue)     ◄── this tool
b. build     corpus   ──►  an index (in-process BM25, a vector store, …)     ◄── a consumer
c. serve     index behind an agent or API                                    ◄── a consumer
```

pinakes is stage **a**. It downloads each source as a codeload tarball (no git needed),
selects pages with a resolver, materialises them into an artifact directory that any consumer
can index, and records exactly what it did, so the same corpus can be rebuilt byte for byte
and every change between two runs can be reviewed in a pull request.

## A real run

The example in `examples/` curates two of Rust's own documentation repositories: the
[Rustonomicon](https://github.com/rust-lang/nomicon), selected by a glob, and the
[API Guidelines](https://github.com/rust-lang/api-guidelines), selected by a small external
resolver script. The config is the whole declaration:

```yaml
version: 1
sources:
  - name: nomicon
    repo: https://github.com/rust-lang/nomicon.git
    ref: master
    priority: 10
    resolver:
      type: glob
      include: ["src/**/*.md"]
      exclude: ["**/SUMMARY.md"]
  - name: api-guidelines
    repo: https://github.com/rust-lang/api-guidelines.git
    ref: master
    priority: 5
    resolver:
      type: external
      command: ["python3", "resolvers/frontmatter_title.py"]
      args: ["src"]
policy:
  deny: ["**/CHANGELOG.md"]
  archived: warn
  min_pages_per_source: 1
eval:
  queries: queries.jsonl
  k: 10
  max_recall_drop: 0.05
```

```sh
cargo build --release
cd examples
../target/release/pinakes resolve
```

```
api-guidelines: 14 pages, 1 residue, 0 unresolved @ 97a0969cb07f
nomicon: 63 pages, 0 residue, 0 unresolved @ 5791ca9f5d67
wrote manifest.json and residue.jsonl (1 residue entries) and artifact
```

`manifest.json` records, per source, the commit that was fetched and every page with its hash
and title. This is the file you commit, and the file `diff` and `verify` compare against:

```json
"nomicon": {
  "repo": "rust-lang/nomicon",
  "ref": "master",
  "commit": "5791ca9f5d671328af7a8fe87b42ca90c7211d28",
  "archived": false,
  "resolver": "glob",
  "pages": {
    "src/aliasing.md": {
      "sha256": "88842cc90f2f65a510e57a4711905847b245a2ce0c226010e41da0c0fa5ade1e",
      "title": "Aliasing",
      "selected_by": "include"
    },
    "src/arc-mutex/arc-and-mutex.md": {
      "sha256": "4f5700bba2120a70eaae96a0c8976b01ef90239b8b7a477bee18e914576612de",
      "title": "Implementing Arc and Mutex",
      "selected_by": "include"
    }
  }
}
```

The one residue entry is the API Guidelines' table of contents, which the resolver reported
but did not select. `residue list` prints it with an excerpt so a reviewer, or an agent, can
decide without opening the file; `decide` records the verdict, keyed to the page's hash so it
expires when the page changes:

```sh
../target/release/pinakes residue list
../target/release/pinakes decide api-guidelines::src/SUMMARY.md exclude \
    --reason "a table of contents, not a page" --by me
```

```
{"id":"api-guidelines::src/SUMMARY.md","source":"api-guidelines","path":"src/SUMMARY.md",
 "reason":"not_selected","title":"Summary","context":"mdBook table of contents",
 "excerpt":"# Summary [About](about.md) [Checklist](checklist.md) - [Naming](naming.md) …"}
```

`eval` measures the corpus against `queries.jsonl`, six questions a reader of these two books
might ask, one of them held out:

```sh
../target/release/pinakes eval
```

```
artifact: 77 pages, 77 searchable, k = 10
| split    | kind      | n | recall@5 | recall@10 | MRR   |
|----------|-----------|---|----------|-----------|-------|
| tuning   | overall   | 5 | 1.000    | 1.000     | 0.900 |
| tuning   | concept   | 2 | 1.000    | 1.000     | 0.750 |
| tuning   | howto     | 2 | 1.000    | 1.000     | 1.000 |
| tuning   | reference | 1 | 1.000    | 1.000     | 1.000 |
| held-out | overall   | 1 | 1.000    | 1.000     | 1.000 |
```

`verify` exits 0 while the committed manifest still matches a fresh resolve, and `report`
renders the PR body from a diff, the residue, the decisions and two eval results:

```sh
../target/release/pinakes verify
../target/release/pinakes report --old manifest.json > report.md
```

## The files

| file | written by | committed | what it is |
|---|---|---|---|
| `pinakes.yaml` | you | yes | Sources (GitHub repo, ref, resolver, priority), policy (`deny`, `archived`, `min_pages_per_source`) and eval settings. |
| `manifest.json` | `resolve` | yes | Per source: resolved commit, archived flag, every selected page with its sha256, title, doc type, section and what selected it, plus residue and unresolved paths. Sorted keys, two-space indent. |
| `<artifact>/` | `resolve` | no | `manifest.json`, `<source>/<original path>.md`, `<source>/meta.json` and `_residue/<source>/…` for the leftovers. A stable layout consumers rely on. |
| `residue.jsonl` | `resolve` | yes | What was left out and why (`not_selected`, `unresolved_link`, `new_source`, `excluded`) with title, excerpt, upstream url and the rule (`{key, text}`) that decided it. |
| `duplicates.jsonl` | `resolve` | yes | Exact, mirror and near-duplicate page pairs, canonical first, each with its upstream url and a `suggested` verdict for `decide`. |
| `decisions.jsonl` | you or an agent | yes | Append-only verdicts on residue or a duplicate: `include`, `exclude` or `unsure`, tied to the page hash. Later lines win; a changed page expires the decision. |
| `queries.jsonl` | you or a grader | yes | The judge for `eval`: query, expected page ids or prefixes, kind, holdout flag. |
| `report.md` | `report` | no | The PR body: counts, eval before/after, added/removed/changed pages (with line counts and an upstream compare link), new residue, expired decisions, unresolved links, archived sources, duplicates. |

Page identity everywhere is `<source name>::<path>`. Precedence for a file is
`policy.deny` > `resolver.exclude` > decisions > resolver selection.

A minimal `pinakes.yaml`:

```yaml
version: 1
sources:
  - name: handbook
    repo: https://github.com/example-org/handbook.git
    ref: main
    priority: 10                     # higher wins when two sources carry the same title; default 1
    resolver:
      type: glob
      include: ["docs/user/**/*.md"]
      exclude: ["**/_sidebar.md"]
  - name: guides
    repo: https://github.com/example-org/guides.git
    ref: main
    resolver:
      type: external
      command: ["python3", "resolvers/toc.py"]   # cwd = the checkout
      residue_mention: "(?i)handbook"
policy:
  deny: ["**/CLAUDE.md", "**/adr/**", "**/CHANGELOG.md"]
  archived: warn
  min_pages_per_source: 1
```

`glob` selects files by pattern. `external` runs a command with cwd set to the checkout and
`PINAKES_SOURCE` / `PINAKES_COMMIT` in the environment; it writes one JSON object per candidate
to stdout (`path`, `title`, `doc_type`, `section`, `selected`) and any non-zero exit fails the
source with the command's stderr. Script paths that exist relative to `pinakes.yaml` are made
absolute before the command runs. A runnable two-source example lives in
[`examples/`](examples/), including a dependency-free Python resolver;
[`examples/handlers/`](examples/handlers/) runs an external resolver and both render types on a
real repository, walked through in [Handlers](docs/handlers.md). A source can also
declare a `render` step to turn non-Markdown files into pages (see
[Rendering schemas and other formats](#rendering-schemas-and-other-formats) below) and one of
the four [built-in navigation resolvers](#built-in-resolvers) instead of `glob` or `external`.

## Commands

All commands take `--config pinakes.yaml` (the default) and print human output to stderr, data
to stdout. `GITHUB_TOKEN` is used when set.

| command | reads | writes | exit codes |
|---|---|---|---|
| `resolve [--artifact DIR] [--from-manifest M]` | config, decisions (or a manifest to reproduce) | artifact, `manifest.json`, `residue.jsonl`, `duplicates.jsonl` | 0 ok; 1 error |
| `verify [--artifact DIR] [--no-artifact]` | config, manifest, artifact | nothing | 0; 3 stale; 4 policy violation |
| `diff OLD.json NEW.json [--new-artifact DIR] [--old-artifact DIR]` | two manifests, page content | JSON on stdout, summary on stderr | 0 same; 3 differences |
| `residue list [--source S] [--reason R] [--include-excluded]` | `residue.jsonl`, decisions | JSONL on stdout | 0 |
| `duplicates [--artifact DIR] [--threshold 0.8] [--json OUT]` | artifact, manifest and config (optional) | JSONL on stdout or in `OUT`, summary on stderr | 0 |
| `decide ID include\|exclude\|unsure --reason "…" [--by NAME] [--superseded-by ID]` | residue, manifest | appends to `decisions.jsonl` | 0; 1 unknown id |
| `report [--old M] [--new M] [--eval-before E] [--eval-after E] [--new-artifact DIR] [--old-artifact DIR]` | manifests, residue, decisions, duplicates, eval JSON | `report.md` on stdout | 0 |
| `eval [--artifact DIR] [--queries FILE] [--k N] [--json OUT] [--gate BASELINE] [--with ID…] [--without ID…] [--backend NAME] [--backend-url URL] [--embeddings FILE] [--allow-stale] [--compare NAME,NAME,…]` | artifact, queries, config (optional), embeddings (`dense`/`hybrid`) | table(s) on stderr, JSON on stdout or in `OUT` | 0; 2 gate failed |
| `embed [--artifact DIR] [--model NAME] [--out embeddings.bin] [--batch 64]` | artifact, `PINAKES_EMBED_URL`/`KEY`/`MODEL` | `embeddings.bin`, `embeddings.json` | 0; 1 error (including a missing endpoint) |
| `queries add --id ID --query TEXT --expected ID… [--kind K] [--holdout] [--queries FILE]` | manifest | appends to `queries.jsonl` | 0; 1 unknown expected id |
| `queries check [--queries FILE]` | `queries.jsonl`, manifest, config (optional) | nothing | 0; 4 unknown id, duplicate id or held-out share too low |
| `classify [--model NAME] [--batch 20] [--dry-run]` | residue, duplicates, decisions, manifest, artifact | `decisions.jsonl` (or JSONL on stdout with `--dry-run`) | 0 |
| `grade --trail FILE [--backend bm25] [--k 20] [--model NAME] [--out OUT]` | trail, artifact | `graded.jsonl` on stdout or in `OUT` | 0 |
| `queries import FILE --min-grade 2 [--holdout-share 0.2] [--seed N] [--queries FILE]` | `graded.jsonl`, manifest | appends to `queries.jsonl` | 0 |
| `usage --trail FILE [--since 30d] [--json OUT]` | trail, manifest, artifact (for gap candidates) | report on stdout or in `OUT`, summary on stderr | 0 |

`verify` treats the manifest as stale when it disagrees with the config (sources, refs,
resolver kinds) or with the artifact on disk (missing pages, changed bytes, a different
`meta.json`). A policy violation is a source with fewer than `min_pages_per_source` pages, an
archived source under `archived: drop`, or a page that matches `policy.deny`.

`resolve --from-manifest` re-fetches exactly the recorded commits and copies exactly the
recorded pages, re-running any recorded `render` step (SPEC §10.1); the artifact it produces is
identical byte for byte, and any hash mismatch is an error rather than a silent drift.

`diff` also computes, per changed page, `lines_added` and `lines_removed` (a small line-based
LCS diff), and per source a `compare_url`
(`https://github.com/<owner>/<repo>/compare/<old>...<new>`) once it already knows both commits.
The new page text comes from `--new-artifact` (default: the artifact next to the config); the
old page text comes from `--old-artifact` when given, or `diff` re-fetches that source at the
old commit itself, one checkout per source, the same way `resolve --from-manifest` does:

```sh
../target/release/pinakes diff /tmp/old.json manifest.json
```

```json
{"id":"handbook::docs/install.md","lines_added":4,"lines_removed":1,"new_sha256":"…","old_sha256":"…","title":"Install"}
```

`report` prints both: a changed page's line counts next to its title, and a `[compare]` link
to the source's diff on GitHub when both commits are known. `report` accepts the same
`--new-artifact`/`--old-artifact` flags, for the same reason.

## Rendering schemas and other formats

Pinakes reads Markdown. Selection works on any file a resolver names, but titles come from an
H1 or frontmatter, sections are split at H2 and H3, and the index strips Markdown syntax, so
other formats are indexed as plain text at best — unless a source declares `render` (SPEC §10),
in which case pinakes turns the selected files into Markdown pages after selection and before
the artifact is written. One selected file may become several pages, or none.
[Handlers](docs/handlers.md) shows both render types before and after on real files.

Two render types exist:

- `type: openapi` — the built-in renderer. Kubernetes `CustomResourceDefinition` files (YAML,
  one or more documents per file) and OpenAPI 3.x documents become one reference page per served
  CRD version (storage version first) or per `components.schemas` entry, with Fields, Status and
  Conditions tables.
- `type: external` — a command of your own. It runs with `cwd` set to the checkout and env
  `PINAKES_SOURCE`, `PINAKES_COMMIT`, `PINAKES_OUT` (a directory it writes pages into), reads the
  selected paths as JSON lines (`{"path": "…"}`) on stdin, and writes one JSON line per generated
  page on stdout: `{"path": "…", "source_path": "…", "title": "…", "doc_type": "…", "section":
  "…"}`, `path` relative to `PINAKES_OUT`. A selected file that no output line names is dropped
  from the artifact and listed in `meta.json` under `unrendered`.

```yaml
sources:
  - name: my-operator
    repo: https://github.com/example/my-operator.git
    ref: main
    resolver:
      type: glob
      include: ["config/crd/bases/*.yaml"]
    render:
      type: openapi
```

A rendered page's manifest entry carries `rendered_from` (the selected path it came from) and
its `sha256` is of the *rendered* Markdown, so `diff` reports a schema change as a changed page.
A source's `render` configuration is itself recorded in the manifest next to its resolver kind
(an external command's path recorded as it was actually run), so `resolve --from-manifest`
re-runs the same render step and reproduces a rendered source byte for byte too.

A CRD with a `sink` field required on its `spec`, a `typeMatching` enum and a `conditions` array
renders to something like:

```markdown
# Subscription (messaging.example.com/v1)

A Subscription describes interest in a class of events.

Scope: Namespaced · Plural: subscriptions · Short names: sub, subs · Served: yes · Storage: yes

## Fields

| Field | Type | Required | Values | Description |
|---|---|---|---|---|
| `spec.sink` | string | yes |  | The URL of the subscriber. |
| `spec.typeMatching` | string | no | `exact`, `standard` | How the event type is matched. |

## Conditions

| Type | Description |
|---|---|
| `Ready` | The kind of condition. |
```

The tokeniser indexes dotted or underscored identifiers as a whole term as well as their parts
(SPEC §10.3), so a query for `spec.sink` matches the field path directly and `jwks_urls` matches
`jwks`, `urls` or the joined form.

## Resolvers

A resolver is the answer to "which files in this repository are the documentation". There are
six, one per way of saying it. Every resolver additionally accepts `include` and `exclude` glob
lists (extras on top of, or carved out of, what it selects) and `scope` (which Markdown counts
as residue when the resolver did not select it). Title and section come from the navigation
where there is one, otherwise from the first H1 or the frontmatter `title`; `doc_type` comes
from the section title via one fixed heuristic (troubleshooting / tutorial / reference /
release-notes / concept, in that precedence, else `concept`).

| `resolver.type` | reads | selects | options |
|---|---|---|---|
| `glob` | nothing but the file tree | files matching `include` minus `exclude`; Markdown only by default | `extensions` (default `["md"]`; `[]` means every file; defaults to every file when the source has a `render` step) |
| `vitepress` | `docs/.vitepress/config.*`, or the `_sidebar.ts` given by `path` | the pages the sidebar links, with its titles and sections | `path`, `scope` |
| `docusaurus` | `sidebars.js` or `sidebars.ts` | doc ids, `{type: 'doc'}`, `{type: 'category'}` (label becomes the section) and `{type: 'autogenerated', dirName}`; ids resolve to `docs/<id>.md` or `.mdx` | `path`, `scope` |
| `mdbook` | `src/SUMMARY.md` | the nested link list; a `# Part` heading becomes the section; a draft `[Title]()` is skipped, neither page nor residue | `path`, `scope` |
| `sitemap` | `sitemap.xml`, or a plain URL list, one per line | URLs mapped to repository paths: the part after `url_prefix` joined to `path_prefix`; `.html` becomes `.md`; a trailing slash falls back to `README.md`, then `index.md` | `path`, `scope`, `url_prefix` and `path_prefix` (required) |
| `external` | whatever the command reads | one JSON line per candidate on the command's stdout (`path`, `title`, `doc_type`, `section`, `selected`); the contract is SPEC.md §3 | `command`, `args`, `residue_mention` (only unselected files whose text matches are reported as residue) |

The navigation resolvers report two things a glob cannot: pages under `scope` that the
navigation does not link (residue, for a decision) and navigation links that point at no file
(unresolved, for the upstream docs team). Link spellings are normalised: a link may omit `.md`,
point at a directory (falls back to its `README.md`), or carry an anchor; VitePress and
Docusaurus files are scanned tolerantly (single or double quotes, escaped quotes), not
executed.

Separate from the resolver, a source may add a `render` step that turns non-Markdown files into
pages: `openapi` is built in (Kubernetes CRDs and OpenAPI schemas become reference pages, see
below), and `external` runs a command with the same one-JSON-line-per-page contract.

## Near-duplicate detection

`pinakes duplicates` finds pages that overlap enough to be worth pruning, and `resolve` runs it
automatically, writing `duplicates.jsonl` next to `residue.jsonl`. Three kinds are reported:

- **exact** — identical `sha256` (the same file, twice).
- **mirror** — the same navigation title or H1 across sources, different bytes (an intentionally
  duplicated page, lightly diverged).
- **near** — distinct titles, but the cleaned text clears the similarity threshold.

Method: the cleaned text (the same frontmatter/comment/link/HTML stripping `eval` uses) is
shingled into 5-token windows; a 128-hash MinHash signature is stored per page; 16 LSH bands of
8 rows produce candidate pairs cheaply, without comparing every page to every other one; exact
Jaccard similarity on the shingle sets is computed for candidates, and pairs at or above
`--threshold` (default `0.8`) are reported alongside the exact and mirror pairs.

```sh
../target/release/pinakes duplicates
```

```json
{"canonical":"handbook::docs/install.md","duplicate":"guides::docs/install.md","kind":"mirror","similarity":0.86,"suggested":"exclude","why":"priority 10 > 1"}
```

**The winner rule** decides which page of a pair is `canonical`, in order: the higher source
`priority`; else a page `selected_by` the resolver beats one `selected_by` a decision, which
beats a bare glob include; else the source with the newer commit (its date fetched from the
GitHub API when reachable, else unknown). When every criterion ties, `suggested` is `"review"`
instead of `"exclude"` — there is no clear canonical page for a person, or `decide`, to prefer.

`decide` accepts a duplicate id the same way it accepts a residue id, and gains
`--superseded-by <ID>`, which defaults `--reason` to `superseded by <ID>` so the common case
needs no separate justification:

```sh
../target/release/pinakes decide guides::docs/install.md exclude \
    --superseded-by handbook::docs/install.md
```

`report` gets a "Duplicates" section grouping pairs by kind, and `duplicates` works on a
manifest-less artifact too (as `eval` does): without a manifest, `sha256` is read straight from
the artifact, `selected_by` is unavailable, and the winner rule falls back to priority alone.

## Measuring retrieval

`eval` builds an in-memory BM25 index over the artifact and runs `queries.jsonl` through it.
The index is the measurement backend only (stage **b** stays with the consumer). Pages are
cleaned of frontmatter, HTML comments, link and image targets and HTML tags, split into the
intro plus one unit per H2 (H2 sections over 1200 tokens split at H3), scored by title (×3),
heading (×2) and body, ranked by their best unit and de-duplicated by tokenised title. The
tokeniser lowercases, keeps `[a-z0-9]+` runs and drops a small stopword list; no stemming.
[Evaluation](docs/evaluation.md) walks through the setup, the query set and the metrics.

```text
--artifact DIR    the artifact to measure (default: artifact next to the config)
--queries FILE    the judge (default: eval.queries from the config)
--k N             result list length (default: eval.k, else 10); recall@10 becomes recall@k below 10
--json OUT        write the result there instead of stdout (what report --eval-before/--eval-after read)
--gate BASELINE   exit 2 when tuning recall@5 drops more than eval.max_recall_drop (default 0.05) below it
--with ID…        add residue pages (_residue/<source>/<path>) before measuring and print the delta
--without ID…     remove pages before measuring and print the delta
```

The table on stderr lists recall@5, recall@10, MRR and n overall and per `kind`, with
`holdout: true` rows in their own split that never feeds a gate. `expected` entries may be
page ids (`handbook::docs/user/x.md`), directory prefixes (`handbook::docs/user/`) or the
legacy `handbook/docs/user` form, which matches the page or anything under that directory on
a path segment boundary.

**Scoring.** tantivy stores the postings; the score is Okapi BM25 with `k1` 1.5, `b` 0.75 and
negative IDFs floored at a quarter of the average IDF, computed with exact unit lengths, and
field boosts act as term-frequency multipliers. This is the common `rank_bm25` BM25Okapi
formula, so results are comparable with that library; tantivy's own scorer (`k1` 1.2, Lucene
IDF, quantised lengths) would rank differently and is not used.

**Mirror rule.** When two sources carry a page with the same title key (navigation title or
H1), only the page from the source with the higher `priority` is indexed; the other is still a
page, just not searchable. Priorities come from `pinakes.yaml` alone (`priority`, default 1).
A source the config does not list, and every source when `eval` runs without a config, gets
that same default, and equal priorities never collapse a page, so a manifest-less artifact
with only `meta.json` files is measured with no mirrors at all. Same-title results are still
de-duplicated at search time, whatever the priorities.

**Golden corpus.** `tests/fixtures/golden` is a synthetic artifact of thirty small pages
across three sources with priorities 10, 5 and 1, sidebar titles in each `meta.json`, three
pages mirrored into lower-priority sources, one frontmatter-only title, one page with H2/H3
sections, a `_residue` directory and (for `tests/duplicates.rs`) a near-duplicate pair across
sources, with twelve queries of which two are held out. `tests/golden.rs` runs `eval` on it,
plain and `--with` one residue page, and compares the
whole result with `expected.json`: today that is tuning recall@5 0.9, recall@10 0.9 and MRR
0.9 over ten rows, 1.0 on the two held-out rows, and 1.0 across the board once the residue
page is in. The numbers are whatever the implementation yields, pinned so that any change to
the index shows up as a diff; `UPDATE_GOLDEN=1 cargo test --test golden` refreshes them after
an intended change.

## Retrieval backends

`eval` measures the built-in BM25 index by default, but stage b (the index itself) is a
consumer's choice, so `eval` can measure five retriever shapes behind the same `Backend`
trait and the same query set, with `--backend NAME`:

```text
--backend bm25            the SPEC §5 index above (default)
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

**`pinakes embed [--artifact DIR] [--model NAME] [--out embeddings.bin] [--batch 64]`**
computes one embedding per retrieval unit — the same intro-plus-H2-sections split `eval`
scores — through an OpenAI-compatible endpoint (`POST {PINAKES_EMBED_URL}/embeddings`,
bearer `PINAKES_EMBED_KEY`, model from `PINAKES_EMBED_MODEL` or `--model`), 64 texts per
request with retries and backoff on HTTP 429 and 5xx. It writes `embeddings.bin`
(little-endian `f32`, row-major) and `embeddings.json` (the model, the vector dimension, the
unit ids in row order, and the artifact's `manifest.json` hash, or `"none"` for a
manifest-less artifact). A missing endpoint is an error, not a silent skip.

None of this says which shape to run in production — that is what the numbers `eval
--compare` prints are for, not an argument about architecture.

## Managing the query set

`queries.jsonl` is the judge `eval` measures against, so it needs to grow carefully and keep a
representative held-out slice rather than drift into a set that only checks what already works.
`pinakes queries add` appends one row after checking that every `--expected` id actually exists
in the committed manifest:

```sh
pinakes queries add --id howto-enable-caching \
  --query "How do I enable caching?" \
  --expected handbook::docs/user/tutorials/01-40-enable-caching.md \
  --kind howto
```

`--expected` accepts a page id, an id prefix ending in `/` for "any page under", or the legacy
`<source>/<path>` form; the legacy form is normalised to the canonical `<source>::<path>` (or
`<source>::<path>/` when it only matches pages as a directory) before the row is written. An id
that matches no page in the manifest is rejected and nothing is appended. Add `--holdout` to
keep a row out of tuning decisions; it is still reported by `eval`, just never used to gate.

`pinakes queries check` re-validates the whole file against the manifest and fails (exit 4) on
an unknown expected id, a duplicate query id, or a held-out share below `eval.holdout_min`
(default 0.2, set in the `eval:` section of `pinakes.yaml`) — the guardrail against quietly
tuning away the very queries meant to catch overfitting:

```sh
pinakes queries check
```

## Classifying residue with a model

Reviewing every residue entry and near-duplicate pair by hand does not scale. `pinakes
classify` sends them to an OpenAI-compatible chat completions endpoint in batches and turns the
model's verdicts into ordinary `decisions.jsonl` lines, the same file `decide` and the
[curation skill](#curating-with-an-agent) write to:

```sh
export PINAKES_LLM_URL=https://api.openai.com/v1     # or any OpenAI-compatible endpoint
export PINAKES_LLM_KEY=sk-...                        # optional; omitted when the endpoint needs none
export PINAKES_LLM_MODEL=gpt-4o-mini                 # or pass --model
pinakes classify --dry-run                           # print the proposed decisions, write nothing
pinakes classify --batch 20                          # write them to decisions.jsonl
```

`PINAKES_LLM_URL` is required — a missing endpoint is an error, never a silent skip. Candidates
are every undecided residue entry plus every near-duplicate pair's `duplicate` id from
`duplicates.jsonl` that has no active decision yet, sent in batches of `--batch` (default 20)
with id, title, excerpt, reason and context (a near-duplicate candidate's context names its
canonical id and similarity instead of a sidebar section). The model is asked for a JSON array
of `{"id", "decision": "include"|"exclude"|"unsure", "rationale", "confidence"}` at temperature
0, retried up to three times on a `429` or `5xx` response. Decisions are written with
`by: "classifier:<model>"`, so `report` and `residue list` show exactly who decided what.

Two rules hold regardless of what the model says: a residue page whose reason is `new_source` is
never written as `include` (a model proposing it is recorded as `unsure` instead, so a person
still looks at genuinely new sources before they enter the corpus), and `unsure` never enters
the manifest — `resolve` keeps treating an `unsure` page as residue, exactly as it does for a
human's `unsure` verdict from `decide`.

## Curating with an agent

Reviewing `residue list` and `duplicates` by hand does not scale much past the first few
sources. [`skills/curate/SKILL.md`](skills/curate/SKILL.md) is a Claude Code skill that runs a
curation session through the CLI only: it never edits `manifest.json` or the artifact directly,
reads `residue list`, `duplicates` (when present), `diff` and `report`, groups candidates by
reason, proposes a decision with a one-sentence rationale in batches of 20, and applies them
with `pinakes decide ... --by <name>` only once a batch is confirmed. It never proposes
excluding a page whose `selected_by` is `resolver`, and it finishes by rendering `report` so the
session leaves a clear record of what changed.

See [`skills/README.md`](skills/README.md) for how to install a skill from this repository into
a project (a `.claude/skills` symlink or a one-off copy), then ask the agent to curate the
corpus.

## Weekly curation workflow

[`.github/workflows/curate.yml`](.github/workflows/curate.yml) in this repository is a reusable
[`workflow_call`](https://docs.github.com/actions/using-workflows/reusing-workflows) workflow
that runs the sequence above on a schedule. It installs the `pinakes` binary (the latest
matching release asset in a consumer repository; `cargo install --path .` when it runs inside
this repository's own CI, detected from `github.workflow_ref` rather than an extra input),
reproduces the committed manifest with `resolve --from-manifest` to measure "before", resolves
fresh sources, diffs the two manifests and stops early once nothing changed, measures "after"
(gated against a baseline only when one is already committed, and never failing the job on a
drop — the point is a reviewable PR, not a silently skipped one), runs `duplicates` when the
installed binary has that subcommand (checked with `--help` first), renders `report`, and opens
or updates one pull request on branch `pinakes/weekly` with the manifest, residue, duplicates
and report committed, using the report as the PR body via
[`peter-evans/create-pull-request`](https://github.com/peter-evans/create-pull-request).

A consumer calls it from its own repository, on whatever schedule it likes:

```yaml
# .github/workflows/curate.yml
name: curate
on:
  schedule: [{ cron: "0 6 * * 1" }]
  workflow_dispatch:
jobs:
  curate:
    uses: OWNER/pinakes/.github/workflows/curate.yml@v1
    with:
      config: pinakes.yaml
      queries: queries.jsonl
      baseline: eval-baseline.json
    permissions:
      contents: write
      pull-requests: write
```

See [`examples/curate-weekly.yml`](examples/curate-weekly.yml) for the full, copy-pasteable
file. Reviewers (or the [curation skill](#curating-with-an-agent) above) read the report, run
`pinakes residue list` for anything new, record `decide` verdicts, and merge; a consumer's build
stage then runs `pinakes resolve --from-manifest manifest.json` to materialise exactly the
reviewed corpus.

## Setup action for GitHub Actions

[`action.yml`](action.yml) at the repository root is a composite action, "Set up pinakes", for
any workflow — a consumer's own CI, not only the curation workflow above — that just wants the
`pinakes` binary on `PATH`. It picks the right release asset for the runner (Linux x86_64 glibc
or musl, Linux aarch64, or Apple silicon macOS; there is no Intel Mac build), downloads it,
verifies it against the release's `SHA256SUMS`, and adds it to `PATH`:

```yaml
- uses: friedrichwilken/pinakes@v1
  with:
    version: latest        # or a specific "X.Y.Z", without a leading "v"
    github-token: ${{ github.token }}   # avoids the anonymous GitHub API rate limit
    musl: false             # true picks x86_64-unknown-linux-musl over the glibc build
- run: pinakes --version
```

`v1` is a moving tag: `release.yml` force-updates it to the commit of every `v1.x.y` release, so
pinning to `@v1` tracks the latest compatible release automatically, the way `actions/checkout@v4`
does. Pin a specific tag (`@v1.0.1`) instead for a fully reproducible workflow. `curate.yml`
above uses this same action when it runs outside this repository, and falls back to
`cargo install --path .` when it runs inside it.

## Learning from serving: the trail, grade, usage

Everything so far tunes the corpus against a judge (`queries.jsonl`) that a person or a model
wrote ahead of time. Stage **c** (serving, a consumer's own job) sees something pinakes never
does: what real users actually asked and whether the pages retrieved for them were any good.
`trail.jsonl` is the bridge back — a consumer-written log pinakes only ever reads:

```json
{"at": "2026-09-16T12:00:00Z", "query": "how do I enable caching", "retrieved": ["handbook::docs/user/caching.md", "handbook::docs/user/quotas.md"], "ranks": [1, 2], "cited": ["handbook::docs/user/caching.md"], "outcome": "ok", "session": "s1"}
```

Every field but `at` and `query` is optional — a consumer that only logs the query text and
what it retrieved still gets useful output. See [`examples/trail.jsonl`](examples/trail.jsonl)
for a dozen realistic lines against the [example corpus](#a-real-run); because it only reads the
committed `manifest.json`, `pinakes usage --trail examples/trail.jsonl` (run from `examples/`)
works offline, with no `resolve` needed first.

**Grading what was served.** `pinakes grade` replays every distinct query in the trail against
a backend (today, always the built-in BM25 index — `--backend` accepts no other name until
[SPEC §16.1](SPEC.md)'s backend trait lands), fetches `--k` candidates (default 20) per query,
and asks the same OpenAI-compatible model endpoint as `classify` to grade each candidate 0
(irrelevant) to 3 (fully relevant):

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

**Usage statistics.** `pinakes usage` turns the trail into four things a judge alone cannot
show: pages the manifest carries that the trail never retrieved in the window (removal
candidates), pages retrieved but never cited, queries that got no citation at all, and — for
each such query — the best-scoring leftover page from `_residue`, found with a small BM25-like
index over the tokenised, cleaned residue text (a possible gap candidate: something worth
promoting out of residue). `--since` narrows the window (`30d`, `12h`, `45m`, `90s`; omit it to
use the whole file):

```sh
pinakes usage --trail trail.jsonl --since 30d --json usage.json
pinakes report --usage usage.json   # adds a "Usage" section to the PR body
```

`report` renders the "Usage" section only when `--usage` is given, so every existing report
(and every snapshot in `tests/snapshots/`) is unaffected by a consumer that has not started
logging a trail yet.

## Development

```sh
just check      # the offline CI gates: rustfmt check, clippy, rustdoc, all tests
just install    # cargo install --path . --locked; puts `pinakes` on PATH
just            # every recipe: build, release, fmt, lint, test, e2e, update-golden, wheel, audit, skill
```

Without [`just`](https://github.com/casey/just), the gates are:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Library crate `pinakes` (modules per SPEC §9), binary `pinakes` (`src/main.rs`, clap
only). Errors are `thiserror` types in the library and `anyhow` at the CLI edge. Report
snapshots live in `tests/snapshots/`; `UPDATE_SNAPSHOTS=1 cargo test` refreshes them. The
golden corpus check in `tests/golden.rs` runs with the rest of the suite;
`UPDATE_GOLDEN=1 cargo test --test golden` re-pins its expected result.

**Unit tests versus e2e.** `cargo test` is offline and deterministic: unit tests in the
modules, the integration tests in `tests/` against fake fetchers and scripts, the golden
corpus and the report snapshots. None of them touch the network, so they say nothing about
whether the codeload download, the GitHub archived check or the example resolver still work
against the real world. That is what the `e2e` job in CI covers: it builds the release
binary and runs `resolve`, `verify`, `diff`, `eval` and `report` on `examples/pinakes.yaml`
against the two public repositories it names. It fails only on exit codes the commands do not
document, so an upstream commit that changes the corpus (exit 3 from `diff`) is reported,
not treated as a failure. Run it locally with the commands under [A real run](#a-real-run).

**CI** (`.github/workflows/ci.yml`, on pushes to `main` and pull requests): `lint`
(`cargo fmt --check`, clippy with `-D warnings`, `cargo doc` with warnings denied); `test`
on Ubuntu and macOS (`cargo test --all-targets` plus the doctests); `msrv` (a build on the
`rust-version` from `Cargo.toml`); `e2e` (the example, as above); `audit` (`cargo audit`,
also weekly on a schedule). Dependabot opens weekly, grouped update PRs for Cargo and the
Actions. See [`CONTRIBUTING.md`](CONTRIBUTING.md) and [`AGENTS.md`](AGENTS.md).

## Releases

A release is a tag `vX.Y.Z` on `main`, matching the `version` in `Cargo.toml`. Pushing the
tag runs `.github/workflows/release.yml`, which builds release binaries for
`x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl` (static), `aarch64-unknown-linux-gnu`
and `aarch64-apple-darwin` (Apple silicon only; Intel Macs are not supported), packages each as
`pinakes-X.Y.Z-<target>.tar.gz` (the binary, `README.md`, `LICENSE` and `SPEC.md`), builds the
Python wheel (see [Python bindings](#python-bindings) below) for the three platforms it
supports, writes a `SHA256SUMS` file and attaches everything to a GitHub release created from
the tag. Nothing is published to crates.io or PyPI.

```sh
git tag -a v0.1.0 -m "pinakes 0.1.0"
git push origin v0.1.0
```

## Python bindings

`python/` is a PyO3 crate (`pinakes-py`, module `pinakes`) built with maturin, wrapping the same
`Index` that `pinakes eval` scores the corpus with (SPEC §17.2): whatever ranking the curator
measured is exactly what this import searches, not a reimplementation of it. `release.yml`
attaches wheels for `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` and
`aarch64-apple-darwin` to each [release](#releases) as `abi3` builds (CPython 3.10+); nothing is
published to PyPI, so install the wheel for your platform directly from the release assets:

```sh
pip install pinakes-0.1.0-cp310-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl
```

```python
from pinakes import Index

index = Index.build("artifact")
hits = index.search("how do I install the service", k=3)
page = index.read(hits[0].page_id)
print(page.title, page.url)
```

## Licence

Apache-2.0.
