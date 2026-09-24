# Configuration and files

`pinakes.yaml` declares your sources; everything else is written by the tool. This page is the
schema for both.

## `pinakes.yaml`

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
gates:                               # optional; each key is a maximum `pinakes check` enforces
  undecided_residue_max: 0
  removed_pages_max: 5
```

| Key | Meaning |
|---|---|
| `sources[].name` | Short identifier; part of every page id (`<name>::<path>`). |
| `sources[].repo` | A GitHub HTTPS URL. Fetched as a codeload tarball at `ref`, no git needed. |
| `sources[].ref` | Branch or tag to resolve. |
| `sources[].priority` | Default `1`. Breaks ties when two sources carry a page with the same title (see [near-duplicate detection](duplicates.md) and eval's [mirror rule](eval.md#how-the-default-bm25-index-scores-a-page)). |
| `sources[].resolver` | Which files are selected; see [Resolvers](resolvers.md). |
| `sources[].render` | Optional; turns non-Markdown files into pages; see [Rendering](rendering.md). |
| `policy.deny` | Glob list. A matching page is always excluded, ahead of everything else. |
| `policy.archived` | `warn` (default) or `drop` for a source whose GitHub repo is archived. |
| `policy.min_pages_per_source` | `verify` reports a policy violation (exit 4) below this count. |
| `eval` | Evaluation settings; see [Evaluation](eval.md#choosing-what-eval-measures). |
| `gates` | Optional maxima for [`check`](commands.md#check); see [Gates](#gates) below. |

`glob` selects files by pattern; `external` runs a command with `cwd` set to the checkout and
`PINAKES_SOURCE`/`PINAKES_COMMIT` in the environment. See [Resolvers](resolvers.md) for the
full contract and the four built-in navigation resolvers.

## Gates

`gates` holds the maxima [`pinakes check`](commands.md#check) compares against the counts in
`report.json` (written by `report --json`). Every key is optional and an absent key is a gate
that is off; a count strictly above its maximum violates the gate, equal passes. Recall is not
gated here: that is `eval --gate` with `eval.max_recall_drop` (see
[Evaluation](eval.md#choosing-what-eval-measures)).

| Key | Counts |
|---|---|
| `gates.undecided_residue_max` | Residue entries no effective decision covers (`residue.undecided`). |
| `gates.removed_pages_max` | Pages removed since the previous manifest (`pages.removed`). Only meaningful when `report` ran with `--old`; without it the count is empty, the gate passes vacuously and `check` prints a `warning:` line. |
| `gates.expired_decisions_max` | Decisions whose page changed or vanished (`expired_decisions`). |
| `gates.archived_sources_max` | Sources whose repository is archived (`archived_sources`). |
| `gates.unresolved_links_max` | Dangling navigation links over every source (`unresolved_links`). |
| `gates.duplicates_max` | Duplicate pairs of any kind, exact, mirror or near (`duplicates.count`). |

The [weekly workflow](weekly-workflow.md) runs `check` after `report` and marks the pull
request when a gate is violated.

## Page identity and precedence

Every page's id is `<source name>::<path>`, used everywhere: `manifest.json`, `residue.jsonl`,
`duplicates.jsonl`, `queries.jsonl`'s `expected`, and `decide`'s `ID` argument.

A file's fate is decided in this order: `policy.deny` > `resolver.exclude` > a recorded
decision (`decide` or `classify`) > the resolver's own selection.

## The files

| File | Written by | Committed | What it is |
|---|---|---|---|
| `pinakes.yaml` | you | yes | Sources, policy, and eval settings (above). |
| `manifest.json` | `resolve` | yes | Per source: resolved commit, archived flag, every selected page with its sha256, title, doc type, section and what selected it, plus residue and unresolved paths. `artifact_version` names the artifact contract the file and its artifact follow (missing means 1; a newer one is rejected). Sorted keys, two-space indent. Full schema: [`docs/schemas/manifest.schema.json`](../schemas/manifest.schema.json). |
| `<artifact>/` | `resolve` | no | `manifest.json`, `<source>/<original path>.md`, `<source>/meta.json` and `_residue/<source>/…` for the leftovers. A stable layout consumers rely on. |
| `residue.jsonl` | `resolve` | yes | What was left out and why (`not_selected`, `unresolved_link`, `new_source`, `excluded`) with title, excerpt, upstream url and the rule (`{key, text}`) that decided it. |
| `duplicates.jsonl` | `resolve` | yes | Exact, mirror and near-duplicate page pairs, canonical first, each with its upstream url and a `suggested` verdict for `decide`. See [Near-duplicate detection](duplicates.md). |
| `decisions.jsonl` | you or an agent | yes | Append-only verdicts on residue or a duplicate: `include`, `exclude` or `unsure`, tied to the page hash. Later lines win; a changed page expires the decision. |
| `queries.jsonl` | you or a grader | yes | The judge for `eval`: query, expected page ids or prefixes, kind, holdout flag. See [Evaluation](eval.md). |
| `report.md` | `report` | no | The PR body: counts, eval before/after, added/removed/changed pages, new residue, expired decisions, unresolved links, archived sources, duplicates. See [Commands](commands.md). |
| `report.json` | `report --json` | no | The same facts as `report.md`, as JSON ([SPEC §2.10](../../SPEC.md#28-reportjson--the-reports-facts-machine-readable)); what `check` reads. |

The artifact directory is not committed; it is rebuilt from the manifest with
`resolve --from-manifest` wherever it is needed (locally, in CI, or by a consumer's build step).
