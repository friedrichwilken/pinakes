pub(crate) mod check;
pub(crate) mod chunks;
pub(crate) mod classify;
pub(crate) mod decide;
pub(crate) mod diff;
pub(crate) mod duplicates;
pub(crate) mod init;
pub(crate) mod moved;
pub(crate) mod report;
pub(crate) mod residue;
pub(crate) mod resolve;
pub(crate) mod usage;
pub(crate) mod verify;

/// Exit code for a violated gate in `check`.
pub(crate) const EXIT_GATE: u8 = 2;
/// Exit code for `diff` differences and a stale manifest in `verify`.
pub(crate) const EXIT_DIFFERENCES: u8 = 3;
/// Exit code for a policy violation in `verify`.
pub(crate) const EXIT_POLICY: u8 = 4;
