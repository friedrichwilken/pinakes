//! `resolver.type: glob` (SPEC §2.1): select files by `include`, restricted to `extensions`.

use std::collections::BTreeMap;
use std::path::Path;

use globset::GlobSet;

use super::{Candidate, Discovery, Plan, ResolveError, Resolver};
use crate::config::{Source, compile_globs};
use crate::manifest::SelectedBy;
use crate::residue::Rule;
use crate::sources::Checkout;

/// The `glob` resolver's own mechanism: select `include`, filtered by the effective
/// `extensions`, which [`super::resolver_for`] computes once via [`effective_extensions`].
pub(super) struct Glob<'a> {
    pub(super) include: &'a [String],
    pub(super) exclude: &'a [String],
    pub(super) residue_scope: &'a [String],
    pub(super) extensions: Vec<String>,
}

impl Resolver for Glob<'_> {
    fn selected_by(&self) -> SelectedBy {
        SelectedBy::Include
    }

    fn exclude(&self) -> &[String] {
        self.exclude
    }

    fn discover(
        &self,
        _source: &Source,
        _checkout: &Checkout,
        files: &[String],
        _config_dir: &Path,
    ) -> Result<Discovery, ResolveError> {
        let include = compile_globs("include", self.include)?;
        let mut candidates = BTreeMap::new();
        for file in files {
            if include.is_match(file) && matches_extension(file, &self.extensions) {
                candidates.insert(file.clone(), Candidate::bare(file, true));
            }
        }
        let scope = if self.residue_scope.is_empty() {
            include
        } else {
            compile_globs("residue_scope", self.residue_scope)?
        };
        Ok(Discovery {
            candidates,
            exclude: compile_globs("exclude", self.exclude)?,
            scope,
            mention: None,
            nav_path: None,
        })
    }

    fn not_selected(&self, path: &str, _candidate: &Candidate, _plan: &Plan<'_>) -> Rule {
        let include_set =
            compile_globs("include", self.include).unwrap_or_else(|_| GlobSet::empty());
        if include_set.is_match(path) && !matches_extension(path, &self.extensions) {
            Rule::glob_extension(&self.extensions)
        } else {
            Rule::glob_outside_include()
        }
    }
}

/// The `glob` resolver's effective `extensions` (SPEC §2.1): `configured` verbatim when given,
/// else `["md"]`, or every file (an empty list) when the source has a `render` step.
pub(super) fn effective_extensions(configured: Option<&[String]>, has_render: bool) -> Vec<String> {
    match configured {
        Some(extensions) => extensions.to_vec(),
        None if has_render => Vec::new(),
        None => vec!["md".to_string()],
    }
}

/// Whether `path`'s extension (case-insensitive, without the dot) is in `extensions`; an empty
/// list matches every file.
fn matches_extension(path: &str, extensions: &[String]) -> bool {
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
