//! A shared OpenAI-compatible chat completions client (SPEC §14.2), used by `classify` and
//! `grade`.
//!
//! Configuration comes from the environment: `PINAKES_LLM_URL` (base URL, required),
//! `PINAKES_LLM_KEY` (bearer token, optional) and `PINAKES_LLM_MODEL` (overridden by a
//! caller-supplied `--model`). Every request asks for JSON through the system prompt, sets
//! `temperature` to `0`, and is retried up to [`MAX_ATTEMPTS`] times with a short backoff on a
//! `429` or `5xx` response. A missing URL is an error, never a silent skip.
//!
//! All network access goes through the [`ChatTransport`] trait so tests can script responses
//! instead of making real HTTP calls; see [`testing`].

use std::time::Duration;

use serde::de::DeserializeOwned;
use thiserror::Error;

/// Chat completions are retried this many times in total before giving up.
pub const MAX_ATTEMPTS: usize = 3;

/// Backoff before each retry (index 0 before the second attempt, index 1 before the third).
const BACKOFF: [Duration; MAX_ATTEMPTS - 1] =
    [Duration::from_millis(50), Duration::from_millis(200)];

/// The crate's user agent for outgoing HTTP requests.
const USER_AGENT: &str = concat!("pinakes/", env!("CARGO_PKG_VERSION"));

/// How long a single chat completion request may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Guards tests (here and in `commands`) that set or read `PINAKES_LLM_*` env vars, so they
/// cannot interleave with `cargo test`'s default multi-threaded runner.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Errors raised by [`chat`] or building an [`LlmConfig`].
#[derive(Debug, Error)]
pub enum ChatError {
    /// `PINAKES_LLM_URL` is not set.
    #[error(
        "PINAKES_LLM_URL is not set: classify and grade need an OpenAI-compatible chat \
         completions endpoint"
    )]
    MissingUrl,
    /// Neither `--model` nor `PINAKES_LLM_MODEL` is set.
    #[error("no model given: pass --model or set PINAKES_LLM_MODEL")]
    MissingModel,
    /// The endpoint returned a non-success status after every retry was exhausted.
    #[error("{url}: HTTP {status}: {message}")]
    Http {
        /// The request URL.
        url: String,
        /// The final HTTP status code.
        status: u16,
        /// Server-supplied or transport-supplied detail.
        message: String,
    },
    /// A transport-level failure (DNS, TLS, timeout, connection refused, …).
    #[error("{url}: {message}")]
    Transport {
        /// The request URL.
        url: String,
        /// What went wrong.
        message: String,
    },
    /// The response body was not a chat completion with a `choices[0].message.content` string.
    #[error("malformed chat completion response: {0}")]
    MalformedResponse(String),
    /// The message content was not valid JSON of the shape the caller expected.
    #[error("model response is not the expected JSON ({source}): {raw}")]
    Json {
        /// The raw message content returned by the model.
        raw: String,
        /// The parse error.
        #[source]
        source: serde_json::Error,
    },
}

/// Configuration for the chat endpoint (SPEC §14.2).
#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// Base URL; `/chat/completions` is appended.
    pub url: String,
    /// Bearer token, when one is configured.
    pub key: Option<String>,
    /// Model name.
    pub model: String,
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

impl LlmConfig {
    /// Read `PINAKES_LLM_URL`, `PINAKES_LLM_KEY` and `PINAKES_LLM_MODEL` from the environment;
    /// `model_override` (a command's `--model`) takes precedence over `PINAKES_LLM_MODEL`.
    pub fn from_env(model_override: Option<String>) -> Result<LlmConfig, ChatError> {
        let url = env_var("PINAKES_LLM_URL").ok_or(ChatError::MissingUrl)?;
        let key = env_var("PINAKES_LLM_KEY");
        let model = model_override
            .filter(|m| !m.trim().is_empty())
            .or_else(|| env_var("PINAKES_LLM_MODEL"))
            .ok_or(ChatError::MissingModel)?;
        Ok(LlmConfig { url, key, model })
    }
}

/// One failed attempt's outcome: whether [`chat`] should retry it.
#[derive(Debug, Clone)]
pub enum TransportError {
    /// A non-2xx HTTP status.
    Status(u16, String),
    /// A transport-level failure; never retried.
    Transport(String),
}

/// Where chat completion requests go, kept behind a trait so tests can script responses.
pub trait ChatTransport {
    /// POST `body` (already `{"model", "temperature", "messages"}`) to `url` with an optional
    /// bearer `key`, returning the parsed JSON response body.
    fn post(
        &self,
        url: &str,
        key: Option<&str>,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, TransportError>;
}

fn is_retryable(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

fn message_content(response: &serde_json::Value) -> Result<String, ChatError> {
    response
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .ok_or_else(|| ChatError::MalformedResponse(response.to_string()))
}

/// Send one chat completion request (`system` and `user` messages, temperature 0) and parse the
/// model's reply as `T`, retrying on `429`/`5xx` up to [`MAX_ATTEMPTS`] times.
pub fn chat<T: DeserializeOwned>(
    transport: &dyn ChatTransport,
    config: &LlmConfig,
    system: &str,
    user: &str,
) -> Result<T, ChatError> {
    let url = format!("{}/chat/completions", config.url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": config.model,
        "temperature": 0,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
    });
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        match transport.post(&url, config.key.as_deref(), &body) {
            Ok(response) => {
                let content = message_content(&response)?;
                return serde_json::from_str(&content).map_err(|source| ChatError::Json {
                    raw: content,
                    source,
                });
            }
            Err(TransportError::Status(status, message)) => {
                if is_retryable(status) && attempt < MAX_ATTEMPTS {
                    std::thread::sleep(BACKOFF[attempt - 1]);
                    continue;
                }
                return Err(ChatError::Http {
                    url,
                    status,
                    message,
                });
            }
            Err(TransportError::Transport(message)) => {
                return Err(ChatError::Transport { url, message });
            }
        }
    }
}

/// The real transport: an OpenAI-compatible endpoint reached over HTTPS via `ureq`.
pub struct UreqChatTransport {
    agent: ureq::Agent,
}

impl Default for UreqChatTransport {
    fn default() -> UreqChatTransport {
        UreqChatTransport::new()
    }
}

impl UreqChatTransport {
    /// A transport with the crate's user agent and a 60s request timeout.
    pub fn new() -> UreqChatTransport {
        let agent = ureq::Agent::config_builder()
            .user_agent(USER_AGENT)
            .build()
            .new_agent();
        UreqChatTransport { agent }
    }
}

impl ChatTransport for UreqChatTransport {
    fn post(
        &self,
        url: &str,
        key: Option<&str>,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, TransportError> {
        let mut request = self
            .agent
            .post(url)
            .config()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .build()
            .header("Content-Type", "application/json");
        if let Some(key) = key.filter(|k| !k.is_empty()) {
            request = request.header("Authorization", format!("Bearer {key}"));
        }
        match request.send_json(body) {
            Ok(response) => response
                .into_body()
                .read_json()
                .map_err(|e| TransportError::Transport(e.to_string())),
            Err(ureq::Error::StatusCode(status)) => {
                Err(TransportError::Status(status, format!("HTTP {status}")))
            }
            Err(e) => Err(TransportError::Transport(e.to_string())),
        }
    }
}

/// Test doubles: a transport that returns scripted responses in order.
///
/// Public so `classify` and `grade`'s own tests, and integration tests, can exercise the
/// retry and parsing logic without a network call.
pub mod testing {
    use std::sync::Mutex;

    use super::{ChatTransport, TransportError};

    /// One scripted outcome for a single call to [`ChatTransport::post`].
    #[derive(Debug, Clone)]
    pub enum Scripted {
        /// Succeed with this response body (build one with [`completion`]).
        Ok(serde_json::Value),
        /// Fail with this transport error.
        Err(TransportError),
    }

    /// A transport that returns the next scripted outcome on each call, recording every
    /// request body it was sent.
    #[derive(Default)]
    pub struct ScriptedTransport {
        responses: Mutex<Vec<Scripted>>,
        /// Every request body sent so far, in order.
        pub requests: Mutex<Vec<serde_json::Value>>,
    }

    impl ScriptedTransport {
        /// A transport that yields `responses` in order, then fails.
        pub fn new(responses: Vec<Scripted>) -> ScriptedTransport {
            ScriptedTransport {
                responses: Mutex::new(responses),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl ChatTransport for ScriptedTransport {
        fn post(
            &self,
            _url: &str,
            _key: Option<&str>,
            body: &serde_json::Value,
        ) -> Result<serde_json::Value, TransportError> {
            if let Ok(mut requests) = self.requests.lock() {
                requests.push(body.clone());
            }
            let Ok(mut responses) = self.responses.lock() else {
                return Err(TransportError::Transport("poisoned lock".to_string()));
            };
            if responses.is_empty() {
                return Err(TransportError::Transport(
                    "no more scripted responses".to_string(),
                ));
            }
            match responses.remove(0) {
                Scripted::Ok(value) => Ok(value),
                Scripted::Err(err) => Err(err),
            }
        }
    }

    /// Build a chat completion response body carrying `content` as the assistant's message.
    pub fn completion(content: &str) -> serde_json::Value {
        serde_json::json!({
            "choices": [{"message": {"content": content}}],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{Scripted, ScriptedTransport, completion};
    use super::*;

    #[derive(Debug, serde::Deserialize, PartialEq)]
    struct Reply {
        ok: bool,
    }

    fn config() -> LlmConfig {
        LlmConfig {
            url: "https://example.test/v1".to_string(),
            key: Some("secret".to_string()),
            model: "test-model".to_string(),
        }
    }

    #[test]
    fn succeeds_and_parses_the_message_content_as_json() {
        let transport = ScriptedTransport::new(vec![Scripted::Ok(completion("{\"ok\": true}"))]);
        let reply: Reply = chat(&transport, &config(), "system", "user").unwrap();
        assert_eq!(reply, Reply { ok: true });
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["model"], "test-model");
        assert_eq!(requests[0]["temperature"], 0);
        assert_eq!(requests[0]["messages"][0]["role"], "system");
        assert_eq!(requests[0]["messages"][1]["content"], "user");
    }

    #[test]
    fn malformed_json_content_is_a_clear_error() {
        let transport = ScriptedTransport::new(vec![Scripted::Ok(completion("not json"))]);
        let err = chat::<Reply>(&transport, &config(), "s", "u").unwrap_err();
        assert!(matches!(err, ChatError::Json { .. }), "{err}");
        assert!(err.to_string().contains("not json"));
    }

    #[test]
    fn retries_once_on_429_then_succeeds() {
        let transport = ScriptedTransport::new(vec![
            Scripted::Err(TransportError::Status(429, "rate limited".to_string())),
            Scripted::Ok(completion("{\"ok\": true}")),
        ]);
        let reply: Reply = chat(&transport, &config(), "s", "u").unwrap();
        assert_eq!(reply, Reply { ok: true });
        assert_eq!(transport.requests.lock().unwrap().len(), 2);
    }

    #[test]
    fn gives_up_after_max_attempts_on_persistent_5xx() {
        let transport = ScriptedTransport::new(vec![
            Scripted::Err(TransportError::Status(500, "boom".to_string())),
            Scripted::Err(TransportError::Status(503, "still down".to_string())),
            Scripted::Err(TransportError::Status(500, "again".to_string())),
        ]);
        let err = chat::<Reply>(&transport, &config(), "s", "u").unwrap_err();
        assert!(matches!(err, ChatError::Http { status: 500, .. }), "{err}");
        assert_eq!(transport.requests.lock().unwrap().len(), MAX_ATTEMPTS);
    }

    #[test]
    fn non_retryable_status_fails_immediately() {
        let transport = ScriptedTransport::new(vec![Scripted::Err(TransportError::Status(
            401,
            "unauthorized".to_string(),
        ))]);
        let err = chat::<Reply>(&transport, &config(), "s", "u").unwrap_err();
        assert!(matches!(err, ChatError::Http { status: 401, .. }), "{err}");
        assert_eq!(transport.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn transport_failure_is_never_retried() {
        let transport = ScriptedTransport::new(vec![Scripted::Err(TransportError::Transport(
            "dns failure".to_string(),
        ))]);
        let err = chat::<Reply>(&transport, &config(), "s", "u").unwrap_err();
        assert!(matches!(err, ChatError::Transport { .. }), "{err}");
        assert_eq!(transport.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn missing_url_is_an_error_not_a_silent_skip() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialised by ENV_LOCK; no other test observes these vars concurrently.
        unsafe {
            std::env::remove_var("PINAKES_LLM_URL");
        }
        let err = LlmConfig::from_env(None).unwrap_err();
        assert!(matches!(err, ChatError::MissingUrl));
    }

    #[test]
    fn model_override_wins_over_the_environment_variable() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialised by ENV_LOCK; no other test observes these vars concurrently.
        unsafe {
            std::env::set_var("PINAKES_LLM_URL", "https://example.test");
            std::env::set_var("PINAKES_LLM_MODEL", "env-model");
        }
        let config = LlmConfig::from_env(Some("cli-model".to_string())).unwrap();
        assert_eq!(config.model, "cli-model");
        let config = LlmConfig::from_env(None).unwrap();
        assert_eq!(config.model, "env-model");
        // SAFETY: serialised by ENV_LOCK; no other test observes these vars concurrently.
        unsafe {
            std::env::remove_var("PINAKES_LLM_URL");
            std::env::remove_var("PINAKES_LLM_MODEL");
        }
    }
}
