//! Backend-local cache for exact repeated planner declines.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::CStr;
use std::rc::Rc;
use std::sync::OnceLock;

use pgrx::pg_sys;

use crate::engine::residency::{ResidentPlannerDependency, revalidate_planner_dependencies};
use crate::engine::{cost, gucs};

const CACHE_CAPACITY: usize = 64;
const MAX_QUERY_BYTES: usize = 64 * 1024;
const MAX_CACHE_QUERY_BYTES: usize = 256 * 1024;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeclinePolicyKey {
    Structural,
    Device {
        auto_load: bool,
        resident_budget_bytes: u64,
        cost_multiplier_bits: u64,
        device_limits: u64,
        test_force_spatial_groupagg: bool,
    },
}

impl DeclinePolicyKey {
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
    query_id: Option<i64>,
    user_oid: u32,
    search_path: Rc<[u8]>,
    row_security: bool,
}

impl QueryFingerprint {
    fn new(
        statement: &[u8],
        query_id: i64,
        user_oid: u32,
        search_path: &[u8],
        row_security: bool,
    ) -> Option<Self> {
        (statement.len().checked_add(search_path.len())? <= MAX_QUERY_BYTES).then(|| Self {
            hash: hash_bytes(statement),
            statement: Rc::from(statement),
            query_id: (query_id != 0).then_some(query_id),
            user_oid,
            search_path: Rc::from(search_path),
            row_security,
        })
    }

    fn byte_len(&self) -> usize {
        self.statement.len().saturating_add(self.search_path.len())
    }
}

#[derive(Debug, Clone)]
struct ActivePlannerSource {
    parse: usize,
    statement: Rc<[u8]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeclineCacheKey {
    query_fingerprint: QueryFingerprint,
    input_rows_bits: u64,
    output_rows_bits: u64,
    native_cost_bits: Option<u64>,
    policy: DeclinePolicyKey,
}

impl DeclineCacheKey {
    pub(super) fn new(
        query_fingerprint: QueryFingerprint,
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
    dependencies: Vec<ResidentPlannerDependency>,
}

thread_local! {
    static CACHE: RefCell<VecDeque<CachedDecline>> =
        RefCell::new(VecDeque::with_capacity(CACHE_CAPACITY));
    static CATALOG_EPOCH: Cell<u64> = const { Cell::new(1) };
    static ACTIVE_PLANNER_SOURCES: RefCell<Vec<ActivePlannerSource>> = const { RefCell::new(Vec::new()) };
}

#[cfg(not(test))]
static mut PREV_PLANNER_HOOK: pg_sys::planner_hook_type = None;

fn hash_bytes(bytes: &[u8]) -> u64 {
    bytes.iter().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

fn device_limits_fingerprint() -> u64 {
    static FINGERPRINT: OnceLock<u64> = OnceLock::new();
    *FINGERPRINT.get_or_init(|| hash_bytes(format!("{:?}", cost::device_limits()).as_bytes()))
}

#[cfg(not(test))]
fn exact_statement_bytes(
    parse: *mut pg_sys::Query,
    query_string: *const ::core::ffi::c_char,
) -> Option<Rc<[u8]>> {
    if parse.is_null() || query_string.is_null() {
        return None;
    }
    // SAFETY: parse and query_string are the paired planner-hook arguments.
    let (mut location, mut len) = unsafe { ((*parse).stmt_location, (*parse).stmt_len) };
    // SAFETY: PostgreSQL supplied all pointers and CleanQuerytext bounds the
    // selected statement to query_string.
    let statement =
        unsafe { pg_sys::CleanQuerytext(query_string, &raw mut location, &raw mut len) };
    let len = usize::try_from(len).ok()?;
    if statement.is_null() || len == 0 || len > MAX_QUERY_BYTES {
        return None;
    }
    // SAFETY: CleanQuerytext returned a source pointer valid for len bytes.
    Some(Rc::from(unsafe {
        std::slice::from_raw_parts(statement.cast(), len)
    }))
}

fn current_search_path() -> Option<Rc<[u8]>> {
    // SAFETY: namespace_search_path is PostgreSQL's backend-local GUC string.
    let path = unsafe { pg_sys::namespace_search_path };
    (!path.is_null()).then(|| {
        // SAFETY: a non-null PostgreSQL GUC string is NUL terminated.
        Rc::from(unsafe { CStr::from_ptr(path) }.to_bytes())
    })
}

fn push_active_source(parse: *mut pg_sys::Query, statement: Option<Rc<[u8]>>) {
    ACTIVE_PLANNER_SOURCES.with(|sources| {
        sources.borrow_mut().push(ActivePlannerSource {
            parse: statement.as_ref().map_or(0, |_| parse.addr()),
            statement: statement.unwrap_or_else(|| Rc::from([])),
        });
    });
}

#[cfg(not(test))]
fn cache_candidate(parse: *mut pg_sys::Query) -> bool {
    !parse.is_null()
        && gucs::enabled()
        && gucs::gpu_enabled()
        // SAFETY: parse is the live planner-hook Query argument.
        && unsafe { (*parse).hasAggs }
}

fn pop_active_source() {
    ACTIVE_PLANNER_SOURCES.with(|sources| {
        sources.borrow_mut().pop();
    });
}

#[cfg(feature = "pg_test")]
pub(crate) fn active_source_depth() -> usize {
    ACTIVE_PLANNER_SOURCES.with(|sources| sources.borrow().len())
}

#[cfg(all(not(test), feature = "pg18"))]
#[pgrx::pg_guard]
unsafe extern "C-unwind" fn planner_hook(
    parse: *mut pg_sys::Query,
    query_string: *const ::core::ffi::c_char,
    cursor_options: ::core::ffi::c_int,
    bound_params: pg_sys::ParamListInfo,
) -> *mut pg_sys::PlannedStmt {
    let statement = cache_candidate(parse)
        .then(|| exact_statement_bytes(parse, query_string))
        .flatten();
    push_active_source(parse, statement);
    pgrx::PgTryBuilder::new(std::panic::AssertUnwindSafe(|| unsafe {
        if let Some(previous) = PREV_PLANNER_HOOK {
            previous(parse, query_string, cursor_options, bound_params)
        } else {
            pg_sys::standard_planner(parse, query_string, cursor_options, bound_params)
        }
    }))
    .finally(pop_active_source)
    .execute()
}

#[cfg(all(not(test), feature = "pg19"))]
#[pgrx::pg_guard]
unsafe extern "C-unwind" fn planner_hook(
    parse: *mut pg_sys::Query,
    query_string: *const ::core::ffi::c_char,
    cursor_options: ::core::ffi::c_int,
    bound_params: pg_sys::ParamListInfo,
    explain_state: *mut pg_sys::ExplainState,
) -> *mut pg_sys::PlannedStmt {
    let statement = cache_candidate(parse)
        .then(|| exact_statement_bytes(parse, query_string))
        .flatten();
    push_active_source(parse, statement);
    pgrx::PgTryBuilder::new(std::panic::AssertUnwindSafe(|| unsafe {
        if let Some(previous) = PREV_PLANNER_HOOK {
            previous(
                parse,
                query_string,
                cursor_options,
                bound_params,
                explain_state,
            )
        } else {
            pg_sys::standard_planner(
                parse,
                query_string,
                cursor_options,
                bound_params,
                explain_state,
            )
        }
    }))
    .finally(pop_active_source)
    .execute()
}

/// Build a collision-checked source and session-resolution identity.
///
/// # Safety
/// `root` must be the live PlannerInfo supplied to the active planner hook.
pub(super) unsafe fn query_fingerprint(root: *mut pg_sys::PlannerInfo) -> Option<QueryFingerprint> {
    if root.is_null() {
        return None;
    }
    // SAFETY: root is live and owned by the active planner invocation.
    let query = unsafe { (*root).parse };
    if query.is_null() {
        return None;
    }
    let statement = ACTIVE_PLANNER_SOURCES.with(|sources| {
        sources
            .borrow()
            .last()
            .filter(|source| source.parse == query.addr())
            .map(|source| Rc::clone(&source.statement))
    })?;
    let search_path = current_search_path()?;
    // SAFETY: these are backend-local planner/session values.
    QueryFingerprint::new(
        &statement,
        unsafe { (*query).queryId },
        u32::from(unsafe { pg_sys::GetUserId() }),
        &search_path,
        unsafe { pg_sys::row_security },
    )
}

pub(super) fn lookup(key: &DeclineCacheKey) -> Option<&'static str> {
    let epoch = CATALOG_EPOCH.with(Cell::get);
    let candidate = CACHE.with(|cache| {
        let cache = cache.borrow();
        cache
            .iter()
            .find(|entry| &entry.key == key && entry.catalog_epoch == epoch)
            .cloned()
    })?;
    if !candidate.dependencies.is_empty()
        && !revalidate_planner_dependencies(&candidate.dependencies)
    {
        CACHE.with(|cache| cache.borrow_mut().retain(|entry| &entry.key != key));
        return None;
    }
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(index) = cache.iter().position(|entry| &entry.key == key) {
            let entry = cache.remove(index).expect("cache index remains valid");
            cache.push_front(entry);
        }
    });
    Some(candidate.reason)
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
            dependencies,
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
        PREV_PLANNER_HOOK = pg_sys::planner_hook;
        pg_sys::planner_hook = Some(planner_hook);
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

    fn key(value: u64) -> DeclineCacheKey {
        DeclineCacheKey::new(
            QueryFingerprint::new(&value.to_le_bytes(), 1, 10, b"public", true)
                .expect("small query key"),
            10_f64.to_bits(),
            2_f64.to_bits(),
            DeclinePolicyKey::Structural,
        )
    }

    fn key_with_bytes(value: u8, len: usize) -> DeclineCacheKey {
        DeclineCacheKey::new(
            QueryFingerprint::new(&vec![value; len], 1, 10, b"", true).expect("bounded query key"),
            10_f64.to_bits(),
            2_f64.to_bits(),
            DeclinePolicyKey::Structural,
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
    fn forced_spatial_test_policy_is_part_of_the_device_key() {
        let common = |test_force_spatial_groupagg| DeclinePolicyKey::Device {
            auto_load: true,
            resident_budget_bytes: 1024,
            cost_multiplier_bits: 1.0_f64.to_bits(),
            device_limits: 7,
            test_force_spatial_groupagg,
        };
        assert_ne!(common(false), common(true));
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
        assert!(QueryFingerprint::new(&vec![0; MAX_QUERY_BYTES], 0, 10, b"", false).is_some());
        assert!(QueryFingerprint::new(&vec![0; MAX_QUERY_BYTES + 1], 0, 10, b"", false).is_none());
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

    #[test]
    fn active_source_stack_restores_outer_identity() {
        ACTIVE_PLANNER_SOURCES.with(|sources| sources.borrow_mut().clear());
        let outer = std::ptr::without_provenance_mut::<pg_sys::Query>(1);
        let inner = std::ptr::without_provenance_mut::<pg_sys::Query>(2);
        push_active_source(outer, Some(Rc::from(&b"outer"[..])));
        push_active_source(inner, Some(Rc::from(&b"inner"[..])));
        ACTIVE_PLANNER_SOURCES.with(|sources| {
            let sources = sources.borrow();
            assert_eq!(sources.len(), 2);
            assert_eq!(
                sources.last().map(|source| source.parse),
                Some(inner.addr())
            );
        });
        pop_active_source();
        ACTIVE_PLANNER_SOURCES.with(|sources| {
            let sources = sources.borrow();
            assert_eq!(sources.len(), 1);
            assert_eq!(
                sources.last().map(|source| source.parse),
                Some(outer.addr())
            );
        });
        pop_active_source();
        ACTIVE_PLANNER_SOURCES.with(|sources| assert!(sources.borrow().is_empty()));
    }
}
