use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use pinakes::commands::{self, EmbedOptions, Paths};
use pinakes::embed::HttpEmbedder;

#[derive(Args)]
pub(crate) struct EmbedArgs {
    /// Artifact directory (default: `artifact` next to the config).
    #[arg(long)]
    artifact: Option<PathBuf>,
    /// The embedding model, sent to the endpoint and recorded in `embeddings.json` (default:
    /// `PINAKES_EMBED_MODEL`).
    #[arg(long, value_name = "NAME")]
    model: Option<String>,
    /// `embeddings.bin` output path (default: `embeddings.bin` next to the config);
    /// `embeddings.json` is written next to it.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
    /// Texts per embeddings request.
    #[arg(long, default_value_t = pinakes::embed::DEFAULT_BATCH)]
    batch: usize,
}

pub(crate) fn run_embed(mut paths: Paths, args: EmbedArgs) -> Result<ExitCode> {
    if let Some(dir) = args.artifact {
        paths.artifact = dir;
    }
    let (embedder, model) = HttpEmbedder::from_env(args.model.as_deref())?;
    let options = EmbedOptions {
        model,
        batch: args.batch,
        out: args.out,
    };
    let outcome = commands::embed(&paths, &options, &embedder)?;
    eprintln!(
        "{}: embedded {} units ({} dims) -> {} and {}",
        paths.artifact.display(),
        outcome.units,
        outcome.dimension,
        outcome.bin_path.display(),
        outcome.json_path.display()
    );
    Ok(ExitCode::SUCCESS)
}
