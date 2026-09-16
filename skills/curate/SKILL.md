---
name: curate
description: Use when a user wants to triage a pinakes documentation corpus after `pinakes resolve` — reviewing residue and near-duplicates, deciding what to include or exclude, and producing an updated report. Trigger phrases include "curate the corpus", "review residue", "triage duplicates", "go through what got left out".
---

# Curate a pinakes corpus

Drive one curation session through the `pinakes` CLI only. Never open `manifest.json` or any
file under the artifact directory in an editor, and never write to either directly — every
verdict goes through `pinakes decide`, which is the only thing allowed to change what is
excluded.

## Ground rules

- **CLI only.** Every fact about the corpus comes from a `pinakes` command's output. Do not
  read source files from the artifact to "double check" a decision; the excerpt in `residue
  list` or `duplicates` is the evidence.
- **Never edit `manifest.json` or the artifact.** Only `pinakes decide` may change what is in
  the corpus, and only `pinakes resolve` regenerates the artifact from that.
- **Never exclude a `resolver`-selected page.** Before proposing `exclude` on any id, check its
  `selected_by` in `manifest.json` (read-only, e.g. `pinakes eval` or `jq` over the file). If it
  is `"resolver"`, the resolver's author decided it belongs; propose `unsure` at most, and say
  why in the rationale instead of excluding it.
- **Batches of 20.** Never propose more than 20 candidates at once, and never call `decide` for
  a batch until the user has explicitly confirmed that specific batch.

## Session outline

1. **Gather.** Run, in this order, and read the output before doing anything else:
   - `pinakes residue list` (add `--source` / `--reason` to narrow a large corpus)
   - `pinakes duplicates` — first check it exists with `pinakes duplicates --help`; if the
     subcommand is not present in this build, skip it and say so
   - `pinakes diff <last-committed-manifest> manifest.json` — what actually changed since the
     last curated state (exit 3 just means there are differences to show; that's expected)
   - `pinakes report` — the current narrative view; skim it for context before triaging

2. **Group.** Bucket every candidate from `residue list` by its `reason`
   (`not_selected`, `unresolved_link`, `new_source`) and every pair from `duplicates` by its
   `kind` (`exact`, `mirror`, `near`). Work one group at a time, largest first, so related
   candidates get consistent verdicts.

3. **Propose a batch.** Take up to 20 candidates from the current group. For each, present:
   - the id, its `reason`/`kind`, and the title or excerpt already in the command's output
   - a proposed verdict (`include`, `exclude`, or `unsure`)
   - one sentence of rationale — specific to that page, not a generic template (e.g. "a table
     of contents, not a page" or "near-duplicate of `handbook::docs/x.md`, lower priority
     source, superseded")

   For a `duplicates` pair, the rationale should name the winner rule that applied (priority,
   `selected_by`, or newer commit — see the `why` field) and, when suggesting `exclude` on the
   `duplicate` side, remember it is only proposed, never applied, until confirmed.

4. **Wait for confirmation.** Show the batch and stop. Do not call `decide` until the user has
   confirmed the batch, in whole or with edits. If they change a verdict or rationale, use
   theirs.

5. **Apply.** For each confirmed candidate, run exactly one command:

   ```sh
   pinakes decide <id> <include|exclude|unsure> --reason "<one sentence>" --by <name>
   ```

   Use the name the user gives you for `--by` (their name, or an agent identity they specify);
   never invent one. For a duplicate being excluded in favour of another page, prefer a reason
   of the form `superseded by <canonical id>` so the report reads clearly.

6. **Repeat** steps 3-5 for the next group or batch until every candidate has a verdict or the
   user says to stop for this session. Leave anything genuinely ambiguous as `unsure` rather
   than guessing — it stays out of `residue list` without being excluded.

7. **Finish with `pinakes report`.** Run it once more at the end (with `--old`/`--eval-before`/
   `--eval-after` when those files are available) and show the result, so the session ends with
   a clear record of what changed and why.

## What this skill does not do

It does not run `pinakes resolve` (that re-fetches sources and is the operator's call), does
not run `pinakes classify` or any LLM-batch decision-making (a human confirms every batch here),
and does not touch `queries.jsonl` — that is `pinakes queries add`/`check`, a separate job.
