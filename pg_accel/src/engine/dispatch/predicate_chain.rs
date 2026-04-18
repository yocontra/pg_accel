//! Late-materialization predicate chain.
//!
//! Orders predicates by selectivity/cost so the cheapest, most-selective
//! predicate runs first. Rows rejected early skip expensive geometry
//! deserialization entirely.

// ---------------------------------------------------------------------------
// Late Materialization — Predicate Chain
// ---------------------------------------------------------------------------

/// A single predicate in a [`PredicateChain`].
#[derive(Debug, Clone)]
pub struct Predicate {
    /// Human-readable label (e.g. `"bbox_overlap"`, `"st_contains"`).
    pub label: &'static str,
    /// Estimated fraction of rows that *pass* this predicate (0.0–1.0).
    /// Lower values are more selective.
    pub selectivity: f64,
    /// Estimated per-row cost in arbitrary units. Higher means more expensive.
    pub cost: f64,
    /// The evaluation function.  Takes a slice of `(Datum, is_null)` and
    /// returns a boolean mask of the same length (`true` = row passes).
    ///
    /// # Safety
    ///
    /// The function must be safe to call in the context where `evaluate_chain`
    /// is invoked (typically main backend thread).
    pub eval_fn: fn(&[(pgrx::pg_sys::Datum, bool)]) -> Vec<bool>,
}

/// An ordered chain of predicates for late materialization.
///
/// Predicates are sorted by *efficiency* (`selectivity / cost`) so the
/// cheapest, most-selective filter runs first. Rows rejected by an early
/// predicate skip all subsequent (more expensive) predicates, avoiding
/// unnecessary geometry deserialization.
#[derive(Debug, Clone)]
pub struct PredicateChain {
    /// Predicates in evaluation order (cheapest/most-selective first).
    predicates: Vec<Predicate>,
}

impl PredicateChain {
    /// Build a new predicate chain, automatically sorted by efficiency.
    ///
    /// Efficiency is defined as `selectivity / cost`. Lower selectivity (more
    /// rows filtered) and lower cost both increase efficiency, so predicates
    /// that filter the most rows for the least work run first.
    #[must_use]
    pub fn new(mut predicates: Vec<Predicate>) -> Self {
        predicates.sort_by(|a, b| {
            let eff_a = efficiency(a);
            let eff_b = efficiency(b);
            // Lower efficiency value = better (more selective & cheaper).
            eff_a
                .partial_cmp(&eff_b)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Self { predicates }
    }

    /// The ordered list of predicates.
    #[must_use]
    pub fn predicates(&self) -> &[Predicate] {
        &self.predicates
    }

    /// Number of predicates in the chain.
    #[must_use]
    pub fn len(&self) -> usize {
        self.predicates.len()
    }

    /// Whether the chain has no predicates.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.predicates.is_empty()
    }
}

/// Efficiency metric: `selectivity / cost`. Lower is better — it means we
/// filter more rows for less work.
pub(crate) fn efficiency(p: &Predicate) -> f64 {
    if p.cost <= 0.0 {
        return 0.0;
    }
    p.selectivity / p.cost
}

/// Evaluate a [`PredicateChain`] against a batch, applying predicates in
/// efficiency order and short-circuiting rejected rows.
///
/// Returns a boolean mask of length `batch.len()` where `true` means the row
/// passed **all** predicates.
///
/// # Late Materialization
///
/// This is the key optimisation: an early, cheap predicate (e.g. integer range
/// check or bounding-box overlap) can eliminate rows before an expensive
/// predicate (e.g. exact `ST_Contains` requiring full geometry deserialization)
/// ever sees them.
#[must_use]
pub fn evaluate_chain(chain: &PredicateChain, batch: &[(pgrx::pg_sys::Datum, bool)]) -> Vec<bool> {
    let mut alive = vec![true; batch.len()];

    for predicate in &chain.predicates {
        // Collect only the surviving rows for this predicate.
        let survivors: Vec<(pgrx::pg_sys::Datum, bool)> = batch
            .iter()
            .zip(alive.iter())
            .filter_map(|(&datum, &is_alive)| if is_alive { Some(datum) } else { None })
            .collect();

        if survivors.is_empty() {
            break;
        }

        let pred_results = (predicate.eval_fn)(&survivors);

        // Map predicate results back to the full-width alive mask.
        let mut survivor_idx = 0;
        for flag in &mut alive {
            if *flag {
                if survivor_idx < pred_results.len() {
                    *flag = pred_results[survivor_idx];
                }
                survivor_idx += 1;
            }
        }
    }

    alive
}
