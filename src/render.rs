//! Render hook (SPEC §10.1): turning selected pages that are not already Markdown into
//! Markdown pages, after selection and before the artifact is written.
//!
//! Two render types exist: [`crate::config::Render::External`], an external command following
//! the JSONL contract of SPEC §10.1, and [`crate::config::Render::Openapi`], the built-in CRD /
//! `OpenAPI` schema renderer of SPEC §10.2 (see [`openapi`]).
//!
//! A generated page is written into the source's checkout at its output path, so the ordinary
//! artifact-copy path in [`crate::artifact::materialise`] picks it up exactly like a selected
//! Markdown file; only its manifest entry differs, carrying `rendered_from` and a hash of the
//! *rendered* Markdown rather than the original file.

pub mod openapi;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;
use thiserror::Error;

use crate::config::Render;
use crate::jsonl;
use crate::manifest::PageEntry;
use crate::sources::Checkout;
use crate::text::{absolutise, sha256_hex};

/// Errors raised while running a source's render step.
#[derive(Debug, Error)]
pub enum RenderError {
    /// The external render command could not be started.
    #[error("source {name}: cannot run {command:?}: {error}")]
    Spawn {
        /// Source name.
        name: String,
        /// The program that failed to start.
        command: String,
        /// Underlying I/O error.
        error: std::io::Error,
    },
    /// The external render command exited with a non-zero status.
    #[error("source {name}: render exited with {status}\n{stderr}")]
    Failed {
        /// Source name.
        name: String,
        /// Exit status as reported by the OS.
        status: String,
        /// Everything the command wrote to stderr.
        stderr: String,
    },
    /// A render command wrote a line that is not a valid generated page.
    #[error("source {name}: render output line {line}: {message}")]
    Output {
        /// Source name.
        name: String,
        /// One-based line number of stdout.
        line: usize,
        /// What was wrong.
        message: String,
    },
    /// A generated page names an output path outside `PINAKES_OUT` (SPEC §10.1).
    #[error("source {name}: generated page path {path:?} escapes the output directory")]
    PathEscape {
        /// Source name.
        name: String,
        /// The offending path.
        path: String,
    },
    /// A generated page names a source path that was not among the selected pages.
    #[error("source {name}: generated page names {path:?}, which was not selected")]
    UnknownSourcePath {
        /// Source name.
        name: String,
        /// The offending source path.
        path: String,
    },
    /// A file could not be read or written while rendering.
    #[error("{path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The built-in `openapi` renderer failed.
    #[error(transparent)]
    Openapi(#[from] openapi::OpenapiError),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> RenderError + '_ {
    move |source| RenderError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// One page as described on the render command's stdout (SPEC §10.1).
#[derive(Debug, Clone, Deserialize)]
struct GeneratedLine {
    path: String,
    source_path: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    doc_type: String,
    #[serde(default)]
    section: String,
}

/// One page produced by either render type, with its content already read into memory.
struct Generated {
    path: String,
    source_path: String,
    title: String,
    doc_type: String,
    section: String,
    content: String,
}

/// What a source's render step produced.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderOutput {
    /// Rendered pages by output path (relative to the artifact source directory).
    pub pages: BTreeMap<String, PageEntry>,
    /// Selected paths that no generated page named (SPEC §10.1).
    pub unrendered: Vec<String>,
    /// The render configuration as actually run: for [`Render::External`], with `command` and
    /// `args` absolutised against the config file's directory, exactly as recorded in the
    /// manifest (SPEC §2.2) so `resolve --from-manifest` can re-run the same program later.
    pub recorded: Render,
}

/// Absolutise an external render command's paths against `config_dir`, the way it is actually
/// run (SPEC §10.1); [`Render::Openapi`] has no paths and is returned unchanged. This is what
/// gets recorded per source in the manifest, so a later `resolve --from-manifest` re-runs the
/// exact program regardless of where the manifest is reproduced from.
pub fn recorded_render(render: &Render, config_dir: &Path) -> Render {
    match render {
        Render::External { command, args } => Render::External {
            command: command.iter().map(|c| absolutise(config_dir, c)).collect(),
            args: args.iter().map(|a| absolutise(config_dir, a)).collect(),
        },
        Render::Openapi => Render::Openapi,
    }
}

/// A path is safe to write under a source's checkout: relative, and never leaving it.
fn is_safe_relative(path: &str) -> bool {
    let candidate = Path::new(path);
    !path.is_empty()
        && candidate.is_relative()
        && !candidate
            .components()
            .any(|c| matches!(c, Component::ParentDir))
}

/// Run a source's render step over its `selected` pages, writing generated Markdown into
/// `checkout.root` at each page's output path. `name` identifies the source in error messages
/// and, for [`Render::External`], the `PINAKES_SOURCE` environment variable; `config_dir` is the
/// directory an external command's paths are resolved against (SPEC §10.1). Pass the config's
/// `render` value on a fresh resolve, or a value already recorded in the manifest (already
/// absolute, so `config_dir` is then irrelevant) to reproduce one.
pub fn render_source(
    name: &str,
    render: &Render,
    checkout: &Checkout,
    selected: &BTreeMap<String, PageEntry>,
    config_dir: &Path,
) -> Result<RenderOutput, RenderError> {
    let recorded = recorded_render(render, config_dir);
    let generated = match &recorded {
        Render::External { command, args } => {
            run_external(name, command, args, checkout, selected)?
        }
        Render::Openapi => run_openapi(checkout, selected)?,
    };

    let mut pages = BTreeMap::new();
    let mut rendered_sources = BTreeSet::new();
    for page in generated {
        if !selected.contains_key(&page.source_path) {
            return Err(RenderError::UnknownSourcePath {
                name: name.to_string(),
                path: page.source_path,
            });
        }
        if !is_safe_relative(&page.path) {
            return Err(RenderError::PathEscape {
                name: name.to_string(),
                path: page.path,
            });
        }
        let original = &selected[&page.source_path];
        let dest = checkout.root.join(&page.path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(io(parent))?;
        }
        std::fs::write(&dest, page.content.as_bytes()).map_err(io(&dest))?;
        rendered_sources.insert(page.source_path.clone());
        pages.insert(
            page.path,
            PageEntry {
                sha256: sha256_hex(page.content.as_bytes()),
                title: page.title,
                doc_type: page.doc_type,
                section: page.section,
                selected_by: original.selected_by,
                rendered_from: Some(page.source_path),
            },
        );
    }
    let unrendered = selected
        .keys()
        .filter(|path| !rendered_sources.contains(path.as_str()))
        .cloned()
        .collect();
    Ok(RenderOutput {
        pages,
        unrendered,
        recorded,
    })
}

/// Run the external render contract (SPEC §10.1) and read every generated page's content back
/// from the staging directory it was asked to write into. `command` and `args` are taken as
/// given: the caller has already absolutised them (see [`recorded_render`]).
fn run_external(
    name: &str,
    command: &[String],
    args: &[String],
    checkout: &Checkout,
    selected: &BTreeMap<String, PageEntry>,
) -> Result<Vec<Generated>, RenderError> {
    let staging = tempfile::tempdir().map_err(io(Path::new("render staging dir")))?;
    let program = command[0].clone();
    let rest: Vec<String> = command[1..].iter().chain(args).cloned().collect();
    let mut child = Command::new(&program)
        .args(&rest)
        .current_dir(&checkout.root)
        .env("PINAKES_SOURCE", name)
        .env("PINAKES_COMMIT", &checkout.commit)
        .env("PINAKES_OUT", staging.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| RenderError::Spawn {
            name: name.to_string(),
            command: program.clone(),
            error,
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        for path in selected.keys() {
            let _ = writeln!(stdin, "{}", serde_json::json!({ "path": path }));
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|error| RenderError::Spawn {
            name: name.to_string(),
            command: program.clone(),
            error,
        })?;
    if !output.status.success() {
        return Err(RenderError::Failed {
            name: name.to_string(),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr)
                .trim_end()
                .to_string(),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut generated = Vec::new();
    for parsed in jsonl::parse_lines::<GeneratedLine>(&stdout) {
        let (line, parsed) = parsed.map_err(|err| RenderError::Output {
            name: name.to_string(),
            line: err.line,
            message: err.source.to_string(),
        })?;
        if parsed.path.trim().is_empty() || parsed.source_path.trim().is_empty() {
            return Err(RenderError::Output {
                name: name.to_string(),
                line,
                message: "path and source_path must not be empty".to_string(),
            });
        }
        if !is_safe_relative(&parsed.path) {
            return Err(RenderError::PathEscape {
                name: name.to_string(),
                path: parsed.path,
            });
        }
        let file = staging.path().join(&parsed.path);
        let content = std::fs::read_to_string(&file).map_err(io(&file))?;
        generated.push(Generated {
            path: parsed.path,
            source_path: parsed.source_path,
            title: parsed.title,
            doc_type: parsed.doc_type,
            section: parsed.section,
            content,
        });
    }
    Ok(generated)
}

/// Run the built-in `openapi` renderer (SPEC §10.2) over every selected file.
fn run_openapi(
    checkout: &Checkout,
    selected: &BTreeMap<String, PageEntry>,
) -> Result<Vec<Generated>, RenderError> {
    let mut generated = Vec::new();
    for path in selected.keys() {
        let full = checkout.root.join(path);
        let bytes = std::fs::read(&full).map_err(io(&full))?;
        let text = String::from_utf8_lossy(&bytes);
        for page in openapi::render(path, &text)? {
            generated.push(Generated {
                path: page.path,
                source_path: path.clone(),
                title: page.title,
                doc_type: page.doc_type,
                section: page.section,
                content: page.content,
            });
        }
    }
    Ok(generated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Source};
    use crate::manifest::SelectedBy;
    use std::fs;

    const SHA: &str = "4427d7ba863973c2cea9da74ed8675c5c74aee77";

    fn checkout(files: &[(&str, &str)]) -> (tempfile::TempDir, Checkout) {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let full = dir.path().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, content).unwrap();
        }
        let checkout = Checkout {
            root: dir.path().to_path_buf(),
            commit: SHA.to_string(),
        };
        (dir, checkout)
    }

    fn source_with_render(yaml_render: &str) -> Source {
        let text = format!(
            "version: 1\nsources:\n  - name: s\n    repo: https://github.com/o/r\n    ref: main\n    \
             resolver:\n      type: glob\n      include: ['**/*']\n{yaml_render}"
        );
        Config::from_yaml(&text).unwrap().sources.remove(0)
    }

    fn selected(path: &str) -> BTreeMap<String, PageEntry> {
        let mut map = BTreeMap::new();
        map.insert(
            path.to_string(),
            PageEntry {
                sha256: "ignored".to_string(),
                title: String::new(),
                doc_type: String::new(),
                section: String::new(),
                selected_by: SelectedBy::Include,
                rendered_from: None,
            },
        );
        map
    }

    /// An external render command written as a shell script (SPEC §10.1 contract).
    fn external_script(dir: &Path, script: &str) -> Source {
        let path = dir.join("render.sh");
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        source_with_render(&format!(
            "    render:\n      type: external\n      command: ['sh', '{}']\n",
            path.display()
        ))
    }

    #[test]
    fn external_render_contract_produces_pages_and_unrendered() {
        let (_dir, co) = checkout(&[("config/a.yaml", "a: 1\n"), ("config/b.yaml", "b: 1\n")]);
        let script_dir = tempfile::tempdir().unwrap();
        let source = external_script(
            script_dir.path(),
            r#"[ "$PINAKES_SOURCE" = "s" ] || exit 9
[ "$PINAKES_COMMIT" = "4427d7ba863973c2cea9da74ed8675c5c74aee77" ] || exit 9
input=$(cat)
echo "$input" | grep -q 'config/a.yaml' || exit 8
mkdir -p "$PINAKES_OUT/reference"
printf 'rendered from a\n' > "$PINAKES_OUT/reference/a.md"
printf '{"path":"reference/a.md","source_path":"config/a.yaml","title":"A","doc_type":"reference","section":"g"}\n'
"#,
        );
        let mut selected = selected("config/a.yaml");
        selected.insert(
            "config/b.yaml".to_string(),
            PageEntry {
                sha256: "ignored".to_string(),
                title: String::new(),
                doc_type: String::new(),
                section: String::new(),
                selected_by: SelectedBy::Include,
                rendered_from: None,
            },
        );
        let render = source.render.clone().unwrap();
        let output = render_source(&source.name, &render, &co, &selected, Path::new(".")).unwrap();
        assert_eq!(output.pages.len(), 1);
        let page = &output.pages["reference/a.md"];
        assert_eq!(page.title, "A");
        assert_eq!(page.doc_type, "reference");
        assert_eq!(page.section, "g");
        assert_eq!(page.rendered_from.as_deref(), Some("config/a.yaml"));
        assert_eq!(page.sha256, sha256_hex(b"rendered from a\n"));
        assert_eq!(output.unrendered, ["config/b.yaml"]);
        assert_eq!(
            fs::read_to_string(co.root.join("reference/a.md")).unwrap(),
            "rendered from a\n"
        );
    }

    #[test]
    fn external_render_failure_carries_stderr() {
        let (_dir, co) = checkout(&[("config/a.yaml", "a: 1\n")]);
        let script_dir = tempfile::tempdir().unwrap();
        let source = external_script(script_dir.path(), "cat >/dev/null; echo boom >&2; exit 3");
        let render = source.render.clone().unwrap();
        let err = render_source(
            &source.name,
            &render,
            &co,
            &selected("config/a.yaml"),
            Path::new("."),
        )
        .unwrap_err();
        assert!(matches!(err, RenderError::Failed { .. }), "{err}");
    }

    #[test]
    fn render_rejects_unknown_source_paths_and_escaping_output_paths() {
        let (_dir, co) = checkout(&[("config/a.yaml", "a: 1\n")]);
        let script_dir = tempfile::tempdir().unwrap();
        let source = external_script(
            script_dir.path(),
            "cat >/dev/null; printf 'x' > \"$PINAKES_OUT/x.md\"; \
             printf '{\"path\":\"x.md\",\"source_path\":\"config/nope.yaml\"}\\n'",
        );
        let render = source.render.clone().unwrap();
        let err = render_source(
            &source.name,
            &render,
            &co,
            &selected("config/a.yaml"),
            Path::new("."),
        )
        .unwrap_err();
        assert!(
            matches!(err, RenderError::UnknownSourcePath { .. }),
            "{err}"
        );

        let source = external_script(
            script_dir.path(),
            "cat >/dev/null; printf '{\"path\":\"../x.md\",\"source_path\":\"config/a.yaml\"}\\n'",
        );
        let render = source.render.clone().unwrap();
        let err = render_source(
            &source.name,
            &render,
            &co,
            &selected("config/a.yaml"),
            Path::new("."),
        )
        .unwrap_err();
        assert!(matches!(err, RenderError::PathEscape { .. }), "{err}");
    }

    #[test]
    fn openapi_render_type_parses_from_config() {
        let source = source_with_render("    render:\n      type: openapi\n");
        assert!(matches!(source.render, Some(Render::Openapi)));
    }
}
