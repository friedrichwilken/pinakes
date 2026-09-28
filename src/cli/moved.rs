//! The subcommands that moved to `kanon` (SPEC §21): each prints one line naming the `kanon`
//! command to run instead and exits 1. They stay for one minor release, then go.

use std::process::ExitCode;

use clap::Args;

/// Whatever followed the moved subcommand: accepted so an old invocation reaches the message
/// instead of a usage error, then ignored.
#[derive(Args)]
#[command(disable_help_flag = true)]
pub(crate) struct MovedArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
    _rest: Vec<String>,
}

/// Print where `command` went and fail.
pub(crate) fn run_moved(command: &str) -> ExitCode {
    eprintln!("pinakes {command} moved to kanon: run `kanon {command}` instead (SPEC §21)");
    ExitCode::from(1)
}
