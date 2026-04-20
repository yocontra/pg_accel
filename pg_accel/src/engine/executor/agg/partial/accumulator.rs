//! [`ColumnAccumulator`] — per-column state populated during a scan and
//! consumed by [`super::PartialEmitter`] at finalize time.
//!
//! The fields cover the union of transition states needed by every supported
//! partial emitter: numeric sums (`sum`, `sum_comp`), stats (`sum_sq`), COUNT
//! (`count`), MIN/MAX (`min_val` / `max_val`), bitwise reductions (`bit_acc`)
//! and boolean reductions (`bool_acc`). The `has_value` flag distinguishes
//! "zero observations" from "zero sum of observations".

/// Per-column accumulator populated as rows flow through the scan.
///
/// All fields default to zero-ish values so [`Default::default`] yields a
/// well-defined "empty" accumulator. Bitwise AND must explicitly initialise
/// `bit_acc` to `!0` at combine time — callers are expected to do so (see
/// `BitReductionEmitter` for the convention).
#[derive(Debug, Default, Clone)]
pub struct ColumnAccumulator {
    /// Running sum (for SUM/AVG; stored as f64 for numeric width).
    pub sum: f64,
    /// Kahan compensation term for `sum`.
    pub sum_comp: f64,
    /// Running sum-of-squares (for STDDEV/VARIANCE).
    pub sum_sq: f64,
    /// Observation count (for COUNT / AVG / stats).
    pub count: u64,
    /// Running MIN.
    pub min_val: f64,
    /// Running MAX.
    pub max_val: f64,
    /// True once at least one non-null observation has been accumulated.
    pub has_value: bool,
    /// Integer bitwise accumulator for BIT_AND / BIT_OR.
    ///
    /// Callers combining BIT_AND across multiple rows should initialise this
    /// to `!0` (all ones) before the first combine when `has_value == false`;
    /// BIT_OR can start at `0`.
    pub bit_acc: i64,
    /// Boolean accumulator for BOOL_AND / BOOL_OR.
    ///
    /// Meaningful only when `has_value == true`. Default `false` is safe for
    /// BOOL_OR; BOOL_AND callers should seed `true` on first observation.
    pub bool_acc: bool,
}
