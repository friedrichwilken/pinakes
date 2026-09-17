//! `resolver.type: mdbook` (SPEC §12): the nested link list in `src/SUMMARY.md`. Part titles
//! (`# Part`) become the section; chapters without a link (drafts) are skipped.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;

use globset::GlobSet;
use regex::Regex;

use super::navigation::{self, LinkTarget};
use super::{Candidate, Discovery, Plan, ResolveError, Resolver, read_file};
use crate::config::Source;
use crate::residue::Rule;
use crate::sources::Checkout;

/// Default navigation file when `path` is not given.
const DEFAULT_PATH: &str = "src/SUMMARY.md";

/// The `mdbook` resolver's own mechanism: the nested link list in `src/SUMMARY.md`.
pub(super) struct Mdbook<'a> {
    pub(super) path: Option<&'a str>,
    pub(super) scope: &'a [String],
    pub(super) include: &'a [String],
    pub(super) exclude: &'a [String],
}

impl Resolver for Mdbook<'_> {
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

    fn not_selected(&self, _path: &str, _candidate: &Candidate, plan: &Plan<'_>) -> Rule {
        Rule::mdbook_unlinked(plan.nav_path.as_deref().unwrap_or_default())
    }
}

/// Build the candidate map, residue scope and navigation file path for an `mdbook` source.
pub(super) fn plan(
    source: &Source,
    checkout: &Checkout,
    files: &[String],
    path: Option<&str>,
    scope: &[String],
) -> Result<(BTreeMap<String, Candidate>, GlobSet, String), ResolveError> {
    let nav_path = path.unwrap_or(DEFAULT_PATH);
    if !files.iter().any(|f| f == nav_path) {
        return Err(ResolveError::Navigation {
            name: source.name.clone(),
            message: format!("navigation file {nav_path:?} not found"),
        });
    }
    let text = read_file(checkout, nav_path)?;
    let base_dir = nav_path.rsplit_once('/').map_or("", |(dir, _)| dir);
    let file_set: BTreeSet<String> = files.iter().cloned().collect();

    let mut candidates = BTreeMap::new();
    let mut section = String::new();
    let mut first_heading = true;
    let mut in_fence = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(heading) = trimmed.strip_prefix("# ") {
            let heading = heading.trim();
            section = if first_heading && heading.eq_ignore_ascii_case("summary") {
                String::new()
            } else {
                heading.to_string()
            };
            first_heading = false;
            continue;
        }
        for (title, target) in link_regex().captures_iter(line).map(|c| {
            (
                c.get(1).map_or(String::new(), |m| m.as_str().to_string()),
                c.get(2).map_or(String::new(), |m| m.as_str().to_string()),
            )
        }) {
            if target.trim().is_empty() {
                continue; // A draft chapter with no link: skipped entirely (SPEC §12).
            }
            let doc_type = navigation::doc_type_from_section(&section);
            let candidate = |path: String, selected: bool| Candidate {
                path: path.clone(),
                title: title.clone(),
                doc_type: doc_type.to_string(),
                section: section.clone(),
                selected,
                context: String::new(),
                rule: None,
            };
            match navigation::resolve_link(base_dir, &target, &file_set) {
                LinkTarget::Resolved(p) => {
                    candidates.insert(p.clone(), candidate(p, true));
                }
                LinkTarget::Unresolved(p) => {
                    candidates.insert(p.clone(), candidate(p, false));
                }
                LinkTarget::Skipped => {}
            }
        }
    }
    let scope_set = navigation::scope_set(base_dir, scope)?;
    Ok((candidates, scope_set, nav_path.to_string()))
}

/// A Markdown link, `[title](target)`; tolerant of an empty target (a draft chapter).
fn link_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[([^\]]*)\]\(([^)]*)\)").expect("valid regex"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::fs;

    fn source(resolver_yaml: &str) -> Source {
        let text = format!(
            "version: 1\nsources:\n  - name: book\n    repo: https://github.com/o/r\n    ref: main\n    \
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

    const SUMMARY: &str = "\
# Summary

[Introduction](introduction.md)

# Getting Started

- [Installation](getting-started/installation.md)
- [Configuration](getting-started/configuration.md)
  - [Advanced Options](getting-started/advanced-options.md)

# Reference

- [CLI Reference](reference/cli.md)
- [Draft Chapter]()
- [Missing Page](reference/missing.md)
";

    fn fixture_files() -> Vec<String> {
        vec![
            "src/SUMMARY.md".to_string(),
            "src/introduction.md".to_string(),
            "src/getting-started/installation.md".to_string(),
            "src/getting-started/configuration.md".to_string(),
            "src/getting-started/advanced-options.md".to_string(),
            "src/reference/cli.md".to_string(),
            "src/orphan.md".to_string(),
        ]
    }

    #[test]
    fn plans_parts_nesting_and_skips_drafts() {
        let (_dir, co) = checkout(&[
            ("src/SUMMARY.md", SUMMARY),
            ("src/introduction.md", "# Introduction\n"),
            ("src/getting-started/installation.md", "# Installation\n"),
            ("src/getting-started/configuration.md", "# Configuration\n"),
            (
                "src/getting-started/advanced-options.md",
                "# Advanced Options\n",
            ),
            ("src/reference/cli.md", "# CLI Reference\n"),
            ("src/orphan.md", "# Orphan\n"),
        ]);
        let src = source("      type: mdbook\n");
        let (candidates, scope, nav_path) = plan(&src, &co, &fixture_files(), None, &[]).unwrap();

        let intro = &candidates["src/introduction.md"];
        assert_eq!(
            intro.section, "",
            "content before the first part has no section"
        );
        assert_eq!(intro.doc_type, "concept");

        let install = &candidates["src/getting-started/installation.md"];
        assert_eq!(install.section, "Getting Started");
        assert_eq!(install.doc_type, "tutorial");
        let advanced = &candidates["src/getting-started/advanced-options.md"];
        assert_eq!(
            advanced.section, "Getting Started",
            "nested item, same part"
        );

        let cli = &candidates["src/reference/cli.md"];
        assert_eq!(cli.section, "Reference");
        assert_eq!(cli.doc_type, "reference");

        assert!(
            !candidates.values().any(|c| c.title == "Draft Chapter"),
            "a draft with no link is skipped entirely"
        );
        let missing = &candidates["src/reference/missing.md"];
        assert!(!missing.selected, "dangling link is unresolved");
        assert_eq!(missing.title, "Missing Page");

        assert!(!candidates.contains_key("src/orphan.md"));
        assert!(scope.is_match("src/orphan.md"));
        assert!(!scope.is_match("other/x.md"));
        assert_eq!(nav_path, "src/SUMMARY.md");
    }

    #[test]
    fn errors_when_the_navigation_file_is_missing() {
        let (_dir, co) = checkout(&[]);
        let src = source("      type: mdbook\n");
        let err = plan(&src, &co, &[], None, &[]).unwrap_err();
        assert!(matches!(err, ResolveError::Navigation { .. }), "{err}");
    }
}
