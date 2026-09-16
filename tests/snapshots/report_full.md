# Corpus report

## Summary

- Sources: 1
- Pages: 3
- Residue: 5 entries, 3 undecided
- Decisions: 4
- Changes since 2026-09-01T00:00:00Z: 1 added, 3 removed, 1 changed

## Eval before/after

### Tuning queries

| kind | recall@5 | recall@10 | MRR | n |
|---|---|---|---|---|
| overall | 0.800 → 0.850 | 0.850 → 0.900 | 0.660 → 0.700 | 40 |
| concept | – → 0.700 | – → 0.700 | – → 0.500 | 5 |
| howto | 0.900 → 0.900 | 0.950 → 1.000 | 0.800 → 0.850 | 10 |

### Held-out queries

| kind | recall@5 | recall@10 | MRR | n |
|---|---|---|---|---|
| overall | 0.500 → 0.750 | 0.500 → 0.750 | 0.400 → 0.600 | 4 |

## Added pages

- [New Page](https://github.com/example-org/handbook/blob/2222222222222222222222222222222222222222/docs/new.md)

## Removed pages

- [Dropped Page](https://github.com/example-org/handbook/blob/1111111111111111111111111111111111111111/docs/dropped.md) (excluded by decision)
- [Old Page](https://github.com/example-org/handbook/blob/1111111111111111111111111111111111111111/docs/old.md) (gone upstream)
- [X](https://github.com/example-org/handbook/blob/9999999999999999999999999999999999999999/docs/x.md) (source removed)

## Changed pages

- [Changed Page](https://github.com/example-org/handbook/blob/2222222222222222222222222222222222222222/docs/changed.md) (+2/-1) ([compare](https://github.com/example-org/handbook/compare/1111111111111111111111111111111111111111...2222222222222222222222222222222222222222))

## New residue

### linked from `docs/_sidebar.md` but the file does not exist

- [Ghost](https://github.com/example-org/handbook/blob/2222222222222222222222222222222222222222/docs/ghost.md)
  - context: Sidebar > Ghost

### outside the configured include patterns

- `handbook::docs/fresh.md`
  > word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word …

## Expired decisions

- `handbook::docs/changed.md` — unsure by alice at 2026-09-15T08:00:00Z (page changed): reviewed
- `handbook::docs/old.md` — include by alice at 2026-09-15T08:00:00Z (page gone): reviewed

## Unresolved links

- [Ghost](https://github.com/example-org/handbook/blob/2222222222222222222222222222222222222222/docs/ghost.md)

## Archived sources

- `handbook` (example-org/handbook)

## Duplicates

### mirror

- [New Page](https://github.com/example-org/handbook/blob/2222222222222222222222222222222222222222/docs/new.md) ← [Getting Started](https://github.com/example-org/handbook/blob/2222222222222222222222222222222222222222/docs/getting-started.md) (similarity 0.710, suggested: review) — same priority, selected_by and commit date

### near

- [Getting Started](https://github.com/example-org/handbook/blob/2222222222222222222222222222222222222222/docs/getting-started.md) ← `removed-src::docs/x.md` (similarity 0.930, suggested: exclude) — priority 10 > 1

