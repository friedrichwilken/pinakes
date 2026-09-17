//! Small text helpers shared across the crate: content hashing, YAML front matter, page titles
//! and command-argument paths.
//!
//! This module depends on nothing else in the crate, so every other module may use it.

use std::path::Path;

use sha2::{Digest, Sha256};

/// Make `arg` absolute when it names an existing path relative to `base`.
pub(crate) fn absolutise(base: &Path, arg: &str) -> String {
    let candidate = base.join(arg);
    if !Path::new(arg).is_absolute() && arg.contains(['/', '\\']) && candidate.exists() {
        std::path::absolute(&candidate)
            .unwrap_or(candidate)
            .to_string_lossy()
            .into_owned()
    } else {
        arg.to_string()
    }
}

/// Hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// The text after a leading YAML frontmatter block, if any.
pub fn strip_frontmatter(content: &str) -> &str {
    let Some(rest) = content.strip_prefix("---") else {
        return content;
    };
    if !rest.starts_with('\n') && !rest.starts_with("\r\n") {
        return content;
    }
    let mut offset = 3;
    for line in rest.split_inclusive('\n') {
        offset += line.len();
        if line.trim_end() == "---" && offset > 4 {
            return &content[offset..];
        }
    }
    content
}

/// The `title:` value of a leading frontmatter block.
pub fn frontmatter_title(content: &str) -> Option<String> {
    let rest = content.strip_prefix("---")?;
    let body_start = strip_frontmatter(content);
    if std::ptr::eq(body_start, content) {
        return None;
    }
    let block = &rest[..rest.len() - body_start.len()];
    block.lines().find_map(|line| {
        let value = line.strip_prefix("title:")?.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        (!value.is_empty()).then(|| value.to_string())
    })
}

/// The first ATX H1 (`# Title`) outside frontmatter and fenced code blocks.
pub fn first_h1(content: &str) -> Option<String> {
    let mut in_fence = false;
    for line in strip_frontmatter(content).lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(title) = trimmed.strip_prefix("# ") {
            let title = title.trim().trim_end_matches('#').trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }
    None
}

/// Title fallback chain: `nav_title`, then the first H1, then frontmatter `title:`, else empty.
pub fn title_of(nav_title: &str, content: &str) -> String {
    if !nav_title.trim().is_empty() {
        return nav_title.trim().to_string();
    }
    first_h1(content)
        .or_else(|| frontmatter_title(content))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_fallback_chain() {
        assert_eq!(title_of("Nav", "# H1\n"), "Nav");
        assert_eq!(title_of("", "---\ntitle: FM\n---\n\n# H1 #\n"), "H1");
        assert_eq!(
            title_of("", "---\ntitle: \"Quoted FM\"\n---\nbody\n"),
            "Quoted FM"
        );
        assert_eq!(title_of("", "```\n# not a title\n```\n\n## only h2\n"), "");
        assert_eq!(title_of("", "--- not frontmatter\ntitle: x\n"), "");
        assert_eq!(strip_frontmatter("---\na: 1\n---\nbody"), "body");
        assert_eq!(
            strip_frontmatter("---\nunterminated\n"),
            "---\nunterminated\n"
        );
        assert_eq!(frontmatter_title("---\n---\n# x"), None);
        assert_eq!(first_h1("#nospace\n# Real\n"), Some("Real".to_string()));
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
