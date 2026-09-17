//! Built-in BM25 measurement backend (SPEC §5): tokeniser, section splitting and the tantivy
//! index over an artifact directory.
//!
//! The rules are this tool's retrieval model, pinned by the golden corpus (SPEC §7.3):
//!
//! - every `<source>/…/*.md` under the artifact is a page (directories starting with `_` and
//!   `manifest.json` are skipped; a `manifest.json` is not required, `<source>/meta.json` is
//!   read for the repo slug, module name and navigation titles);
//! - the page is split into retrieval units: the intro before the first H2 and one unit per H2
//!   section, H2 sections over [`SECTION_SPLIT_TOKENS`] tokens split again at H3;
//! - a unit is scored over three fields, `title` (boost [`TITLE_BOOST`]), `heading` (boost
//!   [`HEADING_BOOST`]) and `body`; the page score is its best unit; results are de-duplicated
//!   by tokenised title;
//! - the mirror rule: when two sources carry a page with the same title key (navigation title
//!   or H1), only the page from the source with the higher `priority` is indexed; priorities
//!   come from `pinakes.yaml` alone and equal priorities never collapse anything.
//!
//! Scoring is Okapi BM25 with `k1 = 1.5`, `b = 0.75` and negative IDFs floored at a quarter of
//! the average IDF, computed from the tantivy postings and an exact per-unit length. Field
//! boosts multiply the term frequency and the unit length, as repeating the title and heading
//! tokens would. This is the common `rank_bm25` `BM25Okapi` formula, so results are comparable
//! with that library; tantivy's own scorer (`k1 = 1.2`, Lucene IDF, quantised lengths) would
//! rank differently and is not used.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use tantivy::postings::Postings;
use tantivy::schema::{
    FAST, Field, IndexRecordOption, STORED, Schema, TantivyDocument, TextFieldIndexing,
    TextOptions, Value as _,
};
use tantivy::tokenizer::{Token, TokenStream, Tokenizer};
use tantivy::{DocAddress, DocSet, IndexWriter, Searcher, TERMINATED, Term};
use thiserror::Error;

use crate::config::Config;
use crate::layout::{META_FILE, RESIDUE_DIR};
use crate::manifest::page_id;
use crate::text::{strip_frontmatter, title_of};

/// Stopwords dropped by [`tokenize`], sorted.
pub const STOPWORDS: [&str; 35] = [
    "a", "an", "and", "are", "as", "at", "be", "by", "can", "do", "does", "for", "from", "how",
    "i", "if", "in", "is", "it", "its", "my", "of", "on", "or", "that", "the", "this", "to", "was",
    "what", "when", "which", "with", "you", "your",
];
/// Weight of the page title in a unit.
pub const TITLE_BOOST: u32 = 3;
/// Weight of the unit heading in a unit.
pub const HEADING_BOOST: u32 = 2;
/// H2 sections with more tokens than this are split at H3.
pub const SECTION_SPLIT_TOKENS: usize = 1200;
/// Priority of a source that `pinakes.yaml` does not list (or of every source without a
/// config): the config default, so unconfigured sources never collapse each other.
pub const DEFAULT_PRIORITY: i64 = crate::config::DEFAULT_PRIORITY;
/// Name under which the tokenizer is registered with tantivy.
pub const TOKENIZER_NAME: &str = "pinakes";

/// BM25 term-frequency saturation.
const K1: f64 = 1.5;
/// BM25 length normalisation.
const B: f64 = 0.75;
/// Fraction of the average IDF used for terms in more than half of the units.
const EPSILON: f64 = 0.25;
/// Memory budget of the single-threaded index writer.
const WRITER_BUDGET: usize = 64 << 20;

static HTML_COMMENT: LazyLock<Regex> = LazyLock::new(|| regex(r"(?s)<!--.*?-->"));
static HTML_TAG: LazyLock<Regex> = LazyLock::new(|| regex(r"</?[a-zA-Z][^>]*>"));
static MD_IMAGE: LazyLock<Regex> = LazyLock::new(|| regex(r"!\[([^\]]*)\]\([^)]*\)"));
static MD_LINK: LazyLock<Regex> = LazyLock::new(|| regex(r"\[([^\]]*)\]\([^)]*\)"));
static H2: LazyLock<Regex> = LazyLock::new(|| regex(r"^##\s+(.+?)\s*#*\s*$"));
static H3: LazyLock<Regex> = LazyLock::new(|| regex(r"^###\s+(.+?)\s*#*\s*$"));

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static pattern is valid")
}

/// Errors raised while loading pages or building the index.
#[derive(Debug, Error)]
pub enum IndexError {
    /// A filesystem operation failed.
    #[error("{path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The artifact path is not a directory.
    #[error("{0}: not an artifact directory")]
    NotADirectory(PathBuf),
    /// tantivy failed.
    #[error("index: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    /// Reading postings failed.
    #[error("index: reading postings: {0}")]
    Postings(#[from] std::io::Error),
    /// A page id is not `<source>::<path>`.
    #[error("invalid page id {0:?}: expected <source>::<path>")]
    BadPageId(String),
    /// `--with` named a page that is not in `_residue`.
    #[error("{id}: no residue page at {path}")]
    MissingResidue {
        /// The page id.
        id: String,
        /// Where the page was expected.
        path: PathBuf,
    },
    /// `--with` named a page that is already in the corpus.
    #[error("{0}: already in the corpus")]
    AlreadyPresent(String),
    /// `--without` named a page that is not in the corpus.
    #[error("{0}: not in the corpus")]
    UnknownPage(String),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> IndexError + '_ {
    move |source| IndexError::Io {
        path: path.to_path_buf(),
        source,
    }
}

// ---------------------------------------------------------------------------------------------
// Tokeniser
// ---------------------------------------------------------------------------------------------

/// Whether `token` is in [`STOPWORDS`].
pub fn is_stopword(token: &str) -> bool {
    STOPWORDS.binary_search(&token).is_ok()
}

/// Accumulates one token while scanning text.
struct Scanner {
    token: String,
    start: usize,
}

impl Scanner {
    fn push(&mut self, ch: char, offset: usize) {
        if self.token.is_empty() {
            self.start = offset;
        }
        self.token.push(ch);
    }

    fn flush(&mut self, end: usize, sink: &mut dyn FnMut(usize, usize, &str)) {
        if !self.token.is_empty() {
            if !is_stopword(&self.token) {
                sink(self.start, end, &self.token);
            }
            self.token.clear();
        }
    }
}

/// Call `sink(start, end, token)` for every token of `text` with its byte offsets.
///
/// A token is a maximal run of ASCII letters and digits after lowercasing; every other
/// character separates tokens. Stopwords are dropped. For an identifier compound — alphanumeric
/// runs joined by `.` `/` `_` or `-` — the joined form with separators removed is also emitted
/// (SPEC §10.3), e.g. `spec.sink` → `spec`, `sink`, `specsink`.
fn scan_tokens(text: &str, sink: &mut dyn FnMut(usize, usize, &str)) {
    let mut scanner = Scanner {
        token: String::new(),
        start: 0,
    };
    for (offset, ch) in text.char_indices() {
        if ch.is_ascii() {
            if ch.is_ascii_alphanumeric() {
                scanner.push(ch.to_ascii_lowercase(), offset);
            } else {
                scanner.flush(offset, sink);
            }
        } else {
            // Lowercasing a non-ASCII letter can yield ASCII (`İ` → `i` + combining dot).
            for lower in ch.to_lowercase() {
                if lower.is_ascii_alphanumeric() {
                    scanner.push(lower, offset);
                } else {
                    scanner.flush(offset + ch.len_utf8(), sink);
                }
            }
        }
    }
    scanner.flush(text.len(), sink);
    scan_compounds(text, sink);
}

/// A separator that, between two alphanumeric runs, marks an identifier compound (SPEC §10.3).
fn is_compound_separator(ch: char) -> bool {
    matches!(ch, '.' | '/' | '_' | '-')
}

/// Emit the joined form of every identifier compound in `text`: a maximal run of ASCII
/// alphanumerics and `.` `/` `_` `-` that, once separators at either end are trimmed away,
/// still contains a separator between two alphanumeric parts.
fn scan_compounds(text: &str, sink: &mut dyn FnMut(usize, usize, &str)) {
    let is_run_char =
        |ch: char| ch.is_ascii() && (ch.is_ascii_alphanumeric() || is_compound_separator(ch));
    let mut run_start: Option<usize> = None;
    let mut run_end = 0;
    for (offset, ch) in text.char_indices() {
        if is_run_char(ch) {
            run_start.get_or_insert(offset);
            run_end = offset + ch.len_utf8();
        } else if let Some(start) = run_start.take() {
            emit_compound(&text[start..run_end], start, sink);
        }
    }
    if let Some(start) = run_start {
        emit_compound(&text[start..run_end], start, sink);
    }
}

/// Emit the joined token for one compound run, when trimming its leading and trailing
/// separators still leaves at least one separator between two alphanumeric parts.
fn emit_compound(run: &str, run_start: usize, sink: &mut dyn FnMut(usize, usize, &str)) {
    let core = run.trim_matches(is_compound_separator);
    if core.is_empty() || !core.contains(is_compound_separator) {
        return;
    }
    let joined: String = core
        .split(is_compound_separator)
        .filter(|part| !part.is_empty())
        .flat_map(str::chars)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if joined.is_empty() {
        return;
    }
    let Some(core_offset) = run.find(core) else {
        return;
    };
    let core_start = run_start + core_offset;
    sink(core_start, core_start + core.len(), &joined);
}

/// Tokenise for indexing and querying: lowercase, `[a-z0-9]+` runs, stopwords dropped.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    scan_tokens(text, &mut |_, _, token| tokens.push(token.to_string()));
    tokens
}

/// The tokenised form of a title, used to detect the same page across sources and to
/// de-duplicate results. Empty for an untitled page.
pub fn title_key(title: &str) -> String {
    tokenize(title).join(" ")
}

/// The [`tokenize`] rules as a tantivy tokenizer, so index and query agree.
#[derive(Debug, Clone, Copy, Default)]
pub struct CuratorTokenizer;

/// Token stream of [`CuratorTokenizer`].
pub struct CuratorTokenStream {
    tokens: std::vec::IntoIter<Token>,
    current: Token,
}

impl Tokenizer for CuratorTokenizer {
    type TokenStream<'a> = CuratorTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> CuratorTokenStream {
        let mut tokens = Vec::new();
        scan_tokens(text, &mut |from, to, token| {
            tokens.push(Token {
                offset_from: from,
                offset_to: to,
                position: tokens.len(),
                text: token.to_string(),
                position_length: 1,
            });
        });
        CuratorTokenStream {
            tokens: tokens.into_iter(),
            current: Token::default(),
        }
    }
}

impl TokenStream for CuratorTokenStream {
    fn advance(&mut self) -> bool {
        match self.tokens.next() {
            Some(token) => {
                self.current = token;
                true
            }
            None => false,
        }
    }

    fn token(&self) -> &Token {
        &self.current
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.current
    }
}

// ---------------------------------------------------------------------------------------------
// Content cleaning and sections
// ---------------------------------------------------------------------------------------------

/// The page content handed to a reader: frontmatter and HTML comments removed.
pub fn clean_content(raw: &str) -> String {
    HTML_COMMENT
        .replace_all(strip_frontmatter(raw), "")
        .trim_matches('\n')
        .to_string()
}

/// Reduce cleaned content to the text worth indexing: link and image targets replaced by their
/// labels, HTML tags by a space.
pub fn index_text(text: &str) -> String {
    let text = MD_IMAGE.replace_all(text, "$1");
    let text = MD_LINK.replace_all(&text, "$1");
    HTML_TAG.replace_all(&text, " ").into_owned()
}

/// The title found in the page itself: the first H1, else the frontmatter `title:`, else empty.
pub fn extract_title(raw: &str) -> String {
    title_of("", raw)
}

/// One retrieval unit of a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// The H2 heading, `<H2> / <H3>` for a split section, empty for the intro.
    pub heading: String,
    /// The section text.
    pub body: String,
}

/// Split at heading lines matching `heading`, ignoring fenced code. The text before the first
/// heading gets an empty heading and is dropped when blank.
fn split_at(text: &str, heading: &Regex) -> Vec<Section> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut body: Vec<&str> = Vec::new();
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
        let matched = if in_fence {
            None
        } else {
            heading.captures(line)
        };
        match matched {
            Some(captures) => {
                parts.push(Section {
                    heading: std::mem::take(&mut current),
                    body: body.join("\n"),
                });
                current = captures[1].trim().to_string();
                body.clear();
            }
            None => body.push(line),
        }
    }
    parts.push(Section {
        heading: current,
        body: body.join("\n"),
    });
    parts.retain(|part| !part.heading.is_empty() || !part.body.trim().is_empty());
    parts
}

/// Split a page into retrieval units: the intro, then one unit per H2 section; H2 sections
/// with more than [`SECTION_SPLIT_TOKENS`] tokens are split again at H3 (`<H2> / <H3>`).
///
/// A page without H2 headings is a single intro unit.
pub fn split_sections(content: &str) -> Vec<Section> {
    let mut units = Vec::new();
    for section in split_at(content, &H2) {
        if !section.heading.is_empty() && tokenize(&section.body).len() > SECTION_SPLIT_TOKENS {
            for sub in split_at(&section.body, &H3) {
                let heading = if sub.heading.is_empty() {
                    section.heading.clone()
                } else {
                    format!("{} / {}", section.heading, sub.heading)
                };
                units.push(Section {
                    heading,
                    body: sub.body,
                });
            }
        } else {
            units.push(section);
        }
    }
    if units.is_empty() {
        units.push(Section {
            heading: String::new(),
            body: content.to_string(),
        });
    }
    units
}

// ---------------------------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------------------------

/// Source priorities for the mirror rule.
///
/// Sources listed in `pinakes.yaml` use their `priority`; any other source, and every source
/// when no config is available (a manifest-less artifact), gets [`DEFAULT_PRIORITY`]. Priority
/// comes from the config alone: nothing about a source's repository makes it canonical, so
/// without a config all sources are equal and the mirror rule collapses nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Priorities {
    /// Priority by source name.
    pub explicit: BTreeMap<String, i64>,
}

impl Priorities {
    /// Priorities from the config's sources.
    pub fn from_config(config: &Config) -> Priorities {
        Priorities {
            explicit: config
                .sources
                .iter()
                .map(|s| (s.name.clone(), s.priority))
                .collect(),
        }
    }

    /// The priority of `source`: its configured value, else [`DEFAULT_PRIORITY`].
    pub fn of(&self, source: &str) -> i64 {
        self.explicit
            .get(source)
            .copied()
            .unwrap_or(DEFAULT_PRIORITY)
    }
}

/// One page of the artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// `<source>::<path>`.
    pub id: String,
    /// The source directory name.
    pub source: String,
    /// Path relative to the source directory.
    pub path: String,
    /// Repo slug from `meta.json` (the source name when absent).
    pub repo: String,
    /// Module name from `meta.json` (the source name when absent).
    pub module: String,
    /// The navigation title, else [`Page::heading`].
    pub title: String,
    /// The title found in the page itself (H1, else frontmatter).
    pub heading: String,
    /// Document type from `meta.json`, possibly empty.
    pub doc_type: String,
    /// Navigation section from `meta.json`, possibly empty.
    pub section: String,
    /// Source priority for the mirror rule.
    pub priority: i64,
    /// Cleaned content (see [`clean_content`]).
    pub content: String,
    /// Set when the page is left out as a mirror of the named page.
    pub mirror_of: Option<String>,
}

/// What `meta.json` contributes; every field is optional.
#[derive(Debug, Default)]
struct SourceMeta {
    repo: Option<String>,
    module: Option<String>,
    pages: BTreeMap<String, NavEntry>,
}

#[derive(Debug, Default, Clone)]
struct NavEntry {
    title: String,
    doc_type: String,
    section: String,
}

fn read_meta(dir: &Path) -> SourceMeta {
    let Ok(text) = std::fs::read_to_string(dir.join(META_FILE)) else {
        return SourceMeta::default();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return SourceMeta::default();
    };
    let string = |key: &str| value.get(key).and_then(|v| v.as_str()).map(str::to_string);
    let field = |entry: &serde_json::Value, key: &str| {
        entry
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let pages = value
        .get("pages")
        .and_then(|v| v.as_object())
        .map(|pages| {
            pages
                .iter()
                .map(|(path, entry)| {
                    let nav = NavEntry {
                        title: field(entry, "title"),
                        doc_type: field(entry, "doc_type"),
                        section: field(entry, "section"),
                    };
                    (path.clone(), nav)
                })
                .collect()
        })
        .unwrap_or_default();
    SourceMeta {
        repo: string("repo"),
        module: string("module"),
        pages,
    }
}

/// Relative paths (`/`-separated) of every `.md` file under `root`, sorted per directory.
fn markdown_files(root: &Path, prefix: &str, out: &mut Vec<String>) -> Result<(), IndexError> {
    let mut entries: Vec<_> = std::fs::read_dir(root)
        .map_err(io(root))?
        .collect::<Result<_, _>>()
        .map_err(io(root))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let path = entry.path();
        if path.is_dir() {
            markdown_files(&path, &rel, out)?;
        } else if path.extension().is_some_and(|ext| ext == "md") {
            out.push(rel);
        }
    }
    Ok(())
}

fn make_page(
    source: &str,
    path: &str,
    raw: &str,
    nav: Option<&NavEntry>,
    meta: &SourceMeta,
    priority: i64,
) -> Page {
    let heading = extract_title(raw);
    let nav_title = nav.map(|n| n.title.as_str()).unwrap_or_default();
    let title = if nav_title.is_empty() {
        heading.clone()
    } else {
        nav_title.to_string()
    };
    Page {
        id: page_id(source, path),
        source: source.to_string(),
        path: path.to_string(),
        repo: meta.repo.clone().unwrap_or_else(|| source.to_string()),
        module: meta.module.clone().unwrap_or_else(|| source.to_string()),
        title,
        heading,
        doc_type: nav.map(|n| n.doc_type.clone()).unwrap_or_default(),
        section: nav.map(|n| n.section.clone()).unwrap_or_default(),
        priority,
        content: clean_content(raw),
        mirror_of: None,
    }
}

fn read_lossy(path: &Path) -> Result<String, IndexError> {
    let bytes = std::fs::read(path).map_err(io(path))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Read every page of an artifact directory, in source and path order, mirrors not yet marked.
pub fn load_pages(artifact: &Path, priorities: &Priorities) -> Result<Vec<Page>, IndexError> {
    if !artifact.is_dir() {
        return Err(IndexError::NotADirectory(artifact.to_path_buf()));
    }
    let mut sources: Vec<_> = std::fs::read_dir(artifact)
        .map_err(io(artifact))?
        .collect::<Result<_, _>>()
        .map_err(io(artifact))?;
    sources.sort_by_key(std::fs::DirEntry::file_name);
    let mut pages = Vec::new();
    for entry in sources {
        let name = entry.file_name().to_string_lossy().into_owned();
        let dir = entry.path();
        if name.starts_with('_') || !dir.is_dir() {
            continue;
        }
        let meta = read_meta(&dir);
        let priority = priorities.of(&name);
        let mut files = Vec::new();
        markdown_files(&dir, "", &mut files)?;
        for path in files {
            let raw = read_lossy(&dir.join(&path))?;
            pages.push(make_page(
                &name,
                &path,
                &raw,
                meta.pages.get(&path),
                &meta,
                priority,
            ));
        }
    }
    Ok(pages)
}

/// Read one page from `_residue/<source>/<path>` for `id = <source>::<path>`.
pub fn load_residue_page(
    artifact: &Path,
    id: &str,
    priorities: &Priorities,
) -> Result<Page, IndexError> {
    let (source, path) = id
        .split_once("::")
        .filter(|(s, p)| !s.is_empty() && !p.is_empty())
        .ok_or_else(|| IndexError::BadPageId(id.to_string()))?;
    let file = artifact.join(RESIDUE_DIR).join(source).join(path);
    if !file.is_file() {
        return Err(IndexError::MissingResidue {
            id: id.to_string(),
            path: file,
        });
    }
    let raw = read_lossy(&file)?;
    let meta = read_meta(&artifact.join(source));
    let priority = priorities.of(source);
    Ok(make_page(source, path, &raw, None, &meta, priority))
}

/// Apply the mirror rule: a page whose title key (navigation title or H1) is also carried by a
/// page from a source with a higher priority is marked as a mirror of that page.
pub fn mark_mirrors(pages: &mut [Page]) {
    let keys: Vec<Vec<String>> = pages
        .iter()
        .map(|page| {
            let mut keys = Vec::new();
            for text in [&page.title, &page.heading] {
                if !text.is_empty() {
                    let key = title_key(text);
                    if !keys.contains(&key) {
                        keys.push(key);
                    }
                }
            }
            keys
        })
        .collect();
    let mut best: HashMap<&str, (i64, usize)> = HashMap::new();
    for (index, page_keys) in keys.iter().enumerate() {
        for key in page_keys {
            let entry = best
                .entry(key.as_str())
                .or_insert((pages[index].priority, index));
            if pages[index].priority > entry.0 {
                *entry = (pages[index].priority, index);
            }
        }
    }
    let mirrors: Vec<Option<String>> = keys
        .iter()
        .enumerate()
        .map(|(index, page_keys)| {
            page_keys.iter().find_map(|key| {
                let (priority, original) = best[key.as_str()];
                (priority > pages[index].priority).then(|| pages[original].id.clone())
            })
        })
        .collect();
    for (page, mirror) in pages.iter_mut().zip(mirrors) {
        page.mirror_of = mirror;
    }
}

/// One retrieval unit exposed for embedding (SPEC §16.2): the same text `Index` scores, so a
/// dense backend built from these embeds exactly what BM25 searches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// The page this unit belongs to.
    pub page_id: String,
    /// The unit's heading, empty for the intro (see [`Section::heading`]).
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

// ---------------------------------------------------------------------------------------------
// Index
// ---------------------------------------------------------------------------------------------

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
            .register(TOKENIZER_NAME, CuratorTokenizer);
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

/// Helpers for building synthetic artifacts in tests.
#[cfg(test)]
pub(crate) mod testing {
    use std::path::Path;

    /// A source to write: directory name, repo slug, `(path, nav title, content)` pages and
    /// `(path, content)` residue pages.
    pub struct SourceSpec<'a> {
        pub name: &'a str,
        pub repo: &'a str,
        pub pages: &'a [(&'a str, &'a str, &'a str)],
        pub residue: &'a [(&'a str, &'a str)],
    }

    /// Write `sources` as an artifact directory with a `meta.json` per source (no manifest).
    pub fn write_artifact(dir: &Path, sources: &[SourceSpec<'_>]) {
        for source in sources {
            let root = dir.join(source.name);
            let mut pages = serde_json::Map::new();
            for (path, title, content) in source.pages {
                let file = root.join(path);
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(&file, content).unwrap();
                if !title.is_empty() {
                    pages.insert(
                        (*path).to_string(),
                        serde_json::json!({"title": title, "doc_type": "", "section": ""}),
                    );
                }
            }
            std::fs::create_dir_all(&root).unwrap();
            let meta = serde_json::json!({
                "repo": source.repo,
                "module": source.name,
                "base_url": format!("https://github.com/{}/blob/abc", source.repo),
                "commit": "abc",
                "pages": pages,
            });
            std::fs::write(root.join("meta.json"), meta.to_string()).unwrap();
            for (path, content) in source.residue {
                let file = dir.join("_residue").join(source.name).join(path);
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(&file, content).unwrap();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{SourceSpec, write_artifact};
    use super::*;

    #[test]
    fn stopwords_are_sorted_for_binary_search() {
        assert!(STOPWORDS.windows(2).all(|w| w[0] < w[1]));
        assert!(is_stopword("the") && !is_stopword("corpus"));
    }

    #[test]
    fn tokenizer_lowercases_splits_on_non_alphanumerics_and_drops_stopwords() {
        // `caching-limits` is also an identifier compound (SPEC §10.3): the joined form is
        // appended after the ordinary tokens.
        assert_eq!(
            tokenize("How do I enable **upload** caching-limits for `StorageClass` v2?"),
            [
                "enable",
                "upload",
                "caching",
                "limits",
                "storageclass",
                "v2",
                "cachinglimits",
            ]
        );
        assert_eq!(
            tokenize("Corpus? Corpus! über K8s"),
            ["corpus", "corpus", "ber", "k8s"]
        );
        assert!(tokenize("the a of").is_empty());
        assert_eq!(title_key("The Storage Module"), "storage module");
        assert_eq!(title_key("Storage Module"), title_key("storage module!"));
    }

    #[test]
    fn identifier_compounds_add_the_joined_form() {
        // SPEC §10.3: alphanumeric runs joined by `.` `/` `_` or `-` also emit the joined form.
        assert_eq!(tokenize("spec.sink"), ["spec", "sink", "specsink"]);
        assert_eq!(tokenize("jwks_urls"), ["jwks", "urls", "jwksurls"]);
        assert_eq!(
            tokenize("aa/bb-cc.dd"),
            ["aa", "bb", "cc", "dd", "aabbccdd"],
            "several separators chain into one compound"
        );
        assert_eq!(
            tokenize("--leading"),
            ["leading"],
            "a separator with nothing alphanumeric before it does not compound"
        );
        assert_eq!(
            tokenize("trailing--"),
            ["trailing"],
            "a separator with nothing alphanumeric after it does not compound"
        );
        assert_eq!(
            tokenize("v2.3"),
            ["v2", "3", "v23"],
            "digits participate like letters"
        );
        assert_eq!(
            tokenize("plain word"),
            ["plain", "word"],
            "no separator, no compound"
        );
        // Queries are tokenised the same way, so a dotted query matches the whole path first.
        assert_eq!(tokenize("spec.sink"), tokenize("spec.sink"));
    }

    #[test]
    fn tantivy_tokenizer_matches_tokenize() {
        let text = "Expose a Workload with an Ingress";
        let mut tokenizer = CuratorTokenizer;
        let mut stream = tokenizer.token_stream(text);
        let mut seen = Vec::new();
        while stream.advance() {
            let token = stream.token();
            assert_eq!(
                &text[token.offset_from..token.offset_to].to_lowercase(),
                &token.text
            );
            seen.push(token.text.clone());
        }
        assert_eq!(seen, tokenize(text));
        assert_eq!(seen, ["expose", "workload", "ingress"]);
    }

    #[test]
    fn cleaning_strips_frontmatter_comments_links_and_tags() {
        let raw = "---\ntitle: T\n---\n\n<!-- hidden\nlines -->\n# H\n\nSee [the guide](https://x/y.md) and ![alt](img.png) <br/>done\n";
        let content = clean_content(raw);
        assert_eq!(
            content,
            "# H\n\nSee [the guide](https://x/y.md) and ![alt](img.png) <br/>done"
        );
        assert_eq!(index_text(&content), "# H\n\nSee the guide and alt  done");
        assert!(!tokenize(&index_text(&content)).contains(&"https".to_string()));
    }

    #[test]
    fn sections_split_at_h2_and_skip_fences() {
        let text =
            "intro\n\n## First ##\n\nbody 1\n```\n## not a heading\n```\n\n## Second\n\nbody 2\n";
        let units = split_sections(text);
        let headings: Vec<&str> = units.iter().map(|u| u.heading.as_str()).collect();
        assert_eq!(headings, ["", "First", "Second"]);
        assert_eq!(units[0].body, "intro\n");
        assert!(units[1].body.contains("## not a heading"));
        assert_eq!(
            split_sections(""),
            [Section {
                heading: String::new(),
                body: String::new()
            }]
        );
        assert_eq!(split_sections("\n\n## Only\n")[0].heading, "Only");
        assert_eq!(split_sections("## \n").len(), 1, "'## ' is not a heading");
    }

    #[test]
    fn long_h2_sections_split_at_h3() {
        let filler = "word ".repeat(700);
        let text = format!(
            "## Big\n\n{filler}\n### Part A\n\n{filler}\n### Part B\n\nshort\n\n## Small\n\n### Sub\n\ntiny\n"
        );
        let units = split_sections(&text);
        let headings: Vec<&str> = units.iter().map(|u| u.heading.as_str()).collect();
        assert_eq!(headings, ["Big", "Big / Part A", "Big / Part B", "Small"]);
        assert!(units[3].body.contains("### Sub"));
    }

    #[test]
    fn title_falls_back_from_nav_to_h1_to_frontmatter() {
        let meta = SourceMeta::default();
        let nav = NavEntry {
            title: "Nav".into(),
            ..NavEntry::default()
        };
        let both = "---\ntitle: Front\n---\n# Heading\n";
        assert_eq!(
            make_page("s", "p.md", both, Some(&nav), &meta, 1).title,
            "Nav"
        );
        let page = make_page("s", "p.md", both, None, &meta, 1);
        assert_eq!(
            (page.title.as_str(), page.heading.as_str()),
            ("Heading", "Heading")
        );
        let front = "---\ntitle: Front\n---\ntext\n";
        assert_eq!(make_page("s", "p.md", front, None, &meta, 1).title, "Front");
        assert_eq!(make_page("s", "p.md", "text\n", None, &meta, 1).title, "");
        assert_eq!(page.id, "s::p.md");
        assert_eq!(page.repo, "s");
    }

    fn page(source: &str, path: &str, title: &str, h1: &str, priority: i64) -> Page {
        let raw = format!("# {h1}\n\nbody\n");
        let meta = SourceMeta::default();
        let nav = NavEntry {
            title: title.into(),
            ..NavEntry::default()
        };
        make_page(source, path, &raw, Some(&nav), &meta, priority)
    }

    #[test]
    fn mirror_rule_keeps_the_higher_priority_source() {
        let mut pages = vec![
            page("handbook", "a.md", "Storage Module", "Storage module", 10),
            page("guides", "b.md", "Storage Module", "Storage module", 1),
            page("guides", "c.md", "Other", "Storage Module", 1),
            page("guides", "d.md", "Unique", "Unique", 1),
            page("other", "e.md", "Storage Module", "Storage", 10),
            page("handbook", "f.md", "", "", 10),
            page("guides", "g.md", "", "", 1),
        ];
        mark_mirrors(&mut pages);
        let mirrors: Vec<Option<&str>> = pages.iter().map(|p| p.mirror_of.as_deref()).collect();
        assert_eq!(
            mirrors,
            [
                None,
                Some("handbook::a.md"),
                Some("handbook::a.md"),
                None,
                None,
                None,
                None
            ]
        );
    }

    #[test]
    fn priorities_come_from_the_config_only() {
        let priorities = Priorities::default();
        assert_eq!(priorities.of("handbook"), DEFAULT_PRIORITY);
        assert_eq!(priorities.of("guides"), DEFAULT_PRIORITY);
        let config = Config::from_yaml(
            "version: 1\nsources:\n  - name: guides\n    repo: https://github.com/o/guides.git\n    \
             ref: main\n    priority: 42\n    resolver:\n      type: glob\n      include: ['**/*.md']\n\
             \x20 - name: handbook\n    repo: https://github.com/o/handbook.git\n    ref: main\n    \
             resolver:\n      type: glob\n      include: ['**/*.md']\n",
        )
        .unwrap();
        let priorities = Priorities::from_config(&config);
        assert_eq!(priorities.of("guides"), 42);
        assert_eq!(
            priorities.of("handbook"),
            DEFAULT_PRIORITY,
            "the config default is the fallback"
        );
        assert_eq!(priorities.of("unlisted"), DEFAULT_PRIORITY);
    }

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
