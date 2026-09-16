# Handlers

Two extension points change what a document looks like by the time it is a page in the
artifact, and one of them changes which documents become pages at all:

- a **render step** (`render:` on a source) turns files that are not Markdown into Markdown
  pages after selection; the built-in `openapi` renderer handles Kubernetes CRDs and OpenAPI
  schemas, `external` runs a command of your own;
- an **external resolver** (`resolver: {type: external}`) decides which files in a checkout are
  documentation when none of the built-in resolvers knows the repository's convention.

This page shows each on a real document. Everything below was produced by `pinakes resolve`
on [`examples/handlers/pinakes.yaml`](../examples/handlers/pinakes.yaml), which declares this
repository three times, once per handler, pinned at commit
[`182e0de`](https://github.com/friedrichwilken/pinakes/tree/182e0dee327e5cd00d7a4a21b07eb0313e6c1006).
Run it yourself from that directory:

```sh
pinakes resolve
```

## 1. The built-in renderer: a CRD becomes a reference page

```yaml
  - name: crd-reference
    repo: https://github.com/friedrichwilken/pinakes.git
    ref: main
    resolver:
      type: glob
      include: ["tests/fixtures/crds/*.yaml", "tests/fixtures/openapi/*.yaml"]
    render:
      type: openapi
```

**Before.** [`tests/fixtures/crds/subscriptions.yaml`](https://github.com/friedrichwilken/pinakes/blob/182e0dee327e5cd00d7a4a21b07eb0313e6c1006/tests/fixtures/crds/subscriptions.yaml)
is a plain `CustomResourceDefinition` with three versions, of which two are served. The part
that matters to a reader is buried five levels deep:

```yaml
    - name: v1
      served: true
      storage: true
      schema:
        openAPIV3Schema:
          description: |
            A Subscription describes interest
            in a class of events.
          type: object
          properties:
            spec:
              type: object
              required: [sink]
              properties:
                sink:
                  type: string
                  description: The URL of the subscriber.
                typeMatching:
                  type: string
                  description: How the event type is matched.
                  enum: [exact, standard]
```

Indexed as text, that file is a bag of `type: object` and `properties:` lines. A question about
`spec.sink` has nothing to match.

**After.** One page per served version, storage version first,
`artifact/crd-reference/reference/messaging.example.com/subscription-v1.md`:

```markdown
# Subscription (messaging.example.com/v1)

A Subscription describes interest in a class of events.

Scope: Namespaced · Plural: subscriptions · Short names: sub, subs · Served: yes · Storage: yes

## Fields

| Field | Type | Required | Values | Description |
|---|---|---|---|---|
| `spec.config.*` | string | no |  |  |
| `spec.extra` | object | no | preserves unknown fields |  |
| `spec.filters[].eventType` | string | no |  |  |
| `spec.sink` | string | yes |  | The URL of the subscriber. |
| `spec.typeMatching` | string | no | `exact`, `standard` | How the event type is matched. |

## Status

| Field | Type | Values | Description |
|---|---|---|---|
| `ready` | boolean |  |  |

## Conditions

| Type | Description |
|---|---|
| `Ready` | The kind of condition. |
| `Subscribed` | The kind of condition. |
```

The unserved `v1beta1` produces no page. The page's title is the kind and version, its section
the API group, its `doc_type` `reference`, and the tokeniser indexes `spec.sink` both whole and
as `spec` + `sink`, so the query `spec.sink` finds this page first (that is one of the golden
corpus's pinned queries). The manifest entry records where it came from:

```json
"reference/messaging.example.com/subscription-v1.md": {
  "doc_type": "reference",
  "rendered_from": "tests/fixtures/crds/subscriptions.yaml",
  "section": "messaging.example.com",
  "selected_by": "include",
  "sha256": "2eae38ac5ce2d702bdb634043afdd4127288da92a8fe4d3208cd41578168b97f",
  "title": "Subscription (messaging.example.com/v1)"
}
```

`sha256` is of the rendered Markdown, so a schema change upstream shows up in `pinakes diff` as
a changed page with a line count, not as an opaque YAML edit.

The same renderer reads OpenAPI 3.x documents, one page per entry in `components.schemas`.
[`tests/fixtures/openapi/widget.yaml`](https://github.com/friedrichwilken/pinakes/blob/182e0dee327e5cd00d7a4a21b07eb0313e6c1006/tests/fixtures/openapi/widget.yaml)
becomes `artifact/crd-reference/reference/widget.md`:

```markdown
# Widget

A small widget.

## Fields

| Field | Type | Required | Values | Description |
|---|---|---|---|---|
| `name` | string | yes |  | Display name. |
| `tags[]` | string | no |  |  |
```

## 2. An external render step: `Cargo.toml` becomes a dependency table

When the file format is yours, the render step is a command. It runs once per source after
selection, with `cwd` set to the checkout and `PINAKES_OUT` set to the directory it writes pages
into; each selected file arrives as one JSON line on stdin, and each page written is announced
as one JSON line on stdout.

```yaml
  - name: cargo-reference
    repo: https://github.com/friedrichwilken/pinakes.git
    ref: main
    resolver:
      type: glob
      include: ["Cargo.toml", "python/Cargo.toml"]
    render:
      type: external
      command: ["python3", "./cargo_deps.py"]   # relative to pinakes.yaml
```

The script is [`examples/handlers/cargo_deps.py`](../examples/handlers/cargo_deps.py), under a
hundred lines of standard-library Python. Its whole contract is the loop at the bottom:

```python
out = Path(os.environ["PINAKES_OUT"])
for line in sys.stdin:                       # {"path": "Cargo.toml"}
    if line.strip():
        print(json.dumps(render(json.loads(line)["path"], out)))
```

and `render` writes `reference/<crate>-dependencies.md` under `PINAKES_OUT` and returns
`{"path", "source_path", "title", "doc_type", "section"}` for it.

**Before.** [`python/Cargo.toml`](https://github.com/friedrichwilken/pinakes/blob/182e0dee327e5cd00d7a4a21b07eb0313e6c1006/python/Cargo.toml),
the lines that matter (the file also carries lint and build settings):

```toml
[package]
name = "pinakes-py"
version = "1.0.3"

[dependencies]
pinakes-lib = { package = "pinakes", path = ".." }
pyo3 = { version = "0.29.2", features = ["extension-module", "abi3-py310"] }
serde_json = "1.0.151"
```

**After.** `artifact/cargo-reference/reference/pinakes-py-dependencies.md`:

```markdown
# pinakes-py 1.0.3 dependencies

Crates `pinakes-py` depends on, from `python/Cargo.toml`.

| Crate | Kind | Version | Features | Optional |
|---|---|---|---|---|
| `pinakes-lib` | runtime | .. |  | no |
| `pyo3` | runtime | 0.29.2 | `extension-module`, `abi3-py310` | no |
| `serde_json` | runtime | 1.0.151 |  | no |
```

A selected file the script never announces is dropped and listed in the source's `meta.json`
under `unrendered`, so a renderer that silently skips something is visible. The manifest records
the command with its path made absolute, exactly as it ran, and `pinakes resolve
--from-manifest` re-runs it, so a rendered source reproduces byte for byte like any other.

Two things to know when writing one:

- A bare program name in `command` (`"cargo_deps.py"`) is taken as something on `PATH`; write
  `"./cargo_deps.py"` or a longer relative path for a script that lives next to `pinakes.yaml`.
- `PINAKES_SOURCE` and `PINAKES_COMMIT` are in the environment too, for scripts that want to
  put the commit into the page.

## 3. An external resolver: `README.md` as the table of contents

A resolver answers "which files here are the documentation". The built-in ones read a
VitePress, Docusaurus, mdBook or sitemap navigation, or a glob. An external resolver is a
command that prints one JSON line per candidate file, selected or not, with `cwd` set to the
checkout.

```yaml
  - name: readme-toc
    repo: https://github.com/friedrichwilken/pinakes.git
    ref: main
    resolver:
      type: external
      command: ["python3", "./readme_toc.py"]
```

[`examples/handlers/readme_toc.py`](../examples/handlers/readme_toc.py) treats the README as
the table of contents: the README itself and every local Markdown file it links are selected,
with the H2 the link sits under as the page's section; every other Markdown file is reported
unselected, with a rule of the script's own.

**Before.** The checkout has 68 Markdown files. The README links a handful of them; 61 are test
fixtures under `tests/` that no reader should ever be sent to. This is what the script prints
(the `tests/` lines elided):

```jsonl
{"path": "AGENTS.md", "title": "AGENTS.md", "doc_type": "", "section": "Development", "selected": true}
{"path": "CHANGELOG.md", "title": "Changelog", "doc_type": "", "section": "", "selected": true}
{"path": "CONTRIBUTING.md", "title": "Contributing", "doc_type": "", "section": "Development", "selected": true}
{"path": "README.md", "title": "pinakes", "doc_type": "", "section": "", "selected": true}
{"path": "SPEC.md", "title": "pinakes — specification, iteration 1", "doc_type": "", "section": "", "selected": true}
{"path": "skills/README.md", "title": "Skills", "doc_type": "", "section": "Curating with an agent", "selected": true}
{"path": "skills/curate/SKILL.md", "title": "Curate a pinakes corpus", "doc_type": "", "section": "Curating with an agent", "selected": true}
{"path": "tests/fixtures/golden/artifact/cookbook/docs/README.md", "selected": false, "rule": {"key": "readme-toc:unlinked", "text": "not linked from README.md"}}
```

**After.** Seven pages in `artifact/readme-toc/`, with the section the README gave them in
`meta.json`:

```json
"pages": {
  "AGENTS.md": { "doc_type": "", "section": "Development", "title": "AGENTS.md" },
  "CHANGELOG.md": { "doc_type": "", "section": "", "title": "Changelog" },
  "CONTRIBUTING.md": { "doc_type": "", "section": "Development", "title": "Contributing" },
  "README.md": { "doc_type": "", "section": "", "title": "pinakes" },
  "SPEC.md": { "doc_type": "", "section": "", "title": "pinakes — specification, iteration 1" },
  "skills/README.md": { "doc_type": "", "section": "Curating with an agent", "title": "Skills" },
  "skills/curate/SKILL.md": { "doc_type": "", "section": "Curating with an agent", "title": "Curate a pinakes corpus" }
}
```

and 61 entries in `residue.jsonl`, each with the script's rule, an excerpt, and a link to the
file at the fetched commit, so a reviewer can decide about it without a checkout:

```json
{
  "id": "readme-toc::tests/fixtures/golden/artifact/cookbook/docs/README.md",
  "source": "readme-toc",
  "path": "tests/fixtures/golden/artifact/cookbook/docs/README.md",
  "title": "Cookbook",
  "reason": "not_selected",
  "rule": {"key": "readme-toc:unlinked", "text": "not linked from README.md"},
  "excerpt": "# Cookbook Community recipes for the service. Each recipe is a short, tested how-to.",
  "url": "https://github.com/friedrichwilken/pinakes/blob/182e0dee327e5cd00d7a4a21b07eb0313e6c1006/tests/fixtures/golden/artifact/cookbook/docs/README.md",
  "sha256": "71505a142a916bae99b97a79eef6aa9912068d0b99a25776d4f04de8b3cacde0",
  "context": ""
}
```

`pinakes residue list` groups them by rule, `pinakes decide` records a verdict per page, and
`pinakes eval --with ID` measures what admitting one would do. For a case like this one, where
the answer is known in advance, a single line on the source keeps the fixtures out for good:
`exclude: ["tests/**"]` turns them into `excluded` residue that `residue list` hides by default
and `report.md` only counts.

A selected path that does not exist in the checkout (a dangling README link) is recorded under
`unresolved` in `meta.json` and in the manifest, for the upstream docs team rather than for the
curator.

## The contract in one place

| | Render step (`render: external`) | External resolver (`resolver: external`) |
|---|---|---|
| Runs | once per source, after selection, before the artifact is written | once per source, to select |
| `cwd` | the checkout | the checkout |
| Environment | `PINAKES_SOURCE`, `PINAKES_COMMIT`, `PINAKES_OUT` | `PINAKES_SOURCE`, `PINAKES_COMMIT` |
| stdin | one `{"path"}` line per selected file | nothing |
| stdout | one line per page written: `path` (relative to `PINAKES_OUT`), `source_path`, `title`, `doc_type`, `section` | one line per candidate: `path`, `selected`, and for selected files `title`, `doc_type`, `section`; optionally `rule` (`{"key", "text"}`) and `context` for unselected ones |
| Non-zero exit | fails the source with the command's stderr | fails the source with the command's stderr |
| Recorded in the manifest | the command as run, paths absolute; `rendered_from` per page | the resolver kind; `selected_by` per page |

The full text is SPEC.md §3 (resolver) and §10.1 (render).
