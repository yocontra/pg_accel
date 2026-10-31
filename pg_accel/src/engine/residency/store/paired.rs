use super::{
    DerivedArtifactIdentity, ResidentRelation, ResidentRelationEvidence, STORE, pg_sys,
    process_invalidations,
};
#[cfg(feature = "pg_test")]
pub fn paired_remove_exact_derived(
    expected: ResidentRelationEvidence,
    identity: &DerivedArtifactIdentity,
) -> Result<u64, String> {
    let owners = crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
    if owners.installed
        != owners
            .normal_end
            .checked_add(owners.query_reset)
            .ok_or("cleanup count overflow")?
    {
        return Err("targeted invalidation requires no installed executor".into());
    }
    if identity.canonical_words().is_empty() {
        return Err("empty exact artifact identity".into());
    }
    // Reject an existing lease before invalidation processing, which itself
    // borrows STORE and may consult PG catalogs. Drop this probe before PG.
    STORE.with(|slot| {
        slot.try_borrow_mut()
            .map(drop)
            .map_err(|_| "active residency lease".to_owned())
    })?;
    process_invalidations();
    STORE.with(|slot| {
        // Fail closed if any active residency lease exists, even if it was not
        // created through a CustomScan. No PG service occurs under this borrow.
        let mut store = slot
            .try_borrow_mut()
            .map_err(|_| "active residency lease")?;
        let entry = store
            .entries
            .iter_mut()
            .find(|entry| entry.relid == expected.relid)
            .ok_or("raw owner is no longer resident")?;
        let current = entry.evidence();
        if (
            current.generation,
            current.global_generation,
            current.relfilenode,
            current.row_count,
            current.raw_bytes,
            current.raw_accounting,
        ) != (
            expected.generation,
            expected.global_generation,
            expected.relfilenode,
            expected.row_count,
            expected.raw_bytes,
            expected.raw_accounting,
        ) {
            return Err("raw generation or exact footprint changed".into());
        }
        let matches = entry
            .derived
            .iter()
            .enumerate()
            .filter_map(|(index, derived)| derived.has_identity(identity).then_some(index))
            .collect::<Vec<_>>();
        let [index] = matches.as_slice() else {
            return Err("fresh phase requires exactly one known cached artifact".into());
        };
        let removed = entry.derived[*index].bytes();
        entry.remove_derived(*index);
        let after = entry.evidence();
        if after.raw_bytes != current.raw_bytes
            || after.raw_accounting != current.raw_accounting
            || after.generation != current.generation
            || after.global_generation != current.global_generation
            || after.relfilenode != current.relfilenode
            || after.row_count != current.row_count
            || after.derived_bytes.checked_add(removed) != Some(current.derived_bytes)
        {
            return Err("targeted derived removal violated footprint conservation".into());
        }
        Ok(removed)
    })
}

// RawIntegerCount has no retained derived owner. Its fresh-artifact action must
// be an explicit checked no-op: inspect current raw evidence and require the
// captured raw recipe owns no derived identity; never evict unrelated Legacy.
// Backend must preserve the last successful Legacy identity/evidence separately
// from the Raw recipe because alternating arms share only immutable raw columns.

#[cfg(feature = "pg_test")]
pub fn paired_current_raw(
    expected: ResidentRelationEvidence,
) -> Result<ResidentRelationEvidence, String> {
    process_invalidations();
    STORE.with(|slot| {
        let store = slot.try_borrow().map_err(|_| "active residency mutation")?;
        let current = store
            .entries
            .iter()
            .find(|entry| entry.relid == expected.relid)
            .ok_or("raw owner no longer resident")?
            .evidence();
        if (
            current.generation,
            current.global_generation,
            current.relfilenode,
            current.row_count,
            current.raw_bytes,
            current.raw_accounting,
        ) != (
            expected.generation,
            expected.global_generation,
            expected.relfilenode,
            expected.row_count,
            expected.raw_bytes,
            expected.raw_accounting,
        ) {
            return Err("raw owner changed".into());
        }
        Ok(current)
    })
}

#[cfg(feature = "pg_test")]
pub fn paired_raw_evidence(relid: pg_sys::Oid) -> Result<ResidentRelationEvidence, String> {
    process_invalidations();
    STORE.with(|slot| {
        let store = slot.try_borrow().map_err(|_| "active residency mutation")?;
        store
            .entries
            .iter()
            .find(|entry| entry.relid == relid)
            .map(ResidentRelation::evidence)
            .ok_or("raw owner not resident".into())
    })
}

pub fn paired_assert_eviction_rejections(
    expected: ResidentRelationEvidence,
    identity: &DerivedArtifactIdentity,
) -> Result<(), String> {
    let original = paired_current_raw(expected)?;
    let mut words = identity.canonical_words().to_vec();
    words.push(i32::MIN);
    let wrong = DerivedArtifactIdentity::from_canonical_words(words);
    if paired_remove_exact_derived(expected, &wrong).is_ok() {
        return Err("wrong identity unexpectedly removed an artifact".into());
    }
    let mut stale = expected;
    stale.generation = stale
        .generation
        .checked_add(1)
        .ok_or("generation overflow")?;
    if paired_remove_exact_derived(stale, identity).is_ok() {
        return Err("stale owner unexpectedly removed an artifact".into());
    }
    STORE.with(|slot| {
        let _active_lease = slot.borrow();
        if paired_remove_exact_derived(expected, identity).is_ok() {
            return Err("active store lease permitted eviction".to_owned());
        }
        Ok(())
    })?;
    let after = paired_current_raw(expected)?;
    if after.derived_bytes != original.derived_bytes {
        return Err("rejected eviction changed derived ownership".into());
    }
    Ok(())
}
