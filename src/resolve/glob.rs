//! `resolver.type: glob` (SPEC §2.1): select files by `include`, restricted to `extensions`.

use std::collections::BTreeMap;
use std::path::Path;

use super::{Candidate, Discovery, ResolveError};
use crate::config::{Source, compile_globs};

/// The `glob` resolver arm of [`super::resolver_plan`]: populate `candidates` from `include`
/// (filtered by `extensions`) and return the resulting [`Discovery`].
pub(super) fn glob_plan<'a>(
    source: &Source,
    files: &[String],
    include: &[String],
    exclude: &[String],
    residue_scope: &[String],
    extensions: Option<&[String]>,
) -> Result<Discovery<'a>, ResolveError> {
    let include = compile_globs("include", include)?;
    let extensions = effective_extensions(extensions, source.render.is_some());
    let mut candidates = BTreeMap::new();
    for file in files {
        if include.is_match(file) && matches_extension(file, &extensions) {
            candidates.insert(file.clone(), Candidate::bare(file, true));
        }
    }
    let scope = if residue_scope.is_empty() {
        include
    } else {
        compile_globs("residue_scope", residue_scope)?
    };
    Ok(Discovery {
        candidates,
        exclude: compile_globs("exclude", exclude)?,
        scope,
        mention: None,
        extra_include: &[],
        nav_path: None,
    })
}

/// The `glob` resolver's effective `extensions` (SPEC §2.1): `configured` verbatim when given,
/// else `["md"]`, or every file (an empty list) when the source has a `render` step.
pub(crate) fn effective_extensions(configured: Option<&[String]>, has_render: bool) -> Vec<String> {
    match configured {
        Some(extensions) => extensions.to_vec(),
        None if has_render => Vec::new(),
        None => vec!["md".to_string()],
    }
}

/// Whether `path`'s extension (case-insensitive, without the dot) is in `extensions`; an empty
/// list matches every file.
pub(crate) fn matches_extension(path: &str, extensions: &[String]) -> bool {
    if extensions.is_empty() {
        return true;
    }
    let Some(ext) = Path::new(path).extension().and_then(|e| e.to_str()) else {
        return false;
    };
    extensions.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_extensions_defaults_to_markdown_or_every_file_with_render() {
        assert_eq!(effective_extensions(None, false), vec!["md".to_string()]);
        assert_eq!(effective_extensions(None, true), Vec::<String>::new());
        assert_eq!(
            effective_extensions(Some(&["yaml".to_string()]), true),
            vec!["yaml".to_string()],
            "an explicit list wins over the render default"
        );
        assert_eq!(
            effective_extensions(Some(&[]), false),
            Vec::<String>::new(),
            "an explicit empty list means every file even without render"
        );
    }

    #[test]
    fn matches_extension_is_case_insensitive_and_empty_means_everything() {
        assert!(matches_extension("a/b.MD", &["md".to_string()]));
        assert!(!matches_extension("a/b.txt", &["md".to_string()]));
        assert!(matches_extension("a/b.txt", &[]));
        assert!(!matches_extension("a/b", &["md".to_string()]));
    }
}
