//! Loading an artifact directory into pages: `meta.json`, the markdown files of every source,
//! residue pages, source priorities and the mirror rule (SPEC §5).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::config::Config;
use crate::layout::{META_FILE, RESIDUE_DIR};
use crate::manifest::page_id;
use crate::text::{clean_content, extract_title};
use crate::tokenizer::title_key;

/// Errors raised while loading an artifact directory into pages.
#[derive(Debug, Error)]
pub enum CorpusError {
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
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> CorpusError + '_ {
    move |source| CorpusError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Priority of a source that `pinakes.yaml` does not list (or of every source without a
/// config): the config default, so unconfigured sources never collapse each other.
pub const DEFAULT_PRIORITY: i64 = crate::config::DEFAULT_PRIORITY;

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
pub(crate) struct SourceMeta {
    repo: Option<String>,
    module: Option<String>,
    pages: BTreeMap<String, NavEntry>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct NavEntry {
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
fn markdown_files(root: &Path, prefix: &str, out: &mut Vec<String>) -> Result<(), CorpusError> {
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

pub(crate) fn make_page(
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

fn read_lossy(path: &Path) -> Result<String, CorpusError> {
    let bytes = std::fs::read(path).map_err(io(path))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Read every page of an artifact directory, in source and path order, mirrors not yet marked.
pub fn load_pages(artifact: &Path, priorities: &Priorities) -> Result<Vec<Page>, CorpusError> {
    if !artifact.is_dir() {
        return Err(CorpusError::NotADirectory(artifact.to_path_buf()));
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
) -> Result<Page, CorpusError> {
    let (source, path) = id
        .split_once("::")
        .filter(|(s, p)| !s.is_empty() && !p.is_empty())
        .ok_or_else(|| CorpusError::BadPageId(id.to_string()))?;
    let file = artifact.join(RESIDUE_DIR).join(source).join(path);
    if !file.is_file() {
        return Err(CorpusError::MissingResidue {
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
