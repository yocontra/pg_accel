//! Planner hook installation for pg_accel Custom Scan injection.
//!
//! Installs `set_rel_pathlist_hook` (scan) and `set_join_pathlist_hook` (join)
//! so the planner considers GPU-accelerated paths for qualifying relations.

use pgrx::pg_sys::{
    self, CustomPath, JoinPathExtraData, List, NodeTag, Path, PlannerInfo, RangeTblEntry,
    RelOptInfo, RestrictInfo, add_path, lappend,
};

use super::custom_scan;
use crate::engine::gucs;
use crate::engine::registry;

// ---------------------------------------------------------------------------
// Previous hook storage
// ---------------------------------------------------------------------------

static mut PREV_SET_REL_PATHLIST_HOOK: pg_sys::set_rel_pathlist_hook_type = None;
static mut PREV_SET_JOIN_PATHLIST_HOOK: pg_sys::set_join_pathlist_hook_type = None;

// ---------------------------------------------------------------------------
// Hook installation
// ---------------------------------------------------------------------------

/// Install planner hooks. Must be called from `_PG_init` after
/// [`custom_scan::register`].
///
/// # Safety
///
/// Must only be called once, on the main backend thread, during extension load.
pub unsafe fn install() {
    // SAFETY: Accessing global hook variables is safe during _PG_init, which
    // runs single-threaded before any queries.
    unsafe {
        PREV_SET_REL_PATHLIST_HOOK = pg_sys::set_rel_pathlist_hook;
        pg_sys::set_rel_pathlist_hook = Some(pgaccel_set_rel_pathlist);

        PREV_SET_JOIN_PATHLIST_HOOK = pg_sys::set_join_pathlist_hook;
        pg_sys::set_join_pathlist_hook = Some(pgaccel_set_join_pathlist);
    }
}

// ---------------------------------------------------------------------------
// Scan hook
// ---------------------------------------------------------------------------

/// `set_rel_pathlist_hook` implementation.
///
/// Injects a `CustomPath` for base relations when:
/// 1. `pg_accel.enabled` is on.
/// 2. The relation is a base relation (`RELOPT_BASEREL` + `RTE_RELATION`).
/// 3. The estimated row count meets `pg_accel.min_batch_size`.
/// 4. A cheapest path exists to wrap.
/// 5. Restriction clauses contain a top-level `FuncExpr`.
///
/// # Safety
///
/// Called by the PostgreSQL planner on the main backend thread.
unsafe extern "C-unwind" fn pgaccel_set_rel_pathlist(
    root: *mut PlannerInfo,
    rel: *mut RelOptInfo,
    rti: pg_sys::Index,
    rte: *mut RangeTblEntry,
) {
    // Chain previous hook first.
    // SAFETY: Previous hook, if set, accepts the same planner-provided args.
    unsafe {
        if let Some(prev) = PREV_SET_REL_PATHLIST_HOOK {
            prev(root, rel, rti, rte);
        }
    }

    // Gate 1: GUC check.
    if !gucs::enabled() {
        return;
    }

    // Ensure the adapter registry is populated (first call does SPI probing).
    registry::lazy_init();

    // SAFETY: rel and rte are valid pointers provided by the planner.
    let rel_ref = unsafe { &*rel };
    let rte_ref = unsafe { &*rte };

    // Gate 2: Only base table relations.
    if rel_ref.reloptkind != pg_sys::RelOptKind::RELOPT_BASEREL {
        return;
    }
    if rte_ref.rtekind != pg_sys::RTEKind::RTE_RELATION {
        return;
    }

    // Gate 3: Skip small relations.
    let min_rows = f64::from(gucs::min_batch_size());
    if rel_ref.rows < min_rows {
        return;
    }

    // Gate 4: Find cheapest path. Hook fires BEFORE set_cheapest(), so
    // cheapest_total_path may be NULL — scan the pathlist manually.
    let cheapest = unsafe { find_cheapest_path(rel_ref.pathlist) };
    if cheapest.is_null() {
        return;
    }

    // Gate 5: Check if restriction clauses contain a registered function.
    if !has_accelerable_restriction(rel_ref.baserestrictinfo) {
        return;
    }

    // Build cost from cheapest path.
    // SAFETY: cheapest is non-null, checked above.
    let base = unsafe { &*cheapest };
    let startup_cost = base.startup_cost + 1.0;
    let total_cost = base.total_cost * 0.8;

    // SAFETY: Allocating via palloc, building valid CustomPath.
    unsafe {
        let cpath = create_custom_path(
            rel,
            cheapest,
            startup_cost,
            total_cost,
            base.rows,
            custom_scan::scan_path_methods(),
        );
        add_path(rel, cpath.cast());
    }
}

// ---------------------------------------------------------------------------
// Join hook
// ---------------------------------------------------------------------------

/// `set_join_pathlist_hook` implementation.
///
/// Injects a `CustomPath` for joins with accelerable residual conditions.
///
/// # Safety
///
/// Called by the PostgreSQL planner on the main backend thread.
unsafe extern "C-unwind" fn pgaccel_set_join_pathlist(
    root: *mut PlannerInfo,
    joinrel: *mut RelOptInfo,
    outerrel: *mut RelOptInfo,
    innerrel: *mut RelOptInfo,
    jointype: pg_sys::JoinType::Type,
    extra: *mut JoinPathExtraData,
) {
    // Chain previous hook first.
    // SAFETY: Previous hook, if set, accepts the same planner-provided args.
    unsafe {
        if let Some(prev) = PREV_SET_JOIN_PATHLIST_HOOK {
            prev(root, joinrel, outerrel, innerrel, jointype, extra);
        }
    }

    // Gate 1: GUC check.
    if !gucs::enabled() {
        return;
    }

    // Ensure the adapter registry is populated (first call does SPI probing).
    registry::lazy_init();

    // SAFETY: pointers provided by the planner are valid.
    let joinrel_ref = unsafe { &*joinrel };
    let outerrel_ref = unsafe { &*outerrel };
    let innerrel_ref = unsafe { &*innerrel };
    let extra_ref = unsafe { &*extra };

    // Gate 2: Enough rows.
    let min_rows = f64::from(gucs::min_batch_size());
    if joinrel_ref.rows < min_rows {
        return;
    }

    // Gate 3: Check join restrictlist for accelerable FuncExpr.
    if !has_accelerable_restriction(extra_ref.restrictlist) {
        return;
    }

    // Gate 4: Both sides need cheapest paths.
    let outer_path = outerrel_ref.cheapest_total_path;
    let inner_path = innerrel_ref.cheapest_total_path;
    if outer_path.is_null() || inner_path.is_null() {
        return;
    }

    // Cost: combine outer + inner total, apply 20% reduction estimate.
    // SAFETY: paths are non-null, verified above.
    let outer_cost = unsafe { (*outer_path).total_cost };
    let inner_cost = unsafe { (*inner_path).total_cost };
    let startup_cost = unsafe { (*outer_path).startup_cost } + 1.0;
    let total_cost = (outer_cost + inner_cost) * 0.8;

    // SAFETY: Allocating via palloc, building valid CustomPath.
    unsafe {
        let cpath = create_custom_path(
            joinrel,
            outer_path,
            startup_cost,
            total_cost,
            joinrel_ref.rows,
            custom_scan::join_path_methods(),
        );

        // Attach both child paths.
        let mut child_list: *mut List = std::ptr::null_mut();
        child_list = lappend(child_list, outer_path.cast());
        child_list = lappend(child_list, inner_path.cast());
        (*cpath).custom_paths = child_list;

        add_path(joinrel, cpath.cast());
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find the cheapest total-cost path in a pathlist.
///
/// # Safety
///
/// `pathlist` must be a valid PG `List` pointer or null.
unsafe fn find_cheapest_path(pathlist: *mut List) -> *mut Path {
    if pathlist.is_null() {
        return std::ptr::null_mut();
    }

    // SAFETY: pathlist is a valid List from the planner.
    let len = unsafe { pg_sys::list_length(pathlist) };
    if len == 0 {
        return std::ptr::null_mut();
    }

    let mut best: *mut Path = std::ptr::null_mut();
    for i in 0..len {
        // SAFETY: i is in [0, len), list_nth returns a valid pointer.
        let path = unsafe { pg_sys::list_nth(pathlist, i).cast::<Path>() };
        if path.is_null() {
            continue;
        }
        if best.is_null() || unsafe { (*path).total_cost < (*best).total_cost } {
            best = path;
        }
    }
    best
}

/// Check if a `List` of `RestrictInfo` contains a function registered in the
/// acceleration registry.
///
/// Extracts `funcid` from `FuncExpr` nodes and `opfuncid` from `OpExpr`
/// nodes, then looks them up in the global adapter registry.
fn has_accelerable_restriction(restrictinfo_list: *mut List) -> bool {
    if restrictinfo_list.is_null() {
        return false;
    }

    let reg = registry::global_registry();

    // SAFETY: restrictinfo_list is a valid List pointer from the planner.
    let len = unsafe { pg_sys::list_length(restrictinfo_list) };
    for i in 0..len {
        // SAFETY: i is in [0, len), list_nth returns a valid RestrictInfo*.
        let ri = unsafe { pg_sys::list_nth(restrictinfo_list, i).cast::<RestrictInfo>() };
        if ri.is_null() {
            continue;
        }
        // SAFETY: ri is a valid RestrictInfo from the planner.
        let clause = unsafe { (*ri).clause };
        if clause.is_null() {
            continue;
        }
        // SAFETY: clause is an Expr*; we check its NodeTag.
        let tag = unsafe { (*clause).type_ };

        // Extract function OID from the clause node, if applicable.
        // SAFETY: PG nodes are palloc'd (always >=8-byte aligned), and we
        // confirmed the NodeTag before casting.
        #[allow(clippy::cast_ptr_alignment)]
        let func_oid = if tag == NodeTag::T_FuncExpr {
            // SAFETY: tag confirmed this is a FuncExpr.
            Some(unsafe { (*clause.cast::<pg_sys::FuncExpr>()).funcid })
        } else if tag == NodeTag::T_OpExpr {
            // SAFETY: tag confirmed this is an OpExpr.
            let oid = unsafe { (*clause.cast::<pg_sys::OpExpr>()).opfuncid };
            // opfuncid may be invalid if not yet resolved by the planner.
            (oid != pg_sys::Oid::INVALID).then_some(oid)
        } else {
            None
        };

        if let Some(oid) = func_oid
            && reg.lookup(oid).is_some()
        {
            return true;
        }
    }

    false
}

/// Allocate and initialize a `CustomPath` node via `palloc0`.
///
/// IMPORTANT: The child path is copied into independently allocated memory
/// because `add_path` will pfree dominated paths. If our custom path is
/// cheaper, the original path would be freed, leaving a dangling pointer
/// in `custom_paths`.
///
/// # Safety
///
/// `rel` and `base_path` must be valid planner pointers. `methods` must
/// point to a static `CustomPathMethods` with `'static` lifetime.
unsafe fn create_custom_path(
    rel: *mut RelOptInfo,
    base_path: *mut Path,
    startup_cost: pg_sys::Cost,
    total_cost: pg_sys::Cost,
    rows: f64,
    methods: *const pg_sys::CustomPathMethods,
) -> *mut CustomPath {
    // SAFETY: palloc0 returns zeroed memory of the requested size.
    let cpath = unsafe { pg_sys::palloc0(std::mem::size_of::<CustomPath>()).cast::<CustomPath>() };

    // Copy child path to avoid dangling pointer after add_path pfrees.
    let child_copy = unsafe {
        let p = pg_sys::palloc0(std::mem::size_of::<Path>()).cast::<Path>();
        std::ptr::copy_nonoverlapping(base_path, p, 1);
        p
    };

    // SAFETY: cpath is freshly allocated and zeroed; all fields set below.
    unsafe {
        (*cpath).path.type_ = NodeTag::T_CustomPath;
        (*cpath).path.pathtype = NodeTag::T_CustomScan;
        (*cpath).path.parent = rel;
        (*cpath).path.pathtarget = (*rel).reltarget;
        (*cpath).path.param_info = (*base_path).param_info;
        (*cpath).path.parallel_aware = false;
        (*cpath).path.parallel_safe = (*base_path).parallel_safe;
        (*cpath).path.parallel_workers = 0;
        (*cpath).path.rows = rows;
        (*cpath).path.startup_cost = startup_cost;
        (*cpath).path.total_cost = total_cost;
        (*cpath).path.pathkeys = (*base_path).pathkeys;

        (*cpath).flags = 0;
        (*cpath).custom_paths = lappend(std::ptr::null_mut(), child_copy.cast());
        (*cpath).custom_restrictinfo = std::ptr::null_mut();
        (*cpath).custom_private = std::ptr::null_mut();
        (*cpath).methods = methods;
    }

    cpath
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_accelerable_restriction_null_list_returns_false() {
        assert!(!has_accelerable_restriction(std::ptr::null_mut()));
    }
}
