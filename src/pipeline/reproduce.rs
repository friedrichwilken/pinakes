//! Replaying a committed manifest: re-fetch every source at its recorded commit, re-render and
//! rebuild residue where needed, and hand the result to [`super::outputs::write_outputs`].

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::RepoSlug;
use crate::layout::ARTIFACT_VERSION;
use crate::manifest::{Manifest, ManifestSource, PageEntry, SelectedBy};
use crate::page::PageRegistry;
use crate::render;
use crate::residue::{self, Reason, ResidueEntry};
use crate::sources::{Checkout, Fetcher, fetch_checkout};
use crate::text::{sha256_hex, strip_frontmatter, title_of};
use crate::workspace::Paths;

use super::outputs::write_outputs;
use super::{CommandError, ResolveOutcome, checkout_dir, io_err};

pub(super) fn reproduce(
    paths: &Paths,
    manifest_path: &Path,
    fetcher: &dyn Fetcher,
    work: &Path,
) -> Result<ResolveOutcome, CommandError> {
    let mut manifest = Manifest::load(manifest_path)?;
    // The artifact and manifest written below have this build's shape, whatever the loaded
    // manifest was written under (SPEC §2.8).
    manifest.artifact_version = ARTIFACT_VERSION;
    let mut checkouts = BTreeMap::new();
    let mut all_residue = Vec::new();
    for (name, source) in &manifest.sources {
        let slug = RepoSlug::from_slug(&source.repo).ok_or_else(|| CommandError::BadSlug {
            name: name.clone(),
            repo: source.repo.clone(),
        })?;
        let dest = checkout_dir(work, name);
        let checkout = fetch_checkout(fetcher, &slug, &source.commit, &dest).map_err(|e| {
            CommandError::Source {
                name: name.clone(),
                source: e,
            }
        })?;
        if checkout.commit != source.commit {
            return Err(CommandError::CommitMismatch {
                name: name.clone(),
                expected: source.commit.clone(),
                actual: checkout.commit,
            });
        }
        if let Some(render) = &source.render {
            let selected = selected_for_render(source);
            render::render_source(name, render, &checkout, &selected, &paths.config_dir())?;
        }
        all_residue.extend(recorded_residue(name, source, &checkout)?);
        checkouts.insert(name.clone(), checkout);
    }
    let mut registry = PageRegistry::from_resolve(&manifest, &all_residue);
    let duplicates = write_outputs(paths, &manifest, &mut registry, &checkouts)?;
    Ok(ResolveOutcome {
        manifest,
        residue: all_residue,
        expired: Vec::new(),
        warnings: Vec::new(),
        duplicates,
        registry,
    })
}

/// Rebuild the pre-render selection [`render::render_source`] needs from a recorded source's
/// post-render pages and its `unrendered` list: one synthetic entry per distinct selected path
/// (`rendered_from`, plus every `unrendered` path), so `resolve --from-manifest` can re-run a
/// render step without the original config's resolver at hand. Only `selected_by` survives into
/// a rendered page's manifest entry, so the other fields are left blank.
fn selected_for_render(source: &ManifestSource) -> BTreeMap<String, PageEntry> {
    let mut selected = BTreeMap::new();
    let blank = |selected_by| PageEntry {
        sha256: String::new(),
        title: String::new(),
        doc_type: String::new(),
        section: String::new(),
        selected_by,
        rendered_from: None,
    };
    for entry in source.pages.values() {
        if let Some(source_path) = &entry.rendered_from {
            selected
                .entry(source_path.clone())
                .or_insert_with(|| blank(entry.selected_by));
        }
    }
    for path in &source.unrendered {
        selected
            .entry(path.clone())
            .or_insert_with(|| blank(SelectedBy::Include));
    }
    selected
}

/// Rebuild residue entries for a recorded source from the files in its checkout.
fn recorded_residue(
    name: &str,
    source: &ManifestSource,
    checkout: &Checkout,
) -> Result<Vec<ResidueEntry>, CommandError> {
    let mut entries = Vec::new();
    for path in &source.residue {
        let full = checkout.root.join(path);
        let bytes = std::fs::read(&full).map_err(io_err(&full))?;
        let text = String::from_utf8_lossy(&bytes);
        entries.push(ResidueEntry {
            id: crate::manifest::page_id(name, path),
            source: name.to_string(),
            path: path.clone(),
            reason: Reason::NotSelected,
            sha256: sha256_hex(&bytes),
            title: title_of("", &text),
            excerpt: residue::excerpt(strip_frontmatter(&text), residue::EXCERPT_TOKENS),
            context: String::new(),
            url: source.page_url(path).unwrap_or_default(),
            rule: Some(residue::Rule::reproduced()),
        });
    }
    for path in &source.unresolved {
        entries.push(ResidueEntry {
            id: crate::manifest::page_id(name, path),
            source: name.to_string(),
            path: path.clone(),
            reason: Reason::UnresolvedLink,
            sha256: String::new(),
            title: String::new(),
            excerpt: String::new(),
            context: String::new(),
            url: source.page_url(path).unwrap_or_default(),
            rule: Some(residue::Rule::reproduced()),
        });
    }
    Ok(entries)
}
