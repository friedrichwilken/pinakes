# Skills

Claude Code skills for working with a `pinakes` corpus. Each skill is a directory holding a
`SKILL.md` with YAML frontmatter (`name`, a one-line `description` of when to use it) and a body
of instructions.

| Skill | Purpose |
|---|---|
| [`curate`](curate/SKILL.md) | Triage residue and near-duplicates after `pinakes resolve`: group by reason, propose decisions in batches of 20, apply them with `pinakes decide` once confirmed. |

## Installing a skill into a project

Claude Code loads skills from `.claude/skills/<name>/SKILL.md` in the project it is run from.
To make a skill in this repository available there, either symlink the directory (keeps it in
sync with this repository, e.g. when this repository is a submodule or a sibling checkout):

```sh
mkdir -p .claude/skills
ln -s /path/to/pinakes/skills/curate .claude/skills/curate
```

or copy it once (simpler, but drifts from this repository over time):

```sh
mkdir -p .claude/skills
cp -r /path/to/pinakes/skills/curate .claude/skills/curate
```

Either way, the consuming project also needs the `pinakes` binary on `PATH` (see the main
[`README.md`](../README.md#releases) for how to install a release, or `cargo install --path .`
against a checkout) since the skill drives a curation session entirely through the CLI.
