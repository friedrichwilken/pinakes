//! The page registry: one description of a page, whether it was selected or left as residue.
//!
//! A [`PageRecord`] joins what `manifest.json` and `residue.jsonl` each say about a page; a
//! [`PageRegistry`] holds every record of one resolve run (or of the files it wrote) in a
//! defined order with lookups by page id. The module does no I/O: it is built from a
//! [`Manifest`] and a slice of [`ResidueEntry`] that the caller already holds. The serialised
//! forms stay what they are; [`PageRegistry::residue_entries`] gives the residue back exactly
//! as it went in.

use std::collections::BTreeMap;

use crate::manifest::{Manifest, ManifestSource, PageEntry, SelectedBy, page_id};
use crate::residue::{Reason, ResidueEntry, Rule};

/// A page id, `<source>::<path>`, as built by [`crate::manifest::page_id`].
pub type PageId = String;

/// Where a page stands after selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageStatus {
    /// The page is in the corpus: the manifest lists it.
    Selected {
        /// What selected the page.
        by: SelectedBy,
        /// The selected path this page was rendered from (SPEC §10.1), when it is a rendered
        /// page rather than a copy of the selected file.
        rendered_from: Option<String>,
    },
    /// The page was left out: `residue.jsonl` lists it.
    Residue {
        /// Why the page is residue.
        reason: Reason,
        /// The mechanism that decided the entry; absent only for entries written by an older
        /// version of this tool.
        rule: Option<Rule>,
        /// Sidebar section or TOC branch when the resolver gave one.
        context: String,
    },
    /// A file under `<artifact>/<source>/` that the manifest does not know.
    ArtifactOnly,
}

/// Everything known about one page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRecord {
    /// `<source>::<path>`. For residue it is the entry's own id, verbatim.
    pub id: PageId,
    /// Source name.
    pub source: String,
    /// Path relative to the repository root; for a rendered page, the output path.
    pub path: String,
    /// The source's `owner/repo`; empty when the manifest does not list the source.
    pub repo: String,
    /// The source's fetched commit; empty when the manifest does not list the source.
    pub commit: String,
    /// Page title; may be empty.
    pub title: String,
    /// Document type as reported by the resolver; empty for residue.
    pub doc_type: String,
    /// Navigation section as reported by the resolver; empty for residue.
    pub section: String,
    /// The page's upstream URL pinned to the fetched commit. Stored, never re-derived: a residue
    /// record keeps its entry's url verbatim, even when that is empty.
    pub url: String,
    /// Hex SHA-256 of the page bytes; empty for an unresolved link, which has no file.
    pub sha256: String,
    /// The first tokens of the page body. `Some` for residue (the inner text may be empty);
    /// `None` for a corpus page until someone loads it.
    pub excerpt: Option<String>,
    /// Whether the page was selected, left as residue, or only found in the artifact.
    pub status: PageStatus,
}

impl PageRecord {
    /// The record of the selected page `path` in the manifest source `source`, named `name`.
    #[must_use]
    pub fn from_page_entry(
        name: &str,
        source: &ManifestSource,
        path: &str,
        entry: &PageEntry,
    ) -> PageRecord {
        PageRecord {
            id: page_id(name, path),
            source: name.to_string(),
            path: path.to_string(),
            repo: source.repo.clone(),
            commit: source.commit.clone(),
            title: entry.title.clone(),
            doc_type: entry.doc_type.clone(),
            section: entry.section.clone(),
            url: source.page_url(path).unwrap_or_default(),
            sha256: entry.sha256.clone(),
            excerpt: None,
            status: PageStatus::Selected {
                by: entry.selected_by,
                rendered_from: entry.rendered_from.clone(),
            },
        }
    }

    /// The record of a residue entry. `source` is the entry's manifest source when the manifest
    /// lists it; it supplies `repo` and `commit` only, never the url.
    #[must_use]
    pub fn from_residue_entry(entry: &ResidueEntry, source: Option<&ManifestSource>) -> PageRecord {
        PageRecord {
            id: entry.id.clone(),
            source: entry.source.clone(),
            path: entry.path.clone(),
            repo: source.map(|s| s.repo.clone()).unwrap_or_default(),
            commit: source.map(|s| s.commit.clone()).unwrap_or_default(),
            title: entry.title.clone(),
            doc_type: String::new(),
            section: String::new(),
            url: entry.url.clone(),
            sha256: entry.sha256.clone(),
            excerpt: Some(entry.excerpt.clone()),
            status: PageStatus::Residue {
                reason: entry.reason,
                rule: entry.rule.clone(),
                context: entry.context.clone(),
            },
        }
    }

    /// A page found under `<artifact>/<source>/` that the manifest does not know about.
    #[must_use]
    pub fn artifact_only(source: &str, path: &str, sha256: String, url: String) -> PageRecord {
        PageRecord {
            id: page_id(source, path),
            source: source.to_string(),
            path: path.to_string(),
            repo: String::new(),
            commit: String::new(),
            title: String::new(),
            doc_type: String::new(),
            section: String::new(),
            url,
            sha256,
            excerpt: None,
            status: PageStatus::ArtifactOnly,
        }
    }

    /// What selected the page, when it is a selected page.
    #[must_use]
    pub fn selected_by(&self) -> Option<SelectedBy> {
        match &self.status {
            PageStatus::Selected { by, .. } => Some(*by),
            _ => None,
        }
    }

    /// Why the page is residue, when it is.
    #[must_use]
    pub fn reason(&self) -> Option<Reason> {
        match &self.status {
            PageStatus::Residue { reason, .. } => Some(*reason),
            _ => None,
        }
    }

    /// The mechanism that made the page residue, when one is recorded.
    #[must_use]
    pub fn rule(&self) -> Option<&Rule> {
        match &self.status {
            PageStatus::Residue { rule, .. } => rule.as_ref(),
            _ => None,
        }
    }

    /// Whether the page is part of the corpus: selected, or found only in the artifact.
    #[must_use]
    pub fn is_corpus(&self) -> bool {
        !matches!(self.status, PageStatus::Residue { .. })
    }

    /// Whether the page is residue with [`Reason::Excluded`].
    #[must_use]
    pub fn is_excluded(&self) -> bool {
        self.reason() == Some(Reason::Excluded)
    }

    /// The `residue.jsonl` entry of a residue record; `None` for any other status.
    #[must_use]
    pub fn to_residue_entry(&self) -> Option<ResidueEntry> {
        let PageStatus::Residue {
            reason,
            rule,
            context,
        } = &self.status
        else {
            return None;
        };
        Some(ResidueEntry {
            id: self.id.clone(),
            source: self.source.clone(),
            path: self.path.clone(),
            reason: *reason,
            sha256: self.sha256.clone(),
            title: self.title.clone(),
            excerpt: self.excerpt.clone().unwrap_or_default(),
            context: context.clone(),
            url: self.url.clone(),
            rule: rule.clone(),
        })
    }
}

/// Every page record of one run, in insertion order, with lookups by id.
///
/// Order is part of the contract, because it decides output bytes downstream: records are never
/// reordered or dropped, and iteration never goes by id string (the id order `a-b::c` < `a::z`
/// differs from the `(source, path)` order `a` < `a-b`). Records that share an id are all kept;
/// a lookup returns the first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageRegistry {
    /// Insertion order; nothing is ever dropped.
    records: Vec<PageRecord>,
    /// Corpus records (selected or artifact-only) by id; the first record with an id wins.
    corpus: BTreeMap<PageId, usize>,
    /// Residue records by id; the first record with an id wins.
    residue: BTreeMap<PageId, usize>,
}

impl PageRegistry {
    /// A registry holding `records` in the given order.
    #[must_use]
    pub fn from_records(records: impl IntoIterator<Item = PageRecord>) -> PageRegistry {
        let mut registry = PageRegistry::default();
        for record in records {
            let position = registry.records.len();
            let index = if record.is_corpus() {
                &mut registry.corpus
            } else {
                &mut registry.residue
            };
            index.entry(record.id.clone()).or_insert(position);
            registry.records.push(record);
        }
        registry
    }

    /// The registry of what is on disk: the manifest's pages in [`Manifest::pages`] order, that
    /// is by `(source, path)`, then `residue` in the order given.
    #[must_use]
    pub fn load(manifest: Option<&Manifest>, residue: &[ResidueEntry]) -> PageRegistry {
        // Not `manifest.pages()`: building a record needs the page's own `&ManifestSource` (for
        // `repo`, `commit` and `page_url`), which that iterator does not hand back.
        let selected = manifest.into_iter().flat_map(|manifest| {
            manifest.sources.iter().flat_map(|(name, source)| {
                source.pages.iter().map(move |(path, entry)| {
                    PageRecord::from_page_entry(name, source, path, entry)
                })
            })
        });
        let left_out = residue.iter().map(|entry| {
            let source = manifest.and_then(|manifest| manifest.sources.get(&entry.source));
            PageRecord::from_residue_entry(entry, source)
        });
        PageRegistry::from_records(selected.chain(left_out))
    }

    /// The registry of a resolve run, from the assembled (post-render) manifest and the residue
    /// the run found. The residue is put in the order `residue.jsonl` is written in, a stable
    /// sort by `(source, path)`, so the result equals [`PageRegistry::load`] over the written
    /// files.
    #[must_use]
    pub fn from_resolve(manifest: &Manifest, residue: &[ResidueEntry]) -> PageRegistry {
        let mut sorted = residue.to_vec();
        sorted.sort_by(|a, b| {
            (a.source.as_str(), a.path.as_str()).cmp(&(b.source.as_str(), b.path.as_str()))
        });
        PageRegistry::load(Some(manifest), &sorted)
    }

    /// The record with this id: the corpus record when there is one, else the residue record.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&PageRecord> {
        self.corpus_page(id).or_else(|| self.residue_page(id))
    }

    /// The first corpus record (selected or artifact-only) with this id.
    #[must_use]
    pub fn corpus_page(&self, id: &str) -> Option<&PageRecord> {
        self.corpus.get(id).and_then(|&i| self.records.get(i))
    }

    /// The first residue record with this id.
    #[must_use]
    pub fn residue_page(&self, id: &str) -> Option<&PageRecord> {
        self.residue.get(id).and_then(|&i| self.records.get(i))
    }

    /// Every record, in insertion order.
    pub fn records(&self) -> impl Iterator<Item = &PageRecord> {
        self.records.iter()
    }

    /// The selected records, in insertion order.
    pub fn selected(&self) -> impl Iterator<Item = &PageRecord> {
        self.records
            .iter()
            .filter(|r| matches!(r.status, PageStatus::Selected { .. }))
    }

    /// The residue records, in insertion order, including records that share an id.
    pub fn residue(&self) -> impl Iterator<Item = &PageRecord> {
        self.records
            .iter()
            .filter(|r| matches!(r.status, PageStatus::Residue { .. }))
    }

    /// The residue records as `residue.jsonl` entries, in insertion order.
    #[must_use]
    pub fn residue_entries(&self) -> Vec<ResidueEntry> {
        self.records
            .iter()
            .filter_map(PageRecord::to_residue_entry)
            .collect()
    }

    /// The number of records.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether the registry holds no record.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Add an [`PageStatus::ArtifactOnly`] record found in the artifact but not the manifest.
    /// Does nothing and returns `false` when a corpus record with this id already exists (a
    /// selected page always wins); otherwise inserts it and returns `true`.
    #[must_use]
    pub fn insert_artifact_only(&mut self, record: PageRecord) -> bool {
        if self.corpus.contains_key(&record.id) {
            return false;
        }
        let position = self.records.len();
        self.corpus.insert(record.id.clone(), position);
        self.records.push(record);
        true
    }

    /// Set the excerpt of the corpus record (selected or artifact-only) with this id. Does
    /// nothing and returns `false` when there is no such record; residue excerpts are set only
    /// at construction, from the residue entry.
    #[must_use]
    pub fn set_excerpt(&mut self, id: &str, excerpt: String) -> bool {
        match self.corpus.get(id).and_then(|&i| self.records.get_mut(i)) {
            Some(record) => {
                record.excerpt = Some(excerpt);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    fn page(title: &str, rendered_from: Option<&str>) -> PageEntry {
        PageEntry {
            sha256: "ab".repeat(32),
            title: title.to_string(),
            doc_type: "concept".to_string(),
            section: "Guides".to_string(),
            selected_by: SelectedBy::Include,
            rendered_from: rendered_from.map(str::to_string),
        }
    }

    /// A manifest from `(source name, [(path, entry)])`, every source at `example/<name>`.
    fn manifest(sources: &[(&str, Vec<(&str, PageEntry)>)]) -> Manifest {
        let mut manifest = Manifest::new("2026-09-16T12:00:00Z".to_string());
        for (name, pages) in sources {
            let json = serde_json::json!({
                "repo": format!("example/{name}"),
                "repo_url": format!("https://github.com/example/{name}"),
                "resolver": "glob",
                "ref": "main",
                "commit": COMMIT,
                "pages": {},
            });
            let mut source: ManifestSource =
                serde_json::from_value(json).expect("a minimal manifest source");
            for (path, entry) in pages {
                source.pages.insert((*path).to_string(), entry.clone());
            }
            manifest.sources.insert((*name).to_string(), source);
        }
        manifest
    }

    fn residue(source: &str, path: &str, reason: Reason) -> ResidueEntry {
        ResidueEntry {
            id: page_id(source, path),
            source: source.to_string(),
            path: path.to_string(),
            reason,
            sha256: "cd".repeat(32),
            title: "Left out".to_string(),
            excerpt: "some words".to_string(),
            context: "Sidebar".to_string(),
            url: format!("https://github.com/example/{source}/blob/{COMMIT}/{path}"),
            rule: Some(Rule::reproduced()),
        }
    }

    fn ids<'a>(records: impl Iterator<Item = &'a PageRecord>) -> Vec<&'a str> {
        records.map(|r| r.id.as_str()).collect()
    }

    #[test]
    fn residue_entries_round_trip_for_every_reason() {
        let mut entries: Vec<ResidueEntry> = [
            Reason::NotSelected,
            Reason::NewSource,
            Reason::Excluded,
            Reason::UnresolvedLink,
        ]
        .into_iter()
        .map(|reason| residue("handbook", "docs/a.md", reason))
        .collect();
        // An unresolved link has no file: no hash, no excerpt, no title.
        let link = entries.last_mut().expect("four entries");
        link.sha256 = String::new();
        link.excerpt = String::new();
        link.title = String::new();
        // An entry written by an older version: no rule, no url.
        let mut old = residue("handbook", "docs/old.md", Reason::NotSelected);
        old.rule = None;
        old.url = String::new();
        entries.push(old);

        for entry in &entries {
            let record = PageRecord::from_residue_entry(entry, None);
            assert_eq!(record.to_residue_entry().as_ref(), Some(entry));
            assert_eq!(record.reason(), Some(entry.reason));
            assert_eq!(record.rule(), entry.rule.as_ref());
            assert_eq!(record.is_excluded(), entry.reason == Reason::Excluded);
            assert!(!record.is_corpus());
            assert_eq!(record.selected_by(), None);
        }
        let registry = PageRegistry::load(None, &entries);
        assert_eq!(registry.residue_entries(), entries);
    }

    #[test]
    fn load_puts_selected_pages_in_source_path_order_then_residue_in_slice_order() {
        // By id string "a-b::c.md" sorts before "a::z.md"; by (source, path) "a" comes first.
        let manifest = manifest(&[
            ("a-b", vec![("c.md", page("C", None))]),
            (
                "a",
                vec![("z.md", page("Z", None)), ("b.md", page("B", None))],
            ),
        ]);
        let left_out = vec![
            residue("a-b", "r.md", Reason::NotSelected),
            residue("a", "r.md", Reason::NotSelected),
        ];
        let registry = PageRegistry::load(Some(&manifest), &left_out);

        assert_eq!(
            ids(registry.selected()),
            ["a::b.md", "a::z.md", "a-b::c.md"]
        );
        // `load` does not call `Manifest::pages()` (see its doc comment), but must still agree
        // with the order that iterator defines.
        let from_manifest_pages: Vec<String> = manifest.pages().map(|(id, ..)| id).collect();
        assert_eq!(ids(registry.selected()), from_manifest_pages);
        assert_eq!(ids(registry.residue()), ["a-b::r.md", "a::r.md"]);
        assert_eq!(
            ids(registry.records()),
            ["a::b.md", "a::z.md", "a-b::c.md", "a-b::r.md", "a::r.md"]
        );
        assert_eq!(registry.len(), 5);
        assert!(!registry.is_empty());
        assert_eq!(registry.residue_entries(), left_out);
    }

    #[test]
    fn from_resolve_sorts_residue_stably_as_the_residue_file_does() {
        let manifest = manifest(&[("a", vec![("z.md", page("Z", None))])]);
        let mut first = residue("a", "r.md", Reason::NotSelected);
        first.title = "first".to_string();
        let mut second = residue("a", "r.md", Reason::Excluded);
        second.title = "second".to_string();
        let found = vec![
            residue("a-b", "r.md", Reason::NotSelected),
            first.clone(),
            residue("a", "q.md", Reason::NotSelected),
            second.clone(),
        ];
        let registry = PageRegistry::from_resolve(&manifest, &found);
        assert_eq!(
            ids(registry.residue()),
            ["a::q.md", "a::r.md", "a::r.md", "a-b::r.md"]
        );
        let titles: Vec<_> = registry.residue().map(|r| r.title.as_str()).collect();
        assert_eq!(titles[1..3], ["first", "second"], "the sort is stable");

        // The same registry comes back from what `residue::to_jsonl` writes.
        let text = crate::residue::to_jsonl(&found).expect("serialises");
        let reread: Vec<ResidueEntry> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("one entry per line"))
            .collect();
        assert_eq!(registry, PageRegistry::load(Some(&manifest), &reread));
    }

    #[test]
    fn the_first_residue_record_with_an_id_wins_and_all_are_kept() {
        let mut first = residue("handbook", "docs/a.md", Reason::UnresolvedLink);
        first.sha256 = String::new();
        let second = residue("handbook", "docs/a.md", Reason::NotSelected);
        let registry = PageRegistry::load(None, &[first.clone(), second.clone()]);

        assert_eq!(registry.residue().count(), 2);
        assert_eq!(registry.residue_entries(), [first, second]);
        let found = registry
            .residue_page("handbook::docs/a.md")
            .expect("a record");
        assert_eq!(found.reason(), Some(Reason::UnresolvedLink));
        // An unresolved link is found, with an empty hash rather than none.
        assert_eq!(
            registry
                .get("handbook::docs/a.md")
                .map(|r| r.sha256.as_str()),
            Some("")
        );
    }

    #[test]
    fn get_prefers_the_corpus_record_over_a_residue_record() {
        let manifest = manifest(&[("handbook", vec![("docs/a.md", page("Selected", None))])]);
        let left_out = [residue("handbook", "docs/a.md", Reason::NotSelected)];
        let registry = PageRegistry::load(Some(&manifest), &left_out);
        let id = "handbook::docs/a.md";

        let got = registry.get(id).expect("a record");
        assert_eq!(got.title, "Selected");
        assert_eq!(got.selected_by(), Some(SelectedBy::Include));
        assert!(got.is_corpus());
        assert_eq!(got.excerpt, None);
        assert_eq!(registry.corpus_page(id), Some(got));
        assert_eq!(
            registry.residue_page(id).map(|r| r.title.as_str()),
            Some("Left out")
        );
        assert_eq!(registry.get("handbook::docs/missing.md"), None);
        assert_eq!(registry.corpus_page("no separator"), None);
    }

    #[test]
    fn a_rendered_page_keeps_rendered_from_and_gets_an_output_path_url() {
        let manifest = manifest(&[(
            "api",
            vec![("crds/widget.md", page("Widget", Some("crds/widget.yaml")))],
        )]);
        let registry = PageRegistry::load(Some(&manifest), &[]);
        let record = registry.get("api::crds/widget.md").expect("a record");

        assert_eq!(
            record.status,
            PageStatus::Selected {
                by: SelectedBy::Include,
                rendered_from: Some("crds/widget.yaml".to_string()),
            }
        );
        assert_eq!(record.path, "crds/widget.md");
        assert_eq!(
            record.url,
            format!("https://github.com/example/api/blob/{COMMIT}/crds/widget.md")
        );
        assert_eq!(record.repo, "example/api");
        assert_eq!(record.commit, COMMIT);
        assert_eq!(record.doc_type, "concept");
        assert_eq!(record.section, "Guides");
        assert_eq!(record.to_residue_entry(), None);
        assert_eq!(record.reason(), None);
        assert_eq!(record.rule(), None);
    }

    #[test]
    fn residue_of_an_unknown_source_has_no_repo_or_commit_but_keeps_its_url() {
        let manifest = manifest(&[("handbook", vec![("docs/a.md", page("A", None))])]);
        let dropped = residue("archived", "docs/x.md", Reason::Excluded);
        let mut known = residue("handbook", "docs/b.md", Reason::NotSelected);
        known.url = String::new();
        let registry = PageRegistry::load(Some(&manifest), &[dropped.clone(), known]);

        let record = registry.get("archived::docs/x.md").expect("a record");
        assert_eq!(record.repo, "");
        assert_eq!(record.commit, "");
        assert_eq!(record.url, dropped.url);

        // A known source supplies repo and commit, but an empty url is never backfilled.
        let record = registry.get("handbook::docs/b.md").expect("a record");
        assert_eq!(record.repo, "example/handbook");
        assert_eq!(record.commit, COMMIT);
        assert_eq!(record.url, "");
    }

    #[test]
    fn load_without_a_manifest_holds_the_residue_only() {
        let left_out = [residue("handbook", "docs/a.md", Reason::NotSelected)];
        let registry = PageRegistry::load(None, &left_out);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.selected().count(), 0);
        let record = registry.get("handbook::docs/a.md").expect("a record");
        assert_eq!(record.repo, "");
        assert_eq!(record.excerpt.as_deref(), Some("some words"));

        let empty = PageRegistry::load(None, &[]);
        assert!(empty.is_empty());
        assert_eq!(empty, PageRegistry::default());
    }

    #[test]
    fn artifact_only_is_a_bare_corpus_record() {
        let record = PageRecord::artifact_only(
            "handbook",
            "docs/orphan.md",
            "ab".repeat(32),
            "https://example.test/orphan.md".to_string(),
        );
        assert_eq!(record.id, "handbook::docs/orphan.md");
        assert_eq!(record.source, "handbook");
        assert_eq!(record.path, "docs/orphan.md");
        assert_eq!((record.repo.as_str(), record.commit.as_str()), ("", ""));
        assert_eq!(record.title, "");
        assert_eq!(record.excerpt, None);
        assert!(record.is_corpus());
        assert_eq!(record.selected_by(), None);
        assert_eq!(record.to_residue_entry(), None);
        assert_eq!(record.status, PageStatus::ArtifactOnly);
    }

    #[test]
    fn insert_artifact_only_loses_to_an_existing_corpus_record_but_wins_over_nothing() {
        let manifest = manifest(&[("handbook", vec![("docs/a.md", page("A", None))])]);
        let mut registry = PageRegistry::load(Some(&manifest), &[]);

        // A corpus record already exists at this id: the manifest's selected page wins.
        let shadowed = PageRecord::artifact_only(
            "handbook",
            "docs/a.md",
            "cd".repeat(32),
            "https://example.test/a.md".to_string(),
        );
        assert!(!registry.insert_artifact_only(shadowed));
        assert_eq!(registry.len(), 1);
        assert_eq!(
            registry
                .get("handbook::docs/a.md")
                .map(|r| r.title.as_str()),
            Some("A")
        );

        // No corpus record at this id: the artifact-only record is inserted.
        let orphan =
            PageRecord::artifact_only("handbook", "docs/orphan.md", "ef".repeat(32), String::new());
        assert!(registry.insert_artifact_only(orphan.clone()));
        assert_eq!(registry.len(), 2);
        assert_eq!(
            registry.corpus_page("handbook::docs/orphan.md"),
            Some(&orphan)
        );

        // Inserting again at the same id now loses too: the first artifact-only record wins.
        let later =
            PageRecord::artifact_only("handbook", "docs/orphan.md", "00".repeat(32), String::new());
        assert!(!registry.insert_artifact_only(later));
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn set_excerpt_only_reaches_a_corpus_record() {
        let manifest = manifest(&[("handbook", vec![("docs/a.md", page("A", None))])]);
        let residue = [residue("handbook", "docs/b.md", Reason::NotSelected)];
        let mut registry = PageRegistry::load(Some(&manifest), &residue);

        assert!(registry.set_excerpt("handbook::docs/a.md", "hello".to_string()));
        assert_eq!(
            registry
                .get("handbook::docs/a.md")
                .and_then(|r| r.excerpt.as_deref()),
            Some("hello")
        );

        // A residue record is not a corpus record: its excerpt is untouched.
        let before = registry
            .residue_page("handbook::docs/b.md")
            .unwrap()
            .excerpt
            .clone();
        assert!(!registry.set_excerpt("handbook::docs/b.md", "ignored".to_string()));
        assert_eq!(
            registry
                .residue_page("handbook::docs/b.md")
                .unwrap()
                .excerpt,
            before
        );

        assert!(!registry.set_excerpt("handbook::docs/missing.md", "x".to_string()));
    }
}
