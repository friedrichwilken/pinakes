//! The library surface `kanon` depends on (SPEC §20, issue #56), pinned at compile time.
//!
//! Every item below is one `kanon` imports (checked against its source, not only against the
//! list in #56), except the `trail` items, which SPEC §20 offers to a serving consumer and which
//! `kanon` does not use today. Nothing here checks behaviour; the unit tests do. What this file
//! catches is a rename, a removal, a visibility change or a changed signature, which SPEC §20
//! allows only with a new major version, before `kanon`'s next `pinakes` bump finds it.
//!
//! Items are pinned in three ways:
//! - a function or method `kanon` names is bound to a function pointer of its exact signature;
//! - a type `kanon` reads is read through a closure that is never called, so no value has to be
//!   built and a field can be added freely: each field is taken at its exact type;
//! - a struct `kanon` builds by literal (`Page`, `LlmConfig`, `PageEntry`, `ManifestSource`) is
//!   built by literal here too, and an enum `kanon` matches exhaustively (`JsonlError`) is
//!   matched exhaustively here too.
//!
//! So an added function, method, item, or variant of any other enum does not fail this file, but
//! an added field on those four structs or an added `JsonlError` variant does, because it breaks
//! `kanon` too. Changing this file is changing the contract: say why in the commit body.

use std::collections::BTreeMap;
use std::path::Path;

use pinakes::chunks::{self, Chunk};
use pinakes::corpus::CorpusError;
use pinakes::index::{
    DEFAULT_PRIORITY, HEADING_BOOST, Hit, Index, IndexError, Page, PinakesTokenizer, Priorities,
    Section, TITLE_BOOST, TOKENIZER_NAME, Unit, index_text, iter_units, load_pages,
    load_residue_page, mark_mirrors, split_sections, title_key, tokenize,
};
use pinakes::jsonl::{self, JsonlError, KeyOrder, LineError};
use pinakes::layout::MANIFEST_FILE;
use pinakes::llm::testing::{Scripted, ScriptedTransport, completion};
use pinakes::llm::{ChatError, ChatTransport, LlmConfig, TransportError, UreqChatTransport, chat};
use pinakes::manifest::{
    Manifest, ManifestError, ManifestSource, PageEntry, SelectedBy, now_rfc3339, page_id,
    split_page_id,
};
use pinakes::residue::excerpt;
use pinakes::text::sha256_hex;
use pinakes::trail::{Outcome, TRAIL_VERSION, TrailEntry, TrailError, read_jsonl};

/// Reads each named field of `$t` at its exact type, through a closure that is never called:
/// no value has to be built, and a field added to the struct does not fail it.
macro_rules! fields {
    ($v:ident: $t:ty => $($f:ident: $ft:ty),+ $(,)?) => {
        let _ = |$v: &$t| {
            $(let _: &$ft = &$v.$f;)+
        };
    };
}

/// `Index::search`'s signature: the query, the result count and the optional module filter.
type Search = fn(&Index, &str, usize, Option<&str>) -> Result<Vec<Hit>, IndexError>;

/// Every error type a consumer wraps with `#[from]` or `#[error(transparent)]` is a real
/// `std::error::Error`.
fn is_error<E: std::error::Error + 'static>() {}

#[test]
fn index() {
    let _: fn(&Path, &Priorities) -> Result<Index, IndexError> = Index::build;
    let _: fn(Vec<Page>) -> Result<Index, IndexError> = Index::from_pages;
    let _: Search = Index::search;
    let _: for<'a> fn(&'a Index, &str) -> Option<&'a Page> = Index::page;
    let _: fn(&Index) -> &[Page] = Index::pages;
    let _: fn(&Index) -> usize = Index::page_count;
    let _: fn(&Index) -> usize = Index::searchable_count;
    let _: fn(CorpusError) -> IndexError = IndexError::from;
    is_error::<IndexError>();

    fields!(hit: Hit => page_id: String, score: f64, heading: String);
}

#[test]
fn corpus() {
    let _: fn(&Path, &Priorities) -> Result<Vec<Page>, CorpusError> = load_pages;
    let _: fn(&Path, &str, &Priorities) -> Result<Page, CorpusError> = load_residue_page;
    let _: fn(&mut [Page]) = mark_mirrors;
    let _: i64 = DEFAULT_PRIORITY;
    is_error::<CorpusError>();

    // `kanon` builds a `Page` by literal.
    let _ = Page {
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
    fields!(page: Page => id: String, source: String, path: String, repo: String,
        module: String, title: String, heading: String, doc_type: String, section: String,
        priority: i64, content: String, mirror_of: Option<String>);

    // It fills `Priorities` from `Default` and the public map, and compares and clones it.
    let _: fn(&Priorities, &str) -> i64 = Priorities::of;
    let _ = |priorities: &mut Priorities| -> Option<i64> {
        let _: &BTreeMap<String, i64> = &priorities.explicit;
        priorities.explicit.insert(String::new(), 0)
    };
    let priorities = Priorities::default();
    assert_eq!(priorities.clone(), priorities);
}

#[test]
fn units_and_tokens() {
    let _: fn(&[Page]) -> Vec<Unit> = iter_units;
    let _: fn(&str) -> Vec<Section> = split_sections;
    let _: fn(&str) -> String = index_text;
    let _: fn(&str) -> Vec<String> = tokenize;
    let _: fn(&str) -> String = title_key;
    let _: &str = TOKENIZER_NAME;
    let _: (u32, u32) = (TITLE_BOOST, HEADING_BOOST);
    let _ = PinakesTokenizer;

    fields!(unit: Unit => id: String, ordinal: usize, page_id: String, title: String,
        heading: String, body: String, text: String);
    fields!(section: Section => heading: String, body: String);
}

#[test]
fn chunks() {
    let _: fn(&[Page]) -> Vec<Chunk> = chunks::chunks;
    let _: fn(&str, usize) -> String = chunks::chunk_id;
    let _: fn(&str, usize) -> String = pinakes::index::chunk_id;

    fields!(chunk: Chunk => id: String, page: String, heading: String, ordinal: usize,
        text: String, sha256: String);
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
    is_error::<ManifestError>();

    // Only the first field of the tuple `Manifest::pages` yields is read by `kanon`.
    let manifest = Manifest::new(now_rfc3339());
    let _: usize = manifest.pages().count();
    fields!(manifest: Manifest => sources: BTreeMap<String, ManifestSource>, artifact_version: u32);
    let _ = |error: &ManifestError| {
        let _ = matches!(
            error,
            ManifestError::ArtifactVersion { .. } | ManifestError::Json { .. }
        );
        if let ManifestError::Io { source, .. } = error {
            let _: &std::io::Error = source;
        }
    };

    // `kanon` builds a `ManifestSource` and a `PageEntry` by literal.
    let _ = ManifestSource {
        repo: String::new(),
        repo_url: String::new(),
        git_ref: String::new(),
        commit: String::new(),
        archived: None,
        resolver: String::new(),
        pages: BTreeMap::new(),
        residue: Vec::new(),
        unresolved: Vec::new(),
        unrendered: Vec::new(),
        render: None,
    };
    fields!(source: ManifestSource => repo: String, repo_url: String, git_ref: String,
        commit: String, archived: Option<bool>, resolver: String,
        pages: BTreeMap<String, PageEntry>, residue: Vec<String>, unresolved: Vec<String>,
        unrendered: Vec<String>);
    let _ = PageEntry {
        sha256: String::new(),
        title: String::new(),
        doc_type: String::new(),
        section: String::new(),
        selected_by: SelectedBy::Resolver,
        rendered_from: None,
    };
    fields!(entry: PageEntry => sha256: String, title: String, doc_type: String,
        section: String, selected_by: SelectedBy);
    for by in [
        SelectedBy::Resolver,
        SelectedBy::Include,
        SelectedBy::Decision,
    ] {
        let _: SelectedBy = by;
    }
}

#[test]
fn jsonl() {
    // The item type is kanon-neutral: only the shape of the functions is pinned.
    type Item = serde_json::Value;
    let _: fn(&Path) -> Result<Vec<Item>, JsonlError> = jsonl::read::<Item>;
    let _: fn(&Path, &[Item], KeyOrder) -> Result<(), JsonlError> = jsonl::write::<Item>;
    let _: fn(&Path, &[Item], KeyOrder) -> Result<(), JsonlError> = jsonl::append::<Item>;
    let _: fn(&[Item], KeyOrder) -> Result<String, serde_json::Error> = jsonl::to_string::<Item>;
    let _: fn(&str) -> Result<Vec<Item>, LineError> = jsonl::parse::<Item>;
    let _ = jsonl::parse_lines::<Item>("").count();
    for keys in [KeyOrder::Sorted, KeyOrder::Declared] {
        let _: KeyOrder = keys;
    }
    is_error::<JsonlError>();
    is_error::<LineError>();

    // `kanon` matches `JsonlError` exhaustively and reads `LineError::source`.
    let _ = |error: &JsonlError| match error {
        JsonlError::Io { path, source } => {
            let _: (&std::path::PathBuf, &std::io::Error) = (path, source);
        }
        JsonlError::Json { path, line, source } => {
            let _: (&std::path::PathBuf, &usize, &serde_json::Error) = (path, line, source);
        }
    };
    fields!(error: LineError => line: usize, source: serde_json::Error);
}

#[test]
fn llm() {
    // `kanon` passes `&dyn ChatTransport`, and coerces `UreqChatTransport` to it.
    let _: &dyn ChatTransport = &UreqChatTransport::new();
    let scripted = ScriptedTransport::new(vec![
        Scripted::Ok(completion("[\"a\"]")),
        Scripted::Err(TransportError::Status(500, String::new())),
    ]);
    let transport: &dyn ChatTransport = &scripted;
    let config = LlmConfig {
        url: "http://localhost".to_string(),
        key: None,
        model: "m".to_string(),
    };
    fields!(llm: LlmConfig => url: String, key: Option<String>, model: String);
    let got: Result<Vec<String>, ChatError> = chat(transport, &config, "system", "user");
    assert_eq!(got.expect("the scripted reply"), ["a"]);
    let _: &std::sync::Mutex<Vec<serde_json::Value>> = &scripted.requests;
    is_error::<ChatError>();

    let _ = |error: &ChatError| {
        let _ = matches!(error, ChatError::Json { .. });
        if let ChatError::Http { status, .. } = error {
            let _: &u16 = status;
        }
    };
    let _ = |error: &TransportError| match error {
        TransportError::Status(status, message) => {
            let _: (&u16, &String) = (status, message);
        }
        TransportError::Transport(message) => {
            let _: &String = message;
        }
    };
}

#[test]
fn trail_text_and_layout() {
    // `kanon` does not use the trail items today; SPEC §20 offers them to a serving consumer.
    let _: fn(&Path) -> Result<Vec<TrailEntry>, TrailError> = read_jsonl;
    is_error::<TrailError>();
    let _: u32 = TRAIL_VERSION;
    fields!(entry: TrailEntry => version: u32, at: String, query: String, retrieved: Vec<String>,
        ranks: Vec<u32>, cited: Vec<String>, outcome: Outcome, session: String);
    for outcome in [Outcome::Ok, Outcome::Bad, Outcome::Unknown] {
        let _: Outcome = outcome;
    }

    let _: fn(&[u8]) -> String = sha256_hex;
    let _: fn(&str, usize) -> String = excerpt;
    let _: &str = MANIFEST_FILE;
}
