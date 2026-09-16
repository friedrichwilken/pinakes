# pinakes — specification, iteration 1

Binary: `pinakes`. Language: Rust (2024 edition), library crate plus thin CLI.
Licence: Apache-2.0.

## 1. Purpose

A retriever-agnostic tool that turns a declared set of documentation sources into a
reproducible, measured corpus for a retrieval system, and keeps it that way over time.

Three stages exist around it. pinakes is stage **a**:

- **a. compile** — sources → curated corpus (artifact + manifest + residue). *This tool.*
- **b. build** — corpus → an index (in-process BM25, a vector store, …). *A consumer's job.*
  pinakes contains one built-in BM25 backend, used only for measurement.
- **c. serve** — the index behind an agent or API. *A consumer's job.*

Everything the tool reads and writes is a plain file that belongs in git. No database, no daemon,
no hidden state. Commands compose through files, JSONL on stdin/stdout, and exit codes.

## 2. Files (the contracts)

### 2.1 `pinakes.yaml` — source config (human-written)

```yaml
version: 1
sources:
  - name: handbook                   # directory name in the artifact; [A-Za-z0-9_-]+
    repo: https://github.com/example-org/handbook.git   # GitHub only in v1
    ref: main                        # branch, tag or SHA; resolved to a SHA at resolve time
    priority: 10                     # higher wins when two sources carry the same title (mirrors); default 1
    resolver:
      type: glob                     # "glob" | "external"
      include: ["docs/user/**/*.md"]
      exclude: ["**/_sidebar.md"]
  - name: guides
    repo: https://github.com/example-org/guides.git
    ref: main
    priority: 1
    resolver:
      type: external
      command: ["python3", "resolvers/toc.py"]   # run with cwd = repo checkout
      args: ["--title-match", "(?i)handbook"]
      residue_mention: "(?i)handbook"  # only leftovers whose text matches are reported
policy:
  deny: ["**/CLAUDE.md", "**/adr/**", "**/CHANGELOG.md"]   # beats everything
  archived: warn                     # warn | drop
  min_pages_per_source: 1            # verify fails below this
eval:
  queries: queries.jsonl
  k: 10
  max_recall_drop: 0.05              # eval --gate exits 2 beyond this
```

Precedence for a file: `policy.deny` > source `resolver.exclude` > decisions > resolver selection.

### 2.2 `manifest.json` — curated references (machine-written, committed)

```json
{
  "version": 1,
  "generated_at": "2026-09-16T12:00:00Z",
  "sources": {
    "handbook": {
      "repo": "example-org/handbook",
      "repo_url": "https://github.com/example-org/handbook.git",
      "ref": "main",
      "commit": "4427d7ba863973c2cea9da74ed8675c5c74aee77",
      "archived": false,
      "resolver": "glob",
      "pages": {
        "docs/user/README.md": {
          "sha256": "…",
          "title": "Handbook",
          "doc_type": "concept",
          "section": "",
          "selected_by": "resolver"          // resolver | include | decision
        }
      },
      "residue": ["docs/user/00-15-overview-setup.md"],
      "unresolved": [],
      "render": {"type": "openapi"}          // absent when the source has no render step (§10.1)
    }
  }
}
```

Page identity everywhere is `<source name>::<path>`. `sha256` is of the file bytes.
Sorted keys, two-space indent, trailing newline, so diffs are readable.

A source's `render` step, when configured, is recorded per source (`{"type": "openapi"}` or
`{"type": "external", "command": [...], "args": [...]}`, absent when there is none) so
`resolve --from-manifest` can re-run it without the original config's resolver at hand; an
external command's `command`/`args` are recorded as they were actually run — absolutised
against the config file at resolve time — so reproduction finds the same program regardless of
where the manifest is later reproduced from.

### 2.3 The artifact directory (materialised, not committed)

```
<artifact>/
  manifest.json
  <source>/…/<page>.md          # selected pages, original relative paths
  <source>/meta.json            # {repo, module: <source name>, base_url, commit, pages: {path: {title, doc_type, section}}, residue, unresolved}
  _residue/<source>/…/<page>.md # leftovers, for excerpts and measurement
```

This layout is a stable contract that consumers rely on; keep it exact.
`base_url` is `https://github.com/<owner>/<repo>/blob/<commit>`.

### 2.4 `residue.jsonl` — what was left out (machine-written, reviewable)

One object per line:
`{"id": "handbook::docs/user/x.md", "source": "handbook", "path": "docs/user/x.md", "reason": "not_selected", "sha256": "…", "title": "…", "excerpt": "first ~600 tokens", "context": "sidebar section or TOC branch if the resolver gave one"}`

Reasons: `not_selected`, `unresolved_link` (a navigation link with no file), `new_source`.

### 2.5 `decisions.jsonl` — verdicts on residue (human- or agent-written, committed)

`{"id": "handbook::docs/user/x.md", "sha256": "…", "decision": "include" | "exclude" | "unsure", "reason": "one sentence", "by": "name or agent", "at": "2026-09-16T12:00:00Z"}`

A decision applies only while the page's `sha256` matches; otherwise it is reported as expired
and the page is residue again. Later lines override earlier ones for the same id.

### 2.6 `queries.jsonl` — the judge (human- or grader-written, committed)

`{"id": "howto-enable-caching", "query": "How do I enable caching?", "expected": ["handbook::docs/user/tutorials/01-40-enable-caching.md", "handbook::docs/user/"], "kind": "howto", "holdout": false}`

`expected` entries are page ids or id prefixes (a trailing `/` means "any page under").
`holdout: true` rows are reported separately and never used for tuning decisions.

### 2.7 `report.md` — rendered PR body

Sections, in order: summary counts; eval before/after (overall and per kind, held-out separately);
added pages; removed pages (with reason: gone upstream, dropped by resolver, excluded by decision);
changed pages (hash changed; link to upstream compare when both commits known); new residue grouped
by reason with excerpt; expired decisions; unresolved links; archived sources.

## 3. External resolver contract

pinakes runs `command + args` with cwd = the checked-out repository, env `PINAKES_SOURCE=<name>`,
`PINAKES_COMMIT=<sha>`. The command writes JSONL to stdout, one object per candidate:

`{"path": "docs/user/x.md", "title": "…", "doc_type": "concept|tutorial|reference|troubleshooting|release-notes|", "section": "…", "selected": true}`

- `selected: false` lines are residue candidates with context; files under the resolver's scope that
  the command never mentions are residue too if `residue_scope` (optional glob list in config) covers them.
- Exit code ≠ 0 fails the resolve for that source with the command's stderr in the message.
- Missing `title` → pinakes takes the first H1, then a frontmatter `title:`, else empty.

## 4. Commands

All commands take `--config pinakes.yaml` (default) and print human output to stderr, data to stdout.

| command | input | output | exit codes |
|---|---|---|---|
| `resolve [--artifact DIR] [--from-manifest M]` | config (or a manifest to reproduce) | artifact dir, `manifest.json`, `residue.jsonl` | 0 ok; 1 error |
| `diff OLD.json NEW.json` | two manifests | JSON on stdout (added/removed/changed/sources) and a human summary on stderr | 0 same; 3 differences |
| `residue list [--source S] [--reason R]` | `residue.jsonl` (+ decisions to hide decided ones) | JSONL | 0 |
| `decide ID include\|exclude\|unsure --reason "…" [--by NAME]` | residue + manifest for the hash | appends to `decisions.jsonl` | 0; 1 unknown id |
| `report [--old M] [--new M] [--eval-before E] [--eval-after E]` | manifests, residue, decisions, eval json | `report.md` on stdout | 0 |
| `eval [--artifact DIR] [--json OUT] [--gate BASELINE.json]` | artifact, queries | table on stderr, json on stdout | 0; 2 gate failed |
| `eval --with ID… / --without ID…` | as above | delta for adding/removing pages | 0 |
| `verify [--artifact DIR]` | config, committed manifest | nothing | 0; 3 manifest stale; 4 policy violation |

`resolve` downloads codeload tarballs (no git needed), reads the resolved commit from the tarball's
pax `comment` header, falls back to the wrapper directory suffix; unauthenticated, `GITHUB_TOKEN`
used when present; checks `archived` via the GitHub API, degrading to unknown on network errors.

## 5. Built-in BM25 backend (measurement only)

- tantivy, in-memory index built from the artifact at `eval` time.
- Units: the page intro (text before the first H2) and one unit per H2 section; H2 sections over
  1200 tokens are split at H3. Fields: `title` (boost 3), `heading` (boost 2), `body`, plus
  stored `page_id`, `source`, `doc_type`.
- Page score = max unit score. Results are de-duplicated by tokenised title; when two sources carry
  the same title (nav title or H1), only the higher `priority` source's page is indexed (mirror rule).
  Priorities come from `pinakes.yaml` alone (`priority`, default 1): a source the config does not
  list, and every source when there is no config, has priority 1, and equal priorities never
  collapse a page, so a manifest-less artifact is measured with no mirrors at all. The same-title
  de-duplication at search time still applies.
- Scoring is Okapi BM25 with `k1` 1.5, `b` 0.75 and negative IDFs floored at a quarter of the
  average IDF; field boosts multiply the term frequency and the unit length. This is the common
  `rank_bm25` BM25Okapi formula, so results are comparable with that library.
- Tokeniser: lowercase, alphanumeric runs, a small English stopword list, no stemming.
- Frontmatter, HTML comments, link targets, image targets and HTML tags are stripped before indexing.

The golden corpus of §7.3 pins what these rules yield; any change to them shows up as a diff of
the pinned result.

## 6. Metrics

`eval` reports, overall and per `kind`, for tuning rows and held-out rows separately:
recall@5, recall@10, MRR (rank of the first expected hit), n. Per-query rows in the JSON:
`{id, kind, holdout, hit5, hit10, rr, top: [page ids]}`.

`--with`/`--without` re-index with the page(s) added from `_residue` or removed, and print the
per-metric delta and the queries whose reciprocal rank changed.

## 7. Definition of done for v1

1. `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` green; docs on every public item.
2. Unit tests for: config parsing and precedence; manifest read/write round trip and sorted output;
   external resolver contract (a fake script); glob resolver; residue reasons; decisions expiry;
   diff; report rendering (snapshot); tokeniser and section splitting; eval metrics on a tiny corpus.
3. Golden corpus: `tests/fixtures/golden` holds a synthetic artifact of about thirty small
   Markdown pages across three sources with different priorities, sidebar-like titles in each
   `meta.json`, deliberate mirrors across sources, one frontmatter-only title, one page with H2/H3
   sections and a `_residue` directory, plus a `queries.jsonl` of twelve queries of which two are
   held out. `expected` entries may be `<source>::<path>`, `<source>::<dir>/` or the legacy
   `<source>/<path>` prefix form. `eval` on it (plain, and `--with` one residue page) must yield
   exactly the result pinned in `expected.json`; the expected metrics are whatever the
   implementation yields, refreshed with `UPDATE_GOLDEN=1 cargo test --test golden` after an
   intended change and reviewed in the diff. `eval` accepts a manifest-less artifact (it reads
   `meta.json` per source), so the fixture carries no manifest.
4. `resolve` on a two-source config (a small public repo with a glob resolver, and one with an
   external resolver script in `examples/`) produces the artifact layout of §2.3 and a manifest that
   `verify` accepts; `resolve --from-manifest` reproduces it byte for byte.
5. README with the three-stage model, the file contracts, and the GitHub Action sketch
   (resolve → diff → eval → report → PR).

## 8. Out of scope for v1 (do not build)

LLM classification of residue; telemetry ingestion and the grader; dense/external retrieval
backends; Python bindings; built-in sidebar/sitemap/OpenAPI resolvers; TUI; the GitHub Action
itself (only a sketch in the README).

## 9. Conventions

- Crate layout: `src/lib.rs` with modules `config`, `sources` (download, archived check), `resolve`
  (glob, external), `artifact`, `manifest`, `residue`, `decisions`, `diff`, `report`, `index`
  (tokeniser, sections, tantivy), `eval`; `src/main.rs` with clap subcommands only.
- Errors: `anyhow` at the CLI edge, `thiserror` types in the library. No panics on user input.
- Serialisation: `serde` + `serde_json` (`to_writer_pretty` with sorted maps: use `BTreeMap`),
  `serde_yaml` for the config.
- HTTP: `ureq` (blocking is fine; the tool is a batch CLI). Tar: `tar` + `flate2`.
- Globs: `globset`. Regex: `regex`. Hashing: `sha2`. Time: `time` or `jiff`, RFC 3339 UTC.
- Line length 100, `rustfmt` defaults, `clippy::pedantic` where it does not fight readability.
- Commit per step, imperative subject, body says what and why. Trailer:
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`

---

# pinakes — specification, iteration 2

Iteration 1 compiles and measures. Iteration 2 adds content that is not prose, better
selection, judgement on top, the loop from serving, retriever shapes, and delivery. Every item
below is a contract; the code follows it, and a change to a contract is made here first.
Everything in iteration 1 stays valid; additions are backwards compatible: new optional keys,
new commands, new reasons.

## 10. Render hook and non-Markdown content

### 10.1 Per-source `render` (config)

```yaml
sources:
  - name: my-operator
    repo: https://github.com/example/my-operator.git
    ref: main
    resolver:
      type: glob
      include: ["config/crd/bases/*.yaml"]
    render:
      type: external             # "external" | "openapi"
      command: ["python3", "render/crd.py"]   # external only; relative to the config file
      args: []
```

A render step runs after selection and before the artifact is written. It receives the
selected files and produces Markdown pages; one input may become several pages.

**External render contract.** The command runs with cwd = the checked-out repository, env
`PINAKES_SOURCE`, `PINAKES_COMMIT`, `PINAKES_OUT` (a staging directory it must write into).
It reads the selected paths as JSON lines on stdin (`{"path": "…"}`) and writes JSON lines on
stdout, one per generated page:
`{"path": "reference/widgets.example.com.md", "source_path": "config/crd/bases/x.yaml", "title": "…", "doc_type": "reference", "section": "…"}`
where `path` is relative to `PINAKES_OUT` and becomes the page path in the artifact. Exit ≠ 0
fails the source with stderr in the message. Selected files that no output line names are
dropped from the artifact and listed in `meta.json` under `unrendered`.

**Manifest.** A rendered page carries `"rendered_from": "<source_path>"` and its `sha256` is of
the *rendered* Markdown. `diff` therefore reports a schema change as a changed page. The source's
`render` configuration itself is also recorded, per source, next to `resolver` (§2.2): `{"type":
"openapi"}`, or `{"type": "external", "command": [...], "args": [...]}` with the command's paths
absolutised against the config file as they were actually run. `resolve --from-manifest` reads
this back and re-runs the render step — an external command included — before materialising, so
a rendered source reproduces byte for byte the same as any other.

### 10.2 Built-in `openapi` renderer

Input: YAML or JSON files containing either a Kubernetes `CustomResourceDefinition` (any file
with `kind: CustomResourceDefinition`, one or more documents per file) or an OpenAPI 3.x
document. Output: one page per kind and served version (CRD) or per schema under
`components.schemas` (OpenAPI).

Page format (Markdown), in this order:

```
# <Kind> (<group>/<version>)

<spec.versions[].schema.openAPIV3Schema.description, or "">

Scope: <Namespaced|Cluster> · Plural: <plural> · Short names: <a, b> · Served: yes · Storage: yes

## Fields

| Field | Type | Required | Values | Description |
|---|---|---|---|---|
| `spec.sink` | string | yes | | The URL of the subscriber… |
| `spec.typeMatching` | string | no | `exact`, `standard` | … |

## Status

| Field | Type | Values | Description |
…

## Conditions

| Type | Description |
…   (from status.conditions[].type enum or description when present)
```

Rules: field paths are dotted, arrays as `items[]`, nested objects flattened; `additionalProperties`
maps as `<path>.*`; `x-kubernetes-preserve-unknown-fields` noted in Values; descriptions are
single-line (whitespace collapsed). Title = `<Kind> (<group>/<version>)`, doc_type `reference`,
section `<group>`. One page per served version; the storage version comes first in `list` order.

### 10.3 Tokeniser rule for identifiers

The tokeniser (§5) emits, for a run that contains `.` `/` `_` or `-` between alphanumerics, both
the split parts and the joined form with separators removed: `spec.sink` → `spec`, `sink`,
`specsink`; `jwks_urls` → `jwks`, `urls`, `jwksurls`. Queries are tokenised the same way, so
`spec.sink` matches the whole path first and the parts second. Golden metrics are re-pinned
once; the commit says so.

## 11. Near-duplicate detection

Command: `pinakes duplicates [--artifact DIR] [--threshold 0.8] [--json OUT]`, also run by
`resolve` (writes `duplicates.jsonl` next to `residue.jsonl`).

Method: per page, the cleaned text (§5 cleaning) is shingled into 5-token windows; a 128-hash
MinHash signature is stored; candidate pairs come from 16 LSH bands of 8 rows; exact Jaccard on
shingle sets is computed for candidates; pairs at or above the threshold are reported. Exact
duplicates (same sha256) and same-title mirrors are reported too, with `kind` set accordingly.

`duplicates.jsonl`, one line per pair, canonical first:
`{"kind": "exact"|"mirror"|"near", "similarity": 0.93, "canonical": "<id>", "duplicate": "<id>", "why": "priority 10 > 1; linked from navigation; newer commit", "suggested": "exclude"}`

Winner rule, in order: higher source `priority`; page `selected_by` = `resolver` beats `include`;
newer source commit date (from the GitHub API when available, else unknown). Ties report
`"suggested": "review"`. `report` gets a "Duplicates" section; `decide` accepts the duplicate id
as usual and `--reason` defaults to `superseded by <canonical>` when `--superseded-by` is given.

## 12. Built-in resolvers

New `resolver.type` values, each selecting the pages a navigation file links and reporting
unlinked Markdown in scope as residue, with title and section from the navigation:

| type | file (default `path`) | notes |
|---|---|---|
| `vitepress` | `docs/.vitepress/config.*` or a `_sidebar.ts` given by `path` | tolerant object-literal scan (`text`, `link`, `items`) |
| `docusaurus` | `sidebars.js` / `sidebars.ts` | categories → section; `doc` ids resolved to `docs/<id>.md(x)` |
| `mdbook` | `src/SUMMARY.md` | nested list of links; part titles → section |
| `sitemap` | `sitemap.xml` or a URL list file | maps URLs to repository paths via `url_prefix` → `path_prefix` in config |

Each has a `scope` glob list (default: the directory of the navigation file, `**/*.md`) used to
compute residue. Doc type comes from the section title with the same heuristic as §2.4's
context: troubleshooting / tutorial / reference / release-notes / concept.

## 13. Diff and report detail

`diff` adds, per changed page, `lines_added` and `lines_removed` (computed from the two
versions; the old version is re-fetched from the old commit when the artifact of the old
manifest is not present, using `resolve --from-manifest` machinery), and per source a `compare_url`
(`https://github.com/<owner>/<repo>/compare/<old>...<new>`). `report` prints both.

## 14. Judgement on top

### 14.1 The skill

`skills/curate/SKILL.md` in the repository: a Claude Code skill that drives a curation session
through the CLI only. It reads `residue list`, `duplicates`, `diff` and the current `report`,
groups candidates by reason, proposes a decision with a one-sentence rationale per candidate,
applies them with `decide` after the user confirms a batch, and finishes with `report`. It never
edits `manifest.json` or the artifact. The file documents the exact commands, the batch size
(20), and the rule that resolver-selected pages are never excluded by the skill.

### 14.2 Batch classifier

Command: `pinakes classify [--model NAME] [--batch 20] [--dry-run]`. Uses an OpenAI-compatible
chat completions endpoint: `PINAKES_LLM_URL` (base URL), `PINAKES_LLM_KEY`, `PINAKES_LLM_MODEL`
(or `--model`). Sends undecided residue and near-duplicate candidates in batches with title,
excerpt, reason, context, and asks for JSON: `{"id", "decision": "include|exclude|unsure",
"rationale", "confidence": 0..1}`. Writes decisions with `by: "classifier:<model>"`. Never
writes `include` for `new_source` reasons; `unsure` never enters the manifest. Temperature 0.
A missing endpoint is an error, not a silent skip.

### 14.3 Queries

`pinakes queries add --id ID --query TEXT --expected ID… [--kind K] [--holdout]` appends to
`queries.jsonl` after checking that expected ids exist in the manifest (prefix form allowed).
`pinakes queries check` fails on unknown ids and on a held-out share below `eval.holdout_min`
(default 0.2, config). `eval --gate` uses tuning rows only, as before; held-out is reported.

## 15. The loop from serving

### 15.1 Trail format (`trail.jsonl`, written by consumers)

`{"at": "2026-09-16T12:00:00Z", "query": "…", "retrieved": ["<id>", …], "ranks": [1,2,…], "cited": ["<id>"], "outcome": "ok"|"bad"|"unknown", "session": "opaque"}`

Consumers write this; pinakes only reads it. Ids are `<source>::<path>`.

### 15.2 Grader

`pinakes grade --trail trail.jsonl [--backend NAME] [--k 20] [--model NAME] [--out graded.jsonl]`
replays each distinct query, fetches k candidates from the chosen backend, asks the model
(§14.2 endpoint) to grade each candidate 0–3 for relevance, and writes
`{"query", "id", "grade", "model", "at"}`. `pinakes queries import graded.jsonl --min-grade 2`
turns grades into query rows (expected = ids at or above the grade; `holdout` chosen at random
to keep the held-out share), with `"by": "grader:<model>"` on the row. Provenance is kept.

### 15.3 Usage statistics

`pinakes usage --trail trail.jsonl [--since 30d]` prints, and writes with `--json`: pages never
retrieved in the window (removal candidates), pages retrieved but never cited, queries with no
citation grouped by their top retrieved page, and for each uncited query the best residue page
by BM25 score (gap candidates). `report` gets a "Usage" section when a usage JSON is given.

## 16. Retriever shapes

### 16.1 Backend interface

`trait Backend { fn build(artifact, config) -> Self; fn search(query, k, module) -> Vec<Hit> }`
with implementations selected by `--backend`: `bm25` (default, §5), `bm25-tantivy` (tantivy's
own scorer, k1 1.2, b 0.75), `dense`, `hybrid`, `external`. `eval` accepts `--backend` and
reports the backend in its JSON; `eval --compare bm25,dense,hybrid` prints one table per backend
on the same query set.

### 16.2 Dense backend (embeddings file)

`pinakes embed [--artifact DIR] [--model NAME] [--out embeddings.bin]` computes one embedding per
retrieval unit (§5 units) through an OpenAI-compatible embeddings endpoint (`PINAKES_EMBED_URL`,
`PINAKES_EMBED_KEY`, `PINAKES_EMBED_MODEL`), batching 64 texts per request, and writes
`embeddings.bin` (little-endian f32 rows) plus `embeddings.json` (model, dimension, unit ids in
row order, artifact manifest hash). The dense backend loads both, embeds the query, scores by
cosine, page score = max unit. Stale embeddings (manifest hash mismatch) are an error unless
`--allow-stale`.

### 16.3 Hybrid

Reciprocal rank fusion of `bm25` and `dense` page rankings, `k = 60`, over the top 50 of each.

### 16.4 External backend

`--backend external --backend-url URL`: `POST <URL>/search` with `{"query", "k", "module"}`,
expecting `{"hits": [{"page_id", "score", "heading"}]}`. Used to evaluate a store a consumer
already runs. Timeouts 30 s; errors fail the eval.

## 17. Delivery

### 17.1 Weekly Action

`.github/workflows/curate.yml` in this repository as a reusable workflow (`workflow_call`) plus
`examples/curate-weekly.yml` showing how a consumer calls it: resolve, diff against the
committed manifest, stop when empty, eval before and after, duplicates, report, then open or
update one PR on branch `pinakes/weekly` with the manifest, residue, duplicates and report
committed. Inputs: config path, queries path, gate baseline path. Uses `peter-evans/create-pull-request`.

### 17.2 Python wheel

`python/` contains a PyO3 crate `pinakes-py` built with maturin exposing `pinakes.Index`:
`Index.build(artifact_dir, priorities: dict[str,int] | None = None)`, `search(query, k=10,
module=None) -> list[Hit]` with `Hit(page_id, score, heading)`, `page_count`, `searchable_count`,
and `read(page_id) -> Page(title, url, module, doc_type, section, content)`. Built in CI for
Linux x86_64/aarch64 and macOS arm64 on tags, attached to the release; not published to PyPI.

## 18. Definition of done for iteration 2

Each item ships with unit tests, docs on public items, a README section, and the gates green.
Golden fixture extended where behaviour changes (schema renderer fixture with one CRD file
containing two versions; a near-duplicate pair; a mdBook and a docusaurus fixture). Metrics on
the existing golden queries must not regress except where §10.3 says they are re-pinned.

## 19. Out of scope for iteration 2

A TUI; a hosted service; PyPI and crates.io publishing; embeddings computed locally without an
endpoint; graph or knowledge-base features.
