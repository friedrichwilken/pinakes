use std::path::PathBuf;

use crate::config::Config;
use crate::embed::{self, Embedder};
use crate::error::CommandError;
use crate::index::{self, Priorities};
use crate::workspace::Paths;

// -------------------------------------------------------------------------------------------
// embed (SPEC §16.2)
// -------------------------------------------------------------------------------------------

/// Options for `embed`.
#[derive(Debug, Clone, Default)]
pub struct EmbedOptions {
    /// The embedding model name, recorded in `embeddings.json`.
    pub model: String,
    /// Texts per embedding request (SPEC §16.2 default: [`embed::DEFAULT_BATCH`]).
    pub batch: usize,
    /// `embeddings.bin` output path (default: `paths.embeddings`); `embeddings.json` is written
    /// next to it, with a `.json` extension.
    pub out: Option<PathBuf>,
}

/// What `embed` wrote.
#[derive(Debug)]
pub struct EmbedOutcome {
    /// Retrieval units embedded.
    pub units: usize,
    /// Vector length.
    pub dimension: usize,
    /// `embeddings.bin` path.
    pub bin_path: PathBuf,
    /// `embeddings.json` path.
    pub json_path: PathBuf,
}

/// Run `embed`: one embedding per retrieval unit of the artifact, through `embedder`.
///
/// The config is optional, exactly as for `eval`: priorities default to
/// [`Priorities::default`] without one.
pub fn embed(
    paths: &Paths,
    options: &EmbedOptions,
    embedder: &dyn Embedder,
) -> Result<EmbedOutcome, CommandError> {
    let priorities = if paths.config.is_file() {
        Priorities::from_config(&Config::load(&paths.config)?)
    } else {
        Priorities::default()
    };
    let mut pages = index::load_pages(&paths.artifact, &priorities)?;
    index::mark_mirrors(&mut pages);
    let units = index::iter_units(&pages);
    let texts: Vec<String> = units.iter().map(|u| u.text.clone()).collect();
    let batch = if options.batch == 0 {
        embed::DEFAULT_BATCH
    } else {
        options.batch
    };
    let vectors = embed::embed_units(embedder, &options.model, &texts, batch)?;
    let dimension = vectors.first().map_or(0, Vec::len);
    let manifest = embed::EmbeddingsManifest {
        model: options.model.clone(),
        dimension,
        unit_ids: units.into_iter().map(|u| u.page_id).collect(),
        manifest_sha256: embed::artifact_manifest_hash(&paths.artifact)?,
    };
    let bin_path = options
        .out
        .clone()
        .unwrap_or_else(|| paths.embeddings.clone());
    let json_path = bin_path.with_extension("json");
    embed::write_embeddings(&bin_path, &json_path, &manifest, &vectors)?;
    Ok(EmbedOutcome {
        units: manifest.unit_ids.len(),
        dimension,
        bin_path,
        json_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testing::eval_workspace;

    #[test]
    fn embed_writes_units_for_every_searchable_page() {
        let (_dir, paths) = eval_workspace();
        let options = EmbedOptions {
            model: "fake".to_string(),
            batch: 1,
            out: None,
        };
        let outcome = embed(&paths, &options, &crate::embed::testing::FakeEmbedder).unwrap();
        assert_eq!(outcome.units, 1, "one intro unit on the single page");
        assert_eq!(outcome.dimension, crate::embed::testing::FAKE_DIMENSION);
        assert_eq!(outcome.bin_path, paths.embeddings);
        assert_eq!(outcome.json_path, paths.embeddings.with_extension("json"));
        let (manifest, vectors) =
            crate::embed::read_embeddings(&outcome.bin_path, &outcome.json_path).unwrap();
        assert_eq!(manifest.unit_ids, ["handbook::docs/user/README.md"]);
        assert_eq!(vectors.len(), 1);
    }
}
