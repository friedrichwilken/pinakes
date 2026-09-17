//! `external` (SPEC §16.4): a consumer's own store, over HTTP.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use super::{Backend, BackendConfig, BackendError};
use crate::index::{Hit, load_pages, mark_mirrors};

/// External backend timeout (SPEC §16.4).
const EXTERNAL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
struct ExternalHit {
    page_id: String,
    score: f64,
    #[serde(default)]
    heading: String,
}

#[derive(Debug, Deserialize)]
struct ExternalResponse {
    hits: Vec<ExternalHit>,
}

/// `external` (SPEC §16.4): `POST {backend_url}/search` with `{"query", "k", "module"}`,
/// expecting `{"hits": [{"page_id", "score", "heading"}]}`. Used to evaluate a store a
/// consumer already runs; a failed request or a malformed response fails the whole `eval`.
pub struct ExternalBackend {
    url: String,
    agent: ureq::Agent,
    page_count: usize,
    searchable_count: usize,
}

impl std::fmt::Debug for ExternalBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalBackend")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl Backend for ExternalBackend {
    fn build(artifact: &Path, config: &BackendConfig) -> Result<ExternalBackend, BackendError> {
        let url = config.backend_url()?.to_string();
        let mut pages = load_pages(artifact, &config.priorities)?;
        mark_mirrors(&mut pages);
        let searchable_count = pages.iter().filter(|p| p.mirror_of.is_none()).count();
        let agent = ureq::Agent::config_builder()
            .user_agent(concat!("pinakes/", env!("CARGO_PKG_VERSION")))
            .build()
            .new_agent();
        Ok(ExternalBackend {
            url,
            agent,
            page_count: pages.len(),
            searchable_count,
        })
    }

    fn search(
        &self,
        query: &str,
        k: usize,
        module: Option<&str>,
    ) -> Result<Vec<Hit>, BackendError> {
        let url = format!("{}/search", self.url.trim_end_matches('/'));
        let body = serde_json::json!({ "query": query, "k": k, "module": module });
        let response = self
            .agent
            .post(&url)
            .config()
            .timeout_global(Some(EXTERNAL_TIMEOUT))
            .build()
            .header("content-type", "application/json")
            .send_json(&body)
            .map_err(|err| BackendError::Http {
                url: url.clone(),
                message: err.to_string(),
            })?;
        let parsed: ExternalResponse =
            response
                .into_body()
                .read_json()
                .map_err(|err| BackendError::BadResponse {
                    url: url.clone(),
                    message: err.to_string(),
                })?;
        Ok(parsed
            .hits
            .into_iter()
            .take(k)
            .map(|hit| Hit {
                page_id: hit.page_id,
                score: hit.score,
                heading: hit.heading,
            })
            .collect())
    }

    fn page_count(&self) -> usize {
        self.page_count
    }

    fn searchable_count(&self) -> usize {
        self.searchable_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::testing::{fixture_pages, read_http_request};

    #[test]
    fn external_backend_sends_the_request_and_parses_the_response() {
        use std::io::Write;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("POST /search"), "{request}");
            assert!(request.contains("\"query\""), "{request}");
            assert!(request.contains("\"caching\""), "{request}");
            let body = r#"{"hits":[{"page_id":"handbook::docs/user/README.md","score":1.5,"heading":"Upload caching"}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let (dir, _pages) = fixture_pages();
        let config = BackendConfig {
            backend_url: Some(format!("http://{addr}")),
            ..BackendConfig::default()
        };
        let backend = ExternalBackend::build(dir.path(), &config).unwrap();
        assert_eq!(backend.page_count(), 2);
        let hits = backend.search("caching", 5, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].page_id, "handbook::docs/user/README.md");
        assert!((hits[0].score - 1.5).abs() < 1e-12);
        assert_eq!(hits[0].heading, "Upload caching");
        handle.join().unwrap();
    }

    #[test]
    fn external_backend_without_a_url_is_a_config_error() {
        let (dir, _pages) = fixture_pages();
        let err = ExternalBackend::build(dir.path(), &BackendConfig::default()).unwrap_err();
        assert!(matches!(err, BackendError::Config { .. }), "{err}");
    }
}
