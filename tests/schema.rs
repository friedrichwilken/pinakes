//! Pins the JSON Schema of `manifest.json` (SPEC §2.2, §2.8) at
//! `docs/schemas/manifest.schema.json`, byte for byte.
//!
//! The schema is generated from the `manifest` types, so any change to a manifest field shows up
//! here as a diff of the committed file, which is what consumers read to learn the contract.
//! Refresh with `UPDATE_SCHEMAS=1 cargo test --test schema`, and only when the change to the
//! contract is intended (see `AGENTS.md`).

use std::path::Path;

use pinakes::manifest::{Manifest, to_sorted_json};

/// The schema as this build generates it: sorted keys, two-space indent, trailing newline, the
/// same shape as every other JSON file pinakes commits.
fn generated() -> String {
    let schema = schemars::schema_for!(Manifest);
    to_sorted_json(&schema).expect("a schema serialises")
}

#[test]
fn manifest_schema_is_pinned() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/schemas/manifest.schema.json");
    let actual = generated();
    if std::env::var_os("UPDATE_SCHEMAS").is_some() {
        std::fs::write(&path, &actual).expect("write the schema");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}; run UPDATE_SCHEMAS=1 cargo test --test schema",
            path.display()
        )
    });
    assert!(
        actual == expected,
        "docs/schemas/manifest.schema.json is stale; run UPDATE_SCHEMAS=1 cargo test --test \
         schema and say in the commit why the contract changed.\n--- committed\n{expected}\n\
         --- generated\n{actual}"
    );
}

#[test]
fn manifest_schema_describes_the_contract_version() {
    let schema: serde_json::Value = serde_json::from_str(&generated()).unwrap();
    let field = &schema["properties"]["artifact_version"];
    assert_eq!(
        field["default"],
        serde_json::json!(pinakes::layout::ARTIFACT_VERSION)
    );
    assert_eq!(field["type"], "integer");
    let required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(!required.contains(&"artifact_version"), "missing means 1");
    assert!(required.contains(&"version"));
}
