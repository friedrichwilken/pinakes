//! The one `usize -> f64` cast used by every ratio and average in the crate.

/// Cast a count to `f64` for use in a ratio or average.
///
/// The loss only matters once `n` exceeds 2^53, far beyond any page, token or query count
/// this tool ever counts.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn float(n: usize) -> f64 {
    n as f64
}
