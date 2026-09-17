//! `bm25-tantivy` (SPEC §16.1): the same units as `bm25`, scored by tantivy's own BM25
//! (`k1` 1.2, `b` 0.75).

use std::collections::HashMap;
use std::path::Path;

use tantivy::query::{BooleanQuery, BoostQuery, Occur, Query, TermQuery};
use tantivy::schema::{
    IndexRecordOption, STORED, Schema, TantivyDocument, TextFieldIndexing, TextOptions, Value as _,
};
use tantivy::{IndexWriter, Searcher, Term};

use super::{Backend, BackendConfig, BackendError};
use crate::index::{
    HEADING_BOOST, Hit, Page, PinakesTokenizer, TITLE_BOOST, TOKENIZER_NAME, index_text,
    load_pages, mark_mirrors, split_sections, title_key, tokenize,
};

#[derive(Debug, Clone, Copy)]
struct TantivyFields {
    title: tantivy::schema::Field,
    heading: tantivy::schema::Field,
    body: tantivy::schema::Field,
    page_id: tantivy::schema::Field,
}

impl TantivyFields {
    fn schema() -> (Schema, TantivyFields) {
        let indexing = TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER_NAME)
            .set_index_option(IndexRecordOption::WithFreqs);
        let indexed = TextOptions::default().set_indexing_options(indexing);
        let mut builder = Schema::builder();
        let fields = TantivyFields {
            title: builder.add_text_field("title", indexed.clone().set_stored()),
            heading: builder.add_text_field("heading", indexed.clone().set_stored()),
            body: builder.add_text_field("body", indexed),
            page_id: builder.add_text_field("page_id", STORED),
        };
        (builder.build(), fields)
    }
}

/// tantivy's own BM25 scorer (`k1 = 1.2`, `b = 0.75`, Lucene-style IDF and field-length norms)
/// over the same retrieval units as [`Index`] (SPEC §16.1): title, heading and body are indexed
/// as separate fields and combined with the same boosts, so the ranking differs from `bm25`
/// only in how tantivy itself scores a field match, not in what is indexed.
///
/// [`Index`]: crate::index::Index
pub struct TantivyBackend {
    pages: Vec<Page>,
    searchable_count: usize,
    page_by_id: HashMap<String, usize>,
    fields: TantivyFields,
    searcher: Searcher,
    total_units: usize,
}

impl Backend for TantivyBackend {
    fn build(artifact: &Path, config: &BackendConfig) -> Result<TantivyBackend, BackendError> {
        let mut pages = load_pages(artifact, &config.priorities)?;
        mark_mirrors(&mut pages);
        let (schema, fields) = TantivyFields::schema();
        let index = tantivy::Index::create_in_ram(schema);
        index
            .tokenizers()
            .register(TOKENIZER_NAME, PinakesTokenizer);
        let mut writer: IndexWriter<TantivyDocument> =
            index.writer_with_num_threads(1, 32 << 20)?;
        let mut total_units = 0usize;
        let mut searchable_count = 0usize;
        let mut page_by_id = HashMap::new();
        for (position, page) in pages.iter().enumerate() {
            if page.mirror_of.is_some() {
                continue;
            }
            searchable_count += 1;
            page_by_id.insert(page.id.clone(), position);
            for section in split_sections(&index_text(&page.content)) {
                let mut doc = TantivyDocument::default();
                doc.add_text(fields.title, &page.title);
                doc.add_text(fields.heading, &section.heading);
                doc.add_text(fields.body, &section.body);
                doc.add_text(fields.page_id, &page.id);
                writer.add_document(doc)?;
                total_units += 1;
            }
        }
        writer.commit()?;
        let searcher = index.reader()?.searcher();
        Ok(TantivyBackend {
            pages,
            searchable_count,
            page_by_id,
            fields,
            searcher,
            total_units,
        })
    }

    fn search(
        &self,
        query: &str,
        k: usize,
        module: Option<&str>,
    ) -> Result<Vec<Hit>, BackendError> {
        if self.total_units == 0 || k == 0 {
            return Ok(Vec::new());
        }
        let tokens = tokenize(query);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for token in &tokens {
            for (field, boost) in [
                (
                    self.fields.title,
                    f32::from(u8::try_from(TITLE_BOOST).unwrap_or(3)),
                ),
                (
                    self.fields.heading,
                    f32::from(u8::try_from(HEADING_BOOST).unwrap_or(2)),
                ),
                (self.fields.body, 1.0),
            ] {
                let term = Term::from_field_text(field, token);
                let term_query = TermQuery::new(term, IndexRecordOption::WithFreqs);
                let query: Box<dyn Query> = if (boost - 1.0).abs() > f32::EPSILON {
                    Box::new(BoostQuery::new(Box::new(term_query), boost))
                } else {
                    Box::new(term_query)
                };
                clauses.push((Occur::Should, query));
            }
        }
        let boolean = BooleanQuery::new(clauses);
        let top = tantivy::collector::TopDocs::with_limit(self.total_units).order_by_score();
        let hits = self.searcher.search(&boolean, &top)?;

        let mut best: HashMap<String, (f32, String)> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for (score, address) in hits {
            let doc: TantivyDocument = self.searcher.doc(address)?;
            let page_id = doc
                .get_first(self.fields.page_id)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let heading = doc
                .get_first(self.fields.heading)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if !best.contains_key(&page_id) {
                order.push(page_id.clone());
            }
            let entry = best.entry(page_id).or_insert((score, heading.clone()));
            if score > entry.0 {
                *entry = (score, heading);
            }
        }

        let module_wanted = module.filter(|m| !m.is_empty()).map(str::to_lowercase);
        let module_of = |id: &str| -> String {
            self.page_by_id
                .get(id)
                .map(|&i| self.pages[i].module.to_lowercase())
                .unwrap_or_default()
        };
        let mut ranked = order;
        if let Some(wanted) = &module_wanted {
            let filtered: Vec<String> = ranked
                .iter()
                .filter(|id| &module_of(id) == wanted)
                .cloned()
                .collect();
            if !filtered.is_empty() {
                ranked = filtered;
            }
        }

        let mut seen_titles = std::collections::HashSet::new();
        let mut out = Vec::new();
        for id in ranked {
            let Some(&position) = self.page_by_id.get(&id) else {
                continue;
            };
            let key = title_key(&self.pages[position].title);
            if !key.is_empty() && !seen_titles.insert(key) {
                continue;
            }
            let (score, heading) = best.get(&id).cloned().unwrap_or_default();
            out.push(Hit {
                page_id: id,
                score: f64::from(score),
                heading,
            });
            if out.len() == k {
                break;
            }
        }
        Ok(out)
    }

    fn page_count(&self) -> usize {
        self.pages.len()
    }

    fn searchable_count(&self) -> usize {
        self.searchable_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::testing::fixture_pages;

    #[test]
    fn tantivy_backend_finds_the_boosted_title_match_first() {
        let (dir, _pages) = fixture_pages();
        let config = BackendConfig::default();
        let backend = TantivyBackend::build(dir.path(), &config).unwrap();
        assert_eq!(backend.page_count(), 2);
        assert_eq!(backend.searchable_count(), 2);
        let hits = backend.search("upload caching", 10, None).unwrap();
        assert_eq!(hits[0].page_id, "handbook::docs/user/README.md");
        assert_eq!(hits[0].heading, "Upload caching");
        assert!(hits[0].score > 0.0);
        assert!(
            backend
                .search("nothing at all matches", 10, None)
                .unwrap()
                .is_empty()
        );
        let hits = backend
            .search("billing invoices", 10, Some("handbook"))
            .unwrap();
        assert_eq!(hits[0].page_id, "handbook::docs/user/billing.md");
    }
}
