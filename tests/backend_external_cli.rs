//! `pinakes eval --backend external --backend-url URL` end to end through the binary (SPEC
//! §16.4): a tiny in-process HTTP server stands in for a consumer's own search endpoint.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;

fn pinakes(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_pinakes"))
        .current_dir(dir)
        .arg("--config")
        .arg("nonexistent.yaml")
        .args(args)
        .output()
        .expect("pinakes runs")
}

/// Answer one `POST /search` with a fixed hit, tolerating the `Expect: 100-continue` ureq sends
/// with the request body.
fn serve_one_search(listener: &TcpListener) {
    let (mut stream, _) = listener.accept().unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut answered_continue = false;
    loop {
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0, "connection closed before a full request arrived");
        buf.extend_from_slice(&chunk[..n]);
        let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&buf[..header_end]).into_owned();
        if !answered_continue && headers.to_lowercase().contains("expect: 100-continue") {
            answer_continue(&mut stream);
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
        if buf.len() - (header_end + 4) >= content_length {
            break;
        }
    }
    let body =
        r#"{"hits":[{"page_id":"handbook::docs/user/README.md","score":2.0,"heading":"Storage"}]}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).unwrap();
}

fn answer_continue(stream: &mut TcpStream) {
    stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").unwrap();
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let page = dir.path().join("artifact/handbook/docs/user/README.md");
    fs::create_dir_all(page.parent().unwrap()).unwrap();
    fs::write(&page, "# Storage\n\nEnable upload caching with a label.\n").unwrap();
    fs::write(
        dir.path().join("queries.jsonl"),
        "{\"id\": \"q\", \"kind\": \"howto\", \"query\": \"enable upload caching\", \
         \"expected\": [\"handbook/docs/user\"]}\n",
    )
    .unwrap();
    dir
}

#[test]
fn external_backend_runs_through_the_cli() {
    let dir = workspace();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || serve_one_search(&listener));

    let out = pinakes(
        dir.path(),
        &[
            "eval",
            "--queries",
            "queries.jsonl",
            "--backend",
            "external",
            "--backend-url",
            &format!("http://{addr}"),
        ],
    );
    handle.join().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let summary: serde_json::Value = serde_json::from_str(&stdout).expect("JSON on stdout");
    assert_eq!(summary["backend"], "external");
    assert_eq!(summary["tuning"]["overall"]["recall@5"], 1.0);
    assert_eq!(
        summary["queries"][0]["top"][0],
        "handbook::docs/user/README.md"
    );

    // No --backend-url is a config error, exit 1.
    let out = pinakes(
        dir.path(),
        &[
            "eval",
            "--queries",
            "queries.jsonl",
            "--backend",
            "external",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--backend-url"));
}
