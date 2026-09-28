use std::collections::BTreeSet;

use crate::artifact;
use crate::config::{Config, QuestionsSettings};
use crate::derive;
use crate::error::CommandError;
use crate::index::{self, Priorities};
use crate::layout::MANIFEST_FILE;
use crate::llm::{ChatTransport, LlmConfig};
use crate::manifest::{DERIVED_QUESTIONS, DerivedEntry, Manifest};
use crate::pipeline::outputs::artifact_page_bytes;
use crate::text::sha256_hex;
use crate::workspace::Paths;

/// Options for `derive` (SPEC §14.4).
#[derive(Debug, Clone, Default)]
pub struct DeriveOptions {
    /// `--model`; falls back to `PINAKES_LLM_MODEL`.
    pub model: Option<String>,
    /// List the pages that would be asked about, without asking and without writing.
    pub dry_run: bool,
}

/// What `derive` did.
#[derive(Debug, Default)]
pub struct DeriveOutcome {
    /// Pages of sources that configure `derive.questions`.
    pub considered: usize,
    /// Of those, pages whose stored questions are still fresh.
    pub fresh: usize,
    /// Pages that got questions in this run.
    pub derived: usize,
    /// Stale or missing pages for a dry run, or the ones the model gave nothing usable for.
    pub pending: Vec<String>,
    /// Entries removed because their page is gone or its source no longer derives questions.
    pub dropped: usize,
    /// Non-fatal observations.
    pub warnings: Vec<String>,
    /// Whether the manifest and the artifact were rewritten.
    pub written: bool,
}

/// Run `derive`: ask the model for the questions each configured page answers and record them
/// in the manifest and the artifact's `derived.jsonl` (SPEC §14.4).
///
/// Only pages whose recorded `input_sha256` no longer equals the current hash are asked about,
/// so a second run costs nothing, and the model endpoint is only needed when there is
/// something to ask. Pages that are mirrors (SPEC §5) are skipped: they are not searchable. A
/// page whose artifact bytes are not the ones the manifest records is skipped too, since its
/// questions would be filed under a hash it does not have. A reply that is not the questions
/// asked for costs only that page; an endpoint that fails ends the run, keeping what earlier
/// pages earned.
pub fn derive(
    paths: &Paths,
    options: &DeriveOptions,
    transport: &dyn ChatTransport,
) -> Result<DeriveOutcome, CommandError> {
    let config = Config::load(&paths.config)?;
    let mut manifest = Manifest::load(&paths.manifest)?;
    let mut outcome = DeriveOutcome::default();
    if !config
        .sources
        .iter()
        .any(|s| s.derive.as_ref().is_some_and(|d| d.questions.is_some()))
    {
        // Nothing derives any more: that is an error, unless there is stored text to drop.
        if manifest.derived.is_empty() {
            return Err(CommandError::NoDerive);
        }
        let changed = drop_orphans(&mut manifest, &BTreeSet::new(), &mut outcome);
        if changed && !options.dry_run {
            write(paths, &manifest)?;
            outcome.written = true;
        }
        return Ok(outcome);
    }
    let mut pages = index::load_pages(&paths.artifact, &Priorities::from_config(&config))?;
    index::mark_mirrors(&mut pages);

    let work = plan(paths, &config, &manifest, &pages, &mut outcome);
    warn_unknown_sections(&config, &pages, &mut outcome);
    let keep: BTreeSet<String> = pages
        .iter()
        .filter(|p| derive::settings_of(&config, &p.source, &p.section).is_some())
        .map(|p| p.id.clone())
        .collect();
    let mut changed = drop_orphans(&mut manifest, &keep, &mut outcome);

    if options.dry_run {
        outcome.pending = work.iter().map(|w| w.page.id.clone()).collect();
        return Ok(outcome);
    }
    if !work.is_empty() {
        let llm = LlmConfig::from_env(options.model.clone())?;
        let asked = ask(paths, transport, &llm, &mut manifest, work, &mut outcome);
        changed |= asked?;
    }
    if changed {
        write(paths, &manifest)?;
        outcome.written = true;
    }
    Ok(outcome)
}

/// A page to ask the model about: its settings and the hash the answer will be filed under.
struct Work<'a> {
    page: &'a index::Page,
    settings: QuestionsSettings,
    expected: String,
}

/// Count the pages of sources that derive questions and pick the ones with no fresh entry:
/// not mirrors, in the manifest, and whose artifact copy is the page the manifest records.
fn plan<'a>(
    paths: &Paths,
    config: &Config,
    manifest: &Manifest,
    pages: &'a [index::Page],
    outcome: &mut DeriveOutcome,
) -> Vec<Work<'a>> {
    let mut work = Vec::new();
    for page in pages.iter().filter(|p| p.mirror_of.is_none()) {
        let Some(settings) = derive::settings_of(config, &page.source, &page.section) else {
            continue;
        };
        let Some(recorded) = manifest
            .sources
            .get(&page.source)
            .and_then(|s| s.pages.get(&page.path))
        else {
            outcome.warnings.push(format!(
                "{}: in the artifact but not in the manifest; skipped",
                page.id
            ));
            continue;
        };
        outcome.considered += 1;
        let on_disk = artifact_page_bytes(&paths.artifact, &page.id).map(|b| sha256_hex(&b));
        if on_disk.as_deref() != Some(recorded.sha256.as_str()) {
            outcome.warnings.push(format!(
                "{}: the artifact's copy is not the page the manifest records; skipped \
                 (run `pinakes resolve --from-manifest`)",
                page.id
            ));
            continue;
        }
        let expected = derive::input_sha256(&recorded.sha256, &settings);
        let stored = manifest
            .derived
            .get(&page.id)
            .and_then(|kinds| kinds.get(DERIVED_QUESTIONS));
        if stored.is_some_and(|entry| entry.input_sha256 == expected) {
            outcome.fresh += 1;
        } else {
            work.push(Work {
                page,
                settings,
                expected,
            });
        }
    }
    work
}

/// Ask about each page in `work`, recording the answers in `manifest`; whether it changed. A
/// reply that is not the questions asked for costs only its page; a failing endpoint ends the
/// run after the pages before it are saved.
fn ask(
    paths: &Paths,
    transport: &dyn ChatTransport,
    llm: &LlmConfig,
    manifest: &mut Manifest,
    work: Vec<Work<'_>>,
    outcome: &mut DeriveOutcome,
) -> Result<bool, CommandError> {
    let mut changed = false;
    for Work {
        page,
        settings,
        expected,
    } in work
    {
        match derive::questions(transport, llm, &settings, &page.title, &page.content) {
            Ok(text) if !text.is_empty() => {
                manifest.derived.entry(page.id.clone()).or_default().insert(
                    DERIVED_QUESTIONS.to_string(),
                    DerivedEntry {
                        input_sha256: expected,
                        model: llm.model.clone(),
                        text,
                    },
                );
                outcome.derived += 1;
                changed = true;
            }
            Ok(_) => {
                outcome
                    .warnings
                    .push(format!("{}: the model gave no usable question", page.id));
                outcome.pending.push(page.id.clone());
                changed |= remove_questions(manifest, &page.id);
            }
            Err(err @ derive::DeriveError::Reply { .. }) => {
                outcome.warnings.push(format!("{}: {err}", page.id));
                outcome.pending.push(page.id.clone());
                changed |= remove_questions(manifest, &page.id);
            }
            Err(source) => {
                // Keep what earlier pages earned: the next run asks only about the rest.
                if changed {
                    write(paths, manifest)?;
                }
                return Err(CommandError::DeriveFailed {
                    page: page.id.clone(),
                    saved: outcome.derived,
                    source,
                });
            }
        }
    }
    Ok(changed)
}

/// Warn about a `derive.questions.sections` key that names no section of the source's pages:
/// a typo there would otherwise change nothing, silently.
fn warn_unknown_sections(config: &Config, pages: &[index::Page], outcome: &mut DeriveOutcome) {
    for source in &config.sources {
        let Some(questions) = source.derive.as_ref().and_then(|d| d.questions.as_ref()) else {
            continue;
        };
        let present: BTreeSet<&str> = pages
            .iter()
            .filter(|p| p.source == source.name)
            .map(|p| p.section.as_str())
            .collect();
        for section in questions.sections.keys() {
            if !present.contains(section.as_str()) {
                outcome.warnings.push(format!(
                    "{}: derive.questions.sections.{section} matches no page section",
                    source.name
                ));
            }
        }
    }
}

/// Remove derived entries whose page is not in `keep`; the count goes to `outcome`.
fn drop_orphans(
    manifest: &mut Manifest,
    keep: &BTreeSet<String>,
    outcome: &mut DeriveOutcome,
) -> bool {
    let before = manifest.derived.len();
    manifest.derived.retain(|id, _| keep.contains(id));
    outcome.dropped = before - manifest.derived.len();
    outcome.dropped > 0
}

/// Remove a page's stored questions; whether there were any.
fn remove_questions(manifest: &mut Manifest, id: &str) -> bool {
    let Some(kinds) = manifest.derived.get_mut(id) else {
        return false;
    };
    let removed = kinds.remove(DERIVED_QUESTIONS).is_some();
    if kinds.is_empty() {
        manifest.derived.remove(id);
    }
    removed
}

/// Write the manifest, its copy in the artifact and the artifact's `derived.jsonl`, so `verify`
/// still finds them equal.
fn write(paths: &Paths, manifest: &Manifest) -> Result<(), CommandError> {
    manifest.save(&paths.manifest)?;
    manifest.save(&paths.artifact.join(MANIFEST_FILE))?;
    artifact::write_derived(&paths.artifact, manifest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testing::with_llm_url;
    use crate::commands::verify::verify;
    use crate::config::QuestionsSettings;
    use crate::llm::TransportError;
    use crate::llm::testing::{Scripted, ScriptedTransport, completion};
    use crate::pipeline::testing::{CONFIG, fetcher, opts, workspace};
    use crate::pipeline::{ResolveOptions, resolve};

    fn derive_config(extra: &str) -> String {
        CONFIG.replace(
            "ref: main\n    resolver:",
            &format!(
                "ref: main\n    derive:\n      questions:\n        n: 2{extra}\n    resolver:"
            ),
        )
    }

    fn reply(questions: &[&str]) -> Scripted {
        let json = serde_json::json!({ "questions": questions });
        Scripted::Ok(completion(&json.to_string()))
    }

    fn options() -> DeriveOptions {
        DeriveOptions {
            model: Some("test-model".to_string()),
            dry_run: false,
        }
    }

    /// A resolved workspace whose source derives two questions per page, and a first `derive`
    /// over its two pages (`docs/a.md`, then `docs/b.md`).
    fn derived_workspace() -> (tempfile::TempDir, Paths, DeriveOutcome) {
        let (dir, paths) = workspace(&derive_config(""));
        resolve(&paths, &opts(), &fetcher()).unwrap();
        let transport = ScriptedTransport::new(vec![
            reply(&["How do I use A?", "What is A?"]),
            reply(&["How do I use B?"]),
        ]);
        let outcome = with_llm_url(|| derive(&paths, &options(), &transport)).unwrap();
        (dir, paths, outcome)
    }

    #[test]
    fn derive_asks_once_per_stale_page_and_a_second_run_asks_nothing() {
        let (_dir, paths, outcome) = derived_workspace();
        assert_eq!(
            (
                outcome.considered,
                outcome.derived,
                outcome.fresh,
                outcome.written
            ),
            (2, 2, 0, true)
        );

        let manifest = Manifest::load(&paths.manifest).unwrap();
        let a = &manifest.derived["handbook::docs/a.md"][DERIVED_QUESTIONS];
        assert_eq!(a.text, ["How do I use A?", "What is A?"]);
        assert_eq!(a.model, "test-model");
        let settings = QuestionsSettings { n: 2, prompt: None };
        let page_sha = &manifest.sources["handbook"].pages["docs/a.md"].sha256;
        assert_eq!(a.input_sha256, derive::input_sha256(page_sha, &settings));

        let derived = std::fs::read_to_string(paths.artifact.join("derived.jsonl")).unwrap();
        assert_eq!(
            derived,
            concat!(
                "{\"kind\":\"questions\",\"page\":\"handbook::docs/a.md\",",
                "\"text\":[\"How do I use A?\",\"What is A?\"]}\n",
                "{\"kind\":\"questions\",\"page\":\"handbook::docs/b.md\",",
                "\"text\":[\"How do I use B?\"]}\n",
            )
        );
        assert!(verify(&paths, true).unwrap().ok());

        // Nothing is stale, so nothing is asked and no endpoint is needed.
        let transport = ScriptedTransport::new(Vec::new());
        let again = derive(&paths, &options(), &transport).unwrap();
        assert_eq!(
            (again.considered, again.derived, again.fresh, again.written),
            (2, 0, 2, false)
        );
        assert!(transport.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn a_dry_run_lists_stale_pages_and_neither_asks_nor_writes() {
        let (_dir, paths) = workspace(&derive_config(""));
        resolve(&paths, &opts(), &fetcher()).unwrap();
        let before = std::fs::read(&paths.manifest).unwrap();
        let transport = ScriptedTransport::new(Vec::new());
        let dry = DeriveOptions {
            dry_run: true,
            ..DeriveOptions::default()
        };
        let outcome = derive(&paths, &dry, &transport).unwrap();
        assert_eq!(
            outcome.pending,
            ["handbook::docs/a.md", "handbook::docs/b.md"]
        );
        assert!(!outcome.written);
        assert_eq!(std::fs::read(&paths.manifest).unwrap(), before);
        assert!(!paths.artifact.join("derived.jsonl").exists());
        assert!(transport.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn a_changed_page_is_asked_again_and_an_orphan_entry_is_dropped() {
        let (_dir, paths, _) = derived_workspace();
        let mut manifest = Manifest::load(&paths.manifest).unwrap();
        manifest
            .derived
            .get_mut("handbook::docs/a.md")
            .and_then(|kinds| kinds.get_mut(DERIVED_QUESTIONS))
            .unwrap()
            .input_sha256 = "stale".to_string();
        let orphan = manifest.derived["handbook::docs/b.md"].clone();
        manifest
            .derived
            .insert("handbook::docs/gone.md".to_string(), orphan);
        manifest.save(&paths.manifest).unwrap();

        let transport = ScriptedTransport::new(vec![reply(&["Fresh A?", "Fresh A again?"])]);
        let outcome = with_llm_url(|| derive(&paths, &options(), &transport)).unwrap();
        assert_eq!(
            (outcome.derived, outcome.fresh, outcome.dropped),
            (1, 1, 1),
            "{outcome:?}"
        );
        let manifest = Manifest::load(&paths.manifest).unwrap();
        assert_eq!(
            manifest.derived["handbook::docs/a.md"][DERIVED_QUESTIONS].text,
            ["Fresh A?", "Fresh A again?"]
        );
        assert!(!manifest.derived.contains_key("handbook::docs/gone.md"));
        assert!(verify(&paths, true).unwrap().ok());
    }

    #[test]
    fn a_model_error_keeps_what_earlier_pages_earned() {
        let (_dir, paths) = workspace(&derive_config(""));
        resolve(&paths, &opts(), &fetcher()).unwrap();
        let transport = ScriptedTransport::new(vec![
            reply(&["How do I use A?"]),
            Scripted::Err(TransportError::Transport("down".to_string())),
        ]);
        let err = with_llm_url(|| derive(&paths, &options(), &transport)).unwrap_err();
        assert!(
            matches!(&err, CommandError::DeriveFailed { page, saved: 1, .. } if page == "handbook::docs/b.md"),
            "{err}"
        );
        assert!(
            err.to_string().starts_with("handbook::docs/b.md: "),
            "{err}"
        );
        assert!(
            err.to_string().ends_with("(1 pages were saved before it)"),
            "{err}"
        );
        let manifest = Manifest::load(&paths.manifest).unwrap();
        assert_eq!(
            manifest.derived.keys().collect::<Vec<_>>(),
            ["handbook::docs/a.md"]
        );
        assert!(verify(&paths, true).unwrap().ok());
    }

    #[test]
    fn a_reply_that_is_not_json_costs_only_its_page() {
        let (_dir, paths) = workspace(&derive_config(""));
        resolve(&paths, &opts(), &fetcher()).unwrap();
        let transport = ScriptedTransport::new(vec![
            Scripted::Ok(completion("Sure! Here are some questions:")),
            reply(&["How do I use B?"]),
        ]);
        let outcome = with_llm_url(|| derive(&paths, &options(), &transport)).unwrap();
        assert_eq!(outcome.derived, 1);
        assert_eq!(outcome.pending, ["handbook::docs/a.md"]);
        assert!(
            outcome.warnings[0]
                .starts_with("handbook::docs/a.md: the reply is not the expected JSON"),
            "{outcome:?}"
        );
        let manifest = Manifest::load(&paths.manifest).unwrap();
        assert_eq!(
            manifest.derived.keys().collect::<Vec<_>>(),
            ["handbook::docs/b.md"]
        );
    }

    #[test]
    fn an_unusable_reply_removes_the_stale_entry_it_replaces() {
        let (_dir, paths, _) = derived_workspace();
        let mut manifest = Manifest::load(&paths.manifest).unwrap();
        manifest
            .derived
            .get_mut("handbook::docs/a.md")
            .and_then(|kinds| kinds.get_mut(DERIVED_QUESTIONS))
            .unwrap()
            .input_sha256 = "stale".to_string();
        manifest.save(&paths.manifest).unwrap();
        let transport = ScriptedTransport::new(vec![reply(&[" "])]);
        let outcome = with_llm_url(|| derive(&paths, &options(), &transport)).unwrap();
        assert_eq!(outcome.pending, ["handbook::docs/a.md"]);
        let manifest = Manifest::load(&paths.manifest).unwrap();
        assert_eq!(
            manifest.derived.keys().collect::<Vec<_>>(),
            ["handbook::docs/b.md"],
            "old questions for a changed page are not left behind"
        );
        assert!(outcome.written);
    }

    #[test]
    fn a_section_override_that_names_no_section_is_a_warning() {
        let (_dir, paths) = workspace(&derive_config(
            "\n        sections:\n          Nowhere: {n: 1}",
        ));
        resolve(&paths, &opts(), &fetcher()).unwrap();
        let dry = DeriveOptions {
            dry_run: true,
            ..DeriveOptions::default()
        };
        let outcome = derive(&paths, &dry, &ScriptedTransport::new(Vec::new())).unwrap();
        assert_eq!(
            outcome.warnings,
            ["handbook: derive.questions.sections.Nowhere matches no page section"]
        );
    }

    #[test]
    fn a_reply_without_a_usable_question_is_a_warning_and_the_page_stays_pending() {
        let (_dir, paths) = workspace(&derive_config(""));
        resolve(&paths, &opts(), &fetcher()).unwrap();
        let transport =
            ScriptedTransport::new(vec![reply(&["  ", ""]), reply(&["How do I use B?"])]);
        let outcome = with_llm_url(|| derive(&paths, &options(), &transport)).unwrap();
        assert_eq!(outcome.derived, 1);
        assert_eq!(outcome.pending, ["handbook::docs/a.md"]);
        assert!(
            outcome.warnings[0].contains("no usable question"),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_config_without_derive_questions_is_an_error() {
        let (_dir, paths) = workspace(CONFIG);
        resolve(&paths, &opts(), &fetcher()).unwrap();
        let transport = ScriptedTransport::new(Vec::new());
        let err = derive(&paths, &options(), &transport).unwrap_err();
        assert!(matches!(err, CommandError::NoDerive), "{err}");
    }

    #[test]
    fn a_missing_endpoint_is_an_error_only_when_there_is_something_to_ask() {
        let (_dir, paths) = workspace(&derive_config(""));
        resolve(&paths, &opts(), &fetcher()).unwrap();
        let transport = ScriptedTransport::new(Vec::new());
        let err = {
            let _guard = crate::llm::ENV_LOCK.lock().unwrap();
            derive(&paths, &options(), &transport).unwrap_err()
        };
        assert!(matches!(err, CommandError::Llm(_)), "{err}");
    }

    #[test]
    fn resolve_keeps_fresh_questions_drops_stale_ones_and_reproduces_them_from_the_manifest() {
        let (_dir, paths, _) = derived_workspace();
        let derived_file = paths.artifact.join("derived.jsonl");
        let first = std::fs::read(&derived_file).unwrap();
        let manifest_before = Manifest::load(&paths.manifest).unwrap();

        // A fresh resolve of unchanged sources carries the questions over.
        let outcome = resolve(&paths, &opts(), &fetcher()).unwrap();
        assert_eq!(outcome.manifest.derived, manifest_before.derived);
        assert_eq!(std::fs::read(&derived_file).unwrap(), first);
        assert!(verify(&paths, true).unwrap().ok());

        // `--from-manifest` rebuilds the file byte for byte, without a model.
        std::fs::remove_dir_all(&paths.artifact).unwrap();
        let from = ResolveOptions {
            from_manifest: Some(paths.manifest.clone()),
            generated_at: None,
        };
        resolve(&paths, &from, &fetcher()).unwrap();
        assert_eq!(std::fs::read(&derived_file).unwrap(), first);

        // Another prompt or count makes every entry stale, so a fresh resolve drops them and the
        // artifact has no derived.jsonl until the next `derive`.
        std::fs::write(
            &paths.config,
            derive_config("\n        prompt: 'ask {n} things'"),
        )
        .unwrap();
        let outcome = resolve(&paths, &opts(), &fetcher()).unwrap();
        assert!(outcome.manifest.derived.is_empty());
        assert!(!derived_file.exists());
        assert!(verify(&paths, true).unwrap().ok());
    }

    /// A hand-built workspace for what the fake fetcher cannot express: two sources whose
    /// `Storage Module` pages share a title (so `guides`' is a mirror of `handbook`'s), a
    /// manifest recording every page, and the config in `config`.
    fn manual_workspace(config: &str) -> (tempfile::TempDir, Paths) {
        use crate::index::testing::{SourceSpec, write_artifact};
        use crate::manifest::{ManifestSource, PageEntry, SelectedBy};
        let (dir, paths) = workspace(config);
        let sources = [
            SourceSpec {
                name: "handbook",
                repo: "o/handbook",
                pages: &[(
                    "docs/storage.md",
                    "Storage Module",
                    "# Storage\n\nThe storage module keeps uploaded files.\n",
                )],
                residue: &[],
            },
            SourceSpec {
                name: "guides",
                repo: "o/guides",
                pages: &[
                    (
                        "docs/storage.md",
                        "Storage Module",
                        "# Storage\n\nA copy.\n",
                    ),
                    (
                        "docs/billing.md",
                        "Billing",
                        "# Billing\n\nInvoices scale to zero.\n",
                    ),
                ],
                residue: &[],
            },
        ];
        write_artifact(&paths.artifact, &sources);
        let mut manifest = Manifest::new("2026-09-16T12:00:00Z".to_string());
        for source in &sources {
            let pages = source
                .pages
                .iter()
                .map(|(path, title, content)| {
                    let entry = PageEntry {
                        sha256: sha256_hex(content.as_bytes()),
                        title: (*title).to_string(),
                        doc_type: String::new(),
                        section: String::new(),
                        selected_by: SelectedBy::Include,
                        rendered_from: None,
                    };
                    ((*path).to_string(), entry)
                })
                .collect();
            manifest.sources.insert(
                source.name.to_string(),
                ManifestSource {
                    repo: source.repo.to_string(),
                    repo_url: format!("https://github.com/{}.git", source.repo),
                    git_ref: "main".to_string(),
                    commit: "abc".to_string(),
                    archived: None,
                    resolver: "glob".to_string(),
                    pages,
                    residue: Vec::new(),
                    unresolved: Vec::new(),
                    unrendered: Vec::new(),
                    render: None,
                },
            );
        }
        manifest.save(&paths.manifest).unwrap();
        manifest.save(&paths.artifact.join(MANIFEST_FILE)).unwrap();
        (dir, paths)
    }

    fn two_sources(guides_derive: bool) -> String {
        let derive = "    derive:\n      questions:\n        n: 2\n";
        format!(
            "version: 1\nsources:\n  - name: handbook\n    repo: https://github.com/o/handbook.git\n    \
             ref: main\n    priority: 10\n{derive}    resolver:\n      type: glob\n      \
             include: ['**/*.md']\n  - name: guides\n    repo: https://github.com/o/guides.git\n    \
             ref: main\n    priority: 1\n{}    resolver:\n      type: glob\n      \
             include: ['**/*.md']\n",
            if guides_derive { derive } else { "" }
        )
    }

    #[test]
    fn a_mirror_page_is_not_asked_about() {
        let (_dir, paths) = manual_workspace(&two_sources(true));
        // Pages in id order: guides::docs/billing.md, [guides::docs/storage.md is a mirror],
        // handbook::docs/storage.md.
        let transport = ScriptedTransport::new(vec![
            reply(&["What does an invoice cost?"]),
            reply(&["Where are uploads kept?"]),
        ]);
        let outcome = with_llm_url(|| derive(&paths, &options(), &transport)).unwrap();
        assert_eq!((outcome.considered, outcome.derived), (2, 2), "{outcome:?}");
        let manifest = Manifest::load(&paths.manifest).unwrap();
        assert_eq!(
            manifest.derived.keys().collect::<Vec<_>>(),
            ["guides::docs/billing.md", "handbook::docs/storage.md"]
        );
        assert_eq!(transport.requests.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_source_that_stopped_deriving_is_pruned_and_a_dry_run_writes_nothing() {
        let (_dir, paths) = manual_workspace(&two_sources(false));
        let mut manifest = Manifest::load(&paths.manifest).unwrap();
        let orphan = DerivedEntry {
            input_sha256: "x".to_string(),
            model: "m".to_string(),
            text: vec!["Old?".to_string()],
        };
        manifest
            .derived
            .entry("guides::docs/billing.md".to_string())
            .or_default()
            .insert(DERIVED_QUESTIONS.to_string(), orphan);
        manifest.save(&paths.manifest).unwrap();
        let before = std::fs::read(&paths.manifest).unwrap();

        let dry = DeriveOptions {
            dry_run: true,
            ..DeriveOptions::default()
        };
        let outcome = derive(&paths, &dry, &ScriptedTransport::new(Vec::new())).unwrap();
        assert_eq!((outcome.dropped, outcome.written), (1, false));
        assert_eq!(
            std::fs::read(&paths.manifest).unwrap(),
            before,
            "dry run rewrote"
        );
        assert!(!paths.artifact.join("derived.jsonl").exists());

        let transport = ScriptedTransport::new(vec![reply(&["Where are uploads kept?"])]);
        let outcome = with_llm_url(|| derive(&paths, &options(), &transport)).unwrap();
        assert_eq!((outcome.dropped, outcome.derived), (1, 1));
        let manifest = Manifest::load(&paths.manifest).unwrap();
        assert_eq!(
            manifest.derived.keys().collect::<Vec<_>>(),
            ["handbook::docs/storage.md"]
        );
    }

    #[test]
    fn a_config_that_no_longer_derives_prunes_what_is_stored_and_then_is_an_error() {
        let (_dir, paths) = manual_workspace(&two_sources(false));
        let config = std::fs::read_to_string(&paths.config)
            .unwrap()
            .replace("    derive:\n      questions:\n        n: 2\n", "");
        std::fs::write(&paths.config, config).unwrap();
        let mut manifest = Manifest::load(&paths.manifest).unwrap();
        manifest
            .derived
            .entry("handbook::docs/storage.md".to_string())
            .or_default()
            .insert(
                DERIVED_QUESTIONS.to_string(),
                DerivedEntry {
                    input_sha256: "x".to_string(),
                    model: "m".to_string(),
                    text: vec!["Old?".to_string()],
                },
            );
        manifest.save(&paths.manifest).unwrap();
        let transport = ScriptedTransport::new(Vec::new());
        let outcome = derive(&paths, &options(), &transport).unwrap();
        assert_eq!((outcome.dropped, outcome.written), (1, true));
        assert!(Manifest::load(&paths.manifest).unwrap().derived.is_empty());
        assert!(matches!(
            derive(&paths, &options(), &transport).unwrap_err(),
            CommandError::NoDerive
        ));
    }

    #[test]
    fn a_page_that_differs_from_the_manifest_is_skipped_not_filed_under_its_hash() {
        let (_dir, paths) = manual_workspace(&two_sources(true));
        std::fs::write(
            paths.artifact.join("guides/docs/billing.md"),
            "# Billing\n\nSomething else entirely.\n",
        )
        .unwrap();
        let transport = ScriptedTransport::new(vec![reply(&["Where are uploads kept?"])]);
        let outcome = with_llm_url(|| derive(&paths, &options(), &transport)).unwrap();
        assert_eq!(outcome.derived, 1);
        assert!(
            outcome.warnings[0].starts_with(
                "guides::docs/billing.md: the artifact's copy is not the page the manifest records"
            ),
            "{outcome:?}"
        );
        let manifest = Manifest::load(&paths.manifest).unwrap();
        assert_eq!(
            manifest.derived.keys().collect::<Vec<_>>(),
            ["handbook::docs/storage.md"]
        );
    }
}
