//! Test-only helpers shared by the backend tests: a small artifact fixture, an embeddings
//! file pair for it, and a one-request HTTP server reader.

use std::path::Path;
use std::rc::Rc;

use super::BackendConfig;
use crate::embed::Embedder;
use crate::index::testing::{SourceSpec, write_artifact};
use crate::index::{Page, Priorities, iter_units, load_pages, mark_mirrors};

pub(super) fn fixture_pages() -> (tempfile::TempDir, Vec<Page>) {
    let dir = tempfile::tempdir().unwrap();
    write_artifact(
        dir.path(),
        &[SourceSpec {
            name: "handbook",
            repo: "example-org/handbook",
            pages: &[
                (
                    "docs/user/README.md",
                    "Storage Module",
                    "# Storage\n\nThe storage module keeps uploaded files.\n\n## Upload caching\n\nEnable upload caching with a bucket label.\n",
                ),
                (
                    "docs/user/billing.md",
                    "Billing",
                    "# Billing\n\nInvoices scale to zero.\n",
                ),
            ],
            residue: &[],
        }],
    );
    let pages = load_pages(dir.path(), &Priorities::default()).unwrap();
    (dir, pages)
}

pub(super) fn dense_config(
    dir: &Path,
    embedder: Rc<dyn Embedder>,
) -> (tempfile::TempDir, BackendConfig) {
    let mut pages = load_pages(dir, &Priorities::default()).unwrap();
    mark_mirrors(&mut pages);
    let units = iter_units(&pages);
    let texts: Vec<String> = units.iter().map(|u| u.text.clone()).collect();
    let vectors = crate::embed::embed_units(embedder.as_ref(), "fake", &texts, 64).unwrap();
    let out = tempfile::tempdir().unwrap();
    let bin = out.path().join("embeddings.bin");
    let json = out.path().join("embeddings.json");
    let manifest = crate::embed::EmbeddingsManifest {
        model: "fake".to_string(),
        dimension: crate::embed::testing::FAKE_DIMENSION,
        unit_ids: units.iter().map(|u| u.page_id.clone()).collect(),
        manifest_sha256: crate::embed::artifact_manifest_hash(dir).unwrap(),
    };
    crate::embed::write_embeddings(&bin, &json, &manifest, &vectors).unwrap();
    let config = BackendConfig {
        embeddings_bin: bin,
        embeddings_json: json,
        embedder: Some(embedder),
        ..BackendConfig::default()
    };
    (out, config)
}

/// Read one HTTP/1.1 request off `stream`, answering `Expect: 100-continue` (which ureq
/// sends before a request body) so the client proceeds to send it, then returning headers
/// and body as one string once `Content-Length` bytes of body have arrived.
pub(super) fn read_http_request(stream: &mut std::net::TcpStream) -> String {
    use std::io::{Read as _, Write as _};

    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut answered_continue = false;
    loop {
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0, "connection closed before a full request arrived");
        buf.extend_from_slice(&chunk[..n]);
        let Some(header_end) = find_subslice(&buf, b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&buf[..header_end]).into_owned();
        if !answered_continue && headers.to_lowercase().contains("expect: 100-continue") {
            stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").unwrap();
            answered_continue = true;
        }
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.to_lowercase()
                    .strip_prefix("content-length:")
                    .map(str::trim)
                    .map(str::to_string)
            })
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        let body_start = header_end + 4;
        if buf.len() - body_start >= content_length {
            return String::from_utf8_lossy(&buf).into_owned();
        }
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
