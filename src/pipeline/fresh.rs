//! Resolving a config from scratch: fetch every source, apply the archived policy, resolve and
//! render its pages, and hand the result to [`super::outputs::write_outputs`].

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::{ArchivedPolicy, Config};
use crate::decisions;
use crate::manifest::{Manifest, ManifestSource, now_rfc3339};
use crate::page::PageRegistry;
use crate::render;
use crate::residue::Reason;
use crate::resolve::{self, ResolveContext};
use crate::sources::{Fetcher, fetch_checkout};
use crate::workspace::Paths;

use super::outputs::write_outputs;
use super::{CommandError, ResolveOptions, ResolveOutcome, checkout_dir};

pub(super) fn resolve_fresh(
    paths: &Paths,
    options: &ResolveOptions,
    fetcher: &dyn Fetcher,
    work: &Path,
) -> Result<ResolveOutcome, CommandError> {
    let config = Config::load(&paths.config)?;
    let deny = config.deny_set()?;
    let all_decisions = decisions::read_jsonl(&paths.decisions)?;
    let effective = decisions::effective(&all_decisions);
    let previous = if paths.manifest.is_file() {
        Some(Manifest::load(&paths.manifest)?)
    } else {
        None
    };
    let config_dir = paths.config_dir();

    let mut manifest = Manifest::new(options.generated_at.clone().unwrap_or_else(now_rfc3339));
    let mut all_residue = Vec::new();
    let mut warnings = Vec::new();
    let mut checkouts = BTreeMap::new();

    for source in &config.sources {
        let slug = source.slug();
        let archived = fetcher.archived(&slug);
        let dropped = archived == Some(true) && config.policy.archived == ArchivedPolicy::Drop;
        if archived == Some(true) {
            if dropped {
                warnings.push(format!("{}: repository is archived; dropped", source.name));
            } else {
                warnings.push(format!("{}: repository is archived", source.name));
            }
        }
        let dest = checkout_dir(work, &source.name);
        let checkout = fetch_checkout(fetcher, &slug, &source.git_ref, &dest).map_err(|e| {
            CommandError::Source {
                name: source.name.clone(),
                source: e,
            }
        })?;
        let ctx = ResolveContext {
            deny: &deny,
            deny_patterns: &config.policy.deny,
            decisions: &effective,
            config_dir: &config_dir,
            is_new_source: previous
                .as_ref()
                .is_some_and(|p| !p.sources.contains_key(&source.name)),
        };
        let resolved = resolve::resolve_source(source, &checkout, &ctx)?;
        if dropped {
            // The whole source is left out of the corpus; its would-be pages are still
            // accounted for, as residue with `source:archived` (SPEC §2.1, §2.4).
            all_residue.extend(resolve::archived_residue(source, &checkout, resolved));
            continue;
        }
        let residue_paths = resolved
            .residue
            .iter()
            .filter(|r| r.reason != Reason::UnresolvedLink)
            .map(|r| r.path.clone())
            .collect();
        let (pages, unrendered, render) = match &source.render {
            Some(render) => {
                let output = render::render_source(
                    &source.name,
                    render,
                    &checkout,
                    &resolved.pages,
                    &config_dir,
                )?;
                (output.pages, output.unrendered, Some(output.recorded))
            }
            None => (resolved.pages, Vec::new(), None),
        };
        manifest.sources.insert(
            source.name.clone(),
            ManifestSource {
                repo: slug.to_string(),
                repo_url: source.repo.clone(),
                git_ref: source.git_ref.clone(),
                commit: checkout.commit.clone(),
                archived,
                resolver: source.resolver.kind().to_string(),
                pages,
                residue: residue_paths,
                unresolved: resolved.unresolved,
                unrendered,
                render,
            },
        );
        all_residue.extend(resolved.residue);
        checkouts.insert(source.name.clone(), checkout);
    }

    let mut registry = PageRegistry::from_resolve(&manifest, &all_residue);
    let expired = decisions::expired(&effective, |id| registry.get(id).map(|r| r.sha256.clone()));
    let duplicates = write_outputs(paths, &manifest, &mut registry, &checkouts)?;
    Ok(ResolveOutcome {
        manifest,
        residue: all_residue,
        expired,
        warnings,
        duplicates,
        registry,
    })
}
