# Near-duplicate detection

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
pinakes duplicates
```

```json
{"canonical":"handbook::docs/install.md","duplicate":"guides::docs/install.md","kind":"mirror","similarity":0.86,"suggested":"exclude","why":"priority 10 > 1"}
```

## The winner rule

Decides which page of a pair is `canonical`, in order: the higher source `priority`; else a page
`selected_by` the resolver beats one `selected_by` a decision, which beats a bare glob include;
else the page id that sorts first lexically, which always picks a winner but is not a real
preference. When priority and `selected_by` both tied (the lexical case), `suggested` is
`"review"` instead of `"exclude"` — there is no clear canonical page for a person, or `decide`,
to prefer.

## Deciding on a duplicate

`decide` accepts a duplicate id the same way it accepts a residue id, and gains
`--superseded-by <ID>`, which defaults `--reason` to `superseded by <ID>` so the common case
needs no separate justification:

```sh
pinakes decide guides::docs/install.md exclude \
    --superseded-by handbook::docs/install.md
```

`report` gets a "Duplicates" section grouping pairs by kind, and `duplicates` works on a
manifest-less artifact too (as `eval` does): without a manifest, `sha256` is read straight from
the artifact, `selected_by` is unavailable, and the winner rule falls back to priority alone.
