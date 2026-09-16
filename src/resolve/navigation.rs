//! Shared machinery for the navigation-based resolvers (SPEC §12): `vitepress`, `docusaurus`,
//! `mdbook` and `sitemap`.
//!
//! This module holds three things every one of them needs: a tolerant scanner for the
//! JavaScript/TypeScript object literals `vitepress` and `docusaurus` read their navigation
//! from, a link-to-repository-path resolver shared by the file-based formats, and the single
//! `doc_type` heuristic (SPEC §2.4 / §12) so troubleshooting/tutorial/reference/release-notes/
//! concept classification is written once and reused everywhere a navigation section decides it.

use std::collections::BTreeSet;

use globset::GlobSet;

use crate::config::{ConfigError, compile_globs};

/// One page a navigation file links: its own title (empty when the navigation gives none),
/// the raw link or id text (interpretation is up to the caller), and the section breadcrumb
/// built from enclosing groups, joined by `" > "`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct NavEntry {
    /// Title from the navigation, if any.
    pub title: String,
    /// The raw link or doc id, not yet resolved to a repository path.
    pub target: String,
    /// Section breadcrumb, e.g. `"Guide"` or `"Guide > Advanced"`; empty at the top level.
    pub section: String,
}

/// Where a navigation link points, once anchors are stripped and the target is resolved
/// against the files that actually exist in the checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LinkTarget {
    /// An existing file at this repository-relative path.
    Resolved(String),
    /// No existing file matches; this is the best-guess path to report as unresolved.
    Unresolved(String),
    /// Not a repository-relative reference at all (an external URL, a bare anchor, or an
    /// empty link): neither a page nor residue.
    Skipped,
}

/// Classify a navigation section title into a `doc_type` (SPEC §2.4, reused by §12).
///
/// Matching is a case-insensitive substring search over a small vocabulary; a section that
/// matches nothing, including an empty one, is `"concept"`. Order matters only where the
/// vocabulary could otherwise overlap (e.g. a "Release Guide" section reads as release notes
/// before it reads as a tutorial).
pub(super) fn doc_type_from_section(section: &str) -> &'static str {
    let lower = section.to_lowercase();
    let has_any = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    if has_any(&["troubleshoot", "faq", "known issue", "debugging"]) {
        "troubleshooting"
    } else if has_any(&[
        "release",
        "changelog",
        "change log",
        "what's new",
        "whats new",
    ]) {
        "release-notes"
    } else if has_any(&[
        "tutorial",
        "getting started",
        "quickstart",
        "quick start",
        "how to",
        "howto",
        "guide",
    ]) {
        "tutorial"
    } else if has_any(&[
        "reference",
        "api",
        "cli",
        "configuration",
        "schema",
        "specification",
    ]) {
        "reference"
    } else {
        "concept"
    }
}

/// Whether `path` has a `.md` or `.mdx` extension, compared case-insensitively.
pub(super) fn is_markdown_extension(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("mdx"))
}

/// Join a base directory and a relative link into a single posix path, collapsing `.` and
/// `..` segments; the result never climbs above `base`'s own root.
pub(super) fn join_path(base: &str, relative: &str) -> String {
    let mut parts: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    for segment in relative.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// Resolve a navigation `link` found in a file whose content root is `base_dir` against the
/// files that exist in the checkout.
///
/// Anchors (`#…`) and query strings (`?…`) are stripped first. A link starting with `/` is
/// taken as root-relative to `base_dir` (`VitePress` sidebar convention); anything else is
/// relative to `base_dir` directly. A link that already ends in `.md`/`.mdx` is checked as
/// given; otherwise `<link>.md`, `<link>.mdx`, `<link>/README.md` and `<link>/index.md` are
/// tried in that order, so an extension-less link, a directory link and a link to `/` all
/// resolve the way a static site generator would serve them.
pub(super) fn resolve_link(base_dir: &str, link: &str, files: &BTreeSet<String>) -> LinkTarget {
    let link = link.trim();
    if link.is_empty()
        || link.starts_with("http://")
        || link.starts_with("https://")
        || link.starts_with("//")
        || link.starts_with("mailto:")
    {
        return LinkTarget::Skipped;
    }
    let without_suffix = link.split(['#', '?']).next().unwrap_or("");
    if without_suffix.is_empty() {
        return LinkTarget::Skipped;
    }
    let relative = without_suffix.strip_prefix('/').unwrap_or(without_suffix);
    let joined = join_path(base_dir, relative);

    let candidates: Vec<String> = if is_markdown_extension(&joined) {
        vec![joined.clone()]
    } else if joined.is_empty() {
        vec!["README.md".to_string(), "index.md".to_string()]
    } else {
        vec![
            format!("{joined}.md"),
            format!("{joined}.mdx"),
            format!("{joined}/README.md"),
            format!("{joined}/index.md"),
        ]
    };
    match candidates.iter().find(|c| files.contains(c.as_str())) {
        Some(found) => LinkTarget::Resolved(found.clone()),
        None => LinkTarget::Unresolved(
            candidates
                .into_iter()
                .next()
                .unwrap_or_else(|| format!("{joined}.md")),
        ),
    }
}

/// The default residue scope, `<dir>/**/*.md` (or `**/*.md` at the repository root), unless
/// `custom` overrides it.
pub(super) fn scope_set(default_dir: &str, custom: &[String]) -> Result<GlobSet, ConfigError> {
    if custom.is_empty() {
        let pattern = if default_dir.is_empty() {
            "**/*.md".to_string()
        } else {
            format!("{default_dir}/**/*.md")
        };
        compile_globs("scope", std::slice::from_ref(&pattern))
    } else {
        compile_globs("scope", custom)
    }
}

/// A JavaScript/TypeScript value, as tolerated by [`parse_js_value`]: enough structure to walk
/// a sidebar configuration and nothing more. Numbers, booleans, identifiers, template literals
/// and anything else that is not an object, array or quoted string collapses to [`JsValue::Other`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum JsValue {
    /// `{ key: value, … }`, in source order; unquoted and quoted keys are both accepted.
    Object(Vec<(String, JsValue)>),
    /// `[ value, … ]`.
    Array(Vec<JsValue>),
    /// A single- or double-quoted string, with `\'`, `\"`, `\\`, `\n` and `\t` unescaped.
    Str(String),
    /// Anything else.
    Other,
}

impl JsValue {
    /// The value of `key` in an object; `None` for any other value or a missing key.
    pub(super) fn get(&self, key: &str) -> Option<&JsValue> {
        match self {
            JsValue::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// This value as a string, if it is one.
    pub(super) fn as_str(&self) -> Option<&str> {
        match self {
            JsValue::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// Parse the first top-level object or array literal in `text`, skipping over `import`
/// statements, `export default` and `module.exports =` prefixes.
///
/// This is not a JavaScript parser: it is a tolerant scanner tuned to the shapes sidebar
/// configuration files use (nested object and array literals, string keys and values,
/// `//` and `/* */` comments, trailing commas), good enough to walk a sidebar tree and nothing
/// more. It never panics: unrecognised input is skipped a character at a time until the
/// scanner can make progress again.
pub(super) fn parse_js_value(text: &str) -> JsValue {
    let mut parser = JsParser {
        chars: text.chars().collect(),
        pos: 0,
    };
    loop {
        parser.skip_trivia();
        match parser.peek() {
            Some('{' | '[') => break,
            Some(_) => parser.pos += 1,
            None => return JsValue::Other,
        }
    }
    parser.parse_value()
}

struct JsParser {
    chars: Vec<char>,
    pos: usize,
}

impl JsParser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek2(&self) -> Option<char> {
        self.chars.get(self.pos + 1).copied()
    }

    fn skip_trivia(&mut self) {
        loop {
            while matches!(self.peek(), Some(c) if c.is_whitespace()) {
                self.pos += 1;
            }
            if self.peek() == Some('/') && self.peek2() == Some('/') {
                while !matches!(self.peek(), None | Some('\n')) {
                    self.pos += 1;
                }
                continue;
            }
            if self.peek() == Some('/') && self.peek2() == Some('*') {
                self.pos += 2;
                while self.pos < self.chars.len()
                    && !(self.peek() == Some('*') && self.peek2() == Some('/'))
                {
                    self.pos += 1;
                }
                self.pos = (self.pos + 2).min(self.chars.len());
                continue;
            }
            break;
        }
    }

    fn parse_value(&mut self) -> JsValue {
        self.skip_trivia();
        match self.peek() {
            Some('{') => self.parse_object(),
            Some('[') => self.parse_array(),
            Some('\'' | '"') => JsValue::Str(self.parse_string()),
            Some(_) => self.parse_other(),
            None => JsValue::Other,
        }
    }

    fn parse_object(&mut self) -> JsValue {
        self.pos += 1; // consume '{'
        let mut fields = Vec::new();
        loop {
            self.skip_trivia();
            match self.peek() {
                Some('}') => {
                    self.pos += 1;
                    break;
                }
                None => break,
                Some('\'' | '"') => {
                    let key = self.parse_string();
                    self.skip_trivia();
                    if self.peek() == Some(':') {
                        self.pos += 1;
                        fields.push((key, self.parse_value()));
                    }
                }
                _ => {
                    let key = self.parse_bare();
                    self.skip_trivia();
                    if key.is_empty() {
                        // Nothing recognisable here; force progress so we never spin.
                        if self.peek().is_some() {
                            self.pos += 1;
                        } else {
                            break;
                        }
                        continue;
                    }
                    if self.peek() == Some(':') {
                        self.pos += 1;
                        fields.push((key, self.parse_value()));
                    }
                }
            }
            self.skip_trivia();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some('}') => {
                    self.pos += 1;
                    break;
                }
                _ => {
                    if self.peek().is_none() {
                        break;
                    }
                }
            }
        }
        JsValue::Object(fields)
    }

    fn parse_array(&mut self) -> JsValue {
        self.pos += 1; // consume '['
        let mut items = Vec::new();
        loop {
            self.skip_trivia();
            match self.peek() {
                Some(']') => {
                    self.pos += 1;
                    break;
                }
                None => break,
                _ => items.push(self.parse_value()),
            }
            self.skip_trivia();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some(']') => {
                    self.pos += 1;
                    break;
                }
                _ => {
                    if self.peek().is_none() {
                        break;
                    }
                }
            }
        }
        JsValue::Array(items)
    }

    fn parse_string(&mut self) -> String {
        let quote = self.peek().unwrap_or('"');
        self.pos += 1;
        let mut out = String::new();
        while let Some(c) = self.peek() {
            self.pos += 1;
            if c == quote {
                break;
            }
            if c == '\\' {
                let Some(escaped) = self.peek() else { break };
                self.pos += 1;
                match escaped {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    other => out.push(other),
                }
                continue;
            }
            out.push(c);
        }
        out
    }

    /// An unquoted object key: everything up to the next structural character or whitespace.
    fn parse_bare(&mut self) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if matches!(c, ':' | ',' | '}' | ']' | '{' | '[') || c.is_whitespace() {
                break;
            }
            out.push(c);
            self.pos += 1;
        }
        out
    }

    /// A bare value we do not care about (a number, boolean, identifier, function call, …):
    /// skip to the next top-level separator.
    fn parse_other(&mut self) -> JsValue {
        while let Some(c) = self.peek() {
            if matches!(c, ',' | '}' | ']') {
                break;
            }
            self.pos += 1;
        }
        JsValue::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn doc_type_heuristic_covers_every_category() {
        assert_eq!(doc_type_from_section("Troubleshooting"), "troubleshooting");
        assert_eq!(doc_type_from_section("FAQ"), "troubleshooting");
        assert_eq!(doc_type_from_section("Release Notes"), "release-notes");
        assert_eq!(doc_type_from_section("Changelog"), "release-notes");
        assert_eq!(doc_type_from_section("Getting Started"), "tutorial");
        assert_eq!(doc_type_from_section("Guide > Advanced"), "tutorial");
        assert_eq!(doc_type_from_section("API Reference"), "reference");
        assert_eq!(doc_type_from_section("Configuration"), "reference");
        assert_eq!(doc_type_from_section("Concepts"), "concept");
        assert_eq!(doc_type_from_section(""), "concept");
        assert_eq!(
            doc_type_from_section("Release Guide"),
            "release-notes",
            "release beats tutorial when both match"
        );
    }

    #[test]
    fn join_path_collapses_dot_segments_without_escaping_the_root() {
        assert_eq!(join_path("docs", "guide/x"), "docs/guide/x");
        assert_eq!(
            join_path("docs/guide", "../reference/y"),
            "docs/reference/y"
        );
        assert_eq!(
            join_path("docs", "../../x"),
            "x",
            "extra .. beyond the root is capped, not escaped"
        );
        assert_eq!(join_path("", "x"), "x");
    }

    #[test]
    fn resolve_link_handles_every_shape() {
        let files = files(&[
            "docs/guide/start.md",
            "docs/guide/README.md",
            "docs/reference/api.md",
        ]);
        assert_eq!(
            resolve_link("docs", "/guide/start", &files),
            LinkTarget::Resolved("docs/guide/start.md".to_string()),
            "link without an extension"
        );
        assert_eq!(
            resolve_link("docs", "/guide/start#install", &files),
            LinkTarget::Resolved("docs/guide/start.md".to_string()),
            "anchor is stripped"
        );
        assert_eq!(
            resolve_link("docs", "/guide/", &files),
            LinkTarget::Resolved("docs/guide/README.md".to_string()),
            "directory link falls back to README.md"
        );
        assert_eq!(
            resolve_link("docs", "reference/api.md", &files),
            LinkTarget::Resolved("docs/reference/api.md".to_string()),
            "already has the extension"
        );
        assert_eq!(
            resolve_link("docs", "/guide/missing", &files),
            LinkTarget::Unresolved("docs/guide/missing.md".to_string())
        );
        assert_eq!(resolve_link("docs", "", &files), LinkTarget::Skipped);
        assert_eq!(
            resolve_link("docs", "https://example.com/x", &files),
            LinkTarget::Skipped
        );
        assert_eq!(resolve_link("docs", "#top", &files), LinkTarget::Skipped);
    }

    #[test]
    fn scope_set_defaults_to_the_content_directory() {
        let set = scope_set("docs", &[]).unwrap();
        assert!(set.is_match("docs/guide/x.md"));
        assert!(!set.is_match("other/x.md"));
        let root = scope_set("", &[]).unwrap();
        assert!(root.is_match("x.md"));
        let custom = scope_set("docs", &["only/**/*.md".to_string()]).unwrap();
        assert!(custom.is_match("only/x.md"));
        assert!(!custom.is_match("docs/x.md"));
    }

    #[test]
    fn js_value_parses_objects_arrays_and_escaped_strings() {
        let value = parse_js_value(
            r#"export default {
                // a comment
                sidebar: [
                    { text: 'Say \'Hi\'', link: '/a' },
                    { text: "Say \"Hi\"", items: [ { text: 'Nested', link: '/b' } ] },
                ],
            }"#,
        );
        let sidebar = value.get("sidebar").expect("sidebar key");
        let JsValue::Array(items) = sidebar else {
            panic!("expected array")
        };
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0].get("text").and_then(JsValue::as_str),
            Some("Say 'Hi'")
        );
        assert_eq!(
            items[1].get("text").and_then(JsValue::as_str),
            Some("Say \"Hi\"")
        );
        let nested = items[1].get("items").unwrap();
        let JsValue::Array(nested_items) = nested else {
            panic!("expected array")
        };
        assert_eq!(
            nested_items[0].get("link").and_then(JsValue::as_str),
            Some("/b")
        );
    }

    #[test]
    fn js_value_never_panics_on_malformed_input() {
        for text in [
            "",
            "{",
            "[",
            "{{{{{",
            "not js at all",
            "{ key: }",
            "[ , , ]",
        ] {
            let _ = parse_js_value(text);
        }
    }
}
