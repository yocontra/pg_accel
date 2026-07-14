use super::Workload;

const BITMAP_HEAP_GPUEXPR_DECLINE_ROW_SCALES: &[usize] = &[10_000, 100_000];

/// Bitmap-prefiltered scalar-expression workload that must stay native until
/// GpuExpr can fuse with scan batches.
pub struct BitmapHeapGpuExprDecline;

impl Workload for BitmapHeapGpuExprDecline {
    fn name(&self) -> &'static str {
        "bitmap_heap_gpuexpr_decline"
    }

    fn description(&self) -> &'static str {
        "BitmapHeapScan-prefiltered scalar expressions - native planner decline \
         (`shape_unsupported_predicate`) until generic predicate descriptors are supported"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_bitmap_gpuexpr_decline".to_owned(),
            "CREATE TABLE bench_bitmap_gpuexpr_decline (\
               bucket int4 NOT NULL, \
               score float4 NOT NULL, \
               qty int4 NOT NULL, \
               payload int4 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_bitmap_gpuexpr_decline (bucket, score, qty, payload) \
                 SELECT \
                   (g % 1000)::int4, \
                   ((g * 37) % 1000)::float4, \
                   ((g * 13) % 100)::int4, \
                   ((g * 17) % 10000)::int4 \
                 FROM generate_series(1, {rows}) g"
            ),
            "CREATE INDEX bench_bitmap_gpuexpr_bucket_idx \
             ON bench_bitmap_gpuexpr_decline (bucket)"
                .to_owned(),
            "ANALYZE bench_bitmap_gpuexpr_decline".to_owned(),
        ]
    }

    fn pre_query_sql(&self) -> Vec<String> {
        vec!["SET enable_indexscan = off".to_owned()]
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) \
         FROM bench_bitmap_gpuexpr_decline \
         WHERE bucket BETWEEN 100 AND 300 \
           AND score > 500.0::float4 \
           AND qty < 50"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        BITMAP_HEAP_GPUEXPR_DECLINE_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_bitmap_gpuexpr_decline".to_owned()]
    }
}
