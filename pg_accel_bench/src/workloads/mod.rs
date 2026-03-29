mod aggregate;
mod fts_rank;
mod h3_bulk;
mod index_recheck;
mod join_residual;
mod large_sort;
mod oltp_point;
mod proximity;
mod simple_agg;
mod small_table;
mod spatial_join;
mod topk_sort;

pub use aggregate::Aggregate;
pub use fts_rank::FtsRank;
pub use h3_bulk::H3Bulk;
pub use index_recheck::IndexRecheck;
pub use join_residual::JoinResidual;
pub use large_sort::LargeSort;
pub use oltp_point::OltpPoint;
pub use proximity::Proximity;
pub use simple_agg::SimpleAgg;
pub use small_table::SmallTable;
pub use spatial_join::SpatialJoin;
pub use topk_sort::TopkSort;

/// A benchmark workload that can set up tables, run a query, and clean up.
pub trait Workload: Send + Sync {
    /// Short identifier for this workload (e.g. `"simple_agg"`).
    fn name(&self) -> &'static str;

    /// Human-readable description of what this workload tests.
    fn description(&self) -> &'static str;

    /// SQL statements to create and populate benchmark tables.
    fn setup_sql(&self, rows: usize) -> Vec<String>;

    /// The query to benchmark under `EXPLAIN ANALYZE`.
    fn query_sql(&self) -> String;

    /// SQL statements to tear down benchmark tables.
    fn cleanup_sql(&self) -> Vec<String>;
}

/// Return all registered workloads.
pub fn all_workloads() -> Vec<Box<dyn Workload>> {
    vec![
        // --- Acceleration workloads (expect speedup) ---
        Box::new(SimpleAgg),
        Box::new(Aggregate),
        Box::new(SpatialJoin),
        Box::new(Proximity),
        Box::new(LargeSort),
        Box::new(TopkSort),
        Box::new(H3Bulk),
        Box::new(JoinResidual),
        Box::new(IndexRecheck),
        Box::new(FtsRank),
        // --- Regression workloads (expect ~1.00x, proving no overhead) ---
        Box::new(OltpPoint),
        Box::new(SmallTable),
    ]
}

/// Look up a workload by name (case-insensitive).
pub fn find_workload(name: &str) -> Option<Box<dyn Workload>> {
    let lower = name.to_lowercase();
    all_workloads()
        .into_iter()
        .find(|w| w.name().to_lowercase() == lower)
}
