//! Derived retrieval text (SPEC §14.4): what `pinakes derive` asks the model for, and how a
//! page's entry goes stale.
//!
//! This module is pure: it builds the prompt, hashes a page's inputs, cleans the model's reply
//! and carries still-fresh entries across a `resolve`. All file I/O (the manifest, the
//! artifact's `derived.jsonl`) is the caller's job (`commands::derive`, `artifact`).
//!
//! Derived text is a search target, never a read target: it is added to a page's index text
//! (see [`crate::index::Index`]) and never to the page a consumer reads. It must not share a
//! prompt or an output with the queries `kanon` generates for the *query set*, or an evaluation
//! would measure its own homework: the built-in prompt below is written for retrieval text and
//! asks for many plain-worded questions, where kanon's asks for a few labelled test queries.

use serde::Deserialize;
use thiserror::Error;

use crate::config::{Config, QuestionsSettings};
use crate::llm::{self, ChatError, ChatTransport, LlmConfig};
use crate::manifest::{DERIVED_QUESTIONS, Manifest, split_page_id};
use crate::text::sha256_hex;

/// The built-in system prompt for generated questions. `{n}` is replaced by the count.
pub const DEFAULT_QUESTIONS_PROMPT: &str = "You write search text for a documentation site. You \
will be given one documentation page: its title and its content. Write {n} different questions \
that a user who does not know this page would type into a search box, and that this page \
answers. Describe what the user wants or is stuck on in everyday words, and prefer the words a \
user would use over the words the page uses (synonyms, plain descriptions of a feature or a \
symptom). Do not copy sentences from the page and do not use the page's title as a question. \
Each question stands alone, without numbering. Respond with JSON only, no prose and no markdown \
code fences, in this exact shape: {\"questions\": [\"...\", \"...\"]}";

/// Bumped when the user message or the reply handling changes, so existing entries go stale.
const FORMAT: &str = "questions/1";

/// The most page content, in characters, sent to the model.
pub const MAX_CONTENT_CHARS: usize = 12_000;

/// The longest question kept, in characters; a longer "question" is not one.
pub const MAX_QUESTION_CHARS: usize = 300;

/// Errors raised while deriving text.
#[derive(Debug, Error)]
pub enum DeriveError {
    /// Talking to the model failed: no endpoint, an HTTP or transport error. Asking about the
    /// next page would fail the same way.
    #[error(transparent)]
    Llm(#[from] ChatError),
    /// The model answered, but not with the questions asked for. Only this page is affected.
    #[error("the reply is not the expected JSON ({source}): {raw}")]
    Reply {
        /// The reply, cut for display.
        raw: String,
        /// Why it did not parse.
        #[source]
        source: serde_json::Error,
    },
}

/// The system prompt for `settings`: the configured one, else the built-in, with `{n}` filled.
pub fn system_prompt(settings: &QuestionsSettings) -> String {
    settings
        .prompt
        .as_deref()
        .unwrap_or(DEFAULT_QUESTIONS_PROMPT)
        .replace("{n}", &settings.n.to_string())
}

/// The hash of everything a page's questions are derived from: the page's `sha256`, the count,
/// the effective prompt and the format of this module. Equal hashes mean the stored questions
/// are still the ones a run would ask for; the model is deliberately not part of it, so
/// changing the model does not regenerate the whole corpus.
pub fn input_sha256(page_sha256: &str, settings: &QuestionsSettings) -> String {
    sha256_hex(
        format!(
            "{FORMAT}\n{page_sha256}\n{}\n{}",
            settings.n,
            system_prompt(settings)
        )
        .as_bytes(),
    )
}

/// The user message for one page: its title and its (cleaned) content, cut at
/// [`MAX_CONTENT_CHARS`].
pub fn user_message(title: &str, content: &str) -> String {
    let content: String = content.chars().take(MAX_CONTENT_CHARS).collect();
    format!("Title: {title}\n\n{content}")
}

/// The reply the prompt asks for, `{"questions": [...]}`, or the bare array a model often
/// writes instead.
#[derive(Deserialize)]
#[serde(untagged)]
enum Reply {
    Object { questions: Vec<String> },
    List(Vec<String>),
}

/// The JSON of a reply that may be wrapped in a markdown code fence.
fn unfenced(reply: &str) -> &str {
    let text = reply.trim();
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let body = rest.split_once('\n').map_or(rest, |(_, body)| body).trim();
    body.strip_suffix("```").unwrap_or(body).trim()
}

/// Collapse each question to one line, drop blanks, control characters, questions longer than
/// [`MAX_QUESTION_CHARS`] and case-insensitive repeats, and keep at most `n`, in the model's
/// order.
pub fn clean_questions(raw: Vec<String>, n: usize) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    raw.into_iter()
        .map(|question| question.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|question| {
            !question.is_empty()
                && question.chars().count() <= MAX_QUESTION_CHARS
                && !question.chars().any(char::is_control)
                && seen.insert(question.to_lowercase())
        })
        .take(n)
        .collect()
}

/// Ask the model for the questions one page answers; empty when it gave no usable one.
pub fn questions(
    transport: &dyn ChatTransport,
    config: &LlmConfig,
    settings: &QuestionsSettings,
    title: &str,
    content: &str,
) -> Result<Vec<String>, DeriveError> {
    let text = llm::chat_text(
        transport,
        config,
        &system_prompt(settings),
        &user_message(title, content),
    )?;
    let reply: Reply =
        serde_json::from_str(unfenced(&text)).map_err(|source| DeriveError::Reply {
            raw: text.chars().take(200).collect(),
            source,
        })?;
    let raw = match reply {
        Reply::Object { questions } | Reply::List(questions) => questions,
    };
    Ok(clean_questions(raw, settings.n))
}

/// The questions settings that apply to a page of `section` in `source`, when the source
/// derives questions at all.
pub fn settings_of(config: &Config, source: &str, section: &str) -> Option<QuestionsSettings> {
    let questions = config.source(source)?.derive.as_ref()?.questions.as_ref()?;
    Some(questions.settings_for(section))
}

/// Copy into `manifest` every derived entry of `previous` that is still fresh: its page is in
/// `manifest` and its `input_sha256` equals the hash the current config and page would give.
/// The rest is dropped, so a page that changed (or whose prompt did) is stale until the next
/// `derive`, and a fresh `resolve` never invents text.
pub fn carry_over(previous: &Manifest, manifest: &mut Manifest, config: &Config) {
    for (id, kinds) in &previous.derived {
        let Some(old) = kinds.get(DERIVED_QUESTIONS) else {
            continue;
        };
        let Some((source, path)) = split_page_id(id) else {
            continue;
        };
        let Some(entry) = manifest.sources.get(source).and_then(|s| s.pages.get(path)) else {
            continue;
        };
        let Some(settings) = settings_of(config, source, &entry.section) else {
            continue;
        };
        if old.input_sha256 == input_sha256(&entry.sha256, &settings) {
            manifest
                .derived
                .entry(id.clone())
                .or_default()
                .insert(DERIVED_QUESTIONS.to_string(), old.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::TransportError;
    use crate::llm::testing::{Scripted, ScriptedTransport, completion};
    use crate::manifest::DerivedEntry;

    fn settings(n: usize, prompt: Option<&str>) -> QuestionsSettings {
        QuestionsSettings {
            n,
            prompt: prompt.map(str::to_string),
        }
    }

    fn llm_config() -> LlmConfig {
        LlmConfig {
            url: "http://localhost".to_string(),
            key: None,
            model: "m".to_string(),
        }
    }

    fn page(sha: &str, section: &str) -> crate::manifest::PageEntry {
        crate::manifest::PageEntry {
            sha256: sha.to_string(),
            title: String::new(),
            doc_type: String::new(),
            section: section.to_string(),
            selected_by: crate::manifest::SelectedBy::Include,
            rendered_from: None,
        }
    }

    fn manifest_with(pages: &[(&str, crate::manifest::PageEntry)]) -> Manifest {
        let mut manifest = Manifest::new("2026-09-16T12:00:00Z".to_string());
        manifest.sources.insert(
            "handbook".to_string(),
            crate::manifest::ManifestSource {
                repo: "o/handbook".to_string(),
                repo_url: "https://github.com/o/handbook.git".to_string(),
                git_ref: "main".to_string(),
                commit: "a".repeat(40),
                archived: None,
                resolver: "glob".to_string(),
                pages: pages
                    .iter()
                    .map(|(path, entry)| ((*path).to_string(), entry.clone()))
                    .collect(),
                residue: Vec::new(),
                unresolved: Vec::new(),
                unrendered: Vec::new(),
                render: None,
            },
        );
        manifest
    }

    #[test]
    fn carry_over_keeps_only_entries_whose_inputs_are_unchanged() {
        let config = Config::from_yaml(
            "version: 1\nsources:\n  - name: handbook\n    repo: https://github.com/o/handbook.git\n    \
             ref: main\n    derive:\n      questions:\n        n: 2\n        sections:\n          \
             Tutorials: {n: 3}\n    resolver:\n      type: glob\n      include: ['**/*.md']\n",
        )
        .unwrap();
        let plain = settings(2, None);
        let tutorial = settings(3, None);
        let derived_entry = |input: String| DerivedEntry {
            input_sha256: input,
            model: "m".to_string(),
            text: vec!["Q?".to_string()],
        };
        let mut previous = manifest_with(&[]);
        for (id, input) in [
            ("handbook::a.md", input_sha256("sha-a", &plain)),
            ("handbook::b.md", input_sha256("old-b", &plain)),
            ("handbook::t.md", input_sha256("sha-t", &tutorial)),
            ("handbook::gone.md", input_sha256("sha-g", &plain)),
            ("other::x.md", input_sha256("sha-x", &plain)),
            ("not-an-id", input_sha256("sha", &plain)),
        ] {
            previous
                .derived
                .entry(id.to_string())
                .or_default()
                .insert(DERIVED_QUESTIONS.to_string(), derived_entry(input));
        }
        // b.md changed; t.md is in a section with its own count, the hash of which it matches.
        let mut manifest = manifest_with(&[
            ("a.md", page("sha-a", "")),
            ("b.md", page("new-b", "")),
            ("t.md", page("sha-t", "Tutorials")),
        ]);
        carry_over(&previous, &mut manifest, &config);
        assert_eq!(
            manifest.derived.keys().collect::<Vec<_>>(),
            ["handbook::a.md", "handbook::t.md"]
        );

        // A source that stopped deriving keeps nothing.
        let mut manifest = manifest_with(&[("a.md", page("sha-a", ""))]);
        let off = Config::from_yaml(
            "version: 1\nsources:\n  - name: handbook\n    repo: https://github.com/o/handbook.git\n    \
             ref: main\n    resolver:\n      type: glob\n      include: ['**/*.md']\n",
        )
        .unwrap();
        carry_over(&previous, &mut manifest, &off);
        assert!(manifest.derived.is_empty());
    }

    #[test]
    fn the_prompt_names_the_count_and_a_configured_prompt_wins() {
        let built_in = system_prompt(&settings(3, None));
        assert!(
            built_in.contains("Write 3 different questions"),
            "{built_in}"
        );
        assert!(!built_in.contains("{n}"));
        assert_eq!(
            system_prompt(&settings(4, Some("ask {n} things, briefly"))),
            "ask 4 things, briefly"
        );
    }

    #[test]
    fn the_input_hash_follows_the_page_the_count_and_the_prompt_only() {
        let base = input_sha256("aa", &settings(5, None));
        assert_eq!(base, input_sha256("aa", &settings(5, None)));
        assert_ne!(base, input_sha256("bb", &settings(5, None)), "page changed");
        assert_ne!(
            base,
            input_sha256("aa", &settings(6, None)),
            "count changed"
        );
        assert_ne!(
            base,
            input_sha256("aa", &settings(5, Some("another prompt"))),
            "prompt changed"
        );
        assert_eq!(base.len(), 64);
    }

    #[test]
    fn replies_are_trimmed_deduplicated_and_capped() {
        let raw = [
            "  How do I enable caching? ",
            "",
            "how do i enable caching?",
            "Why is upload slow",
            "What is a bucket label",
        ]
        .map(str::to_string)
        .to_vec();
        assert_eq!(
            clean_questions(raw, 2),
            ["How do I enable caching?", "Why is upload slow"]
        );
    }

    #[test]
    fn the_user_message_is_the_title_and_the_content_cut_at_the_limit() {
        let long = "x".repeat(MAX_CONTENT_CHARS + 50);
        let message = user_message("Caching", &long);
        assert!(message.starts_with("Title: Caching\n\nxxx"));
        assert_eq!(
            message.chars().count(),
            "Title: Caching\n\n".len() + MAX_CONTENT_CHARS
        );
    }

    #[test]
    fn questions_asks_once_and_parses_the_reply() {
        let transport = ScriptedTransport::new(vec![Scripted::Ok(completion(
            "{\"questions\": [\"How do I cache uploads?\", \"Is caching on by default?\"]}",
        ))]);
        let got = questions(
            &transport,
            &llm_config(),
            &settings(5, None),
            "Caching",
            "Enable upload caching with a bucket label.",
        )
        .unwrap();
        assert_eq!(
            got,
            ["How do I cache uploads?", "Is caching on by default?"]
        );
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let messages = requests[0]["messages"].as_array().unwrap();
        assert!(
            messages[0]["content"]
                .as_str()
                .unwrap()
                .contains("Write 5 different")
        );
        assert!(
            messages[1]["content"]
                .as_str()
                .unwrap()
                .contains("Enable upload caching")
        );
    }

    fn ask(reply: &str) -> Result<Vec<String>, DeriveError> {
        let transport = ScriptedTransport::new(vec![Scripted::Ok(completion(reply))]);
        questions(&transport, &llm_config(), &settings(5, None), "T", "c")
    }

    #[test]
    fn a_bare_array_and_a_fenced_reply_are_read_like_the_object() {
        let want = ["How do I cache uploads?", "Is caching on by default?"];
        for reply in [
            "{\"questions\": [\"How do I cache uploads?\", \"Is caching on by default?\"]}",
            "[\"How do I cache uploads?\", \"Is caching on by default?\"]",
            "```json\n{\"questions\": [\"How do I cache uploads?\", \"Is caching on by default?\"]}\n```",
            "```\n[\"How do I cache uploads?\", \"Is caching on by default?\"]\n```",
            "  {\"questions\": [\"How do I cache uploads?\", \"Is caching on by default?\"], \"note\": 1}  ",
        ] {
            assert_eq!(ask(reply).unwrap(), want, "{reply}");
        }
    }

    #[test]
    fn a_reply_that_is_not_the_shape_is_a_per_page_error_and_a_dead_endpoint_is_not() {
        for reply in [
            "{\"questions\": \"a string\"}",
            "{\"questions\": null}",
            "[1, {\"a\": 2}, null]",
            "Sure! Here are some questions.",
            "",
        ] {
            let err = ask(reply).unwrap_err();
            assert!(matches!(err, DeriveError::Reply { .. }), "{reply}: {err}");
        }
        let transport = ScriptedTransport::new(vec![Scripted::Err(TransportError::Transport(
            "down".to_string(),
        ))]);
        let err = questions(&transport, &llm_config(), &settings(5, None), "T", "c").unwrap_err();
        assert!(
            matches!(err, DeriveError::Llm(ChatError::Transport { .. })),
            "{err}"
        );
    }

    #[test]
    fn questions_are_one_line_short_and_free_of_control_characters() {
        let long = "x".repeat(MAX_QUESTION_CHARS + 1);
        let fits = "y".repeat(MAX_QUESTION_CHARS);
        let raw = vec![
            "How do I\n  set   it up?".to_string(),
            long,
            fits.clone(),
            "bell\u{7}".to_string(),
            "tab\tseparated".to_string(),
        ];
        assert_eq!(
            clean_questions(raw, 10),
            [
                "How do I set it up?".to_string(),
                fits,
                "tab separated".to_string()
            ]
        );
    }

    #[test]
    fn the_count_is_part_of_the_hash_even_when_the_prompt_never_mentions_it() {
        assert_ne!(
            input_sha256("aa", &settings(5, Some("a custom prompt"))),
            input_sha256("aa", &settings(6, Some("a custom prompt")))
        );
    }
}
