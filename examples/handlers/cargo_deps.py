#!/usr/bin/env python3
"""An external render step (SPEC §10.1) that turns a ``Cargo.toml`` into a reference page.

pinakes runs this once per source after selection, with cwd = the checkout and
``PINAKES_OUT`` set to the directory pages go into. Each selected file arrives as one JSON
line on stdin (``{"path": "Cargo.toml"}``); for each page written this script prints one
JSON line on stdout with ``path`` relative to ``PINAKES_OUT``:

    {"path": "reference/pinakes-dependencies.md", "source_path": "Cargo.toml",
     "title": "...", "doc_type": "reference", "section": "Dependencies"}

A selected file that no output line names is dropped from the artifact and listed in the
source's ``meta.json`` under ``unrendered``. Needs Python 3.11 or newer for ``tomllib``.
"""

from __future__ import annotations

import json
import os
import sys
import tomllib
from pathlib import Path

TABLES = (
    ("dependencies", "runtime"),
    ("dev-dependencies", "development"),
    ("build-dependencies", "build"),
)


def rows(data: dict) -> list[tuple[str, str, str, str, str]]:
    """One row per dependency across the three dependency tables."""
    out = []
    for table, kind in TABLES:
        for name, spec in sorted(data.get(table, {}).items()):
            if isinstance(spec, str):
                version, features, optional = spec, [], False
            else:
                version = spec.get("version", spec.get("path", spec.get("git", "")))
                features = spec.get("features", [])
                optional = spec.get("optional", False)
            joined = ", ".join(f"`{feature}`" for feature in features)
            out.append((name, kind, version, joined, "yes" if optional else "no"))
    return out


def render(source_path: str, out: Path) -> dict:
    """Write the page for one Cargo.toml and return its output record."""
    data = tomllib.loads(Path(source_path).read_text(encoding="utf-8"))
    package = data.get("package", {})
    name = package.get("name") or Path(source_path).parent.name or "crate"
    version = package.get("version", "")
    title = f"{name} {version} dependencies".replace("  ", " ")
    lines = [
        f"# {title}",
        "",
        f"Crates `{name}` depends on, from `{source_path}`.",
        "",
        "| Crate | Kind | Version | Features | Optional |",
        "|---|---|---|---|---|",
    ]
    lines += [f"| `{n}` | {k} | {v} | {f} | {o} |" for n, k, v, f, o in rows(data)]
    rel = f"reference/{name}-dependencies.md"
    target = out / rel
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return {
        "path": rel,
        "source_path": source_path,
        "title": title,
        "doc_type": "reference",
        "section": "Dependencies",
    }


def main() -> int:
    """Render every selected file named on stdin."""
    out = Path(os.environ["PINAKES_OUT"])
    for line in sys.stdin:
        if line.strip():
            print(json.dumps(render(json.loads(line)["path"], out)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
