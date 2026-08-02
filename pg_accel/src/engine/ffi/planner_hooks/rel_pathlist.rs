//! `set_rel_pathlist_hook` — observes base-relation opportunities and records
//! structural declines without injecting a host-staged `CustomPath`.
//!
//! Entry points:
//! - `pgaccel_set_rel_pathlist` — main hook fn (registered in `install()`)

use pgrx::pg_sys::{self, List, NodeTag, Path, PlannerInfo, RangeTblEntry, RelOptInfo};

use super::{PREV_SET_REL_PATHLIST_HOOK, unwrap_var};
use crate::engine::gucs;
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
/// Resident v2 does not inject base-scan, standalone sort/top-k, or scalar
/// function paths. This hook observes those opportunities, records stable
/// structural-decline reasons, and leaves PostgreSQL's native paths untouched.
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
    let _hook_finish =
        super::HookElapsedGuard::new("rel_pathlist", stats::PlannerHookStage::RelPathlist);

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

    // These cheap shape facts drive resident-only fail-closed observability.
    // SAFETY: root is a valid PlannerInfo pointer.
    let has_sort = unsafe { !(*root).sort_pathkeys.is_null() };
    let has_restrictions = !rel_ref.baserestrictinfo.is_null()
        // SAFETY: the non-null baserestrictinfo pointer is a planner-owned List.
        && unsafe { pg_sys::list_length(rel_ref.baserestrictinfo) } > 0;

    // A plain base scan with only direct Vars, Consts, or Params in its target
    // list cannot match any resident-v2 observer. Decline before creating a
    // tracing span or performing raster/function catalog lookups. Wrappers and
    // every callable expression deliberately fall through to exact observers.
    // SAFETY: parse is the live Query checked above and its target list is
    // planner-owned for this hook invocation.
    if !has_sort
        && !has_restrictions
        && rte_ref.rtekind == pg_sys::RTEKind::RTE_RELATION
        && matches!(
            rel_ref.reloptkind,
            pg_sys::RelOptKind::RELOPT_BASEREL | pg_sys::RelOptKind::RELOPT_OTHER_MEMBER_REL
        )
        && unsafe { query_has_only_plain_native_targets(&*parse) }
    {
        stats::record_planner_stage_fast_decline(
            stats::PlannerHookStage::RelPathlist,
            "rel_pathlist_plain_native",
        );
        return;
    }

    let _span =
        tracing::info_span!("planner.rel_pathlist", relid = u32::from(rte_ref.relid)).entered();

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
                    stats::PlannerHookStage::RelPathlist,
                    "rel_pathlist_no_resident_pipeline",
                    rel,
                );
            }
        }
    }
}

unsafe fn query_has_only_plain_native_targets(query: &pg_sys::Query) -> bool {
    let len = unsafe { pg_sys::list_length(query.targetList) };
    for index in 0..len {
        // SAFETY: index is bounded by the target-list length measured above.
        let target =
            unsafe { pg_sys::list_nth(query.targetList, index).cast::<pg_sys::TargetEntry>() };
        if target.is_null() {
            return false;
        }
        // SAFETY: the target-list cell is a live planner-owned TargetEntry.
        if unsafe { (*target).resjunk } {
            continue;
        }
        // SAFETY: the TargetEntry expression belongs to the same planner tree.
        let expression = unsafe { (*target).expr.cast::<pg_sys::Node>() };
        if expression.is_null() || !plain_native_target_tag(unsafe { (*expression).type_ }) {
            return false;
        }
    }
    true
}

#[must_use]
const fn plain_native_target_tag(tag: NodeTag) -> bool {
    matches!(tag, NodeTag::T_Var | NodeTag::T_Const | NodeTag::T_Param)
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
            stats::increment_planner_rejected(
                super::RejectionReason::SortIncrementalOpportunity.stats_key(),
                rejected_rows,
            );
            return;
        }
        SortShape::FullSort { .. } => {}
    }

    if num_pathkeys > 1 {
        stats::increment_planner_rejected(
            super::RejectionReason::SortMultiKeyNoGpuKernel.stats_key(),
            rejected_rows,
        );
        return;
    }

    if min_max_rewrite_shape(root_ref.limit_tuples, num_pathkeys) {
        stats::increment_planner_rejected(
            super::RejectionReason::MinMaxRewriteNotASort.stats_key(),
            rejected_rows,
        );
        return;
    }
    #[allow(clippy::cast_sign_loss)]
    let rows = rel_ref.rows.max(0.0) as usize;
    let reason = if standalone_sort_has_bounded_output(root_ref.limit_tuples, rows) {
        super::RejectionReason::SortStandaloneTopKNoGpuKernel
    } else {
        super::RejectionReason::SortHeapFullOutput
    };
    stats::increment_planner_rejected(reason.stats_key(), rejected_rows);
}

/// Whether a native standalone sort has a positive LIMIT smaller than its
/// estimated input. This is observability only: bounded top-k is still a
/// structural decline because no standalone sort kernel or executor ships.
#[must_use]
#[inline]
pub(super) fn standalone_sort_has_bounded_output(limit_tuples: f64, rows: usize) -> bool {
    if rows == 0 || !limit_tuples.is_finite() || limit_tuples <= 0.0 {
        return false;
    }
    #[allow(clippy::cast_precision_loss)]
    {
        limit_tuples.ceil() < rows as f64
    }
}

/// Whether a single-key LIMIT 1 shape matches PostgreSQL's MIN/MAX rewrite.
///
/// The observer retains a distinct reason because historical evidence uses
/// `min_max_rewrite_not_a_sort`. It never admits a GPU path: the standalone
/// sort kernel and executor are retired, including bounded top-k.
#[must_use]
#[inline]
pub(super) fn min_max_rewrite_shape(limit_tuples: f64, num_pathkeys: i32) -> bool {
    if !limit_tuples.is_finite() {
        return false;
    }
    // PG's `preprocess_limit` lowers `LIMIT 1` to `limit_tuples == 1.0`.
    // Allow the open interval (0, 2) to catch any fractional rounding
    // (e.g. `limit_tuples = 1.0` exactly) while excluding LIMIT 0 (no rows)
    // and LIMIT >= 2.
    (limit_tuples > 0.0) && (limit_tuples < 2.0) && (num_pathkeys == 1)
}

/// Classification of how `root->sort_pathkeys` relates to native child paths.
///
/// This is used only for decline observability. Numeric sort strategy tags and
/// descriptors remain in the private-data codec so old plans fail closed; no
/// production sort path is injected.
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

/// Pure observer classifier for [`SortShape`] given list lengths.
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

/// Return the longest `sort_pathkeys` prefix supplied by a native child path.
///
/// Uses PG's own [`pg_sys::pathkeys_count_contained_in`] — the same helper
/// `create_incremental_sort_path` uses in `src/backend/optimizer/util/pathkeys.c`
/// to decide the presorted-prefix length. A byte-wise list compare would
/// be wrong because PG canonicalises pathkeys and semantically equal keys
/// may be different `PathKey*` pointers.
///
/// Returns 0 when `pathlist` is empty or no path shares any prefix. The result
/// only refines structural-decline observability.
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
        NodeTag, SortShape, classify_sort_shape, min_max_rewrite_shape, plain_native_target_tag,
        standalone_sort_has_bounded_output,
    };

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

    // -- standalone_sort_has_bounded_output -------------------------------

    #[test]
    fn bounded_output_classifier_rejects_absent_or_invalid_limits() {
        assert!(!standalone_sort_has_bounded_output(-1.0, 1_000_000));
        assert!(!standalone_sort_has_bounded_output(0.0, 1_000_000));
        assert!(!standalone_sort_has_bounded_output(f64::NAN, 1_000_000));
        assert!(!standalone_sort_has_bounded_output(
            f64::INFINITY,
            1_000_000
        ));
    }

    #[test]
    fn bounded_output_classifier_rejects_limit_covering_all_rows() {
        assert!(!standalone_sort_has_bounded_output(1_000_000.0, 1_000_000));
        assert!(!standalone_sort_has_bounded_output(1_500_000.0, 1_000_000));
    }

    #[test]
    fn bounded_output_classifier_identifies_topk_without_admitting_it() {
        assert!(standalone_sort_has_bounded_output(1.0, 1_000_000));
        assert!(standalone_sort_has_bounded_output(128.0, 1_000_000));
    }

    // -- min_max_rewrite_shape --------------------------------------------
    //
    // PG's `preprocess_minmax_aggregates` rewrites `SELECT MIN(x) FROM t`
    // (and MAX) to a subplan shaped `ORDER BY x LIMIT 1`. The observer keeps
    // this historical reason distinct while every standalone sort remains
    // PostgreSQL-native.

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
        // LIMIT 2+ is not the MIN/MAX rewrite, but it remains a native
        // structural decline under `sort_standalone_topk_no_gpu_kernel`.
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

    #[test]
    fn plain_native_target_classifier_never_skips_callable_or_wrapped_nodes() {
        assert!(plain_native_target_tag(NodeTag::T_Var));
        assert!(plain_native_target_tag(NodeTag::T_Const));
        assert!(plain_native_target_tag(NodeTag::T_Param));

        assert!(!plain_native_target_tag(NodeTag::T_FuncExpr));
        assert!(!plain_native_target_tag(NodeTag::T_Aggref));
        assert!(!plain_native_target_tag(NodeTag::T_RelabelType));
        assert!(!plain_native_target_tag(NodeTag::T_CoerceViaIO));
    }
}
