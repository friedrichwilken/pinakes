#!/usr/bin/env python3
"""An external resolver for pinakes (SPEC §3), dependency-free.

Walks ``<root>/**/*.md`` under the current directory (pinakes runs us with cwd = the
repository checkout; ``root`` is the optional first argument, default ``docs``) and emits one
JSON object per file on stdout:

    {"path": "docs/user/x.md", "title": "...", "doc_type": "", "section": "docs/user",
     "selected": true}

A file is selected when it has a frontmatter ``title:`` or a first-level ``# Heading``; files
without either, and mdBook ``SUMMARY.md`` tables of contents, are reported with
``selected: false`` so pinakes lists them as residue. The environment carries
``PINAKES_SOURCE`` and ``PINAKES_COMMIT``; they are echoed to stderr only, to show they arrive.
"""

from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path

H1 = re.compile(r"^#\s+(?P<title>.+?)\s*#*\s*$")
FRONTMATTER_TITLE = re.compile(r"^title:\s*(?P<title>.+?)\s*$")


def frontmatter_title(lines: list[str]) -> str:
    """Return the ``title:`` value of a leading ``---`` block, or an empty string."""
    if not lines or lines[0].strip() != "---":
        return ""
    for line in lines[1:]:
        if line.strip() == "---":
            return ""
        match = FRONTMATTER_TITLE.match(line)
        if match:
            return match.group("title").strip("\"'")
    return ""


def first_h1(lines: list[str]) -> str:
    """Return the first ``# Heading`` outside frontmatter and fenced code, or an empty string."""
    in_frontmatter = bool(lines) and lines[0].strip() == "---"
    in_fence = False
    for index, line in enumerate(lines):
        if in_frontmatter:
            if index > 0 and line.strip() == "---":
                in_frontmatter = False
            continue
        if line.lstrip().startswith(("```", "~~~")):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        match = H1.match(line)
        if match:
            return match.group("title")
    return ""


def doc_type_for(path: Path) -> str:
    """A coarse document type from the directory name, empty when unknown."""
    parts = {part.lower() for part in path.parts}
    if "tutorials" in parts or "tutorial" in parts:
        return "tutorial"
    if "troubleshooting" in parts:
        return "troubleshooting"
    if "resources" in parts or "technical-reference" in parts:
        return "reference"
    if "release-notes" in parts:
        return "release-notes"
    return ""


def main() -> int:
    """Emit the candidate list; exit 0 even when nothing is selected."""
    source = os.environ.get("PINAKES_SOURCE", "?")
    commit = os.environ.get("PINAKES_COMMIT", "?")
    print(f"frontmatter_title: resolving {source} at {commit[:12]}", file=sys.stderr)
    docs = Path(sys.argv[1] if len(sys.argv) > 1 else "docs")
    if not docs.is_dir():
        print(f"frontmatter_title: no {docs}/ directory", file=sys.stderr)
        return 0
    for path in sorted(docs.rglob("*.md")):
        if not path.is_file():
            continue
        try:
            lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError as err:
            print(f"frontmatter_title: {path}: {err}", file=sys.stderr)
            return 1
        title = frontmatter_title(lines) or first_h1(lines)
        is_toc = path.name == "SUMMARY.md"
        record = {
            "path": path.as_posix(),
            "title": title,
            "doc_type": doc_type_for(path),
            "section": path.parent.as_posix(),
            "selected": bool(title) and not is_toc,
        }
        if is_toc:
            record["context"] = "mdBook table of contents"
        print(json.dumps(record, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
