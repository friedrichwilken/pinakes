# Derived retrieval text: generated questions

`pinakes derive` asks a model for the questions each page answers and keeps them as search
text, so a user who types "is the service mesh preinstalled" finds a page that says "ships by
default". The questions are added to the page's index text only. They are never part of the
page a consumer reads, and a hit on them is a hit on the page (`SPEC.md` §14.4).

## Turn it on

Add `derive.questions` to a source in `pinakes.yaml`. The endpoint is the one
[`classify`](classify.md) uses (`PINAKES_LLM_URL`, `PINAKES_LLM_KEY`, `PINAKES_LLM_MODEL`):

```yaml
sources:
  - name: handbook
    repo: https://github.com/example-org/handbook.git
    ref: main
    resolver: { type: glob, include: ["docs/**/*.md"] }
    derive:
      questions:
        n: 5                          # questions per page, 1 to 20; default 5
        # prompt: "Write {n} questions …"   # the system prompt; {n} is the count; default built in
        # sections:                   # per navigation section (meta.json), overriding n and prompt
        #   Tutorials: { n: 3 }
```

```sh
pinakes resolve                 # as before: no model is called
pinakes derive --dry-run        # which pages would be asked about
pinakes derive                  # ask, then write manifest.json and artifact/derived.jsonl
pinakes verify                  # still green: the manifest, the artifact and derived.jsonl agree
```

## What it stores and when it goes stale

The questions are recorded in `manifest.json` under `derived`, per page id, with the hash of
what they were derived from:

```json
"derived": {
  "handbook::docs/user/README.md": {
    "questions": {"input_sha256": "…", "model": "…", "text": ["How do I install it?"]}
  }
}
```

`input_sha256` covers the page's `sha256`, the count and the effective prompt (not the model),
so an entry goes stale exactly when the page, `n` or the prompt changes. Model output is not
reproducible, so it is committed and reviewed in the pull request like the rest of the
manifest, and it is never regenerated behind your back:

- `pinakes resolve` calls no model. It keeps every entry that is still fresh and drops the rest;
  a changed page has no questions until the next `derive`.
- `pinakes resolve --from-manifest` writes `artifact/derived.jsonl` from the manifest, byte for
  byte, with no model.
- `pinakes derive` asks only about pages with no fresh entry, so a second run asks nothing and
  needs no endpoint. If the model fails on a page, the pages before it keep their questions.

The [weekly workflow](weekly-workflow.md) runs `derive` after it finds a change when it is
called with `derive: true` and the `PINAKES_LLM_*` secrets. A prompt or count change alone does
not open a pull request; run `pinakes derive` yourself, or wait for the next page change.

## How the index uses it

`artifact/derived.jsonl` has one line per page, `{"kind": "questions", "page": "<id>", "text":
[…]}`. `Index::build`, and so `kanon eval` and the Python `Index`, reads it: each page's
questions become one extra document in the built-in index, matched like body text and credited
to the page. They are not retrieval units, so `chunks.jsonl`, `Unit::text` and the page files do
not change. Questions for a mirror page are ignored (it is not searchable).

## Measuring it

Run [`kanon eval`](measuring-retrieval.md) twice on the same query set: once on an artifact
without `derived.jsonl`, once with it, and compare recall@5 and MRR. `derive` prints how many
pages got questions.

The query set must not be written from the questions. `kanon queries suggest` also has a model
write questions from pages, for the query set; the two use different prompts and never share
output, and the query set is committed before derived text is generated. Otherwise the
evaluation would measure its own homework.
