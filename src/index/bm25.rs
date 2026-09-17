//! The in-memory BM25 index over the searchable pages of an artifact, and its scoring.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use tantivy::postings::Postings;
use tantivy::schema::{
    FAST, Field, IndexRecordOption, STORED, Schema, TantivyDocument, TextFieldIndexing,
    TextOptions, Value as _,
};
use tantivy::{DocAddress, DocSet, IndexWriter, Searcher, TERMINATED, Term};

use super::IndexError;
use super::sections::{index_text, split_sections};
use super::tokenizer::{PinakesTokenizer, TOKENIZER_NAME, title_key, tokenize};
use crate::corpus::{Page, Priorities, load_pages, mark_mirrors};

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

/// One retrieval unit exposed for embedding (SPEC §16.2): the same text `Index` scores, so a
/// dense backend built from these embeds exactly what BM25 searches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// The page this unit belongs to.
    pub page_id: String,
    /// The unit's heading, empty for the intro (see [`Section::heading`](super::Section::heading)).
    pub heading: String,
    /// Title, heading and body joined into one text worth embedding.
    pub text: String,
}

/// The retrieval units of the searchable (non-mirror) pages, in page then section order.
///
/// `pages` must already have [`mark_mirrors`] applied; mirrors are skipped, matching what
/// [`Index::from_pages`] indexes.
pub fn iter_units(pages: &[Page]) -> Vec<Unit> {
    let mut units = Vec::new();
    for page in pages.iter().filter(|p| p.mirror_of.is_none()) {
        for section in split_sections(&index_text(&page.content)) {
            let text = if section.heading.is_empty() {
                format!("{}\n\n{}", page.title, section.body)
            } else {
                format!("{}\n{}\n\n{}", page.title, section.heading, section.body)
            };
            units.push(Unit {
                page_id: page.id.clone(),
                heading: section.heading,
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

#[allow(clippy::cast_precision_loss)]
fn float(n: usize) -> f64 {
    n as f64
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
    /// Load every page of `artifact` and index the searchable ones.
    pub fn build(artifact: &Path, priorities: &Priorities) -> Result<Index, IndexError> {
        Index::from_pages(load_pages(artifact, priorities)?)
    }

    /// Apply the mirror rule to `pages` and index the searchable ones.
    pub fn from_pages(mut pages: Vec<Page>) -> Result<Index, IndexError> {
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
        for (position, &page_index) in searchable.iter().enumerate() {
            let page = &pages[page_index];
            let title_tokens = tokenize(&page.title);
            for section in split_sections(&index_text(&page.content)) {
                let heading_tokens = tokenize(&section.heading);
                let body_tokens = tokenize(&section.body);
                let len = stats.add_unit(&title_tokens, &heading_tokens, &body_tokens);
                let mut doc = TantivyDocument::default();
                doc.add_text(fields.title, &page.title);
                doc.add_text(fields.heading, &section.heading);
                doc.add_text(fields.body, &section.body);
                doc.add_text(fields.page_id, &page.id);
                doc.add_text(fields.source, &page.source);
                doc.add_text(fields.doc_type, &page.doc_type);
                doc.add_u64(fields.page, position as u64);
                doc.add_u64(fields.len, len as u64);
                writer.add_document(doc)?;
            }
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
    use super::super::testing::{SourceSpec, write_artifact};
    use super::*;
    use crate::corpus::{SourceMeta, load_residue_page, make_page};
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
            IndexError::MissingResidue { .. }
        ));
        assert!(matches!(
            load_residue_page(dir.path(), "no-separator", &priorities).unwrap_err(),
            IndexError::BadPageId(_)
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
        assert!(matches!(err, IndexError::NotADirectory(_)));
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
    }
}
