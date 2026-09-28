use std::collections::BTreeSet;

use crate::artifact;
use crate::config::Config;
use crate::derive;
use crate::error::CommandError;
use crate::index::{self, Priorities};
use crate::layout::MANIFEST_FILE;
use crate::llm::{ChatTransport, LlmConfig};
use crate::manifest::{DERIVED_QUESTIONS, DerivedEntry, Manifest};
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
/// something to ask. Pages that are mirrors (SPEC §5) are skipped: they are not searchable.
pub fn derive(
    paths: &Paths,
    options: &DeriveOptions,
    transport: &dyn ChatTransport,
) -> Result<DeriveOutcome, CommandError> {
    let config = Config::load(&paths.config)?;
    if !config
        .sources
        .iter()
        .any(|s| s.derive.as_ref().is_some_and(|d| d.questions.is_some()))
    {
        return Err(CommandError::NoDerive);
    }
    let mut manifest = Manifest::load(&paths.manifest)?;
    let mut pages = index::load_pages(&paths.artifact, &Priorities::from_config(&config))?;
    index::mark_mirrors(&mut pages);

    let mut outcome = DeriveOutcome::default();
    let mut work = Vec::new();
    for page in pages.iter().filter(|p| p.mirror_of.is_none()) {
        let Some(settings) = derive::settings_of(&config, &page.source, &page.section) else {
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
        let expected = derive::input_sha256(&recorded.sha256, &settings);
        let stored = manifest
            .derived
            .get(&page.id)
            .and_then(|kinds| kinds.get(DERIVED_QUESTIONS));
        if stored.is_some_and(|entry| entry.input_sha256 == expected) {
            outcome.fresh += 1;
        } else {
            work.push((page, settings, expected));
        }
    }

    let keep: BTreeSet<String> = pages
        .iter()
        .filter(|p| derive::settings_of(&config, &p.source, &p.section).is_some())
        .map(|p| p.id.clone())
        .collect();
    let mut changed = drop_orphans(&mut manifest, &keep, &mut outcome);

    if options.dry_run {
        outcome.pending = work.iter().map(|(page, ..)| page.id.clone()).collect();
        return Ok(outcome);
    }
    if !work.is_empty() {
        let llm = LlmConfig::from_env(options.model.clone())?;
        for (page, settings, expected) in work {
            match derive::questions(transport, &llm, &settings, &page.title, &page.content) {
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
                    changed |= remove_questions(&mut manifest, &page.id);
                }
                Err(err) => {
                    // Keep what earlier pages earned: the next run asks only about the rest.
                    if changed {
                        write(paths, &manifest)?;
                    }
                    return Err(err.into());
                }
            }
        }
    }
    if changed {
        write(paths, &manifest)?;
        outcome.written = true;
    }
    Ok(outcome)
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
        assert!(matches!(err, CommandError::Derive(_)), "{err}");
        let manifest = Manifest::load(&paths.manifest).unwrap();
        assert_eq!(
            manifest.derived.keys().collect::<Vec<_>>(),
            ["handbook::docs/a.md"]
        );
        assert!(verify(&paths, true).unwrap().ok());
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
}
