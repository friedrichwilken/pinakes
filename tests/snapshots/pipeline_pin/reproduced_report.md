# Corpus report

## Summary

- Sources: 6
- Pages: 17
- Residue: 15 entries, 14 undecided
- Decisions: 5
- Changes: no previous manifest to compare with

## Eval before/after

_No evaluation results supplied._

## Added pages

_none_

## Removed pages

_none_

## Changed pages

_none_

## New residue

### recomputed by `resolve --from-manifest`; the original resolver did not run

- [Repository A](https://github.com/acme/a/blob/a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1/README.md)
  > # Repository A In scope, not selected.
- [Changelog](https://github.com/acme/a/blob/a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1/docs/CHANGELOG.md)
  > # Changelog - denied by policy
- `a::docs/_sidebar.md`
  > - [Intro](intro.md)
- [Stale](https://github.com/acme/a/blob/a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1/notes/stale.md)
  > # Stale This note changed after somebody decided about it.
- [Extra](https://github.com/acme/a-b/blob/abababababababababababababababababababab/extra.md)
  > # Extra In scope, not selected.
- [Summary](https://github.com/acme/book/blob/b00cb00cb00cb00cb00cb00cb00cb00cb00cb00c/src/SUMMARY.md)
  > # Summary [Introduction](intro.md) # Guides - [Setup guide](guide/setup.md) - [Missing chapter](guide/missing.md) - [Draft chapter]()
- `book::src/guide/missing.md`
- [Orphan](https://github.com/acme/book/blob/b00cb00cb00cb00cb00cb00cb00cb00cb00cb00c/src/orphan.md)
  > No chapter links this page.
- [B](https://github.com/acme/ext/blob/e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1/docs/b.md)
  > # B Left out by the resolver; mentions storage.
- [C](https://github.com/acme/ext/blob/e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1/docs/c.md)
  > # C Also left out; mentions storage too.
- [D](https://github.com/acme/ext/blob/e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1/docs/d.md)
  > # D Never mentioned by the resolver; mentions storage.
- `ext::docs/ghost.md`
- [Internal](https://github.com/acme/ext/blob/e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1/docs/internal.md)
  > # Internal Selected, then excluded; storage.
- [Fresh repository](https://github.com/acme/fresh/blob/f4e5f4e5f4e5f4e5f4e5f4e5f4e5f4e5f4e5f4e5/README.md)
  > # Fresh repository In scope, not selected.

## Expired decisions

- `a::notes/stale.md` — exclude by tester at 2026-09-01T08:00:00Z (page changed): decided on older bytes
- `a::notes/vanished.md` — exclude by tester at 2026-09-01T08:00:00Z (page gone): the page no longer exists
- `ext::docs/ghost.md` — exclude by tester at 2026-09-01T08:00:00Z (page changed): a dangling link has no bytes

## Unresolved links

- `book::src/guide/missing.md`
- `ext::docs/ghost.md`

## Archived sources

_none_

## Duplicates

### exact

- [Zebra](https://github.com/acme/a-b/blob/abababababababababababababababababababab/docs/zebra.md) ← [Zebra](https://github.com/acme/fresh/blob/f4e5f4e5f4e5f4e5f4e5f4e5f4e5f4e5f4e5f4e5/docs/zebra.md) (similarity 1.000, suggested: review) — priority and selected_by tie; a-b::docs/zebra.md sorts first
- [License terms](https://github.com/acme/a/blob/a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1/docs/license.md) ← [License terms](https://github.com/acme/a-b/blob/abababababababababababababababababababab/docs/license.md) (similarity 1.000, suggested: exclude) — priority 10 > 1
- [Questions](https://github.com/acme/ext/blob/e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1/docs/questions.md) ← [Questions](https://github.com/acme/ext/blob/e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1/extra/faq.md) (similarity 1.000, suggested: exclude) — resolver beats include

### mirror

- [Introduction](https://github.com/acme/a/blob/a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1/docs/intro.md) ← [Introduction](https://github.com/acme/book/blob/b00cb00cb00cb00cb00cb00cb00cb00cb00cb00c/src/intro.md) (similarity 0.000, suggested: exclude) — priority 10 > 5
- [Setup guide](https://github.com/acme/a/blob/a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1/docs/setup.md) ← [Setup guide](https://github.com/acme/book/blob/b00cb00cb00cb00cb00cb00cb00cb00cb00cb00c/src/guide/setup.md) (similarity 0.000, suggested: exclude) — priority 10 > 5

### near

- [Email notifications](https://github.com/acme/a/blob/a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1/docs/notifications.md) ← [Sending email](https://github.com/acme/a-b/blob/abababababababababababababababababababab/docs/notifications.md) (similarity 0.806, suggested: exclude) — priority 10 > 1

