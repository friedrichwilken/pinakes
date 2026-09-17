//! `resolver.type: external` (SPEC §3): run a command in the checkout and parse its JSONL
//! candidate output.

use std::path::Path;
use std::process::Command;

use crate::config::Source;
use crate::jsonl;
use crate::sources::Checkout;
use crate::text::absolutise;

use super::{Candidate, ResolveError};

/// Parse resolver stdout: one JSON object per non-blank line.
pub fn parse_candidates(text: &str) -> Result<Vec<Candidate>, (usize, String)> {
    let mut out = Vec::new();
    for parsed in jsonl::parse_lines::<Candidate>(text) {
        let (line, candidate) = parsed.map_err(|err| (err.line, err.source.to_string()))?;
        if candidate.path.trim().is_empty() {
            return Err((line, "path must not be empty".to_string()));
        }
        out.push(candidate);
    }
    Ok(out)
}

/// Run an external resolver in `checkout` and parse its output.
///
/// Arguments that name an existing file relative to `config_dir` are made absolute so that
/// scripts kept next to `pinakes.yaml` can be referenced by relative path.
pub fn run_external(
    source: &Source,
    command: &[String],
    args: &[String],
    checkout: &Checkout,
    config_dir: &Path,
) -> Result<Vec<Candidate>, ResolveError> {
    let program = absolutise(config_dir, &command[0]);
    let rest: Vec<String> = command[1..]
        .iter()
        .chain(args)
        .map(|a| absolutise(config_dir, a))
        .collect();
    let output = Command::new(&program)
        .args(&rest)
        .current_dir(&checkout.root)
        .env("PINAKES_SOURCE", &source.name)
        .env("PINAKES_COMMIT", &checkout.commit)
        .output()
        .map_err(|error| ResolveError::ResolverSpawn {
            name: source.name.clone(),
            command: program.clone(),
            error,
        })?;
    if !output.status.success() {
        return Err(ResolveError::ResolverFailed {
            name: source.name.clone(),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr)
                .trim_end()
                .to_string(),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_candidates(&stdout).map_err(|(line, message)| ResolveError::ResolverOutput {
        name: source.name.clone(),
        line,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_candidates_defaults_selected_to_true() {
        let parsed =
            parse_candidates("{\"path\":\"a.md\"}\n\n{\"path\":\"b.md\",\"selected\":false}\n")
                .unwrap();
        assert!(parsed[0].selected);
        assert!(!parsed[1].selected);
        assert_eq!(parse_candidates("{\"path\":\"\"}").unwrap_err().0, 1);
    }
}
