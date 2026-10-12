//! Batch-boundary preparation of the existing byte-per-row NULL sidecar.
//!
//! Producers append flags and OR `saw_null` in their existing row loop. Until
//! the first NULL, completed all-zero prefixes are implicit; only one load
//! batch needs scratch. Once a NULL appears, backfill that prefix exactly once.

pub(super) const STAGING_ROWS: usize = 8192;

#[inline]
fn normalize(
    nulls: &mut Vec<u8>,
    saw_null: bool,
    rows: usize,
    additional: usize,
) -> Result<(), String> {
    if nulls.len() > rows {
        return Err("resident NULL sidecar exceeds staged row count".to_owned());
    }
    if !saw_null {
        nulls.clear();
        return Ok(());
    }
    if nulls.len() != rows {
        restore_prefix(nulls, rows, additional)?;
    }
    Ok(())
}

// At most one omitted prefix is restored per column. Keep this allocation/
// movement path out of normalize so the ordinary no-NULL/per-row reservation
// path remains small enough to inline into the producer.
#[inline(never)]
fn restore_prefix(nulls: &mut Vec<u8>, rows: usize, additional: usize) -> Result<(), String> {
    let tail = nulls.len();
    let prefix = rows - tail;
    // Compute both growth steps before allocating. Reserve enough for the
    // restored prefix and next batch, retaining geometric headroom: merely
    // reserving rows+additional can establish a larger growth sequence on
    // subsequent batches (for example 3,6,12 instead of 4,8,16 batches).
    let total = rows
        .checked_add(additional)
        .ok_or_else(|| "resident NULL sidecar reservation overflows usize".to_owned())?;
    let grow = |capacity: usize, required: usize| {
        if required <= capacity {
            capacity
        } else {
            capacity.saturating_mul(2).max(required).max(8)
        }
    };
    let target = grow(grow(nulls.capacity(), rows), total);
    nulls
        .try_reserve(target - tail)
        .map_err(|error| format!("resident NULL sidecar allocation failed: {error}"))?;
    nulls.resize(rows, 0);
    nulls.copy_within(..tail, prefix);
    // resize already zeroed [tail, rows). Only the old tail bytes left
    // before the destination can still be nonzero. For a late first NULL,
    // do not write the potentially table-sized zero prefix a second time.
    nulls[..tail.min(prefix)].fill(0);
    Ok(())
}

/// Called before a producer's next batch (or row), never from its flag append
/// hot path. `rows` counts all published values, including discarded zero
/// prefixes; `additional` is only a capacity hint. Producers must reserve at
/// least once per STAGING_ROWS to keep the scratch bound.
#[inline(always)]
pub(super) fn reserve(
    nulls: &mut Vec<u8>,
    saw_null: bool,
    rows: usize,
    additional: usize,
) -> Result<(), String> {
    // After the first NULL's prefix has been restored, length equality proves
    // that no normalization is needed. Keep nullable batches on Vec's ordinary
    // reserve path without entering the backfill/error-formatting helper.
    if !saw_null || nulls.len() != rows {
        normalize(nulls, saw_null, rows, additional)?;
    }
    nulls
        .try_reserve(if saw_null {
            additional
        } else {
            additional.min(STAGING_ROWS)
        })
        .map_err(|error| format!("resident NULL sidecar allocation failed: {error}"))
}

pub(super) fn finish(
    mut nulls: Vec<u8>,
    saw_null: bool,
    rows: usize,
) -> Result<Option<Vec<u8>>, String> {
    if !saw_null {
        return Ok(None);
    }
    normalize(&mut nulls, true, rows, 0)?;
    Ok(Some(nulls))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(input: &[bool], batch: usize) -> (Option<Vec<u8>>, usize) {
        let mut flags = Vec::new();
        let mut saw_null = false;
        let mut rows = 0;
        for chunk in input.chunks(batch) {
            reserve(&mut flags, saw_null, rows, chunk.len()).expect("batch reserve");
            for &null in chunk {
                flags.push(u8::from(null));
                saw_null |= null;
                rows += 1;
            }
        }
        let capacity = flags.capacity();
        (
            finish(flags, saw_null, rows).expect("complete mask"),
            capacity,
        )
    }

    #[test]
    fn null_free_columns_keep_only_one_batch_of_scratch() {
        for rows in [0, 1, 8191, 8192, 8193, 1_000_000] {
            let (mask, capacity) = stage(&vec![false; rows], STAGING_ROWS);
            assert!(mask.is_none());
            assert!(capacity <= STAGING_ROWS);
        }
    }

    #[test]
    fn first_null_in_any_batch_preserves_the_complete_mask() {
        for batch in [1, 3, 64, 8192] {
            for first in [0, 1, 63, 64, 8191, 8192, 8193, 24577] {
                let mut input = vec![false; first + 7];
                input[first] = true;
                input[first + 3] = true;
                let (mask, _) = stage(&input, batch);
                assert_eq!(mask, Some(input.into_iter().map(u8::from).collect()));
            }
        }
    }

    #[test]
    fn backfill_handles_overlapping_moves_and_all_nulls() {
        for prefix in [1, 2, 3, 1000] {
            let mut flags = vec![1, 0, 1, 1, 0, 0, 1];
            let expected = [vec![0; prefix], flags.clone()].concat();
            normalize(&mut flags, true, prefix + 7, 0).expect("backfill");
            assert_eq!(flags, expected);
        }
        assert_eq!(stage(&vec![true; 20_000], 8192).0, Some(vec![1; 20_000]));
    }

    #[test]
    fn backfill_zeroes_only_old_tail_without_losing_overlap() {
        for tail in 0..=10 {
            for bits in 0..(1_u32 << tail) {
                let flags: Vec<_> = (0..tail)
                    .map(|bit| u8::from(bits & (1 << bit) != 0))
                    .collect();
                for prefix in [0, 1, 2, 3, 7, 10, 11, 31, 8192] {
                    let mut actual = flags.clone();
                    normalize(&mut actual, true, prefix + tail, 0).expect("backfill");
                    assert!(actual[..prefix].iter().all(|byte| *byte == 0));
                    assert_eq!(&actual[prefix..], flags);
                    let pointer = actual.as_ptr();
                    reserve(&mut actual, true, prefix + tail, 0).expect("already normalized");
                    assert_eq!(actual.as_ptr(), pointer);
                    assert!(actual[..prefix].iter().all(|byte| *byte == 0));
                    assert_eq!(&actual[prefix..], flags);
                }
            }
        }
    }

    #[test]
    fn late_null_backfill_reserves_next_batch_in_one_allocation() {
        let mut flags = vec![0; STAGING_ROWS];
        flags[STAGING_ROWS - 1] = 1;
        let mut baseline = flags.clone();
        let rows = 3 * STAGING_ROWS;
        let additional = STAGING_ROWS;
        crate::engine::residency::begin_test_allocation_count();
        let baseline_result = normalize(&mut baseline, true, rows, 0).and_then(|()| {
            baseline
                .try_reserve(additional)
                .map_err(|error| error.to_string())
        });
        let baseline_allocations = crate::engine::residency::finish_test_allocation_count();
        baseline_result.expect("original two-step backfill");
        assert_eq!(baseline_allocations, 2);
        crate::engine::residency::begin_test_allocation_count();
        let result = reserve(&mut flags, true, rows, additional);
        let allocations = crate::engine::residency::finish_test_allocation_count();
        result.expect("prefix and next batch reservation");
        assert_eq!(allocations, 1, "never grow twice to restore one prefix");
        assert_eq!(flags.len(), rows);
        assert!(flags.capacity() >= rows + additional);
        assert_eq!(flags, baseline);
        assert_eq!(flags.capacity(), baseline.capacity());
        assert!(flags[..rows - 1].iter().all(|flag| *flag == 0));
        assert_eq!(flags[rows - 1], 1);
        let pointer = flags.as_ptr();
        flags.extend(std::iter::repeat_n(0, additional));
        assert_eq!(
            flags.as_ptr(),
            pointer,
            "next batch fits the same allocation"
        );
        let mask = finish(flags, true, rows + additional)
            .expect("finish")
            .expect("NULL lane");
        assert_eq!(mask.len(), rows + additional);
        assert_eq!(mask.iter().filter(|flag| **flag != 0).count(), 1);
    }

    #[test]
    fn coalesced_backfill_preserves_two_step_capacity_and_payload() {
        for capacity in [0, 1, 3, 8, 64, 8192] {
            for tail in [0, capacity / 2, capacity] {
                for prefix in [1, 3, 64, 8192, 65_537] {
                    for additional in [0, 1, 7, 8192] {
                        let mut candidate = Vec::with_capacity(capacity);
                        candidate.extend((0..tail).map(|row| (row % 2) as u8));
                        let mut baseline = Vec::with_capacity(capacity);
                        baseline.extend_from_slice(&candidate);
                        let rows = prefix + tail;
                        // Original two-stage Vec growth and exact backfill.
                        baseline.try_reserve(prefix).expect("baseline prefix");
                        baseline.resize(rows, 0);
                        baseline.copy_within(..tail, prefix);
                        baseline[..tail.min(prefix)].fill(0);
                        baseline
                            .try_reserve(additional)
                            .expect("baseline next batch");
                        reserve(&mut candidate, true, rows, additional)
                            .expect("coalesced backfill");
                        assert_eq!(candidate, baseline);
                        assert_eq!(candidate.capacity(), baseline.capacity());
                    }
                }
            }
        }
    }

    #[test]
    fn prefix_plus_next_batch_overflow_preserves_pending_bytes() {
        for additional in [usize::MAX, usize::MAX - 1] {
            let mut flags = vec![1, 0, 1];
            assert!(reserve(&mut flags, true, 5, additional).is_err());
            assert_eq!(flags, [1, 0, 1]);
            reserve(&mut flags, true, 5, 2).expect("retry after overflow");
            assert_eq!(flags, [0, 0, 1, 0, 1]);
        }
    }

    #[test]
    fn impossible_backfill_does_not_mutate_the_pending_tail() {
        let mut flags = vec![0, 1, 0];
        assert!(reserve(&mut flags, true, usize::MAX, 1).is_err());
        assert_eq!(flags, [0, 1, 0]);
        assert!(reserve(&mut flags, true, 2, 1).is_err());
        assert_eq!(flags, [0, 1, 0]);
        reserve(&mut flags, true, 6, 1).expect("retry");
        assert_eq!(flags, [0, 0, 0, 0, 1, 0]);
    }
}
