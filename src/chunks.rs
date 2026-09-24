//! The retrieval units of an artifact as a contract (SPEC §2.9): one [`Chunk`] per unit `eval`
//! measures, so a consumer's own index and `eval` agree on what a hit refers to.
//!
//! The cut is [`crate::index::iter_units`] itself, not a copy of its rules (SPEC §5): a chunk's
//! `text` is byte for byte the unit `embed` embeds (title, heading and body joined), and the
//! built-in index scores the same cut as three boosted fields, so `chunks` pins the units, not
//! the scores. This module depends on [`crate::corpus`], [`crate::index`] and [`crate::text`]
//! only.

use serde::{Deserialize, Serialize};

use crate::corpus::Page;
use crate::index::iter_units;
use crate::text::sha256_hex;

/// One retrieval unit of a page, as written to `chunks.jsonl` (SPEC §2.9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    /// `<page>#<ordinal>`, see [`chunk_id`].
    pub id: String,
    /// The page id, `<source>::<path>`.
    pub page: String,
    /// The H2 heading, `<H2> / <H3>` for a section split at H3, empty for the intro.
    pub heading: String,
    /// The unit's 0-based position within its page.
    pub ordinal: usize,
    /// The unit text `embed` embeds: title, heading and body joined (SPEC §5 step 4); the
    /// built-in index scores the same three parts as separate fields.
    pub text: String,
    /// Lowercase hex SHA-256 of `text`'s UTF-8 bytes.
    pub sha256: String,
}

/// The id of the unit at `ordinal` within `page`: `<page>#<ordinal>`.
pub fn chunk_id(page: &str, ordinal: usize) -> String {
    format!("{page}#{ordinal}")
}

/// The chunks of the searchable pages, in page then unit order, cut by [`iter_units`].
///
/// `pages` must already have [`crate::corpus::mark_mirrors`] applied: mirror pages yield no
/// chunks, matching what the index indexes. Ordinals restart at 0 on every page.
pub fn chunks(pages: &[Page]) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = Vec::new();
    for unit in iter_units(pages) {
        let ordinal = match out.last() {
            Some(previous) if previous.page == unit.page_id => previous.ordinal + 1,
            _ => 0,
        };
        out.push(Chunk {
            id: chunk_id(&unit.page_id, ordinal),
            page: unit.page_id,
            heading: unit.heading,
            ordinal,
            sha256: sha256_hex(unit.text.as_bytes()),
            text: unit.text,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{Priorities, load_pages, mark_mirrors};
    use crate::index::testing::{SourceSpec, write_artifact};

    fn pages() -> Vec<Page> {
        let dir = tempfile::tempdir().unwrap();
        write_artifact(
            dir.path(),
            &[
                SourceSpec {
                    name: "handbook",
                    repo: "example-org/handbook",
                    pages: &[
                        (
                            "docs/a.md",
                            "Alpha",
                            "# Alpha\n\nintro\n\n## One\n\nfirst\n\n## Two\n\nsecond\n",
                        ),
                        ("docs/b.md", "Beta", "# Beta\n\nonly the intro\n"),
                    ],
                    residue: &[],
                },
                SourceSpec {
                    name: "mirror",
                    repo: "example-org/mirror",
                    pages: &[("docs/a.md", "Alpha", "# Alpha\n\na copy\n")],
                    residue: &[],
                },
            ],
        );
        let priorities = Priorities {
            explicit: [("handbook".to_string(), 10), ("mirror".to_string(), 1)]
                .into_iter()
                .collect(),
        };
        let mut pages = load_pages(dir.path(), &priorities).unwrap();
        mark_mirrors(&mut pages);
        pages
    }

    #[test]
    fn ids_and_ordinals_restart_on_every_page_and_mirrors_are_skipped() {
        let pages = pages();
        assert_eq!(pages.len(), 3);
        let chunks = chunks(&pages);
        let ids: Vec<&str> = chunks.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "handbook::docs/a.md#0",
                "handbook::docs/a.md#1",
                "handbook::docs/a.md#2",
                "handbook::docs/b.md#0",
            ]
        );
        let ordinals: Vec<usize> = chunks.iter().map(|c| c.ordinal).collect();
        assert_eq!(ordinals, [0, 1, 2, 0]);
        let headings: Vec<&str> = chunks.iter().map(|c| c.heading.as_str()).collect();
        assert_eq!(headings, ["", "One", "Two", ""]);
        assert!(chunks.iter().all(|c| !c.page.starts_with("mirror::")));
    }

    #[test]
    fn text_is_the_unit_text_and_sha256_hashes_it() {
        let pages = pages();
        let chunks = chunks(&pages);
        let units = iter_units(&pages);
        assert_eq!(chunks.len(), units.len());
        for (chunk, unit) in chunks.iter().zip(&units) {
            assert_eq!(chunk.text, unit.text);
            assert_eq!(chunk.sha256, sha256_hex(unit.text.as_bytes()));
            assert_eq!(chunk.sha256.len(), 64);
        }
        // The body keeps the blank line after the heading line and the one before the next.
        assert_eq!(chunks[1].text, "Alpha\nOne\n\n\nfirst\n");
        assert_eq!(chunks[3].text, "Beta\n\n# Beta\n\nonly the intro");
    }

    #[test]
    fn chunk_id_joins_page_and_ordinal_with_a_hash() {
        assert_eq!(chunk_id("src::docs/x.md", 4), "src::docs/x.md#4");
    }

    #[test]
    fn a_chunk_round_trips_through_json_with_sorted_keys() {
        let chunk = Chunk {
            id: "s::p.md#0".to_string(),
            page: "s::p.md".to_string(),
            heading: String::new(),
            ordinal: 0,
            text: "T\n\nbody".to_string(),
            sha256: sha256_hex(b"T\n\nbody"),
        };
        let line =
            crate::jsonl::to_string(std::slice::from_ref(&chunk), crate::jsonl::KeyOrder::Sorted)
                .unwrap();
        assert!(line.starts_with("{\"heading\":\"\",\"id\":\"s::p.md#0\",\"ordinal\":0,"));
        let back: Vec<Chunk> = crate::jsonl::parse(&line).unwrap();
        assert_eq!(back, [chunk]);
    }
}
