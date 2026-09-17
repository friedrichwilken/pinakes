use std::io::Write as _;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Subcommand;

use pinakes::commands::{self, Paths};
use pinakes::residue::{ListFilter, Reason};

#[derive(Subcommand)]
pub(crate) enum ResidueCommand {
    /// Print undecided residue entries as JSONL.
    List {
        /// Only entries from this source.
        #[arg(long)]
        source: Option<String>,
        /// Only entries with this reason.
        #[arg(long, value_name = "not_selected|unresolved_link|new_source|excluded")]
        reason: Option<String>,
        /// Also show `excluded` entries (kept out by `policy.deny`, a resolver's `exclude`, a
        /// decision, or an archived source): hidden by default since they are never candidates
        /// for `decide` (SPEC §2.4).
        #[arg(long)]
        include_excluded: bool,
    },
}

pub(crate) fn run_residue_list(
    paths: &Paths,
    source: Option<&str>,
    reason: Option<&str>,
    include_excluded: bool,
) -> Result<ExitCode> {
    let reason = match reason {
        Some(text) => Some(Reason::parse(text).with_context(|| {
            format!(
                "unknown reason {text:?}: expected not_selected, unresolved_link, new_source or excluded"
            )
        })?),
        None => None,
    };
    let filter = ListFilter {
        source,
        reason,
        include_excluded,
    };
    let entries = commands::residue_list(paths, &filter)?;
    let mut out = std::io::stdout().lock();
    out.write_all(pinakes::residue::to_jsonl(&entries)?.as_bytes())?;
    Ok(ExitCode::SUCCESS)
}
