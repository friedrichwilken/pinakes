use crate::classify;
use crate::decisions::{self, Decision};
use crate::duplicates;
use crate::error::CommandError;
use crate::llm::{ChatTransport, LlmConfig};
use crate::manifest::{Manifest, now_rfc3339};
use crate::page::{PageRegistry, PageStatus};
use crate::residue;
use crate::text::strip_frontmatter;
use crate::workspace::Paths;

/// Options for `classify` (SPEC §14.2).
#[derive(Debug, Clone, Default)]
pub struct ClassifyOptions {
    /// `--model`; falls back to `PINAKES_LLM_MODEL`.
    pub model: Option<String>,
    /// `--batch` (default [`classify::DEFAULT_BATCH`]).
    pub batch: Option<usize>,
    /// Print the proposed decisions instead of writing them.
    pub dry_run: bool,
}

/// What `classify` produced.
#[derive(Debug)]
pub struct ClassifyOutcome {
    /// One decision per candidate the model classified.
    pub decisions: Vec<Decision>,
    /// Non-fatal observations (a rule override, or an id the model invented).
    pub warnings: Vec<String>,
    /// Whether the decisions were appended to `decisions.jsonl` (`false` for `--dry-run`).
    pub written: bool,
}

/// Run `classify`: send undecided residue and near-duplicate candidates to the model in
/// batches and write (or, with `--dry-run`, print) the resulting decisions (SPEC §14.2).
pub fn classify(
    paths: &Paths,
    options: &ClassifyOptions,
    transport: &dyn ChatTransport,
) -> Result<ClassifyOutcome, CommandError> {
    let config = LlmConfig::from_env(options.model.clone())?;
    let residue = if paths.residue.is_file() {
        residue::read_jsonl(&paths.residue)?
    } else {
        Vec::new()
    };
    let duplicate_pairs = duplicates::read_jsonl(&paths.duplicates)?;
    let effective = decisions::effective(&decisions::read_jsonl(&paths.decisions)?);
    let manifest = if paths.manifest.is_file() {
        Some(Manifest::load(&paths.manifest)?)
    } else {
        None
    };
    let mut registry = PageRegistry::load(manifest.as_ref(), &residue);
    // A near-duplicate candidate not already covered by residue needs an excerpt from the
    // artifact, read here (once, eagerly) so `classify::candidates` stays pure.
    for pair in &duplicate_pairs {
        let id = &pair.duplicate;
        let is_selected = registry
            .corpus_page(id)
            .is_some_and(|record| matches!(record.status, PageStatus::Selected { .. }));
        if !is_selected {
            continue;
        }
        let Some(bytes) = crate::pipeline::outputs::artifact_page_bytes(&paths.artifact, id) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        let excerpt = residue::excerpt(strip_frontmatter(&text), residue::EXCERPT_TOKENS);
        let _ = registry.set_excerpt(id, excerpt);
    }
    let candidates = classify::candidates(&registry, &duplicate_pairs, &effective);
    let batch = options.batch.unwrap_or(classify::DEFAULT_BATCH);
    let at = now_rfc3339();
    let outcome = classify::run(transport, &config, &candidates, batch, &at)?;
    let written = if options.dry_run {
        false
    } else {
        for decision in &outcome.decisions {
            decisions::append(&paths.decisions, decision)?;
        }
        true
    };
    Ok(ClassifyOutcome {
        decisions: outcome.decisions,
        warnings: outcome.warnings,
        written,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testing::with_llm_url;
    use crate::decisions::Verdict;
    use crate::llm::ChatError;
    use crate::pipeline::testing::{CONFIG, workspace};
    use crate::residue::{Reason, ResidueEntry};

    fn classify_reply(id: &str, decision: &str) -> serde_json::Value {
        crate::llm::testing::completion(
            &serde_json::to_string(&serde_json::json!([
                {"id": id, "decision": decision, "rationale": "noise", "confidence": 0.9}
            ]))
            .unwrap(),
        )
    }

    #[test]
    fn classify_dry_run_prints_but_writes_nothing_and_a_real_run_writes_with_classifier_by() {
        let (_dir, paths) = workspace(CONFIG);
        residue::write_jsonl(
            &paths.residue,
            &[ResidueEntry {
                id: "handbook::docs/x.md".to_string(),
                source: "handbook".to_string(),
                path: "docs/x.md".to_string(),
                reason: Reason::NotSelected,
                sha256: "aa".repeat(32),
                title: "X".to_string(),
                excerpt: "some text".to_string(),
                context: String::new(),
                url: String::new(),
                rule: None,
            }],
        )
        .unwrap();

        with_llm_url(|| {
            let dry_run_transport = crate::llm::testing::ScriptedTransport::new(vec![
                crate::llm::testing::Scripted::Ok(classify_reply("handbook::docs/x.md", "exclude")),
            ]);
            let options = ClassifyOptions {
                model: Some("m".to_string()),
                batch: None,
                dry_run: true,
            };
            let outcome = classify(&paths, &options, &dry_run_transport).unwrap();
            assert!(!outcome.written);
            assert_eq!(outcome.decisions.len(), 1);
            assert!(!paths.decisions.is_file(), "dry run must not write");

            let write_transport = crate::llm::testing::ScriptedTransport::new(vec![
                crate::llm::testing::Scripted::Ok(classify_reply("handbook::docs/x.md", "exclude")),
            ]);
            let options = ClassifyOptions {
                dry_run: false,
                ..options
            };
            let outcome = classify(&paths, &options, &write_transport).unwrap();
            assert!(outcome.written);
            let written = decisions::read_jsonl(&paths.decisions).unwrap();
            assert_eq!(written.len(), 1);
            assert_eq!(written[0].id, "handbook::docs/x.md");
            assert_eq!(written[0].decision, Verdict::Exclude);
            assert_eq!(written[0].by, "classifier:m");
        });
    }

    #[test]
    fn classify_requires_the_llm_url_even_with_no_candidates() {
        let _guard = crate::llm::ENV_LOCK.lock().unwrap();
        // SAFETY: serialised by ENV_LOCK; no other test observes this var concurrently.
        unsafe {
            std::env::remove_var("PINAKES_LLM_URL");
        }
        let (_dir, paths) = workspace(CONFIG);
        let transport = crate::llm::testing::ScriptedTransport::new(vec![]);
        let options = ClassifyOptions::default();
        let err = classify(&paths, &options, &transport).unwrap_err();
        assert!(
            matches!(err, CommandError::Llm(ChatError::MissingUrl)),
            "{err}"
        );
    }
}
