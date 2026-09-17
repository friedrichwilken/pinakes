# Corpus report

## Summary

- Sources: 6
- Pages: 17
- Residue: 18 entries, 17 undecided
- Decisions: 5
- Changes since 2026-09-16T12:00:00Z: 4 added, 1 removed, 1 changed

## Eval before/after

_No evaluation results supplied._

## Added pages

- [Subscription (messaging.example.com/v1)](https://github.com/acme/crds/blob/c4d5c4d5c4d5c4d5c4d5c4d5c4d5c4d5c4d5c4d5/reference/messaging.example.com/subscription-v1.md)
- [Subscription (messaging.example.com/v1alpha1)](https://github.com/acme/crds/blob/c4d5c4d5c4d5c4d5c4d5c4d5c4d5c4d5c4d5c4d5/reference/messaging.example.com/subscription-v1alpha1.md)
- [New page](https://github.com/acme/fresh/blob/f4e5f4e5f4e5f4e5f4e5f4e5f4e5f4e5f4e5f4e5/docs/new.md)
- [Zebra](https://github.com/acme/fresh/blob/f4e5f4e5f4e5f4e5f4e5f4e5f4e5f4e5f4e5f4e5/docs/zebra.md)

## Removed pages

- [Old page](https://github.com/acme/a/blob/a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0/docs/old.md) (gone upstream)

## Changed pages

- [Introduction](https://github.com/acme/a/blob/a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1/docs/intro.md) (+2/-1) ([compare](https://github.com/acme/a/compare/a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0...a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1))

## New residue

### the source appeared after the previous manifest, so nothing in it has been reviewed yet

- [Fresh repository](https://github.com/acme/fresh/blob/f4e5f4e5f4e5f4e5f4e5f4e5f4e5f4e5f4e5f4e5/README.md)
  > # Fresh repository In scope, not selected.
- [Other](https://github.com/acme/gone/blob/9090909090909090909090909090909090909090/other.md)
  > # Other In scope, not selected.

<details>
<summary>5 pages excluded by policy or resolver rules</summary>

### matches `policy.deny` (`**/CHANGELOG.md`)

- [Changelog](https://github.com/acme/a/blob/a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1/docs/CHANGELOG.md)
  > # Changelog - denied by policy
- [Changelog](https://github.com/acme/gone/blob/9090909090909090909090909090909090909090/docs/CHANGELOG.md)
  > # Changelog - denied

### matches `resolver.exclude` (`**/_sidebar.md`)

- `a::docs/_sidebar.md`
  > - [Intro](intro.md)

### matches `resolver.exclude` (`docs/internal.md`)

- [Internal](https://github.com/acme/ext/blob/e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1/docs/internal.md)
  > # Internal Selected, then excluded; storage.

### the source is archived upstream and policy.archived is drop

- [Archived page](https://github.com/acme/gone/blob/9090909090909090909090909090909090909090/docs/page.md)
  > # Archived page Would have been selected.

</details>

## Expired decisions

- `a::notes/stale.md` — exclude by tester at 2026-09-01T08:00:00Z (page changed): decided on older bytes
- `a::notes/vanished.md` — exclude by tester at 2026-09-01T08:00:00Z (page gone): the page no longer exists
- `ext::docs/ghost.md` — exclude by tester at 2026-09-01T08:00:00Z (page changed): a dangling link has no bytes

## Unresolved links

- [Missing chapter](https://github.com/acme/book/blob/b00cb00cb00cb00cb00cb00cb00cb00cb00cb00c/src/SUMMARY.md)
- [Ghost](https://github.com/acme/ext/blob/e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1/docs/ghost.md)

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

