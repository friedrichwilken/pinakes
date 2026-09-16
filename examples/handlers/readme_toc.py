#!/usr/bin/env python3
"""An external resolver (SPEC §3) that treats README.md as the table of contents.

pinakes runs this with cwd = the repository checkout and prints nothing itself; every line
this script writes to stdout is one candidate:

    {"path": "docs/x.md", "title": "...", "doc_type": "", "section": "...", "selected": true}

Selected: README.md and every local Markdown file it links, with the H2 the link sits under as
the page's section. Every other Markdown file in the checkout is reported with
``selected: false`` and a ``rule`` of its own, so pinakes lists it as residue under that rule
instead of the generic ``external:not-selected``. A linked file that does not exist is still
reported as selected: pinakes records it as an unresolved link.
"""

from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path

LINK = re.compile(r"\[([^\]]*)\]\(([^)\s#]+)(?:#[^)]*)?\)")
H1 = re.compile(r"^#\s+(?P<title>.+?)\s*#*\s*$")
H2 = re.compile(r"^##\s+(?P<title>.+?)\s*#*\s*$")
UNLINKED = {"key": "readme-toc:unlinked", "text": "not linked from README.md"}


def first_h1(path: Path) -> str:
    """Return the first ``# Heading`` outside fenced code, or an empty string."""
    in_fence = False
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        if line.lstrip().startswith(("```", "~~~")):
            in_fence = not in_fence
            continue
        if not in_fence and (match := H1.match(line)):
            return match.group("title")
    return ""


def linked_pages(readme: Path) -> dict[str, tuple[str, str]]:
    """Map each local Markdown link target to ``(link text, section)`` in reading order."""
    pages: dict[str, tuple[str, str]] = {}
    section = ""
    in_fence = False
    for line in readme.read_text(encoding="utf-8", errors="replace").splitlines():
        if line.lstrip().startswith(("```", "~~~")):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        if match := H2.match(line):
            section = match.group("title")
            continue
        for text, target in LINK.findall(line):
            if "://" in target or not target.endswith(".md"):
                continue
            path = os.path.normpath(os.path.join(readme.parent.as_posix(), target))
            pages.setdefault(path, (text, section))
    return pages


def main() -> int:
    """Emit one candidate per Markdown file; exit 1 only when there is no README.md."""
    readme = Path("README.md")
    if not readme.is_file():
        print("readme_toc: no README.md in the checkout", file=sys.stderr)
        return 1
    selected = {"README.md": (first_h1(readme) or "README", "")}
    for path, (text, section) in linked_pages(readme).items():
        selected.setdefault(path, (text, section))

    seen: set[str] = set()
    for path in sorted(Path().rglob("*.md")):
        if not path.is_file() or any(part.startswith(".") for part in path.parts):
            continue
        rel = path.as_posix()
        seen.add(rel)
        if rel in selected:
            text, section = selected[rel]
            record = {
                "path": rel,
                "title": first_h1(path) or text,
                "doc_type": "",
                "section": section,
                "selected": True,
            }
        else:
            record = {"path": rel, "selected": False, "rule": UNLINKED}
        print(json.dumps(record, ensure_ascii=False))
    for rel, (text, section) in selected.items():
        if rel not in seen:  # linked from README.md, but no such file
            print(json.dumps({"path": rel, "title": text, "section": section, "selected": True}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
