//! Text preparation: content cleaning, title extraction and splitting a page into retrieval
//! units (SPEC §5).

use std::sync::LazyLock;

use regex::Regex;

pub use crate::text::{clean_content, extract_title};
use crate::tokenizer::tokenize;

/// H2 sections with more tokens than this are split at H3.
pub const SECTION_SPLIT_TOKENS: usize = 1200;

static HTML_TAG: LazyLock<Regex> = LazyLock::new(|| regex(r"</?[a-zA-Z][^>]*>"));
static MD_IMAGE: LazyLock<Regex> = LazyLock::new(|| regex(r"!\[([^\]]*)\]\([^)]*\)"));
static MD_LINK: LazyLock<Regex> = LazyLock::new(|| regex(r"\[([^\]]*)\]\([^)]*\)"));
static H2: LazyLock<Regex> = LazyLock::new(|| regex(r"^##\s+(.+?)\s*#*\s*$"));
static H3: LazyLock<Regex> = LazyLock::new(|| regex(r"^###\s+(.+?)\s*#*\s*$"));

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static pattern is valid")
}

/// Reduce cleaned content to the text worth indexing: link and image targets replaced by their
/// labels, HTML tags by a space.
pub fn index_text(text: &str) -> String {
    let text = MD_IMAGE.replace_all(text, "$1");
    let text = MD_LINK.replace_all(&text, "$1");
    HTML_TAG.replace_all(&text, " ").into_owned()
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
