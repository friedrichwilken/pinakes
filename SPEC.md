# pinakes — specification, iteration 1

Binary: `pinakes`. Language: Rust (2024 edition), library crate plus thin CLI.
Licence: Apache-2.0.

## 1. Purpose

A retriever-agnostic tool that turns a declared set of documentation sources into a
reproducible, measured corpus for a retrieval system, and keeps it that way over time.

Three stages exist around it. pinakes is stage **a**:

- **a. compile** — sources → curated corpus (artifact + manifest + residue). *This tool.*
- **b. build** — corpus → an index (in-process BM25, a vector store, …). *A consumer's job.*
  pinakes contains one built-in BM25 index, the reference `kanon` measures by default (§5).
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
      extensions: ["md"]             # default; [] means every file regardless of extension
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
  max_recall_drop: 0.05              # kanon eval --gate exits 2 beyond this
  backend: bm25                      # what a bare `kanon eval` measures; --backend overrides
  # backend_url: http://localhost:8080   # for backend: external
  # embeddings: embeddings.bin       # for backend: dense | hybrid, relative to this file
  # compare: [bm25, dense, hybrid]   # one table per backend; wins over `backend` for a bare eval
gates:                               # `check` (§2.11) exits 2 when report.json exceeds a maximum
  undecided_residue_max: 0           # each key is optional; an absent key is a gate that is off
  removed_pages_max: 5
  # expired_decisions_max: 0
  # archived_sources_max: 0
  # unresolved_links_max: 0
  # duplicates_max: 0
```

Precedence for a file: `policy.deny` > source `resolver.exclude` > decisions > resolver selection.

`include` and `exclude` glob lists are accepted by every resolver, not only `glob`: for
`external`, `vitepress`, `docusaurus`, `mdbook` and `sitemap`, files matching `include` are
selected in addition to whatever the resolver's own mechanism selects — with `selected_by:
"include"`, title from the first H1 then frontmatter `title:`, empty `doc_type` and `section`,
and never reported as residue — while `exclude` removes a file from selection exactly as it does
for `glob`. A file the resolver's own mechanism already selects is unaffected by `include`.
Precedence is unchanged: `policy.deny` beats `exclude`, which beats decisions, which beats
selection (resolver or `include`).

`glob`'s `extensions` restricts `include` matches to files whose extension (case-insensitive,
without the dot) is in the list; it does not affect `residue_scope`. Default `["md"]`, so a
plain `include: ["docs/**/*"]` still only selects Markdown; an empty list selects every file
regardless of extension. When the source has a `render` step (§10.1) and `extensions` is not
given explicitly, the default becomes `[]` (every file) instead, since a renderer typically
consumes YAML or JSON rather than Markdown.

The `eval:` block is `kanon`'s (§21): this crate parses it so an existing `pinakes.yaml` keeps
loading, and `kanon` acts on it.

### 2.2 `manifest.json` — curated references (machine-written, committed)

```json
{
  "version": 1,
  "artifact_version": 1,                     // the artifact contract this file follows (§2.8)
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
Sorted keys, two-space indent, trailing newline, so diffs are readable. A JSON Schema of the
file, generated from the code and pinned by a test, lives at
[`docs/schemas/manifest.schema.json`](docs/schemas/manifest.schema.json).

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
  <source>/meta.json            # {artifact_version, repo, module: <source name>, base_url, commit, pages: {path: {title, doc_type, section}}, residue, unresolved, unrendered}
  _residue/<source>/…/<page>.md # leftovers, for excerpts and measurement
```

This layout is a stable contract that consumers rely on; keep it exact.
`base_url` is `https://github.com/<owner>/<repo>/blob/<commit>`. `meta.json` carries the same
`artifact_version` as the manifest (§2.8), so a consumer reading one source directory can check
it without the manifest.

### 2.4 `residue.jsonl` — what was left out (machine-written, reviewable)

One object per line:
`{"id": "handbook::docs/user/x.md", "source": "handbook", "path": "docs/user/x.md", "reason": "not_selected", "sha256": "…", "title": "…", "excerpt": "first ~600 tokens", "context": "sidebar section or TOC branch if the resolver gave one", "url": "https://github.com/example-org/handbook/blob/4427d7ba863973c2cea9da74ed8675c5c74aee77/docs/user/x.md", "rule": {"key": "glob:outside-include", "text": "outside the configured include patterns"}}`

`url` is the page's upstream URL pinned to the fetched commit, `https://github.com/<owner>/
<repo>/blob/<commit>/<path>` — the same `base_url` the source's `meta.json` carries (§2.3),
derived from the manifest's `repo` and `commit`; empty when it cannot be derived (the source's
`repo` does not parse as a GitHub URL). For reason `unresolved_link` there is no file at `path`
to point at, so `url` instead names the navigation file itself (the sidebar, `sidebars.js`,
`SUMMARY.md` or sitemap a built-in resolver read, §12) at the fetched commit; the `glob` and
`external` resolvers have no navigation file, so `url` falls back to the (nonexistent) target
path for them.

Reasons: `not_selected`, `unresolved_link` (a navigation link with no file), `new_source`,
`excluded` (kept out by `policy.deny`, a resolver's `exclude`, a decision, or an archived source
dropped by `policy.archived` — see below).

`rule` names the mechanism that decided, as `{"key": "...", "text": "one sentence"}`. pinakes
assigns these keys itself:

| key | when | text names |
|---|---|---|
| `sidebar:unlinked` | `vitepress`: in scope, not linked from the sidebar | the navigation file |
| `docusaurus:unlinked` | `docusaurus`: in scope, not linked from the sidebar | the navigation file |
| `mdbook:unlinked` | `mdbook`: in scope, not linked from `SUMMARY.md` | the navigation file |
| `sitemap:unlisted` | `sitemap`: in scope, not listed in the sitemap | the navigation file |
| `nav:dangling-link` | any of the four above: linked but the file does not exist (reason `unresolved_link`) | the navigation file |
| `glob:extension` | `glob`: matches `include` but not the configured `extensions` | the configured extensions |
| `glob:outside-include` | `glob`: matches `residue_scope` but not `include` | — |
| `external:not-selected` | `external`: the command reported the path with `selected: false` and no `rule` of its own | — |
| `external:unmatched` | `external`: in `residue_scope` but the command's output never mentioned it at all | — |
| `external:dangling-link` | `external`: the command selected a path that does not exist (reason `unresolved_link`); there is no navigation file to name | — |
| `policy:deny` | matches `policy.deny`, beating every other mechanism | the pattern that matched |
| `resolver:exclude` | matches the source's own `resolver.exclude` | the pattern that matched |
| `decision:exclude` | an active decision with verdict `exclude` (§2.5) | who decided and their reason |
| `source:archived` | the whole source was dropped by `policy.archived: drop` (§2.1); its would-be pages become residue, reason `excluded` | — |
| `source:new` | the source is new since the previous manifest (reason `new_source`), overriding the mechanism-specific key above | — |

The external resolver contract (§3) accepts an optional `rule` on a candidate line, used
verbatim instead of `external:not-selected` when given — `toc:outside-match` and
`tutorials:no-match` are examples a resolver script might use for its own selection logic;
pinakes does not validate these keys. `resolve --from-manifest` (§4) has no resolver plan to
recover the original mechanism from, so its rebuilt residue carries `reproduced:from-manifest`
instead.

Pages kept out by `policy.deny` or a resolver's `exclude` were previously dropped with no
record; they are now residue (reason `excluded`) so nothing disappears without a trace. They are
never candidates for `decide` (precedence, §2.1, checks `policy.deny` and `exclude` before any
decision, so one would have no effect anyway); `residue list` hides them by default (`--include-
excluded` shows them, and `--reason excluded` shows them regardless), and `report` (§2.7) always
accounts for their count in a collapsed block.

`residue.jsonl` is written sorted by `(source, path)`, so re-running `resolve` against
unchanged inputs reproduces the file byte for byte.

### 2.5 `decisions.jsonl` — verdicts on residue (human- or agent-written, committed)

`{"id": "handbook::docs/user/x.md", "sha256": "…", "decision": "include" | "exclude" | "unsure", "reason": "one sentence", "by": "name or agent", "at": "2026-09-16T12:00:00Z"}`

A decision applies only while the page's `sha256` matches; otherwise it is reported as expired
and the page is residue again. Later lines override earlier ones for the same id.

### 2.6 `queries.jsonl` — the judge (human- or grader-written, committed)

The file `kanon` scores a retriever against (§21); this crate no longer reads it. Its format is
unchanged:

`{"id": "howto-enable-caching", "query": "How do I enable caching?", "expected": ["handbook::docs/user/tutorials/01-40-enable-caching.md", "handbook::docs/user/"], "kind": "howto", "holdout": false}`

`expected` entries are page ids or id prefixes (a trailing `/` means "any page under").
`holdout: true` rows are reported separately and never used for tuning decisions.

### 2.7 `report.md` — rendered PR body

Sections, in order: summary counts; added pages; removed pages (with reason: gone upstream,
dropped by resolver, excluded by decision); changed pages (hash changed; link to upstream compare
when both commits known); new residue grouped by rule with excerpt; expired decisions;
unresolved links; archived sources.

"New residue" groups undecided entries new since the previous report by `rule` (§2.4), not by
`reason`: each group's heading is the rule's own sentence, e.g. "not linked from
`docs/.vitepress/config.ts`". Reason `excluded` entries are never "new" in this sense; they are
instead always accounted for, grouped by rule the same way, inside one collapsed `<details>`
block placed after the ordinary groups (or alone, when there are no others), headed by their
total count, e.g. "12 pages excluded by policy or resolver rules" — present only when that count
is above zero, but never filtered by report history, so the count is always current.

Every page mentioned in "New residue", "Duplicates", "Added pages", "Removed pages" and
"Unresolved links" renders as a Markdown link `[title](url)` using the entry's or page's `url`
(§2.4, §11, §2.2); a mention with no title renders as `path` in code instead. "Unresolved
links" links the navigation file the dangling link came from, not the missing target, since the
target does not exist (§2.4).

### 2.8 Artifact contract version

The artifact contract is everything a consumer of an artifact reads: the directory layout of
§2.3, the fields of `<source>/meta.json` and the fields of `manifest.json` (§2.2). It carries
one integer version, `artifact_version`, written into both `manifest.json` and every
`meta.json`; the current version is 1. A file without the field is version 1.

Within a major, changes are additive only: a new field, a new optional file, a new enum value.
Removing or renaming a field or a file, or changing what an existing field means, is a new
major. A reader accepts an equal or lower version and rejects a higher one with a single line
naming both versions (`artifact version 2 is newer than this pinakes supports (1); upgrade
pinakes`): the manifest reader when it loads `manifest.json`, and the artifact reader when it
loads a source's `meta.json`, so every consumer that goes through the library's artifact reader
gets the check without doing anything. A JSON Schema for `manifest.json` is generated from the
code and committed at `docs/schemas/manifest.schema.json`; a test keeps it current.

Upgrading: `verify` compares the artifact's `manifest.json` and every `meta.json` byte for byte
with what the current manifest implies, so an artifact materialised by a release that did not
write `artifact_version` exits 3 (`manifest.json` differs, plus one `meta.json` per source);
run `resolve --from-manifest` to rematerialise it and commit the one-line `artifact_version`
diff to `manifest.json`.
### 2.9 `chunks.jsonl` — the retrieval units (machine-written, optional)

`{"heading": "Install", "id": "handbook::docs/install.md#1", "ordinal": 1, "page": "handbook::docs/install.md", "sha256": "…", "text": "Install\nInstall\n\nRun the installer …"}`

One line per retrieval unit of the artifact, cut by exactly the rules of §5, in page then unit
order. Written by `chunks` (§4) to `--out FILE` or stdout; keys sorted. Fields:

- `page`: the page id, `<source>::<path>` (§2.2).
- `ordinal`: the unit's 0-based position within its page.
- `id`: `<page>#<ordinal>`, e.g. `handbook::docs/install.md#0` for the page intro; the same id
  `index::Unit::id` carries (§5 step 5).
- `heading`: the unit's H2 heading, `<H2> / <H3>` for a section split at H3, empty for the intro.
- `text`: the unit text a consumer embeds, title, heading and body joined as §5 step 4
  shows. The built-in index scores the same cut as three fields (title ×3, heading ×2, body ×1),
  so `chunks` pins the units, not their scores.
- `sha256`: lowercase hex SHA-256 of `text`'s UTF-8 bytes.

Mirror pages (§5) yield no lines, matching what the built-in index searches. A consumer building
its own index (stage b) can index these lines, or reimplement §5 and check its cut against them,
so a recall number from `kanon` describes the units it actually serves.
### 2.10 `report.json` — the report's facts (machine-readable)

`report --json OUT` writes the facts behind `report.md` as one JSON document. The Markdown and
the JSON are rendered from the same prepared structure (the effective decisions, the residue and
its undecided subset, the diff, the expired decisions, and so on, computed once), so they
cannot drift: every count or id the Markdown shows is in the JSON, and every list keeps the
Markdown's order. The JSON additionally carries the id lists and counts a CI gate needs (the
undecided ids, the duplicate count); titles, URLs, excerpts and prose
stay in the Markdown. The file is pretty-printed with a two-space indent and a trailing
newline. Shown here compacted, this is the document for the report the test fixture
`tests/snapshots/report_full.md` renders (`tests/snapshots/report_full.json` is the file):

```json
{
  "version": 2,
  "summary": {"sources": 1, "pages": 3, "residue": 5, "undecided": 3, "excluded": 0, "decisions": 4, "changes": {"since": "2026-09-01T00:00:00Z", "added": 1, "removed": 3, "changed": 1}},
  "pages": {
    "added": ["handbook::docs/new.md"],
    "removed": [{"id": "handbook::docs/dropped.md", "reason": "excluded_by_decision"}, {"id": "handbook::docs/old.md", "reason": "gone_upstream"}, {"id": "removed-src::docs/x.md", "reason": "source_removed"}],
    "changed": [{"id": "handbook::docs/changed.md", "lines_added": 2, "lines_removed": 1}]
  },
  "residue": {
    "new": [{"rule": "nav:dangling-link", "text": "linked from `docs/_sidebar.md` but the file does not exist", "ids": ["handbook::docs/ghost.md"]},
            {"rule": "glob:outside-include", "text": "outside the configured include patterns", "ids": ["handbook::docs/fresh.md"]}],
    "undecided": ["handbook::docs/known-residue.md", "handbook::docs/fresh.md", "handbook::docs/ghost.md"],
    "excluded": []
  },
  "expired_decisions": [{"id": "handbook::docs/changed.md", "decision": "unsure", "why": "page_changed"}, {"id": "handbook::docs/old.md", "decision": "include", "why": "page_gone"}],
  "unresolved_links": {"handbook": ["handbook::docs/ghost.md"]},
  "archived_sources": ["handbook"],
  "duplicates": {"count": 2, "pairs": [
    {"kind": "mirror", "canonical": "handbook::docs/new.md", "duplicate": "handbook::docs/getting-started.md", "similarity": 0.71, "suggested": "review"},
    {"kind": "near", "canonical": "handbook::docs/getting-started.md", "duplicate": "removed-src::docs/x.md", "similarity": 0.93, "suggested": "exclude"}]},
  "usage": null
}
```

Fields, one per report section, in the section order of §2.7:

- `version` — the document's major version. `2` today; a missing field means `1`. Within a
  major version changes are additive only (new fields may appear, none is removed or changes
  meaning); a field removal or a change of meaning bumps it. Version 2 dropped the `eval` key
  (the before/after numbers, §21); a version 1 document, which may carry it, still reads, and
  the key is ignored.
- `summary` — the Summary counts: `sources` and `pages` in the current manifest; `residue`
  entries, of which `undecided` have no decision that applies to their current hash and
  `excluded` carry reason `excluded` (§2.4); `decisions` in effect (the last line per id);
  `changes` is `null` without a previous manifest, else `since` (the previous manifest's
  `generated_at`) and the diff's `added`, `removed` and `changed` counts.
- `pages` — `added` page ids; `removed` pages with their `reason`, one of `gone_upstream`,
  `dropped_by_resolver`, `source_removed` or `excluded_by_decision` (the wording the Markdown
  prints, as identifiers); `changed` pages with `lines_added` and `lines_removed` (§13). All
  empty without a previous manifest, else in the diff's order, as the Markdown lists them.
- `residue` — `new`: the "New residue" groups in the Markdown's order, each the `rule` key and
  `text` (§2.4) and the `ids` in that group in the Markdown's order; `undecided`: every
  undecided residue id, in registry order (the order the Markdown lists residue in);
  `excluded`: the groups of the collapsed excluded block, in the same shape.
- `expired_decisions` — each expired decision's `id`, its `decision` (`include`, `exclude` or
  `unsure`) and `why`: `page_changed` (the page exists with another hash) or `page_gone`.
- `unresolved_links` — dangling navigation links (§2.4's `unresolved_link`), the residue ids
  grouped by source name, each list in the Markdown's order.
- `archived_sources` — the names of sources the manifest records as archived, by name.
- `duplicates` — `count` and every pair (§11) as `kind` (`exact`, `mirror`, `near`), the
  `canonical` and `duplicate` ids, `similarity` and the `suggested` verdict, in the Markdown's
  order (by kind, then as `duplicates.jsonl` lists them).
- `usage` — `null` unless a usage report was given (§15.3), else that report's
  `never_retrieved`, `retrieved_never_cited` and `uncited_queries` as `usage --json` writes them.

### 2.11 `check` — gates on `report.json`

`check [--report FILE]` reads a `report.json` (default: `report.json` next to the config) and
compares each maximum set under `gates:` in `pinakes.yaml` (§2.1) with the corresponding count in
it. Recall is not gated here; that stays with `kanon eval --gate`. The gates, and the fact each one
reads:

| gate | count in `report.json` |
|---|---|
| `undecided_residue_max` | the length of `residue.undecided` |
| `removed_pages_max` | the length of `pages.removed`; empty when `report` ran without `--old` (`summary.changes` is `null`), so the gate then passes vacuously and `check` prints a `warning:` line on stderr |
| `expired_decisions_max` | the length of `expired_decisions` |
| `archived_sources_max` | the length of `archived_sources` |
| `unresolved_links_max` | the number of ids over every source in `unresolved_links` |
| `duplicates_max` | `duplicates.count` (pairs of every kind) |

A gate is violated when its count is strictly greater than the maximum; equal passes. An absent
key is a gate that is off. On stderr, one line per violated gate, `gate <name>: <count> >
<maximum>`, then a summary line: `gates: ok (N checked)`, `gates: FAILED (M of N violated)` or,
when no gate is set at all, `gates: none configured` (the report is then not read). On stdout,
one JSON line with the same facts, so a workflow reads the outcome without parsing stderr:

```json
{"version":1,"checked":2,"violations":[{"gate":"undecided_residue_max","limit":0,"actual":3}]}
```

`version` follows the rule of §2.10. Exit 0 when nothing is violated, 2 when something is, 1 on
an error — including a missing report file, which the message says to produce with
`report --json` first, and a report whose `version` is newer than this build's (§2.10), which
is refused rather than read with the wrong meaning.

## 3. External resolver contract

pinakes runs `command + args` with cwd = the checked-out repository, env `PINAKES_SOURCE=<name>`,
`PINAKES_COMMIT=<sha>`. The command writes JSONL to stdout, one object per candidate:

`{"path": "docs/user/x.md", "title": "…", "doc_type": "concept|tutorial|reference|troubleshooting|release-notes|", "section": "…", "selected": true, "rule": {"key": "toc:outside-match", "text": "outside the table-of-contents subtrees matching docs/guide/**"}}`

- `selected: false` lines are residue candidates with context; files under the resolver's scope that
  the command never mentions are residue too if `residue_scope` (optional glob list in config) covers them.
- `rule` (optional; SPEC §2.4) names the mechanism behind a `selected: false` line for the
  residue entry's own `rule` field, in place of the generic `external:not-selected`; pinakes
  passes it through verbatim and does not validate the key. Ignored on a selected line.
- Exit code ≠ 0 fails the resolve for that source with the command's stderr in the message.
- Missing `title` → pinakes takes the first H1, then a frontmatter `title:`, else empty.
- Config accepts optional `include` and `exclude` glob lists (§2.1): `include` selects files the
  command's output never mentions at all, with `selected_by: "include"` and a title derived the
  same way; `exclude` drops a file from selection regardless of what the command reports.

## 4. Commands

All commands take `--config pinakes.yaml` (default) and print human output to stderr, data to stdout.

| command | input | output | exit codes |
|---|---|---|---|
| `resolve [--artifact DIR] [--from-manifest M]` | config (or a manifest to reproduce) | artifact dir, `manifest.json`, `residue.jsonl` | 0 ok; 1 error |
| `diff OLD.json NEW.json` | two manifests | JSON on stdout (added/removed/changed/sources) and a human summary on stderr | 0 same; 3 differences |
| `residue list [--source S] [--reason R] [--include-excluded]` | `residue.jsonl` (+ decisions to hide decided ones) | JSONL | 0 |
| `decide ID include\|exclude\|unsure --reason "…" [--by NAME]` | residue + manifest for the hash | appends to `decisions.jsonl` | 0; 1 unknown id |
| `report [--old M] [--new M] [--json OUT]` | manifests, residue, decisions | `report.md` on stdout; `report.json` (§2.10) in `OUT` | 0 |
| `check [--report FILE]` | config `gates`, `report.json` (§2.11) | violated gates on stderr, JSON summary on stdout | 0 ok; 2 gate violated; 1 error (e.g. no report) |
| `verify [--artifact DIR]` | config, committed manifest | nothing | 0; 3 manifest stale; 4 policy violation |
| `init [REPO_URL…] [--workflow] [--dir DIR]` | nothing; each URL is fetched once to detect its layout | `pinakes.yaml`, empty `decisions.jsonl` and `queries.jsonl`, `.gitignore` entries; with `--workflow`, `.github/workflows/curate.yml` | 0; 1 bad URL or I/O error |
| `chunks [--artifact DIR] [--out FILE]` | artifact, config (optional, for priorities) | `chunks.jsonl` (§2.9) in `FILE` or on stdout, a summary on stderr | 0; 1 error |

`eval`, `embed`, `grade` and `queries` moved to `kanon` (§21). For one minor release each stays
as a stub that prints one line naming the `kanon` command to run instead, accepts and ignores
the arguments that follow it, and exits 1.

`resolve` downloads codeload tarballs (no git needed), reads the resolved commit from the tarball's
pax `comment` header, falls back to the wrapper directory suffix; unauthenticated, `GITHUB_TOKEN`
used when present; checks `archived` via the GitHub API, degrading to unknown on network errors.

`init` scaffolds a workspace for a new adopter and never overwrites: a file that already exists
is left alone and reported as skipped; an existing `.gitignore` only gains the lines it does not
already contain (`/artifact`, `/artifact-*`, `/report.md`). It writes a commented `pinakes.yaml`
(`version`, one source per URL given — or one annotated placeholder source when none — the
`policy` block with the values §2.1 shows, and a commented-out `eval` block), an empty
`decisions.jsonl` (§2.5) and an empty `queries.jsonl` (§2.6), all next to the config (`--dir`
picks another directory, created when missing). `--workflow` also writes
`.github/workflows/curate.yml`, the consumer half of §17.1, byte for byte the content of
`examples/curate-weekly.yml`. The generated config always loads: every source it writes is
valid, so, given at least one URL, `pinakes resolve` can run on it unedited; with none, the
placeholder source must be edited first, and `init` says so on stderr. A URL given more than
once is fetched and written once, with a warning.

For each URL, `init` derives the source name from the repository name (characters outside
`[A-Za-z0-9_-]` become `-`, a name already taken gets a numeric suffix), asks the GitHub API for
the default branch (falling back to `main`, and saying so in a comment when the branch could
not be determined), fetches that ref once as a tarball and picks the resolver from the file
list, first match wins: a `.vitepress/config.*` anywhere → `vitepress`; a `sidebars.js` or
`sidebars.ts` anywhere → `docusaurus`; a `SUMMARY.md` anywhere → `mdbook`; otherwise `glob`
with `include: ["docs/**/*.md"]` when the checkout has a `docs/` directory, else `["**/*.md"]`.
A navigation file found somewhere other than the resolver's default `path` (§12) is written as
an explicit `path`. When the fetch fails, the source is still written — `glob` on `**/*.md` with
a comment saying that detection failed and why — and `init` still exits 0: a scaffold must not
fail on a flaky network.

## 5. Built-in BM25 index (the reference for measurement)

- tantivy, in-memory index built from the artifact by whoever measures: `kanon eval` by default
  (§21), a consumer through the library surface (§20).
- Units: the page intro (text before the first H2) and one unit per H2 section; H2 sections over
  1200 tokens are split at H3. Fields: `title` (boost 3), `heading` (boost 2), `body`, plus
  stored `page_id`, `source`, `doc_type`. The exact cut, which `chunks` (§2.9) emits, a
  consumer's embedder can embed and this index scores as those three fields:
  1. Start from the page's cleaned content (frontmatter and HTML comments removed, §2.3) and
     reduce it to index text, in this order: images `!\[([^\]]*)\]\([^)]*\)` become their
     label (`$1`), then links `\[([^\]]*)\]\([^)]*\)` become their label, then every HTML tag
     `</?[a-zA-Z][^>]*>` (which may span lines) becomes one space.
  2. Split at H2 lines, going line by line: a line matching `^##\s+(.+?)\s*#*\s*$` opens a new
     unit whose heading is the captured text, trimmed; lines inside a fenced code block (a line
     whose trimmed start is ```` ``` ```` toggles the fence) never count as headings. The lines
     before the first H2 are the intro, with an empty heading; a part with an empty heading is
     dropped when blank (whitespace only). A page without H2 lines is a single intro unit, even
     when empty.
  3. An H2 unit (never the intro) whose body has more than 1200 tokens is split again the same
     way at H3 lines (`^###\s+(.+?)\s*#*\s*$`): the lines before its first
     H3 keep the H2 heading (dropped when blank), each H3 part gets the heading `<H2> / <H3>`.
     Tokens are counted as the tokeniser below emits them: after stopword removal (the list is
     `tokenizer::STOPWORDS`: a, an, and, are, as, at, be, by, can, do, does, for, from, how, i,
     if, in, is, it, its, my, of, on, or, that, the, this, to, was, what, when, which, with, you,
     your) and including the §10.3 compound forms, which count as extra tokens.
  4. A unit's text is `<title>\n\n<body>` for the intro and `<title>\n<heading>\n\n<body>`
     otherwise, where `title` is the page's navigation title, else its H1, else its frontmatter
     title, and `body` is the unit's lines joined with `\n` (heading lines excluded; a page's
     trailing newline is not kept). Lines are Rust's `str::lines`: split at `\n`, a trailing
     `\r` dropped from each line, so a CRLF page yields the same text and `sha256` as its
     LF-only twin, which a splitter that keeps `\r` would not. Mirror pages yield no units.
  5. A unit's id is `<page id>#<ordinal>`, `ordinal` being the unit's 0-based position within
     its page in the order steps 2 and 3 produce the units (ordinal 0 is the first unit: the
     intro, unless a blank intro was dropped). It needs no state beyond the page, so a
     consumer can reimplement it from these rules; `chunks` (§2.9) writes the same id and
     ordinal, and the library's `Unit::id`, `Unit::ordinal` and `chunk_id` are their one
     implementation.
- Page score = max unit score. Results are de-duplicated by tokenised title; when two sources carry
  the same title (nav title or H1), only the higher `priority` source's page is indexed (mirror rule).
  Priorities come from `pinakes.yaml` alone (`priority`, default 1): a source the config does not
  list, and every source when there is no config, has priority 1, and equal priorities never
  collapse a page, so a manifest-less artifact is indexed with no mirrors at all. The same-title
  de-duplication at search time still applies.
- Scoring is Okapi BM25 with `k1` 1.5, `b` 0.75 and negative IDFs floored at a quarter of the
  average IDF; field boosts multiply the term frequency and the unit length. This is the common
  `rank_bm25` BM25Okapi formula, so results are comparable with that library.
- Tokeniser: lowercase, alphanumeric runs, a small English stopword list, no stemming.
- Frontmatter, HTML comments, link targets, image targets and HTML tags are stripped before indexing.

The golden corpus of §7.3 pins what these rules yield; any change to them shows up as a diff of
the pinned result.

## 6. Metrics

Recall@5, recall@10, MRR and the rest of the scoring rules are `kanon`'s (§21); see its manual
for the metrics, the query file's judgement rules and the run files. This crate no longer
computes them.

## 7. Definition of done for v1

1. `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` green; docs on every public item.
2. Unit tests for: config parsing and precedence; manifest read/write round trip and sorted output;
   external resolver contract (a fake script); glob resolver; residue reasons; decisions expiry;
   diff; report rendering (snapshot); tokeniser and section splitting.
3. Golden corpus: `tests/fixtures/golden` holds a synthetic artifact of about thirty small
   Markdown pages across three sources with different priorities, sidebar-like titles in each
   `meta.json`, deliberate mirrors across sources, one frontmatter-only title, one page with H2/H3
   sections and a `_residue` directory, plus a `queries.jsonl` of fourteen queries of which two are
   held out. `expected` entries may be `<source>::<path>`, `<source>::<dir>/` or the legacy
   `<source>/<path>` prefix form. The built-in index over it must return exactly the pages
   pinned in `expected.json` (the top ten for each query); the pin is whatever the
   implementation yields, refreshed with `UPDATE_GOLDEN=1 cargo test --test golden` after an
   intended change and reviewed in the diff. The index accepts a manifest-less artifact (it
   reads `meta.json` per source), so the fixture carries no manifest. The recall numbers the
   queries produce are `kanon`'s to pin (§21).
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
  (tokeniser, sections, tantivy); `src/main.rs` with clap subcommands only.
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
`{"kind": "exact"|"mirror"|"near", "similarity": 0.93, "canonical": "<id>", "duplicate": "<id>", "why": "priority 10 > 1; linked from navigation; newer commit", "suggested": "exclude", "canonical_url": "https://github.com/…", "duplicate_url": "https://github.com/…"}`

`canonical_url` and `duplicate_url` are each page's upstream URL pinned to its source's fetched
commit (§2.4), derived from the manifest the same way; empty when there is no manifest to derive
them from (a manifest-less artifact) or the source's `repo` does not parse.

Winner rule, in order: higher source `priority`; page `selected_by` = `resolver` beats `include`;
lexically first page id, as the final, deterministic tie-break — `why` says which step decided.
`"suggested": "review"` only when priority and `selected_by` both tie and the lexical step had to
pick; that step itself never yields `"suggested": "exclude"`, since neither page actually
outranks the other. `report` gets a "Duplicates" section; `decide` accepts the duplicate id
as usual and `--reason` defaults to `superseded by <canonical>` when `--superseded-by` is given.

`duplicates.jsonl` is written sorted by `(canonical, duplicate)`, so re-running `resolve` or
`duplicates` against unchanged inputs reproduces the file byte for byte.

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

Each also accepts optional `include` and `exclude` glob lists (§2.1), with the same semantics
as the external resolver: `include` selects extra pages the navigation file does not link (a
landing `README.md` outside the sidebar, say); `exclude` removes a file from selection.

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

`queries.jsonl` (§2.6) and the commands that grow and check it (`queries add`, `check`,
`import`) moved to `kanon` (§21).

## 15. The loop from serving

### 15.1 Trail format (`trail.jsonl`, written by consumers)

The shape of a `trail.jsonl` line is a contract shared with `kanon`, which defines it once as
serde types and publishes it as a JSON Schema, `docs/schemas/trail-entry.schema.json` in its
repository (<https://github.com/friedrichwilken/kanon>). The fields are not restated here; what
this crate adds is the following.

- `version` is an integer, and a missing one means 1. Within a version changes are additive
  (a reader ignores fields it does not know); a removal, rename or change of meaning is a new
  version. `trail::read_jsonl`, and so `usage`, checks each line's version before the rest of
  the line is read and rejects a newer one with one line naming the file, the line and both
  versions:
  `trail.jsonl:3: version 2 is newer than the version 1 this pinakes reads; upgrade pinakes`.
  The version this build reads is `trail::TRAIL_VERSION`, 1.
- Ids in `retrieved` and `cited` are `<source>::<path>` (§2.2); a line with a differently
  shaped id is rejected.

Consumers write this; pinakes only reads it.

### 15.2 Grader

`grade`, which replays a trail against a backend and has a model grade each candidate, moved to
`kanon` (§21), as did `queries import`, which turns its output into query rows.

### 15.3 Usage statistics

`pinakes usage --trail trail.jsonl [--since 30d]` prints, and writes with `--json`: pages never
retrieved in the window (removal candidates), pages retrieved but never cited, queries with no
citation grouped by their top retrieved page, and for each uncited query the best residue page
by BM25 score (gap candidates). `report` gets a "Usage" section when a usage JSON is given.

## 16. Retriever shapes

The retriever backends (`bm25`, `bm25-tantivy`, `dense`, `hybrid`, `external`), the embeddings
file that `embed` writes for the dense one and `--compare` moved to `kanon` (§21). Its contracts
(the backend request and response, the unit id, the trail) are versioned there, with a JSON
Schema each; the unit cut they refer to is §5 and §2.9 here.

## 17. Delivery

### 17.1 Weekly Action

`.github/workflows/curate.yml` in this repository as a reusable workflow (`workflow_call`) plus
`examples/curate-weekly.yml` showing how a consumer calls it: resolve, diff against the
committed manifest, stop when empty, `kanon eval` before and after (§21), duplicates, report
(Markdown, with `kanon report`'s evaluation sections appended, and `report.json`), `check`
against the config's `gates` (§2.11), then open or update one PR on branch `pinakes/weekly` with the manifest, residue, duplicates and report committed
(`report.json` is not committed). A violated gate never fails the job: it puts `(gates failed:
<names>)` in the PR title and the label `gate-failed` next to the label `pinakes`. Inputs:
config path, queries path, gate baseline path (for `kanon eval --gate`). Uses
`peter-evans/create-pull-request`.

`action.yml` at the repository root, "Set up pinakes", is a composite action that downloads a
released binary for the runner's platform (`version`, default `latest`, resolved through the
GitHub releases API; `github-token` to avoid the anonymous rate limit; `musl` to pick the musl
build over glibc on Linux x86_64), verifies its `SHA256SUMS` line, and adds it to `PATH`.
`curate.yml` uses it when running outside this repository (falling back to `cargo install --path
.` inside it); a consumer can use it directly, `uses: friedrichwilken/pinakes@v1`, instead of
installing pinakes some other way. A moving major tag, `v1`, is force-updated to each `v1.x.y`
release so `@v1` always resolves to the newest compatible one.

### 17.2 Python wheel

`python/` contains a PyO3 crate `pinakes-py` built with maturin exposing `pinakes.Index`:
`Index.build(artifact_dir, priorities: dict[str,int] | None = None)`, `search(query, k=10,
module=None) -> list[Hit]` with `Hit(page_id, score, heading)`, `page_count`, `searchable_count`,
`read(page_id) -> Page(title, url, module, doc_type, section, content)` and `chunks() ->
list[Chunk]` with `Chunk(id, page, heading, ordinal, text, sha256)` (§2.9). Built in CI for
Linux x86_64/aarch64 and macOS arm64 on tags, attached to the release; not published to PyPI.

## 18. Definition of done for iteration 2

Each item ships with unit tests, docs on public items, a README section, and the gates green.
Golden fixture extended where behaviour changes (schema renderer fixture with one CRD file
containing two versions; a near-duplicate pair; a mdBook and a docusaurus fixture). Metrics on
the existing golden queries must not regress except where §10.3 says they are re-pinned.

## 19. Out of scope for iteration 2

A TUI; a hosted service; PyPI and crates.io publishing; embeddings computed locally without an
endpoint; graph or knowledge-base features.

---

# pinakes — specification, iteration 3

Iteration 3 has one theme: pinakes is stage a only. Evaluation moves to its own tool, `kanon`,
and pinakes becomes a library the other tools depend on for the one reading of an artifact.

## 20. Library surface

Two consumers read the artifact through this crate rather than parsing its files themselves:
`kanon` (evaluation) and a serving consumer. The modules below are the surface they may depend
on; their public items follow the same compatibility rule as the files they read (the artifact
contract version of §2.8): additive within a major version of the crate, a removal or rename only
with a new major.

| module | what a consumer gets |
|---|---|
| `corpus` | `load_pages` (an artifact directory into `Page`s, with the source priorities and the mirror rule), `Page` (every field), `Priorities` (`Default`, `of`, and its public `explicit` map, which a consumer fills itself), `DEFAULT_PRIORITY`, `mark_mirrors`, `load_residue_page`, `CorpusError` |
| `index` | the built-in BM25 index of §5: `Index` (`build`, `from_pages`, `search`, `page`, `pages`, `page_count`, `searchable_count`), `Hit`, `Unit` (with its `id` and `ordinal`), `chunk_id`, `iter_units`, `split_sections`, `Section`, `index_text`, `SECTION_SPLIT_TOKENS`, `TITLE_BOOST`, `HEADING_BOOST`, `IndexError`, and its re-exports of the `corpus`, `tokenizer` and `text` items, so `pinakes::index::…` paths stay |
| `tokenizer` | the tokeniser of §5 and §10.3, so a consumer's own index cuts the same tokens: `PinakesTokenizer`, `TOKENIZER_NAME`, `tokenize`, `title_key` |
| `chunks` | `Chunk`, `chunks`, `chunk_id`: the retrieval units of §2.9, the same cut the index searches and a consumer embeds |
| `manifest`, `layout` | `manifest.json`'s types and loader (`Manifest` with `new`, `load`, `save`, `pages`, `page` and its `sources`; `ManifestSource`, `PageEntry`, `SelectedBy`, `page_id`, `split_page_id`, `now_rfc3339`, `ManifestError` with its `Io`, `Json` and `ArtifactVersion` variants), the artifact's file and directory names (`MANIFEST_FILE` and the rest) |
| `text`, `jsonl`, `num` | content hashing and cleaning (`sha256_hex`), JSON Lines reading and writing (`read`, `write`, `parse`, `parse_lines`, `append`, `to_string`, `KeyOrder`, `JsonlError`, `LineError`), the one `usize -> f64` cast |
| `trail` | `trail.jsonl`'s types and reader (§15.1: `read_jsonl`, `TrailEntry` with its `version`, `TRAIL_VERSION`, `Outcome`, `TrailError`); written by a serving consumer, read by `usage` here; `kanon` keeps its own reading of the same contract |
| `residue` | `excerpt`: the leading words of a page's text, as residue entries and `kanon`'s grader show them |
| `config` | `pinakes.yaml`'s schema, for a consumer that reads the `eval:` block or the source priorities (`kanon` uses its own lenient parser today) |
| `llm` | the OpenAI-compatible chat client (`chat`, `ChatTransport`, `LlmConfig`, `UreqChatTransport`, `TransportError`, `ChatError` with its `Http` and `Json` variants) and its `testing` transport, shared by `classify` here and by `kanon`'s grader |

Everything else in the crate is an implementation detail of the pinakes commands and may change
in a minor release.

The items `kanon` imports, checked against its source, are pinned by
`tests/library_surface.rs` (issue #56): a function or method it names is bound to a function
pointer of its exact signature, and a type it reads is read field by field at its exact type.
Four structs `kanon` builds by literal (`Page`, `LlmConfig`, `PageEntry`, `ManifestSource`) and
one enum it matches exhaustively (`JsonlError`) are built and matched the same way, so the
additive rule above has one exception: a new field on one of those four structs or a new
`JsonlError` variant breaks `kanon`, and is made together with a `kanon` release. A new
function, method, item, or variant of any other enum is additive and fails nothing. The `trail`
items are pinned too, for the serving consumer; `kanon` does not use them today.

The additive rule is source-level only for what a consumer reads. A struct a consumer builds by
literal (a serving consumer writing `TrailEntry`, say) cannot gain a field without breaking that
consumer, and none of these structs is `#[non_exhaustive]`, because that would forbid the
literal outright. Such a field is added only when a contract requires it, in a minor release,
and the CHANGELOG says so: `TrailEntry` gained `version` this way (issue #57), the trail
contract's version field.

Editing the test is editing this contract, so a commit that does says why in its body, and a
removal waits for a new major version. After a change to the surface lands, `kanon` bumps the
`pinakes` rev in its `Cargo.toml`.

## 21. What moves to kanon

`eval`, `embed`, `grade`, `queries` (add, check, import), the backends of §16 (`bm25-tantivy`,
`dense`, `hybrid`, `external`), `--gate`, and the evaluation sections of `report` (§2.7's
before/after tables) move to `kanon`, which depends on this crate for §20. The move happens in
this order, so both repositories stay green at every step:

1. `kanon` builds the moved code against this crate's library surface, with the same flags and
   file formats, so an existing `queries.jsonl` and `eval:` block keep working. The moved code
   uses nothing outside §20.
2. This crate deletes the moved commands, `src/backend/`, `report`'s `--eval-before` /
   `--eval-after` flags and `ReportInput`'s eval fields (§2.7's before/after tables are then
   `kanon`'s), and the matching `CommandError` variants. `report.json` (§2.10) loses its `eval`
   key, which bumps its `version` to 2. For one minor release the deleted subcommands remain as
   stubs that print one line naming the `kanon` command to run instead and exit 1; the release
   after removes the stubs.
3. §5 and §9 are reworded (no `eval` in the crate layout; the index is built by whoever measures);
   §6 and §16 shrink to a pointer at `kanon`'s contracts; the README, manual and tutorial drop
   the evaluation sections and link across; the reusable curate workflow (§17.1) calls `kanon
   eval --gate` instead of `pinakes eval --gate`.

`usage` (§15.3) stays: it reads the trail, but its output is residue-side. `llm` and `classify`
(§14.2) stay. The index (§5) stays as the reference measurement index that `kanon` uses by
default. The three steps have landed: `pinakes eval` and friends are the stubs of step 2, and the
release after the first one that ships them removes those. The `eval:` block of `pinakes.yaml`
(§2.1) and `queries.jsonl` (§2.6) keep their formats; `kanon` reads both.
