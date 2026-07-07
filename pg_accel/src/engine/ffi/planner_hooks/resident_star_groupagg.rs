//! Planner recognizer for generic resident one-dimension star group aggregates.

use std::ffi::CStr;

use pgrx::pg_sys::{self, List, NodeTag, PlannerInfo, RelOptInfo};

use super::resident_groupagg_path::{
    ResidentGroupAggPathShape, inject_childless_resident_groupagg_path,
};
use super::{find_cheapest_path, find_equi_join_key, rel_rows_estimate};
use crate::engine::executor::agg::AggOp;
use crate::engine::executor::olap::{
    OlapAggSpec, ResidentGroupAggLogicalSpec, ResidentStarDimGroupedF64Spec,
};
use crate::engine::expr_compiler::opcode;
use crate::engine::olap_cache::{self, ResidentStarDimGroupAggCacheShape};
use crate::engine::residency::ResidentOperatorStage;
use crate::engine::stats;

const RESIDENT_STAR_GROUPAGG_MAX_DIM_KEYS: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VarRef {
    varno: pg_sys::Index,
    attno: i32,
}

#[derive(Debug, Clone, Copy)]
struct VarConstPredicate {
    var: VarRef,
    cmp_opcode: u16,
    const_value: f64,
}

#[derive(Debug, Clone, Copy)]
struct VarVarEquality {
    left: VarRef,
    right: VarRef,
}

struct StarJoinCandidate {
    fact_rel_oid: pg_sys::Oid,
    dim_rel_oid: pg_sys::Oid,
    fact_key_attno: i32,
    dim_key_attno: i32,
}

#[derive(Debug, Clone, Copy)]
struct StarTarget {
    group_var: VarRef,
    value_var: VarRef,
    has_count_star: bool,
}

/// Try to inject a childless resident star group aggregate path.
///
/// # Safety
///
/// Planner pointers must be valid for the current `UPPERREL_GROUP_AGG` hook.
pub(super) unsafe fn try_inject(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
    output_rel: *mut RelOptInfo,
) -> bool {
    let Some(spec) = (unsafe { recognize(root, input_rel) }) else {
        return false;
    };

    if olap_cache::resident_star_dim_groupagg_cache_shape() != Some(spec.shape) {
        let rows_est = rel_rows_estimate(input_rel).unwrap_or(0);
        stats::increment_planner_rejected("recognized_resident_star_groupagg_no_cache", rows_est);
        stats::record_planner_fast_decline("upper_paths_resident_star_groupagg_no_cache");
        pgrx::debug1!(
            "pg_accel: upper_paths resident star groupagg decline: recognized shape but \
             backend-local resident star cache is not loaded or has different source columns"
        );
        return true;
    }

    let rows_est = rel_rows_estimate(input_rel).unwrap_or(0);
    let dim_key_count = olap_cache::resident_star_dim_groupagg_cache_dim_key_count();
    if dim_key_count > RESIDENT_STAR_GROUPAGG_MAX_DIM_KEYS {
        stats::increment_planner_rejected("hashjoin_build_side_too_large", dim_key_count as u64);
        stats::record_planner_fast_decline("upper_paths_resident_star_groupagg_dim_too_large");
        pgrx::debug1!(
            "pg_accel: upper_paths resident star groupagg decline: dim_key_count={} exceeds \
             max_dim_keys={} for generic compact+sort path",
            dim_key_count,
            RESIDENT_STAR_GROUPAGG_MAX_DIM_KEYS
        );
        return true;
    }
    unsafe {
        inject_childless_resident_groupagg_path(
            output_rel,
            rows_est,
            ResidentGroupAggPathShape {
                context: "upper_paths_resident_star_groupagg",
                olap_spec: OlapAggSpec::ResidentStarDimGroupedF64(spec),
                agg_op: AggOp::Sum,
                result_oid: pg_sys::FLOAT8OID,
                output_rows: if output_rel.is_null() {
                    1.0
                } else {
                    (*output_rel).rows.max(1.0)
                },
                cost_per_row: 0.00003,
                stages: vec![
                    ResidentOperatorStage::Scan,
                    ResidentOperatorStage::Join,
                    ResidentOperatorStage::Expression,
                    ResidentOperatorStage::GroupedAggregate,
                    ResidentOperatorStage::FinalMaterialization,
                ],
                device_columns: 5,
                has_filter: true,
            },
        );
    }
    true
}

unsafe fn recognize(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
) -> Option<ResidentStarDimGroupedF64Spec> {
    if root.is_null() || unsafe { (*root).parse }.is_null() || input_rel.is_null() {
        pgrx::debug1!("pg_accel: resident star groupagg miss: missing planner input");
        return None;
    }
    let query = unsafe { &*(*root).parse };
    if !query.hasAggs
        || query.groupClause.is_null()
        || unsafe { pg_sys::list_length(query.groupClause) } != 1
    {
        pgrx::debug1!(
            "pg_accel: resident star groupagg miss: query is not a single-group aggregate"
        );
        return None;
    }

    let Some(target) = (unsafe { recognize_target(query) }) else {
        pgrx::debug1!(
            "pg_accel: resident star groupagg miss: target list is not group_var, sum(value_var)[, count(*)]"
        );
        return None;
    };
    let group_var = target.group_var;
    let value_var = target.value_var;
    if group_var.varno == value_var.varno || group_var.attno <= 0 || value_var.attno <= 0 {
        pgrx::debug1!(
            "pg_accel: resident star groupagg miss: group/value vars are not fact-dimension refs"
        );
        return None;
    }
    let Some(candidate) =
        (unsafe { find_star_join_candidate(input_rel, query, value_var, group_var) })
    else {
        pgrx::debug1!("pg_accel: resident star groupagg miss: no fact-dimension equality join key");
        return None;
    };
    let mut predicates = unsafe { query_var_const_predicates(query) };
    unsafe {
        collect_base_rel_var_const_predicates(
            root,
            &[value_var.varno, group_var.varno],
            &mut predicates,
        );
    }
    let fact_predicate = predicates
        .iter()
        .copied()
        .find(|predicate| predicate.var == value_var)
        .unwrap_or(VarConstPredicate {
            var: value_var,
            cmp_opcode: opcode::ALWAYS_TRUE,
            const_value: 0.0,
        });
    let dim_predicate = predicates
        .iter()
        .copied()
        .find(|predicate| {
            predicate.var.varno == group_var.varno
                && predicate.var.attno != group_var.attno
                && predicate.var.attno != candidate.dim_key_attno
        })
        .unwrap_or(VarConstPredicate {
            var: group_var,
            cmp_opcode: opcode::ALWAYS_TRUE,
            const_value: 0.0,
        });
    if target.has_count_star {
        pgrx::debug1!("pg_accel: resident star groupagg recognized optional COUNT(*) lane");
    }

    let logical = ResidentGroupAggLogicalSpec::for_star_dim_grouped_f64();
    Some(ResidentStarDimGroupedF64Spec {
        shape: ResidentStarDimGroupAggCacheShape {
            fact_rel_oid: candidate.fact_rel_oid,
            dim_rel_oid: candidate.dim_rel_oid,
            fact_key_attno: candidate.fact_key_attno,
            fact_value_attno: value_var.attno,
            dim_key_attno: candidate.dim_key_attno,
            dim_group_attno: group_var.attno,
            dim_filter_attno: dim_predicate.var.attno,
            fact_value_cmp_opcode: fact_predicate.cmp_opcode,
            fact_value_cmp_const: fact_predicate.const_value,
            dim_filter_cmp_opcode: dim_predicate.cmp_opcode,
            dim_filter_cmp_const: dim_predicate.const_value,
        },
        logical,
    })
}

unsafe fn recognize_target(query: &pg_sys::Query) -> Option<StarTarget> {
    let group_var = unsafe { single_group_var(query) }?;
    if query.targetList.is_null() {
        return None;
    }
    let len = unsafe { pg_sys::list_length(query.targetList) };
    let mut nonjunk = Vec::new();
    for i in 0..len {
        let tle = unsafe { pg_sys::list_nth(query.targetList, i).cast::<pg_sys::TargetEntry>() };
        if tle.is_null() || unsafe { (*tle).resjunk } || unsafe { (*tle).expr }.is_null() {
            continue;
        }
        nonjunk.push(unsafe { (*tle).expr.cast::<pg_sys::Node>() });
    }
    if !(2..=3).contains(&nonjunk.len()) || unsafe { extract_var(nonjunk[0]) } != Some(group_var) {
        return None;
    }
    let value_var = unsafe { aggref_direct_var(nonjunk[1], b"sum") }?;
    let has_count_star = if nonjunk.len() == 3 {
        unsafe { aggref_count_star(nonjunk[2]) }?
    } else {
        false
    };
    Some(StarTarget {
        group_var,
        value_var,
        has_count_star,
    })
}

unsafe fn single_group_var(query: &pg_sys::Query) -> Option<VarRef> {
    let sc = unsafe { pg_sys::list_nth(query.groupClause, 0).cast::<pg_sys::SortGroupClause>() };
    if sc.is_null() {
        return None;
    }
    unsafe { target_var_for_sortgroupref(query.targetList, (*sc).tleSortGroupRef) }
}

unsafe fn target_var_for_sortgroupref(tlist: *mut List, sgref: pg_sys::Index) -> Option<VarRef> {
    if tlist.is_null() {
        return None;
    }
    let len = unsafe { pg_sys::list_length(tlist) };
    for i in 0..len {
        let tle = unsafe { pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>() };
        if tle.is_null() || unsafe { (*tle).ressortgroupref } != sgref {
            continue;
        }
        return unsafe { extract_var((*tle).expr.cast::<pg_sys::Node>()) };
    }
    None
}

unsafe fn extract_var(mut node: *mut pg_sys::Node) -> Option<VarRef> {
    while !node.is_null() {
        match unsafe { (*node).type_ } {
            NodeTag::T_Var => {
                let var = node.cast::<pg_sys::Var>();
                return Some(VarRef {
                    varno: pg_sys::Index::try_from(unsafe { (*var).varno }).ok()?,
                    attno: i32::from(unsafe { (*var).varattno }),
                });
            }
            NodeTag::T_RelabelType => {
                node = unsafe {
                    (*node.cast::<pg_sys::RelabelType>())
                        .arg
                        .cast::<pg_sys::Node>()
                };
            }
            _ => return None,
        }
    }
    None
}

unsafe fn aggref_direct_var(node: *mut pg_sys::Node, name: &[u8]) -> Option<VarRef> {
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_Aggref {
        return None;
    }
    let agg = node.cast::<pg_sys::Aggref>();
    let agg_ref = unsafe { &*agg };
    if agg_ref.aggstar
        || !agg_ref.aggdistinct.is_null()
        || !agg_ref.aggorder.is_null()
        || !agg_ref.aggfilter.is_null()
        || !unsafe { agg_name_matches(agg_ref.aggfnoid, name) }
        || agg_ref.args.is_null()
        || unsafe { pg_sys::list_length(agg_ref.args) } != 1
    {
        return None;
    }
    let arg_tle = unsafe { pg_sys::list_nth(agg_ref.args, 0).cast::<pg_sys::TargetEntry>() };
    if arg_tle.is_null() || unsafe { (*arg_tle).expr }.is_null() {
        return None;
    }
    unsafe { extract_var((*arg_tle).expr.cast::<pg_sys::Node>()) }
}

unsafe fn agg_name_matches(aggfnoid: pg_sys::Oid, expected: &[u8]) -> bool {
    let name = unsafe { pg_sys::get_func_name(aggfnoid) };
    !name.is_null() && unsafe { CStr::from_ptr(name) }.to_bytes() == expected
}

unsafe fn aggref_count_star(node: *mut pg_sys::Node) -> Option<bool> {
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_Aggref {
        return None;
    }
    let agg = node.cast::<pg_sys::Aggref>();
    let agg_ref = unsafe { &*agg };
    if !agg_ref.aggstar
        || !agg_ref.aggdistinct.is_null()
        || !agg_ref.aggorder.is_null()
        || !agg_ref.aggfilter.is_null()
        || !unsafe { agg_name_matches(agg_ref.aggfnoid, b"count") }
    {
        return None;
    }
    Some(agg_ref.args.is_null() || unsafe { pg_sys::list_length(agg_ref.args) } == 0)
}

unsafe fn find_star_join_candidate(
    input_rel: *mut RelOptInfo,
    query: &pg_sys::Query,
    fact_value_var: VarRef,
    group_var: VarRef,
) -> Option<StarJoinCandidate> {
    if let Some(candidate) =
        unsafe { find_star_join_candidate_in_query(query, fact_value_var.varno, group_var.varno) }
    {
        return Some(candidate);
    }

    let path = unsafe { find_cheapest_path((*input_rel).pathlist) };
    unsafe {
        find_star_join_candidate_in_path(path, 0, query, fact_value_var.varno, group_var.varno)
    }
}

unsafe fn find_star_join_candidate_in_query(
    query: &pg_sys::Query,
    fact_varno: pg_sys::Index,
    dim_varno: pg_sys::Index,
) -> Option<StarJoinCandidate> {
    let mut equalities = Vec::new();
    if !query.jointree.is_null() {
        unsafe {
            collect_var_var_equalities(query.jointree.cast::<pg_sys::Node>(), &mut equalities);
        }
    }
    for equality in equalities {
        let (fact_key_attno, dim_key_attno) =
            if equality.left.varno == fact_varno && equality.right.varno == dim_varno {
                (equality.left.attno, equality.right.attno)
            } else if equality.left.varno == dim_varno && equality.right.varno == fact_varno {
                (equality.right.attno, equality.left.attno)
            } else {
                continue;
            };
        if fact_key_attno <= 0 || dim_key_attno <= 0 {
            continue;
        }
        return Some(StarJoinCandidate {
            fact_rel_oid: unsafe { relation_oid_from_rtable(query.rtable, fact_varno) }?,
            dim_rel_oid: unsafe { relation_oid_from_rtable(query.rtable, dim_varno) }?,
            fact_key_attno,
            dim_key_attno,
        });
    }
    None
}

unsafe fn find_star_join_candidate_in_path(
    path: *mut pg_sys::Path,
    depth: u8,
    query: &pg_sys::Query,
    fact_varno: pg_sys::Index,
    dim_varno: pg_sys::Index,
) -> Option<StarJoinCandidate> {
    if path.is_null() || depth > 8 {
        return None;
    }
    let fact_varno_i32 = i32::try_from(fact_varno).ok()?;
    let dim_varno_i32 = i32::try_from(dim_varno).ok()?;
    match unsafe { (*path.cast::<pg_sys::Node>()).type_ } {
        NodeTag::T_AggPath => unsafe {
            let agg = path.cast::<pg_sys::AggPath>();
            find_star_join_candidate_in_path(
                (*agg).subpath,
                depth + 1,
                query,
                fact_varno,
                dim_varno,
            )
        },
        NodeTag::T_HashPath | NodeTag::T_NestPath | NodeTag::T_MergePath => unsafe {
            let jp = path.cast::<pg_sys::JoinPath>();
            if (*jp).jointype != pg_sys::JoinType::JOIN_INNER {
                return None;
            }
            let outer_path = (*jp).outerjoinpath;
            let inner_path = (*jp).innerjoinpath;
            if outer_path.is_null() || inner_path.is_null() {
                return None;
            }
            let outer_rel = (*outer_path).parent;
            let inner_rel = (*inner_path).parent;
            let equi = find_equi_join_key((*jp).joinrestrictinfo, outer_rel, inner_rel)?;
            let fact_key_attno = if equi.outer_varno == fact_varno_i32 {
                equi.outer_attno
            } else if equi.inner_varno == fact_varno_i32 {
                equi.inner_attno
            } else {
                return None;
            };
            let dim_key_attno = if equi.outer_varno == dim_varno_i32 {
                equi.outer_attno
            } else if equi.inner_varno == dim_varno_i32 {
                equi.inner_attno
            } else {
                return None;
            };
            Some(StarJoinCandidate {
                fact_rel_oid: relation_oid_from_rtable(query.rtable, fact_varno)?,
                dim_rel_oid: relation_oid_from_rtable(query.rtable, dim_varno)?,
                fact_key_attno,
                dim_key_attno,
            })
        },
        NodeTag::T_GatherPath => unsafe {
            let gather = path.cast::<pg_sys::GatherPath>();
            find_star_join_candidate_in_path(
                (*gather).subpath,
                depth + 1,
                query,
                fact_varno,
                dim_varno,
            )
        },
        NodeTag::T_GatherMergePath => unsafe {
            let gather = path.cast::<pg_sys::GatherMergePath>();
            find_star_join_candidate_in_path(
                (*gather).subpath,
                depth + 1,
                query,
                fact_varno,
                dim_varno,
            )
        },
        NodeTag::T_ProjectionPath => unsafe {
            let projection = path.cast::<pg_sys::ProjectionPath>();
            find_star_join_candidate_in_path(
                (*projection).subpath,
                depth + 1,
                query,
                fact_varno,
                dim_varno,
            )
        },
        NodeTag::T_ProjectSetPath => unsafe {
            let project_set = path.cast::<pg_sys::ProjectSetPath>();
            find_star_join_candidate_in_path(
                (*project_set).subpath,
                depth + 1,
                query,
                fact_varno,
                dim_varno,
            )
        },
        NodeTag::T_MaterialPath => unsafe {
            let material = path.cast::<pg_sys::MaterialPath>();
            find_star_join_candidate_in_path(
                (*material).subpath,
                depth + 1,
                query,
                fact_varno,
                dim_varno,
            )
        },
        NodeTag::T_MemoizePath => unsafe {
            let memoize = path.cast::<pg_sys::MemoizePath>();
            find_star_join_candidate_in_path(
                (*memoize).subpath,
                depth + 1,
                query,
                fact_varno,
                dim_varno,
            )
        },
        NodeTag::T_UniquePath => unsafe {
            let unique = path.cast::<pg_sys::UniquePath>();
            find_star_join_candidate_in_path(
                (*unique).subpath,
                depth + 1,
                query,
                fact_varno,
                dim_varno,
            )
        },
        NodeTag::T_SortPath => unsafe {
            let sort = path.cast::<pg_sys::SortPath>();
            find_star_join_candidate_in_path(
                (*sort).subpath,
                depth + 1,
                query,
                fact_varno,
                dim_varno,
            )
        },
        NodeTag::T_IncrementalSortPath => unsafe {
            let sort = path.cast::<pg_sys::IncrementalSortPath>();
            find_star_join_candidate_in_path(
                (*sort).spath.subpath,
                depth + 1,
                query,
                fact_varno,
                dim_varno,
            )
        },
        NodeTag::T_GroupPath => unsafe {
            let group = path.cast::<pg_sys::GroupPath>();
            find_star_join_candidate_in_path(
                (*group).subpath,
                depth + 1,
                query,
                fact_varno,
                dim_varno,
            )
        },
        NodeTag::T_UpperUniquePath => unsafe {
            let unique = path.cast::<pg_sys::UpperUniquePath>();
            find_star_join_candidate_in_path(
                (*unique).subpath,
                depth + 1,
                query,
                fact_varno,
                dim_varno,
            )
        },
        NodeTag::T_WindowAggPath => unsafe {
            let window = path.cast::<pg_sys::WindowAggPath>();
            find_star_join_candidate_in_path(
                (*window).subpath,
                depth + 1,
                query,
                fact_varno,
                dim_varno,
            )
        },
        _ => None,
    }
}

unsafe fn query_var_const_predicates(query: &pg_sys::Query) -> Vec<VarConstPredicate> {
    let mut predicates = Vec::new();
    if !query.jointree.is_null() {
        unsafe {
            collect_var_const_predicates(query.jointree.cast::<pg_sys::Node>(), &mut predicates);
        }
    }
    predicates
}

unsafe fn collect_base_rel_var_const_predicates(
    root: *mut PlannerInfo,
    relids: &[pg_sys::Index],
    predicates: &mut Vec<VarConstPredicate>,
) {
    if root.is_null() {
        return;
    }
    let root_ref = unsafe { &*root };
    if root_ref.simple_rel_array.is_null() {
        return;
    }

    for &relid in relids {
        let Ok(rel_index) = i32::try_from(relid) else {
            continue;
        };
        if rel_index <= 0 || rel_index >= root_ref.simple_rel_array_size {
            continue;
        }
        let rel = unsafe { *root_ref.simple_rel_array.offset(rel_index as isize) };
        if rel.is_null()
            || unsafe { (*rel).reloptkind } != pg_sys::RelOptKind::RELOPT_BASEREL
            || unsafe { (*rel).baserestrictinfo }.is_null()
        {
            continue;
        }
        unsafe {
            collect_restrictinfo_var_const_predicates((*rel).baserestrictinfo, predicates);
        }
    }
}

unsafe fn collect_restrictinfo_var_const_predicates(
    restrictinfo_list: *mut List,
    predicates: &mut Vec<VarConstPredicate>,
) {
    if restrictinfo_list.is_null() {
        return;
    }
    let len = unsafe { pg_sys::list_length(restrictinfo_list) };
    for i in 0..len {
        let ri = unsafe { pg_sys::list_nth(restrictinfo_list, i).cast::<pg_sys::RestrictInfo>() };
        if ri.is_null() || unsafe { (*ri).clause }.is_null() {
            continue;
        }
        unsafe { collect_var_const_predicates((*ri).clause.cast::<pg_sys::Node>(), predicates) };
    }
}

unsafe fn collect_var_var_equalities(
    node: *mut pg_sys::Node,
    equalities: &mut Vec<VarVarEquality>,
) {
    if node.is_null() {
        return;
    }
    match unsafe { (*node).type_ } {
        NodeTag::T_FromExpr => unsafe {
            let from_expr = node.cast::<pg_sys::FromExpr>();
            collect_var_var_equalities((*from_expr).quals.cast::<pg_sys::Node>(), equalities);
            let fromlist = (*from_expr).fromlist;
            let len = pg_sys::list_length(fromlist);
            for i in 0..len {
                collect_var_var_equalities(
                    pg_sys::list_nth(fromlist, i).cast::<pg_sys::Node>(),
                    equalities,
                );
            }
        },
        NodeTag::T_JoinExpr => unsafe {
            let join_expr = node.cast::<pg_sys::JoinExpr>();
            collect_var_var_equalities((*join_expr).larg.cast::<pg_sys::Node>(), equalities);
            collect_var_var_equalities((*join_expr).rarg.cast::<pg_sys::Node>(), equalities);
            collect_var_var_equalities((*join_expr).quals.cast::<pg_sys::Node>(), equalities);
        },
        NodeTag::T_BoolExpr => unsafe {
            let bool_expr = node.cast::<pg_sys::BoolExpr>();
            if (*bool_expr).boolop != pg_sys::BoolExprType::AND_EXPR {
                return;
            }
            let args = (*bool_expr).args;
            let len = pg_sys::list_length(args);
            for i in 0..len {
                collect_var_var_equalities(
                    pg_sys::list_nth(args, i).cast::<pg_sys::Node>(),
                    equalities,
                );
            }
        },
        NodeTag::T_OpExpr => unsafe {
            if let Some(equality) = var_var_equality_from_opexpr(node.cast::<pg_sys::OpExpr>()) {
                equalities.push(equality);
            }
        },
        _ => {}
    }
}

unsafe fn collect_var_const_predicates(
    node: *mut pg_sys::Node,
    predicates: &mut Vec<VarConstPredicate>,
) {
    if node.is_null() {
        return;
    }
    match unsafe { (*node).type_ } {
        NodeTag::T_FromExpr => unsafe {
            let from_expr = node.cast::<pg_sys::FromExpr>();
            collect_var_const_predicates((*from_expr).quals.cast::<pg_sys::Node>(), predicates);
            let fromlist = (*from_expr).fromlist;
            let len = pg_sys::list_length(fromlist);
            for i in 0..len {
                collect_var_const_predicates(
                    pg_sys::list_nth(fromlist, i).cast::<pg_sys::Node>(),
                    predicates,
                );
            }
        },
        NodeTag::T_JoinExpr => unsafe {
            let join_expr = node.cast::<pg_sys::JoinExpr>();
            collect_var_const_predicates((*join_expr).larg.cast::<pg_sys::Node>(), predicates);
            collect_var_const_predicates((*join_expr).rarg.cast::<pg_sys::Node>(), predicates);
            collect_var_const_predicates((*join_expr).quals.cast::<pg_sys::Node>(), predicates);
        },
        NodeTag::T_BoolExpr => unsafe {
            let bool_expr = node.cast::<pg_sys::BoolExpr>();
            if (*bool_expr).boolop != pg_sys::BoolExprType::AND_EXPR {
                return;
            }
            let args = (*bool_expr).args;
            let len = pg_sys::list_length(args);
            for i in 0..len {
                collect_var_const_predicates(
                    pg_sys::list_nth(args, i).cast::<pg_sys::Node>(),
                    predicates,
                );
            }
        },
        NodeTag::T_OpExpr => unsafe {
            if let Some(predicate) = var_const_predicate_from_opexpr(node.cast::<pg_sys::OpExpr>())
            {
                predicates.push(predicate);
            }
        },
        _ => {}
    }
}

unsafe fn var_var_equality_from_opexpr(op: *mut pg_sys::OpExpr) -> Option<VarVarEquality> {
    if op.is_null() {
        return None;
    }
    let args = unsafe { (*op).args };
    if args.is_null() || unsafe { pg_sys::list_length(args) } != 2 {
        return None;
    }
    let op_name = unsafe { pg_sys::get_opname((*op).opno) };
    if op_name.is_null() || unsafe { CStr::from_ptr(op_name) }.to_bytes() != b"=" {
        return None;
    }
    let left = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
    let right = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };
    let left = unsafe { extract_var(left) }?;
    let right = unsafe { extract_var(right) }?;
    (left.varno != right.varno && left.attno > 0 && right.attno > 0)
        .then_some(VarVarEquality { left, right })
}

unsafe fn var_const_predicate_from_opexpr(op: *mut pg_sys::OpExpr) -> Option<VarConstPredicate> {
    if op.is_null() {
        return None;
    }
    let args = unsafe { (*op).args };
    if args.is_null() || unsafe { pg_sys::list_length(args) } != 2 {
        return None;
    }
    let op_name = unsafe { pg_sys::get_opname((*op).opno) };
    if op_name.is_null() {
        return None;
    }
    let op_name = unsafe { CStr::from_ptr(op_name) }.to_str().ok()?;
    let cmp_opcode = crate::engine::expr_compiler::pg_cmp_op_to_opcode(op_name)?;
    let left = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
    let right = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };
    if let Some((var, value)) = unsafe { extract_var_const(left, right) } {
        return Some(VarConstPredicate {
            var,
            cmp_opcode,
            const_value: value,
        });
    }
    let (var, value) = unsafe { extract_var_const(right, left) }?;
    Some(VarConstPredicate {
        var,
        cmp_opcode: crate::engine::expr_compiler::flip_cmp_opcode(cmp_opcode),
        const_value: value,
    })
}

unsafe fn extract_var_const(
    var_node: *mut pg_sys::Node,
    const_node: *mut pg_sys::Node,
) -> Option<(VarRef, f64)> {
    let var = unsafe { extract_var(var_node) }?;
    if var.attno <= 0 {
        return None;
    }
    let value = unsafe { extract_const_f64(const_node) }?;
    (!value.is_nan()).then_some((var, value))
}

unsafe fn extract_const_f64(mut node: *mut pg_sys::Node) -> Option<f64> {
    while !node.is_null() && unsafe { (*node).type_ } == NodeTag::T_RelabelType {
        node = unsafe {
            (*node.cast::<pg_sys::RelabelType>())
                .arg
                .cast::<pg_sys::Node>()
        };
    }
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_Const {
        return None;
    }
    let cst = node.cast::<pg_sys::Const>();
    if unsafe { (*cst).constisnull } {
        return None;
    }
    let datum = unsafe { (*cst).constvalue };
    let typid = u32::from(unsafe { (*cst).consttype });
    match typid {
        21 => Some(f64::from(datum.value() as i16)),
        23 => Some(f64::from(datum.value() as i32)),
        20 => Some(datum.value() as i64 as f64),
        700 => Some(f64::from(f32::from_bits(datum.value() as u32))),
        701 => Some(f64::from_bits(datum.value() as u64)),
        1082 => Some(f64::from(datum.value() as i32)),
        _ => None,
    }
}

unsafe fn relation_oid_from_rtable(
    rtable: *mut pg_sys::List,
    varno: pg_sys::Index,
) -> Option<pg_sys::Oid> {
    if rtable.is_null() || varno == 0 {
        return None;
    }
    let index = i32::try_from(varno).ok()?.checked_sub(1)?;
    if index >= unsafe { pg_sys::list_length(rtable) } {
        return None;
    }
    let rte = unsafe { pg_sys::list_nth(rtable, index).cast::<pg_sys::RangeTblEntry>() };
    if rte.is_null()
        || unsafe { (*rte).rtekind } != pg_sys::RTEKind::RTE_RELATION
        || unsafe { (*rte).relid } == pg_sys::InvalidOid
    {
        return None;
    }
    Some(unsafe { (*rte).relid })
}
