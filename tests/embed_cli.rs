//! `pinakes embed` end to end through the binary (SPEC §16.2): a tiny in-process HTTP server
//! stands in for an OpenAI-compatible embeddings endpoint.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;

fn pinakes(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pinakes"));
    command
        .current_dir(dir)
        .arg("--config")
        .arg("nonexistent.yaml")
        .args(args);
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("pinakes runs")
}

/// Answer every `POST /embeddings` on `listener` with a two-dimensional vector per input text,
/// until the connection closes. Tolerates `Expect: 100-continue`.
fn serve_embeddings(listener: &TcpListener) {
    let (mut stream, _) = listener.accept().unwrap();
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    let body_start = request.find("\r\n\r\n").unwrap() + 4;
    let body: serde_json::Value = serde_json::from_str(&request[body_start..]).unwrap();
    let count = body["input"].as_array().unwrap().len();
    let data: Vec<_> = (0..count)
        .map(|i| {
            #[allow(clippy::cast_precision_loss)]
            let index = i as f64;
            serde_json::json!({"embedding": [index, 1.0]})
        })
        .collect();
    let response_body = serde_json::json!({ "data": data }).to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        response_body.len(),
        response_body
    );
    stream.write_all(response.as_bytes()).unwrap();
}

fn read_request(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut answered_continue = false;
    loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
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
        if buf.len() - (header_end + 4) >= content_length {
            return Some(String::from_utf8_lossy(&buf).into_owned());
        }
    }
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let page = dir.path().join("artifact/handbook/docs/user/README.md");
    fs::create_dir_all(page.parent().unwrap()).unwrap();
    fs::write(&page, "# Storage\n\nEnable upload caching with a label.\n").unwrap();
    dir
}

#[test]
fn embed_writes_the_embeddings_file_pair() {
    let dir = workspace();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || serve_embeddings(&listener));

    let out = pinakes(
        dir.path(),
        &["embed"],
        &[
            ("PINAKES_EMBED_URL", &format!("http://{addr}")),
            ("PINAKES_EMBED_MODEL", "test-model"),
        ],
    );
    handle.join().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let bin = dir.path().join("embeddings.bin");
    let json = dir.path().join("embeddings.json");
    assert!(bin.is_file());
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&json).unwrap()).unwrap();
    assert_eq!(manifest["model"], "test-model");
    assert_eq!(manifest["dimension"], 2);
    assert_eq!(manifest["unit_ids"][0], "handbook::docs/user/README.md");
    assert_eq!(manifest["manifest_sha256"], "none");
    assert_eq!(fs::metadata(&bin).unwrap().len(), 8, "one unit, 2 f32s");

    // A missing PINAKES_EMBED_URL is an error, not a silent skip.
    let out = Command::new(env!("CARGO_BIN_EXE_pinakes"))
        .current_dir(dir.path())
        .env_remove("PINAKES_EMBED_URL")
        .arg("--config")
        .arg("nonexistent.yaml")
        .arg("embed")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("PINAKES_EMBED_URL"));
}
