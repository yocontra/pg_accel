mod large_sort;
mod simple_agg;
mod spatial_join;

pub use large_sort::LargeSort;
pub use simple_agg::SimpleAgg;
pub use spatial_join::SpatialJoin;

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
        Box::new(SimpleAgg),
        Box::new(SpatialJoin),
        Box::new(LargeSort),
    ]
}

/// Look up a workload by name (case-insensitive).
pub fn find_workload(name: &str) -> Option<Box<dyn Workload>> {
    let lower = name.to_lowercase();
    all_workloads()
        .into_iter()
        .find(|w| w.name().to_lowercase() == lower)
}
