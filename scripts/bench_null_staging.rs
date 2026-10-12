//! Host NULL-mask microbenchmark; not an end-to-end query/GPU benchmark.
//! rustc -O --edition=2024 scripts/bench_null_staging.rs -o /tmp/bench-null-staging
//! Arguments: rows [values|mask] [batch_rows]. Use batch_rows=1 for builders
//! that also reserve before individual appends (NUMERIC and raster).

#[path = "../pg_accel/src/engine/residency/nulls.rs"]
mod production;

use std::hint::black_box;
use std::time::Instant;

type Output = (Option<Vec<u8>>, usize);

fn eager(input: &[bool], batch_rows: usize) -> Output {
    let mut nulls = Vec::new();
    let mut saw_null = false;
    for chunk in black_box(input).chunks(batch_rows) {
        nulls.try_reserve(chunk.len()).expect("eager batch reserve");
        for &null in chunk {
            nulls.push(u8::from(null));
            saw_null |= null;
        }
    }
    black_box(&nulls);
    let capacity = nulls.capacity();
    (saw_null.then_some(nulls), capacity)
}

fn lazy(input: &[bool], batch_rows: usize) -> Output {
    let mut nulls = Vec::new();
    let mut saw_null = false;
    let mut rows = 0;
    for chunk in black_box(input).chunks(batch_rows) {
        production::reserve(&mut nulls, saw_null, rows, chunk.len()).expect("batch reserve");
        for &null in chunk {
            nulls.push(u8::from(null));
            saw_null |= null;
        }
        rows += chunk.len();
    }
    black_box(&nulls);
    let capacity = nulls.capacity();
    let mask = production::finish(nulls, saw_null, rows).expect("finish mask");
    let peak = capacity.max(mask.as_ref().map_or(0, Vec::capacity));
    (mask, peak)
}

fn timed(
    function: fn(&[bool], usize) -> Output,
    input: &[bool],
    batch_rows: usize,
) -> (u128, Output) {
    let start = Instant::now();
    let output = function(black_box(input), batch_rows);
    let elapsed = start.elapsed().as_nanos();
    black_box(&output);
    (elapsed, output)
}

fn timed_values(input: &[bool], lazy: bool, batch_rows: usize) -> (u128, Output) {
    let start = Instant::now();
    let mut values = Vec::new();
    let mut nulls = Vec::new();
    let mut saw_null = false;
    for chunk in black_box(input).chunks(batch_rows) {
        values
            .try_reserve(chunk.len())
            .expect("value batch reserve");
        if lazy {
            production::reserve(&mut nulls, saw_null, values.len(), chunk.len())
                .expect("mask batch reserve");
        } else {
            nulls
                .try_reserve(chunk.len())
                .expect("eager mask batch reserve");
        }
        for &null in chunk {
            values.push(if null {
                0
            } else {
                (values.len() as i64).wrapping_mul(37)
            });
            nulls.push(u8::from(null));
            saw_null |= null;
        }
    }
    black_box(&values);
    black_box(&nulls);
    let capacity = nulls.capacity();
    let mask = if lazy {
        production::finish(nulls, saw_null, values.len()).expect("finish mask")
    } else {
        saw_null.then_some(nulls)
    };
    let peak = capacity.max(mask.as_ref().map_or(0, Vec::capacity));
    let output = (mask, peak);
    let elapsed = start.elapsed().as_nanos();
    assert!(values.iter().enumerate().all(|(row, value)| {
        *value
            == if input[row] {
                0
            } else {
                (row as i64).wrapping_mul(37)
            }
    }));
    black_box(&output);
    (elapsed, output)
}

fn main() {
    let rows: usize = std::env::args()
        .nth(1)
        .map_or(Ok(1_000_000), |s| s.parse())
        .expect("positive row count");
    assert!(rows > 0);
    let with_values = std::env::args().nth(2).as_deref() == Some("values");
    let batch_rows: usize = std::env::args()
        .nth(3)
        .map_or(Ok(production::STAGING_ROWS), |s| s.parse())
        .expect("batch row count");
    assert!((1..=production::STAGING_ROWS).contains(&batch_rows));
    for pattern in [
        "none",
        "first",
        "last",
        "late_batch",
        "second_batch",
        "middle",
        "sparse",
        "alternating",
        "all",
    ] {
        let input: Vec<_> = (0..rows)
            .map(|row| match pattern {
                "none" => false,
                "first" => row == 0,
                "last" => row == rows - 1,
                "late_batch" => row == rows.saturating_sub(production::STAGING_ROWS),
                "second_batch" => row == production::STAGING_ROWS.min(rows - 1),
                "middle" => row == rows / 2,
                "sparse" => row % 997 == 0,
                "alternating" => row % 2 == 0,
                "all" => true,
                _ => unreachable!(),
            })
            .collect();
        let expected = input
            .iter()
            .any(|null| *null)
            .then(|| input.iter().copied().map(u8::from).collect::<Vec<_>>());
        for _ in 0..3 {
            if with_values {
                assert_eq!(timed_values(&input, false, batch_rows).1.0, expected);
                assert_eq!(timed_values(&input, true, batch_rows).1.0, expected);
            } else {
                assert_eq!(eager(&input, batch_rows).0, expected);
                assert_eq!(lazy(&input, batch_rows).0, expected);
            }
        }
        let mut eager_ns = Vec::new();
        let mut lazy_ns = Vec::new();
        let mut eager_bytes = 0;
        let mut lazy_bytes = 0;
        for pair in 0..21 {
            let old_run = || {
                if with_values {
                    timed_values(&input, false, batch_rows)
                } else {
                    timed(eager, &input, batch_rows)
                }
            };
            let new_run = || {
                if with_values {
                    timed_values(&input, true, batch_rows)
                } else {
                    timed(lazy, &input, batch_rows)
                }
            };
            let (old, new) = if pair % 2 == 0 {
                (old_run(), new_run())
            } else {
                let new = new_run();
                (old_run(), new)
            };
            assert_eq!(old.1.0, expected);
            assert_eq!(new.1.0, expected);
            eager_ns.push(old.0);
            lazy_ns.push(new.0);
            eager_bytes = old.1.1;
            lazy_bytes = new.1.1;
        }
        let mut old_sorted = eager_ns.clone();
        let mut new_sorted = lazy_ns.clone();
        old_sorted.sort_unstable();
        new_sorted.sort_unstable();
        println!(
            "{{\"pattern\":\"{pattern}\",\"with_values\":{with_values},\"rows\":{rows},\"batch_rows\":{batch_rows},\"eager_capacity_bytes\":{eager_bytes},\"lazy_capacity_bytes\":{lazy_bytes},\"eager_median_ns\":{},\"lazy_median_ns\":{},\"eager_ns\":{eager_ns:?},\"lazy_ns\":{lazy_ns:?}}}",
            old_sorted[10], new_sorted[10]
        );
    }
}
