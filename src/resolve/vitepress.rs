//! `resolver.type: vitepress` (SPEC §12): a tolerant scan of a `VitePress` sidebar for `text`,
//! `link` and nested `items`, in a full `.vitepress/config.*` or a standalone sidebar file.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use globset::GlobSet;

use super::navigation::{self, JsValue, LinkTarget, NavEntry};
use super::{Candidate, Discovery, Mechanism, Plan, ResolveError, Resolver, read_file};
use crate::config::{Source, compile_globs};
use crate::sources::Checkout;

/// Default glob for the `VitePress` config file when `path` is not given.
const DEFAULT_CONFIG_GLOB: &str = "docs/.vitepress/config.*";

/// The `vitepress` resolver's own mechanism: a tolerant scan of a `VitePress` sidebar.
pub(super) struct Vitepress<'a> {
    pub(super) path: Option<&'a str>,
    pub(super) scope: &'a [String],
    pub(super) include: &'a [String],
    pub(super) exclude: &'a [String],
}

impl Resolver for Vitepress<'_> {
    fn exclude(&self) -> &[String] {
        self.exclude
    }

    fn extra_include(&self) -> &[String] {
        self.include
    }

    fn discover(
        &self,
        source: &Source,
        checkout: &Checkout,
        files: &[String],
        _config_dir: &Path,
    ) -> Result<Discovery, ResolveError> {
        let (found, nav_scope, nav_path) = plan(source, checkout, files, self.path, self.scope)?;
        Discovery::navigation(found, nav_scope, nav_path, self.exclude)
    }

    fn not_selected(&self, _path: &str, _candidate: &Candidate, plan: &Plan<'_>) -> Mechanism {
        sidebar_unlinked(plan.nav_path.as_deref().unwrap_or_default())
    }
}

/// A `vitepress` sidebar links other pages but not this one.
fn sidebar_unlinked(nav_file: &str) -> Mechanism {
    Mechanism {
        key: "sidebar:unlinked".to_string(),
        text: format!("not linked from `{nav_file}`"),
    }
}

/// Build the candidate map, residue scope and navigation file path for a `vitepress` source.
pub(super) fn plan(
    source: &Source,
    checkout: &Checkout,
    files: &[String],
    path: Option<&str>,
    scope: &[String],
) -> Result<(BTreeMap<String, Candidate>, GlobSet, String), ResolveError> {
    let nav_path = locate(&source.name, files, path)?;
    let text = read_file(checkout, &nav_path)?;
    let base_dir = content_dir(&nav_path);
    let file_set: BTreeSet<String> = files.iter().cloned().collect();

    let root = navigation::parse_js_value(&text);
    let mut entries = Vec::new();
    collect(&root, &[], &mut entries);

    let mut candidates = BTreeMap::new();
    for entry in entries {
        let doc_type = navigation::doc_type_from_section(&entry.section);
        let candidate = |path: String, selected: bool| Candidate {
            path: path.clone(),
            title: entry.title.clone(),
            doc_type: doc_type.to_string(),
            section: entry.section.clone(),
            selected,
            context: String::new(),
            rule: None,
        };
        match navigation::resolve_link(&base_dir, &entry.target, &file_set) {
            LinkTarget::Resolved(p) => {
                candidates.insert(p.clone(), candidate(p, true));
            }
            LinkTarget::Unresolved(p) => {
                candidates.insert(p.clone(), candidate(p, false));
            }
            LinkTarget::Skipped => {}
        }
    }
    let scope_set = navigation::scope_set(&base_dir, scope)?;
    Ok((candidates, scope_set, nav_path))
}

/// Find the navigation file: `path` verbatim when given, else the first file matching
/// [`DEFAULT_CONFIG_GLOB`].
fn locate(source_name: &str, files: &[String], path: Option<&str>) -> Result<String, ResolveError> {
    if let Some(p) = path {
        return files
            .iter()
            .find(|f| f.as_str() == p)
            .cloned()
            .ok_or_else(|| ResolveError::Navigation {
                name: source_name.to_string(),
                message: format!("navigation file {p:?} not found"),
            });
    }
    let glob = compile_globs("vitepress path", &[DEFAULT_CONFIG_GLOB.to_string()])?;
    let mut matches: Vec<&String> = files.iter().filter(|f| glob.is_match(f.as_str())).collect();
    matches.sort();
    matches
        .into_iter()
        .next()
        .cloned()
        .ok_or_else(|| ResolveError::Navigation {
            name: source_name.to_string(),
            message: format!(
                "no file matches the default vitepress config path {DEFAULT_CONFIG_GLOB:?}"
            ),
        })
}

/// The directory sidebar links are relative to: the parent of `.vitepress` for the default
/// config file, otherwise the navigation file's own directory (a standalone sidebar such as
/// `docs/user/_sidebar.ts` sits inside the content it indexes).
fn content_dir(nav_path: &str) -> String {
    if let Some(index) = nav_path.find("/.vitepress/") {
        nav_path[..index].to_string()
    } else if nav_path.starts_with(".vitepress/") {
        String::new()
    } else {
        nav_path
            .rsplit_once('/')
            .map(|(dir, _)| dir.to_string())
            .unwrap_or_default()
    }
}

/// Walk a parsed sidebar tree, collecting every object with a `link` and building the section
/// breadcrumb from enclosing objects' `text`, wherever in the tree the sidebar array sits
/// (`themeConfig.sidebar`, a bare array export, a multi-sidebar map by path, …).
fn collect(value: &JsValue, ancestors: &[String], out: &mut Vec<NavEntry>) {
    match value {
        JsValue::Array(items) => {
            for item in items {
                collect(item, ancestors, out);
            }
        }
        JsValue::Object(fields) => {
            let text = value.get("text").and_then(JsValue::as_str).unwrap_or("");
            if let Some(link) = value.get("link").and_then(JsValue::as_str) {
                out.push(NavEntry {
                    title: text.to_string(),
                    target: link.to_string(),
                    section: ancestors.join(" > "),
                });
            }
            if let Some(items) = value.get("items") {
                let mut next = ancestors.to_vec();
                if !text.is_empty() {
                    next.push(text.to_string());
                }
                collect(items, &next, out);
            } else {
                for (_, v) in fields {
                    collect(v, ancestors, out);
                }
            }
        }
        JsValue::Str(_) | JsValue::Other => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::fs;

    fn source(resolver_yaml: &str) -> Source {
        let text = format!(
            "version: 1\nsources:\n  - name: docs\n    repo: https://github.com/o/r\n    ref: main\n    \
             resolver:\n{resolver_yaml}"
        );
        Config::from_yaml(&text).unwrap().sources.remove(0)
    }

    fn checkout(entries: &[(&str, &str)]) -> (tempfile::TempDir, Checkout) {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in entries {
            let full = dir.path().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, content).unwrap();
        }
        let checkout = Checkout {
            root: dir.path().to_path_buf(),
            commit: "abc".to_string(),
        };
        (dir, checkout)
    }

    const CONFIG: &str = r"
export default {
  title: 'Docs',
  themeConfig: {
    sidebar: [
      {
        text: 'Guide',
        items: [
          { text: 'Getting Started', link: '/guide/getting-started#install' },
          { text: 'Overview', link: '/guide/' },
          {
            text: 'Advanced',
            items: [
              { text: 'Deep \'Dive\'', link: '/guide/advanced/deep' },
            ],
          },
        ],
      },
      {
        text: 'Reference',
        items: [
          { text: 'API', link: '/reference/api.md' },
          { text: 'Missing Page', link: '/reference/missing' },
        ],
      },
    ],
  },
}
";

    fn fixture() -> (tempfile::TempDir, Checkout) {
        checkout(&[
            ("docs/.vitepress/config.ts", CONFIG),
            ("docs/README.md", "# Docs Home\n"),
            ("docs/guide/getting-started.md", "# Getting Started\n"),
            ("docs/guide/README.md", "# Guide Overview\n"),
            ("docs/guide/advanced/deep.md", "# Deep Dive\n"),
            ("docs/reference/api.md", "# API Reference\n"),
        ])
    }

    #[test]
    fn plans_pages_sections_doc_types_residue_and_unresolved() {
        let (_dir, co) = fixture();
        let src = source("      type: vitepress\n");
        let (candidates, scope, nav_path) = plan(&src, &co, &fixture_files(), None, &[]).unwrap();

        let getting_started = &candidates["docs/guide/getting-started.md"];
        assert_eq!(getting_started.title, "Getting Started");
        assert_eq!(getting_started.section, "Guide");
        assert_eq!(getting_started.doc_type, "tutorial");
        assert!(getting_started.selected, "anchor is stripped and resolves");

        let overview = &candidates["docs/guide/README.md"];
        assert_eq!(overview.title, "Overview");
        assert!(overview.selected, "directory link resolves to README.md");

        let deep = &candidates["docs/guide/advanced/deep.md"];
        assert_eq!(deep.title, "Deep 'Dive'", "escaped quote is unescaped");
        assert_eq!(deep.section, "Guide > Advanced", "nested items group");

        let api = &candidates["docs/reference/api.md"];
        assert_eq!(api.section, "Reference");
        assert_eq!(api.doc_type, "reference");

        let missing = &candidates["docs/reference/missing.md"];
        assert!(!missing.selected, "dangling link is not selected");
        assert_eq!(missing.title, "Missing Page");

        assert!(scope.is_match("docs/x.md"));
        assert!(!scope.is_match("other/x.md"));
        assert_eq!(nav_path, "docs/.vitepress/config.ts");
    }

    /// The file list matching the fixture's checkout.
    fn fixture_files() -> Vec<String> {
        vec![
            "docs/.vitepress/config.ts".to_string(),
            "docs/README.md".to_string(),
            "docs/guide/getting-started.md".to_string(),
            "docs/guide/README.md".to_string(),
            "docs/guide/advanced/deep.md".to_string(),
            "docs/reference/api.md".to_string(),
        ]
    }

    #[test]
    fn locate_uses_an_explicit_path_or_errors_when_nothing_matches() {
        let files = fixture_files();
        assert_eq!(
            locate("s", &files, Some("docs/.vitepress/config.ts")).unwrap(),
            "docs/.vitepress/config.ts"
        );
        let err = locate("s", &files, Some("nope.ts")).unwrap_err();
        assert!(matches!(err, ResolveError::Navigation { .. }), "{err}");
        let err = locate("s", &[], None).unwrap_err();
        assert!(matches!(err, ResolveError::Navigation { .. }), "{err}");
    }

    #[test]
    fn content_dir_strips_the_vitepress_component() {
        assert_eq!(content_dir("docs/.vitepress/config.ts"), "docs");
        assert_eq!(content_dir(".vitepress/config.ts"), "");
        assert_eq!(content_dir("docs/user/_sidebar.ts"), "docs/user");
    }

    #[test]
    fn sidebar_unlinked_names_the_nav_file() {
        let mechanism = sidebar_unlinked("docs/.vitepress/config.ts");
        assert_eq!(mechanism.key, "sidebar:unlinked");
        assert_eq!(
            mechanism.text,
            "not linked from `docs/.vitepress/config.ts`"
        );
    }
}
