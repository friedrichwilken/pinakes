//! Built-in `openapi` renderer (SPEC §10.2).
//!
//! Turns a Kubernetes `CustomResourceDefinition` (one or more YAML documents per file) or an
//! `OpenAPI` 3.x document's `components.schemas` into reference pages: one page per served CRD
//! version (storage version first) or one page per schema.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

/// Errors raised while rendering a CRD or `OpenAPI` file.
#[derive(Debug, Error)]
pub enum OpenapiError {
    /// The file is not valid YAML (JSON is a YAML subset, so this covers both).
    #[error("{path}: invalid YAML: {source}")]
    Yaml {
        /// The file path, for the error message.
        path: String,
        /// Underlying YAML error.
        #[source]
        source: serde_yaml_ng::Error,
    },
    /// A document could not be reinterpreted as JSON (should not happen for YAML/JSON input).
    #[error("{path}: {message}")]
    Shape {
        /// The file path, for the error message.
        path: String,
        /// What was wrong.
        message: String,
    },
}

fn shape(path: &str, message: impl Into<String>) -> OpenapiError {
    OpenapiError::Shape {
        path: path.to_string(),
        message: message.into(),
    }
}

/// One page produced from a CRD or `OpenAPI` document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedPage {
    /// Path relative to the artifact source directory, e.g. `reference/g/kind-v1.md`.
    pub path: String,
    /// `<Kind> (<group>/<version>)` for a CRD page, the schema name for an `OpenAPI` page.
    pub title: String,
    /// Always `reference` (SPEC §10.2).
    pub doc_type: String,
    /// `<group>` for a CRD page, empty for an `OpenAPI` schema page.
    pub section: String,
    /// The rendered Markdown.
    pub content: String,
}

/// One row of a Fields or Status table.
struct FieldRow {
    path: String,
    ty: String,
    required: bool,
    values: String,
    description: String,
}

/// One row of a Conditions table.
struct ConditionRow {
    kind: String,
    description: String,
}

/// Collapse whitespace (including newlines) to single spaces, as SPEC §10.2 requires for
/// descriptions.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn description_of(schema: &Value) -> String {
    schema
        .get("description")
        .and_then(Value::as_str)
        .map(collapse)
        .unwrap_or_default()
}

/// Values column: enum members (backtick-quoted) plus a note when unknown fields are preserved.
fn values_of(schema: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        let rendered: Vec<String> = values
            .iter()
            .filter_map(Value::as_str)
            .map(|v| format!("`{v}`"))
            .collect();
        if !rendered.is_empty() {
            parts.push(rendered.join(", "));
        }
    }
    if schema
        .get("x-kubernetes-preserve-unknown-fields")
        .and_then(Value::as_bool)
        == Some(true)
    {
        parts.push("preserves unknown fields".to_string());
    }
    parts.join("; ")
}

/// The schema's declared or inferred type: `type`, else `object`/`array` when `properties`,
/// `additionalProperties` or `items` say so, else empty (an unconstrained `{}` schema).
fn schema_type(schema: &Value) -> &str {
    if let Some(ty) = schema.get("type").and_then(Value::as_str) {
        return ty;
    }
    if schema.get("properties").is_some() || schema.get("additionalProperties").is_some() {
        return "object";
    }
    if schema.get("items").is_some() {
        return "array";
    }
    ""
}

fn required_keys(schema: &Value) -> BTreeSet<&str> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

fn join_path(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_string()
    } else {
        format!("{parent}.{key}")
    }
}

/// Flatten `schema` at `path` into `rows` (SPEC §10.2): nested objects flattened with dotted
/// paths, arrays as `items[]`, `additionalProperties` maps as `<path>.*`.
fn walk(path: &str, schema: &Value, required: bool, rows: &mut Vec<FieldRow>) {
    match schema_type(schema) {
        "object" => walk_object(path, schema, required, rows),
        "array" => match schema.get("items") {
            Some(items) => walk(&format!("{path}[]"), items, required, rows),
            None => rows.push(leaf(&format!("{path}[]"), "", required, schema)),
        },
        ty => rows.push(leaf(path, ty, required, schema)),
    }
}

fn walk_object(path: &str, schema: &Value, required: bool, rows: &mut Vec<FieldRow>) {
    if let Some(properties) = schema.get("properties").and_then(Value::as_object)
        && !properties.is_empty()
    {
        let required_here = required_keys(schema);
        for (key, sub) in properties {
            walk(
                &join_path(path, key),
                sub,
                required_here.contains(key.as_str()),
                rows,
            );
        }
        return;
    }
    match schema.get("additionalProperties") {
        Some(additional) if additional.is_object() => {
            walk(&format!("{path}.*"), additional, false, rows);
        }
        _ => rows.push(leaf(path, "object", required, schema)),
    }
}

fn leaf(path: &str, ty: &str, required: bool, schema: &Value) -> FieldRow {
    FieldRow {
        path: path.to_string(),
        ty: ty.to_string(),
        required,
        values: values_of(schema),
        description: description_of(schema),
    }
}

/// Flatten `status` into its own rows, splitting `conditions` into condition rows.
fn walk_status(status: &Value) -> (Vec<FieldRow>, Vec<ConditionRow>) {
    let mut fields = Vec::new();
    let mut conditions = Vec::new();
    if let Some(properties) = status.get("properties").and_then(Value::as_object) {
        let required_here = required_keys(status);
        for (key, sub) in properties {
            if key == "conditions" {
                conditions = conditions_of(sub);
                continue;
            }
            walk(key, sub, required_here.contains(key.as_str()), &mut fields);
        }
    }
    (fields, conditions)
}

/// Condition rows from `status.conditions[].type`'s enum, when present (SPEC §10.2).
fn conditions_of(conditions_schema: &Value) -> Vec<ConditionRow> {
    let Some(type_schema) = conditions_schema
        .get("items")
        .and_then(|items| items.get("properties"))
        .and_then(|props| props.get("type"))
    else {
        return Vec::new();
    };
    let Some(values) = type_schema.get("enum").and_then(Value::as_array) else {
        return Vec::new();
    };
    let description = description_of(type_schema);
    values
        .iter()
        .filter_map(Value::as_str)
        .map(|kind| ConditionRow {
            kind: kind.to_string(),
            description: description.clone(),
        })
        .collect()
}

fn escape_cell(text: &str) -> String {
    text.replace('|', "\\|")
}

fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut out = format!(
        "| {} |\n|{}\n",
        headers.join(" | "),
        "---|".repeat(headers.len())
    );
    for row in rows {
        let cells: Vec<String> = row.iter().map(|c| escape_cell(c)).collect();
        let _ = writeln!(out, "| {} |", cells.join(" | "));
    }
    out
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// Render one CRD version's page (SPEC §10.2 page format).
#[allow(clippy::too_many_arguments)]
fn render_crd_page(
    kind: &str,
    group: &str,
    version: &str,
    description: &str,
    scope: &str,
    plural: &str,
    short_names: &[String],
    storage: bool,
    fields: &[FieldRow],
    status: &[FieldRow],
    conditions: &[ConditionRow],
) -> String {
    let mut out = format!("# {kind} ({group}/{version})\n\n{description}\n\n");
    let _ = writeln!(
        out,
        "Scope: {scope} · Plural: {plural} · Short names: {} · Served: yes · Storage: {}\n",
        short_names.join(", "),
        yes_no(storage),
    );
    out.push_str("## Fields\n\n");
    let field_rows: Vec<Vec<String>> = fields
        .iter()
        .map(|f| {
            vec![
                format!("`{}`", f.path),
                f.ty.clone(),
                yes_no(f.required).to_string(),
                f.values.clone(),
                f.description.clone(),
            ]
        })
        .collect();
    out.push_str(&table(
        &["Field", "Type", "Required", "Values", "Description"],
        &field_rows,
    ));
    if !status.is_empty() {
        out.push_str("\n## Status\n\n");
        let status_rows: Vec<Vec<String>> = status
            .iter()
            .map(|f| {
                vec![
                    format!("`{}`", f.path),
                    f.ty.clone(),
                    f.values.clone(),
                    f.description.clone(),
                ]
            })
            .collect();
        out.push_str(&table(
            &["Field", "Type", "Values", "Description"],
            &status_rows,
        ));
    }
    if !conditions.is_empty() {
        out.push_str("\n## Conditions\n\n");
        let condition_rows: Vec<Vec<String>> = conditions
            .iter()
            .map(|c| vec![format!("`{}`", c.kind), c.description.clone()])
            .collect();
        out.push_str(&table(&["Type", "Description"], &condition_rows));
    }
    out
}

/// Render every served version of one `CustomResourceDefinition` document, storage version
/// first.
fn render_crd(path: &str, doc: &Value) -> Result<Vec<GeneratedPage>, OpenapiError> {
    let spec = doc
        .get("spec")
        .ok_or_else(|| shape(path, "CustomResourceDefinition has no spec"))?;
    let group = str_field(spec, "group");
    let names = spec.get("names");
    let kind = names
        .and_then(|n| n.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let plural = names
        .and_then(|n| n.get("plural"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let short_names: Vec<String> = names
        .and_then(|n| n.get("shortNames"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let scope = str_field(spec, "scope");
    let versions = spec
        .get("versions")
        .and_then(Value::as_array)
        .ok_or_else(|| shape(path, "CustomResourceDefinition has no spec.versions"))?;
    let mut ordered: Vec<&Value> = versions
        .iter()
        .filter(|v| v.get("served").and_then(Value::as_bool).unwrap_or(false))
        .collect();
    if let Some(pos) = ordered
        .iter()
        .position(|v| v.get("storage").and_then(Value::as_bool).unwrap_or(false))
    {
        let storage_version = ordered.remove(pos);
        ordered.insert(0, storage_version);
    }

    let mut pages = Vec::new();
    for version in ordered {
        let name = str_field(version, "name");
        let storage = version
            .get("storage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let schema = version.get("schema").and_then(|s| s.get("openAPIV3Schema"));
        let description = schema.map(description_of).unwrap_or_default();
        let mut fields = Vec::new();
        let mut status = Vec::new();
        let mut conditions = Vec::new();
        if let Some(properties) = schema
            .and_then(|s| s.get("properties"))
            .and_then(Value::as_object)
        {
            let required_here = schema.map(required_keys).unwrap_or_default();
            for (key, sub) in properties {
                match key.as_str() {
                    "status" => (status, conditions) = walk_status(sub),
                    "metadata" | "apiVersion" | "kind" => {}
                    _ => walk(key, sub, required_here.contains(key.as_str()), &mut fields),
                }
            }
        }
        let content = render_crd_page(
            kind,
            group,
            name,
            &description,
            scope,
            plural,
            &short_names,
            storage,
            &fields,
            &status,
            &conditions,
        );
        pages.push(GeneratedPage {
            path: format!("reference/{group}/{}-{name}.md", kind.to_lowercase()),
            title: format!("{kind} ({group}/{name})"),
            doc_type: "reference".to_string(),
            section: group.to_string(),
            content,
        });
    }
    Ok(pages)
}

/// Render every schema under `components.schemas` of an `OpenAPI` 3.x document.
fn render_openapi_document(doc: &Value) -> Vec<GeneratedPage> {
    let Some(schemas) = doc
        .get("components")
        .and_then(|c| c.get("schemas"))
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    schemas
        .iter()
        .map(|(name, schema)| {
            let mut fields = Vec::new();
            walk_object("", schema, false, &mut fields);
            let description = description_of(schema);
            let field_rows: Vec<Vec<String>> = fields
                .iter()
                .map(|f| {
                    vec![
                        format!("`{}`", f.path),
                        f.ty.clone(),
                        yes_no(f.required).to_string(),
                        f.values.clone(),
                        f.description.clone(),
                    ]
                })
                .collect();
            let content = format!(
                "# {name}\n\n{description}\n\n## Fields\n\n{}",
                table(
                    &["Field", "Type", "Required", "Values", "Description"],
                    &field_rows
                )
            );
            GeneratedPage {
                path: format!("reference/{}.md", name.to_lowercase()),
                title: name.clone(),
                doc_type: "reference".to_string(),
                section: String::new(),
                content,
            }
        })
        .collect()
}

/// Render every recognised document in `text` (a CRD or `OpenAPI` 3.x file, possibly several YAML
/// documents). Documents that are neither a `CustomResourceDefinition` nor an `OpenAPI` 3.x
/// document are silently skipped, so that a glob covering unrelated YAML does not fail the
/// source; such an input simply produces no page and is reported as `unrendered`.
pub fn render(path: &str, text: &str) -> Result<Vec<GeneratedPage>, OpenapiError> {
    let mut pages = Vec::new();
    for document in serde_yaml_ng::Deserializer::from_str(text) {
        let value =
            serde_yaml_ng::Value::deserialize(document).map_err(|source| OpenapiError::Yaml {
                path: path.to_string(),
                source,
            })?;
        if value.is_null() {
            continue;
        }
        let value: Value = serde_json::to_value(&value).map_err(|e| shape(path, e.to_string()))?;
        if value.get("kind").and_then(Value::as_str) == Some("CustomResourceDefinition") {
            pages.extend(render_crd(path, &value)?);
        } else if value
            .get("openapi")
            .and_then(Value::as_str)
            .is_some_and(|v| v.starts_with('3'))
        {
            pages.extend(render_openapi_document(&value));
        }
    }
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CRD: &str = include_str!("../../tests/fixtures/crds/subscriptions.yaml");

    #[test]
    fn renders_the_storage_version_first_and_skips_unserved_versions() {
        let pages = render("config/crd/subscriptions.yaml", CRD).unwrap();
        assert_eq!(pages.len(), 2, "v1beta1 is not served");
        assert_eq!(pages[0].title, "Subscription (messaging.example.com/v1)");
        assert_eq!(
            pages[1].title,
            "Subscription (messaging.example.com/v1alpha1)"
        );
        assert_eq!(pages[0].doc_type, "reference");
        assert_eq!(pages[0].section, "messaging.example.com");
        assert_eq!(
            pages[0].path,
            "reference/messaging.example.com/subscription-v1.md"
        );
    }

    #[test]
    fn field_table_covers_required_arrays_maps_and_preserved_fields() {
        let pages = render("x.yaml", CRD).unwrap();
        let v1 = &pages[0].content;
        assert!(v1.starts_with("# Subscription (messaging.example.com/v1)\n\n"));
        assert!(v1.contains("A Subscription describes interest in a class of events."));
        assert!(v1.contains(
            "Scope: Namespaced · Plural: subscriptions · Short names: sub, subs · Served: yes · Storage: yes"
        ));
        assert!(v1.contains("| `spec.sink` | string | yes |  | The URL of the subscriber. |"));
        assert!(v1.contains(
            "| `spec.typeMatching` | string | no | `exact`, `standard` | How the event type is matched. |"
        ));
        assert!(v1.contains("| `spec.filters[].eventType` | string | no |  |  |"));
        assert!(v1.contains("| `spec.config.*` | string | no |  |  |"));
        assert!(v1.contains("| `spec.extra` | object | no | preserves unknown fields |  |"));
        assert!(v1.contains("## Status"));
        assert!(v1.contains("| `ready` | boolean |  |  |"));
        assert!(v1.contains("## Conditions"));
        assert!(v1.contains("| `Ready` | The kind of condition. |"));
        assert!(v1.contains("| `Subscribed` | The kind of condition. |"));

        let old = &pages[1].content;
        assert!(old.contains("Storage: no"));
        assert!(!old.contains("## Status"), "v1alpha1 has no status");
    }

    #[test]
    fn non_crd_non_openapi_documents_are_skipped() {
        assert_eq!(render("x.yaml", "kind: Pod\n").unwrap(), Vec::new());
        assert_eq!(render("x.yaml", "---\n---\n").unwrap(), Vec::new());
    }

    #[test]
    fn multi_document_yaml_renders_every_crd() {
        let two_docs = format!("{CRD}\n---\n{CRD}");
        let pages = render("x.yaml", &two_docs).unwrap();
        assert_eq!(pages.len(), 4);
    }

    #[test]
    fn missing_spec_or_versions_is_an_error() {
        let err = render("x.yaml", "kind: CustomResourceDefinition\n").unwrap_err();
        assert!(matches!(err, OpenapiError::Shape { .. }), "{err}");
        let err = render(
            "x.yaml",
            "kind: CustomResourceDefinition\nspec:\n  group: g\n",
        )
        .unwrap_err();
        assert!(matches!(err, OpenapiError::Shape { .. }), "{err}");
    }

    #[test]
    fn invalid_yaml_is_an_error() {
        let err = render("x.yaml", "kind: [unterminated\n").unwrap_err();
        assert!(matches!(err, OpenapiError::Yaml { .. }), "{err}");
    }

    const OPENAPI: &str = include_str!("../../tests/fixtures/openapi/widget.yaml");

    #[test]
    fn openapi_document_renders_one_page_per_schema() {
        let pages = render("api.yaml", OPENAPI).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].title, "Widget");
        assert_eq!(pages[0].path, "reference/widget.md");
        assert_eq!(pages[0].section, "");
        assert!(pages[0].content.contains("A small widget."));
        assert!(
            pages[0]
                .content
                .contains("| `name` | string | yes |  | Display name. |")
        );
        assert!(
            pages[0]
                .content
                .contains("| `tags[]` | string | no |  |  |")
        );
    }

    /// Write `rendered` over the snapshot when `UPDATE_SNAPSHOTS` is set, then return the
    /// expected text so a refreshed snapshot passes in the same run (mirrors `report::tests`).
    fn snapshot(name: &str, rendered: &str, expected: &'static str) -> String {
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
            let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/snapshots/");
            std::fs::write(format!("{path}{name}"), rendered).unwrap();
            return rendered.to_string();
        }
        expected.to_string()
    }

    #[test]
    fn storage_version_page_matches_snapshot() {
        let pages = render("config/crd/bases/subscriptions.yaml", CRD).unwrap();
        let rendered = &pages[0].content;
        let expected = include_str!("../../tests/snapshots/crd_subscription_v1.md");
        let expected = snapshot("crd_subscription_v1.md", rendered, expected);
        assert_eq!(rendered, &expected, "rendered page:\n{rendered}");
    }
}
