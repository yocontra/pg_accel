//! `set_rel_pathlist_hook` — injects `CustomPath`s for base relations.
//!
//! Entry points:
//! - `pgaccel_set_rel_pathlist` — main hook fn (registered in `install()`)
//! - `try_inject_gpu_sort_path` — scan-level GPU sort injection helper

use pgrx::pg_sys::{
    self, CustomPath, List, NodeTag, Path, PlannerInfo, RangeTblEntry, RelOptInfo, lappend,
};

use super::super::custom_scan;
use super::{PREV_SET_REL_PATHLIST_HOOK, find_cheapest_path, unwrap_var};
use crate::engine::cost;
use crate::engine::executor::sort::SortKeyDesc;
use crate::engine::expr_compiler::{CompiledExpr, TemplateKernel};
use crate::engine::gucs;
use crate::engine::registry;
use crate::engine::stats;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum H3LatLngQualDecline {
    UnsupportedShape,
    ScalarPredicateNoGpuPipeline,
}

impl H3LatLngQualDecline {
    const fn stats_key(self) -> &'static str {
        match self {
            Self::UnsupportedShape => super::RejectionReason::H3LatLngUnsupportedShape.stats_key(),
            Self::ScalarPredicateNoGpuPipeline => {
                super::RejectionReason::H3LatLngScalarPredicateNoGpuPipeline.stats_key()
            }
        }
    }
}

unsafe fn h3_point_var_node(node: *mut pg_sys::Node) -> bool {
    // SAFETY: callers pass a node from the current planner expression tree;
    // unwrap_var only follows tag-checked RelabelType links in that tree.
    let var = unsafe { unwrap_var(node) };
    if var.is_null() {
        return false;
    }
    // SAFETY: unwrap_var returned non-null only after proving the Var NodeTag.
    let vartype = u32::from(unsafe { (*var).vartype });
    // SAFETY: the same tag-checked Var remains live in planner memory.
    let attno = unsafe { (*var).varattno };
    vartype == pg_sys::POINTOID.to_u32() && attno > 0
}

unsafe fn h3_resolution_const_node(node: *mut pg_sys::Node) -> bool {
    // SAFETY: short-circuiting proves the planner-owned node is non-null before
    // reading its common NodeTag field.
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_Const {
        return false;
    }
    let cst = node.cast::<pg_sys::Const>();
    // SAFETY: the checked NodeTag proves cst has Const layout.
    if unsafe { (*cst).constisnull } {
        return false;
    }
    // SAFETY: cst is a non-null, tag-checked planner Const.
    let datum = unsafe { (*cst).constvalue };
    // SAFETY: the same tag-checked Const remains readable in planner memory.
    let resolution = match u32::from(unsafe { (*cst).consttype }) {
        21 => i32::from(datum.value() as i16),
        23 => datum.value() as i32,
        20 => (datum.value() as i64).try_into().unwrap_or(i32::MAX),
        _ => return false,
    };
    (0..=15).contains(&resolution)
}

unsafe fn h3_latlng_args_supported(args: *mut List) -> bool {
    // SAFETY: short-circuiting checks the planner-owned List pointer before
    // asking PostgreSQL for its length.
    if args.is_null() || unsafe { pg_sys::list_length(args) } != 2 {
        return false;
    }
    // SAFETY: the exact length check proves index 0 exists in this planner List.
    let arg0 = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
    // SAFETY: the exact length check proves index 1 exists in this planner List.
    let arg1 = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };
    // SAFETY: arg0 is the node pointer stored at a valid List index.
    let point_supported = unsafe { h3_point_var_node(arg0) };
    // SAFETY: arg1 is the node pointer stored at a valid List index.
    let resolution_supported = unsafe { h3_resolution_const_node(arg1) };
    point_supported && resolution_supported
}

fn merge_h3_latlng_decline(
    current: Option<H3LatLngQualDecline>,
    next: Option<H3LatLngQualDecline>,
) -> Option<H3LatLngQualDecline> {
    match (current, next) {
        (Some(H3LatLngQualDecline::UnsupportedShape), _) => {
            Some(H3LatLngQualDecline::UnsupportedShape)
        }
        (_, Some(H3LatLngQualDecline::UnsupportedShape)) => {
            Some(H3LatLngQualDecline::UnsupportedShape)
        }
        (Some(H3LatLngQualDecline::ScalarPredicateNoGpuPipeline), _)
        | (_, Some(H3LatLngQualDecline::ScalarPredicateNoGpuPipeline)) => {
            Some(H3LatLngQualDecline::ScalarPredicateNoGpuPipeline)
        }
        (None, None) => None,
    }
}

unsafe fn h3_latlng_qual_decline_list(args: *mut List) -> Option<H3LatLngQualDecline> {
    if args.is_null() {
        return None;
    }
    // SAFETY: args was checked non-null and is a planner-owned expression List.
    let len = unsafe { pg_sys::list_length(args) };
    let mut out = None;
    for i in 0..len {
        // SAFETY: i is bounded by list_length(args).
        let child = unsafe { pg_sys::list_nth(args, i).cast::<pg_sys::Node>() };
        // SAFETY: child is the planner-owned node stored at that valid List index.
        out = merge_h3_latlng_decline(out, unsafe { h3_latlng_qual_decline_node(child) });
        if out == Some(H3LatLngQualDecline::UnsupportedShape) {
            return out;
        }
    }
    out
}

unsafe fn h3_latlng_qual_decline_node(node: *mut pg_sys::Node) -> Option<H3LatLngQualDecline> {
    if node.is_null() {
        return None;
    }
    // SAFETY: node was checked non-null and belongs to the current planner tree.
    match unsafe { (*node).type_ } {
        NodeTag::T_FuncExpr => {
            let funcexpr = node.cast::<pg_sys::FuncExpr>();
            // SAFETY: the matched NodeTag proves the FuncExpr layout; catalog
            // lookup runs on the backend thread during planning.
            let fn_name = unsafe { function_name_for_oid((*funcexpr).funcid) };
            if fn_name.as_deref() == Some("h3_latlng_to_cell") {
                // SAFETY: the matched FuncExpr owns this planner argument List.
                return if unsafe { h3_latlng_args_supported((*funcexpr).args) } {
                    Some(H3LatLngQualDecline::ScalarPredicateNoGpuPipeline)
                } else {
                    Some(H3LatLngQualDecline::UnsupportedShape)
                };
            }
            // SAFETY: the matched FuncExpr owns this planner argument List.
            unsafe { h3_latlng_qual_decline_list((*funcexpr).args) }
        }
        NodeTag::T_OpExpr => {
            let opexpr = node.cast::<pg_sys::OpExpr>();
            // SAFETY: the matched NodeTag proves OpExpr layout and its args List
            // remains owned by the current planner expression.
            unsafe { h3_latlng_qual_decline_list((*opexpr).args) }
        }
        NodeTag::T_BoolExpr => {
            let bool_expr = node.cast::<pg_sys::BoolExpr>();
            // SAFETY: the matched NodeTag proves BoolExpr layout and its args
            // are nodes in the same planner expression.
            unsafe { h3_latlng_qual_decline_list((*bool_expr).args) }
        }
        NodeTag::T_BooleanTest => {
            let bool_test = node.cast::<pg_sys::BooleanTest>();
            // SAFETY: the matched NodeTag proves BooleanTest layout; arg is its
            // planner-owned child node.
            unsafe { h3_latlng_qual_decline_node((*bool_test).arg.cast::<pg_sys::Node>()) }
        }
        NodeTag::T_NullTest => {
            let null_test = node.cast::<pg_sys::NullTest>();
            // SAFETY: the matched NodeTag proves NullTest layout; arg is its
            // planner-owned child node.
            unsafe { h3_latlng_qual_decline_node((*null_test).arg.cast::<pg_sys::Node>()) }
        }
        NodeTag::T_RelabelType => {
            let relabel = node.cast::<pg_sys::RelabelType>();
            // SAFETY: the matched NodeTag proves RelabelType layout; arg is its
            // planner-owned child node.
            unsafe { h3_latlng_qual_decline_node((*relabel).arg.cast::<pg_sys::Node>()) }
        }
        NodeTag::T_CoerceViaIO => {
            let coercion = node.cast::<pg_sys::CoerceViaIO>();
            // SAFETY: the matched NodeTag proves CoerceViaIO layout; arg is its
            // planner-owned child node.
            unsafe { h3_latlng_qual_decline_node((*coercion).arg.cast::<pg_sys::Node>()) }
        }
        NodeTag::T_ScalarArrayOpExpr => {
            let scalar_array = node.cast::<pg_sys::ScalarArrayOpExpr>();
            // SAFETY: the matched NodeTag proves ScalarArrayOpExpr layout and
            // its args List remains planner-owned.
            unsafe { h3_latlng_qual_decline_list((*scalar_array).args) }
        }
        NodeTag::T_CaseExpr => {
            let case_expr = node.cast::<pg_sys::CaseExpr>();
            // SAFETY: the matched NodeTag proves CaseExpr layout; arg is its
            // planner-owned optional child node.
            let mut out =
                unsafe { h3_latlng_qual_decline_node((*case_expr).arg.cast::<pg_sys::Node>()) };
            // SAFETY: the same CaseExpr owns its planner argument List.
            out = merge_h3_latlng_decline(out, unsafe {
                h3_latlng_qual_decline_list((*case_expr).args)
            });
            // SAFETY: defresult is the planner-owned default child of this
            // tag-checked CaseExpr.
            merge_h3_latlng_decline(out, unsafe {
                h3_latlng_qual_decline_node((*case_expr).defresult.cast::<pg_sys::Node>())
            })
        }
        NodeTag::T_CaseWhen => {
            let case_when = node.cast::<pg_sys::CaseWhen>();
            // SAFETY: the matched NodeTag proves CaseWhen layout; expr is its
            // planner-owned condition child.
            let out =
                unsafe { h3_latlng_qual_decline_node((*case_when).expr.cast::<pg_sys::Node>()) };
            // SAFETY: result is the planner-owned result child of the same
            // tag-checked CaseWhen.
            merge_h3_latlng_decline(out, unsafe {
                h3_latlng_qual_decline_node((*case_when).result.cast::<pg_sys::Node>())
            })
        }
        NodeTag::T_CoalesceExpr => {
            let coalesce = node.cast::<pg_sys::CoalesceExpr>();
            // SAFETY: the matched NodeTag proves CoalesceExpr layout and its
            // args List remains planner-owned.
            unsafe { h3_latlng_qual_decline_list((*coalesce).args) }
        }
        _ => None,
    }
}

unsafe fn h3_latlng_qual_decline(restrictinfo_list: *mut List) -> Option<H3LatLngQualDecline> {
    if restrictinfo_list.is_null() {
        return None;
    }
    // SAFETY: restrictinfo_list was checked non-null and is planner-owned.
    let len = unsafe { pg_sys::list_length(restrictinfo_list) };
    let mut out = None;
    for i in 0..len {
        // SAFETY: i is bounded by list_length(restrictinfo_list).
        let ri = unsafe { pg_sys::list_nth(restrictinfo_list, i).cast::<pg_sys::RestrictInfo>() };
        if ri.is_null() {
            continue;
        }
        // SAFETY: ri is the non-null RestrictInfo at a valid planner List index.
        let clause = unsafe { (*ri).clause };
        // SAFETY: clause is the planner-owned expression referenced by ri.
        out = merge_h3_latlng_decline(out, unsafe { h3_latlng_qual_decline_node(clause.cast()) });
        if out == Some(H3LatLngQualDecline::UnsupportedShape) {
            return out;
        }
    }
    out
}

/// Resolve a function OID to a lowercase SQL function name.
///
/// The adapter registry stores OIDs, while the standalone-scan gates need to
/// distinguish cheap scalar functions from compute-heavy kernels.
unsafe fn function_name_for_oid(fn_oid: pg_sys::Oid) -> Option<String> {
    if fn_oid == pg_sys::Oid::INVALID {
        return None;
    }

    // SAFETY: get_func_name is a backend catalog lookup. The planner hook runs
    // on the main backend thread, and null simply means the OID was not found.
    let name_ptr = unsafe { pg_sys::get_func_name(fn_oid) };
    if name_ptr.is_null() {
        return None;
    }

    // SAFETY: name_ptr is a null-terminated C string owned by the current PG
    // memory context.
    let name_cstr = unsafe { std::ffi::CStr::from_ptr(name_ptr) };
    name_cstr.to_str().ok().map(str::to_ascii_lowercase)
}

unsafe fn postgis_function_name_for_oid(fn_oid: pg_sys::Oid) -> Option<String> {
    // SAFETY: this planner callback runs on PostgreSQL's backend thread, and
    // fn_oid is checked against the live extension catalogs.
    if !unsafe { super::super::syscache::function_is_extension_member(fn_oid, "postgis") } {
        return None;
    }
    // SAFETY: the same backend-thread catalog context keeps the function and
    // namespace syscache lookups valid for this call.
    let (_, name) = unsafe { super::super::syscache::function_schema_and_name(fn_oid) }?;
    Some(name)
}

unsafe fn oid_is_postgis_spatial_scalar_type(type_oid: pg_sys::Oid) -> bool {
    if type_oid == pg_sys::InvalidOid {
        return false;
    }

    // SAFETY: getBaseType is called on the backend thread with a non-invalid OID.
    let base_type = unsafe { pg_sys::getBaseType(type_oid) };
    if base_type == pg_sys::InvalidOid {
        return false;
    }
    // SAFETY: base_type is a live catalog OID resolved by PostgreSQL above.
    if !unsafe { super::super::syscache::type_is_extension_member(base_type, "postgis") } {
        return false;
    }
    // SAFETY: base_type was resolved and proved to belong to the PostGIS
    // extension in this backend's current catalogs.
    unsafe { super::super::syscache::type_name(base_type) }
        .as_deref()
        .is_some_and(|name| matches!(name, "geometry" | "geography"))
}

unsafe fn expr_list_has_spatial_prefix(args: *mut List, required: usize) -> bool {
    // SAFETY: short-circuiting checks the planner-owned List pointer before
    // PostgreSQL reads its length.
    if args.is_null() || unsafe { pg_sys::list_length(args) } < required as i32 {
        return false;
    }

    for i in 0..required {
        // SAFETY: i is below required, which the length check proved is present.
        let child = unsafe { pg_sys::list_nth(args, i as i32).cast::<pg_sys::Node>() };
        if child.is_null() {
            return false;
        }
        // SAFETY: child is a non-null planner Node obtained from a valid List index.
        let arg_type = unsafe { pg_sys::exprType(child) };
        // SAFETY: arg_type came from PostgreSQL's expression metadata and catalog
        // inspection runs on the current backend thread.
        if !unsafe { oid_is_postgis_spatial_scalar_type(arg_type) } {
            return false;
        }
    }
    true
}

unsafe fn function_matches_postgis_spatial_prefix(
    fn_oid: pg_sys::Oid,
    args: *mut List,
    name_predicate: impl FnOnce(&str) -> bool,
) -> bool {
    // SAFETY: fn_oid is planner expression metadata and catalog access occurs on
    // the backend thread during this callback.
    let Some(name) = (unsafe { postgis_function_name_for_oid(fn_oid) }) else {
        return false;
    };
    // SAFETY: args is the argument List of the tag-checked expression supplied
    // by the caller; the helper validates its length before indexing.
    name_predicate(&name) && unsafe { expr_list_has_spatial_prefix(args, 2) }
}

unsafe fn funcexpr_is_postgis_intersects(funcexpr: *mut pg_sys::FuncExpr) -> bool {
    // SAFETY: callers invoke this only after matching the FuncExpr NodeTag.
    let funcid = unsafe { (*funcexpr).funcid };
    // SAFETY: the same tag-checked planner FuncExpr remains readable.
    let args = unsafe { (*funcexpr).args };
    // SAFETY: funcid and args were read from that live FuncExpr and catalog
    // resolution runs on the backend thread.
    unsafe { function_matches_postgis_spatial_prefix(funcid, args, |name| name == "st_intersects") }
}

unsafe fn opexpr_is_postgis_intersects(opexpr: *mut pg_sys::OpExpr) -> bool {
    // SAFETY: callers invoke this only after matching the OpExpr NodeTag.
    let mut opfuncid = unsafe { (*opexpr).opfuncid };
    if opfuncid == pg_sys::InvalidOid {
        // SAFETY: opexpr is a writable planner-owned OpExpr and syscache lookup
        // runs on the current backend thread.
        unsafe { pg_sys::set_opfuncid(opexpr) };
        // SAFETY: set_opfuncid initialized this field on the same live OpExpr.
        opfuncid = unsafe { (*opexpr).opfuncid };
    }
    // SAFETY: the tag-checked planner OpExpr remains readable.
    let args = unsafe { (*opexpr).args };
    // SAFETY: opfuncid and args belong to that live OpExpr and the helper bounds
    // all List access before catalog classification.
    unsafe {
        function_matches_postgis_spatial_prefix(opfuncid, args, |name| name == "st_intersects")
    }
}

unsafe fn restrictinfo_contains_wrapped_postgis_intersects(restrictinfo_list: *mut List) -> bool {
    if restrictinfo_list.is_null() {
        return false;
    }

    // SAFETY: restrictinfo_list was checked non-null and is planner-owned.
    let len = unsafe { pg_sys::list_length(restrictinfo_list) };
    for i in 0..len {
        // SAFETY: i is bounded by list_length(restrictinfo_list).
        let ri = unsafe { pg_sys::list_nth(restrictinfo_list, i).cast::<pg_sys::RestrictInfo>() };
        if ri.is_null() {
            continue;
        }
        // SAFETY: ri is the non-null RestrictInfo stored at a valid List index.
        let clause = unsafe { (*ri).clause };
        // SAFETY: clause is the planner-owned expression referenced by ri.
        if unsafe { node_contains_wrapped_postgis_intersects(clause.cast()) } {
            return true;
        }
    }
    false
}

unsafe fn node_contains_wrapped_postgis_intersects(node: *mut pg_sys::Node) -> bool {
    if node.is_null() {
        return false;
    }

    // SAFETY: node was checked non-null and belongs to the current planner tree.
    let tag = unsafe { (*node).type_ };
    #[allow(clippy::cast_ptr_alignment)]
    match tag {
        NodeTag::T_FuncExpr => {
            let funcexpr = node.cast::<pg_sys::FuncExpr>();
            // SAFETY: the matched NodeTag proves FuncExpr layout and the helper
            // reads only this live planner node and backend catalogs.
            if unsafe { funcexpr_is_postgis_intersects(funcexpr) } {
                return true;
            }
            // SAFETY: the matched FuncExpr owns this planner argument List.
            unsafe { args_contain_wrapped_postgis_intersects((*funcexpr).args) }
        }
        NodeTag::T_OpExpr => {
            let opexpr = node.cast::<pg_sys::OpExpr>();
            // SAFETY: the matched NodeTag proves OpExpr layout and the helper may
            // initialize only its PostgreSQL-managed opfuncid cache field.
            if unsafe { opexpr_is_postgis_intersects(opexpr) } {
                return true;
            }
            // SAFETY: the matched OpExpr owns this planner argument List.
            unsafe { args_contain_wrapped_postgis_intersects((*opexpr).args) }
        }
        NodeTag::T_BoolExpr => {
            let bool_expr = node.cast::<pg_sys::BoolExpr>();
            // SAFETY: the matched NodeTag proves BoolExpr layout and its args
            // remain planner-owned.
            unsafe { args_contain_wrapped_postgis_intersects((*bool_expr).args) }
        }
        NodeTag::T_NullTest => {
            let null_test = node.cast::<pg_sys::NullTest>();
            // SAFETY: the matched NodeTag proves NullTest layout; arg is its
            // planner-owned child expression.
            unsafe { node_contains_wrapped_postgis_intersects((*null_test).arg.cast()) }
        }
        NodeTag::T_BooleanTest => {
            let bool_test = node.cast::<pg_sys::BooleanTest>();
            // SAFETY: the matched NodeTag proves BooleanTest layout; arg is its
            // planner-owned child expression.
            unsafe { node_contains_wrapped_postgis_intersects((*bool_test).arg.cast()) }
        }
        NodeTag::T_RelabelType => {
            let relabel = node.cast::<pg_sys::RelabelType>();
            // SAFETY: the matched NodeTag proves RelabelType layout; arg is its
            // planner-owned child expression.
            unsafe { node_contains_wrapped_postgis_intersects((*relabel).arg.cast()) }
        }
        NodeTag::T_CoerceViaIO => {
            let coercion = node.cast::<pg_sys::CoerceViaIO>();
            // SAFETY: the matched NodeTag proves CoerceViaIO layout; arg is its
            // planner-owned child expression.
            unsafe { node_contains_wrapped_postgis_intersects((*coercion).arg.cast()) }
        }
        NodeTag::T_CoerceToDomain => {
            let coercion = node.cast::<pg_sys::CoerceToDomain>();
            // SAFETY: the matched NodeTag proves CoerceToDomain layout; arg is
            // its planner-owned child expression.
            unsafe { node_contains_wrapped_postgis_intersects((*coercion).arg.cast()) }
        }
        NodeTag::T_CoalesceExpr => {
            let coalesce = node.cast::<pg_sys::CoalesceExpr>();
            // SAFETY: the matched NodeTag proves CoalesceExpr layout and its
            // args List remains planner-owned.
            unsafe { args_contain_wrapped_postgis_intersects((*coalesce).args) }
        }
        NodeTag::T_ScalarArrayOpExpr => {
            let scalar_array = node.cast::<pg_sys::ScalarArrayOpExpr>();
            // SAFETY: the matched NodeTag proves ScalarArrayOpExpr layout and
            // its args List remains planner-owned.
            unsafe { args_contain_wrapped_postgis_intersects((*scalar_array).args) }
        }
        NodeTag::T_CaseExpr => {
            let case_expr = node.cast::<pg_sys::CaseExpr>();
            // SAFETY: the matched NodeTag proves CaseExpr layout; all three
            // traversals remain within its planner-owned child nodes and List.
            unsafe {
                node_contains_wrapped_postgis_intersects((*case_expr).arg.cast())
                    || args_contain_wrapped_postgis_intersects((*case_expr).args)
                    || node_contains_wrapped_postgis_intersects((*case_expr).defresult.cast())
            }
        }
        NodeTag::T_CaseWhen => {
            let case_when = node.cast::<pg_sys::CaseWhen>();
            // SAFETY: the matched NodeTag proves CaseWhen layout; both pointers
            // are its planner-owned child expressions.
            unsafe {
                node_contains_wrapped_postgis_intersects((*case_when).expr.cast())
                    || node_contains_wrapped_postgis_intersects((*case_when).result.cast())
            }
        }
        NodeTag::T_ArrayExpr => {
            let array = node.cast::<pg_sys::ArrayExpr>();
            // SAFETY: the matched NodeTag proves ArrayExpr layout and elements
            // remains a planner-owned List.
            unsafe { args_contain_wrapped_postgis_intersects((*array).elements) }
        }
        NodeTag::T_MinMaxExpr => {
            let minmax = node.cast::<pg_sys::MinMaxExpr>();
            // SAFETY: the matched NodeTag proves MinMaxExpr layout and its args
            // remain planner-owned.
            unsafe { args_contain_wrapped_postgis_intersects((*minmax).args) }
        }
        _ => false,
    }
}

unsafe fn args_contain_wrapped_postgis_intersects(args: *mut List) -> bool {
    if args.is_null() {
        return false;
    }

    // SAFETY: args was checked non-null and is a planner-owned expression List.
    let len = unsafe { pg_sys::list_length(args) };
    for i in 0..len {
        // SAFETY: i is bounded by list_length(args).
        let child = unsafe { pg_sys::list_nth(args, i).cast::<pg_sys::Node>() };
        // SAFETY: child is the planner-owned node stored at that valid List index.
        if unsafe { node_contains_wrapped_postgis_intersects(child) } {
            return true;
        }
    }
    false
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
#[pgrx::pg_guard]
#[allow(clippy::too_many_lines)]
pub(super) unsafe extern "C-unwind" fn pgaccel_set_rel_pathlist(
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
    if super::planner_hooks_suspended() {
        return;
    }

    // Time this hook invocation so the benchmark harness can detect
    // star-schema no-dispatch regressions in planner overhead.
    let _hook_finish = super::HookElapsedGuard::new("rel_pathlist");

    // Record this planner hook invocation (main backend thread only).
    stats::record_planner_hook_call();

    // Gate 1: GUC check — single branch, ~1ns.
    if !gucs::enabled() {
        pgrx::debug1!("pg_accel: set_rel_pathlist: extension disabled");
        return;
    }

    // Gate: Only accelerate pure SELECT statements. INSERT...SELECT,
    // UPDATE...FROM, DELETE...USING run scans in ModifyTable context
    // where Custom Scan slot handling is incompatible.
    // SAFETY: root.parse is a valid Query pointer provided by the planner.
    let parse = unsafe { (*root).parse };
    // SAFETY: short-circuiting proves parse is non-null before reading the
    // command type from the planner-owned Query.
    if parse.is_null() || unsafe { (*parse).commandType } != pg_sys::CmdType::CMD_SELECT {
        stats::record_command_type_skip();
        return;
    }

    // SAFETY: rel and rte are valid pointers provided by the planner.
    let rel_ref = unsafe { &*rel };
    // SAFETY: rte is the planner-owned range-table entry supplied to this hook.
    let rte_ref = unsafe { &*rte };

    let _span =
        tracing::info_span!("planner.rel_pathlist", relid = u32::from(rte_ref.relid)).entered();

    // These cheap shape facts are needed both for resident-only fail-closed
    // behavior and the normal scan/sort admission gates below.
    // SAFETY: root is a valid PlannerInfo pointer.
    let has_sort = unsafe { !(*root).sort_pathkeys.is_null() };
    let has_restrictions = !rel_ref.baserestrictinfo.is_null()
        // SAFETY: the non-null baserestrictinfo pointer is a planner-owned List.
        && unsafe { pg_sys::list_length(rel_ref.baserestrictinfo) } > 0;

    // GPU-resident-only admission: pg_accel no longer injects a host-staged
    // scan/sort/function-scan CustomPath. Record the honest decline and leave
    // the query on PostgreSQL's native plan. When `gpu_enabled` is off,
    // pg_accel injects nothing here either.
    if gucs::gpu_enabled() {
        // SAFETY: all pointers are planner-owned for this hook invocation.
        let raster_observed = unsafe { super::raster::observe(root, rel, rti, rte) };
        if !raster_observed {
            // SAFETY: all pointers are planner-owned for this hook invocation.
            unsafe {
                observe_resident_only_rel_declines(root, rel, rte, has_sort, has_restrictions);
            }
            if rte_ref.rtekind == pg_sys::RTEKind::RTE_FUNCTION || has_sort || has_restrictions {
                super::record_no_gpu_resident_pipeline_decline(
                    "rel_pathlist_no_resident_pipeline",
                    rel,
                );
            }
        }
    }
}

unsafe fn observe_resident_only_rel_declines(
    root: *mut PlannerInfo,
    rel: *mut RelOptInfo,
    rte: *mut RangeTblEntry,
    has_sort: bool,
    has_restrictions: bool,
) {
    if root.is_null() || rel.is_null() || rte.is_null() {
        return;
    }
    // SAFETY: caller passed planner-owned pointers from set_rel_pathlist.
    let rel_ref = unsafe { &*rel };
    // SAFETY: rte was checked non-null and remains planner-owned for this callback.
    let rte_ref = unsafe { &*rte };

    if has_sort {
        // SAFETY: root and rel are non-null planner pointers supplied by the
        // enclosing set_rel_pathlist callback.
        unsafe { observe_resident_only_sort_declines(root, rel) };
    }

    if !has_restrictions
        || rte_ref.rtekind != pg_sys::RTEKind::RTE_RELATION
        || !matches!(
            rel_ref.reloptkind,
            pg_sys::RelOptKind::RELOPT_BASEREL | pg_sys::RelOptKind::RELOPT_OTHER_MEMBER_REL
        )
    {
        return;
    }

    #[allow(clippy::cast_sign_loss)]
    let rows = rel_ref.tuples.max(rel_ref.rows).max(0.0) as u64;
    // SAFETY: baserestrictinfo is the live planner List whose non-emptiness was
    // established by the caller's has_restrictions flag.
    if let Some(h3_decline) = unsafe { h3_latlng_qual_decline(rel_ref.baserestrictinfo) } {
        stats::increment_planner_rejected(h3_decline.stats_key(), rows);
        pgrx::debug1!(
            "pg_accel: set_rel_pathlist resident-only observer: h3_latlng_to_cell qual \
             declined ({:?}) before the generic resident-pipeline gate",
            h3_decline
        );
    }
    // SAFETY: baserestrictinfo is the same live planner-owned restriction List.
    if unsafe { restrictinfo_contains_wrapped_postgis_intersects(rel_ref.baserestrictinfo) } {
        stats::increment_planner_rejected(
            super::RejectionReason::PostgisIntersectsUnsupportedShape.stats_key(),
            rows,
        );
        pgrx::debug1!(
            "pg_accel: set_rel_pathlist resident-only observer: ST_Intersects shape declined \
             before the generic resident-pipeline gate"
        );
    }
}

unsafe fn observe_resident_only_sort_declines(root: *mut PlannerInfo, rel: *mut RelOptInfo) {
    if root.is_null() || rel.is_null() {
        return;
    }
    // SAFETY: caller passed planner-owned pointers from set_rel_pathlist.
    let root_ref = unsafe { &*root };
    // SAFETY: rel was checked non-null and remains planner-owned for this callback.
    let rel_ref = unsafe { &*rel };
    let sort_pathkeys = root_ref.sort_pathkeys;
    if sort_pathkeys.is_null() {
        return;
    }
    // SAFETY: sort_pathkeys is a valid planner List.
    let num_pathkeys = unsafe { pg_sys::list_length(sort_pathkeys) };
    if num_pathkeys < 1 {
        return;
    }

    #[allow(clippy::cast_sign_loss)]
    let rejected_rows = rel_ref.rows.max(0.0) as u64;
    // SAFETY: pathlist and sort_pathkeys are live planner Lists, and
    // num_pathkeys is the measured length of the latter.
    let presorted =
        unsafe { longest_presorted_prefix(rel_ref.pathlist, sort_pathkeys, num_pathkeys) };
    match classify_sort_shape(presorted, num_pathkeys) {
        SortShape::AlreadySorted { .. } => return,
        SortShape::IncrementalOpportunity { .. } => {
            stats::increment_planner_rejected("sort_incremental_opportunity", rejected_rows);
        }
        SortShape::FullSort { .. } => {}
    }

    if num_pathkeys > GPU_SORT_MAX_PATHKEYS {
        stats::increment_planner_rejected(
            super::RejectionReason::SortMultiKeyNoGpuKernel.stats_key(),
            rejected_rows,
        );
        return;
    }

    #[allow(clippy::cast_sign_loss)]
    let rows = rel_ref.rows.max(0.0) as usize;
    if rows < cost::device_limits().gpu_sort_planner_min_rows {
        return;
    }
    if min_max_rewrite_shape(root_ref.limit_tuples, num_pathkeys) {
        stats::increment_planner_rejected(
            super::RejectionReason::MinMaxRewriteNotASort.stats_key(),
            rejected_rows,
        );
        return;
    }
    if !heap_topk_sort_candidate(root_ref.limit_tuples, rows) {
        stats::increment_planner_rejected(
            super::RejectionReason::SortHeapFullOutput.stats_key(),
            rejected_rows,
        );
    }
}

/// Maximum number of pathkeys the GPU sort executor supports.
///
/// Pinned to 1: the executor in `engine/executor/sort/` only dispatches a
/// single-key GPU sort. Multi-key sort requires cascaded stable passes
/// (sort by last key first, then prior keys) and is tracked as post-1.0
/// work. Planner + executor MUST agree on this bound — otherwise the
/// planner injects paths the executor bails on, wasting a plan.
pub(super) const GPU_SORT_MAX_PATHKEYS: i32 = 1;

#[must_use]
#[inline]
pub(super) fn heap_topk_sort_candidate(limit_tuples: f64, rows: usize) -> bool {
    #[allow(clippy::cast_precision_loss)]
    let materialized_output_fraction = if rows == 0 {
        1.0
    } else {
        limit_tuples.ceil() / rows as f64
    };
    cost::formulas::sort_admission(
        cost::formulas::SortAdmissionInput {
            rows,
            limit_tuples: Some(limit_tuples),
            estimated_row_width: 0,
            key_count: 1,
            key_class: None,
            algorithm: cost::formulas::SortAlgorithm::StandaloneTopK,
            materialized_output_fraction,
            chunk_count: 1,
            cold_jit: false,
        },
        cost::device_limits(),
    )
    .eligible
}

/// Whether a (`limit_tuples`, `num_pathkeys`) pair looks like PostgreSQL's
/// MIN/MAX → IndexScan+Limit rewrite — equivalently, a single-column
/// ORDER BY with LIMIT 1.
///
/// PostgreSQL rewrites `SELECT MIN(x) FROM t` / `SELECT MAX(x) FROM t` into
/// an InitPlan shaped `SELECT x FROM t WHERE x IS NOT NULL ORDER BY x
/// [DESC] LIMIT 1` (see `preprocess_minmax_aggregates` in
/// `src/backend/optimizer/plan/planagg.c`). The base-relation planner hook
/// sees this as a regular ORDER BY + LIMIT 1 single-key sort and would
/// otherwise route it through `Strategy: GpuSort`, which runs a full GPU
/// sort just to return the first row.
///
/// At LIMIT 1, GPU sort is the wrong shape regardless of provenance:
/// - The MIN/MAX rewrite: PG's IndexScan+Limit (or sequential scan + Limit)
///   is single-digit-millisecond on a real index, while GpuSort is
///   hundreds-of-milliseconds for a full sort of the same input.
/// - A user-written `SELECT * FROM t ORDER BY x LIMIT 1`: same story.
///   GPU sort wins at large bounded top-K (LIMIT >= 2) where the win
///   amortises over the result; at LIMIT 1 the GPU launch + sort cost
///   dominates.
///
/// Predicate exact: `num_pathkeys == 1 && limit_tuples == 1.0`. This is
/// deliberately narrow — `LIMIT 100` over a single-column ORDER BY remains
/// a valid GpuSort path (handled by `heap_topk_sort_candidate`); multi-key
/// LIMIT 1 is not the MIN/MAX rewrite (PG only rewrites single-aggregate
/// MIN/MAX queries) so we leave it to the other gates.
#[must_use]
#[inline]
pub(super) fn min_max_rewrite_shape(limit_tuples: f64, num_pathkeys: i32) -> bool {
    // LIMIT 1 is the canonical MIN/MAX rewrite output cardinality. Use a
    // finite-equality check rather than a range so that LIMIT 2+ stays in
    // the legitimate top-K lane.
    if !limit_tuples.is_finite() {
        return false;
    }
    // PG's `preprocess_limit` lowers `LIMIT 1` to `limit_tuples == 1.0`.
    // Allow the open interval (0, 2) to catch any fractional rounding
    // (e.g. `limit_tuples = 1.0` exactly) while excluding LIMIT 0 (no rows)
    // and LIMIT >= 2.
    (limit_tuples > 0.0) && (limit_tuples < 2.0) && (num_pathkeys == 1)
}

/// Classification of how `root->sort_pathkeys` relates to the pathkeys of
/// paths already attached to the base relation.
///
/// Used by [`try_inject_gpu_sort_path`] to decide between full-sort
/// injection, a no-op (PG sees the sort as free), and an IncrementalSort
/// opportunity the production planner currently declines because no complete
/// resident IncrementalSort pipeline exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SortShape {
    /// No existing child path has any pathkey prefix of `sort_pathkeys`.
    /// `total` is `list_length(sort_pathkeys)`.
    FullSort { total: i32 },
    /// Some existing path's pathkeys already cover the full `sort_pathkeys`.
    /// No injection needed; PG treats it as free. `total` is carried for
    /// observability.
    AlreadySorted { total: i32 },
    /// Some existing path has a non-empty pathkey prefix of
    /// `sort_pathkeys` but does NOT cover it — an IncrementalSort
    /// opportunity. `presorted` is the matching prefix length, `suffix`
    /// is the remainder (`total - presorted`).
    IncrementalOpportunity {
        presorted: i32,
        suffix: i32,
        total: i32,
    },
}

/// Pure classifier for [`SortShape`] given list lengths.
///
/// Separated from the FFI wrapper so it can be unit-tested without a live
/// planner. `presorted` is the longest common prefix length between some
/// child path's pathkeys and `sort_pathkeys`. `total` is
/// `list_length(sort_pathkeys)`.
///
/// Preconditions (caller-enforced): `total >= 1`, `0 <= presorted <= total`.
pub(super) const fn classify_sort_shape(presorted: i32, total: i32) -> SortShape {
    if presorted >= total {
        SortShape::AlreadySorted { total }
    } else if presorted > 0 {
        SortShape::IncrementalOpportunity {
            presorted,
            suffix: total - presorted,
            total,
        }
    } else {
        SortShape::FullSort { total }
    }
}

/// Walk `rel->pathlist` and return the longest pathkey prefix of
/// `sort_pathkeys` shared by any attached path.
///
/// Uses PG's own [`pg_sys::pathkeys_count_contained_in`] — the same helper
/// `create_incremental_sort_path` uses in `src/backend/optimizer/util/pathkeys.c`
/// to decide the presorted-prefix length. A byte-wise list compare would
/// be wrong because PG canonicalises pathkeys and semantically equal keys
/// may be different `PathKey*` pointers.
///
/// Returns 0 when `pathlist` is empty or no path shares any prefix.
///
/// # Safety
///
/// `pathlist` and `sort_pathkeys` must be valid planner-provided `List*`
/// (possibly null for `pathlist`). `total_keys` must equal
/// `list_length(sort_pathkeys)`.
unsafe fn longest_presorted_prefix(
    pathlist: *mut List,
    sort_pathkeys: *mut List,
    total_keys: i32,
) -> i32 {
    if pathlist.is_null() || sort_pathkeys.is_null() || total_keys <= 0 {
        return 0;
    }
    // SAFETY: pathlist is a valid List.
    let n = unsafe { pg_sys::list_length(pathlist) };
    if n == 0 {
        return 0;
    }
    let mut best: i32 = 0;
    for i in 0..n {
        // SAFETY: i < list_length(pathlist).
        let p = unsafe { pg_sys::list_nth(pathlist, i).cast::<Path>() };
        if p.is_null() {
            continue;
        }
        // SAFETY: p is a valid Path node.
        let pk = unsafe { (*p).pathkeys };
        if pk.is_null() {
            continue;
        }
        // Ask PG how many leading keys of sort_pathkeys are covered by
        // this path's pathkeys. The return value is the "fully contained"
        // bool (true iff the entire sort is covered); we only use the
        // out-parameter.
        let mut n_common: i32 = 0;
        // SAFETY: both lists are valid List* of PathKey*; n_common ptr is
        // a stack local writable i32.
        let _fully_contained = unsafe {
            pg_sys::pathkeys_count_contained_in(sort_pathkeys, pk, std::ptr::addr_of_mut!(n_common))
        };
        if n_common > best {
            best = n_common;
        }
        if best >= total_keys {
            break;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::{
        GPU_SORT_MAX_PATHKEYS, SortShape, classify_sort_shape, heap_topk_sort_candidate,
        min_max_rewrite_shape,
    };
    use crate::engine::cost::DeviceLimits;
    use pgrx::pg_sys;

    /// Regression guard: the planner sort gate and the executor sort
    /// dispatcher must agree on the supported pathkey count. The executor
    /// in `engine/executor/sort/` bails on anything other than a single
    /// key (see `exec_gpu_sort`'s `sort_keys().len() == 1` path). If this
    /// constant grows above 1 without cascaded stable-sort support being
    /// landed in the executor, the planner will inject paths that bail
    /// at execution, wasting a plan. Pin it until the executor work lands.
    #[test]
    fn gpu_sort_max_pathkeys_matches_executor_support() {
        assert_eq!(
            GPU_SORT_MAX_PATHKEYS, 1,
            "executor only supports single-key GPU sort; see comment above GPU_SORT_MAX_PATHKEYS"
        );
    }

    #[test]
    fn heap_topk_width_cap_stays_narrow_until_late_fetch_lands() {
        assert_eq!(
            DeviceLimits::cpu_only().gpu_sort_heap_topk_max_width_bytes,
            16
        );
    }

    // -- classify_sort_shape ------------------------------------------------
    //
    // The classifier is a pure function on (presorted, total) list lengths
    // so it can be unit-tested without a live planner. The IncrementalSort
    // FFI wrapper feeds the output of pathkeys_count_contained_in into this
    // classifier; mocking the FFI side requires a live planner (see the
    // #[pg_test] coverage).

    #[test]
    fn classify_full_sort_when_no_presorted_prefix() {
        assert_eq!(classify_sort_shape(0, 1), SortShape::FullSort { total: 1 });
        assert_eq!(classify_sort_shape(0, 3), SortShape::FullSort { total: 3 });
    }

    #[test]
    fn classify_already_sorted_when_prefix_covers_total() {
        assert_eq!(
            classify_sort_shape(2, 2),
            SortShape::AlreadySorted { total: 2 }
        );
        // Defensive: presorted > total (shouldn't happen in practice
        // since PG returns n_common <= list_length(keys1), but classify
        // should not panic).
        assert_eq!(
            classify_sort_shape(5, 3),
            SortShape::AlreadySorted { total: 3 }
        );
    }

    #[test]
    fn classify_incremental_opportunity_when_strict_prefix() {
        let shape = classify_sort_shape(1, 3);
        assert_eq!(
            shape,
            SortShape::IncrementalOpportunity {
                presorted: 1,
                suffix: 2,
                total: 3,
            }
        );
    }

    #[test]
    fn classify_incremental_opportunity_covers_multi_key_suffix() {
        // The 4-key / 3-presorted case: one trailing key left to sort.
        let shape = classify_sort_shape(3, 4);
        assert_eq!(
            shape,
            SortShape::IncrementalOpportunity {
                presorted: 3,
                suffix: 1,
                total: 4,
            }
        );
    }

    #[test]
    fn classify_is_const_eval_compatible() {
        // const fn guarantee: calling at compile time must succeed.
        const SHAPE: SortShape = classify_sort_shape(1, 2);
        assert_eq!(
            SHAPE,
            SortShape::IncrementalOpportunity {
                presorted: 1,
                suffix: 1,
                total: 2,
            }
        );
    }

    // -- heap_topk_sort_candidate ------------------------------------------

    #[test]
    fn heap_topk_gate_rejects_no_limit_full_output_sort() {
        assert!(!heap_topk_sort_candidate(-1.0, 1_000_000));
        assert!(!heap_topk_sort_candidate(0.0, 1_000_000));
        assert!(!heap_topk_sort_candidate(f64::NAN, 1_000_000));
    }

    #[test]
    fn heap_topk_gate_rejects_limit_covering_all_rows() {
        assert!(!heap_topk_sort_candidate(1_000_000.0, 1_000_000));
        assert!(!heap_topk_sort_candidate(1_500_000.0, 1_000_000));
    }

    #[test]
    fn heap_topk_gate_rejects_non_selective_limit() {
        let rows = 1_000_000;
        let non_selective_limit =
            1_000_000.0 * (DeviceLimits::cpu_only().gpu_sort_heap_topk_max_fraction + 0.01);
        assert!(!heap_topk_sort_candidate(non_selective_limit, rows));
    }

    #[test]
    fn heap_topk_gate_rejects_limit_above_implemented_topk_bound() {
        assert!(!heap_topk_sort_candidate(
            DeviceLimits::cpu_only().gpu_sort_topk_max_limit as f64 + 1.0,
            1_000_000
        ));
    }

    #[test]
    fn heap_topk_gate_allows_bounded_topk() {
        assert!(heap_topk_sort_candidate(2.0, 1_000_000));
        assert!(heap_topk_sort_candidate(
            DeviceLimits::cpu_only().gpu_sort_topk_max_limit as f64,
            1_000_000
        ));
    }

    // -- min_max_rewrite_shape --------------------------------------------
    //
    // PG's `preprocess_minmax_aggregates` rewrites `SELECT MIN(x) FROM t`
    // (and MAX) to a subplan shaped `ORDER BY x LIMIT 1`. The base-relation
    // planner hook must NOT route this shape through `Strategy: GpuSort` —
    // a full GPU sort to fetch one row is hundreds of ms vs ~10-20 ms for
    // PG's native IndexScan/SeqScan + Limit. The gate is a pure function
    // of `(limit_tuples, num_pathkeys)` so it is unit-testable without a
    // live planner.

    #[test]
    fn min_max_rewrite_gate_matches_limit_1_single_key() {
        // The canonical MIN/MAX rewrite output: LIMIT 1 + 1 ORDER BY key.
        assert!(min_max_rewrite_shape(1.0, 1));
    }

    #[test]
    fn min_max_rewrite_gate_rejects_multi_key_sort() {
        // Multi-key ORDER BY LIMIT 1 is not the MIN/MAX rewrite (PG only
        // rewrites single-aggregate MIN/MAX). Other gates handle these.
        assert!(!min_max_rewrite_shape(1.0, 2));
        assert!(!min_max_rewrite_shape(1.0, 4));
    }

    #[test]
    fn min_max_rewrite_gate_rejects_bounded_topk_with_limit_above_one() {
        // Legitimate top-K — LIMIT 100, LIMIT 1000 etc. — stays in the
        // GpuSort lane handled by `heap_topk_sort_candidate`.
        assert!(!min_max_rewrite_shape(2.0, 1));
        assert!(!min_max_rewrite_shape(100.0, 1));
        assert!(!min_max_rewrite_shape(1_000.0, 1));
    }

    #[test]
    fn min_max_rewrite_gate_rejects_no_limit() {
        // No LIMIT, or LIMIT 0, or non-finite limit_tuples means PG didn't
        // emit the MIN/MAX rewrite shape. Other gates handle full sorts.
        assert!(!min_max_rewrite_shape(0.0, 1));
        assert!(!min_max_rewrite_shape(-1.0, 1));
        assert!(!min_max_rewrite_shape(f64::INFINITY, 1));
        assert!(!min_max_rewrite_shape(f64::NAN, 1));
    }

    #[test]
    fn min_max_rewrite_gate_rejects_zero_pathkeys() {
        // Zero pathkeys means no ORDER BY at all; nothing to gate.
        assert!(!min_max_rewrite_shape(1.0, 0));
    }

    #[test]
    fn min_max_rewrite_gate_uses_stable_stats_key() {
        // The pgrx integration test and any external consumer reading
        // `pg_accel_stats()` rely on this exact string. If you rename it,
        // update those consumers in the same commit.
        use super::super::RejectionReason;
        assert_eq!(
            RejectionReason::MinMaxRewriteNotASort.stats_key(),
            "min_max_rewrite_not_a_sort"
        );
    }
}
