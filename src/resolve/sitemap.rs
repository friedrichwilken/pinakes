//! `resolver.type: sitemap` (SPEC §12): `sitemap.xml`, or a plain URL list file, mapped to
//! repository paths via `url_prefix` → `path_prefix`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use globset::GlobSet;
use regex::Regex;

use super::navigation::{self, LinkTarget};
use super::{Candidate, ResolveError, read_file};
use crate::config::Source;
use crate::sources::Checkout;

/// Default navigation file when `path` is not given.
const DEFAULT_PATH: &str = "sitemap.xml";

/// Build the candidate map, residue scope and navigation file path for a `sitemap` source.
pub(super) fn plan(
    source: &Source,
    checkout: &Checkout,
    files: &[String],
    path: Option<&str>,
    scope: &[String],
    url_prefix: &str,
    path_prefix: &str,
) -> Result<(BTreeMap<String, Candidate>, GlobSet, String), ResolveError> {
    let nav_path = path.unwrap_or(DEFAULT_PATH);
    if !files.iter().any(|f| f == nav_path) {
        return Err(ResolveError::Navigation {
            name: source.name.clone(),
            message: format!("navigation file {nav_path:?} not found"),
        });
    }
    let text = read_file(checkout, nav_path)?;
    let file_set: BTreeSet<String> = files.iter().cloned().collect();

    let mut candidates = BTreeMap::new();
    for url in extract_urls(&text) {
        let Some(rest) = url.trim().strip_prefix(url_prefix) else {
            continue;
        };
        match map_url(rest, path_prefix, &file_set) {
            LinkTarget::Resolved(p) => {
                candidates.insert(
                    p.clone(),
                    Candidate {
                        path: p,
                        title: String::new(),
                        doc_type: navigation::doc_type_from_section("").to_string(),
                        section: String::new(),
                        selected: true,
                        context: String::new(),
                    },
                );
            }
            LinkTarget::Unresolved(p) => {
                candidates.insert(
                    p.clone(),
                    Candidate {
                        path: p,
                        title: String::new(),
                        doc_type: navigation::doc_type_from_section("").to_string(),
                        section: String::new(),
                        selected: false,
                        context: String::new(),
                    },
                );
            }
            LinkTarget::Skipped => {}
        }
    }
    let default_dir = path_prefix.trim_end_matches('/');
    let scope_set = navigation::scope_set(default_dir, scope)?;
    Ok((candidates, scope_set, nav_path.to_string()))
}

/// Map the part of a sitemap URL after `url_prefix` to a repository path: `.html` becomes
/// `.md`, and a trailing slash (or nothing at all) falls back to `README.md` then `index.md`.
fn map_url(rest: &str, path_prefix: &str, files: &BTreeSet<String>) -> LinkTarget {
    let mut target = format!("{path_prefix}{rest}");
    if let Some(stripped) = target.strip_suffix(".html") {
        target = format!("{stripped}.md");
    }
    if target.is_empty() || target.ends_with('/') {
        let base = target.trim_end_matches('/');
        let readme = format!("{base}/README.md");
        let index = format!("{base}/index.md");
        if files.contains(&readme) {
            return LinkTarget::Resolved(readme);
        }
        if files.contains(&index) {
            return LinkTarget::Resolved(index);
        }
        return LinkTarget::Unresolved(readme);
    }
    if navigation::is_markdown_extension(&target) {
        return if files.contains(&target) {
            LinkTarget::Resolved(target)
        } else {
            LinkTarget::Unresolved(target)
        };
    }
    let md = format!("{target}.md");
    if files.contains(&md) {
        LinkTarget::Resolved(md)
    } else {
        LinkTarget::Unresolved(md)
    }
}

/// `<loc>…</loc>` entries when the file looks like XML, else one URL per non-blank,
/// non-comment line.
fn extract_urls(text: &str) -> Vec<String> {
    if text.contains("<loc") {
        loc_regex()
            .captures_iter(text)
            .map(|c| c[1].trim().to_string())
            .collect()
    } else {
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_string)
            .collect()
    }
}

fn loc_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?is)<loc>\s*([^<]+?)\s*</loc>").expect("valid regex"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::fs;

    fn source(resolver_yaml: &str) -> Source {
        let text = format!(
            "version: 1\nsources:\n  - name: site\n    repo: https://github.com/o/r\n    ref: main\n    \
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

    const SITEMAP_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <url><loc>https://example.com/docs/guide/intro.html</loc></url>
  <url><loc>https://example.com/docs/guide/</loc></url>
  <url><loc>https://example.com/docs/reference/api.html</loc></url>
  <url><loc>https://example.com/docs/missing.html</loc></url>
</urlset>
"#;

    const SITEMAP_URLS: &str = "\
https://example.com/docs/guide/intro.html
https://example.com/docs/guide/
https://example.com/docs/reference/api.html
https://example.com/docs/missing.html
";

    fn fixture_files(nav_path: &str) -> Vec<String> {
        vec![
            nav_path.to_string(),
            "docs/guide/intro.md".to_string(),
            "docs/guide/index.md".to_string(),
            "docs/reference/api.md".to_string(),
            "docs/orphan.md".to_string(),
        ]
    }

    fn resolver_yaml() -> String {
        "      type: sitemap\n      url_prefix: 'https://example.com/docs/'\n      path_prefix: 'docs/'\n"
            .to_string()
    }

    fn assert_common(candidates: &BTreeMap<String, Candidate>, scope: &GlobSet) {
        assert!(
            candidates["docs/guide/intro.md"].selected,
            ".html maps to .md"
        );
        assert!(
            candidates["docs/guide/index.md"].selected,
            "trailing slash falls back to index.md"
        );
        assert!(candidates["docs/reference/api.md"].selected);
        assert!(!candidates["docs/missing.md"].selected, "dangling url");
        assert!(!candidates.contains_key("docs/orphan.md"));
        assert!(scope.is_match("docs/orphan.md"));
        assert!(!scope.is_match("other/x.md"));
    }

    #[test]
    fn plans_pages_and_residue_from_an_xml_sitemap() {
        let (_dir, co) = checkout(&[
            ("sitemap.xml", SITEMAP_XML),
            ("docs/guide/intro.md", "# Intro\n"),
            ("docs/guide/index.md", "# Guide\n"),
            ("docs/reference/api.md", "# API\n"),
            ("docs/orphan.md", "# Orphan\n"),
        ]);
        let src = source(&resolver_yaml());
        let (candidates, scope, nav_path) = plan(
            &src,
            &co,
            &fixture_files("sitemap.xml"),
            None,
            &[],
            "https://example.com/docs/",
            "docs/",
        )
        .unwrap();
        assert_common(&candidates, &scope);
        assert_eq!(nav_path, "sitemap.xml");
    }

    #[test]
    fn plans_pages_from_a_plain_url_list() {
        let (_dir, co) = checkout(&[
            ("urls.txt", SITEMAP_URLS),
            ("docs/guide/intro.md", "# Intro\n"),
            ("docs/guide/index.md", "# Guide\n"),
            ("docs/reference/api.md", "# API\n"),
            ("docs/orphan.md", "# Orphan\n"),
        ]);
        let src = source(&resolver_yaml());
        let (candidates, scope, nav_path) = plan(
            &src,
            &co,
            &fixture_files("urls.txt"),
            Some("urls.txt"),
            &[],
            "https://example.com/docs/",
            "docs/",
        )
        .unwrap();
        assert_common(&candidates, &scope);
        assert_eq!(nav_path, "urls.txt");
    }

    #[test]
    fn errors_when_the_navigation_file_is_missing() {
        let (_dir, co) = checkout(&[]);
        let src = source(&resolver_yaml());
        let err = plan(
            &src,
            &co,
            &[],
            None,
            &[],
            "https://example.com/docs/",
            "docs/",
        )
        .unwrap_err();
        assert!(matches!(err, ResolveError::Navigation { .. }), "{err}");
    }
}
