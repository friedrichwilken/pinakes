"""Tests for the `pinakes` Python bindings (SPEC §17.2) over the golden fixture artifact.

The fixture (``tests/fixtures/golden/artifact`` at the repository root) and its priorities
mirror ``tests/fixtures/golden/pinakes.yaml`` and ``tests/golden.rs``: ``handbook`` (10)
outranks ``guides`` (5), which outranks ``cookbook`` and ``schemas`` (1 each), so the mirrored
pages in the lower-priority sources are left out of the search corpus. ``tests/queries.jsonl``
pins the query -> expected-page pairs reused below.
"""

import hashlib
from pathlib import Path

import pytest

from pinakes import Index

ARTIFACT = Path(__file__).resolve().parents[2] / "tests" / "fixtures" / "golden" / "artifact"

# tests/fixtures/golden/pinakes.yaml's source priorities.
PRIORITIES = {"handbook": 10, "guides": 5, "cookbook": 1, "schemas": 1}


@pytest.fixture(scope="module")
def index() -> Index:
    """The golden fixture built with the same priorities as ``tests/golden.rs``."""
    return Index.build(str(ARTIFACT), PRIORITIES)


def test_build_counts_pages_and_collapses_mirrors(index: Index) -> None:
    # Pinned by tests/golden.rs: 33 pages read, 3 mirrored away in lower-priority sources.
    assert index.page_count == 33
    assert index.searchable_count == 30


def test_build_without_priorities_collapses_nothing() -> None:
    # Equal (default) priorities never collapse a page (SPEC §5): every page stays searchable.
    plain = Index.build(str(ARTIFACT))
    assert plain.page_count == 33
    assert plain.searchable_count == 33


def test_search_returns_the_known_top_page(index: Index) -> None:
    # tests/fixtures/golden/queries.jsonl: "install" -> handbook::docs/install.md.
    hits = index.search("how do I install the service", k=5)
    assert hits
    assert hits[0].page_id == "handbook::docs/install.md"
    assert hits[0].score > 0
    assert repr(hits[0]).startswith("Hit(")


def test_search_respects_k(index: Index) -> None:
    # "storage" appears across many golden fixture pages, so k actually caps the result.
    hits = index.search("storage", k=3)
    assert len(hits) == 3
    assert len(index.search("storage", k=100)) > 3


def test_search_module_filter_restricts_results(index: Index) -> None:
    # "rotate the signing keys" -> guides::docs/rotate-keys.md (tests/fixtures/golden/queries.jsonl).
    hits = index.search("rotate the signing keys", k=10, module="guides")
    assert hits
    assert hits[0].page_id == "guides::docs/rotate-keys.md"
    assert all(hit.page_id.startswith("guides::") for hit in hits)


def test_read_round_trips_page_metadata(index: Index) -> None:
    page = index.read("handbook::docs/install.md")
    assert page is not None
    assert page.module == "handbook"
    assert page.url.startswith(
        "https://github.com/example-org/handbook/blob/"
    )
    assert page.url.endswith("/docs/install.md")
    assert page.content
    assert repr(page).startswith("Page(")


def test_read_missing_page_returns_none(index: Index) -> None:
    assert index.read("handbook::docs/does-not-exist.md") is None


def test_chunks_are_the_units_eval_measures(index: Index) -> None:
    # Pinned by tests/chunks_cli.rs: 52 units over the 30 searchable pages.
    chunks = index.chunks()
    assert len(chunks) == 52
    assert chunks[0].id == "cookbook::docs/README.md#0"
    assert chunks[0].ordinal == 0
    assert chunks[0].heading == ""
    assert chunks[0].text.startswith("Cookbook\n\n# Cookbook\n\n")
    assert repr(chunks[0]).startswith("Chunk(")
    # ids are page#ordinal, ordinals restart at 0 on every page and run consecutively.
    next_ordinal: dict[str, int] = {}
    for chunk in chunks:
        assert chunk.id == f"{chunk.page}#{chunk.ordinal}"
        assert chunk.ordinal == next_ordinal.get(chunk.page, 0)
        next_ordinal[chunk.page] = chunk.ordinal + 1
    assert len(next_ordinal) == index.searchable_count
    # sha256 is the hash of the text's UTF-8 bytes.
    for chunk in chunks:
        assert chunk.sha256 == hashlib.sha256(chunk.text.encode("utf-8")).hexdigest()
