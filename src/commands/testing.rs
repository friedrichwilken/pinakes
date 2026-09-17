//! Test fixtures shared by more than one command's tests.

use std::fs;

use crate::embed::Embedder;
use crate::workspace::Paths;

/// A workspace with a synthetic artifact, a query file and no config.
pub(super) fn eval_workspace() -> (tempfile::TempDir, Paths) {
    use crate::index::testing::{SourceSpec, write_artifact};
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::for_config(&dir.path().join("pinakes.yaml"));
    write_artifact(
        &paths.artifact,
        &[SourceSpec {
            name: "handbook",
            repo: "example-org/handbook",
            pages: &[(
                "docs/user/README.md",
                "Storage Module",
                "# Storage\n\nEnable upload caching with a bucket label.\n",
            )],
            residue: &[(
                "docs/user/quotas.md",
                "# Configure Quotas\n\nRate limits in strict mode.\n",
            )],
        }],
    );
    fs::write(
        dir.path().join("queries.jsonl"),
        concat!(
            "{\"id\": \"caching\", \"kind\": \"howto\", \"query\": \"enable upload caching\", \"expected\": [\"handbook/docs/user\"]}\n",
            "{\"id\": \"quotas\", \"kind\": \"howto\", \"query\": \"quotas rate limits\", \"expected\": [\"handbook::docs/user/quotas.md\"]}\n",
        ),
    )
    .unwrap();
    (dir, paths)
}

pub(super) fn fake_embedder() -> std::rc::Rc<dyn Embedder> {
    std::rc::Rc::new(crate::embed::testing::FakeEmbedder)
}

/// Set `PINAKES_LLM_URL` for the duration of `body`, serialised against every other test that
/// touches `PINAKES_LLM_*`, and always clean up afterwards.
pub(super) fn with_llm_url<T>(body: impl FnOnce() -> T) -> T {
    let _guard = crate::llm::ENV_LOCK.lock().unwrap();
    // SAFETY: serialised by ENV_LOCK; no other test observes this var concurrently.
    unsafe {
        std::env::set_var("PINAKES_LLM_URL", "https://example.test");
    }
    let result = body();
    // SAFETY: serialised by ENV_LOCK; no other test observes this var concurrently.
    unsafe {
        std::env::remove_var("PINAKES_LLM_URL");
    }
    result
}
