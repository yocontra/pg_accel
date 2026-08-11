//! Backend-local cache for exact repeated planner declines.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::OnceLock;

use std::ffi::CStr;

use pgrx::pg_sys;

use super::PlannerSubstageGuard;
use crate::engine::residency::{ResidentPlannerDependency, revalidate_planner_dependencies};
use crate::engine::{cost, gucs, stats};

const CACHE_CAPACITY: usize = 64;
const MAX_QUERY_BYTES: usize = 64 * 1024;
const MAX_CACHE_QUERY_BYTES: usize = 256 * 1024;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeclinePolicyKey {
    Structural {
        test_force_spatial_groupagg: bool,
    },
    Device {
        auto_load: bool,
        resident_budget_bytes: u64,
        cost_multiplier_bits: u64,
        device_limits: u64,
        test_force_spatial_groupagg: bool,
    },
}

impl DeclinePolicyKey {
    pub(super) fn structural() -> Self {
        #[cfg(any(test, feature = "pg_test"))]
        let test_force_spatial_groupagg = gucs::test_force_spatial_groupagg();
        #[cfg(not(any(test, feature = "pg_test")))]
        let test_force_spatial_groupagg = false;

        Self::Structural {
            test_force_spatial_groupagg,
        }
    }

    pub(super) fn device() -> Self {
        #[cfg(any(test, feature = "pg_test"))]
        let test_force_spatial_groupagg = gucs::test_force_spatial_groupagg();
        #[cfg(not(any(test, feature = "pg_test")))]
        let test_force_spatial_groupagg = false;

        Self::Device {
            auto_load: gucs::auto_load(),
            resident_budget_bytes: gucs::resident_memory_budget_bytes(),
            cost_multiplier_bits: gucs::cost_multiplier().to_bits(),
            device_limits: device_limits_fingerprint(),
            test_force_spatial_groupagg,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct QueryFingerprint {
    hash: u64,
    statement: Rc<[u8]>,
    query_tree: Rc<[u8]>,
    query_id: Option<i64>,
    user_oid: u32,
    search_path: Rc<[u8]>,
    row_security: bool,
}

impl QueryFingerprint {
    #[cfg(test)]
    fn new(
        statement: Rc<[u8]>,
        query_tree: Rc<[u8]>,
        query_id: i64,
        user_oid: u32,
        search_path: Rc<[u8]>,
        row_security: bool,
    ) -> Option<Self> {
        (fingerprint_byte_len(&statement, &query_tree, &search_path)? <= MAX_QUERY_BYTES).then(
            || Self {
                hash: hash_fingerprint(&statement, &query_tree),
                statement,
                query_tree,
                query_id: (query_id != 0).then_some(query_id),
                user_oid,
                search_path,
                row_security,
            },
        )
    }

    fn byte_len(&self) -> usize {
        self.statement
            .len()
            .saturating_add(self.query_tree.len())
            .saturating_add(self.search_path.len())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeclineCacheKey {
    query_fingerprint: Rc<QueryFingerprint>,
    input_rows_bits: u64,
    output_rows_bits: u64,
    native_cost_bits: Option<u64>,
    policy: DeclinePolicyKey,
}

impl DeclineCacheKey {
    pub(super) fn new(
        query_fingerprint: Rc<QueryFingerprint>,
        input_rows_bits: u64,
        output_rows_bits: u64,
        policy: DeclinePolicyKey,
    ) -> Self {
        Self {
            query_fingerprint,
            input_rows_bits,
            output_rows_bits,
            native_cost_bits: None,
            policy,
        }
    }

    pub(super) fn with_native_cost(mut self, native_cost: Option<f64>) -> Self {
        self.native_cost_bits = native_cost.map(f64::to_bits);
        self
    }
}

#[derive(Debug, Clone)]
struct CachedDecline {
    key: DeclineCacheKey,
    reason: &'static str,
    catalog_epoch: u64,
    dependencies: Rc<[ResidentPlannerDependency]>,
}

thread_local! {
    static CACHE: RefCell<VecDeque<CachedDecline>> =
        RefCell::new(VecDeque::with_capacity(CACHE_CAPACITY));
    static CATALOG_EPOCH: Cell<u64> = const { Cell::new(1) };
    /// Most benchmark and prepared-statement traffic replans the same exact
    /// top-level statement repeatedly. Retain one bounded, collision-checked
    /// identity so a cache probe can clone its `Rc` instead of allocating and
    /// recopying statement/search-path bytes on every plan.
    static LAST_QUERY_FINGERPRINT: RefCell<Option<Rc<QueryFingerprint>>> =
        const { RefCell::new(None) };
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    bytes.iter().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

fn hash_fingerprint(statement: &[u8], query_tree: &[u8]) -> u64 {
    query_tree.iter().fold(hash_bytes(statement), |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

fn fingerprint_byte_len(statement: &[u8], query_tree: &[u8], search_path: &[u8]) -> Option<usize> {
    statement
        .len()
        .checked_add(query_tree.len())?
        .checked_add(search_path.len())
}

fn device_limits_fingerprint() -> u64 {
    static FINGERPRINT: OnceLock<u64> = OnceLock::new();
    *FINGERPRINT.get_or_init(|| hash_bytes(format!("{:?}", cost::device_limits()).as_bytes()))
}

fn statement_bounds(source_len: usize, location: i32, len: i32) -> Option<(usize, usize)> {
    if location < 0 {
        return (source_len > 0 && source_len <= MAX_QUERY_BYTES).then_some((0, source_len));
    }
    let start = usize::try_from(location).ok()?;
    let remaining = source_len.checked_sub(start)?;
    let statement_len = if len <= 0 {
        remaining
    } else {
        let requested = usize::try_from(len).ok()?;
        (requested <= remaining).then_some(requested)?
    };
    (statement_len > 0 && statement_len <= MAX_QUERY_BYTES).then(|| (start, start + statement_len))
}

fn intern_query_fingerprint(
    statement: &[u8],
    query_tree: &[u8],
    query_id: i64,
    user_oid: u32,
    search_path: &[u8],
    row_security: bool,
) -> Option<Rc<QueryFingerprint>> {
    (fingerprint_byte_len(statement, query_tree, search_path)? <= MAX_QUERY_BYTES).then_some(())?;
    let hash = hash_fingerprint(statement, query_tree);
    LAST_QUERY_FINGERPRINT.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(existing) = slot.as_ref()
            && existing.hash == hash
            && existing.statement.as_ref() == statement
            && existing.query_tree.as_ref() == query_tree
            && existing.query_id == (query_id != 0).then_some(query_id)
            && existing.user_oid == user_oid
            && existing.search_path.as_ref() == search_path
            && existing.row_security == row_security
        {
            return Some(Rc::clone(existing));
        }
        let fingerprint = Rc::new(QueryFingerprint {
            hash,
            statement: Rc::from(statement),
            query_tree: Rc::from(query_tree),
            query_id: (query_id != 0).then_some(query_id),
            user_oid,
            search_path: Rc::from(search_path),
            row_security,
        });
        *slot = Some(Rc::clone(&fingerprint));
        Some(fingerprint)
    })
}

fn build_query_fingerprint(
    parse: *mut pg_sys::Query,
    query_string: *const ::core::ffi::c_char,
) -> Option<Rc<QueryFingerprint>> {
    if parse.is_null() || query_string.is_null() {
        return None;
    }
    // A prepared plan's Query can retain the source statement's location and
    // length while debug_query_string names the surrounding EXECUTE. Calling
    // PostgreSQL's CleanQuerytext with that mismatched pair trips an assertion
    // in assert-enabled builds. Slice only after proving every bound here; a
    // mismatch simply disables this optional cache entry.
    // SAFETY: query_string was checked non-null and is NUL terminated for this
    // planner call. The borrowed bytes do not escape this function.
    let source = unsafe { CStr::from_ptr(query_string) }.to_bytes();
    // SAFETY: parse was checked non-null and is live for this planner call.
    let (location, len, query_id) =
        unsafe { ((*parse).stmt_location, (*parse).stmt_len, (*parse).queryId) };
    let (start, end) = statement_bounds(source.len(), location, len)?;
    let statement = source.get(start..end)?;
    // SAFETY: namespace_search_path is PostgreSQL's backend-local GUC string.
    let path = unsafe { pg_sys::namespace_search_path };
    if path.is_null() {
        return None;
    }
    // SAFETY: a non-null PostgreSQL GUC string is NUL terminated. The borrowed
    // bytes are compared or copied before this function returns.
    let search_path = unsafe { CStr::from_ptr(path) }.to_bytes();
    // debug_query_string remains the client-supplied outer statement while
    // PL/pgSQL or an extension plans nested SPI statements. Source text alone
    // therefore is not an exact identity for those plans. Serialize the
    // analyzed Query as well: nodeToString includes exact Const bytes,
    // relation/function/operator OIDs, and the rewritten expression tree. The
    // cache retains and compares these bytes, not only their hash.
    // SAFETY: parse was checked non-null and points to PostgreSQL's live,
    // well-typed Query. nodeToString returns a palloc-owned NUL-terminated
    // string or raises a PostgreSQL ERROR.
    let serialized_tree = unsafe { pg_sys::nodeToString(parse.cast()) };
    if serialized_tree.is_null() {
        return None;
    }
    // SAFETY: serialized_tree is non-null, NUL terminated, and remains live
    // until the matching pfree below.
    let query_tree = unsafe { CStr::from_ptr(serialized_tree) }.to_bytes();
    // SAFETY: parse is the live planner-hook Query argument and the remaining
    // values are backend-local session state read on PostgreSQL's main thread.
    let fingerprint = intern_query_fingerprint(
        statement,
        query_tree,
        query_id,
        u32::from(unsafe { pg_sys::GetUserId() }),
        search_path,
        unsafe { pg_sys::row_security },
    );
    // SAFETY: serialized_tree is the exact palloc-owned pointer returned by
    // nodeToString, and its bytes have either been copied or rejected.
    unsafe { pg_sys::pfree(serialized_tree.cast()) };
    fingerprint
}

fn cache_candidate(parse: *mut pg_sys::Query) -> bool {
    !parse.is_null()
        && gucs::enabled()
        && gucs::gpu_enabled()
        // SAFETY: parse is the live planner-hook Query argument.
        && unsafe { (*parse).hasAggs }
}

/// Build a collision-checked source and session-resolution identity.
///
/// # Safety
/// `root` must be the live PlannerInfo supplied to the active planner hook.
pub(super) unsafe fn query_fingerprint(
    root: *mut pg_sys::PlannerInfo,
) -> Option<Rc<QueryFingerprint>> {
    if root.is_null() {
        return None;
    }
    // Cache only top-level statements. A nested planner root can refer to a
    // rewritten query whose source is not represented exactly by
    // debug_query_string, so declining to cache it is the safe choice.
    // SAFETY: root is live and owned by the active planner invocation.
    if unsafe { !(*root).parent_root.is_null() || (*root).query_level != 1 } {
        return None;
    }
    // SAFETY: root is live and owned by the active planner invocation.
    let query = unsafe { (*root).parse };
    if !cache_candidate(query) {
        return None;
    }
    // PostgreSQL keeps the exact top-level source string here for the duration
    // of planning. Unlike wrapping planner_hook/standard_planner in Rust, this
    // lazy read cannot intercept or truncate PostgreSQL ErrorData fields.
    // SAFETY: debug_query_string is a backend-local PostgreSQL global.
    let query_string = unsafe { pg_sys::debug_query_string };
    let _profile = PlannerSubstageGuard::new(stats::PlannerSubstage::QueryFingerprint);
    build_query_fingerprint(query, query_string)
}

pub(super) fn lookup(key: &DeclineCacheKey) -> Option<&'static str> {
    let epoch = CATALOG_EPOCH.with(Cell::get);
    let candidate = {
        let _profile = PlannerSubstageGuard::new(stats::PlannerSubstage::DeclineCacheLookup);
        CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            let index = cache
                .iter()
                .position(|entry| &entry.key == key && entry.catalog_epoch == epoch)?;
            let entry = cache.remove(index).expect("cache index remains valid");
            let candidate = (entry.reason, Rc::clone(&entry.dependencies));
            cache.push_front(entry);
            Some(candidate)
        })
    }?;
    let dependencies_valid = if candidate.1.is_empty() {
        true
    } else {
        let _profile = PlannerSubstageGuard::new(stats::PlannerSubstage::DependencyRevalidation);
        revalidate_planner_dependencies(&candidate.1)
    };
    if !dependencies_valid {
        CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.front().is_some_and(|entry| &entry.key == key) {
                cache.pop_front();
            } else {
                cache.retain(|entry| &entry.key != key);
            }
        });
        return None;
    }
    Some(candidate.0)
}

pub(super) fn insert(
    key: DeclineCacheKey,
    reason: &'static str,
    dependencies: Vec<ResidentPlannerDependency>,
) {
    let catalog_epoch = CATALOG_EPOCH.with(Cell::get);
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.retain(|entry| entry.key != key);
        let query_bytes = key.query_fingerprint.byte_len();
        while cache.len() >= CACHE_CAPACITY
            || cache
                .iter()
                .map(|entry| entry.key.query_fingerprint.byte_len())
                .sum::<usize>()
                .saturating_add(query_bytes)
                > MAX_CACHE_QUERY_BYTES
        {
            if cache.pop_back().is_none() {
                break;
            }
        }
        cache.push_front(CachedDecline {
            key,
            reason,
            catalog_epoch,
            dependencies: Rc::from(dependencies),
        });
        cache.truncate(CACHE_CAPACITY);
    });
}

fn invalidate() {
    let _ = CATALOG_EPOCH.try_with(|epoch| epoch.set(epoch.get().wrapping_add(1).max(1)));
}

#[cfg(not(test))]
#[pgrx::pg_guard]
unsafe extern "C-unwind" fn relcache_callback(_arg: pg_sys::Datum, _relid: pg_sys::Oid) {
    invalidate();
}

#[cfg(not(test))]
#[pgrx::pg_guard]
unsafe extern "C-unwind" fn syscache_callback(
    _arg: pg_sys::Datum,
    _cacheid: ::core::ffi::c_int,
    _hashvalue: u32,
) {
    invalidate();
}

/// Register invalidation callbacks once during planner-hook installation.
///
/// # Safety
/// Must run once from `_PG_init` on PostgreSQL's main thread.
pub(super) unsafe fn install() {
    #[cfg(not(test))]
    unsafe {
        pg_sys::CacheRegisterRelcacheCallback(Some(relcache_callback), pg_sys::Datum::from(0));
        for cache_id in [
            pg_sys::SysCacheIdentifier::AGGFNOID,
            pg_sys::SysCacheIdentifier::OPEROID,
            pg_sys::SysCacheIdentifier::PROCOID,
            pg_sys::SysCacheIdentifier::TYPEOID,
        ] {
            pg_sys::CacheRegisterSyscacheCallback(
                cache_id as ::core::ffi::c_int,
                Some(syscache_callback),
                pg_sys::Datum::from(0),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fingerprint(statement: &[u8], search_path: &[u8]) -> Rc<QueryFingerprint> {
        Rc::new(
            QueryFingerprint::new(
                Rc::from(statement),
                Rc::from(&b""[..]),
                1,
                10,
                Rc::from(search_path),
                true,
            )
            .expect("small query fingerprint"),
        )
    }

    fn key(value: u64) -> DeclineCacheKey {
        DeclineCacheKey::new(
            fingerprint(&value.to_le_bytes(), b"public"),
            10_f64.to_bits(),
            2_f64.to_bits(),
            DeclinePolicyKey::Structural {
                test_force_spatial_groupagg: false,
            },
        )
    }

    fn key_with_bytes(value: u8, len: usize) -> DeclineCacheKey {
        DeclineCacheKey::new(
            fingerprint(&vec![value; len], b""),
            10_f64.to_bits(),
            2_f64.to_bits(),
            DeclinePolicyKey::Structural {
                test_force_spatial_groupagg: false,
            },
        )
    }

    #[test]
    fn repeated_exact_key_hits_and_catalog_epoch_invalidates() {
        CACHE.with(|cache| cache.borrow_mut().clear());
        insert(key(1), "reason_one", Vec::new());
        assert_eq!(lookup(&key(1)), Some("reason_one"));
        assert_eq!(lookup(&key(2)), None);
        invalidate();
        assert_eq!(lookup(&key(1)), None);
    }

    #[test]
    fn native_comparator_cost_is_part_of_the_exact_key() {
        CACHE.with(|cache| cache.borrow_mut().clear());
        insert(
            key(7).with_native_cost(Some(100.0)),
            "cost_decline",
            Vec::new(),
        );
        assert_eq!(
            lookup(&key(7).with_native_cost(Some(100.0))),
            Some("cost_decline")
        );
        assert_eq!(lookup(&key(7).with_native_cost(Some(100.5))), None);
        assert_eq!(lookup(&key(7)), None);
    }

    #[test]
    fn forced_spatial_test_policy_is_part_of_all_relevant_keys() {
        let device = |test_force_spatial_groupagg| DeclinePolicyKey::Device {
            auto_load: true,
            resident_budget_bytes: 1024,
            cost_multiplier_bits: 1.0_f64.to_bits(),
            device_limits: 7,
            test_force_spatial_groupagg,
        };
        let structural = |test_force_spatial_groupagg| DeclinePolicyKey::Structural {
            test_force_spatial_groupagg,
        };
        assert_ne!(device(false), device(true));
        assert_ne!(structural(false), structural(true));
    }

    #[test]
    fn bounded_cache_evicts_oldest_entries() {
        CACHE.with(|cache| cache.borrow_mut().clear());
        for value in 0..=CACHE_CAPACITY as u64 {
            insert(key(value), "bounded", Vec::new());
        }
        assert_eq!(lookup(&key(0)), None);
        assert_eq!(lookup(&key(CACHE_CAPACITY as u64)), Some("bounded"));
    }

    #[test]
    fn oversized_query_trees_are_not_cacheable() {
        assert!(
            QueryFingerprint::new(
                Rc::from(vec![0; MAX_QUERY_BYTES - b"{QUERY}".len()]),
                Rc::from(&b"{QUERY}"[..]),
                0,
                10,
                Rc::from(&b""[..]),
                false,
            )
            .is_some()
        );
        assert!(
            QueryFingerprint::new(
                Rc::from(vec![0; MAX_QUERY_BYTES]),
                Rc::from(&b"{QUERY}"[..]),
                0,
                10,
                Rc::from(&b""[..]),
                false,
            )
            .is_none()
        );
    }

    #[test]
    fn statement_bounds_fail_closed_for_prepared_source_mismatch() {
        assert_eq!(statement_bounds(7, 0, 7), Some((0, 7)));
        assert_eq!(statement_bounds(12, 3, 4), Some((3, 7)));
        assert_eq!(statement_bounds(12, 3, 0), Some((3, 12)));
        assert_eq!(statement_bounds(7, 0, 100), None);
        assert_eq!(statement_bounds(7, 8, 0), None);
        assert_eq!(statement_bounds(0, -1, -1), None);
        assert_eq!(statement_bounds(MAX_QUERY_BYTES + 1, -1, -1), None);
    }

    #[test]
    fn exact_repeated_fingerprint_is_interned_without_weakening_identity() {
        LAST_QUERY_FINGERPRINT.with(|slot| *slot.borrow_mut() = None);
        let first = intern_query_fingerprint(b"SELECT 1", b"{CONST 1}", 7, 10, b"public", true)
            .expect("first fingerprint");
        let repeated = intern_query_fingerprint(b"SELECT 1", b"{CONST 1}", 7, 10, b"public", true)
            .expect("repeated fingerprint");
        assert!(Rc::ptr_eq(&first, &repeated));

        for distinct in [
            intern_query_fingerprint(b"SELECT 2", b"{CONST 1}", 7, 10, b"public", true),
            intern_query_fingerprint(b"SELECT 1", b"{CONST 2}", 7, 10, b"public", true),
            intern_query_fingerprint(b"SELECT 1", b"{CONST 1}", 8, 10, b"public", true),
            intern_query_fingerprint(b"SELECT 1", b"{CONST 1}", 7, 11, b"public", true),
            intern_query_fingerprint(b"SELECT 1", b"{CONST 1}", 7, 10, b"private", true),
            intern_query_fingerprint(b"SELECT 1", b"{CONST 1}", 7, 10, b"public", false),
        ] {
            assert!(!Rc::ptr_eq(
                &first,
                &distinct.expect("distinct fingerprint")
            ));
        }
    }

    #[test]
    fn total_query_byte_budget_evicts_lru_entries() {
        CACHE.with(|cache| cache.borrow_mut().clear());
        for value in 1..=5 {
            insert(
                key_with_bytes(value, MAX_QUERY_BYTES),
                "budgeted",
                Vec::new(),
            );
        }
        assert_eq!(lookup(&key_with_bytes(1, MAX_QUERY_BYTES)), None);
        assert_eq!(
            lookup(&key_with_bytes(5, MAX_QUERY_BYTES)),
            Some("budgeted")
        );
        CACHE.with(|cache| {
            assert!(
                cache
                    .borrow()
                    .iter()
                    .map(|entry| entry.key.query_fingerprint.byte_len())
                    .sum::<usize>()
                    <= MAX_CACHE_QUERY_BYTES
            );
        });
    }
}
