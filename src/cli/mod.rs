pub(crate) mod check;
pub(crate) mod chunks;
pub(crate) mod classify;
pub(crate) mod decide;
pub(crate) mod diff;
pub(crate) mod duplicates;
pub(crate) mod embed;
pub(crate) mod eval;
pub(crate) mod grade;
pub(crate) mod init;
pub(crate) mod queries;
pub(crate) mod report;
pub(crate) mod residue;
pub(crate) mod resolve;
pub(crate) mod usage;
pub(crate) mod verify;

/// Exit code for a failed `eval --gate` or a violated gate in `check`.
pub(crate) const EXIT_GATE: u8 = 2;
/// Exit code for `diff` differences and a stale manifest in `verify`.
pub(crate) const EXIT_DIFFERENCES: u8 = 3;
/// Exit code for a policy violation in `verify`, or a failed `queries check`.
pub(crate) const EXIT_POLICY: u8 = 4;
