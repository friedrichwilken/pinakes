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
```

| Key | Meaning |
|---|---|
| `sources[].name` | Short identifier; part of every page id (`<name>::<path>`). |
| `sources[].repo` | A GitHub HTTPS URL. Fetched as a codeload tarball at `ref`, no git needed. |
| `sources[].ref` | Branch or tag to resolve. |
| `sources[].priority` | Default `1`. Breaks ties when two sources carry a page with the same title (see [near-duplicate detection](duplicates.md) and [evaluation](eval.md#choosing-what-eval-measures)'s mirror rule). |
| `sources[].resolver` | Which files are selected; see [Resolvers](resolvers.md). |
| `sources[].render` | Optional; turns non-Markdown files into pages; see [Rendering](rendering.md). |
| `policy.deny` | Glob list. A matching page is always excluded, ahead of everything else. |
| `policy.archived` | `warn` (default) or `drop` for a source whose GitHub repo is archived. |
| `policy.min_pages_per_source` | `verify` reports a policy violation (exit 4) below this count. |
| `eval` | Evaluation settings; see [Evaluation](eval.md#choosing-what-eval-measures). |

`glob` selects files by pattern; `external` runs a command with `cwd` set to the checkout and
`PINAKES_SOURCE`/`PINAKES_COMMIT` in the environment. See [Resolvers](resolvers.md) for the
full contract and the four built-in navigation resolvers.

## Page identity and precedence

Every page's id is `<source name>::<path>`, used everywhere: `manifest.json`, `residue.jsonl`,
`duplicates.jsonl`, `queries.jsonl`'s `expected`, and `decide`'s `ID` argument.

A file's fate is decided in this order: `policy.deny` > `resolver.exclude` > a recorded
decision (`decide` or `classify`) > the resolver's own selection.

## The files

| File | Written by | Committed | What it is |
|---|---|---|---|
| `pinakes.yaml` | you | yes | Sources, policy, and eval settings (above). |
| `manifest.json` | `resolve` | yes | Per source: resolved commit, archived flag, every selected page with its sha256, title, doc type, section and what selected it, plus residue and unresolved paths. Sorted keys, two-space indent. |
| `<artifact>/` | `resolve` | no | `manifest.json`, `<source>/<original path>.md`, `<source>/meta.json` and `_residue/<source>/…` for the leftovers. A stable layout consumers rely on. |
| `residue.jsonl` | `resolve` | yes | What was left out and why (`not_selected`, `unresolved_link`, `new_source`, `excluded`) with title, excerpt, upstream url and the rule (`{key, text}`) that decided it. |
| `duplicates.jsonl` | `resolve` | yes | Exact, mirror and near-duplicate page pairs, canonical first, each with its upstream url and a `suggested` verdict for `decide`. See [Near-duplicate detection](duplicates.md). |
| `decisions.jsonl` | you or an agent | yes | Append-only verdicts on residue or a duplicate: `include`, `exclude` or `unsure`, tied to the page hash. Later lines win; a changed page expires the decision. |
| `queries.jsonl` | you or a grader | yes | The judge for `eval`: query, expected page ids or prefixes, kind, holdout flag. See [Evaluation](eval.md). |
| `report.md` | `report` | no | The PR body: counts, eval before/after, added/removed/changed pages, new residue, expired decisions, unresolved links, archived sources, duplicates. See [Commands](commands.md). |

The artifact directory is not committed; it is rebuilt from the manifest with
`resolve --from-manifest` wherever it is needed (locally, in CI, or by a consumer's build step).
