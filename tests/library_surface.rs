//! The library surface `kanon` depends on (SPEC §20, issue #56), pinned at compile time.
//!
//! Every item below is one `kanon` imports. Function and method items are bound to a function
//! pointer of their exact signature, struct fields are read at their exact type, and the two
//! structs a consumer builds itself (`Priorities`, `LlmConfig`) are built by literal. Nothing
//! here checks behaviour; each unit test pins that. What it catches is a rename, a removal or a
//! changed signature, which SPEC §20 allows only with a new major version, before `kanon`'s
//! next `pinakes` bump finds it. Adding an item, a field or a variant does not fail it.
//! Changing this file is changing the contract: say why in the commit body.

use std::collections::BTreeMap;
use std::path::Path;

use pinakes::corpus::CorpusError;
use pinakes::index::{
    DEFAULT_PRIORITY, HEADING_BOOST, Hit, Index, IndexError, Page, PinakesTokenizer, Priorities,
    Section, TITLE_BOOST, TOKENIZER_NAME, Unit, index_text, iter_units, load_pages,
    load_residue_page, mark_mirrors, split_sections, title_key, tokenize,
};
use pinakes::jsonl::{self, JsonlError, KeyOrder};
use pinakes::layout::MANIFEST_FILE;
use pinakes::llm::testing::{Scripted, ScriptedTransport, completion};
use pinakes::llm::{ChatError, ChatTransport, LlmConfig, TransportError, UreqChatTransport, chat};
use pinakes::manifest::{
    Manifest, ManifestError, ManifestSource, PageEntry, SelectedBy, now_rfc3339, page_id,
    split_page_id,
};
use pinakes::residue::excerpt;
use pinakes::text::sha256_hex;
use pinakes::trail::{Outcome, TrailEntry, TrailError, read_jsonl};

/// `Index::search`'s signature: the query, the result count and the optional module filter.
type Search = fn(&Index, &str, usize, Option<&str>) -> Result<Vec<Hit>, IndexError>;

/// A reader item that only exists to be named: a consumer implements the trait itself.
struct Offline;

impl ChatTransport for Offline {
    fn post(
        &self,
        _url: &str,
        _key: Option<&str>,
        _body: &serde_json::Value,
    ) -> Result<serde_json::Value, TransportError> {
        Err(TransportError::Transport(String::new()))
    }
}

#[test]
fn index_and_corpus() {
    let _: fn(&Path, &Priorities) -> Result<Index, IndexError> = Index::build;
    let _: fn(Vec<Page>) -> Result<Index, IndexError> = Index::from_pages;
    let _: Search = Index::search;
    let _: for<'a> fn(&'a Index, &str) -> Option<&'a Page> = Index::page;
    let _: fn(&Index) -> &[Page] = Index::pages;
    let _: fn(&Index) -> usize = Index::page_count;
    let _: fn(&Index) -> usize = Index::searchable_count;

    let _: fn(&Path, &Priorities) -> Result<Vec<Page>, CorpusError> = load_pages;
    let _: fn(&Path, &str, &Priorities) -> Result<Page, CorpusError> = load_residue_page;
    let _: fn(&mut [Page]) = mark_mirrors;
    let _: i64 = DEFAULT_PRIORITY;

    // `kanon` fills the priorities itself from `pinakes.yaml`.
    let priorities = Priorities {
        explicit: BTreeMap::from([("handbook".to_string(), 10_i64)]),
    };
    let _: &BTreeMap<String, i64> = &priorities.explicit;

    let hit = Hit {
        page_id: String::new(),
        score: 0.0,
        heading: String::new(),
    };
    let _: (&String, &f64, &String) = (&hit.page_id, &hit.score, &hit.heading);

    let page = Page {
        id: String::new(),
        source: String::new(),
        path: String::new(),
        repo: String::new(),
        module: String::new(),
        title: String::new(),
        heading: String::new(),
        doc_type: String::new(),
        section: String::new(),
        priority: 0,
        content: String::new(),
        mirror_of: None,
    };
    let _: (&String, &String, &String, &String, &String, &String) = (
        &page.id,
        &page.source,
        &page.path,
        &page.repo,
        &page.module,
        &page.title,
    );
    let _: (&String, &String, &String, &i64, &String, &Option<String>) = (
        &page.heading,
        &page.doc_type,
        &page.section,
        &page.priority,
        &page.content,
        &page.mirror_of,
    );
}

#[test]
fn units_sections_and_tokens() {
    let _: fn(&[Page]) -> Vec<Unit> = iter_units;
    let _: fn(&str) -> Vec<Section> = split_sections;
    let _: fn(&str) -> String = index_text;
    let _: fn(&str) -> Vec<String> = tokenize;
    let _: fn(&str) -> String = title_key;
    let _: &str = TOKENIZER_NAME;
    let _: (u32, u32) = (TITLE_BOOST, HEADING_BOOST);
    let _ = PinakesTokenizer;

    let units = iter_units(&[]);
    assert!(units.is_empty());
    let unit = Unit {
        id: String::new(),
        ordinal: 0,
        page_id: String::new(),
        title: String::new(),
        heading: String::new(),
        body: String::new(),
        text: String::new(),
    };
    let _: (&String, &usize, &String, &String, &String, &String, &String) = (
        &unit.id,
        &unit.ordinal,
        &unit.page_id,
        &unit.title,
        &unit.heading,
        &unit.body,
        &unit.text,
    );
    let section = split_sections("").into_iter().next().unwrap_or(Section {
        heading: String::new(),
        body: String::new(),
    });
    let _: (&String, &String) = (&section.heading, &section.body);
}

#[test]
fn manifest() {
    let _: fn(String) -> Manifest = Manifest::new;
    let _: fn(&Path) -> Result<Manifest, ManifestError> = Manifest::load;
    let _: fn(&Manifest, &Path) -> Result<(), ManifestError> = Manifest::save;
    let _: for<'a> fn(&'a Manifest, &str) -> Option<&'a PageEntry> = Manifest::page;
    let _: fn(&str, &str) -> String = page_id;
    let _: for<'a> fn(&'a str) -> Option<(&'a str, &'a str)> = split_page_id;
    let _: fn() -> String = now_rfc3339;

    let manifest = Manifest::new(now_rfc3339());
    let _: &BTreeMap<String, ManifestSource> = &manifest.sources;
    let _: usize = manifest.pages().count();
    for source in manifest.sources.values() {
        let _: &BTreeMap<String, PageEntry> = &source.pages;
        let _: &Vec<String> = &source.residue;
    }
    let entry = PageEntry {
        sha256: String::new(),
        title: String::new(),
        doc_type: String::new(),
        section: String::new(),
        selected_by: SelectedBy::Resolver,
        rendered_from: None,
    };
    let _: (&String, &String, &String, &String, &SelectedBy) = (
        &entry.sha256,
        &entry.title,
        &entry.doc_type,
        &entry.section,
        &entry.selected_by,
    );
    for by in [
        SelectedBy::Resolver,
        SelectedBy::Include,
        SelectedBy::Decision,
    ] {
        let _: SelectedBy = by;
    }
}

#[test]
fn jsonl_files() {
    let _: fn(&Path) -> Result<Vec<TrailEntry>, JsonlError> = jsonl::read::<TrailEntry>;
    let _: fn(&Path, &[TrailEntry], KeyOrder) -> Result<(), JsonlError> =
        jsonl::append::<TrailEntry>;
    let _: fn(&[TrailEntry], KeyOrder) -> Result<String, serde_json::Error> =
        jsonl::to_string::<TrailEntry>;
    // `parse`'s error type is not part of the surface, so the results are only unwrapped.
    assert!(
        jsonl::parse::<TrailEntry>("")
            .expect("empty text has no lines")
            .is_empty()
    );
    assert_eq!(jsonl::parse_lines::<TrailEntry>("").count(), 0);
    for keys in [KeyOrder::Sorted, KeyOrder::Declared] {
        let _: KeyOrder = keys;
    }
}

#[test]
fn chat_client() {
    let _: fn(&Path) -> Result<Vec<TrailEntry>, TrailError> = read_jsonl;

    // The transport is implemented by the consumer; the config is built by literal.
    let config = LlmConfig {
        url: "http://localhost".to_string(),
        key: None,
        model: "m".to_string(),
    };
    let _: (&String, &Option<String>, &String) = (&config.url, &config.key, &config.model);
    let err: Result<Vec<String>, ChatError> = chat(&Offline, &config, "system", "user");
    assert!(err.is_err());
    let _ = UreqChatTransport::new();

    let scripted = ScriptedTransport::new(vec![
        Scripted::Ok(completion("[\"a\"]")),
        Scripted::Err(TransportError::Status(500, String::new())),
    ]);
    let got: Vec<String> = chat(&scripted, &config, "system", "user").expect("scripted reply");
    assert_eq!(got, ["a"]);
    let _: &std::sync::Mutex<Vec<serde_json::Value>> = &scripted.requests;
}

#[test]
fn trail_text_and_layout() {
    let entry = TrailEntry {
        at: String::new(),
        query: String::new(),
        retrieved: Vec::new(),
        ranks: Vec::new(),
        cited: Vec::new(),
        outcome: Outcome::Unknown,
        session: String::new(),
    };
    let _: (&String, &String, &Vec<String>, &Vec<u32>) =
        (&entry.at, &entry.query, &entry.retrieved, &entry.ranks);
    let _: (&Vec<String>, &Outcome, &String) = (&entry.cited, &entry.outcome, &entry.session);
    for outcome in [Outcome::Ok, Outcome::Bad, Outcome::Unknown] {
        let _: Outcome = outcome;
    }

    let _: fn(&[u8]) -> String = sha256_hex;
    let _: fn(&str, usize) -> String = excerpt;
    let _: &str = MANIFEST_FILE;
}
