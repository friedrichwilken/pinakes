//! The in-memory BM25 index over the searchable pages of an artifact, and its scoring.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use tantivy::postings::Postings;
use tantivy::schema::{
    FAST, Field, IndexRecordOption, STORED, Schema, TantivyDocument, TextFieldIndexing,
    TextOptions, Value as _,
};
use tantivy::{DocAddress, DocSet, IndexWriter, Searcher, TERMINATED, Term};

use super::IndexError;
use super::sections::{index_text, split_sections};
use crate::corpus::{Page, Priorities, load_derived, load_pages, mark_mirrors};
use crate::num::float;
use crate::tokenizer::{PinakesTokenizer, TOKENIZER_NAME, title_key, tokenize};

/// Weight of the page title in a unit.
pub const TITLE_BOOST: u32 = 3;
/// Weight of the unit heading in a unit.
pub const HEADING_BOOST: u32 = 2;

/// BM25 term-frequency saturation.
const K1: f64 = 1.5;
/// BM25 length normalisation.
const B: f64 = 0.75;
/// Fraction of the average IDF used for terms in more than half of the units.
const EPSILON: f64 = 0.25;
/// Memory budget of the single-threaded index writer.
const WRITER_BUDGET: usize = 64 << 20;

/// One retrieval unit (SPEC §5): the one cut [`Index::from_pages`] indexes as three fields,
/// a consumer embeds as `text` and `chunks` (SPEC §2.9) emits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// The unit's own identity, `<page id>#<ordinal>` (see [`chunk_id`]): `ordinal` is the
    /// unit's 0-based position within its page, in [`split_sections`](super::split_sections)
    /// order. Deterministic, so a consumer can derive the same id from the splitting rules.
    pub id: String,
    /// The unit's 0-based position within its page: the number in [`Unit::id`].
    pub ordinal: usize,
    /// The page this unit belongs to.
    pub page_id: String,
    /// The page title, the index's `title` field.
    pub title: String,
    /// The unit's heading, empty for the intro (see [`Section::heading`](super::Section::heading));
    /// the index's `heading` field.
    pub heading: String,
    /// The section text, the index's `body` field.
    pub body: String,
    /// Title, heading and body joined into one text worth embedding.
    pub text: String,
}

/// The id of the unit at `ordinal` within `page`: `<page>#<ordinal>`.
pub fn chunk_id(page: &str, ordinal: usize) -> String {
    format!("{page}#{ordinal}")
}

/// The retrieval units of the searchable (non-mirror) pages, in page then section order.
///
/// `pages` must already have [`mark_mirrors`] applied; mirrors are skipped. This is the only
/// place the cut is made: [`Index::from_pages`] indexes exactly these units.
pub fn iter_units(pages: &[Page]) -> Vec<Unit> {
    let mut units = Vec::new();
    for page in pages.iter().filter(|p| p.mirror_of.is_none()) {
        for (ordinal, section) in split_sections(&index_text(&page.content))
            .into_iter()
            .enumerate()
        {
            let text = if section.heading.is_empty() {
                format!("{}\n\n{}", page.title, section.body)
            } else {
                format!("{}\n{}\n\n{}", page.title, section.heading, section.body)
            };
            units.push(Unit {
                id: chunk_id(&page.id, ordinal),
                ordinal,
                page_id: page.id.clone(),
                title: page.title.clone(),
                heading: section.heading,
                body: section.body,
                text,
            });
        }
    }
    units
}

/// One search result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// `<source>::<path>`.
    pub page_id: String,
    /// BM25 score of the page's best unit.
    pub score: f64,
    /// Heading of the best unit, empty for the intro.
    pub heading: String,
}

#[derive(Debug, Clone, Copy)]
struct Fields {
    title: Field,
    heading: Field,
    body: Field,
    page_id: Field,
    source: Field,
    doc_type: Field,
    page: Field,
    len: Field,
}

impl Fields {
    fn schema() -> (Schema, Fields) {
        let indexing = TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER_NAME)
            .set_index_option(IndexRecordOption::WithFreqs);
        let indexed = TextOptions::default().set_indexing_options(indexing);
        let mut builder = Schema::builder();
        let fields = Fields {
            title: builder.add_text_field("title", indexed.clone().set_stored()),
            heading: builder.add_text_field("heading", indexed.clone().set_stored()),
            body: builder.add_text_field("body", indexed),
            page_id: builder.add_text_field("page_id", STORED),
            source: builder.add_text_field("source", STORED),
            doc_type: builder.add_text_field("doc_type", STORED),
            page: builder.add_u64_field("page", FAST),
            len: builder.add_u64_field("len", FAST),
        };
        (builder.build(), fields)
    }

    fn boosted(&self) -> [(Field, f64); 3] {
        [
            (self.title, f64::from(TITLE_BOOST)),
            (self.heading, f64::from(HEADING_BOOST)),
            (self.body, 1.0),
        ]
    }
}

/// Corpus statistics gathered while writing units.
#[derive(Default)]
struct Stats {
    units: usize,
    total_len: usize,
    df: HashMap<String, usize>,
}

impl Stats {
    fn add_unit(&mut self, title: &[String], heading: &[String], body: &[String]) -> usize {
        let len = TITLE_BOOST as usize * title.len()
            + HEADING_BOOST as usize * heading.len()
            + body.len();
        self.units += 1;
        self.total_len += len;
        let terms: HashSet<&String> = title.iter().chain(heading).chain(body).collect();
        for term in terms {
            *self.df.entry(term.clone()).or_default() += 1;
        }
        len
    }

    fn avgdl(&self) -> f64 {
        if self.units == 0 {
            0.0
        } else {
            float(self.total_len) / float(self.units)
        }
    }

    /// Mean IDF over the vocabulary, negative values included; the epsilon floor is relative
    /// to it.
    fn avg_idf(&self) -> f64 {
        if self.df.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.df.values().map(|&df| raw_idf(self.units, df)).sum();
        sum / float(self.df.len())
    }
}

fn raw_idf(units: usize, df: usize) -> f64 {
    (float(units) - float(df) + 0.5).ln() - (float(df) + 0.5).ln()
}

/// Per dense unit index, the query score and whether any query token occurred in the unit.
///
/// The two are tracked apart because BM25 scores can be negative on a tiny corpus (every term
/// in more than half of the units gives a negative average IDF), and matching, not the sign
/// of the score, decides whether a page is a result at all.
struct UnitScores {
    score: Vec<f64>,
    matched: Vec<bool>,
}

/// The in-memory BM25 index over the searchable pages of an artifact.
pub struct Index {
    pages: Vec<Page>,
    /// Indices into `pages` of the searchable (non-mirror) pages, in unit order.
    searchable: Vec<usize>,
    searcher: Searcher,
    fields: Fields,
    /// First dense unit index of every segment.
    segment_offsets: Vec<usize>,
    /// Per dense unit index, the position in `searchable`.
    unit_page: Vec<usize>,
    /// Per dense unit index, the boosted token count.
    unit_len: Vec<f64>,
    avgdl: f64,
    avg_idf: f64,
}

impl std::fmt::Debug for Index {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Index")
            .field("pages", &self.pages.len())
            .field("searchable", &self.searchable.len())
            .field("units", &self.unit_len.len())
            .finish_non_exhaustive()
    }
}

impl Index {
    /// Load every page of `artifact` and index the searchable ones, with the generated
    /// questions of its `derived.jsonl` (SPEC §14.4) when it has one.
    pub fn build(artifact: &Path, priorities: &Priorities) -> Result<Index, IndexError> {
        let pages = load_pages(artifact, priorities)?;
        Index::from_pages_with_derived(pages, &load_derived(artifact)?)
    }

    /// Apply the mirror rule to `pages` and index the searchable ones.
    pub fn from_pages(pages: Vec<Page>) -> Result<Index, IndexError> {
        Index::from_pages_with_derived(pages, &BTreeMap::new())
    }

    /// Like [`Index::from_pages`], and index `derived` (page id to generated questions) as
    /// well (SPEC §14.4).
    ///
    /// The questions of a page are one extra document in the index, matched through the body
    /// field and attributed to that page, so a hit on them is a hit on the page and the page
    /// itself is unchanged. They are not a retrieval unit: [`iter_units`] and the `chunks` it
    /// feeds do not know them. Questions for a page that is missing or not searchable (a
    /// mirror) are ignored.
    pub fn from_pages_with_derived(
        mut pages: Vec<Page>,
        derived: &BTreeMap<String, Vec<String>>,
    ) -> Result<Index, IndexError> {
        mark_mirrors(&mut pages);
        let searchable: Vec<usize> = (0..pages.len())
            .filter(|&i| pages[i].mirror_of.is_none())
            .collect();
        let (schema, fields) = Fields::schema();
        let index = tantivy::Index::create_in_ram(schema);
        index
            .tokenizers()
            .register(TOKENIZER_NAME, PinakesTokenizer);
        let mut writer: IndexWriter<TantivyDocument> =
            index.writer_with_num_threads(1, WRITER_BUDGET)?;
        let mut stats = Stats::default();
        // Position in `searchable` by page id; `iter_units` yields units in that same order.
        let position_of: HashMap<&str, usize> = searchable
            .iter()
            .enumerate()
            .map(|(position, &page_index)| (pages[page_index].id.as_str(), position))
            .collect();
        for unit in iter_units(&pages) {
            let Some(&position) = position_of.get(unit.page_id.as_str()) else {
                continue;
            };
            let page = &pages[searchable[position]];
            let title_tokens = tokenize(&unit.title);
            let heading_tokens = tokenize(&unit.heading);
            let body_tokens = tokenize(&unit.body);
            let len = stats.add_unit(&title_tokens, &heading_tokens, &body_tokens);
            let mut doc = TantivyDocument::default();
            doc.add_text(fields.title, &unit.title);
            doc.add_text(fields.heading, &unit.heading);
            doc.add_text(fields.body, &unit.body);
            doc.add_text(fields.page_id, &unit.page_id);
            doc.add_text(fields.source, &page.source);
            doc.add_text(fields.doc_type, &page.doc_type);
            doc.add_u64(fields.page, position as u64);
            doc.add_u64(fields.len, len as u64);
            writer.add_document(doc)?;
        }
        for (id, questions) in derived {
            let Some(&position) = position_of.get(id.as_str()) else {
                continue;
            };
            let page = &pages[searchable[position]];
            let body = questions.join("\n");
            let body_tokens = tokenize(&body);
            if body_tokens.is_empty() {
                continue;
            }
            let len = stats.add_unit(&[], &[], &body_tokens);
            let mut doc = TantivyDocument::default();
            doc.add_text(fields.title, "");
            doc.add_text(fields.heading, "");
            doc.add_text(fields.body, &body);
            doc.add_text(fields.page_id, id);
            doc.add_text(fields.source, &page.source);
            doc.add_text(fields.doc_type, &page.doc_type);
            doc.add_u64(fields.page, position as u64);
            doc.add_u64(fields.len, len as u64);
            writer.add_document(doc)?;
        }
        writer.commit()?;
        let searcher = index.reader()?.searcher();
        let (segment_offsets, unit_page, unit_len) = dense_units(&searcher)?;
        Ok(Index {
            pages,
            searchable,
            searcher,
            fields,
            segment_offsets,
            unit_page,
            unit_len,
            avgdl: stats.avgdl(),
            avg_idf: stats.avg_idf(),
        })
    }

    /// Every page read from the artifact, mirrors included.
    pub fn pages(&self) -> &[Page] {
        &self.pages
    }

    /// The page with this id, if any.
    pub fn page(&self, id: &str) -> Option<&Page> {
        self.pages.iter().find(|p| p.id == id)
    }

    /// Number of pages read, mirrors included.
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Number of pages in the search corpus (mirrors excluded).
    pub fn searchable_count(&self) -> usize {
        self.searchable.len()
    }

    /// The best `k` pages for `query`: pages ranked by their best unit, results de-duplicated
    /// by tokenised title, pages that match no query token left out.
    ///
    /// With `module`, only pages of that module (case-insensitive) are returned unless none
    /// scores, in which case the filter is dropped.
    pub fn search(
        &self,
        query: &str,
        k: usize,
        module: Option<&str>,
    ) -> Result<Vec<Hit>, IndexError> {
        if self.unit_len.is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        let scores = self.unit_scores(&tokenize(query))?;
        let (best, best_unit, matched) = self.page_scores(&scores);
        let mut ranked: Vec<usize> = (0..self.searchable.len()).filter(|&p| matched[p]).collect();
        ranked.sort_by(|&a, &b| best[b].total_cmp(&best[a]));
        if let Some(module) = module.filter(|m| !m.is_empty()) {
            let wanted = module.to_lowercase();
            let filtered: Vec<usize> = ranked
                .iter()
                .copied()
                .filter(|&p| self.pages[self.searchable[p]].module.to_lowercase() == wanted)
                .collect();
            if !filtered.is_empty() {
                ranked = filtered;
            }
        }
        let mut seen = HashSet::new();
        let mut hits = Vec::new();
        for p in ranked {
            let page = &self.pages[self.searchable[p]];
            let key = title_key(&page.title);
            if !key.is_empty() && !seen.insert(key) {
                continue;
            }
            hits.push(Hit {
                page_id: page.id.clone(),
                score: best[p],
                heading: self.unit_heading(best_unit[p])?,
            });
            if hits.len() == k {
                break;
            }
        }
        Ok(hits)
    }

    /// BM25 score of every unit for the query tokens (repeated tokens count twice), and
    /// whether the unit contains any of them.
    fn unit_scores(&self, tokens: &[String]) -> Result<UnitScores, IndexError> {
        let units = self.unit_len.len();
        let mut scores = UnitScores {
            score: vec![0.0; units],
            matched: vec![false; units],
        };
        let mut tf = vec![0.0; units];
        for token in tokens {
            tf.fill(0.0);
            let mut df = 0usize;
            for (segment, reader) in self.searcher.segment_readers().iter().enumerate() {
                let offset = self.segment_offsets[segment];
                for (field, boost) in self.fields.boosted() {
                    let term = Term::from_field_text(field, token);
                    let inverted = reader.inverted_index(field)?;
                    let Some(mut postings) =
                        inverted.read_postings(&term, IndexRecordOption::WithFreqs)?
                    else {
                        continue;
                    };
                    let mut doc = postings.doc();
                    while doc != TERMINATED {
                        let unit = offset + doc as usize;
                        if tf[unit] == 0.0 {
                            df += 1;
                        }
                        tf[unit] += boost * f64::from(postings.term_freq());
                        doc = postings.advance();
                    }
                }
            }
            if df == 0 {
                continue;
            }
            let idf = self.idf(df);
            for (unit, &freq) in tf.iter().enumerate() {
                if freq > 0.0 {
                    let norm = (1.0 - B) + (B * self.unit_len[unit]) / self.avgdl;
                    scores.score[unit] += idf * (freq * (K1 + 1.0) / (freq + K1 * norm));
                    scores.matched[unit] = true;
                }
            }
        }
        Ok(scores)
    }

    fn idf(&self, df: usize) -> f64 {
        let idf = raw_idf(self.unit_len.len(), df);
        if idf < 0.0 {
            EPSILON * self.avg_idf
        } else {
            idf
        }
    }

    /// Per searchable page, the best unit score, the dense index of that unit (the first one
    /// on ties) and whether any unit matched a query token.
    fn page_scores(&self, scores: &UnitScores) -> (Vec<f64>, Vec<usize>, Vec<bool>) {
        let pages = self.searchable.len();
        let mut best = vec![f64::NEG_INFINITY; pages];
        let mut best_unit = vec![0; pages];
        let mut matched = vec![false; pages];
        for (unit, &score) in scores.score.iter().enumerate() {
            let page = self.unit_page[unit];
            if score > best[page] {
                best[page] = score;
                best_unit[page] = unit;
            }
            matched[page] |= scores.matched[unit];
        }
        (best, best_unit, matched)
    }

    /// The stored heading of a unit by dense index.
    fn unit_heading(&self, unit: usize) -> Result<String, IndexError> {
        let segment = self
            .segment_offsets
            .partition_point(|&start| start <= unit)
            .saturating_sub(1);
        let doc_id = u32::try_from(unit - self.segment_offsets[segment]).unwrap_or(u32::MAX);
        let address = DocAddress::new(u32::try_from(segment).unwrap_or(u32::MAX), doc_id);
        let doc: TantivyDocument = self.searcher.doc(address)?;
        Ok(doc
            .get_first(self.fields.heading)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string())
    }
}

/// Segment offsets and, per dense unit index, the page position and boosted length.
#[allow(clippy::type_complexity)]
fn dense_units(searcher: &Searcher) -> Result<(Vec<usize>, Vec<usize>, Vec<f64>), IndexError> {
    let mut offsets = Vec::new();
    let mut unit_page = Vec::new();
    let mut unit_len = Vec::new();
    for reader in searcher.segment_readers() {
        offsets.push(unit_page.len());
        let page = reader.fast_fields().u64("page")?;
        let len = reader.fast_fields().u64("len")?;
        for doc in 0..reader.max_doc() {
            let position = page.first(doc).unwrap_or(0);
            unit_page.push(usize::try_from(position).unwrap_or(0));
            let boosted = len.first(doc).unwrap_or(0);
            unit_len.push(float(usize::try_from(boosted).unwrap_or(0)));
        }
    }
    Ok((offsets, unit_page, unit_len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{CorpusError, SourceMeta, load_residue_page, make_page};
    use crate::index::testing::{SourceSpec, write_artifact};
    /// The two-source test artifact's priorities: the first source outranks the second.
    fn priorities() -> Priorities {
        Priorities {
            explicit: [("handbook".to_string(), 10), ("guides".to_string(), 1)].into(),
        }
    }

    fn artifact() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        write_artifact(
            dir.path(),
            &[
                SourceSpec {
                    name: "handbook",
                    repo: "example-org/handbook",
                    pages: &[
                        (
                            "docs/user/README.md",
                            "Storage Module",
                            "# Storage\n\nThe storage module keeps uploaded files.\n\n## Upload caching\n\nEnable upload caching with a bucket label.\n",
                        ),
                        (
                            "docs/user/tutorials/quotas.md",
                            "",
                            "# Configure Quotas\n\nRate limits in strict mode.\n",
                        ),
                        (
                            "docs/user/tutorials/quotas-copy.md",
                            "",
                            "# Configure Quotas\n\nRate limits in strict mode, copied.\n",
                        ),
                    ],
                    residue: &[(
                        "docs/user/extra.md",
                        "# Upload caching details\n\nupload caching upload caching\n",
                    )],
                },
                SourceSpec {
                    name: "guides",
                    repo: "example-org/guides",
                    pages: &[
                        (
                            "docs/storage.md",
                            "Storage Module",
                            "# Storage module\n\nMirror of the module page with upload caching.\n",
                        ),
                        (
                            "docs/billing.md",
                            "Billing",
                            "# Billing\n\nInvoices scale to zero.\n",
                        ),
                    ],
                    residue: &[],
                },
            ],
        );
        std::fs::write(dir.path().join("manifest.json"), "{}").unwrap();
        std::fs::create_dir_all(dir.path().join("_scratch")).unwrap();
        std::fs::write(dir.path().join("_scratch/x.md"), "# ignored\n").unwrap();
        dir
    }

    fn ids(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|h| h.page_id.as_str()).collect()
    }

    /// Generated questions are one extra document per page, attributed to that page: a query in
    /// the questions' words finds a page whose own text never uses them, the units and the
    /// page count do not change, and questions for a mirror or an unknown page are ignored.
    #[test]
    fn derived_questions_find_a_page_by_words_it_does_not_use() {
        let dir = artifact();
        let mut pages = load_pages(dir.path(), &priorities()).unwrap();
        mark_mirrors(&mut pages);
        let units_before = iter_units(&pages).len();

        let plain = Index::build(dir.path(), &priorities()).unwrap();
        let query = "preinstalled on my cluster";
        assert!(plain.search(query, 5, None).unwrap().is_empty());

        let derived: BTreeMap<String, Vec<String>> = [
            (
                "handbook::docs/user/README.md",
                vec!["Is the storage module preinstalled on my cluster?"],
            ),
            ("guides::docs/storage.md", vec!["preinstalled cluster"]),
            ("nobody::gone.md", vec!["preinstalled cluster"]),
            ("guides::docs/billing.md", vec!["   "]),
        ]
        .into_iter()
        .map(|(id, q)| (id.to_string(), q.into_iter().map(str::to_string).collect()))
        .collect();
        let index = Index::from_pages_with_derived(pages.clone(), &derived).unwrap();
        let hits = index.search(query, 5, None).unwrap();
        assert_eq!(ids(&hits), ["handbook::docs/user/README.md"], "{hits:?}");
        assert_eq!(hits[0].heading, "", "the questions are not a section");

        assert_eq!(index.page_count(), plain.page_count());
        assert_eq!(index.searchable_count(), plain.searchable_count());
        assert_eq!(
            iter_units(index.pages()).len(),
            units_before,
            "units are untouched"
        );
        // Without derived text nothing else changes: same hits, same scores.
        let same = Index::from_pages_with_derived(pages, &BTreeMap::new()).unwrap();
        assert_eq!(
            same.search("upload caching", 5, None).unwrap(),
            plain.search("upload caching", 5, None).unwrap()
        );
    }

    #[test]
    fn build_reads_derived_jsonl_from_the_artifact() {
        let dir = artifact();
        let query = "preinstalled on my cluster";
        assert!(
            Index::build(dir.path(), &priorities())
                .unwrap()
                .search(query, 5, None)
                .unwrap()
                .is_empty()
        );
        std::fs::write(
            dir.path().join("derived.jsonl"),
            concat!(
                "{\"kind\":\"questions\",\"page\":\"handbook::docs/user/README.md\",",
                "\"text\":[\"Is the storage module preinstalled on my cluster?\"]}\n",
                "{\"kind\":\"summary\",\"page\":\"handbook::docs/user/tutorials/quotas.md\",",
                "\"text\":[\"a kind this build does not know\"]}\n",
            ),
        )
        .unwrap();
        let index = Index::build(dir.path(), &priorities()).unwrap();
        let hits = index.search(query, 5, None).unwrap();
        assert_eq!(ids(&hits), ["handbook::docs/user/README.md"], "{hits:?}");
        assert!(
            index
                .search("a kind this build does not know", 5, None)
                .unwrap()
                .is_empty(),
            "an unknown kind is ignored"
        );

        std::fs::write(dir.path().join("derived.jsonl"), "{not json}\n").unwrap();
        assert!(matches!(
            Index::build(dir.path(), &priorities()).unwrap_err(),
            IndexError::Corpus(CorpusError::Derived(_))
        ));
    }

    #[test]
    fn index_counts_mirrors_and_ranks_pages() {
        let dir = artifact();
        let index = Index::build(dir.path(), &priorities()).unwrap();
        assert_eq!(index.page_count(), 5);
        assert_eq!(
            index.searchable_count(),
            4,
            "guides::docs/storage.md is a mirror"
        );
        assert_eq!(
            index
                .page("guides::docs/storage.md")
                .unwrap()
                .mirror_of
                .as_deref(),
            Some("handbook::docs/user/README.md")
        );

        let hits = index.search("enable upload caching", 10, None).unwrap();
        assert_eq!(hits[0].page_id, "handbook::docs/user/README.md");
        assert_eq!(hits[0].heading, "Upload caching");
        assert!(hits.iter().all(|h| h.page_id != "guides::docs/storage.md"));

        // Two pages with the same H1 in one source both stay indexed but collapse in results.
        let hits = index.search("quotas rate limits", 10, None).unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.page_id.as_str()).collect();
        assert_eq!(ids, ["handbook::docs/user/tutorials/quotas.md"]);

        // Pages without any matching token are not returned.
        assert!(
            index
                .search("nothing matches here", 10, None)
                .unwrap()
                .is_empty()
        );
        assert!(index.search("the of", 10, None).unwrap().is_empty());

        // Module filter (case-insensitive), with fallback to the global ranking when the
        // module has no scoring page (the guides storage page is a mirror).
        let hits = index.search("storage billing", 10, Some("GUIDES")).unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.page_id.as_str()).collect();
        assert_eq!(ids, ["guides::docs/billing.md"]);
        let hits = index.search("storage upload", 10, Some("guides")).unwrap();
        assert_eq!(hits[0].page_id, "handbook::docs/user/README.md");
        let hits = index.search("billing invoices", 10, Some("nope")).unwrap();
        assert_eq!(hits[0].page_id, "guides::docs/billing.md");
    }

    #[test]
    fn residue_pages_can_be_added() {
        let dir = artifact();
        let priorities = priorities();
        let extra =
            load_residue_page(dir.path(), "handbook::docs/user/extra.md", &priorities).unwrap();
        assert_eq!(extra.title, "Upload caching details");
        assert_eq!(extra.repo, "example-org/handbook");
        assert!(matches!(
            load_residue_page(dir.path(), "handbook::nope.md", &priorities).unwrap_err(),
            CorpusError::MissingResidue { .. }
        ));
        assert!(matches!(
            load_residue_page(dir.path(), "no-separator", &priorities).unwrap_err(),
            CorpusError::BadPageId(_)
        ));
        let mut pages = load_pages(dir.path(), &priorities).unwrap();
        pages.push(extra);
        let index = Index::from_pages(pages).unwrap();
        assert_eq!(index.searchable_count(), 5);
        let hits = index.search("upload caching", 10, None).unwrap();
        assert_eq!(hits[0].page_id, "handbook::docs/user/extra.md");
    }

    #[test]
    fn bm25_matches_the_documented_formula() {
        // Three single-unit pages with bodies "alpha alpha beta", "alpha" and "gamma".
        let meta = SourceMeta::default();
        let pages = vec![
            make_page("s", "a.md", "alpha alpha beta", None, &meta, 1),
            make_page("s", "b.md", "alpha", None, &meta, 1),
            make_page("s", "c.md", "gamma", None, &meta, 1),
        ];
        let index = Index::from_pages(pages).unwrap();
        let hits = index.search("beta alpha", 10, None).unwrap();
        // avgdl = 5/3; "alpha" is in 2 of 3 units, so its IDF is negative and floored.
        let avg_idf = (raw_idf(3, 2) + raw_idf(3, 1) + raw_idf(3, 1)) / 3.0;
        let bm25 = |idf: f64, tf: f64, dl: f64| {
            idf * (tf * (K1 + 1.0) / (tf + K1 * ((1.0 - B) + (B * dl) / (5.0 / 3.0))))
        };
        let expected = bm25(raw_idf(3, 1), 1.0, 3.0) + bm25(EPSILON * avg_idf, 2.0, 3.0);
        assert_eq!(hits[0].page_id, "s::a.md");
        assert!(
            (hits[0].score - expected).abs() < 1e-12,
            "{}",
            hits[0].score
        );
        assert_eq!(hits[1].page_id, "s::b.md");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn missing_artifact_is_an_error() {
        let err =
            Index::build(Path::new("/nonexistent/artifact"), &Priorities::default()).unwrap_err();
        assert!(matches!(
            err,
            IndexError::Corpus(CorpusError::NotADirectory(_))
        ));
    }

    #[test]
    fn without_priorities_nothing_collapses() {
        let dir = artifact();
        let index = Index::build(dir.path(), &Priorities::default()).unwrap();
        assert_eq!(index.page_count(), 5);
        assert_eq!(
            index.searchable_count(),
            5,
            "equal priorities: the same title in two sources is not a mirror"
        );
        assert!(index.pages().iter().all(|p| p.mirror_of.is_none()));
        // Same-title results still collapse at search time.
        let hits = index.search("upload caching", 10, None).unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.page_id.as_str()).collect();
        assert_eq!(ids, ["handbook::docs/user/README.md"]);
    }

    #[test]
    fn iter_units_skips_mirrors_and_matches_index_sections() {
        let dir = artifact();
        let mut pages = load_pages(dir.path(), &priorities()).unwrap();
        mark_mirrors(&mut pages);
        let units = iter_units(&pages);
        assert!(
            units.iter().all(|u| u.page_id != "guides::docs/storage.md"),
            "the mirror page contributes no units"
        );
        let readme_units: Vec<&Unit> = units
            .iter()
            .filter(|u| u.page_id == "handbook::docs/user/README.md")
            .collect();
        // One intro unit and one "Upload caching" H2 unit, same split as the index.
        assert_eq!(readme_units.len(), 2);
        assert_eq!(readme_units[1].heading, "Upload caching");
        assert!(readme_units[1].text.contains("Storage Module"));
        assert!(readme_units[1].text.contains("Enable upload caching"));
        assert_eq!(readme_units[1].title, "Storage Module");
        // Ids are `<page id>#<ordinal>`, and the ordinal restarts on every page.
        let ids: Vec<&str> = readme_units.iter().map(|u| u.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "handbook::docs/user/README.md#0",
                "handbook::docs/user/README.md#1"
            ]
        );
        for page in units
            .iter()
            .map(|u| u.page_id.as_str())
            .collect::<std::collections::BTreeSet<_>>()
        {
            let first = units.iter().find(|u| u.page_id == page).unwrap();
            assert_eq!(first.id, chunk_id(page, 0), "{page}");
        }
        assert!(readme_units[1].body.starts_with("\nEnable upload caching"));
    }

    fn hand_page(id: &str, content: &str) -> Page {
        Page {
            id: id.to_string(),
            source: "s".to_string(),
            path: "p.md".to_string(),
            repo: "s".to_string(),
            module: "s".to_string(),
            title: "T".to_string(),
            heading: String::new(),
            doc_type: String::new(),
            section: String::new(),
            priority: 1,
            content: content.to_string(),
            mirror_of: None,
        }
    }

    /// The ordinal counts the units that exist: a blank intro that is dropped leaves the first
    /// H2 unit at 0, an H2 split at H3 numbers its parts consecutively, and both restart on the
    /// next page.
    #[test]
    fn unit_ids_count_the_units_after_a_dropped_intro_and_an_h3_split() {
        let big = format!(
            "intro\n\n## Big\n\nlead\n\n### P\n\n{p}\n\n### Q\n\n{p}\n",
            p = "alpha ".repeat(700)
        );
        let pages = [
            hand_page("s::blank-intro.md", "\n\n## A\n\nx\n\n## B\n\ny\n"),
            hand_page("s::big.md", &big),
            hand_page("s::plain.md", "just an intro\n"),
        ];
        let got: Vec<(String, usize, String)> = iter_units(&pages)
            .into_iter()
            .map(|u| (u.id, u.ordinal, u.heading))
            .collect();
        let want = [
            ("s::blank-intro.md#0", 0, "A"),
            ("s::blank-intro.md#1", 1, "B"),
            ("s::big.md#0", 0, ""),
            ("s::big.md#1", 1, "Big"),
            ("s::big.md#2", 2, "Big / P"),
            ("s::big.md#3", 3, "Big / Q"),
            ("s::plain.md#0", 0, ""),
        ]
        .map(|(id, ordinal, heading)| (id.to_string(), ordinal, heading.to_string()));
        assert_eq!(got, want);
    }

    /// The index holds exactly the units `iter_units` cuts: same count, same page per unit,
    /// on the golden fixture with its configured priorities (three mirrors collapsed).
    #[test]
    fn index_units_are_iter_units_on_the_golden_fixture() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden/artifact");
        let priorities = Priorities {
            explicit: [
                ("handbook", 10),
                ("guides", 5),
                ("cookbook", 1),
                ("schemas", 1),
            ]
            .into_iter()
            .map(|(name, priority)| (name.to_string(), priority))
            .collect(),
        };
        let index = Index::build(&fixture, &priorities).unwrap();
        let units = iter_units(index.pages());
        assert_eq!(index.unit_len.len(), units.len());
        assert_eq!(index.unit_page.len(), units.len());
        assert_eq!(units.len(), 52, "pinned with tests/chunks_cli.rs");
        for (dense, unit) in units.iter().enumerate() {
            let page = &index.pages()[index.searchable[index.unit_page[dense]]];
            assert_eq!(page.id, unit.page_id, "unit {dense}");
        }
        let distinct: std::collections::BTreeSet<&str> =
            units.iter().map(|u| u.id.as_str()).collect();
        assert_eq!(distinct.len(), units.len(), "every unit id is unique");
    }
}
