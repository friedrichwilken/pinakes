# Curating with an agent

Reviewing `residue list` and `duplicates` by hand does not scale much past the first few
sources. [`skills/curate/SKILL.md`](../../skills/curate/SKILL.md) is a Claude Code skill that
runs a curation session through the CLI only: it never edits `manifest.json` or the artifact
directly, reads `residue list`, `duplicates` (when present), `diff` and `report`, groups
candidates by reason, proposes a decision with a one-sentence rationale in batches of 20, and
applies them with `pinakes decide ... --by <name>` only once a batch is confirmed. It never
proposes excluding a page whose `selected_by` is `resolver`, and it finishes by rendering
`report` so the session leaves a clear record of what changed.

See [`skills/README.md`](../../skills/README.md) for how to install a skill from this
repository into a project (a `.claude/skills` symlink or a one-off copy), then ask the agent to
curate the corpus.

For an unattended alternative that classifies residue without a session, see
[Classifying residue with a model](classify.md).
