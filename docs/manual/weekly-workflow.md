# Weekly curation workflow

[`.github/workflows/curate.yml`](../../.github/workflows/curate.yml) in this repository is a
reusable [`workflow_call`](https://docs.github.com/actions/using-workflows/reusing-workflows)
workflow that runs the curate-then-review sequence on a schedule. It installs the `pinakes`
binary (the latest matching release asset in a consumer repository; `cargo install --path .`
when it runs inside this repository's own CI, detected from `github.workflow_ref` rather than
an extra input), reproduces the committed manifest with `resolve --from-manifest` to measure
"before", resolves fresh sources, diffs the two manifests and stops early once nothing changed,
measures "after" (gated against a baseline only when one is already committed, and never
failing the job on a drop — the point is a reviewable PR, not a silently skipped one), runs
`duplicates` when the installed binary has that subcommand (checked with `--help` first),
renders `report` (the Markdown, and `report.json` into the runner's temp directory), runs
[`check`](commands.md#check) against the config's [`gates`](config.md#gates), and opens or
updates one pull request on branch `pinakes/weekly` with the manifest, residue, duplicates and
`report.md` committed (never `report.json`), using the report as the PR body via
[`peter-evans/create-pull-request`](https://github.com/peter-evans/create-pull-request).

A violated gate never fails the job, for the same reason as the eval gate: the pull request is
where a reviewer should see it. The PR now carries the label `pinakes`; when a gate is violated
its title becomes `chore: refresh documentation corpus (gates failed: <gate names>)` and it also
carries the label `gate-failed`, so a branch protection rule or a reviewer's filter can act on
it. Both labels are new with this step; `create-pull-request` does not document creating a
label that does not exist, so a consumer may need to create `pinakes` and `gate-failed` in its
repository once. Without a `gates:` block the step reports `gates: none configured` and the
title and labels are the plain ones. The step's outputs are `gate` (`passed`, `failed`,
`skipped` for a binary whose `report` has no `--json`, or `error`) and `violations` (the
violated gates' names, comma separated).

A consumer calls it from its own repository, on whatever schedule it likes:

```yaml
# .github/workflows/curate.yml
name: curate
on:
  schedule: [{ cron: "0 6 * * 1" }]
  workflow_dispatch:
jobs:
  curate:
    uses: OWNER/pinakes/.github/workflows/curate.yml@v1
    with:
      config: pinakes.yaml
      queries: queries.jsonl
      baseline: eval-baseline.json
    permissions:
      contents: write
      pull-requests: write
```

See [`examples/curate-weekly.yml`](../../examples/curate-weekly.yml) for the full,
copy-pasteable file, and the [tutorial](../tutorials/curate-a-corpus.md) for wiring it up step
by step. Reviewers (or the [curation skill](curating-with-an-agent.md)) read the report, run
`pinakes residue list` for anything new, record `decide` verdicts, and merge; a consumer's build
stage then runs `pinakes resolve --from-manifest manifest.json` to materialise exactly the
reviewed corpus.
