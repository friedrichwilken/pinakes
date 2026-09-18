# Rendering schemas and other formats

Pinakes reads Markdown. Selection works on any file a resolver names, but titles come from an
H1 or frontmatter, sections are split at H2 and H3, and the index strips Markdown syntax, so
other formats are indexed as plain text at best — unless a source declares `render` (`SPEC.md`
§10), in which case pinakes turns the selected files into Markdown pages after selection and
before the artifact is written. One selected file may become several pages, or none.
[Handlers](handlers.md) shows both render types before and after on real files.

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
(`SPEC.md` §10.3), so a query for `spec.sink` matches the field path directly and `jwks_urls`
matches `jwks`, `urls` or the joined form. See [Handlers](handlers.md) for a worked example of
each render type, including an external one, and of an external resolver.
