//! `set_rel_pathlist_hook` — injects `CustomPath`s for base relations.
//!
//! Entry points:
//! - `pgaccel_set_rel_pathlist` — main hook fn (registered in `install()`)
//! - `try_inject_gpu_sort_path` — scan-level GPU sort injection helper

use pgrx::pg_sys::{
    self, CustomPath, List, NodeTag, Path, PlannerInfo, RangeTblEntry, RelOptInfo, lappend,
};

use super::super::custom_scan;
use super::{
    PREV_SET_REL_PATHLIST_HOOK, create_custom_path, extract_const_geom_vertex_count,
    find_accelerable_match, find_cheapest_path, find_cheapest_seqscan_cost,
    find_cheapest_seqscan_path, has_cheap_spatial_index_path, has_cheaper_spatial_index_path,
    unwrap_var,
};
use crate::engine::cost;
use crate::engine::executor::agg::AggOp;
use crate::engine::executor::sort::SortKeyDesc;
use crate::engine::expr_compiler::{CompiledExpr, TemplateKernel};
use crate::engine::gucs;
use crate::engine::registry;
use crate::engine::stats;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectGpuExprTemplateMatch {
    referenced_cols: usize,
    predicate_complexity: u64,
    uses_fp64: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GpuExprTemplateVar {
    col_idx: u32,
    typid: pg_sys::Oid,
    uses_fp64: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct GpuExprTemplateTerm {
    col_idx: u32,
    cmp_opcode: u16,
    const_val: f64,
    uses_fp64: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum H3LatLngQualDecline {
    UnsupportedShape,
    ScalarPredicateNoGpuPipeline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GpuExprTemplateAdmission {
    RowFilter,
    FusedFilterAgg,
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

/// Minimum standalone scan rows for H3 scalar kernels.
///
/// H3 target-list / aggregate fusion is handled elsewhere. The scan hook
/// should only expose a Custom Scan when there is enough bulk work to amortize
/// PostgreSQL tuple extraction and GPU launch outside a fused GpuScan pipeline.
pub(super) fn h3_standalone_scan_min_rows(limits: &cost::DeviceLimits) -> usize {
    limits.gpu_min_rows.saturating_mul(10).max(100_000)
}

/// Minimum standalone scan rows for raster kernels.
///
/// Raster work is per-pixel, but the scan hook only sees table rows. Require a
/// larger row batch here until the planner can cost tile dimensions directly.
pub(super) fn raster_standalone_scan_min_rows(limits: &cost::DeviceLimits) -> usize {
    limits.gpu_min_rows.saturating_mul(20).max(100_000)
}

fn gpuexpr_template_type_supported_for(
    oid: pg_sys::Oid,
    admission: GpuExprTemplateAdmission,
) -> bool {
    matches!(
        (admission, u32::from(oid)),
        (_, SORT_INT4OID | SORT_FLOAT4OID | SORT_FLOAT8OID)
            | (GpuExprTemplateAdmission::FusedFilterAgg, SORT_INT8OID)
    )
}

fn gpuexpr_template_type_uses_fp64(oid: pg_sys::Oid) -> bool {
    u32::from(oid) == SORT_FLOAT8OID
}

/// Return the 0-based column index for a template-safe GpuExpr input Var.
unsafe fn gpuexpr_template_var_col(
    node: *mut pg_sys::Node,
    admission: GpuExprTemplateAdmission,
) -> Option<GpuExprTemplateVar> {
    if node.is_null() {
        return None;
    }
    match unsafe { (*node).type_ } {
        NodeTag::T_Var => {
            let var = node.cast::<pg_sys::Var>();
            let vartype = unsafe { (*var).vartype };
            if !gpuexpr_template_type_supported_for(vartype, admission) {
                return None;
            }
            let attno = unsafe { (*var).varattno };
            if attno <= 0 {
                return None;
            }
            Some(GpuExprTemplateVar {
                col_idx: (attno - 1) as u32,
                typid: vartype,
                uses_fp64: gpuexpr_template_type_uses_fp64(vartype),
            })
        }
        NodeTag::T_RelabelType => {
            let relabel = node.cast::<pg_sys::RelabelType>();
            let result_type = unsafe { (*relabel).resulttype };
            if !gpuexpr_template_type_supported_for(result_type, admission) {
                return None;
            }
            unsafe { gpuexpr_template_var_col((*relabel).arg.cast::<pg_sys::Node>(), admission) }
        }
        _ => None,
    }
}

unsafe fn gpuexpr_template_const_value(
    node: *mut pg_sys::Node,
    admission: GpuExprTemplateAdmission,
    var_typid: pg_sys::Oid,
) -> Option<(f64, bool)> {
    if node.is_null() {
        return None;
    }
    match unsafe { (*node).type_ } {
        NodeTag::T_Const => {
            let cst = node.cast::<pg_sys::Const>();
            if unsafe { (*cst).constisnull } {
                return None;
            }
            let const_type = unsafe { (*cst).consttype };
            if !gpuexpr_template_type_supported_for(const_type, admission) {
                return None;
            }
            let datum = unsafe { (*cst).constvalue };
            let val = match u32::from(const_type) {
                SORT_INT4OID => f64::from(datum.value() as i32),
                SORT_INT8OID => super::int8_template_const_to_f64(datum.value() as i64)?,
                SORT_FLOAT4OID => f64::from(f32::from_bits(datum.value() as u32)),
                SORT_FLOAT8OID => f64::from_bits(datum.value() as u64),
                _ => return None,
            };
            if admission == GpuExprTemplateAdmission::FusedFilterAgg
                && u32::from(var_typid) == SORT_INT8OID
                && !super::template_f64_const_fits_exact_i64(val)
            {
                return None;
            }
            Some((val, gpuexpr_template_type_uses_fp64(const_type)))
        }
        NodeTag::T_RelabelType => {
            let relabel = node.cast::<pg_sys::RelabelType>();
            let result_type = unsafe { (*relabel).resulttype };
            if !gpuexpr_template_type_supported_for(result_type, admission) {
                return None;
            }
            let (val, inner_uses_fp64) = unsafe {
                gpuexpr_template_const_value(
                    (*relabel).arg.cast::<pg_sys::Node>(),
                    admission,
                    var_typid,
                )
            }?;
            Some((
                val,
                inner_uses_fp64 || gpuexpr_template_type_uses_fp64(result_type),
            ))
        }
        _ => None,
    }
}

/// Match exactly the template kernels that are production-admitted for direct
/// `GpuExpr`: `Var <cmp> Const`. `Const <cmp> Var` stays native until the
/// direct scan lane has the same live coverage as fused count.
unsafe fn gpuexpr_template_cmp_const_for(
    node: *mut pg_sys::Node,
    admission: GpuExprTemplateAdmission,
) -> Option<GpuExprTemplateTerm> {
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_OpExpr {
        return None;
    }

    let opexpr = node.cast::<pg_sys::OpExpr>();
    if u32::from(unsafe { (*opexpr).opresulttype }) != u32::from(pg_sys::BOOLOID) {
        return None;
    }

    let args = unsafe { (*opexpr).args };
    if args.is_null() || unsafe { pg_sys::list_length(args) } != 2 {
        return None;
    }

    let op_name_ptr = unsafe { pg_sys::get_opname((*opexpr).opno) };
    if op_name_ptr.is_null() {
        return None;
    }
    let op_name = unsafe { std::ffi::CStr::from_ptr(op_name_ptr) }
        .to_str()
        .ok()?;
    let cmp_opcode = crate::engine::expr_compiler::pg_cmp_op_to_opcode(op_name)?;

    let arg0 = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
    let arg1 = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };
    let term = unsafe { gpuexpr_template_var_col(arg0, admission) }?;
    let (const_val, const_uses_fp64) =
        unsafe { gpuexpr_template_const_value(arg1, admission, term.typid) }?;

    Some(GpuExprTemplateTerm {
        col_idx: term.col_idx,
        cmp_opcode,
        const_val,
        uses_fp64: term.uses_fp64 || const_uses_fp64,
    })
}

unsafe fn gpuexpr_template_terms_from_clause(
    node: *mut pg_sys::Node,
    admission: GpuExprTemplateAdmission,
) -> Option<Vec<GpuExprTemplateTerm>> {
    if node.is_null() {
        return None;
    }

    if let Some(term) = unsafe { gpuexpr_template_cmp_const_for(node, admission) } {
        return Some(vec![term]);
    }

    if unsafe { (*node).type_ } != NodeTag::T_BoolExpr {
        return None;
    }
    let boolexpr = node.cast::<pg_sys::BoolExpr>();
    if unsafe { (*boolexpr).boolop } != pg_sys::BoolExprType::AND_EXPR {
        return None;
    }
    let args = unsafe { (*boolexpr).args };
    if args.is_null() || unsafe { pg_sys::list_length(args) } != 2 {
        return None;
    }

    let lhs = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
    let rhs = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };
    Some(vec![
        unsafe { gpuexpr_template_cmp_const_for(lhs, admission) }?,
        unsafe { gpuexpr_template_cmp_const_for(rhs, admission) }?,
    ])
}

unsafe fn gpuexpr_template_terms(
    restrictinfo_list: *mut List,
    rows: usize,
    limits: &cost::DeviceLimits,
    admission: GpuExprTemplateAdmission,
) -> Option<Vec<GpuExprTemplateTerm>> {
    if restrictinfo_list.is_null() || rows < limits.gpu_expr_min_rows {
        return None;
    }

    let len = unsafe { pg_sys::list_length(restrictinfo_list) };
    if len == 0 || len > 2 {
        return None;
    }

    let mut terms = Vec::new();
    for i in 0..len {
        let ri = unsafe { pg_sys::list_nth(restrictinfo_list, i).cast::<pg_sys::RestrictInfo>() };
        if ri.is_null() {
            return None;
        }
        let clause = unsafe { (*ri).clause };
        if clause.is_null() {
            return None;
        }
        let clause_terms = unsafe { gpuexpr_template_terms_from_clause(clause.cast(), admission) }?;
        terms.extend(clause_terms);
    }

    if terms.is_empty() || terms.len() > 2 {
        return None;
    }

    let predicate_complexity = (terms.len() as u64)
        .saturating_mul(3)
        .saturating_add(u64::from(terms.len() > 1));
    let work = predicate_complexity.saturating_mul(rows as u64);
    if work < limits.expr_min_predicate_complexity_x_rows {
        return None;
    }

    Some(terms)
}

unsafe fn direct_gpuexpr_template_match(
    restrictinfo_list: *mut List,
    rows: usize,
    limits: &cost::DeviceLimits,
) -> Option<DirectGpuExprTemplateMatch> {
    let terms = unsafe {
        gpuexpr_template_terms(
            restrictinfo_list,
            rows,
            limits,
            GpuExprTemplateAdmission::RowFilter,
        )
    }?;
    let mut cols = Vec::new();
    let mut uses_fp64 = false;
    for term in &terms {
        if !cols.contains(&term.col_idx) {
            cols.push(term.col_idx);
        }
        uses_fp64 |= term.uses_fp64;
    }

    let predicate_complexity = (terms.len() as u64)
        .saturating_mul(3)
        .saturating_add(u64::from(terms.len() > 1));

    Some(DirectGpuExprTemplateMatch {
        referenced_cols: cols.len(),
        predicate_complexity,
        uses_fp64,
    })
}

pub(super) unsafe fn direct_gpuexpr_template_fused_scan_expr(
    restrictinfo_list: *mut List,
    rows: usize,
    limits: &cost::DeviceLimits,
) -> Option<CompiledExpr> {
    let terms = unsafe {
        gpuexpr_template_terms(
            restrictinfo_list,
            rows,
            limits,
            GpuExprTemplateAdmission::FusedFilterAgg,
        )
    }?;
    match terms.as_slice() {
        [term] => Some(CompiledExpr::Template(TemplateKernel::CmpConst {
            col_idx: term.col_idx,
            cmp_opcode: term.cmp_opcode,
            const_val: term.const_val,
        })),
        [lhs, rhs] if lhs.col_idx != rhs.col_idx => {
            Some(CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                col1_idx: lhs.col_idx,
                cmp1_opcode: lhs.cmp_opcode,
                const1_val: lhs.const_val,
                col2_idx: rhs.col_idx,
                cmp2_opcode: rhs.cmp_opcode,
                const2_val: rhs.const_val,
            }))
        }
        _ => None,
    }
}

pub(super) unsafe fn direct_gpuexpr_template_count_scan_expr(
    restrictinfo_list: *mut List,
    rows: usize,
    limits: &cost::DeviceLimits,
) -> Option<CompiledExpr> {
    unsafe { direct_gpuexpr_template_fused_scan_expr(restrictinfo_list, rows, limits) }
}

unsafe fn query_is_simple_count_star(root: *mut PlannerInfo) -> bool {
    if root.is_null() {
        return false;
    }
    let parse = unsafe { (*root).parse };
    if parse.is_null() {
        return false;
    }
    let query = unsafe { &*parse };
    if !query.hasAggs
        || (!query.groupClause.is_null() && unsafe { pg_sys::list_length(query.groupClause) } > 0)
        || !query.havingQual.is_null()
        || query.targetList.is_null()
        || unsafe { pg_sys::list_length(query.targetList) } != 1
    {
        return false;
    }

    let tle = unsafe { pg_sys::list_nth(query.targetList, 0).cast::<pg_sys::TargetEntry>() };
    if tle.is_null() {
        return false;
    }
    let expr = unsafe { (*tle).expr };
    if expr.is_null() || unsafe { (*expr.cast::<pg_sys::Node>()).type_ } != NodeTag::T_Aggref {
        return false;
    }
    let aggref = expr.cast::<pg_sys::Aggref>();
    let aggref_ref = unsafe { &*aggref };
    if !aggref_ref.aggstar
        || !aggref_ref.aggdistinct.is_null()
        || !aggref_ref.aggorder.is_null()
        || !aggref_ref.aggfilter.is_null()
    {
        return false;
    }

    matches!(
        unsafe { super::agg_common::classify_aggref(aggref) },
        Some((AggOp::Count, _))
    )
}

unsafe fn direct_gpuexpr_count_has_parallel_owner(
    root: *mut PlannerInfo,
    rel: &RelOptInfo,
) -> bool {
    rel.consider_parallel
        && unsafe { pg_sys::max_parallel_workers_per_gather } > 0
        && unsafe { query_is_simple_count_star(root) }
}

fn h3_scan_function_is_filter_safe(_name: &str) -> bool {
    false
}

unsafe fn h3_point_var_node(node: *mut pg_sys::Node) -> bool {
    let var = unsafe { unwrap_var(node) };
    if var.is_null() {
        return false;
    }
    let vartype = u32::from(unsafe { (*var).vartype });
    let attno = unsafe { (*var).varattno };
    vartype == pg_sys::POINTOID.to_u32() && attno > 0
}

unsafe fn h3_resolution_const_node(node: *mut pg_sys::Node) -> bool {
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_Const {
        return false;
    }
    let cst = node.cast::<pg_sys::Const>();
    if unsafe { (*cst).constisnull } {
        return false;
    }
    let datum = unsafe { (*cst).constvalue };
    let resolution = match u32::from(unsafe { (*cst).consttype }) {
        21 => i32::from(datum.value() as i16),
        23 => datum.value() as i32,
        20 => (datum.value() as i64).try_into().unwrap_or(i32::MAX),
        _ => return false,
    };
    (0..=15).contains(&resolution)
}

unsafe fn h3_latlng_args_supported(args: *mut List) -> bool {
    if args.is_null() || unsafe { pg_sys::list_length(args) } != 2 {
        return false;
    }
    let arg0 = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
    let arg1 = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };
    (unsafe { h3_point_var_node(arg0) }) && (unsafe { h3_resolution_const_node(arg1) })
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
    let len = unsafe { pg_sys::list_length(args) };
    let mut out = None;
    for i in 0..len {
        let child = unsafe { pg_sys::list_nth(args, i).cast::<pg_sys::Node>() };
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
    match unsafe { (*node).type_ } {
        NodeTag::T_FuncExpr => {
            let funcexpr = node.cast::<pg_sys::FuncExpr>();
            let fn_name = unsafe { function_name_for_oid((*funcexpr).funcid) };
            if fn_name.as_deref() == Some("h3_latlng_to_cell") {
                return if unsafe { h3_latlng_args_supported((*funcexpr).args) } {
                    Some(H3LatLngQualDecline::ScalarPredicateNoGpuPipeline)
                } else {
                    Some(H3LatLngQualDecline::UnsupportedShape)
                };
            }
            unsafe { h3_latlng_qual_decline_list((*funcexpr).args) }
        }
        NodeTag::T_OpExpr => {
            let opexpr = node.cast::<pg_sys::OpExpr>();
            unsafe { h3_latlng_qual_decline_list((*opexpr).args) }
        }
        NodeTag::T_BoolExpr => {
            let bool_expr = node.cast::<pg_sys::BoolExpr>();
            unsafe { h3_latlng_qual_decline_list((*bool_expr).args) }
        }
        NodeTag::T_BooleanTest => {
            let bool_test = node.cast::<pg_sys::BooleanTest>();
            unsafe { h3_latlng_qual_decline_node((*bool_test).arg.cast::<pg_sys::Node>()) }
        }
        NodeTag::T_NullTest => {
            let null_test = node.cast::<pg_sys::NullTest>();
            unsafe { h3_latlng_qual_decline_node((*null_test).arg.cast::<pg_sys::Node>()) }
        }
        NodeTag::T_RelabelType => {
            let relabel = node.cast::<pg_sys::RelabelType>();
            unsafe { h3_latlng_qual_decline_node((*relabel).arg.cast::<pg_sys::Node>()) }
        }
        NodeTag::T_CoerceViaIO => {
            let coercion = node.cast::<pg_sys::CoerceViaIO>();
            unsafe { h3_latlng_qual_decline_node((*coercion).arg.cast::<pg_sys::Node>()) }
        }
        NodeTag::T_ScalarArrayOpExpr => {
            let scalar_array = node.cast::<pg_sys::ScalarArrayOpExpr>();
            unsafe { h3_latlng_qual_decline_list((*scalar_array).args) }
        }
        NodeTag::T_CaseExpr => {
            let case_expr = node.cast::<pg_sys::CaseExpr>();
            let mut out =
                unsafe { h3_latlng_qual_decline_node((*case_expr).arg.cast::<pg_sys::Node>()) };
            out = merge_h3_latlng_decline(out, unsafe {
                h3_latlng_qual_decline_list((*case_expr).args)
            });
            merge_h3_latlng_decline(out, unsafe {
                h3_latlng_qual_decline_node((*case_expr).defresult.cast::<pg_sys::Node>())
            })
        }
        NodeTag::T_CaseWhen => {
            let case_when = node.cast::<pg_sys::CaseWhen>();
            let out =
                unsafe { h3_latlng_qual_decline_node((*case_when).expr.cast::<pg_sys::Node>()) };
            merge_h3_latlng_decline(out, unsafe {
                h3_latlng_qual_decline_node((*case_when).result.cast::<pg_sys::Node>())
            })
        }
        NodeTag::T_CoalesceExpr => {
            let coalesce = node.cast::<pg_sys::CoalesceExpr>();
            unsafe { h3_latlng_qual_decline_list((*coalesce).args) }
        }
        _ => None,
    }
}

unsafe fn h3_latlng_qual_decline(restrictinfo_list: *mut List) -> Option<H3LatLngQualDecline> {
    if restrictinfo_list.is_null() {
        return None;
    }
    let len = unsafe { pg_sys::list_length(restrictinfo_list) };
    let mut out = None;
    for i in 0..len {
        let ri = unsafe { pg_sys::list_nth(restrictinfo_list, i).cast::<pg_sys::RestrictInfo>() };
        if ri.is_null() {
            continue;
        }
        let clause = unsafe { (*ri).clause };
        out = merge_h3_latlng_decline(out, unsafe { h3_latlng_qual_decline_node(clause.cast()) });
        if out == Some(H3LatLngQualDecline::UnsupportedShape) {
            return out;
        }
    }
    out
}

fn raster_scan_function_is_compute_heavy(name: &str) -> bool {
    matches!(
        name,
        "st_mapalgebra"
            | "st_clip"
            | "st_reclass"
            | "st_resample"
            | "st_slope"
            | "st_aspect"
            | "st_hillshade"
            | "st_summarystats"
    )
}

/// Strategy-specific standalone scan gate for extension functions.
///
/// Cheap scalar H3/raster functions are only acceptable once they are fused
/// into a real GpuScan pipeline. Normal rel_pathlist exposure is reserved for
/// batched compute-heavy kernels. Spatial predicates are handled by the
/// vertex/work-product gate below because their cost depends on polygon shape.
pub(super) fn extension_scan_gate(
    strategy: registry::AccelStrategy,
    fn_name: Option<&str>,
    rows: usize,
    limits: &cost::DeviceLimits,
) -> Result<(), &'static str> {
    match strategy {
        registry::AccelStrategy::GpuH3 => {
            if rows < h3_standalone_scan_min_rows(limits) {
                return Err("h3_rows_below_standalone_min");
            }
            let Some(name) = fn_name else {
                return Err("h3_unknown_function");
            };
            if h3_scan_function_is_filter_safe(name) {
                Ok(())
            } else {
                Err("h3_function_not_compute_heavy")
            }
        }
        registry::AccelStrategy::GpuRaster => {
            if rows < raster_standalone_scan_min_rows(limits) {
                return Err("raster_rows_below_standalone_min");
            }
            let Some(name) = fn_name else {
                return Err("raster_unknown_function");
            };
            if raster_scan_function_is_compute_heavy(name) {
                Ok(())
            } else {
                Err("raster_function_not_compute_heavy")
            }
        }
        _ => Ok(()),
    }
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
    if !unsafe { super::super::syscache::function_is_extension_member(fn_oid, "postgis") } {
        return None;
    }
    let (_, name) = unsafe { super::super::syscache::function_schema_and_name(fn_oid) }?;
    Some(name)
}

unsafe fn oid_is_postgis_spatial_scalar_type(type_oid: pg_sys::Oid) -> bool {
    if type_oid == pg_sys::InvalidOid {
        return false;
    }

    let base_type = unsafe { pg_sys::getBaseType(type_oid) };
    if base_type == pg_sys::InvalidOid {
        return false;
    }
    if !unsafe { super::super::syscache::type_is_extension_member(base_type, "postgis") } {
        return false;
    }
    unsafe { super::super::syscache::type_name(base_type) }
        .as_deref()
        .is_some_and(|name| matches!(name, "geometry" | "geography"))
}

unsafe fn oid_is_postgis_spatial_or_array_type(type_oid: pg_sys::Oid) -> bool {
    if unsafe { oid_is_postgis_spatial_scalar_type(type_oid) } {
        return true;
    }

    let base_type = unsafe { pg_sys::getBaseType(type_oid) };
    if base_type == pg_sys::InvalidOid {
        return false;
    }
    let element_type = unsafe { pg_sys::get_element_type(base_type) };
    element_type != pg_sys::InvalidOid
        && unsafe { oid_is_postgis_spatial_scalar_type(element_type) }
}

unsafe fn expr_list_has_spatial_prefix(args: *mut List, required: usize) -> bool {
    if args.is_null() || unsafe { pg_sys::list_length(args) } < required as i32 {
        return false;
    }

    for i in 0..required {
        let child = unsafe { pg_sys::list_nth(args, i as i32).cast::<pg_sys::Node>() };
        if child.is_null() {
            return false;
        }
        let arg_type = unsafe { pg_sys::exprType(child) };
        if !unsafe { oid_is_postgis_spatial_scalar_type(arg_type) } {
            return false;
        }
    }
    true
}

unsafe fn expr_list_contains_spatial_arg(args: *mut List) -> bool {
    if args.is_null() {
        return false;
    }

    let len = unsafe { pg_sys::list_length(args) };
    for i in 0..len {
        let child = unsafe { pg_sys::list_nth(args, i).cast::<pg_sys::Node>() };
        if child.is_null() {
            continue;
        }
        let arg_type = unsafe { pg_sys::exprType(child) };
        if unsafe { oid_is_postgis_spatial_or_array_type(arg_type) } {
            return true;
        }
    }
    false
}

unsafe fn function_matches_postgis_spatial_prefix(
    fn_oid: pg_sys::Oid,
    args: *mut List,
    name_predicate: impl FnOnce(&str) -> bool,
) -> bool {
    let Some(name) = (unsafe { postgis_function_name_for_oid(fn_oid) }) else {
        return false;
    };
    name_predicate(&name) && unsafe { expr_list_has_spatial_prefix(args, 2) }
}

unsafe fn function_matches_postgis_spatial_arg(
    fn_oid: pg_sys::Oid,
    args: *mut List,
    name_predicate: impl FnOnce(&str) -> bool,
) -> bool {
    let Some(name) = (unsafe { postgis_function_name_for_oid(fn_oid) }) else {
        return false;
    };
    name_predicate(&name) && unsafe { expr_list_contains_spatial_arg(args) }
}

#[must_use]
fn postgis_spatial_predicate_name(name: &str) -> bool {
    matches!(
        name,
        "st_intersects"
            | "st_contains"
            | "st_within"
            | "st_dwithin"
            | "st_disjoint"
            | "st_equals"
            | "st_touches"
            | "st_crosses"
            | "st_overlaps"
            | "st_covers"
            | "st_coveredby"
    )
}

#[must_use]
fn postgis_distance_function_name(name: &str) -> bool {
    matches!(name, "st_distance")
}

#[must_use]
fn postgis_geometry_constructor_function_name(name: &str) -> bool {
    matches!(name, "st_buffer" | "st_union" | "st_intersection")
}

unsafe fn funcexpr_is_postgis_intersects(funcexpr: *mut pg_sys::FuncExpr) -> bool {
    let funcid = unsafe { (*funcexpr).funcid };
    let args = unsafe { (*funcexpr).args };
    unsafe { function_matches_postgis_spatial_prefix(funcid, args, |name| name == "st_intersects") }
}

unsafe fn opexpr_is_postgis_intersects(opexpr: *mut pg_sys::OpExpr) -> bool {
    let mut opfuncid = unsafe { (*opexpr).opfuncid };
    if opfuncid == pg_sys::InvalidOid {
        unsafe { pg_sys::set_opfuncid(opexpr) };
        opfuncid = unsafe { (*opexpr).opfuncid };
    }
    let args = unsafe { (*opexpr).args };
    unsafe {
        function_matches_postgis_spatial_prefix(opfuncid, args, |name| name == "st_intersects")
    }
}

unsafe fn restrictinfo_contains_wrapped_postgis_intersects(restrictinfo_list: *mut List) -> bool {
    if restrictinfo_list.is_null() {
        return false;
    }

    let len = unsafe { pg_sys::list_length(restrictinfo_list) };
    for i in 0..len {
        let ri = unsafe { pg_sys::list_nth(restrictinfo_list, i).cast::<pg_sys::RestrictInfo>() };
        if ri.is_null() {
            continue;
        }
        let clause = unsafe { (*ri).clause };
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

    let tag = unsafe { (*node).type_ };
    #[allow(clippy::cast_ptr_alignment)]
    match tag {
        NodeTag::T_FuncExpr => {
            let funcexpr = node.cast::<pg_sys::FuncExpr>();
            if unsafe { funcexpr_is_postgis_intersects(funcexpr) } {
                return true;
            }
            unsafe { args_contain_wrapped_postgis_intersects((*funcexpr).args) }
        }
        NodeTag::T_OpExpr => {
            let opexpr = node.cast::<pg_sys::OpExpr>();
            if unsafe { opexpr_is_postgis_intersects(opexpr) } {
                return true;
            }
            unsafe { args_contain_wrapped_postgis_intersects((*opexpr).args) }
        }
        NodeTag::T_BoolExpr => {
            let bool_expr = node.cast::<pg_sys::BoolExpr>();
            unsafe { args_contain_wrapped_postgis_intersects((*bool_expr).args) }
        }
        NodeTag::T_NullTest => {
            let null_test = node.cast::<pg_sys::NullTest>();
            unsafe { node_contains_wrapped_postgis_intersects((*null_test).arg.cast()) }
        }
        NodeTag::T_BooleanTest => {
            let bool_test = node.cast::<pg_sys::BooleanTest>();
            unsafe { node_contains_wrapped_postgis_intersects((*bool_test).arg.cast()) }
        }
        NodeTag::T_RelabelType => {
            let relabel = node.cast::<pg_sys::RelabelType>();
            unsafe { node_contains_wrapped_postgis_intersects((*relabel).arg.cast()) }
        }
        NodeTag::T_CoerceViaIO => {
            let coercion = node.cast::<pg_sys::CoerceViaIO>();
            unsafe { node_contains_wrapped_postgis_intersects((*coercion).arg.cast()) }
        }
        NodeTag::T_CoerceToDomain => {
            let coercion = node.cast::<pg_sys::CoerceToDomain>();
            unsafe { node_contains_wrapped_postgis_intersects((*coercion).arg.cast()) }
        }
        NodeTag::T_CoalesceExpr => {
            let coalesce = node.cast::<pg_sys::CoalesceExpr>();
            unsafe { args_contain_wrapped_postgis_intersects((*coalesce).args) }
        }
        NodeTag::T_ScalarArrayOpExpr => {
            let scalar_array = node.cast::<pg_sys::ScalarArrayOpExpr>();
            unsafe { args_contain_wrapped_postgis_intersects((*scalar_array).args) }
        }
        NodeTag::T_CaseExpr => {
            let case_expr = node.cast::<pg_sys::CaseExpr>();
            unsafe {
                node_contains_wrapped_postgis_intersects((*case_expr).arg.cast())
                    || args_contain_wrapped_postgis_intersects((*case_expr).args)
                    || node_contains_wrapped_postgis_intersects((*case_expr).defresult.cast())
            }
        }
        NodeTag::T_CaseWhen => {
            let case_when = node.cast::<pg_sys::CaseWhen>();
            unsafe {
                node_contains_wrapped_postgis_intersects((*case_when).expr.cast())
                    || node_contains_wrapped_postgis_intersects((*case_when).result.cast())
            }
        }
        NodeTag::T_ArrayExpr => {
            let array = node.cast::<pg_sys::ArrayExpr>();
            unsafe { args_contain_wrapped_postgis_intersects((*array).elements) }
        }
        NodeTag::T_MinMaxExpr => {
            let minmax = node.cast::<pg_sys::MinMaxExpr>();
            unsafe { args_contain_wrapped_postgis_intersects((*minmax).args) }
        }
        _ => false,
    }
}

unsafe fn args_contain_wrapped_postgis_intersects(args: *mut List) -> bool {
    if args.is_null() {
        return false;
    }

    let len = unsafe { pg_sys::list_length(args) };
    for i in 0..len {
        let child = unsafe { pg_sys::list_nth(args, i).cast::<pg_sys::Node>() };
        if unsafe { node_contains_wrapped_postgis_intersects(child) } {
            return true;
        }
    }
    false
}

/// Detect a PostGIS spatial predicate that is intentionally not registered in
/// the normal adapter because the current GPU coverage still needs CPU
/// recheck/uncertain-row handling for some geometry pairs.
unsafe fn restrictinfo_contains_unregistered_spatial_predicate(
    restrictinfo_list: *mut List,
) -> bool {
    if restrictinfo_list.is_null() {
        return false;
    }

    // SAFETY: restrictinfo_list is a valid List from the planner.
    let len = unsafe { pg_sys::list_length(restrictinfo_list) };
    for i in 0..len {
        // SAFETY: i is in [0, len), list_nth returns a RestrictInfo pointer.
        let ri = unsafe { pg_sys::list_nth(restrictinfo_list, i).cast::<pg_sys::RestrictInfo>() };
        if ri.is_null() {
            continue;
        }
        // SAFETY: ri is a planner-owned RestrictInfo.
        let clause = unsafe { (*ri).clause };
        if unsafe { node_contains_unregistered_spatial_predicate(clause.cast()) } {
            return true;
        }
    }
    false
}

unsafe fn node_contains_unregistered_spatial_predicate(node: *mut pg_sys::Node) -> bool {
    if node.is_null() {
        return false;
    }

    // SAFETY: node is a valid planner expression node.
    let tag = unsafe { (*node).type_ };
    #[allow(clippy::cast_ptr_alignment)]
    match tag {
        NodeTag::T_FuncExpr => {
            let funcexpr = node.cast::<pg_sys::FuncExpr>();
            // SAFETY: tag checked above.
            let result_type = unsafe { (*funcexpr).funcresulttype };
            // SAFETY: tag checked above.
            let funcid = unsafe { (*funcexpr).funcid };
            if result_type == pg_sys::BOOLOID
                && unsafe {
                    function_matches_postgis_spatial_prefix(
                        funcid,
                        (*funcexpr).args,
                        postgis_spatial_predicate_name,
                    )
                }
            {
                return true;
            }
            // SAFETY: tag checked above.
            unsafe { args_contain_unregistered_spatial_predicate((*funcexpr).args) }
        }
        NodeTag::T_OpExpr => {
            let opexpr = node.cast::<pg_sys::OpExpr>();
            // SAFETY: tag checked above.
            let mut funcid = unsafe { (*opexpr).opfuncid };
            if funcid == pg_sys::InvalidOid {
                // SAFETY: opexpr is a valid OpExpr pointer.
                unsafe { pg_sys::set_opfuncid(opexpr) };
                // SAFETY: tag checked above.
                funcid = unsafe { (*opexpr).opfuncid };
            }
            // SAFETY: tag checked above.
            let result_type = unsafe { (*opexpr).opresulttype };
            if result_type == pg_sys::BOOLOID
                && unsafe {
                    function_matches_postgis_spatial_prefix(
                        funcid,
                        (*opexpr).args,
                        postgis_spatial_predicate_name,
                    )
                }
            {
                return true;
            }
            // SAFETY: tag checked above.
            unsafe { args_contain_unregistered_spatial_predicate((*opexpr).args) }
        }
        NodeTag::T_BoolExpr => {
            let bool_expr = node.cast::<pg_sys::BoolExpr>();
            // SAFETY: tag checked above.
            unsafe { args_contain_unregistered_spatial_predicate((*bool_expr).args) }
        }
        NodeTag::T_NullTest => {
            let null_test = node.cast::<pg_sys::NullTest>();
            unsafe { node_contains_unregistered_spatial_predicate((*null_test).arg.cast()) }
        }
        NodeTag::T_BooleanTest => {
            let bool_test = node.cast::<pg_sys::BooleanTest>();
            unsafe { node_contains_unregistered_spatial_predicate((*bool_test).arg.cast()) }
        }
        NodeTag::T_RelabelType => {
            let relabel = node.cast::<pg_sys::RelabelType>();
            unsafe { node_contains_unregistered_spatial_predicate((*relabel).arg.cast()) }
        }
        NodeTag::T_CoerceViaIO => {
            let coercion = node.cast::<pg_sys::CoerceViaIO>();
            unsafe { node_contains_unregistered_spatial_predicate((*coercion).arg.cast()) }
        }
        NodeTag::T_CoerceToDomain => {
            let coercion = node.cast::<pg_sys::CoerceToDomain>();
            unsafe { node_contains_unregistered_spatial_predicate((*coercion).arg.cast()) }
        }
        NodeTag::T_CoalesceExpr => {
            let coalesce = node.cast::<pg_sys::CoalesceExpr>();
            unsafe { args_contain_unregistered_spatial_predicate((*coalesce).args) }
        }
        NodeTag::T_ScalarArrayOpExpr => {
            let scalar_array = node.cast::<pg_sys::ScalarArrayOpExpr>();
            unsafe { args_contain_unregistered_spatial_predicate((*scalar_array).args) }
        }
        NodeTag::T_CaseExpr => {
            let case_expr = node.cast::<pg_sys::CaseExpr>();
            unsafe {
                node_contains_unregistered_spatial_predicate((*case_expr).arg.cast())
                    || args_contain_unregistered_spatial_predicate((*case_expr).args)
                    || node_contains_unregistered_spatial_predicate((*case_expr).defresult.cast())
            }
        }
        NodeTag::T_CaseWhen => {
            let case_when = node.cast::<pg_sys::CaseWhen>();
            unsafe {
                node_contains_unregistered_spatial_predicate((*case_when).expr.cast())
                    || node_contains_unregistered_spatial_predicate((*case_when).result.cast())
            }
        }
        NodeTag::T_ArrayExpr => {
            let array = node.cast::<pg_sys::ArrayExpr>();
            unsafe { args_contain_unregistered_spatial_predicate((*array).elements) }
        }
        NodeTag::T_MinMaxExpr => {
            let minmax = node.cast::<pg_sys::MinMaxExpr>();
            unsafe { args_contain_unregistered_spatial_predicate((*minmax).args) }
        }
        _ => false,
    }
}

unsafe fn args_contain_unregistered_spatial_predicate(args: *mut List) -> bool {
    if args.is_null() {
        return false;
    }

    // SAFETY: args is a valid planner expression List.
    let len = unsafe { pg_sys::list_length(args) };
    for i in 0..len {
        // SAFETY: i is in [0, len).
        let child = unsafe { pg_sys::list_nth(args, i).cast::<pg_sys::Node>() };
        if unsafe { node_contains_unregistered_spatial_predicate(child) } {
            return true;
        }
    }
    false
}

unsafe fn restrictinfo_contains_unregistered_postgis_geometry_constructor(
    restrictinfo_list: *mut List,
) -> bool {
    if restrictinfo_list.is_null() {
        return false;
    }

    // SAFETY: restrictinfo_list is a valid List from the planner.
    let len = unsafe { pg_sys::list_length(restrictinfo_list) };
    for i in 0..len {
        // SAFETY: i is in [0, len), list_nth returns a RestrictInfo pointer.
        let ri = unsafe { pg_sys::list_nth(restrictinfo_list, i).cast::<pg_sys::RestrictInfo>() };
        if ri.is_null() {
            continue;
        }
        // SAFETY: ri is a planner-owned RestrictInfo.
        let clause = unsafe { (*ri).clause };
        if unsafe { node_contains_unregistered_postgis_geometry_constructor(clause.cast()) } {
            return true;
        }
    }
    false
}

unsafe fn node_contains_unregistered_postgis_geometry_constructor(node: *mut pg_sys::Node) -> bool {
    if node.is_null() {
        return false;
    }

    // SAFETY: node is a valid planner expression node.
    let tag = unsafe { (*node).type_ };
    #[allow(clippy::cast_ptr_alignment)]
    match tag {
        NodeTag::T_FuncExpr => {
            let funcexpr = node.cast::<pg_sys::FuncExpr>();
            // SAFETY: tag checked above.
            let funcid = unsafe { (*funcexpr).funcid };
            if unsafe {
                function_matches_postgis_spatial_arg(
                    funcid,
                    (*funcexpr).args,
                    postgis_geometry_constructor_function_name,
                )
            } {
                return true;
            }
            // SAFETY: tag checked above.
            unsafe { args_contain_unregistered_postgis_geometry_constructor((*funcexpr).args) }
        }
        NodeTag::T_OpExpr => {
            let opexpr = node.cast::<pg_sys::OpExpr>();
            // SAFETY: tag checked above.
            unsafe { args_contain_unregistered_postgis_geometry_constructor((*opexpr).args) }
        }
        NodeTag::T_BoolExpr => {
            let bool_expr = node.cast::<pg_sys::BoolExpr>();
            // SAFETY: tag checked above.
            unsafe { args_contain_unregistered_postgis_geometry_constructor((*bool_expr).args) }
        }
        NodeTag::T_NullTest => {
            let null_test = node.cast::<pg_sys::NullTest>();
            unsafe {
                node_contains_unregistered_postgis_geometry_constructor((*null_test).arg.cast())
            }
        }
        NodeTag::T_BooleanTest => {
            let bool_test = node.cast::<pg_sys::BooleanTest>();
            unsafe {
                node_contains_unregistered_postgis_geometry_constructor((*bool_test).arg.cast())
            }
        }
        NodeTag::T_RelabelType => {
            let relabel = node.cast::<pg_sys::RelabelType>();
            unsafe {
                node_contains_unregistered_postgis_geometry_constructor((*relabel).arg.cast())
            }
        }
        NodeTag::T_CoerceViaIO => {
            let coercion = node.cast::<pg_sys::CoerceViaIO>();
            unsafe {
                node_contains_unregistered_postgis_geometry_constructor((*coercion).arg.cast())
            }
        }
        NodeTag::T_CoerceToDomain => {
            let coercion = node.cast::<pg_sys::CoerceToDomain>();
            unsafe {
                node_contains_unregistered_postgis_geometry_constructor((*coercion).arg.cast())
            }
        }
        NodeTag::T_CoalesceExpr => {
            let coalesce = node.cast::<pg_sys::CoalesceExpr>();
            unsafe { args_contain_unregistered_postgis_geometry_constructor((*coalesce).args) }
        }
        NodeTag::T_ScalarArrayOpExpr => {
            let scalar_array = node.cast::<pg_sys::ScalarArrayOpExpr>();
            unsafe { args_contain_unregistered_postgis_geometry_constructor((*scalar_array).args) }
        }
        NodeTag::T_CaseExpr => {
            let case_expr = node.cast::<pg_sys::CaseExpr>();
            unsafe {
                node_contains_unregistered_postgis_geometry_constructor((*case_expr).arg.cast())
                    || args_contain_unregistered_postgis_geometry_constructor((*case_expr).args)
                    || node_contains_unregistered_postgis_geometry_constructor(
                        (*case_expr).defresult.cast(),
                    )
            }
        }
        NodeTag::T_CaseWhen => {
            let case_when = node.cast::<pg_sys::CaseWhen>();
            unsafe {
                node_contains_unregistered_postgis_geometry_constructor((*case_when).expr.cast())
                    || node_contains_unregistered_postgis_geometry_constructor(
                        (*case_when).result.cast(),
                    )
            }
        }
        NodeTag::T_ArrayExpr => {
            let array = node.cast::<pg_sys::ArrayExpr>();
            unsafe { args_contain_unregistered_postgis_geometry_constructor((*array).elements) }
        }
        NodeTag::T_MinMaxExpr => {
            let minmax = node.cast::<pg_sys::MinMaxExpr>();
            unsafe { args_contain_unregistered_postgis_geometry_constructor((*minmax).args) }
        }
        _ => false,
    }
}

unsafe fn args_contain_unregistered_postgis_geometry_constructor(args: *mut List) -> bool {
    if args.is_null() {
        return false;
    }

    // SAFETY: args is a valid planner expression List.
    let len = unsafe { pg_sys::list_length(args) };
    for i in 0..len {
        // SAFETY: i is in [0, len).
        let child = unsafe { pg_sys::list_nth(args, i).cast::<pg_sys::Node>() };
        if unsafe { node_contains_unregistered_postgis_geometry_constructor(child) } {
            return true;
        }
    }
    false
}

unsafe fn restrictinfo_contains_unregistered_postgis_distance_function(
    restrictinfo_list: *mut List,
) -> bool {
    if restrictinfo_list.is_null() {
        return false;
    }

    // SAFETY: restrictinfo_list is a valid List from the planner.
    let len = unsafe { pg_sys::list_length(restrictinfo_list) };
    for i in 0..len {
        // SAFETY: i is in [0, len), list_nth returns a RestrictInfo pointer.
        let ri = unsafe { pg_sys::list_nth(restrictinfo_list, i).cast::<pg_sys::RestrictInfo>() };
        if ri.is_null() {
            continue;
        }
        // SAFETY: ri is a planner-owned RestrictInfo.
        let clause = unsafe { (*ri).clause };
        if unsafe { node_contains_unregistered_postgis_distance_function(clause.cast()) } {
            return true;
        }
    }
    false
}

unsafe fn node_contains_unregistered_postgis_distance_function(node: *mut pg_sys::Node) -> bool {
    if node.is_null() {
        return false;
    }

    // SAFETY: node is a valid planner expression node.
    let tag = unsafe { (*node).type_ };
    #[allow(clippy::cast_ptr_alignment)]
    match tag {
        NodeTag::T_FuncExpr => {
            let funcexpr = node.cast::<pg_sys::FuncExpr>();
            // SAFETY: tag checked above.
            let funcid = unsafe { (*funcexpr).funcid };
            if unsafe {
                function_matches_postgis_spatial_prefix(
                    funcid,
                    (*funcexpr).args,
                    postgis_distance_function_name,
                )
            } {
                return true;
            }
            // SAFETY: tag checked above.
            unsafe { args_contain_unregistered_postgis_distance_function((*funcexpr).args) }
        }
        NodeTag::T_OpExpr => {
            let opexpr = node.cast::<pg_sys::OpExpr>();
            // SAFETY: tag checked above.
            unsafe { args_contain_unregistered_postgis_distance_function((*opexpr).args) }
        }
        NodeTag::T_BoolExpr => {
            let bool_expr = node.cast::<pg_sys::BoolExpr>();
            // SAFETY: tag checked above.
            unsafe { args_contain_unregistered_postgis_distance_function((*bool_expr).args) }
        }
        NodeTag::T_NullTest => {
            let null_test = node.cast::<pg_sys::NullTest>();
            unsafe { node_contains_unregistered_postgis_distance_function((*null_test).arg.cast()) }
        }
        NodeTag::T_BooleanTest => {
            let bool_test = node.cast::<pg_sys::BooleanTest>();
            unsafe { node_contains_unregistered_postgis_distance_function((*bool_test).arg.cast()) }
        }
        NodeTag::T_RelabelType => {
            let relabel = node.cast::<pg_sys::RelabelType>();
            unsafe { node_contains_unregistered_postgis_distance_function((*relabel).arg.cast()) }
        }
        NodeTag::T_CoerceViaIO => {
            let coercion = node.cast::<pg_sys::CoerceViaIO>();
            unsafe { node_contains_unregistered_postgis_distance_function((*coercion).arg.cast()) }
        }
        NodeTag::T_CoerceToDomain => {
            let coercion = node.cast::<pg_sys::CoerceToDomain>();
            unsafe { node_contains_unregistered_postgis_distance_function((*coercion).arg.cast()) }
        }
        NodeTag::T_CoalesceExpr => {
            let coalesce = node.cast::<pg_sys::CoalesceExpr>();
            unsafe { args_contain_unregistered_postgis_distance_function((*coalesce).args) }
        }
        NodeTag::T_ScalarArrayOpExpr => {
            let scalar_array = node.cast::<pg_sys::ScalarArrayOpExpr>();
            unsafe { args_contain_unregistered_postgis_distance_function((*scalar_array).args) }
        }
        NodeTag::T_CaseExpr => {
            let case_expr = node.cast::<pg_sys::CaseExpr>();
            unsafe {
                node_contains_unregistered_postgis_distance_function((*case_expr).arg.cast())
                    || args_contain_unregistered_postgis_distance_function((*case_expr).args)
                    || node_contains_unregistered_postgis_distance_function(
                        (*case_expr).defresult.cast(),
                    )
            }
        }
        NodeTag::T_CaseWhen => {
            let case_when = node.cast::<pg_sys::CaseWhen>();
            unsafe {
                node_contains_unregistered_postgis_distance_function((*case_when).expr.cast())
                    || node_contains_unregistered_postgis_distance_function(
                        (*case_when).result.cast(),
                    )
            }
        }
        NodeTag::T_ArrayExpr => {
            let array = node.cast::<pg_sys::ArrayExpr>();
            unsafe { args_contain_unregistered_postgis_distance_function((*array).elements) }
        }
        NodeTag::T_MinMaxExpr => {
            let minmax = node.cast::<pg_sys::MinMaxExpr>();
            unsafe { args_contain_unregistered_postgis_distance_function((*minmax).args) }
        }
        _ => false,
    }
}

unsafe fn args_contain_unregistered_postgis_distance_function(args: *mut List) -> bool {
    if args.is_null() {
        return false;
    }

    // SAFETY: args is a valid planner expression List.
    let len = unsafe { pg_sys::list_length(args) };
    for i in 0..len {
        // SAFETY: i is in [0, len).
        let child = unsafe { pg_sys::list_nth(args, i).cast::<pg_sys::Node>() };
        if unsafe { node_contains_unregistered_postgis_distance_function(child) } {
            return true;
        }
    }
    false
}

unsafe fn unregistered_spatial_decline_reason(
    restrictinfo_list: *mut List,
    rows: usize,
    output_rows: f64,
    limits: &cost::DeviceLimits,
) -> Option<super::RejectionReason> {
    if unsafe { restrictinfo_contains_unregistered_postgis_geometry_constructor(restrictinfo_list) }
    {
        return Some(super::RejectionReason::PostgisGeometryConstructorNoGpuOutputProtocol);
    }
    if unsafe { restrictinfo_contains_unregistered_postgis_distance_function(restrictinfo_list) } {
        return Some(super::RejectionReason::PostgisDistanceNoGpuKernel);
    }
    if unsafe { restrictinfo_contains_wrapped_postgis_intersects(restrictinfo_list) } {
        return Some(super::RejectionReason::PostgisIntersectsUnsupportedShape);
    }
    if !unsafe { restrictinfo_contains_unregistered_spatial_predicate(restrictinfo_list) } {
        return None;
    }

    // SAFETY: restrictinfo_list is the planner-provided base restriction list.
    let vcount = unsafe { extract_const_geom_vertex_count(restrictinfo_list) }.unwrap_or(0);
    Some(unregistered_spatial_shape_decline_reason(
        vcount,
        rows,
        output_rows,
        limits,
    ))
}

fn unregistered_spatial_shape_decline_reason(
    vcount: usize,
    rows: usize,
    output_rows: f64,
    limits: &cost::DeviceLimits,
) -> super::RejectionReason {
    if !cost::spatial_polygon_rows_safe(rows, vcount, limits) {
        return super::RejectionReason::SpatialUnsafeRowBand;
    }
    if vcount > 0 && vcount < limits.gpu_spatial_min_vertices {
        return super::RejectionReason::SpatialVerticesBelowBreakEven;
    }
    if vcount > 0 {
        let work_product = (vcount as u64).saturating_mul(rows as u64);
        if work_product < limits.spatial_point_in_ring_break_even_verts_x_rows {
            return super::RejectionReason::SpatialWorkBelowBreakEven;
        }
        if work_product > limits.spatial_point_in_ring_max_verts_x_rows {
            return super::RejectionReason::SpatialWorkAboveMax;
        }
    }
    if !cost::spatial_output_fraction_allowed(output_rows, rows as f64, limits) {
        return super::RejectionReason::SpatialHighOutputFraction;
    }
    if vcount >= limits.gpu_spatial_min_vertices {
        return super::RejectionReason::SpatialPreparedGeometryNotAvailable;
    }
    super::RejectionReason::SpatialNoRegisteredGpuPredicate
}

unsafe fn record_unregistered_spatial_decline(
    restrictinfo_list: *mut List,
    rows: usize,
    output_rows: f64,
    limits: &cost::DeviceLimits,
) -> bool {
    if let Some(reason) =
        unsafe { unregistered_spatial_decline_reason(restrictinfo_list, rows, output_rows, limits) }
    {
        let rejected_rows = u64::try_from(rows).unwrap_or(u64::MAX);
        let reason = reason.stats_key();
        stats::increment_planner_rejected(reason, rejected_rows);
        pgrx::debug1!(
            "pg_accel: set_rel_pathlist: PostGIS spatial predicate declined with reason={}",
            reason
        );
        true
    } else {
        false
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

    // Phase 0 audit: time this hook invocation (TODO.md 2026-05-14). Bench
    // harness reads `pg_accel_planner_overhead_us()` to detect SSBM-style
    // no-dispatch regressions in planner overhead.
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
    if parse.is_null() || unsafe { (*parse).commandType } != pg_sys::CmdType::CMD_SELECT {
        stats::record_command_type_skip();
        return;
    }

    // SAFETY: rel and rte are valid pointers provided by the planner.
    let rel_ref = unsafe { &*rel };
    let rte_ref = unsafe { &*rte };

    let _span =
        tracing::info_span!("planner.rel_pathlist", relid = u32::from(rte_ref.relid)).entered();

    // These cheap shape facts are needed both for resident-only fail-closed
    // behavior and the normal scan/sort admission gates below.
    // SAFETY: root is a valid PlannerInfo pointer.
    let has_sort = unsafe { !(*root).sort_pathkeys.is_null() };
    let has_restrictions = !rel_ref.baserestrictinfo.is_null()
        && unsafe { pg_sys::list_length(rel_ref.baserestrictinfo) } > 0;

    if gucs::gpu_enabled() && super::gpu_resident_pipeline_required() {
        unsafe {
            observe_resident_only_rel_declines(root, rel, rte, has_sort, has_restrictions);
        }
        if rte_ref.rtekind == pg_sys::RTEKind::RTE_FUNCTION || has_sort || has_restrictions {
            super::record_no_gpu_resident_pipeline_decline(
                "rel_pathlist_no_resident_pipeline",
                rel,
            );
        }
        return;
    }

    // Phase 2 F3: FunctionScan injection. Fires on RTE_FUNCTION rels with a
    // registered SRF / record-returning function (h3_grid_disk,
    // h3_cell_to_boundary, st_summarystats). The injector itself rejects
    // other rtekinds, so this is a cheap dispatch. h3_polyfill is not
    // registered until pg_accel covers h3-pg's polygon[] holes signature.
    // SAFETY: All pointers are valid planner arguments.
    unsafe { super::projectset::try_inject_function_scan(root, rel, rti, rte) };

    // Gate 2: Only base relations or partition children — cheap field
    // checks before any registry or clause work.
    //
    // RELOPT_OTHER_MEMBER_REL is the kind PG assigns to each partition
    // child of a partitioned table after `expand_inherited_rtentry`.
    // Allowing it through lets `add_paths_to_append_rel` collect any
    // GPU CustomPath we inject into the child's pathlist when it
    // composes the final Append / MergeAppend over the partition set
    // (TODO.md Phase 4 "Append / MergeAppend injection").
    if !matches!(
        rel_ref.reloptkind,
        pg_sys::RelOptKind::RELOPT_BASEREL | pg_sys::RelOptKind::RELOPT_OTHER_MEMBER_REL
    ) {
        return;
    }
    if rte_ref.rtekind != pg_sys::RTEKind::RTE_RELATION {
        return;
    }

    // Gate 2c: Skip system catalog tables. Accelerator strategies are only
    // useful for user tables. System catalogs have OIDs below
    // FirstNormalObjectId (16384).
    if u32::from(rte_ref.relid) < pg_sys::FirstNormalObjectId {
        return;
    }

    // Gate 2b: Early-exit when there is nothing to accelerate.
    // If no WHERE clauses and no ORDER BY, skip all registry and clause
    // work. This eliminates 100-500µs overhead on queries like SSBM
    // that pg_accel cannot accelerate anyway.
    if !has_sort && !has_restrictions {
        return;
    }

    // GPU sort path for ORDER BY on numeric columns.
    if has_sort {
        pgrx::debug1!("pg_accel: calling try_inject_gpu_sort_path");
        // SAFETY: root, rel are valid planner pointers.
        unsafe { try_inject_gpu_sort_path(root, rel) };
    }

    // Early exit if no restriction clauses — sort was the only possibility.
    if !has_restrictions {
        return;
    }

    let direct_gpuexpr_template;

    // Gate 3: Early rows exit. No scan strategy can fire below min_batch_size
    // (should_batch will reject at Gate 4), so skip expensive clause matching
    // and registry initialisation. This eliminates 20-50µs of overhead on
    // small dimension tables in star-schema queries.
    {
        let rows_early = rel_ref.tuples.max(rel_ref.rows) as usize;
        let rows_early_u64 = u64::try_from(rows_early).unwrap_or(u64::MAX);
        let min_batch = gucs::min_batch_size().max(1) as usize;
        let limits = cost::device_limits();
        if let Some(h3_decline) = unsafe { h3_latlng_qual_decline(rel_ref.baserestrictinfo) } {
            stats::increment_planner_rejected(h3_decline.stats_key(), rows_early_u64);
            pgrx::debug1!(
                "pg_accel: set_rel_pathlist: h3_latlng_to_cell qual declined ({:?}); \
                 scalar H3 predicates stay native until fused GPU expression filtering exists",
                h3_decline
            );
            return;
        }
        if unsafe { restrictinfo_contains_wrapped_postgis_intersects(rel_ref.baserestrictinfo) } {
            stats::increment_planner_rejected(
                super::RejectionReason::PostgisIntersectsUnsupportedShape.stats_key(),
                rows_early_u64,
            );
            pgrx::debug1!(
                "pg_accel: set_rel_pathlist: ST_Intersects scan predicate declined before \
                 row gating; exact fp64/PostGIS semantics are not ready"
            );
            return;
        }
        if rows_early < min_batch {
            unsafe {
                record_unregistered_spatial_decline(
                    rel_ref.baserestrictinfo,
                    rows_early,
                    rel_ref.rows,
                    limits,
                );
            }
            return;
        }

        // BitmapHeapScan + generic GpuExpr is a real planner opportunity, but
        // the current standalone GpuExpr path is disabled until scan input and
        // expression output can stay in one GPU-resident batch pipeline. Keep
        // this visible as an explicit decline instead of silently falling
        // through to PostgreSQL when a bitmap-prefiltered numeric predicate
        // shape appears.
        let bitmap_path = unsafe { super::find_cheapest_bitmap_heap_path(rel_ref.pathlist) };
        let gpu_expr_candidate =
            super::try_gpu_expr_match(rel_ref.baserestrictinfo, rows_early_u64).is_some();
        direct_gpuexpr_template =
            unsafe { direct_gpuexpr_template_match(rel_ref.baserestrictinfo, rows_early, limits) };
        if !bitmap_path.is_null() && gpu_expr_candidate {
            stats::increment_planner_rejected(
                super::RejectionReason::BitmapHeapGpuExprNoGpuPipeline.stats_key(),
                rows_early_u64,
            );
            pgrx::debug1!(
                "pg_accel: set_rel_pathlist: BitmapHeapPath + GpuExpr candidate \
                 declined until fused GPU scan pipeline exists"
            );
            return;
        }

        if let Some(shape) = direct_gpuexpr_template {
            if unsafe { direct_gpuexpr_count_has_parallel_owner(root, rel_ref) } {
                if super::partial_agg::parallel_fused_filter_count_enabled() {
                    pgrx::debug1!(
                        "pg_accel: set_rel_pathlist: direct GpuExpr count path declined because \
                         pg_accel.parallel_fused_count owns this parallel COUNT(*) shape"
                    );
                } else {
                    pgrx::debug1!(
                        "pg_accel: set_rel_pathlist: direct GpuExpr count path declined because \
                         parallel fused-count is disabled; native PostgreSQL \
                         parallel aggregation should own this shape"
                    );
                }
                return;
            }
            pgrx::debug1!(
                "pg_accel: set_rel_pathlist: direct GpuExpr template candidate \
                 admitted through scan-to-columnar producer \
                 (rows={} cols={} complexity={} fp64={})",
                rows_early,
                shape.referenced_cols,
                shape.predicate_complexity,
                shape.uses_fp64
            );
        } else if gpu_expr_candidate {
            stats::increment_planner_rejected(
                super::RejectionReason::StandaloneGpuExprNoGpuPipeline.stats_key(),
                rows_early_u64,
            );
            pgrx::debug1!(
                "pg_accel: set_rel_pathlist: standalone GpuExpr candidate \
                 declined until fused GPU scan pipeline exists"
            );
            return;
        }
    }

    // Extension-function match: requires the adapter registry (PostGIS,
    // H3, raster). Broad generic numeric GpuExpr is not exposed as a
    // standalone path until the selected scan can stay GPU-resident and prove
    // crash-free in live integration.
    registry::lazy_init();
    let reg_match = if registry::is_ready() {
        let reg = registry::global_registry();
        if reg.is_empty() {
            None
        } else {
            find_accelerable_match(rel_ref.baserestrictinfo)
        }
    } else {
        None
    };

    let accel = if direct_gpuexpr_template.is_some() {
        Some(super::AccelMatch {
            strategy: registry::AccelStrategy::GpuExpr,
            fn_oid: pg_sys::Oid::INVALID,
            target_attno: 0,
        })
    } else {
        reg_match
    };
    let Some(accel) = accel else {
        let rows = rel_ref.tuples.max(rel_ref.rows) as usize;
        unsafe {
            record_unregistered_spatial_decline(
                rel_ref.baserestrictinfo,
                rows,
                rel_ref.rows,
                cost::device_limits(),
            );
        }
        pgrx::debug1!("pg_accel: set_rel_pathlist: no accelerable match found");
        return;
    };
    let strategy = accel.strategy;
    let fn_name = unsafe { function_name_for_oid(accel.fn_oid) };
    if strategy == registry::AccelStrategy::GpuExpr && direct_gpuexpr_template.is_none() {
        pgrx::debug1!(
            "pg_accel: set_rel_pathlist: standalone GpuExpr Custom Scan exposure disabled"
        );
        return;
    }
    if strategy == registry::AccelStrategy::GpuSpatial {
        stats::increment_planner_rejected(
            super::RejectionReason::PostgisIntersectsUnsupportedShape.stats_key(),
            u64::try_from(rel_ref.tuples.max(rel_ref.rows) as usize).unwrap_or(u64::MAX),
        );
        pgrx::debug1!(
            "pg_accel: set_rel_pathlist: PostGIS function={:?} declined before \
             expensive shape validation because exact fp64/PostGIS semantics are not ready",
            fn_name.as_deref()
        );
        return;
    }
    if strategy == registry::AccelStrategy::GpuH3 {
        stats::increment_planner_rejected(
            super::RejectionReason::H3LatLngScalarPredicateNoGpuPipeline.stats_key(),
            u64::try_from(rel_ref.tuples.max(rel_ref.rows) as usize).unwrap_or(u64::MAX),
        );
        pgrx::debug1!(
            "pg_accel: set_rel_pathlist: H3 function={:?} declined because \
             standalone scalar H3 scan predicates need fused GPU expression filtering",
            fn_name.as_deref()
        );
        return;
    }
    pgrx::debug1!("pg_accel: set_rel_pathlist: found {:?} match", strategy);

    // Gate 3b: GPU must be available and enabled before any pg_accel path is
    // injected. This intentionally sits after registry/shape-only declines so
    // unsupported PostGIS/H3/raster shapes expose precise native-decline
    // reasons even on test hosts without a usable GPU.
    if !cost::gpu_is_usable() {
        stats::increment_planner_rejected(
            super::RejectionReason::GpuNotUsable.stats_key(),
            u64::try_from(rel_ref.tuples.max(rel_ref.rows) as usize).unwrap_or(u64::MAX),
        );
        pgrx::debug1!("pg_accel: set_rel_pathlist: GPU not usable");
        return;
    }
    pgrx::debug1!("pg_accel: set_rel_pathlist: GPU usable, checking restrictions");

    // Gate 4: Cost model gating — skip if batching is not worthwhile.
    // Use strategy-aware per-row cost so GPU paths (with higher overhead)
    // require more rows to break even.
    // Use rel.tuples (total table rows) not rel.rows (estimated output after
    // filtering). The GPU processes ALL input rows to evaluate the predicate;
    // rel.rows is the post-filter estimate which can be tiny for selective
    // spatial predicates even on large tables.
    let rows = rel_ref.tuples.max(rel_ref.rows) as usize;
    let min_batch = gucs::min_batch_size().max(1) as usize;
    let per_row_cost_base = match strategy {
        registry::AccelStrategy::GpuRaster => cost::GPU_RASTER_PER_ROW_COST,
        registry::AccelStrategy::GpuH3 => cost::GPU_H3_PER_ROW_COST,
        _ => cost::GPU_SPATIAL_PER_ROW_COST,
    };
    // fp64 classification via the adapter helper (`adapters::uses_fp64`).
    // Spatial always uses fp64 (recheck path); H3 uses fp64 only for
    // `h3_latlng_to_cell`; raster reports false. For strategies where
    // the function name can't be resolved, default to `false`. The helper
    // centralises the per-function classification so it's consistent with
    // the adapters' own declarations in `pg_accel/src/adapters/*.rs`.
    let fn_uses_fp64 = fn_name
        .as_deref()
        .is_some_and(|name| crate::adapters::uses_fp64(strategy, name));
    let per_row_cost =
        cost::apply_fp64_penalty(per_row_cost_base, fn_uses_fp64, cost::device_limits());
    if !cost::should_batch(rows, per_row_cost, min_batch) {
        pgrx::debug1!(
            "pg_accel: set_rel_pathlist: batch rejected tuples={} min_batch={}",
            rows,
            min_batch
        );
        return;
    }

    // Gate 4a: extension-specific standalone scan policy.
    //
    // H3/PostGIS/raster functions are only exposed through rel_pathlist when
    // the scan itself is a batch-sized compute-heavy unit. Cheap scalar ops
    // should be fused into GpuScan later rather than wrapped as standalone
    // Custom Scans over PostgreSQL tuples.
    if let Err(reason) =
        extension_scan_gate(strategy, fn_name.as_deref(), rows, cost::device_limits())
    {
        pgrx::debug1!(
            "pg_accel: set_rel_pathlist: {:?} rejected function={:?} rows={} reason={}",
            strategy,
            fn_name.as_deref(),
            rows,
            reason
        );
        return;
    }

    // Gate 4b: Defer to GiST/SP-GiST index scan for selective spatial filters.
    // When a spatial index exists and is highly selective, PG's native index
    // scan avoids touching most heap pages entirely. Wrapping that in a Custom
    // Scan adds geometry deser + batch + kernel overhead that causes a
    // regression (measured 6.9x slower on spatial_filter benchmark).
    if matches!(
        strategy,
        registry::AccelStrategy::GpuSpatial | registry::AccelStrategy::GpuRaster
    ) {
        // Find the cheapest sequential scan (T_Path) as the baseline for
        // cost-ratio comparison. Using find_cheapest_path() here would
        // return the index path itself when it's cheapest, making the
        // ratio idx_cost/idx_cost = 1.0 (always fails the < 0.40 check).
        // SAFETY: rel_ref.pathlist is a valid List pointer from the planner.
        let seq_scan_cost = unsafe { find_cheapest_seqscan_cost(rel_ref.pathlist) };
        // SAFETY: rel_ref.pathlist is a valid List pointer from the planner.
        if unsafe { has_cheap_spatial_index_path(rel_ref.pathlist, seq_scan_cost) } {
            stats::increment_planner_rejected(
                super::RejectionReason::SpatialIndexCheaper.stats_key(),
                rows as u64,
            );
            return;
        }
    }

    // Gate 4c: Vertex count thresholds for spatial predicates.
    //
    // Two-sided gate:
    //
    // - LOWER gate: GPU overhead is ~19ms constant (geometry deser + seq
    //   scan), regardless of polygon complexity. PG parallel scales linearly
    //   with vertex count. Below the threshold, PG parallel is faster.
    //
    // - UPPER gate: The relevant work metric for point-in-polygon is the
    //   product `vertex_count * row_count`. Megapoly fixtures (e.g.
    //   `scale_1m_mega500v` with a 1M-vertex polygon and 10M rows) push
    //   this product into the 10^13 range, which fp32 PIP kernels can't
    //   amortize against transfer. Above the max product the megapoly
    //   loser path is strictly worse than PG parallel — skip injection
    //   entirely.
    //
    // Both thresholds come from `DeviceLimits` per CLAUDE.md rule 10;
    // they are calibrated from the 2026-04-11 benchmark run and derived
    // from the hardware profile in `from_profile`.
    if strategy == registry::AccelStrategy::GpuSpatial {
        let dl = cost::device_limits();
        let rows_u64 = rows as u64;
        let min_verts = dl.gpu_spatial_min_vertices;
        let min_product = dl.spatial_point_in_ring_break_even_verts_x_rows;
        let max_product = dl.spatial_point_in_ring_max_verts_x_rows;
        // SAFETY: rel_ref.baserestrictinfo is a valid List from the planner.
        // Treat unknown vertex count (None) as 0 — conservatively reject
        // when the planner cannot confirm the polygon has enough vertices.
        // This prevents injection for FuncExpr geometry args (ST_Buffer,
        // ST_MakeEnvelope) that PG didn't constant-fold.
        let vcount =
            unsafe { extract_const_geom_vertex_count(rel_ref.baserestrictinfo) }.unwrap_or(0);
        if !cost::spatial_polygon_rows_safe(rows, vcount, dl) {
            stats::increment_planner_rejected(
                super::RejectionReason::SpatialUnsafeRowBand.stats_key(),
                rows_u64,
            );
            pgrx::debug1!(
                "pg_accel: spatial 100K safety gate: rows={} vcount={} in unsafe band \
                 [{}..={}] with min_vertices={}, skipping",
                rows,
                vcount,
                dl.gpu_spatial_unsafe_band_min_rows,
                dl.gpu_spatial_unsafe_band_max_rows,
                dl.gpu_spatial_unsafe_band_min_vertices,
            );
            return;
        }
        if vcount < min_verts {
            stats::increment_planner_rejected(
                super::RejectionReason::SpatialVerticesBelowBreakEven.stats_key(),
                rows_u64,
            );
            pgrx::debug1!(
                "pg_accel: spatial vertex gate: vcount={} < min={}, skipping",
                vcount,
                min_verts
            );
            return;
        }
        // Compute the work product as u64 to avoid overflow at 10^13.
        let work_product = (vcount as u64).saturating_mul(rows as u64);
        if work_product < min_product {
            stats::increment_planner_rejected(
                super::RejectionReason::SpatialWorkBelowBreakEven.stats_key(),
                rows_u64,
            );
            pgrx::debug1!(
                "pg_accel: spatial work-product gate: vcount={} * rows={} = {} < min {}, \
                 skipping (below break-even)",
                vcount,
                rows,
                work_product,
                min_product
            );
            return;
        }
        if work_product > max_product {
            stats::increment_planner_rejected(
                super::RejectionReason::SpatialWorkAboveMax.stats_key(),
                rows_u64,
            );
            pgrx::debug1!(
                "pg_accel: spatial work-product gate: vcount={} * rows={} = {} > max {}, \
                 skipping (megapoly loser path)",
                vcount,
                rows,
                work_product,
                max_product
            );
            return;
        }
        let output_fraction = cost::spatial_output_fraction(rel_ref.rows, rows as f64);
        if !cost::spatial_output_fraction_allowed(rel_ref.rows, rows as f64, dl) {
            stats::increment_planner_rejected(
                super::RejectionReason::SpatialHighOutputFraction.stats_key(),
                rows_u64,
            );
            pgrx::debug1!(
                "pg_accel: spatial output gate: estimated output fraction {:.3} > max {:.3}, \
                 skipping heap-backed Custom Scan",
                output_fraction,
                dl.gpu_spatial_max_output_fraction
            );
            return;
        }
    }

    // Gate 5: Find the path to wrap as the CustomScan child.
    //
    // Default baseline is the cheapest sequential scan: the Custom Scan
    // always does a full heap walk, so the seq scan cost is the honest
    // baseline. Using an index path as baseline + discount makes the
    // Custom Scan appear cheaper than the index scan it was based on,
    // causing regressions when the index scan would be far faster.
    //
    // BitmapHeapScan injection: when PG has pruned the seq scan in favour
    // of a bitmap-driven plan (typical for selective predicates), try
    // wrapping the cheapest `T_BitmapHeapPath` instead. The bitmap child
    // still pre-filters via the index, and the GPU then evaluates the full
    // qual on the bitmap-filtered rows. Both `plan.qual` and
    // `bitmapqualorig` are stripped in `make_custom_scan_plan`
    // (custom_scan/mod.rs), so a selected pg_accel node never uses PG's CPU
    // bitmap recheck as the predicate implementation.
    //
    // Registry-backed scan strategies (GpuSpatial / GpuH3 / GpuRaster) use
    // `scanrelid=0` and consume the child via `ExecProcNode`, which works
    // for any child plan node including BitmapHeapScan.
    // SAFETY: rel_ref.pathlist is a valid List pointer from the planner.
    let mut cheapest = unsafe { find_cheapest_seqscan_path(rel_ref.pathlist) };
    let mut wrapped_bitmap = false;
    if cheapest.is_null() {
        // SAFETY: rel_ref.pathlist is a valid List pointer.
        let bitmap = unsafe { super::find_cheapest_bitmap_heap_path(rel_ref.pathlist) };
        if !bitmap.is_null() {
            pgrx::debug1!(
                "pg_accel: scan hook: no seq scan path; wrapping cheapest \
                 BitmapHeapPath as CustomScan child"
            );
            cheapest = bitmap;
            wrapped_bitmap = true;
        }
    }
    if cheapest.is_null() {
        pgrx::debug1!(
            "pg_accel: scan hook: no seq scan or bitmap path in pathlist, \
             index paths dominate — skipping Custom Scan injection"
        );
        return;
    }

    // Build cost estimate using the cost model with strategy-aware constants.
    // SAFETY: cheapest is non-null, checked above.
    let base = unsafe { &*cheapest };
    #[allow(clippy::cast_precision_loss)]
    let batch_size = cost::optimal_batch_size(rows) as f64;
    let num_batches = (base.rows / batch_size).ceil();
    let batch_overhead = num_batches * 2.0; // per-batch dispatch + sync cost
    // GPU strategies incur a fixed kernel launch overhead.
    let gpu_overhead = match strategy {
        registry::AccelStrategy::GpuSpatial
        | registry::AccelStrategy::GpuRaster
        | registry::AccelStrategy::GpuH3 => cost::GPU_LAUNCH_OVERHEAD,
        _ => 0.0,
    };
    // Custom Scan runs single-threaded. Do NOT add parallel_setup_cost to
    // our startup — we already compare against PG's cheapest NON-parallel
    // seqscan downstream (find_cheapest_seqscan_path). Adding the parallel
    // penalty here was a double charge that pushed us out of contention
    // for every parallelizable relation, which is exactly the set of
    // queries we want to accelerate.
    let startup_cost = base.startup_cost + 1.0 + gpu_overhead;

    // All registry-backed GPU strategies get a cost-margin discount vs PG's
    // serial seqscan. Compute-bound spatial/h3/raster operations get a more
    // aggressive margin because GPU batched SIMD evaluation is dramatically
    // faster than PG's per-row function call loop (even parallelized across
    // workers), and PG's parallel cost model divides by worker count which
    // underestimates the gap.
    let cost_margin = match strategy {
        registry::AccelStrategy::GpuSpatial
        | registry::AccelStrategy::GpuRaster
        | registry::AccelStrategy::GpuH3 => cost::GPU_COST_SAFETY_MARGIN * 0.5,
        _ => cost::GPU_COST_SAFETY_MARGIN,
    };
    // Soft-fp64 per-row overhead: when the classified function uses fp64
    // and the device lacks native fp64, add a per-row penalty equal to the
    // unpenalised per-row cost multiplied by `(multiplier - 1.0)`. This is
    // the extra work the emulation performs vs native fp32. Zero when
    // `fn_uses_fp64 == false` or the device has native fp64 (the penalty
    // simplifies to `per_row_cost_base * (multiplier - 1.0)` only when the
    // penalty was actually applied above; otherwise per_row_cost equals
    // per_row_cost_base and the delta is zero).
    #[allow(clippy::cast_precision_loss)]
    let soft_fp64_overhead = rows as f64 * (per_row_cost - per_row_cost_base);
    let raw_total =
        (base.total_cost.mul_add(cost_margin, batch_overhead) + gpu_overhead + soft_fp64_overhead)
            * gucs::cost_multiplier();

    // Let PG's native cost comparison decide — add_path() discards
    // paths that are strictly dominated by cheaper alternatives.
    let total_cost = raw_total;

    // Gate 6: Post-cost spatial index check.
    // After computing our Custom Scan cost, compare against any GiST/SP-GiST
    // index path. The Custom Scan always does a full sequential heap scan.
    // At large scales (10M+), a selective GiST index scan reads far fewer
    // heap pages, making it faster even if PG's cost estimate is close.
    // This applies to all scan strategies: the Custom Scan still does a full
    // heap walk unless a bitmap child is wrapped.
    // SAFETY: rel_ref.pathlist is a valid List pointer from the planner.
    if unsafe { has_cheaper_spatial_index_path(rel_ref.pathlist, total_cost) } {
        stats::increment_planner_rejected(
            super::RejectionReason::SpatialIndexCheaper.stats_key(),
            rows as u64,
        );
        return;
    }

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

        // Serialize [fn_oid, target_attno, accel_strategy] into the path's
        // custom_private. PlanCustomPath reads this and builds the full
        // [strategy, batch_size, threads, fn_oid, target_attno, accel_strategy]
        // layout for the scan node's custom_private.
        // SAFETY: makeInteger + lappend allocate in CurrentMemoryContext.
        let mut priv_list: *mut List = std::ptr::null_mut();
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(u32::from(accel.fn_oid) as i32).cast(),
        );
        priv_list = lappend(priv_list, pg_sys::makeInteger(accel.target_attno).cast());
        priv_list = lappend(priv_list, pg_sys::makeInteger(accel.strategy as i32).cast());
        (*cpath).custom_private = priv_list;

        if !super::add_gpu_path_with_resident_proof(
            "rel_pathlist_scan_no_resident_pipeline",
            rel,
            cpath,
            custom_scan::resident_proof_default_for_strategy(custom_scan::GpuStrategy::Scan),
        ) {
            return;
        }
    }

    // Observability for the new injection sites added in TODO.md Phase 4.
    // `wrapped_bitmap` fires when the cheapest seq scan was pruned and we
    // fell back to wrapping a `T_BitmapHeapPath` child; `partition_child`
    // fires when the planner hook was invoked on a `RELOPT_OTHER_MEMBER_REL`
    // (i.e. a partition of a partitioned table).
    if wrapped_bitmap {
        tracing::info!(
            target: "pg_accel::planner",
            relid = u32::from(rte_ref.relid),
            strategy = ?strategy,
            rows = base.rows,
            "planner.wrapped_bitmap_heap_path"
        );
    }

    // Second inject site for bitmap-driven plans (TODO.md Phase 4
    // "BitmapHeapScan injection"): when a BitmapHeapPath also exists
    // alongside the seq scan we just wrapped, add a parallel CustomPath
    // wrapping the bitmap path too. PG's `add_path` cost-compares the two
    // and keeps whichever is cheaper, so a selective bitmap subset
    // (e.g. `WHERE indexed_col = const AND ...`) gets a GPU CustomScan
    // child that pre-filters via the index instead of doing a full
    // sequential heap walk.
    //
    // Skip when `wrapped_bitmap` is already true: we already wrapped the
    // bitmap path because seqscan was pruned.
    if !wrapped_bitmap {
        // SAFETY: rel_ref.pathlist is a valid List pointer.
        let bitmap_alt = unsafe { super::find_cheapest_bitmap_heap_path(rel_ref.pathlist) };
        if !bitmap_alt.is_null() {
            // SAFETY: bitmap_alt is non-null and is a valid Path*.
            let bbase = unsafe { &*bitmap_alt };
            // Recompute cost against the bitmap baseline so add_path's
            // dominance check sees an honest total. Bitmap paths read
            // fewer heap pages, so the wrapped cost is typically lower
            // than the seqscan-wrapped CustomPath cost.
            #[allow(clippy::cast_precision_loss)]
            let b_batch_size = cost::optimal_batch_size(rows) as f64;
            let b_num_batches = (bbase.rows / b_batch_size).ceil();
            let b_batch_overhead = b_num_batches * 2.0;
            let b_startup = bbase.startup_cost + 1.0 + gpu_overhead;
            #[allow(clippy::cast_precision_loss)]
            let b_soft_fp64 = rows as f64 * (per_row_cost - per_row_cost_base);
            let b_total = (bbase.total_cost.mul_add(cost_margin, b_batch_overhead)
                + gpu_overhead
                + b_soft_fp64)
                * gucs::cost_multiplier();

            // SAFETY: Allocating via palloc, building valid CustomPath.
            unsafe {
                let bcpath = create_custom_path(
                    rel,
                    bitmap_alt,
                    b_startup,
                    b_total,
                    bbase.rows,
                    custom_scan::scan_path_methods(),
                );
                // Same custom_private layout as the seqscan injection.
                let mut bpriv: *mut List = std::ptr::null_mut();
                bpriv = lappend(
                    bpriv,
                    pg_sys::makeInteger(u32::from(accel.fn_oid) as i32).cast(),
                );
                bpriv = lappend(bpriv, pg_sys::makeInteger(accel.target_attno).cast());
                bpriv = lappend(bpriv, pg_sys::makeInteger(accel.strategy as i32).cast());
                (*bcpath).custom_private = bpriv;
                if !super::add_gpu_path_with_resident_proof(
                    "rel_pathlist_bitmap_scan_no_resident_pipeline",
                    rel,
                    bcpath,
                    custom_scan::resident_proof_default_for_strategy(
                        custom_scan::GpuStrategy::Scan,
                    ),
                ) {
                    return;
                }
            }

            tracing::info!(
                target: "pg_accel::planner",
                relid = u32::from(rte_ref.relid),
                strategy = ?strategy,
                rows = bbase.rows,
                "planner.injected_bitmap_heap_alt"
            );
        }
    }

    if rel_ref.reloptkind == pg_sys::RelOptKind::RELOPT_OTHER_MEMBER_REL {
        tracing::info!(
            target: "pg_accel::planner",
            relid = u32::from(rte_ref.relid),
            strategy = ?strategy,
            rows = base.rows,
            "planner.injected_partition_child"
        );
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
    let rte_ref = unsafe { &*rte };

    if has_sort {
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
    if let Some(h3_decline) = unsafe { h3_latlng_qual_decline(rel_ref.baserestrictinfo) } {
        stats::increment_planner_rejected(h3_decline.stats_key(), rows);
        pgrx::debug1!(
            "pg_accel: set_rel_pathlist resident-only observer: h3_latlng_to_cell qual \
             declined ({:?}) before the generic resident-pipeline gate",
            h3_decline
        );
    }
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

// ---------------------------------------------------------------------------
// GPU sort injection (scan-level, avoids upper-path tlist issues)
// ---------------------------------------------------------------------------

/// GPU-sortable numeric type OIDs.
pub(super) const SORT_INT4OID: u32 = 23;
pub(super) const SORT_INT8OID: u32 = 20;
pub(super) const SORT_FLOAT4OID: u32 = 700;
pub(super) const SORT_FLOAT8OID: u32 = 701;

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
pub(super) fn sort_topk_backend_supported(backend_name: &[u8]) -> bool {
    // The Metal/AdaptiveCpp top-k SQL path can still crash a backend even
    // under the bounded LIMIT cap. Keep direct kernel tests available, but
    // leave selected SQL ORDER BY plans native on Metal until that path is
    // repaired.
    backend_name != b"metal"
}

#[must_use]
fn selected_gpu_sort_topk_kernel_supported() -> bool {
    // On macOS the supported AdaptiveCpp backend is Metal. Keep this gate
    // compile-time cheap so no-dispatch ORDER BY queries do not initialise
    // the GPU runtime just to learn that standalone top-k is unavailable.
    #[cfg(target_os = "macos")]
    {
        sort_topk_backend_supported(b"metal")
    }
    #[cfg(not(target_os = "macos"))]
    {
        sort_topk_backend_supported(b"")
    }
}

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
/// opportunity we currently decline (tracked in TODO.md Phase 4
/// "IncrementalSort injection").
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

/// Inject a GPU sort `CustomPath` at the scan level when `root->sort_pathkeys`
/// has a single numeric key and the relation has enough rows.
///
/// By injecting at scan level with pathkeys set, PG knows the scan output is
/// already sorted and won't add a Sort node. This avoids the upper-path
/// `apply_tlist_labeling` assert that fires when `custom_scan_tlist` length
/// differs from the child plan's targetlist.
///
/// # Safety
///
/// `root` and `rel` must be valid planner-provided pointers.
#[allow(clippy::too_many_lines)]
unsafe fn try_inject_gpu_sort_path(root: *mut PlannerInfo, rel: *mut RelOptInfo) {
    // SAFETY: root is a valid PlannerInfo pointer.
    let root_ref = unsafe { &*root };
    let rel_ref = unsafe { &*rel };

    // Gate: Need sort pathkeys (i.e., an ORDER BY clause).
    let sort_pathkeys = root_ref.sort_pathkeys;
    if sort_pathkeys.is_null() {
        pgrx::debug1!("pg_accel sort: sort_pathkeys is NULL");
        return;
    }
    // SAFETY: sort_pathkeys is a valid List.
    let num_pathkeys = unsafe { pg_sys::list_length(sort_pathkeys) };
    pgrx::debug1!("pg_accel sort: num_pathkeys={num_pathkeys}");

    // Classify the sort shape BEFORE gating on GPU_SORT_MAX_PATHKEYS so
    // IncrementalSort opportunities and already-sorted paths are visible
    // in the trace / planner-rejected counter even when we decline. This
    // lets us measure real-world incidence of multi-key sorts with a
    // presorted prefix — the work item to actually exploit them is
    // tracked in TODO.md Phase 4 "IncrementalSort injection" and blocked
    // on cascaded multi-key GPU sort (post-1.0).
    // SAFETY: rel_ref.pathlist is a valid List provided by the planner.
    let presorted =
        unsafe { longest_presorted_prefix(rel_ref.pathlist, sort_pathkeys, num_pathkeys) };
    let shape = classify_sort_shape(presorted, num_pathkeys);
    match shape {
        SortShape::AlreadySorted { total } => {
            // PG already sees this as free — no Sort node will be planned,
            // so there is nothing for us to accelerate. Distinct from the
            // FullSort/Incremental decline so logs aren't misleading.
            pgrx::debug1!(
                "pg_accel sort: ORDER BY ({total} keys) already satisfied by an existing \
                 path; no Sort node to inject"
            );
            return;
        }
        SortShape::IncrementalOpportunity {
            presorted: p,
            suffix: s,
            total,
        } => {
            // Observability only. The GPU sort executor is single-key
            // (see GPU_SORT_MAX_PATHKEYS) and cannot do per-group sort on
            // the suffix. Record the rejection so we can count how often
            // real workloads hit this shape, then fall through to the
            // standard multi-key decline below.
            pgrx::debug1!(
                "pg_accel sort: IncrementalSort opportunity skipped: {p} presorted keys, \
                 {s} suffix keys (total={total}); deferred to cascaded multi-key GPU sort"
            );
            #[allow(clippy::cast_sign_loss)]
            let n_rows_est = rel_ref.rows.max(0.0) as u64;
            stats::increment_planner_rejected("sort_incremental_opportunity", n_rows_est);
        }
        SortShape::FullSort { .. } => {
            // Fall through to the standard full-sort path.
        }
    }

    // GPU sort executor currently only supports single-key sort. Multi-key
    // would require cascaded stable sorts (sort by last key first, then by
    // prior keys) — post-1.0 work. Gate planner + executor on the same
    // constant so the advertised capability matches what actually dispatches.
    // See `GPU_SORT_MAX_PATHKEYS` below.
    if num_pathkeys < 1 || num_pathkeys > GPU_SORT_MAX_PATHKEYS {
        pgrx::debug1!(
            "pg_accel sort: {num_pathkeys} keys outside supported range 1..={GPU_SORT_MAX_PATHKEYS}, skipping"
        );
        #[allow(clippy::cast_sign_loss)]
        let rejected_rows = rel_ref.rows.max(0.0) as u64;
        stats::increment_planner_rejected(
            super::RejectionReason::SortMultiKeyNoGpuKernel.stats_key(),
            rejected_rows,
        );
        return;
    }

    // Gate: Row count threshold.
    let rows = rel_ref.rows as usize;
    let limits = cost::device_limits();
    let min_rows = limits.gpu_sort_planner_min_rows;
    pgrx::debug1!("pg_accel sort: rows={rows}, min={min_rows}");
    if rows < min_rows {
        pgrx::debug1!("pg_accel sort: too few rows");
        return;
    }

    // Gate: reject the MIN/MAX → ORDER BY LIMIT 1 rewrite. PG transforms
    // `SELECT MIN(col) FROM t` into a subplan shaped `SELECT col FROM t
    // ORDER BY col LIMIT 1` (see `preprocess_minmax_aggregates` in
    // `src/backend/optimizer/plan/planagg.c`). That subplan reaches us as a
    // single-key ORDER BY + LIMIT 1 here, and previously got routed through
    // `Strategy: GpuSort` — which runs a full GPU sort to return one row,
    // taking 100s of ms vs PG's 10-20 ms native IndexScan/SeqScan + Limit.
    // Decline so PG can run its native plan untouched. See
    // `min_max_rewrite_shape` for the exact predicate.
    let limit_tuples = root_ref.limit_tuples;
    if min_max_rewrite_shape(limit_tuples, num_pathkeys) {
        pgrx::debug1!(
            "pg_accel sort: LIMIT 1 on {} rows with single ORDER BY key — \
             MIN/MAX rewrite shape; deferring to PostgreSQL native plan",
            rows
        );
        let rejected_rows = u64::try_from(rows).unwrap_or(u64::MAX);
        stats::increment_planner_rejected(
            super::RejectionReason::MinMaxRewriteNotASort.stats_key(),
            rejected_rows,
        );
        return;
    }

    // Gate: reject full-output standalone heap sorts. A positive LIMIT by
    // itself is not enough: LIMIT values close to the relation cardinality
    // still push nearly every heap tuple through the Custom Scan and are the
    // same loser lane as no-limit ORDER BY. Keep only bounded top-k shapes
    // exposed from this hook; GPU-resident rank/finalization/full-sort uses
    // should enter through a later internal pipeline instead.
    if !heap_topk_sort_candidate(limit_tuples, rows) {
        pgrx::debug1!(
            "pg_accel sort: LIMIT {:?} on {} rows is not a bounded top-k heap sort; \
             deferring standalone ORDER BY to PostgreSQL",
            limit_tuples,
            rows
        );
        let rejected_rows = u64::try_from(rows).unwrap_or(u64::MAX);
        stats::increment_planner_rejected(
            super::RejectionReason::SortHeapFullOutput.stats_key(),
            rejected_rows,
        );
        return;
    }

    // Backend support is checked after the pure shape gates so no-dispatch
    // benchmark rows keep their truthful decline reason. On macOS this still
    // keeps the Metal top-k kernel unavailable for selected SQL plans until
    // the backend-specific candidate bug is fixed.
    if !selected_gpu_sort_topk_kernel_supported() {
        pgrx::debug1!(
            "pg_accel sort: selected top-k kernel unsupported on this GPU backend; \
             deferring standalone ORDER BY to PostgreSQL"
        );
        let rejected_rows = u64::try_from(rows).unwrap_or(u64::MAX);
        stats::increment_planner_rejected("sort_topk_backend_unsupported", rejected_rows);
        return;
    }

    if !cost::gpu_is_usable() {
        pgrx::debug1!("pg_accel sort: gpu not usable");
        return;
    }

    // Extract sort key info from the single PathKey.
    // SAFETY: sort_pathkeys has exactly 1 element (checked above).
    let pathkey = unsafe { pg_sys::list_nth(sort_pathkeys, 0).cast::<pg_sys::PathKey>() };
    if pathkey.is_null() {
        return;
    }
    let pk = unsafe { &*pathkey };

    let eclass = pk.pk_eclass;
    if eclass.is_null() {
        return;
    }
    let ec = unsafe { &*eclass };

    // Find a Var member in the equivalence class to get attno and type.
    let ec_members = ec.ec_members;
    if ec_members.is_null() {
        return;
    }
    let num_members = unsafe { pg_sys::list_length(ec_members) };

    let mut var_attno: i16 = 0;
    let mut var_typid: u32 = 0;
    let mut found_var = false;

    for i in 0..num_members {
        // SAFETY: i is in [0, num_members).
        #[allow(clippy::cast_ptr_alignment)]
        let em = unsafe { pg_sys::list_nth(ec_members, i).cast::<pg_sys::EquivalenceMember>() };
        if em.is_null() {
            continue;
        }
        let em_ref = unsafe { &*em };
        let expr = em_ref.em_expr;
        if expr.is_null() {
            continue;
        }
        // SAFETY: expr is a valid Expr node.
        #[allow(clippy::cast_ptr_alignment)]
        if unsafe { (*expr.cast::<pg_sys::Node>()).type_ } == NodeTag::T_Var {
            let var = expr.cast::<pg_sys::Var>();
            // SAFETY: var is a valid Var node.
            var_attno = unsafe { (*var).varattno };
            var_typid = u32::from(em_ref.em_datatype);
            found_var = true;
            break;
        }
    }

    if !found_var || var_attno <= 0 {
        pgrx::debug1!("pg_accel sort: no Var found (found={found_var}, attno={var_attno})");
        return;
    }

    pgrx::debug1!("pg_accel sort: var_attno={var_attno}, var_typid={var_typid}");

    // Gate: Only GPU-sortable numeric types.
    if !matches!(
        var_typid,
        SORT_INT4OID | SORT_INT8OID | SORT_FLOAT4OID | SORT_FLOAT8OID
    ) {
        // SAFETY: planner hook runs on the main backend thread, so catalog
        // type lookups inside planner_type_policy are safe here.
        if let Some(policy) =
            unsafe { super::planner_type_policy(pg_sys::Oid::from(var_typid)).rejection() }
        {
            super::record_planner_type_rejection(
                "sort",
                pg_sys::Oid::from(var_typid),
                policy,
                rows as u64,
            );
        }
        pgrx::debug1!("pg_accel sort: unsupported type {var_typid}");
        return;
    }

    let uses_fp64 = var_typid == SORT_FLOAT8OID;

    let output_width = unsafe { (*(*rel).reltarget).width.max(0) } as usize;
    let key_class = if matches!(var_typid, SORT_FLOAT4OID | SORT_FLOAT8OID) {
        cost::formulas::SortKeyClass::Float
    } else {
        cost::formulas::SortKeyClass::Integer
    };
    #[allow(clippy::cast_precision_loss)]
    let materialized_output_fraction = limit_tuples.ceil() / rows.max(1) as f64;
    let admission = cost::formulas::sort_admission(
        cost::formulas::SortAdmissionInput {
            rows,
            limit_tuples: Some(limit_tuples),
            estimated_row_width: output_width,
            key_count: num_pathkeys as usize,
            key_class: Some(key_class),
            algorithm: cost::formulas::SortAlgorithm::StandaloneTopK,
            materialized_output_fraction,
            chunk_count: 1,
            cold_jit: false,
        },
        limits,
    );
    if !admission.eligible {
        pgrx::debug1!(
            "pg_accel sort: admission declined {:?} for LIMIT {:?}, rows={}, width={}; \
             deferring standalone ORDER BY to PostgreSQL",
            admission.reason,
            limit_tuples,
            rows,
            output_width
        );
        let rejected_rows = u64::try_from(rows).unwrap_or(u64::MAX);
        let reason = match admission.reason {
            Some(cost::formulas::SortDeclineReason::RowTooWide) => "sort_heap_topk_wide_output",
            _ => super::RejectionReason::SortHeapFullOutput.stats_key(),
        };
        stats::increment_planner_rejected(reason, rejected_rows);
        return;
    }

    // Derive the sort operator OID from the PathKey's opfamily + compare type.
    let strategy = match pk.pk_cmptype {
        pg_sys::CompareType::COMPARE_LT => pg_sys::BTLessStrategyNumber,
        pg_sys::CompareType::COMPARE_LE => pg_sys::BTLessEqualStrategyNumber,
        pg_sys::CompareType::COMPARE_GE => pg_sys::BTGreaterEqualStrategyNumber,
        pg_sys::CompareType::COMPARE_GT => pg_sys::BTGreaterStrategyNumber,
        _ => return,
    };

    // SAFETY: get_opfamily_member is a catalog lookup, safe on main thread.
    let sort_op = unsafe {
        pg_sys::get_opfamily_member(
            pk.pk_opfamily,
            pg_sys::Oid::from(var_typid),
            pg_sys::Oid::from(var_typid),
            strategy as i16,
        )
    };
    if sort_op == pg_sys::Oid::INVALID {
        return;
    }

    // Find cheapest path to wrap.
    let cheapest = unsafe { find_cheapest_path(rel_ref.pathlist) };
    if cheapest.is_null() {
        return;
    }

    // Universal cost model for self-scanning sort path.
    // Sort extracts 1 key column; output includes all columns (yield overhead
    // accounted for in scan_cost component of self_scan_cost).
    // SAFETY: cheapest is non-null.
    let base = unsafe { &*cheapest };
    #[allow(clippy::cast_precision_loss)]
    let n = base.rows;
    let sort_scan_cost = cost::self_scan_cost_fp64_aware(
        n,
        1, // single sort key column
        cost::device_limits().gpu_op_cost_sort,
        uses_fp64,
    );
    let startup_cost = base.startup_cost + cost::GPU_LAUNCH_OVERHEAD;
    // Honest cost — no cost_multiplier. Let add_path() decide.
    let total_cost = base.total_cost + sort_scan_cost;

    // Build the sort key descriptor.
    let sort_key = SortKeyDesc {
        attno: var_attno,
        sort_op,
        collation: ec.ec_collation,
        nulls_first: pk.pk_nulls_first,
    };

    // SAFETY: Allocating via palloc, building valid CustomPath.
    unsafe {
        let cpath = create_custom_path(
            rel,
            cheapest,
            startup_cost,
            total_cost,
            n,
            custom_scan::scan_path_methods(),
        );

        // Set pathkeys so PG knows our output is already sorted.
        (*cpath).path.pathkeys = sort_pathkeys;

        // Serialize [fn_oid=0, target_attno=0, accel_strategy=GpuSort,
        //            num_sort_keys, attno, sort_op, collation, nulls_first]
        // into custom_private. make_custom_scan_plan reads this and detects
        // the Sort strategy from accel_strategy.
        let mut priv_list: *mut List = std::ptr::null_mut();
        priv_list = lappend(priv_list, pg_sys::makeInteger(0).cast()); // fn_oid
        priv_list = lappend(priv_list, pg_sys::makeInteger(0).cast()); // target_attno
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(registry::AccelStrategy::GpuSort as i32).cast(),
        );
        // Sort key data: [num_keys, attno, sort_op, collation, nulls_first]
        priv_list = lappend(priv_list, pg_sys::makeInteger(1).cast());
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(i32::from(sort_key.attno)).cast(),
        );
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(u32::from(sort_key.sort_op) as i32).cast(),
        );
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(u32::from(sort_key.collation) as i32).cast(),
        );
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(i32::from(sort_key.nulls_first)).cast(),
        );
        // Serialize limit_tuples for top-k sort optimization.
        // 0 or negative means no limit.
        #[allow(clippy::cast_possible_truncation)]
        let limit_int = if limit_tuples > 0.0 {
            limit_tuples as i32
        } else {
            0
        };
        priv_list = lappend(priv_list, pg_sys::makeInteger(limit_int).cast());

        // Serialize self_scan_relid for VectorizedScan. When the child is a
        // plain SeqScan on a base relation, we can bypass ExecProcNode and
        // scan the heap directly.
        let self_scan_relid = if cheapest.is_null() {
            0
        } else {
            // SAFETY: cheapest is the child path; check if it's a simple
            // Path on a base relation (i.e., parent is a baserel).
            let parent = (*cheapest).parent;
            if !parent.is_null()
                && (*parent).rtekind == pg_sys::RTEKind::RTE_RELATION
                && (*parent).relid > 0
            {
                (*parent).relid as i32
            } else {
                0
            }
        };
        priv_list = lappend(priv_list, pg_sys::makeInteger(self_scan_relid).cast());
        (*cpath).custom_private = priv_list;

        if !super::add_gpu_path_with_resident_proof(
            "rel_pathlist_sort_no_resident_pipeline",
            rel,
            cpath,
            custom_scan::resident_proof_default_for_strategy(custom_scan::GpuStrategy::Sort),
        ) {
            return;
        }

        pgrx::debug1!(
            "pg_accel: injected GpuSort scan path, attno={}, rows={}",
            var_attno,
            rows
        );
    }

    // Mirror into partial pathlist so `Gather Merge ∘ Parallel Sort` can pick
    // up the GPU sort. Each parallel-safe child (e.g. Parallel Seq Scan) gets
    // its own parallel-safe CustomPath wrapper with the same sort pathkeys so
    // PG can emit a Gather Merge on top.
    // SAFETY: rel_ref.partial_pathlist is a valid List (possibly null).
    unsafe {
        inject_gpu_sort_partial_paths(
            rel,
            rel_ref.partial_pathlist,
            sort_pathkeys,
            &sort_key,
            limit_tuples,
            uses_fp64,
        );
    }
}

/// Inject a parallel-safe GpuSort CustomPath for every entry in
/// `partial_pathlist`. Sets `pathkeys` so PG can place a `Gather Merge` on
/// top and preserve the sort order across workers.
///
/// # Safety
///
/// All pointers must be valid planner-provided args.
#[allow(clippy::too_many_arguments)]
unsafe fn inject_gpu_sort_partial_paths(
    rel: *mut RelOptInfo,
    partial_pathlist: *mut List,
    sort_pathkeys: *mut List,
    sort_key: &SortKeyDesc,
    limit_tuples: f64,
    uses_fp64: bool,
) {
    if partial_pathlist.is_null() {
        return;
    }
    // SAFETY: partial_pathlist is a valid List.
    let n = unsafe { pg_sys::list_length(partial_pathlist) };
    if n == 0 {
        return;
    }

    for i in 0..n {
        // SAFETY: i < list_length.
        let child = unsafe { pg_sys::list_nth(partial_pathlist, i).cast::<Path>() };
        if child.is_null() {
            continue;
        }
        // SAFETY: child is a valid Path node.
        let child_ref = unsafe { &*child };
        if !child_ref.parallel_safe {
            continue;
        }

        // Per-worker cost: workers see fewer rows, so use the child's own row
        // estimate (already partitioned by PG's parallel cost model).
        let per_worker_n = child_ref.rows;
        let sort_scan_cost = cost::self_scan_cost_fp64_aware(
            per_worker_n,
            1,
            cost::device_limits().gpu_op_cost_sort,
            uses_fp64,
        );
        let startup_cost = child_ref.startup_cost + cost::GPU_LAUNCH_OVERHEAD;
        let total_cost = child_ref.total_cost + sort_scan_cost;

        // SAFETY: palloc-based construction + explicit parallel field override.
        unsafe {
            let cpath = create_custom_path(
                rel,
                child,
                startup_cost,
                total_cost,
                per_worker_n,
                custom_scan::scan_path_methods(),
            );

            // Output is sorted per worker; pathkeys enable Gather Merge.
            (*cpath).path.pathkeys = sort_pathkeys;
            (*cpath).path.parallel_aware = false;
            (*cpath).path.parallel_safe = true;
            (*cpath).path.parallel_workers = child_ref.parallel_workers.max(1);

            // Same custom_private layout as the non-partial variant.
            let mut priv_list: *mut List = std::ptr::null_mut();
            priv_list = lappend(priv_list, pg_sys::makeInteger(0).cast()); // fn_oid
            priv_list = lappend(priv_list, pg_sys::makeInteger(0).cast()); // target_attno
            priv_list = lappend(
                priv_list,
                pg_sys::makeInteger(registry::AccelStrategy::GpuSort as i32).cast(),
            );
            // Sort key data: [num_keys, attno, sort_op, collation, nulls_first]
            priv_list = lappend(priv_list, pg_sys::makeInteger(1).cast());
            priv_list = lappend(
                priv_list,
                pg_sys::makeInteger(i32::from(sort_key.attno)).cast(),
            );
            priv_list = lappend(
                priv_list,
                pg_sys::makeInteger(u32::from(sort_key.sort_op) as i32).cast(),
            );
            priv_list = lappend(
                priv_list,
                pg_sys::makeInteger(u32::from(sort_key.collation) as i32).cast(),
            );
            priv_list = lappend(
                priv_list,
                pg_sys::makeInteger(i32::from(sort_key.nulls_first)).cast(),
            );
            // limit_tuples (0 = no limit).
            #[allow(clippy::cast_possible_truncation)]
            let limit_int = if limit_tuples > 0.0 {
                limit_tuples as i32
            } else {
                0
            };
            priv_list = lappend(priv_list, pg_sys::makeInteger(limit_int).cast());

            // self_scan_relid detection mirrors the non-partial branch: a
            // plain Path on a base relation is the only shape the vectorized
            // scan supports. Parallel Seq Scan is still a T_Path with a
            // relation parent, so this holds here too.
            let self_scan_relid = {
                let parent = (*child).parent;
                if !parent.is_null()
                    && (*parent).rtekind == pg_sys::RTEKind::RTE_RELATION
                    && (*parent).relid > 0
                {
                    (*parent).relid as i32
                } else {
                    0
                }
            };
            priv_list = lappend(priv_list, pg_sys::makeInteger(self_scan_relid).cast());
            (*cpath).custom_private = priv_list;

            if !super::add_gpu_partial_path_with_resident_proof(
                "rel_pathlist_partial_sort_no_resident_pipeline",
                rel,
                cpath,
                custom_scan::resident_proof_default_for_strategy(custom_scan::GpuStrategy::Sort),
            ) {
                continue;
            }

            pgrx::debug1!(
                "pg_accel: injected GpuSort PARTIAL path, attno={}, rows={}",
                sort_key.attno,
                per_worker_n as usize
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GPU_SORT_MAX_PATHKEYS, GpuExprTemplateAdmission, SortShape, classify_sort_shape,
        gpuexpr_template_type_supported_for, heap_topk_sort_candidate, min_max_rewrite_shape,
        postgis_distance_function_name, postgis_geometry_constructor_function_name,
        postgis_spatial_predicate_name, sort_topk_backend_supported,
        unregistered_spatial_shape_decline_reason,
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

    #[test]
    fn metal_topk_backend_stays_planner_declined_until_kernel_fixed() {
        assert!(!sort_topk_backend_supported(b"metal"));
        assert!(sort_topk_backend_supported(b"cuda"));
        assert!(sort_topk_backend_supported(b"hip"));
    }

    #[test]
    fn gpuexpr_template_type_gate_keeps_int8_fused_aggregate_only() {
        assert!(gpuexpr_template_type_supported_for(
            pg_sys::INT4OID,
            GpuExprTemplateAdmission::RowFilter
        ));
        assert!(gpuexpr_template_type_supported_for(
            pg_sys::FLOAT4OID,
            GpuExprTemplateAdmission::RowFilter
        ));
        assert!(gpuexpr_template_type_supported_for(
            pg_sys::FLOAT8OID,
            GpuExprTemplateAdmission::RowFilter
        ));
        assert!(!gpuexpr_template_type_supported_for(
            pg_sys::INT8OID,
            GpuExprTemplateAdmission::RowFilter
        ));
        assert!(gpuexpr_template_type_supported_for(
            pg_sys::INT8OID,
            GpuExprTemplateAdmission::FusedFilterAgg
        ));
        assert_eq!(
            super::super::int8_template_const_to_f64(9_007_199_254_740_992),
            Some(9_007_199_254_740_992.0)
        );
        assert!(super::super::int8_template_const_to_f64(9_007_199_254_740_993).is_none());
        assert!(super::super::template_f64_const_fits_exact_i64(42.0));
        assert!(!super::super::template_f64_const_fits_exact_i64(42.5));
        assert!(!super::super::template_f64_const_fits_exact_i64(f64::NAN));
    }

    #[test]
    fn unregistered_spatial_predicate_reason_is_stable() {
        assert_eq!(
            super::super::RejectionReason::SpatialNoRegisteredGpuPredicate.stats_key(),
            "spatial_no_registered_gpu_predicate"
        );
        assert!(postgis_spatial_predicate_name("st_intersects"));
        assert!(postgis_spatial_predicate_name("st_dwithin"));
        assert!(!postgis_spatial_predicate_name("st_buffer"));
        assert!(postgis_distance_function_name("st_distance"));
        assert!(!postgis_distance_function_name("st_dwithin"));
        assert!(postgis_geometry_constructor_function_name("st_buffer"));
        assert!(postgis_geometry_constructor_function_name("st_union"));
        assert!(postgis_geometry_constructor_function_name(
            "st_intersection"
        ));
        assert!(!postgis_geometry_constructor_function_name("st_area"));
    }

    #[test]
    fn unregistered_spatial_shape_marks_prepared_geometry_gap_after_cost_gates() {
        let limits = DeviceLimits::cpu_only();

        let reason = unregistered_spatial_shape_decline_reason(
            limits.gpu_spatial_min_vertices,
            limits.gpu_min_rows,
            1.0,
            &limits,
        );

        assert_eq!(
            reason.stats_key(),
            "spatial_prepared_geometry_not_available"
        );
    }

    #[test]
    fn unregistered_spatial_shape_keeps_generic_reason_for_unknown_geometry() {
        let limits = DeviceLimits::cpu_only();

        let reason =
            unregistered_spatial_shape_decline_reason(0, limits.gpu_min_rows, 1.0, &limits);

        assert_eq!(reason.stats_key(), "spatial_no_registered_gpu_predicate");
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

    /// The GpuSort partial-path injector must be early-exit-safe on a null
    /// `partial_pathlist` — PG passes null when parallelism is disabled
    /// for this rel, and we must not dereference it. Generic GpuExpr no
    /// longer has a standalone partial-path injector.
    ///
    /// We can't construct a real `RelOptInfo` in a unit test (dozens of
    /// PG-internal fields), but we can confirm the null-list short-circuit
    /// doesn't touch `rel`. Passing `rel = null_mut()` is invariant-breaking
    /// for `create_custom_path`, but we short-circuit BEFORE calling it.
    ///
    /// This test intentionally exercises only the null-list branch; any
    /// non-null list requires a live planner and belongs in `#[pg_test]`.
    #[test]
    fn inject_partial_paths_null_list_is_safe() {
        use super::inject_gpu_sort_partial_paths;
        use crate::engine::executor::sort::SortKeyDesc;
        use pgrx::pg_sys;

        let sort_key = SortKeyDesc {
            attno: 1,
            sort_op: pg_sys::Oid::INVALID,
            collation: pg_sys::Oid::INVALID,
            nulls_first: false,
        };
        // SAFETY: same — early exit on null list.
        unsafe {
            inject_gpu_sort_partial_paths(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &sort_key,
                0.0,
                false,
            );
        }
    }
}
