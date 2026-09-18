# Tutorial: curate a corpus and wire up a weekly workflow

We'll declare two real documentation sources, resolve them into a corpus, decide what to do
with what got left out, measure retrieval against a query set, and end with a scheduled GitHub
Actions workflow that reproduces the whole thing and opens a pull request when the sources
change. Every step shows the complete file so far — copy, paste, run.

## 0. Install

```sh
cargo install --git https://github.com/friedrichwilken/pinakes --tag v1
```

Or use the [setup action](../manual/setup-action.md) in a workflow instead of a local install.

## 1. Declare one source

Create `pinakes.yaml`:

```yaml
version: 1
sources:
  - name: nomicon                                    # <- one source; page ids become nomicon::<path>
    repo: https://github.com/rust-lang/nomicon.git    # <- fetched as a tarball, no git needed
    ref: master
    resolver:
      type: glob                                       # <- select files by pattern, no navigation needed
      include: ["src/**/*.md"]
      exclude: ["**/SUMMARY.md"]                        # <- mdBook's table of contents, not a page
```

Run it:

```sh
pinakes resolve
```

What you should see:

- `nomicon: 63 pages, 0 residue, 0 unresolved @ 5791ca9f5d67` on stderr.
- `manifest.json`, `residue.jsonl`, `duplicates.jsonl` and an `artifact/` directory next to
  `pinakes.yaml`.
- `manifest.json` records the commit and every page's hash and title (see
  [Configuration and files](../manual/config.md) for the full schema):

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
      }
    }
  }
  ```

Commit `pinakes.yaml`, `manifest.json`, `residue.jsonl` and `duplicates.jsonl`. The `artifact/`
directory is not committed — anyone rebuilds it with `pinakes resolve --from-manifest`.

## 2. Add a second source, selected differently

Not every repository ships a glob-friendly layout. `api-guidelines` has no sidebar, so we select
it with a small external resolver — a script that reads the checkout and prints one JSON line
per candidate file:

```yaml
version: 1
sources:
  - name: nomicon
    repo: https://github.com/rust-lang/nomicon.git
    ref: master
    priority: 10                                        # <- higher wins a same-title tie against api-guidelines
    resolver:
      type: glob
      include: ["src/**/*.md"]
      exclude: ["**/SUMMARY.md"]
  - name: api-guidelines                                 # <- second source, same config, its own resolver
    repo: https://github.com/rust-lang/api-guidelines.git
    ref: master
    priority: 5
    resolver:
      type: external                                      # <- runs a command instead of matching globs
      command: ["python3", "resolvers/frontmatter_title.py"]
      args: ["src"]
```

`resolvers/frontmatter_title.py` is a few lines of stdlib Python; see
[`examples/`](../../examples/) for the runnable script. Re-run:

```sh
pinakes resolve
```

What you should see:

- Both sources on stderr: `api-guidelines: 14 pages, 1 residue, 0 unresolved @ 97a0969cb07f`
  alongside the `nomicon` line from step 1.
- One residue entry: the resolver saw `api-guidelines`'s table of contents but did not select
  it (it is navigation, not a page).

## 3. Decide on what got left out

```sh
pinakes residue list
```

```json
{"id":"api-guidelines::src/SUMMARY.md","source":"api-guidelines","path":"src/SUMMARY.md",
 "reason":"not_selected","title":"Summary","context":"mdBook table of contents",
 "excerpt":"# Summary [About](about.md) [Checklist](checklist.md) - [Naming](naming.md) …"}
```

A reviewer — or an agent, see [Curating with an agent](../manual/curating-with-an-agent.md) —
decides without opening the file:

```sh
pinakes decide api-guidelines::src/SUMMARY.md exclude \
    --reason "a table of contents, not a page" --by me
```

What you should see: the verdict appended to `decisions.jsonl`, keyed to the page's hash, so it
expires automatically if the page ever changes. Residue and duplicates work the same way; see
[Near-duplicate detection](../manual/duplicates.md) for the latter.

## 4. Write a query set and measure retrieval

Create `queries.jsonl`, one question per line, a mix of things a reader of these two books might
ask:

```jsonl
{"id": "unsafe-aliasing", "kind": "concept", "query": "what is aliasing in unsafe rust", "expected": ["nomicon::src/aliasing.md"]}
{"id": "naming-conventions", "kind": "reference", "query": "rust naming conventions for getters", "expected": ["api-guidelines::src/naming.md"], "holdout": true}
```

Or grow it with `pinakes queries add` instead of writing JSON by hand — see
[Evaluation](../manual/eval.md#the-judge-queriesjsonl). Then measure:

```sh
pinakes eval
```

```text
artifact: 77 pages, 77 searchable, k = 10
| split    | kind      | n | recall@5 | recall@10 | MRR   |
|----------|-----------|---|----------|-----------|-------|
| tuning   | overall   | 5 | 1.000    | 1.000     | 0.900 |
| held-out | overall   | 1 | 1.000    | 1.000     | 1.000 |
```

What you should see: recall@5 and MRR for the tuning split and, separately, for the held-out row
that never feeds a gate (see [Evaluation](../manual/eval.md) for what each column means).

## 5. Guard the corpus and render a report

```sh
pinakes verify                              # <- exits non-zero if the manifest is stale or violates policy
pinakes report --old manifest.json > report.md   # <- the PR body: counts, eval, residue, duplicates
```

What you should see: `verify` exits 0 (nothing to fix); `report.md` is empty of changes the
first time, since `--old` and the current manifest are the same file. It becomes useful once a
source has moved on — which is exactly what the workflow below runs on a schedule.

## 6. Turn it into a weekly workflow

Everything above — reproduce, resolve, diff, eval, verify, report — is exactly what
[`.github/workflows/curate.yml`](../../.github/workflows/curate.yml) in the pinakes repository
runs as a reusable workflow. Call it from your own repository:

```yaml
# .github/workflows/curate.yml
name: curate
on:
  schedule: [{ cron: "0 6 * * 1" }]        # <- every Monday at 06:00 UTC
  workflow_dispatch:                        # <- lets you also trigger it by hand
jobs:
  curate:
    uses: friedrichwilken/pinakes/.github/workflows/curate.yml@v1
    with:
      config: pinakes.yaml
      queries: queries.jsonl
    permissions:
      contents: write                       # <- it commits manifest.json etc. to a branch
      pull-requests: write                  # <- and opens or updates a PR
```

What you should see: a scheduled run resolves your sources fresh, diffs against the committed
manifest, and — only when something changed — opens or updates one pull request on branch
`pinakes/weekly`, with the report as its body. See
[Weekly curation workflow](../manual/weekly-workflow.md) for exactly what it does and
[`examples/curate-weekly.yml`](../../examples/curate-weekly.yml) for the full file with a
baseline gate.

## 7. Use `pinakes` in your own CI too

The workflow above installs `pinakes` for you, but any other job — a lint check, a build step
that materialises the corpus — needs it on `PATH` directly. That is the
[setup action](../manual/setup-action.md):

```yaml
- uses: friedrichwilken/pinakes@v1
- run: pinakes resolve --from-manifest manifest.json --artifact docs-artifact
```

## Where to go next

- [Manual](../manual/README.md) — full reference for every command, config key and file.
- [Handlers](../manual/handlers.md) — rendering CRDs and OpenAPI schemas into pages, and a
  second worked external resolver.
- [Learning from serving](../manual/serving-feedback.md) — once this is live, feed real queries
  back in with the trail, grade and usage commands.
