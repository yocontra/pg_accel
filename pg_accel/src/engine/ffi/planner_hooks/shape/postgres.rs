//! PostgreSQL-node adapter for the neutral shape builder.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::c_void;
use std::num::NonZeroU32;

use pgrx::pg_sys::{self, Node, NodeTag};

use crate::engine::ffi::syscache::{PostgisCatalogIdentity, PostgisSpatialFunction};
use crate::engine::spec::{
    AggregateKind, AggregateOutput, AggregateSource, BinaryMeasureOp, ColumnRef, FilterSpec,
    GroupKeySource, MaskKind, MeasureExpr, ScalarRange, ScalarValue, SpatialOperand,
    SpatialPredicateKind, SpatialValueKind, SpatialValueMetadata,
};

use super::{
    AggregateExpr, EquiJoin, InputProjection, OutputMetadata, PlannerColumn, PlannerGroupKey,
    RelationResidency, RelationShape, ShapeDecline, ShapeInput, ShapeModifiers,
};

const MAX_QUAL_CONJUNCTS: usize = 64;

pub(super) fn bounded_qual_conjunct_indices(
    count: usize,
) -> Result<std::ops::Range<usize>, ShapeDecline> {
    if count == 0 || count > MAX_QUAL_CONJUNCTS {
        return Err(ShapeDecline::UnsupportedPredicate);
    }
    Ok(0..count)
}

#[derive(Default)]
struct ExpressionInventory {
    aggregate_nodes: Vec<*mut pg_sys::Aggref>,
    join_nodes: Vec<*mut pg_sys::JoinExpr>,
    from_nodes: Vec<*mut pg_sys::FromExpr>,
    vars: Vec<*mut pg_sys::Var>,
    saw_window_function: bool,
    saw_sublink: bool,
}

/// A single PostgreSQL walker callback is used for the complete query tree.
/// PostgreSQL owns recursion and therefore automatically follows node kinds
/// added to expression trees; this pass only records nodes relevant to the
/// shape contract instead of maintaining a brittle allowed-node whitelist.
#[pgrx::pg_guard]
unsafe extern "C-unwind" fn inventory_walker(node: *mut Node, context: *mut c_void) -> bool {
    if node.is_null() || context.is_null() {
        return false;
    }
    // SAFETY: query_tree_walker_impl receives this exact inventory pointer
    // and invokes the callback synchronously while it remains live.
    let inventory = unsafe { &mut *context.cast::<ExpressionInventory>() };
    // SAFETY: PostgreSQL only invokes a tree walker with a valid Node.
    match unsafe { (*node).type_ } {
        NodeTag::T_Aggref => inventory.aggregate_nodes.push(node.cast()),
        NodeTag::T_JoinExpr => inventory.join_nodes.push(node.cast()),
        NodeTag::T_Var => inventory.vars.push(node.cast()),
        NodeTag::T_WindowFunc => inventory.saw_window_function = true,
        NodeTag::T_SubLink => inventory.saw_sublink = true,
        _ => {}
    }
    // SAFETY: PostgreSQL passed a valid node from the current query tree and
    // the callback/context remain live until query_tree_walker_impl returns.
    let stopped =
        unsafe { pg_sys::expression_tree_walker_impl(node, Some(inventory_walker), context) };
    // PostgreSQL distributes jointree qualifiers child-first. Pulled-up
    // subqueries can leave nested FromExprs even with a single base relation.
    // SAFETY: node and context remain live across the synchronous recursion.
    if unsafe { (*node).type_ } == NodeTag::T_FromExpr {
        unsafe { &mut *context.cast::<ExpressionInventory>() }
            .from_nodes
            .push(node.cast());
    }
    stopped
}

unsafe fn inventory_query(query: *mut pg_sys::Query) -> ExpressionInventory {
    let mut inventory = ExpressionInventory::default();
    let flags = pg_sys::QTW_IGNORE_RT_SUBQUERIES
        | pg_sys::QTW_IGNORE_CTE_SUBQUERIES
        | pg_sys::QTW_IGNORE_JOINALIASES
        | pg_sys::QTW_EXAMINE_SORTGROUP;
    // SAFETY: query is the planner-owned Query and inventory outlives the
    // synchronous tree walk. The callback delegates recursion to PostgreSQL.
    unsafe {
        pg_sys::query_tree_walker_impl(
            query,
            Some(inventory_walker),
            (&raw mut inventory).cast(),
            flags as i32,
        );
    }
    inventory
}

pub(super) fn list_len(list: *mut pg_sys::List) -> usize {
    if list.is_null() {
        0
    } else {
        // SAFETY: non-null List pointers passed here belong to the active
        // planner tree and remain valid for the synchronous extraction.
        usize::try_from(unsafe { pg_sys::list_length(list) }).unwrap_or(usize::MAX)
    }
}

pub(super) unsafe fn list_item<T>(list: *mut pg_sys::List, index: usize) -> Option<*mut T> {
    if index >= list_len(list) {
        return None;
    }
    let index = i32::try_from(index).ok()?;
    // SAFETY: the bounds check above proves index is within this valid List.
    let item = unsafe { pg_sys::list_nth(list, index) }.cast::<T>();
    (!item.is_null()).then_some(item)
}

unsafe fn rte_for_varno(
    query: &pg_sys::Query,
    varno: pg_sys::Index,
) -> Option<*mut pg_sys::RangeTblEntry> {
    if varno == 0 {
        return None;
    }
    let index = usize::try_from(varno - 1).ok()?;
    // SAFETY: rte_for_varno has converted PostgreSQL's one-based varno to a
    // checked zero-based List index.
    unsafe { list_item(query.rtable, index) }
}

pub(super) unsafe fn direct_var(node: *mut Node, query: &pg_sys::Query) -> Option<PlannerColumn> {
    if node.is_null() {
        return None;
    }
    // SAFETY: node is a planner expression. PostgreSQL returns either the
    // same node or an inner expression from planner-owned memory.
    let mut node = unsafe { pg_sys::strip_implicit_coercions(node) };
    let mut explicit_collation = None;
    // SAFETY: strip_implicit_coercions returned a planner-owned Node.
    if !node.is_null() && unsafe { (*node).type_ } == NodeTag::T_CollateExpr {
        // SAFETY: the NodeTag check proves the concrete node layout.
        let collate = unsafe { &*node.cast::<pg_sys::CollateExpr>() };
        explicit_collation = Some(collate.collOid);
        // SAFETY: CollateExpr::arg is a planner-owned expression.
        node = unsafe { pg_sys::strip_implicit_coercions(collate.arg.cast()) };
    }
    // SAFETY: node is non-null planner memory when its tag is read.
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_Var {
        return None;
    }
    // SAFETY: the NodeTag check above proves the Var layout.
    let var = unsafe { &*node.cast::<pg_sys::Var>() };
    if var.varlevelsup != 0 || var.varattno <= 0 || var.varno <= 0 {
        return None;
    }
    let varno = pg_sys::Index::try_from(var.varno).ok()?;
    // SAFETY: var.varno comes from this Query and rte_for_varno bounds-checks.
    let rte = unsafe { rte_for_varno(query, varno) }?;
    // SAFETY: rte_for_varno returned a non-null RTE in planner-owned memory.
    let rte = unsafe { &*rte };
    if rte.rtekind != pg_sys::RTEKind::RTE_RELATION || rte.relid == pg_sys::InvalidOid {
        return None;
    }
    // Catalog type identity must match the analyzed Var. A mismatch means a
    // stale or synthetic Var that cannot address the selected base column.
    // SAFETY: catalog lookup runs on the backend thread for a valid relation
    // OID and positive attribute number from the analyzed Var.
    let catalog_type = unsafe { pg_sys::get_atttype(rte.relid, var.varattno) };
    if catalog_type == pg_sys::InvalidOid || catalog_type != var.vartype {
        return None;
    }
    let collation_oid = explicit_collation.unwrap_or(var.varcollid);
    let collatable_text = matches!(
        var.vartype,
        pg_sys::TEXTOID | pg_sys::VARCHAROID | pg_sys::BPCHAROID
    );
    let collation_is_deterministic = !collatable_text
        || (collation_oid != pg_sys::InvalidOid
            // SAFETY: non-invalid collation OID came from the analyzed Var or
            // its explicit CollateExpr and is looked up on the backend thread.
            && unsafe { pg_sys::get_collation_isdeterministic(collation_oid) });
    Some(PlannerColumn {
        varno,
        column: ColumnRef {
            relation_oid: u32::from(rte.relid),
            attno: i32::from(var.varattno),
            type_oid: u32::from(var.vartype),
        },
        type_modifier: var.vartypmod,
        collation_oid: u32::from(collation_oid),
        collation_is_deterministic,
    })
}

unsafe fn direct_const(node: *mut Node) -> Option<pg_sys::Const> {
    if node.is_null() {
        return None;
    }
    // SAFETY: node is a planner expression and PostgreSQL owns the returned
    // inner expression for the same planner lifetime.
    let node = unsafe { pg_sys::strip_implicit_coercions(node) };
    // SAFETY: the non-null returned pointer is a valid planner Node.
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_Const {
        return None;
    }
    // Const is a bindgen POD value; copying avoids manufacturing a Rust
    // lifetime for planner-owned memory.
    // SAFETY: the NodeTag check proves the Const layout; Const is Copy.
    Some(unsafe { *node.cast::<pg_sys::Const>() })
}

#[derive(Clone)]
struct H3ParentGroup {
    key: PlannerGroupKey,
    source_column: PlannerColumn,
}

/// Recognize only the catalog-proved `h3_cell_to_parent(h3index, int4)`
/// grouping expression with a direct base-table cell and a plan-time
/// resolution constant.
unsafe fn h3_parent_group_key(node: *mut Node, query: &pg_sys::Query) -> Option<H3ParentGroup> {
    if node.is_null() {
        return None;
    }
    // SAFETY: node is planner-owned and coercion stripping preserves that
    // lifetime. Explicit CollateExpr is intentionally not stripped here.
    let node = unsafe { pg_sys::strip_implicit_coercions(node) };
    // SAFETY: the short-circuit proves node is non-null before reading its tag;
    // coercion stripping returned a node in the same planner-owned expression.
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_FuncExpr {
        return None;
    }
    // SAFETY: the NodeTag check proves the FuncExpr layout.
    let function = unsafe { &*node.cast::<pg_sys::FuncExpr>() };
    if function.funcid == pg_sys::InvalidOid
        || function.funcretset
        || function.funcvariadic
        || function.funccollid != pg_sys::InvalidOid
        || function.inputcollid != pg_sys::InvalidOid
        || list_len(function.args) != 2
    {
        return None;
    }
    // SAFETY: planner extraction runs on PostgreSQL's main backend thread.
    let catalog = unsafe { crate::engine::ffi::syscache::resolve_h3_catalog() }.ok()?;
    if function.funcid != catalog.parent_fn_oid
        || function.funcresulttype != catalog.type_oid
        // SAFETY: the canonical type/function pair is resolved above; this
        // validates the analyzed function OID against its exact signature.
        || unsafe {
            crate::engine::ffi::syscache::validate_h3_parent_function(
                function.funcid,
                catalog.type_oid,
            )
        }
        .is_err()
    {
        return None;
    }
    // SAFETY: the two-item length check bounds both argument reads.
    let cell_node = unsafe { list_item::<Node>(function.args, 0) }?;
    // SAFETY: cell_node belongs to this analyzed Query.
    let source_column = unsafe { direct_var(cell_node, query) }?;
    if pg_sys::Oid::from(source_column.column.type_oid) != catalog.type_oid
        || source_column.collation_oid != 0
    {
        return None;
    }
    // SAFETY: the two-item length check bounds the constant argument read.
    let resolution_node = unsafe { list_item::<Node>(function.args, 1) }?;
    // SAFETY: resolution_node belongs to the active planner expression.
    let resolution = unsafe { direct_const(resolution_node) }?;
    if resolution.consttype != pg_sys::INT4OID
        || resolution.consttypmod != -1
        || resolution.constcollid != pg_sys::InvalidOid
        || resolution.constlen != 4
        || !resolution.constbyval
        || resolution.constisnull
    {
        return None;
    }
    // SAFETY: the exact non-NULL INT4 Const contract was checked above.
    let resolution = unsafe { pg_sys::DatumGetInt32(resolution.constvalue) };
    if !(0..=15).contains(&resolution)
        // SAFETY: node is the analyzed function expression.
        || unsafe { pg_sys::exprType(node) } != catalog.type_oid
        // SAFETY: h3index is noncollatable and the canonical expression has no collation.
        || unsafe { pg_sys::exprCollation(node) } != pg_sys::InvalidOid
    {
        return None;
    }
    Some(H3ParentGroup {
        key: PlannerGroupKey::Expression {
            source: GroupKeySource::H3CellToParent {
                cell: source_column.column,
                resolution,
            },
            type_oid: u32::from(catalog.type_oid),
            collation_oid: 0,
            collation_is_deterministic: true,
        },
        source_column,
    })
}

unsafe fn default_equality_operator(type_oid: pg_sys::Oid) -> Option<pg_sys::Oid> {
    // SAFETY: type_oid comes from a catalog-validated analyzed expression and
    // the type cache lookup runs on PostgreSQL's main backend thread.
    let entry = unsafe { pg_sys::lookup_type_cache(type_oid, pg_sys::TYPECACHE_EQ_OPR as i32) };
    if entry.is_null() {
        return None;
    }
    // SAFETY: lookup_type_cache returned a live backend TypeCacheEntry with
    // TYPECACHE_EQ_OPR populated for this call.
    let equality = unsafe { (*entry).eq_opr };
    (equality != pg_sys::InvalidOid).then_some(equality)
}

pub(super) fn is_ordinary_hash_equality(
    actual: pg_sys::Oid,
    ordinary: pg_sys::Oid,
    hashable: bool,
) -> bool {
    actual != pg_sys::InvalidOid && actual == ordinary && hashable
}

unsafe fn target_entry_expr(
    list: *mut pg_sys::List,
    index: usize,
) -> Option<(*mut pg_sys::TargetEntry, *mut Node)> {
    // SAFETY: list_item performs the bounds/null checks for this planner List.
    let target = unsafe { list_item::<pg_sys::TargetEntry>(list, index) }?;
    // SAFETY: target is a valid TargetEntry returned from the planner list.
    let expr = unsafe { (*target).expr.cast::<Node>() };
    (!expr.is_null()).then_some((target, expr))
}

fn checked_cardinalities(cardinalities: impl IntoIterator<Item = f64>) -> Option<Vec<u64>> {
    cardinalities
        .into_iter()
        .map(|cardinality| {
            if !cardinality.is_finite() || cardinality < 0.0 || cardinality >= u64::MAX as f64 {
                return None;
            }
            Some(cardinality.ceil().max(1.0) as u64)
        })
        .collect()
}

/// Estimate each dictionary separately: joint ndistinct cannot size a dense
/// Cartesian radix domain when keys are correlated. No row-count cap applies
/// to the resulting product. Root and its query must remain planner-owned.
pub(super) unsafe fn estimate_dense_key_cardinalities(
    root: *mut pg_sys::PlannerInfo,
    input_rows: u64,
) -> Option<Vec<u64>> {
    if root.is_null() {
        return None;
    }
    // SAFETY: caller guarantees root's planner lifetime; null parse is rejected.
    let query = unsafe { (*root).parse.as_ref() }?;
    let mut cardinalities = Vec::with_capacity(list_len(query.groupClause));
    for index in 0..list_len(query.groupClause) {
        // SAFETY: index is in the live query's group-clause list.
        let clause = unsafe { list_item::<pg_sys::SortGroupClause>(query.groupClause, index) }?;
        // SAFETY: clause and targetList belong to the same analyzed Query.
        let expression = unsafe { pg_sys::get_sortgroupclause_expr(clause, query.targetList) };
        if expression.is_null() {
            return None;
        }
        // SAFETY: lappend creates a temporary one-element pointer list; the
        // expression itself remains query-owned. Only list cells are freed.
        let estimate = unsafe {
            let expressions = pg_sys::lappend(std::ptr::null_mut(), expression.cast());
            let estimate = pg_sys::estimate_num_groups(
                root,
                expressions,
                input_rows as f64,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            pg_sys::list_free(expressions);
            estimate
        };
        cardinalities.push(estimate);
    }
    checked_cardinalities(cardinalities)
}

unsafe fn group_keys(query: &pg_sys::Query) -> Result<Vec<PlannerGroupKey>, ShapeDecline> {
    let mut groups = Vec::with_capacity(list_len(query.groupClause));
    for index in 0..list_len(query.groupClause) {
        // SAFETY: index is bounded by list_len(groupClause).
        let clause = unsafe { list_item::<pg_sys::SortGroupClause>(query.groupClause, index) }
            .ok_or(ShapeDecline::UnsupportedGroupExpression)?;
        // SAFETY: clause is a valid SortGroupClause from groupClause.
        let clause = unsafe { &*clause };
        let sort_ref = clause.tleSortGroupRef;
        let mut found: Option<PlannerGroupKey> = None;
        for target_index in 0..list_len(query.targetList) {
            // SAFETY: target_index is bounded by list_len(targetList).
            let (target, expr) = unsafe { target_entry_expr(query.targetList, target_index) }
                .ok_or(ShapeDecline::UnsupportedGroupExpression)?;
            // SAFETY: target_entry_expr returned a valid TargetEntry.
            if unsafe { (*target).ressortgroupref } == sort_ref {
                // SAFETY: expr belongs to this Query's target list.
                let parsed = if let Some(column) = unsafe { direct_var(expr, query) } {
                    Some((
                        PlannerGroupKey::Column(column),
                        column.column.type_oid,
                        column.collation_oid,
                    ))
                } else {
                    // SAFETY: expr belongs to this analyzed target list.
                    unsafe { h3_parent_group_key(expr, query) }.map(|group| {
                        let PlannerGroupKey::Expression {
                            type_oid,
                            collation_oid,
                            ..
                        } = &group.key
                        else {
                            unreachable!("H3 parser always returns an expression key")
                        };
                        (group.key.clone(), *type_oid, *collation_oid)
                    })
                };
                if let Some((group, type_oid, collation_oid)) = parsed {
                    // SAFETY: expr is a non-null analyzed target expression.
                    let expression_collation = unsafe { pg_sys::exprCollation(expr) };
                    // SAFETY: the type is catalog-validated by direct_var or
                    // the exact H3 catalog proof.
                    let ordinary =
                        unsafe { default_equality_operator(pg_sys::Oid::from(type_oid)) }
                            .ok_or(ShapeDecline::UnsupportedGroupExpression)?;
                    // SAFETY: clause.eqop and the validated type OID came from
                    // this analyzed SortGroupClause.
                    let hashable = unsafe {
                        pg_sys::op_hashjoinable(clause.eqop, pg_sys::Oid::from(type_oid))
                    };
                    if pg_sys::Oid::from(collation_oid) != expression_collation
                        || !clause.hashable
                        || !is_ordinary_hash_equality(clause.eqop, ordinary, hashable)
                    {
                        return Err(ShapeDecline::UnsupportedGroupExpression);
                    }
                    found = Some(group);
                }
                break;
            }
        }
        let group = found.ok_or(ShapeDecline::UnsupportedGroupExpression)?;
        if groups.contains(&group) {
            return Err(ShapeDecline::UnsupportedGroupExpression);
        }
        groups.push(group);
    }
    Ok(groups)
}

pub(super) fn classify_aggregate(aggregate_oid: u32) -> Option<AggregateKind> {
    match aggregate_oid {
        pg_sys::F_SUM_INT2
        | pg_sys::F_SUM_INT8
        | pg_sys::F_SUM_INT4
        | pg_sys::F_SUM_FLOAT4
        | pg_sys::F_SUM_FLOAT8
        | pg_sys::F_SUM_NUMERIC => Some(AggregateKind::Sum),
        pg_sys::F_COUNT_ANY | pg_sys::F_COUNT_ => Some(AggregateKind::Count),
        pg_sys::F_MIN_INT2
        | pg_sys::F_MIN_INT4
        | pg_sys::F_MIN_INT8
        | pg_sys::F_MIN_FLOAT4
        | pg_sys::F_MIN_FLOAT8
        | pg_sys::F_MIN_DATE
        | pg_sys::F_MIN_TIME
        | pg_sys::F_MIN_TIMETZ
        | pg_sys::F_MIN_TIMESTAMP
        | pg_sys::F_MIN_TIMESTAMPTZ => Some(AggregateKind::Min),
        pg_sys::F_MAX_INT2
        | pg_sys::F_MAX_INT4
        | pg_sys::F_MAX_INT8
        | pg_sys::F_MAX_FLOAT4
        | pg_sys::F_MAX_FLOAT8
        | pg_sys::F_MAX_DATE
        | pg_sys::F_MAX_TIME
        | pg_sys::F_MAX_TIMETZ
        | pg_sys::F_MAX_TIMESTAMP
        | pg_sys::F_MAX_TIMESTAMPTZ => Some(AggregateKind::Max),
        pg_sys::F_AVG_INT2
        | pg_sys::F_AVG_INT8
        | pg_sys::F_AVG_INT4
        | pg_sys::F_AVG_FLOAT4
        | pg_sys::F_AVG_FLOAT8
        | pg_sys::F_AVG_NUMERIC => Some(AggregateKind::Avg),
        pg_sys::F_STDDEV_FLOAT8 | pg_sys::F_STDDEV_SAMP_FLOAT8 => Some(AggregateKind::StddevSamp),
        _ => None,
    }
}

pub(super) fn needs_numeric_accumulator(aggregate_oid: u32) -> bool {
    matches!(aggregate_oid, pg_sys::F_AVG_INTERVAL)
}

fn classify_binary_function(function_oid: u32) -> Option<BinaryMeasureOp> {
    match function_oid {
        pg_sys::F_INT4PL => Some(BinaryMeasureOp::Add),
        pg_sys::F_INT2MUL
        | pg_sys::F_INT4MUL
        | pg_sys::F_INT8MUL
        | pg_sys::F_FLOAT4MUL
        | pg_sys::F_FLOAT8MUL => Some(BinaryMeasureOp::Mul),
        pg_sys::F_INT2MI
        | pg_sys::F_INT4MI
        | pg_sys::F_INT8MI
        | pg_sys::F_FLOAT4MI
        | pg_sys::F_FLOAT8MI => Some(BinaryMeasureOp::Sub),
        _ => None,
    }
}

unsafe fn first_aggregate_argument(aggregate: &pg_sys::Aggref) -> Option<*mut Node> {
    if list_len(aggregate.args) != 1 {
        return None;
    }
    // SAFETY: the single aggregate argument list entry is a TargetEntry in
    // PostgreSQL's analyzed Aggref representation.
    let target = unsafe { list_item::<pg_sys::TargetEntry>(aggregate.args, 0) }?;
    // SAFETY: target is a valid planner-owned TargetEntry.
    let expr = unsafe { (*target).expr.cast::<Node>() };
    (!expr.is_null()).then_some(expr)
}

unsafe fn measure_expression(
    aggregate: &pg_sys::Aggref,
    query: &pg_sys::Query,
    kind: AggregateKind,
) -> Result<MeasureExpr, ShapeDecline> {
    // Preserve the released raw-column and two-column binary representations.
    // SAFETY: caller supplies the active analyzed aggregate and Query.
    match unsafe { legacy_measure_expression(aggregate, query, kind) } {
        Ok(expression) => return Ok(expression),
        Err(ShapeDecline::UnsupportedMeasureExpression) => {}
        Err(error) => return Err(error),
    }
    // Lower the original argument, not the coercion-stripped legacy probe.
    // SAFETY: aggregate arguments share the active planner tree's lifetime.
    let argument = unsafe { first_aggregate_argument(aggregate) }
        .ok_or(ShapeDecline::UnsupportedMeasureExpression)?;
    // SAFETY: lowerer reads planner-simplified nodes synchronously on the
    // backend thread and retains only validated logical identities/programs.
    let lowered = unsafe { super::postgres_scalar::lower_scalar_expression(argument, query) }?;
    Ok(lowered.into_measure_expression())
}

unsafe fn legacy_measure_expression(
    aggregate: &pg_sys::Aggref,
    query: &pg_sys::Query,
    kind: AggregateKind,
) -> Result<MeasureExpr, ShapeDecline> {
    if kind == AggregateKind::Count && aggregate.aggstar {
        if !aggregate.args.is_null() && list_len(aggregate.args) != 0 {
            return Err(ShapeDecline::UnsupportedMeasureExpression);
        }
        return Ok(MeasureExpr::CountStar);
    }
    // SAFETY: aggregate is a validated Aggref from the active query tree.
    let argument = unsafe { first_aggregate_argument(aggregate) }
        .ok_or(ShapeDecline::UnsupportedMeasureExpression)?;
    // SAFETY: argument and query share the active planner lifetime.
    if let Some(column) = unsafe { direct_var(argument, query) } {
        return Ok(MeasureExpr::Column(column.column));
    }
    // SAFETY: argument is a planner expression and PostgreSQL owns any
    // returned inner expression for the same planner lifetime.
    let argument = unsafe { pg_sys::strip_implicit_coercions(argument) };
    // SAFETY: the non-null returned pointer is a valid planner Node.
    if argument.is_null() || unsafe { (*argument).type_ } != NodeTag::T_OpExpr {
        return Err(ShapeDecline::UnsupportedMeasureExpression);
    }
    // SAFETY: the NodeTag check proves the OpExpr layout.
    let operation = unsafe { &*argument.cast::<pg_sys::OpExpr>() };
    if list_len(operation.args) != 2 {
        return Err(ShapeDecline::UnsupportedMeasureExpression);
    }
    // SAFETY: the two-item length check bounds both argument reads.
    let left_node = unsafe { list_item::<Node>(operation.args, 0) }
        .ok_or(ShapeDecline::UnsupportedMeasureExpression)?;
    // SAFETY: the two-item length check bounds both argument reads.
    let right_node = unsafe { list_item::<Node>(operation.args, 1) }
        .ok_or(ShapeDecline::UnsupportedMeasureExpression)?;
    // SAFETY: both nodes and query belong to the active planner tree.
    let left = unsafe { direct_var(left_node, query) }
        .ok_or(ShapeDecline::UnsupportedMeasureExpression)?;
    // SAFETY: both nodes and query belong to the active planner tree.
    let right = unsafe { direct_var(right_node, query) }
        .ok_or(ShapeDecline::UnsupportedMeasureExpression)?;
    if left.column.type_oid != right.column.type_oid {
        return Err(ShapeDecline::UnsupportedMeasureExpression);
    }
    let function_oid = if operation.opfuncid == pg_sys::InvalidOid {
        // SAFETY: operation.opno is an analyzed operator OID and the lookup
        // runs on PostgreSQL's backend thread.
        unsafe { pg_sys::get_opcode(operation.opno) }
    } else {
        operation.opfuncid
    };
    let op = classify_binary_function(u32::from(function_oid))
        .ok_or(ShapeDecline::UnsupportedMeasureExpression)?;
    Ok(MeasureExpr::Binary {
        op,
        lhs: left.column,
        rhs: right.column,
    })
}

pub(super) unsafe fn parse_aggregate(
    node: *mut Node,
    query: &pg_sys::Query,
) -> Result<AggregateExpr, ShapeDecline> {
    // SAFETY: preserve the caller's live planner-tree contract without an H3 context.
    unsafe { parse_aggregate_with_h3_context(node, query, None) }
}

unsafe fn parse_aggregate_with_h3_context(
    node: *mut Node,
    query: &pg_sys::Query,
    h3_fact: Option<u32>,
) -> Result<AggregateExpr, ShapeDecline> {
    // SAFETY: node is a planner expression and PostgreSQL owns the returned
    // inner expression for the same planner lifetime.
    let node = unsafe { pg_sys::strip_implicit_coercions(node) };
    // SAFETY: the non-null returned pointer is a valid planner Node.
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_Aggref {
        return Err(ShapeDecline::UnsupportedProjection);
    }
    // SAFETY: the NodeTag check proves the Aggref layout.
    let aggregate = unsafe { &*node.cast::<pg_sys::Aggref>() };
    if aggregate.agglevelsup != 0
        || aggregate.aggvariadic
        || !aggregate.aggdirectargs.is_null()
        || !aggregate.aggorder.is_null()
        || !aggregate.aggdistinct.is_null()
    {
        return Err(ShapeDecline::UnsupportedAggregateModifier);
    }
    let aggregate_oid = u32::from(aggregate.aggfnoid);
    if needs_numeric_accumulator(aggregate_oid) {
        return Err(ShapeDecline::NumericAccumulatorUnavailable { aggregate_oid });
    }
    let kind = classify_aggregate(aggregate_oid)
        .ok_or(ShapeDecline::UnsupportedAggregate { aggregate_oid })?;
    // SAFETY: aggregate and query are from the same active planner tree.
    let expression = unsafe { measure_expression(aggregate, query, kind) }?;
    // SAFETY: aggfilter, when present, belongs to this analyzed Aggref and
    // shares the active Query's planner lifetime.
    let filter_node = aggregate.aggfilter.cast::<Node>();
    let direct_h3_filter = if kind == AggregateKind::Count
        && h3_fact.is_some()
        && !filter_node.is_null()
        // SAFETY: non-null aggregate FILTER belongs to this analyzed Aggref.
        && unsafe { (*filter_node).type_ } == NodeTag::T_Var
        && (expression == MeasureExpr::CountStar
            || matches!(expression, MeasureExpr::Column(column)
                if Some(column.relation_oid) == h3_fact
                    && matches!(column.type_oid, 16 | 21 | 23 | 20 | 700 | 701 | 1082 | 1114 | 1184)))
    {
        // SAFETY: exact Var node belongs to the same active query; no coercion stripping.
        unsafe { direct_var(filter_node, query) }
            .filter(|column| {
                Some(column.column.relation_oid) == h3_fact
                    && column.column.type_oid == u32::from(pg_sys::BOOLOID)
            })
            .map(|column| FilterSpec::Mask {
                input: column.column,
                kind: MaskKind::Sql,
            })
    } else {
        None
    };
    // SAFETY: FILTER and query share the active analyzed planner lifetime.
    let parsed_filter =
        direct_h3_filter.map_or_else(|| unsafe { aggregate_filter(filter_node, query) }, Ok);
    let (expression, filter) = match parsed_filter {
        Ok(filter) => (lower_filtered_count(expression, kind, &filter), filter),
        Err(ShapeDecline::UnsupportedAggregateModifier) => {
            // The built-in aggregate and argument shape were checked above.
            // Qualified strict INT4/DATE aggregates or Boolean COUNT can use
            // a lazy scalar producer: rejected rows yield NULL, never an
            // argument evaluation, while group activity stays unchanged.
            let argument = if expression == MeasureExpr::CountStar {
                None
            } else {
                // SAFETY: aggregate is the same validated, live Aggref.
                Some(
                    unsafe { first_aggregate_argument(aggregate) }
                        .ok_or(ShapeDecline::UnsupportedAggregateModifier)?,
                )
            };
            // SAFETY: predicate/argument belong to this simplified query;
            // no PostgreSQL pointers escape the synchronous lowerer.
            let lowered = unsafe {
                super::postgres_scalar::lower_filtered_scalar_expression(
                    aggregate.aggfilter.cast::<Node>(),
                    argument,
                    query,
                )
            }
            .map_err(|_| ShapeDecline::UnsupportedAggregateModifier)?;
            (lowered.into_measure_expression(), FilterSpec::None)
        }
        Err(error) => return Err(error),
    };
    Ok(AggregateExpr {
        expression,
        output: AggregateOutput {
            source: AggregateSource::Value,
            kind,
        },
        filter,
    })
}

fn lower_filtered_count(
    expression: MeasureExpr,
    kind: AggregateKind,
    filter: &FilterSpec,
) -> MeasureExpr {
    // Every accepted interval row has a non-NULL predicate input. Therefore
    // COUNT(*) FILTER (WHERE p(x)) is exactly COUNT(x) FILTER (WHERE p(x)).
    // Keep the filter: removing it would incorrectly count every non-NULL x.
    if expression == MeasureExpr::CountStar
        && kind == AggregateKind::Count
        && let FilterSpec::Ranges { input, ranges } = filter
        && input.type_oid == u32::from(pg_sys::INT4OID)
        && !ranges.is_empty()
    {
        MeasureExpr::Column(*input)
    } else {
        expression
    }
}

unsafe fn column_is_nullable(column: PlannerColumn) -> bool {
    // SAFETY: catalog lookup runs on the backend thread; relation OID and
    // attno came from a catalog-validated Var.
    let tuple = unsafe {
        pg_sys::SearchSysCache2(
            pg_sys::SysCacheIdentifier::ATTNUM as ::core::ffi::c_int,
            pg_sys::Oid::from(column.column.relation_oid).into(),
            pg_sys::Datum::from(i16::try_from(column.column.attno).unwrap_or(i16::MAX)),
        )
    };
    if tuple.is_null() {
        return true;
    }
    // SAFETY: tuple is a syscache-pinned pg_attribute HeapTuple.
    let nullable = unsafe {
        let attribute = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_attribute>();
        attribute.is_null() || !(*attribute).attnotnull
    };
    // SAFETY: releases the pin obtained by SearchSysCache2 above exactly once.
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    nullable
}

unsafe fn output_metadata(
    expression: *mut Node,
    source_type_oid: u32,
    nullable: bool,
) -> OutputMetadata {
    OutputMetadata {
        source_type_oid,
        // SAFETY: expression is a non-null analyzed target expression.
        result_type_oid: u32::from(unsafe { pg_sys::exprType(expression) }),
        // SAFETY: expression is a non-null analyzed target expression.
        result_typmod: unsafe { pg_sys::exprTypmod(expression) },
        // SAFETY: expression is a non-null analyzed target expression.
        result_collation_oid: u32::from(unsafe { pg_sys::exprCollation(expression) }),
        nullable,
    }
}

unsafe fn projections_and_aggregates(
    query: &pg_sys::Query,
    inventory: &ExpressionInventory,
    targets: *mut pg_sys::List,
    h3_fact: Option<u32>,
) -> Result<(Vec<InputProjection>, Vec<AggregateExpr>), ShapeDecline> {
    let mut projections = Vec::new();
    let mut aggregates = Vec::new();
    let mut direct_aggregate_nodes = Vec::new();
    for index in 0..list_len(targets) {
        // SAFETY: index is bounded by list_len(targetList).
        let (_, expression) = unsafe { target_entry_expr(targets, index) }
            .ok_or(ShapeDecline::UnsupportedProjection)?;
        // SAFETY: expression belongs to this Query target list.
        if let Some(group) = unsafe { direct_var(expression, query) } {
            projections.push(InputProjection::Group {
                key: PlannerGroupKey::Column(group),
                // SAFETY: expression/group came from the active target list;
                // the catalog lookup runs synchronously on the backend.
                output: unsafe {
                    output_metadata(expression, group.column.type_oid, column_is_nullable(group))
                },
            });
            continue;
        }
        // SAFETY: expression belongs to this Query target list.
        if let Some(group) = unsafe { h3_parent_group_key(expression, query) } {
            let source_type_oid = match &group.key {
                PlannerGroupKey::Expression { type_oid, .. } => *type_oid,
                PlannerGroupKey::Column(_) => {
                    unreachable!("H3 parser always returns an expression key")
                }
            };
            // SAFETY: the source is a catalog-validated direct base-table Var.
            let nullable = unsafe { column_is_nullable(group.source_column) };
            projections.push(InputProjection::Group {
                key: group.key,
                // SAFETY: expression is the active analyzed H3 expression.
                output: unsafe { output_metadata(expression, source_type_oid, nullable) },
            });
            continue;
        }
        // SAFETY: expression is planner-owned; the returned inner expression
        // has the same lifetime.
        let stripped = unsafe { pg_sys::strip_implicit_coercions(expression) };
        // SAFETY: stripped is a valid planner Node when non-null.
        if stripped.is_null() || unsafe { (*stripped).type_ } != NodeTag::T_Aggref {
            return Err(ShapeDecline::UnsupportedProjection);
        }
        let aggregate_index =
            u32::try_from(aggregates.len()).map_err(|_| ShapeDecline::UnsupportedProjection)?;
        // SAFETY: the NodeTag check proves stripped is an Aggref in query.
        let aggregate = if h3_fact.is_some() {
            // SAFETY: the NodeTag check proves this is an Aggref in query.
            unsafe { parse_aggregate_with_h3_context(stripped, query, h3_fact) }?
        } else {
            // SAFETY: preserve the original parser for non-H3 grouping.
            unsafe { parse_aggregate(stripped, query) }?
        };
        let nullable = aggregate.output.kind != AggregateKind::Count;
        let source_type_oid = match &aggregate.expression {
            MeasureExpr::CountStar => 0,
            MeasureExpr::Column(column) | MeasureExpr::Binary { lhs: column, .. } => {
                column.type_oid
            }
            MeasureExpr::StatsPair { value, rhs } => match aggregate.output.source {
                AggregateSource::Value => value.type_oid,
                AggregateSource::Rhs => rhs.type_oid,
            },
            MeasureExpr::Bytecode {
                result_type_oid, ..
            } => *result_type_oid,
        };
        aggregates.push(aggregate);
        projections.push(InputProjection::Aggregate {
            aggregate_index,
            // SAFETY: expression is the active non-junk target expression.
            output: unsafe { output_metadata(expression, source_type_oid, nullable) },
        });
        direct_aggregate_nodes.push(stripped);
    }
    if inventory.aggregate_nodes.iter().any(|aggregate| {
        !direct_aggregate_nodes.iter().any(|projected| {
            // HAVING owns separate Aggref nodes for the same aggregate.
            // Compare expressions, not allocation addresses; setrefs uses
            // the same equality when binding the CPU qual to scan outputs.
            // SAFETY: both nodes belong to this live planner Query.
            unsafe { pg_sys::equal(aggregate.cast(), (*projected).cast()) }
        })
    }) {
        return Err(ShapeDecline::UnsupportedProjection);
    }
    Ok((projections, aggregates))
}

/// Construct the internal scan layout without changing PostgreSQL's query or
/// final output. Junk grouping/sort keys and hidden HAVING aggregates are real
/// scan attributes; PostgreSQL projects them away after evaluating the qual.
///
/// # Safety
/// `query` must belong to the current live planner invocation.
pub unsafe fn aggregate_scan_target_list(
    query: &pg_sys::Query,
) -> Result<*mut pg_sys::List, ShapeDecline> {
    // SAFETY: targetList is a planner-owned List<TargetEntry>.
    let mut targets: *mut pg_sys::List =
        unsafe { pg_sys::copyObjectImpl(query.targetList.cast()).cast() };
    let mut inventory = ExpressionInventory::default();
    // SAFETY: the synchronous walker borrows only the live HAVING expression.
    unsafe { inventory_walker(query.havingQual, (&raw mut inventory).cast()) };
    for aggregate in inventory.aggregate_nodes {
        // SAFETY: all nodes are planner-owned; equality deduplicates Aggrefs.
        unsafe {
            if pg_sys::tlist_member(aggregate.cast(), targets).is_null() {
                let expression = pg_sys::copyObjectImpl(aggregate.cast()).cast();
                let target = pg_sys::makeTargetEntry(
                    expression,
                    (pg_sys::list_length(targets) + 1)
                        .try_into()
                        .map_err(|_| ShapeDecline::UnsupportedProjection)?,
                    std::ptr::null_mut(),
                    false,
                );
                targets = pg_sys::lappend(targets, target.cast());
            }
        }
    }
    // Qualified kernels put value measures before COUNT(*). SQL output order
    // must not dictate that device layout, especially when a value is hidden
    // in HAVING. Stable grouping preserves the order within each category.
    let mut entries: Vec<*mut pg_sys::TargetEntry> = (0..list_len(targets))
        .map(|index| {
            // SAFETY: targets contains copied or newly allocated TargetEntries.
            unsafe { pg_sys::list_nth(targets, index as i32).cast() }
        })
        .collect();
    entries.sort_by_key(|target| {
        // SAFETY: every target expression belongs to this planner context.
        unsafe {
            let expression = pg_sys::strip_implicit_coercions((**target).expr.cast());
            if !expression.is_null() && (*expression).type_ == NodeTag::T_Aggref {
                if (*expression.cast::<pg_sys::Aggref>()).aggstar {
                    2
                } else {
                    1
                }
            } else {
                0
            }
        }
    });
    targets = std::ptr::null_mut();
    // Internal attributes must be dense and must not be dropped as junk.
    for (index, target) in entries.into_iter().enumerate() {
        // SAFETY: targets contains copied or newly allocated TargetEntries.
        unsafe {
            (*target).resno = (index + 1)
                .try_into()
                .map_err(|_| ShapeDecline::UnsupportedProjection)?;
            (*target).resjunk = false;
            targets = pg_sys::lappend(targets, target.cast());
        }
    }
    Ok(targets)
}

/// A native qual may consume only materialized scan values. Stop at a matching
/// output expression rather than descending into its input Vars.
#[pgrx::pg_guard]
unsafe extern "C-unwind" fn unsupported_having_input(
    node: *mut Node,
    context: *mut c_void,
) -> bool {
    if node.is_null() {
        return false;
    }
    let targets = context.cast::<pg_sys::List>();
    // SAFETY: targets and node are planner-owned for this synchronous walk.
    let target = unsafe { pg_sys::tlist_member(node.cast(), targets) };
    // SAFETY: tlist_member returns null or a TargetEntry from targets.
    if !target.is_null() && !unsafe { (*target).resjunk } {
        return false;
    }
    // SAFETY: PostgreSQL supplies a live expression node.
    if matches!(unsafe { (*node).type_ }, NodeTag::T_Var | NodeTag::T_Aggref) {
        return true;
    }
    // SAFETY: the same planner-owned target list remains live during recursion.
    unsafe { pg_sys::expression_tree_walker_impl(node, Some(unsupported_having_input), context) }
}

fn scalar_value(constant: &pg_sys::Const) -> Result<ScalarValue, ShapeDecline> {
    if constant.constisnull {
        return Err(ShapeDecline::UnsupportedPredicate);
    }
    let type_oid = u32::from(constant.consttype);
    // SAFETY: each Datum conversion matches the Const's exact catalog type.
    let value = unsafe {
        match constant.consttype {
            pg_sys::BOOLOID => ScalarValue::Bool(pg_sys::DatumGetBool(constant.constvalue)),
            pg_sys::INT4OID => ScalarValue::I32(pg_sys::DatumGetInt32(constant.constvalue)),
            pg_sys::INT8OID => ScalarValue::I64(pg_sys::DatumGetInt64(constant.constvalue)),
            pg_sys::FLOAT4OID => ScalarValue::F32(pg_sys::DatumGetFloat4(constant.constvalue)),
            pg_sys::FLOAT8OID => ScalarValue::F64(pg_sys::DatumGetFloat8(constant.constvalue)),
            pg_sys::DATEOID => ScalarValue::Date(pg_sys::DatumGetDateADT(constant.constvalue)),
            pg_sys::TIMESTAMPOID => {
                ScalarValue::Timestamp(pg_sys::DatumGetTimestamp(constant.constvalue))
            }
            pg_sys::TIMESTAMPTZOID => {
                ScalarValue::TimestampTz(pg_sys::DatumGetTimestampTz(constant.constvalue))
            }
            _ => return Err(ShapeDecline::UnsupportedFilterType { type_oid }),
        }
    };
    Ok(value)
}

fn scalar_min(value: ScalarValue) -> ScalarValue {
    match value {
        ScalarValue::Bool(_) => ScalarValue::Bool(false),
        ScalarValue::I32(_) => ScalarValue::I32(i32::MIN),
        ScalarValue::I64(_) => ScalarValue::I64(i64::MIN),
        ScalarValue::F32(_) => ScalarValue::F32(f32::NEG_INFINITY),
        ScalarValue::F64(_) => ScalarValue::F64(f64::NEG_INFINITY),
        ScalarValue::Date(_) => ScalarValue::Date(i32::MIN),
        ScalarValue::Timestamp(_) => ScalarValue::Timestamp(i64::MIN),
        ScalarValue::TimestampTz(_) => ScalarValue::TimestampTz(i64::MIN),
    }
}

fn scalar_max(value: ScalarValue) -> ScalarValue {
    match value {
        ScalarValue::Bool(_) => ScalarValue::Bool(true),
        ScalarValue::I32(_) => ScalarValue::I32(i32::MAX),
        ScalarValue::I64(_) => ScalarValue::I64(i64::MAX),
        ScalarValue::F32(_) => ScalarValue::F32(f32::INFINITY),
        ScalarValue::F64(_) => ScalarValue::F64(f64::INFINITY),
        ScalarValue::Date(_) => ScalarValue::Date(i32::MAX),
        ScalarValue::Timestamp(_) => ScalarValue::Timestamp(i64::MAX),
        ScalarValue::TimestampTz(_) => ScalarValue::TimestampTz(i64::MAX),
    }
}

fn scalar_cmp(left: ScalarValue, right: ScalarValue) -> Option<std::cmp::Ordering> {
    match (left, right) {
        (ScalarValue::Bool(left), ScalarValue::Bool(right)) => left.partial_cmp(&right),
        (ScalarValue::I32(left), ScalarValue::I32(right)) => left.partial_cmp(&right),
        (ScalarValue::I64(left), ScalarValue::I64(right)) => left.partial_cmp(&right),
        (ScalarValue::F32(left), ScalarValue::F32(right)) => left.partial_cmp(&right),
        (ScalarValue::F64(left), ScalarValue::F64(right)) => left.partial_cmp(&right),
        (ScalarValue::Date(left), ScalarValue::Date(right)) => left.partial_cmp(&right),
        (ScalarValue::Timestamp(left), ScalarValue::Timestamp(right))
        | (ScalarValue::TimestampTz(left), ScalarValue::TimestampTz(right)) => {
            left.partial_cmp(&right)
        }
        _ => None,
    }
}

fn scalar_next(value: ScalarValue) -> Option<ScalarValue> {
    match value {
        ScalarValue::I32(value) => value.checked_add(1).map(ScalarValue::I32),
        ScalarValue::I64(value) => value.checked_add(1).map(ScalarValue::I64),
        ScalarValue::Date(value) => value.checked_add(1).map(ScalarValue::Date),
        ScalarValue::Timestamp(value) => value.checked_add(1).map(ScalarValue::Timestamp),
        ScalarValue::TimestampTz(value) => value.checked_add(1).map(ScalarValue::TimestampTz),
        _ => None,
    }
}

fn scalar_previous(value: ScalarValue) -> Option<ScalarValue> {
    match value {
        ScalarValue::I32(value) => value.checked_sub(1).map(ScalarValue::I32),
        ScalarValue::I64(value) => value.checked_sub(1).map(ScalarValue::I64),
        ScalarValue::Date(value) => value.checked_sub(1).map(ScalarValue::Date),
        ScalarValue::Timestamp(value) => value.checked_sub(1).map(ScalarValue::Timestamp),
        ScalarValue::TimestampTz(value) => value.checked_sub(1).map(ScalarValue::TimestampTz),
        _ => None,
    }
}

pub(super) fn range_for_strategy(
    value: ScalarValue,
    strategy: i32,
) -> Result<ScalarRange, ShapeDecline> {
    let less = pg_sys::BTLessStrategyNumber as i32;
    let less_equal = pg_sys::BTLessEqualStrategyNumber as i32;
    let equal = pg_sys::BTEqualStrategyNumber as i32;
    let greater_equal = pg_sys::BTGreaterEqualStrategyNumber as i32;
    let greater = pg_sys::BTGreaterStrategyNumber as i32;
    if matches!(value, ScalarValue::F32(value) if value.is_nan())
        || matches!(value, ScalarValue::F64(value) if value.is_nan())
    {
        return Err(ShapeDecline::UnsupportedPredicate);
    }
    if matches!(value, ScalarValue::F32(_) | ScalarValue::F64(_))
        && (strategy == greater_equal || strategy == greater)
    {
        // PostgreSQL orders NaN above +Infinity. The frozen inclusive-range
        // encoding has no unbounded upper endpoint, so synthesizing +Infinity
        // would incorrectly reject NaNs for `column >= constant`.
        return Err(ShapeDecline::UnsupportedPredicate);
    }
    let range = if strategy == less {
        ScalarRange {
            lo: scalar_min(value),
            hi: scalar_previous(value).ok_or(ShapeDecline::UnsupportedPredicate)?,
        }
    } else if strategy == less_equal {
        ScalarRange {
            lo: scalar_min(value),
            hi: value,
        }
    } else if strategy == equal {
        ScalarRange {
            lo: value,
            hi: value,
        }
    } else if strategy == greater_equal {
        ScalarRange {
            lo: value,
            hi: scalar_max(value),
        }
    } else if strategy == greater {
        ScalarRange {
            lo: scalar_next(value).ok_or(ShapeDecline::UnsupportedPredicate)?,
            hi: scalar_max(value),
        }
    } else {
        return Err(ShapeDecline::UnsupportedPredicate);
    };
    Ok(range)
}

unsafe fn btree_strategy(operator: pg_sys::Oid, type_oid: pg_sys::Oid) -> Option<i32> {
    // SAFETY: operator/type OIDs came from an analyzed OpExpr/Var and the
    // catalog lookup runs on PostgreSQL's backend thread.
    let opclass = unsafe { pg_sys::GetDefaultOpClass(type_oid, pg_sys::BTREE_AM_OID) };
    if opclass == pg_sys::InvalidOid {
        return None;
    }
    // SAFETY: opclass is non-invalid and returned by GetDefaultOpClass.
    let family = unsafe { pg_sys::get_opclass_family(opclass) };
    if family == pg_sys::InvalidOid {
        return None;
    }
    // SAFETY: family and operator are valid catalog OIDs in this backend.
    let strategy = unsafe { pg_sys::get_op_opfamily_strategy(operator, family) };
    if strategy == 0 {
        return None;
    }
    let strategy_i16 = i16::try_from(strategy).ok()?;
    // SAFETY: family/type came from the default btree opclass and strategy is
    // the checked catalog strategy for this operator.
    let ordinary = unsafe { pg_sys::get_opfamily_member(family, type_oid, type_oid, strategy_i16) };
    (ordinary == operator).then_some(strategy)
}

pub(super) fn postgis_geometry_typmod_metadata(typmod: i32) -> Option<SpatialValueMetadata> {
    if typmod < 0 || typmod & 0b11 != 0 {
        return None;
    }
    let geom_type = u32::try_from((typmod & 0xfc) >> 2).ok()?;
    if !matches!(
        geom_type,
        crate::engine::residency::RESIDENT_GEOMETRY_POINT
            | crate::engine::residency::RESIDENT_GEOMETRY_LINESTRING
            | crate::engine::residency::RESIDENT_GEOMETRY_POLYGON
            | crate::engine::residency::RESIDENT_GEOMETRY_MULTIPOLYGON
    ) {
        return None;
    }
    let srid = ((typmod & 0x0fff_ff00) - (typmod & 0x1000_0000)) >> 8;
    if !(0..=999_999).contains(&srid) {
        return None;
    }
    Some(SpatialValueMetadata {
        kind: SpatialValueKind::Geometry,
        typmod,
        srid: Some(srid),
    })
}

pub(super) fn require_valid_spatial_constant(is_valid: bool) -> Result<(), ShapeDecline> {
    is_valid
        .then_some(())
        .ok_or(ShapeDecline::InvalidSpatialConstant)
}

unsafe fn spatial_constant_operand(
    constant: &pg_sys::Const,
    catalog: &PostgisCatalogIdentity,
) -> Result<SpatialOperand, ShapeDecline> {
    if constant.consttype != catalog.geometry_type_oid
        || constant.consttypmod < -1
        || constant.constcollid != pg_sys::InvalidOid
        || constant.constlen != -1
        || constant.constbyval
        || constant.constisnull
    {
        return Err(ShapeDecline::UnsupportedPredicate);
    }
    // SAFETY: this is a non-NULL pass-by-reference Const of the exact
    // catalog-proved PostGIS geometry type in the active planner context.
    let original = constant.constvalue.cast_mut_ptr::<pg_sys::varlena>();
    // SAFETY: original points to the non-null varlena Datum proved above and is
    // valid in the active planner memory context.
    let detoasted = unsafe { pg_sys::pg_detoast_datum(original) };
    if detoasted.is_null() {
        return Err(ShapeDecline::UnsupportedPredicate);
    }
    // SAFETY: pg_detoast_datum returned a flat varlena readable for VARSIZE
    // bytes until the planner context is released.
    let byte_count = unsafe { pgrx::varsize(detoasted.cast()) };
    // SAFETY: detoasted is non-null and VARSIZE supplies the complete readable
    // allocation length; to_vec copies it before any conditional pfree.
    let bytes = unsafe { std::slice::from_raw_parts(detoasted.cast::<u8>(), byte_count) }.to_vec();
    if detoasted != original {
        // SAFETY: a distinct pg_detoast_datum result is palloc-owned; bytes
        // now owns the complete copy used by every validation path below.
        unsafe { pg_sys::pfree(detoasted.cast()) };
    }
    let limits = crate::engine::cost::device_limits();
    let maximum = limits
        .resident_domain_max_exact_value_bytes
        .min(crate::engine::spec::MAX_SPATIAL_CONSTANT_BYTES);
    let parsed = crate::engine::residency::validate_resident_geometry_value(&bytes, maximum)
        .map_err(|_| ShapeDecline::UnsupportedPredicate)?;
    if parsed.coordinate_pairs > limits.gpu_spatial_max_vertices_per_row {
        return Err(ShapeDecline::UnsupportedPredicate);
    }
    if let Some(typmod) = postgis_geometry_typmod_metadata(constant.consttypmod)
        && (typmod.srid != Some(parsed.srid)
            || u32::try_from((constant.consttypmod & 0xfc) >> 2).ok() != Some(parsed.geom_type))
    {
        return Err(ShapeDecline::UnsupportedPredicate);
    }
    // Run GEOS only after the strict byte, dimensionality, type, SRID, and
    // device-cap checks above. The original planner Const remains alive even
    // when pg_detoast_datum returned and freed a distinct flat copy.
    // SAFETY: the catalog identity proves this exact extension-owned OID is
    // canonical strict ST_IsValid(geometry), and constvalue is the exact
    // non-NULL catalog-proved geometry Datum checked above.
    let valid = unsafe {
        pg_sys::OidFunctionCall1Coll(
            catalog.is_valid_fn_oid,
            pg_sys::InvalidOid,
            constant.constvalue,
        )
    }
    .value()
        != 0;
    require_valid_spatial_constant(valid)?;
    Ok(SpatialOperand::Constant {
        metadata: SpatialValueMetadata {
            kind: SpatialValueKind::Geometry,
            typmod: constant.consttypmod,
            srid: Some(parsed.srid),
        },
        bytes: bytes.into_boxed_slice(),
    })
}

unsafe fn spatial_operand(
    node: *mut Node,
    query: &pg_sys::Query,
    catalog: &PostgisCatalogIdentity,
) -> Result<(SpatialOperand, Option<pg_sys::Index>), ShapeDecline> {
    // SAFETY: node belongs to the active analyzed Query.
    if let Some(column) = unsafe { direct_var(node, query) } {
        if pg_sys::Oid::from(column.column.type_oid) != catalog.geometry_type_oid
            || column.collation_oid != 0
        {
            return Err(ShapeDecline::UnsupportedPredicate);
        }
        let metadata = postgis_geometry_typmod_metadata(column.type_modifier)
            .ok_or(ShapeDecline::UnsupportedPredicate)?;
        return Ok((
            SpatialOperand::Column {
                column: column.column,
                metadata,
            },
            Some(column.varno),
        ));
    }
    // SAFETY: node belongs to the active planner expression.
    let constant = unsafe { direct_const(node) }.ok_or(ShapeDecline::UnsupportedPredicate)?;
    // SAFETY: the exact catalog type and Const physical shape are checked by
    // spatial_constant_operand before detoasting.
    unsafe { spatial_constant_operand(&constant, catalog) }.map(|operand| (operand, None))
}

#[derive(Debug, Clone)]
enum PendingFilter {
    Ranges {
        input: PlannerColumn,
        range: ScalarRange,
        conjunct_count: u8,
    },
    Mask(PlannerColumn),
    Spatial {
        relation_oid: u32,
        filter: FilterSpec,
    },
}

/// Lower a bounded, single-column three-valued predicate to exact INT4 TRUE
/// intervals. This is input-independent planning, not host row filtering.
unsafe fn aggregate_filter(
    node: *mut Node,
    query: &pg_sys::Query,
) -> Result<FilterSpec, ShapeDecline> {
    if node.is_null() {
        return Ok(FilterSpec::None);
    }
    // SAFETY: node and query share the active analyzed planner lifetime.
    let (input, predicate) = unsafe { aggregate_int4_predicate(node, query, &mut 64) }
        .map_err(|_| ShapeDecline::UnsupportedAggregateModifier)?;
    normalized_int4_filter(input, predicate)
}

fn normalized_int4_filter(
    input: Option<PlannerColumn>,
    predicate: super::int4_predicate::Int4Predicate,
) -> Result<FilterSpec, ShapeDecline> {
    let input = input.ok_or(ShapeDecline::UnsupportedAggregateModifier)?;
    let accept_null = predicate.null_truth == super::int4_predicate::Truth::True;
    if (!accept_null && predicate.true_ranges.is_empty())
        || predicate.true_ranges.len()
            > crate::engine::spec::abi::PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES
    {
        // Zero ordinary ranges disables the ABI lane; a NULL-accepting
        // predicate instead carries an explicit active flag, including IS NULL.
        return Err(ShapeDecline::UnsupportedAggregateModifier);
    }
    let ranges = predicate
        .true_ranges
        .into_iter()
        .map(|(lo, hi)| ScalarRange {
            lo: ScalarValue::I32(lo),
            hi: ScalarValue::I32(hi),
        })
        .collect();
    Ok(if accept_null {
        FilterSpec::NullOrRanges {
            input: input.column,
            ranges,
        }
    } else {
        FilterSpec::Ranges {
            input: input.column,
            ranges,
        }
    })
}

unsafe fn aggregate_int4_operator(operator: pg_sys::Oid) -> Result<(i32, bool), ShapeDecline> {
    // SAFETY: OID is from an analyzed comparison over two exact INT4 inputs.
    if let Some(strategy) = unsafe { btree_strategy(operator, pg_sys::INT4OID) } {
        return Ok((strategy, false));
    }
    // SAFETY: catalog lookup on the backend thread; only the ordinary equality
    // operator's reciprocal negator is accepted, never a name-based match.
    let negator = unsafe { pg_sys::get_negator(operator) };
    if negator != pg_sys::InvalidOid
        && unsafe { btree_strategy(negator, pg_sys::INT4OID) } == Some(3)
        && unsafe { pg_sys::get_negator(negator) } == operator
    {
        Ok((3, true))
    } else {
        Err(ShapeDecline::UnsupportedAggregateModifier)
    }
}

fn aggregate_int4_constant(constant: &pg_sys::Const) -> Result<Option<i32>, ShapeDecline> {
    if constant.consttype != pg_sys::INT4OID || !constant.constbyval || constant.constlen != 4 {
        return Err(ShapeDecline::UnsupportedAggregateModifier);
    }
    // SAFETY: exact INT4 physical representation was checked above.
    Ok((!constant.constisnull).then(|| unsafe { pg_sys::DatumGetInt32(constant.constvalue) }))
}

unsafe fn aggregate_int4_predicate(
    node: *mut Node,
    query: &pg_sys::Query,
    budget: &mut usize,
) -> Result<(Option<PlannerColumn>, super::int4_predicate::Int4Predicate), ShapeDecline> {
    use super::int4_predicate::{Int4Predicate, Truth};
    use pgrx::FromDatum;
    *budget = budget
        .checked_sub(1)
        .ok_or(ShapeDecline::UnsupportedAggregateModifier)?;
    if node.is_null() {
        return Err(ShapeDecline::UnsupportedAggregateModifier);
    }
    // SAFETY: node belongs to the active query and is non-null.
    match unsafe { (*node).type_ } {
        NodeTag::T_BoolExpr | NodeTag::T_List => {
            // SAFETY: the tag is one of the two layouts in this arm. Planner
            // WHERE lists represent implicit AND; explicit Boolean nodes
            // carry their own operation and argument list.
            let (args, boolop) = unsafe {
                if (*node).type_ == NodeTag::T_List {
                    (node.cast::<pg_sys::List>(), pg_sys::BoolExprType::AND_EXPR)
                } else {
                    let boolean = &*node.cast::<pg_sys::BoolExpr>();
                    (boolean.args, boolean.boolop)
                }
            };
            let count = list_len(args);
            if count == 0 || count > 64 {
                return Err(ShapeDecline::UnsupportedAggregateModifier);
            }
            let mut input = None;
            let mut result: Option<Int4Predicate> = None;
            for index in 0..count {
                // SAFETY: index is bounded by the list length.
                let child = unsafe { list_item::<Node>(args, index) }
                    .ok_or(ShapeDecline::UnsupportedAggregateModifier)?;
                // SAFETY: child shares the analyzed query lifetime; budget bounds recursion.
                let (column, next) = unsafe { aggregate_int4_predicate(child, query, budget) }?;
                if input.is_some() && column.is_some() && input != column {
                    return Err(ShapeDecline::UnsupportedAggregateModifier);
                }
                input = input.or(column);
                result = Some(match (boolop, result) {
                    (pg_sys::BoolExprType::NOT_EXPR, None) if count == 1 => next.not(),
                    (pg_sys::BoolExprType::AND_EXPR | pg_sys::BoolExprType::OR_EXPR, None) => next,
                    (pg_sys::BoolExprType::AND_EXPR, Some(previous)) => previous.and(next)?,
                    (pg_sys::BoolExprType::OR_EXPR, Some(previous)) => previous.or(next)?,
                    _ => return Err(ShapeDecline::UnsupportedAggregateModifier),
                });
            }
            Ok((
                input,
                result.ok_or(ShapeDecline::UnsupportedAggregateModifier)?,
            ))
        }
        NodeTag::T_Const => {
            // SAFETY: tag proves Const layout.
            let constant = unsafe { &*node.cast::<pg_sys::Const>() };
            if constant.consttype != pg_sys::BOOLOID
                || !constant.constbyval
                || constant.constlen != 1
            {
                return Err(ShapeDecline::UnsupportedAggregateModifier);
            }
            let truth = if constant.constisnull {
                Truth::Unknown
            }
            // SAFETY: checked the boolean Datum representation above.
            else if unsafe { pg_sys::DatumGetBool(constant.constvalue) } {
                Truth::True
            } else {
                Truth::False
            };
            Ok((None, Int4Predicate::constant(truth)))
        }
        NodeTag::T_NullTest => {
            // SAFETY: this tag proves NullTest layout in the live query tree.
            let test = unsafe { &*node.cast::<pg_sys::NullTest>() };
            // SAFETY: arg belongs to the same analyzed query.
            let column = unsafe { direct_var(test.arg.cast(), query) }
                .filter(|column| {
                    !test.argisrow && column.column.type_oid == u32::from(pg_sys::INT4OID)
                })
                .ok_or(ShapeDecline::UnsupportedAggregateModifier)?;
            let predicate = Int4Predicate::is_null();
            Ok((
                Some(column),
                if test.nulltesttype == pg_sys::NullTestType::IS_NULL {
                    predicate
                } else {
                    predicate.not()
                },
            ))
        }
        NodeTag::T_OpExpr | NodeTag::T_ScalarArrayOpExpr => {
            // SAFETY: each tag proves the corresponding struct layout.
            let (operator, args, array_mode) = unsafe {
                if (*node).type_ == NodeTag::T_OpExpr {
                    let op = &*node.cast::<pg_sys::OpExpr>();
                    (op.opno, op.args, None)
                } else {
                    let op = &*node.cast::<pg_sys::ScalarArrayOpExpr>();
                    (op.opno, op.args, Some(op.useOr))
                }
            };
            if list_len(args) != 2 {
                return Err(ShapeDecline::UnsupportedAggregateModifier);
            }
            // SAFETY: both indices are within the checked list length.
            let left = unsafe { list_item::<Node>(args, 0) }
                .ok_or(ShapeDecline::UnsupportedAggregateModifier)?;
            // SAFETY: both indices are within the checked list length.
            let right = unsafe { list_item::<Node>(args, 1) }
                .ok_or(ShapeDecline::UnsupportedAggregateModifier)?;
            // SAFETY: both operands belong to this query.
            let (column, constant_node, swapped) =
                if let Some(column) = unsafe { direct_var(left, query) } {
                    (column, right, false)
                } else if array_mode.is_none() {
                    (
                        unsafe { direct_var(right, query) }
                            .ok_or(ShapeDecline::UnsupportedAggregateModifier)?,
                        left,
                        true,
                    )
                } else {
                    return Err(ShapeDecline::UnsupportedAggregateModifier);
                };
            if column.column.type_oid != u32::from(pg_sys::INT4OID) {
                return Err(ShapeDecline::UnsupportedAggregateModifier);
            }
            // SAFETY: analyzed comparison over the exact checked input type.
            let (mut strategy, negate) = unsafe { aggregate_int4_operator(operator) }?;
            if swapped {
                strategy = 6 - strategy;
            }
            let compare = |value| {
                Int4Predicate::comparison(value, strategy)
                    .map(|predicate| if negate { predicate.not() } else { predicate })
            };
            // SAFETY: constant_node belongs to the analyzed operator.
            let constant = unsafe { direct_const(constant_node) }
                .ok_or(ShapeDecline::UnsupportedAggregateModifier)?;
            let predicate = if let Some(any) = array_mode {
                if constant.consttype != pg_sys::INT4ARRAYOID
                    || constant.constbyval
                    || constant.constlen != -1
                {
                    return Err(ShapeDecline::UnsupportedAggregateModifier);
                }
                if constant.constisnull {
                    Int4Predicate::constant(Truth::Unknown)
                } else {
                    // SAFETY: exact non-NULL INT4[] Datum from PostgreSQL-owned Const.
                    let array =
                        unsafe { pgrx::Array::<i32>::from_datum(constant.constvalue, false) }
                            .ok_or(ShapeDecline::UnsupportedAggregateModifier)?;
                    if array.len() > 256 {
                        return Err(ShapeDecline::UnsupportedAggregateModifier);
                    }
                    let mut result =
                        Int4Predicate::constant(if any { Truth::False } else { Truth::True });
                    for item in array.iter() {
                        let next = compare(item)?;
                        result = if any {
                            result.or(next)?
                        } else {
                            result.and(next)?
                        };
                    }
                    result
                }
            } else {
                compare(aggregate_int4_constant(&constant)?)?
            };
            Ok((Some(column), predicate))
        }
        _ => Err(ShapeDecline::UnsupportedAggregateModifier),
    }
}

fn intersect_ranges(left: ScalarRange, right: ScalarRange) -> Result<ScalarRange, ShapeDecline> {
    let lo = match scalar_cmp(left.lo, right.lo) {
        Some(std::cmp::Ordering::Less) => right.lo,
        Some(_) => left.lo,
        None => return Err(ShapeDecline::InvalidFilterRange),
    };
    let hi = match scalar_cmp(left.hi, right.hi) {
        Some(std::cmp::Ordering::Greater) => right.hi,
        Some(_) => left.hi,
        None => return Err(ShapeDecline::InvalidFilterRange),
    };
    if scalar_cmp(lo, hi).is_none_or(std::cmp::Ordering::is_gt) {
        return Err(ShapeDecline::InvalidFilterRange);
    }
    Ok(ScalarRange { lo, hi })
}

fn add_filter(
    filters: &mut BTreeMap<u32, PendingFilter>,
    filter: PendingFilter,
) -> Result<(), ShapeDecline> {
    let relation_oid = match &filter {
        PendingFilter::Ranges { input, .. } | PendingFilter::Mask(input) => {
            input.column.relation_oid
        }
        PendingFilter::Spatial { relation_oid, .. } => *relation_oid,
    };
    let Some(existing) = filters.get_mut(&relation_oid) else {
        filters.insert(relation_oid, filter);
        return Ok(());
    };
    match (existing, filter) {
        (
            PendingFilter::Ranges {
                input,
                range: existing_range,
                conjunct_count,
            },
            PendingFilter::Ranges {
                input: new_input,
                range: new_range,
                ..
            },
        ) if *input == new_input && input.column.type_oid == u32::from(pg_sys::INT4OID) => {
            if *conjunct_count != 1 {
                return Err(ShapeDecline::MultipleRangePredicates);
            }
            *existing_range = intersect_ranges(*existing_range, new_range)?;
            *conjunct_count = 2;
            Ok(())
        }
        (
            PendingFilter::Ranges { input, .. },
            PendingFilter::Ranges {
                input: new_input, ..
            },
        ) if *input == new_input => Err(ShapeDecline::MultipleRangePredicates),
        (PendingFilter::Ranges { conjunct_count, .. }, PendingFilter::Ranges { .. })
            if *conjunct_count > 1 =>
        {
            Err(ShapeDecline::MultipleRangePredicates)
        }
        (PendingFilter::Mask(existing_column), PendingFilter::Mask(new_column))
            if *existing_column == new_column =>
        {
            Ok(())
        }
        (
            PendingFilter::Spatial {
                relation_oid: existing_relation,
                filter: existing_filter,
            },
            PendingFilter::Spatial {
                relation_oid: new_relation,
                filter: new_filter,
            },
        ) if *existing_relation == new_relation && *existing_filter == new_filter => Ok(()),
        _ => Err(ShapeDecline::MultipleFiltersPerRelation { relation_oid }),
    }
}

unsafe fn parse_operator_predicate(
    operation: &pg_sys::OpExpr,
    query: &pg_sys::Query,
    joins: &mut Vec<EquiJoin>,
    filters: &mut BTreeMap<u32, PendingFilter>,
) -> Result<(), ShapeDecline> {
    if list_len(operation.args) != 2 {
        return Err(ShapeDecline::UnsupportedPredicate);
    }
    // SAFETY: the two-item length check bounds both argument reads.
    let left_node = unsafe { list_item::<Node>(operation.args, 0) }
        .ok_or(ShapeDecline::UnsupportedPredicate)?;
    // SAFETY: the two-item length check bounds both argument reads.
    let right_node = unsafe { list_item::<Node>(operation.args, 1) }
        .ok_or(ShapeDecline::UnsupportedPredicate)?;
    // SAFETY: both argument nodes belong to this active Query.
    let left_var = unsafe { direct_var(left_node, query) };
    // SAFETY: both argument nodes belong to this active Query.
    let right_var = unsafe { direct_var(right_node, query) };
    if let (Some(left), Some(right)) = (left_var, right_var) {
        if left.column.type_oid != right.column.type_oid {
            return Err(ShapeDecline::JoinKeyTypeMismatch {
                left_type_oid: left.column.type_oid,
                right_type_oid: right.column.type_oid,
            });
        }
        let input_collation_oid = u32::from(operation.inputcollid);
        if input_collation_oid != left.collation_oid || input_collation_oid != right.collation_oid {
            return Err(ShapeDecline::JoinKeyCollationMismatch {
                left_collation_oid: left.collation_oid,
                right_collation_oid: right.collation_oid,
            });
        }
        if matches!(left.column.type_oid, 25 | 1042 | 1043)
            && (operation.inputcollid == pg_sys::InvalidOid
                // SAFETY: a non-invalid input collation on the analyzed
                // operator is safe to inspect on the backend thread.
                || unsafe { !pg_sys::get_collation_isdeterministic(operation.inputcollid) })
        {
            return Err(ShapeDecline::NondeterministicKeyCollation {
                collation_oid: u32::from(operation.inputcollid),
            });
        }
        // SAFETY: operation.opno and input type come from the analyzed
        // equality expression and are valid catalog identities.
        let hashable = unsafe {
            pg_sys::op_hashjoinable(operation.opno, pg_sys::Oid::from(left.column.type_oid))
        };
        // SAFETY: direct_var catalog-validated the input type.
        let ordinary =
            unsafe { default_equality_operator(pg_sys::Oid::from(left.column.type_oid)) }
                .ok_or(ShapeDecline::NonEqualityJoin)?;
        if !is_ordinary_hash_equality(operation.opno, ordinary, hashable) {
            return Err(ShapeDecline::NonEqualityJoin);
        }
        let join = EquiJoin { left, right };
        if !joins.contains(&join) {
            joins.push(join);
        }
        return Ok(());
    }

    let (column, constant, swapped) = if let Some(column) = left_var {
        (
            column,
            // SAFETY: right_node belongs to the current planner expression.
            unsafe { direct_const(right_node) }.ok_or(ShapeDecline::UnsupportedPredicate)?,
            false,
        )
    } else if let Some(column) = right_var {
        (
            column,
            // SAFETY: left_node belongs to the current planner expression.
            unsafe { direct_const(left_node) }.ok_or(ShapeDecline::UnsupportedPredicate)?,
            true,
        )
    } else {
        return Err(ShapeDecline::UnsupportedPredicate);
    };
    if u32::from(constant.consttype) != column.column.type_oid {
        return Err(ShapeDecline::UnsupportedPredicate);
    }
    // SAFETY: analyzed operator and catalog-validated column type are looked
    // up synchronously on the backend thread.
    let mut strategy =
        unsafe { btree_strategy(operation.opno, pg_sys::Oid::from(column.column.type_oid)) }
            .ok_or(ShapeDecline::UnsupportedPredicate)?;
    if swapped {
        strategy = 6_i32
            .checked_sub(strategy)
            .ok_or(ShapeDecline::UnsupportedPredicate)?;
    }
    let range = range_for_strategy(scalar_value(&constant)?, strategy)?;
    add_filter(
        filters,
        PendingFilter::Ranges {
            input: column,
            range,
            conjunct_count: 1,
        },
    )
}

fn spatial_operand_metadata(operand: &SpatialOperand) -> SpatialValueMetadata {
    match operand {
        SpatialOperand::Column { metadata, .. } | SpatialOperand::Constant { metadata, .. } => {
            *metadata
        }
    }
}

unsafe fn spatial_distance(node: *mut Node) -> Result<ScalarValue, ShapeDecline> {
    // SAFETY: node belongs to the current analyzed function expression.
    let constant = unsafe { direct_const(node) }.ok_or(ShapeDecline::UnsupportedPredicate)?;
    if constant.consttype != pg_sys::FLOAT8OID
        || constant.consttypmod != -1
        || constant.constcollid != pg_sys::InvalidOid
        || constant.constlen != 8
        || !constant.constbyval
        || constant.constisnull
    {
        return Err(ShapeDecline::UnsupportedPredicate);
    }
    // SAFETY: the exact non-NULL FLOAT8 Const physical contract was checked.
    let value = unsafe { pg_sys::DatumGetFloat8(constant.constvalue) };
    if !value.is_finite() || value < 0.0 {
        return Err(ShapeDecline::UnsupportedPredicate);
    }
    Ok(ScalarValue::F64(value))
}

pub(super) fn require_postgis_function_member(is_member: bool) -> Result<(), ShapeDecline> {
    is_member
        .then_some(())
        .ok_or(ShapeDecline::UnsupportedPredicate)
}

unsafe fn parse_function_predicate(
    function: &pg_sys::FuncExpr,
    query: &pg_sys::Query,
    filters: &mut BTreeMap<u32, PendingFilter>,
) -> Result<(), ShapeDecline> {
    if function.funcid == pg_sys::InvalidOid
        || function.funcretset
        || function.funcvariadic
        || function.funcresulttype != pg_sys::BOOLOID
        || function.funccollid != pg_sys::InvalidOid
        || function.inputcollid != pg_sys::InvalidOid
    {
        return Err(ShapeDecline::UnsupportedPredicate);
    }
    // SAFETY: this cheap extension-membership check runs on the backend main
    // thread. Ordinary FuncExprs and same-name impostors decline without
    // requiring PostGIS to be installed or resolving its complete catalog.
    require_postgis_function_member(unsafe {
        crate::engine::ffi::syscache::function_is_extension_member(function.funcid, "postgis")
    })?;
    // SAFETY: planner extraction runs on PostgreSQL's main backend thread.
    let catalog = unsafe { crate::engine::ffi::syscache::resolve_postgis_catalog() }
        .map_err(ShapeDecline::PostgisCatalog)?;
    let operation = catalog
        .classify_function(function.funcid)
        .ok_or(ShapeDecline::UnsupportedPredicate)?;
    let (predicate, argument_count) = match operation {
        PostgisSpatialFunction::Intersects => (SpatialPredicateKind::Intersects, 2),
        PostgisSpatialFunction::Contains => (SpatialPredicateKind::Contains, 2),
        PostgisSpatialFunction::Within => (SpatialPredicateKind::Within, 2),
        PostgisSpatialFunction::Covers => (SpatialPredicateKind::Covers, 2),
        PostgisSpatialFunction::CoveredBy => (SpatialPredicateKind::CoveredBy, 2),
        PostgisSpatialFunction::DWithin => (SpatialPredicateKind::DWithin, 3),
        // ST_Distance is a scalar value producer, not a boolean fact filter.
        PostgisSpatialFunction::Distance => return Err(ShapeDecline::UnsupportedPredicate),
    };
    if list_len(function.args) != argument_count {
        return Err(ShapeDecline::UnsupportedPredicate);
    }
    // SAFETY: the exact argument-count check bounds both geometry reads.
    let left_node =
        unsafe { list_item::<Node>(function.args, 0) }.ok_or(ShapeDecline::UnsupportedPredicate)?;
    // SAFETY: the exact argument-count check above proves index 1 is present in
    // this planner-owned function argument List.
    let right_node =
        unsafe { list_item::<Node>(function.args, 1) }.ok_or(ShapeDecline::UnsupportedPredicate)?;
    // SAFETY: both arguments belong to this analyzed Query and catalog is the
    // freshly proved PostGIS identity.
    let (left, left_varno) = unsafe { spatial_operand(left_node, query, &catalog) }?;
    // SAFETY: right_node belongs to the same analyzed Query and catalog is the
    // freshly proved PostGIS identity.
    let (right, right_varno) = unsafe { spatial_operand(right_node, query, &catalog) }?;
    let relation_oid = match (left.column(), right.column()) {
        (Some(left), Some(right)) if left.relation_oid == right.relation_oid => left.relation_oid,
        (Some(left), None) => left.relation_oid,
        (None, Some(right)) => right.relation_oid,
        _ => return Err(ShapeDecline::UnsupportedPredicate),
    };
    if let (Some(left_varno), Some(right_varno)) = (left_varno, right_varno)
        && left_varno != right_varno
    {
        return Err(ShapeDecline::UnsupportedPredicate);
    }
    let left_metadata = spatial_operand_metadata(&left);
    let right_metadata = spatial_operand_metadata(&right);
    if left_metadata.kind != SpatialValueKind::Geometry
        || right_metadata.kind != SpatialValueKind::Geometry
        || left_metadata.srid != right_metadata.srid
    {
        return Err(ShapeDecline::UnsupportedPredicate);
    }
    let distance = if predicate == SpatialPredicateKind::DWithin {
        // SAFETY: DWithin's exact three-argument count bounds this read.
        let distance_node = unsafe { list_item::<Node>(function.args, 2) }
            .ok_or(ShapeDecline::UnsupportedPredicate)?;
        // SAFETY: distance_node belongs to the analyzed function expression.
        Some(unsafe { spatial_distance(distance_node) }?)
    } else {
        None
    };
    add_filter(
        filters,
        PendingFilter::Spatial {
            relation_oid,
            filter: FilterSpec::Spatial {
                predicate,
                left,
                right,
                distance,
            },
        },
    )
}

unsafe fn parse_qual(
    node: *mut Node,
    query: &pg_sys::Query,
    joins: &mut Vec<EquiJoin>,
    filters: &mut BTreeMap<u32, PendingFilter>,
) -> Result<(), ShapeDecline> {
    if node.is_null() {
        return Ok(());
    }
    // SAFETY: node is a planner qualifier and PostgreSQL owns any returned
    // inner expression for the same planner lifetime.
    let node = unsafe { pg_sys::strip_implicit_coercions(node) };
    // SAFETY: node is non-null planner memory here.
    match unsafe { (*node).type_ } {
        NodeTag::T_List => {
            let list = node.cast::<pg_sys::List>();
            for index in bounded_qual_conjunct_indices(list_len(list))? {
                // SAFETY: index is bounded by list_len(list), and the implicit
                // AND list contains planner-owned expression pointers.
                let child = unsafe { list_item::<Node>(list, index) }
                    .ok_or(ShapeDecline::UnsupportedPredicate)?;
                // SAFETY: child and query belong to the same qualifier tree.
                unsafe { parse_qual(child, query, joins, filters) }?;
            }
            Ok(())
        }
        NodeTag::T_BoolExpr => {
            // SAFETY: the NodeTag arm proves the BoolExpr layout.
            let boolean = unsafe { &*node.cast::<pg_sys::BoolExpr>() };
            if boolean.boolop != pg_sys::BoolExprType::AND_EXPR {
                return Err(ShapeDecline::UnsupportedPredicate);
            }
            for index in bounded_qual_conjunct_indices(list_len(boolean.args))? {
                // SAFETY: index is bounded by list_len(boolean.args).
                let child = unsafe { list_item::<Node>(boolean.args, index) }
                    .ok_or(ShapeDecline::UnsupportedPredicate)?;
                // SAFETY: child and query belong to the same qualifier tree.
                unsafe { parse_qual(child, query, joins, filters) }?;
            }
            Ok(())
        }
        // SAFETY: this NodeTag arm proves the OpExpr layout; query and node
        // share the active planner lifetime.
        NodeTag::T_OpExpr => unsafe {
            parse_operator_predicate(&*node.cast::<pg_sys::OpExpr>(), query, joins, filters)
        },
        // SAFETY: this NodeTag arm proves the FuncExpr layout; query and node
        // share the active planner lifetime.
        NodeTag::T_FuncExpr => unsafe {
            parse_function_predicate(&*node.cast::<pg_sys::FuncExpr>(), query, filters)
        },
        NodeTag::T_Var => {
            // SAFETY: node is a Var in this Query's qualifier tree.
            let column = unsafe { direct_var(node, query) }
                .filter(|column| column.column.type_oid == u32::from(pg_sys::BOOLOID))
                .ok_or(ShapeDecline::UnsupportedPredicate)?;
            add_filter(filters, PendingFilter::Mask(column))
        }
        _ => Err(ShapeDecline::UnsupportedPredicate),
    }
}

type ExtractedPredicates = (Vec<EquiJoin>, Vec<(u32, FilterSpec)>);

/// Consume every top-level AND conjunct. Only scalar subtrees may contain
/// OR/NOT; a spatial OR must never be mistaken for a prefiltering conjunction.
unsafe fn spatial_and_scalar_filter(
    node: *mut Node,
    query: &pg_sys::Query,
) -> Result<(u32, FilterSpec), ShapeDecline> {
    let mut pending = vec![node];
    let mut budget = 64;
    let mut spatial_filters = BTreeMap::new();
    let mut scalar_input = None;
    let mut scalar_predicate: Option<super::int4_predicate::Int4Predicate> = None;
    while let Some(node) = pending.pop() {
        if node.is_null() || budget == 0 {
            return Err(ShapeDecline::UnsupportedPredicate);
        }
        budget -= 1;
        // SAFETY: all pending nodes originate in this live analyzed query.
        let node = unsafe { pg_sys::strip_implicit_coercions(node) };
        // SAFETY: a non-null planner node has its tagged PostgreSQL layout.
        let tag = unsafe { (*node).type_ };
        let conjunction = if tag == NodeTag::T_List {
            Some(node.cast::<pg_sys::List>())
        } else if tag == NodeTag::T_BoolExpr {
            // SAFETY: tag proves BoolExpr layout.
            let boolean = unsafe { &*node.cast::<pg_sys::BoolExpr>() };
            (boolean.boolop == pg_sys::BoolExprType::AND_EXPR).then_some(boolean.args)
        } else {
            None
        };
        if let Some(args) = conjunction {
            let count = list_len(args);
            if count == 0 || count > budget || pending.len() + count > budget {
                return Err(ShapeDecline::UnsupportedPredicate);
            }
            for index in (0..count).rev() {
                // SAFETY: index is bounded by this planner-owned argument list.
                pending.push(
                    unsafe { list_item::<Node>(args, index) }
                        .ok_or(ShapeDecline::UnsupportedPredicate)?,
                );
            }
        } else if tag == NodeTag::T_FuncExpr {
            // SAFETY: tag proves FuncExpr; catalog and operands are validated
            // by the existing exact PostGIS parser on the backend thread.
            unsafe {
                parse_function_predicate(
                    &*node.cast::<pg_sys::FuncExpr>(),
                    query,
                    &mut spatial_filters,
                )
            }?;
        } else {
            // SAFETY: this subtree shares query lifetime; the shared budget
            // bounds both scalar recursion and the outer conjunction walk.
            let (input, predicate) = unsafe { aggregate_int4_predicate(node, query, &mut budget) }?;
            if scalar_input.is_some() && input.is_some() && scalar_input != input {
                return Err(ShapeDecline::UnsupportedPredicate);
            }
            scalar_input = scalar_input.or(input);
            scalar_predicate = Some(match scalar_predicate {
                Some(previous) => previous.and(predicate)?,
                None => predicate,
            });
        }
    }
    if spatial_filters.len() != 1 {
        return Err(ShapeDecline::UnsupportedPredicate);
    }
    let (relation_oid, spatial) = spatial_filters
        .into_iter()
        .next()
        .ok_or(ShapeDecline::UnsupportedPredicate)?;
    let PendingFilter::Spatial { filter, .. } = spatial else {
        return Err(ShapeDecline::UnsupportedPredicate);
    };
    if scalar_input.is_none_or(|input| input.column.relation_oid != relation_oid) {
        return Err(ShapeDecline::UnsupportedPredicate);
    }
    let scalar = normalized_int4_filter(
        scalar_input,
        scalar_predicate.ok_or(ShapeDecline::UnsupportedPredicate)?,
    )?;
    Ok((
        relation_oid,
        FilterSpec::SpatialAnd {
            spatial: Box::new(filter),
            scalar: Box::new(scalar),
        },
    ))
}

/// Compile the complete qualification, substituting exactly one top-level
/// spatial conjunct with its execution-time SQL mask. Keeping the mask in the
/// program preserves native qual-cost ordering, including throwing scalars.
unsafe fn spatial_program_filter(
    root: *mut pg_sys::PlannerInfo,
    node: *mut Node,
    query: &pg_sys::Query,
) -> Result<(u32, FilterSpec), ShapeDecline> {
    let mut pending = vec![node];
    let mut budget = MAX_QUAL_CONJUNCTS;
    let mut spatial_filters = BTreeMap::new();
    let mut mask_node: *mut Node = std::ptr::null_mut();
    let mut spatial_column = None;
    while let Some(node) = pending.pop() {
        if node.is_null() || budget == 0 {
            return Err(ShapeDecline::UnsupportedPredicate);
        }
        budget -= 1;
        // SAFETY: every pending node belongs to the caller's live WHERE tree.
        let tag = unsafe { (*node).type_ };
        let conjunction = if tag == NodeTag::T_List {
            Some(node.cast::<pg_sys::List>())
        } else if tag == NodeTag::T_BoolExpr {
            // SAFETY: tag proves BoolExpr layout.
            let boolean = unsafe { &*node.cast::<pg_sys::BoolExpr>() };
            (boolean.boolop == pg_sys::BoolExprType::AND_EXPR).then_some(boolean.args)
        } else {
            None
        };
        if let Some(args) = conjunction {
            let count = list_len(args);
            if count == 0 || pending.len() + count > budget {
                return Err(ShapeDecline::UnsupportedPredicate);
            }
            for index in (0..count).rev() {
                // SAFETY: bounded index in this planner-owned argument list.
                pending.push(
                    unsafe { list_item::<Node>(args, index) }
                        .ok_or(ShapeDecline::UnsupportedPredicate)?,
                );
            }
        } else if tag == NodeTag::T_FuncExpr {
            if !mask_node.is_null() {
                return Err(ShapeDecline::UnsupportedPredicate);
            }
            // SAFETY: tag proves the layout; exact catalog/operand validation
            // is shared with the existing qualified spatial parser.
            let function = unsafe { &*node.cast::<pg_sys::FuncExpr>() };
            // SAFETY: function/query share this planner invocation.
            unsafe { parse_function_predicate(function, query, &mut spatial_filters)? };
            for index in 0..2 {
                // SAFETY: the successful parser proved at least two arguments.
                let argument = unsafe { list_item::<Node>(function.args, index) }
                    .ok_or(ShapeDecline::UnsupportedPredicate)?;
                // SAFETY: this is an operand in the same analyzed query.
                if let Some(column) = unsafe { direct_var(argument, query) } {
                    if spatial_column
                        .is_some_and(|previous: PlannerColumn| previous.varno != column.varno)
                    {
                        return Err(ShapeDecline::UnsupportedPredicate);
                    }
                    spatial_column = Some(column);
                }
            }
            mask_node = node;
        }
    }
    let column = spatial_column.ok_or(ShapeDecline::UnsupportedPredicate)?;
    let Some((relation_oid, PendingFilter::Spatial { filter, .. })) =
        spatial_filters.into_iter().next()
    else {
        return Err(ShapeDecline::UnsupportedPredicate);
    };
    // SAFETY: mask_node is the sole exact spatial conjunct in this same tree.
    let lowered = unsafe {
        super::postgres_scalar::lower_spatial_where_expression(root, node, query, mask_node)?
    };
    if lowered.inputs.is_empty()
        || lowered
            .inputs
            .iter()
            .any(|input| input.varno != column.varno || input.column.relation_oid != relation_oid)
    {
        return Err(ShapeDecline::UnsupportedPredicate);
    }
    Ok((
        relation_oid,
        FilterSpec::SpatialAnd {
            spatial: Box::new(filter),
            scalar: Box::new(FilterSpec::Bytecode {
                inputs: lowered.inputs.iter().map(|input| input.column).collect(),
                program: lowered.program.encode(),
            }),
        },
    ))
}

unsafe fn joins_and_filters(
    root: *mut pg_sys::PlannerInfo,
    query: &pg_sys::Query,
    inventory: &ExpressionInventory,
    dense_where: bool,
) -> Result<ExtractedPredicates, ShapeDecline> {
    // Collect every FromExpr qualification, not just the outermost WHERE.
    // Keep planner-owned expression nodes and build only a fresh list spine.
    let mut where_quals = std::ptr::null_mut();
    for from in &inventory.from_nodes {
        // SAFETY: inventory records live FromExpr pointers in child-first order.
        let quals = unsafe { (**from).quals };
        if quals.is_null() {
            continue;
        }
        // SAFETY: quals is a live planner expression or implicit-AND List.
        if unsafe { (*quals).type_ } == NodeTag::T_List {
            // SAFETY: copies list cells without mutating either input list.
            where_quals = unsafe { pg_sys::list_concat_copy(where_quals, quals.cast()) };
        } else {
            // SAFETY: append the borrowed expression to our own list spine.
            where_quals = unsafe { pg_sys::lappend(where_quals, quals.cast()) };
        }
    }
    let where_qual = where_quals.cast::<Node>();
    if dense_where && inventory.join_nodes.is_empty() && !query.jointree.is_null() {
        // SAFETY: jointree belongs to the live analyzed Query. Normalize the
        // complete WHERE, never a selected subexpression of a larger filter.
        let normalized = unsafe { aggregate_filter(where_qual, query) };
        if let Ok(
            filter @ (FilterSpec::Ranges { input, .. } | FilterSpec::NullOrRanges { input, .. }),
        ) = normalized
        {
            return Ok((Vec::new(), vec![(input.relation_oid, filter)]));
        }
        // SAFETY: consume the complete WHERE and return only if every
        // conjunct participates in the validated combined contract.
        if let Ok(filter) = unsafe { spatial_and_scalar_filter(where_qual, query) } {
            return Ok((Vec::new(), vec![filter]));
        }
        // SAFETY: the complete WHERE and root belong to this same planner tree.
        if let Ok(filter) = unsafe { spatial_program_filter(root, where_qual, query) } {
            return Ok((Vec::new(), vec![filter]));
        }
        // SAFETY: lower the entire live WHERE, preserving lazy expression
        // semantics and one range-table occurrence across all scalar inputs.
        if let Ok(lowered) =
            unsafe { super::postgres_scalar::lower_where_expression(root, where_qual, query) }
            && lowered.program.result_type() == crate::engine::spec::scalar::ScalarType::Bool
            && let Some(input) = lowered.inputs.first()
        {
            if lowered.inputs.len() == 1
                && lowered.program.instructions()
                    == [crate::engine::spec::scalar::ScalarInstruction::Input(0)]
            {
                // Keep the existing direct Boolean mask admission and costs.
                return Ok((
                    Vec::new(),
                    vec![(
                        input.column.relation_oid,
                        FilterSpec::Mask {
                            input: input.column,
                            kind: MaskKind::Sql,
                        },
                    )],
                ));
            }
            return Ok((
                Vec::new(),
                vec![(
                    input.column.relation_oid,
                    FilterSpec::Bytecode {
                        inputs: lowered.inputs.iter().map(|input| input.column).collect(),
                        program: lowered.program.encode(),
                    },
                )],
            ));
        }
    }
    let mut joins = Vec::new();
    let mut filters = BTreeMap::new();
    let mut seen_joins = BTreeSet::new();
    for join_ptr in &inventory.join_nodes {
        if !seen_joins.insert(join_ptr.addr()) {
            continue;
        }
        // SAFETY: inventory recorded this pointer only from T_JoinExpr nodes
        // in the still-live query tree.
        let join = unsafe { &**join_ptr };
        if join.jointype != pg_sys::JoinType::JOIN_INNER {
            return Err(ShapeDecline::UnsupportedOuterJoin);
        }
        // SAFETY: join.quals and query belong to the same planner tree.
        unsafe { parse_qual(join.quals, query, &mut joins, &mut filters) }?;
    }
    // SAFETY: all collected qualifications and query share the planner tree.
    unsafe { parse_qual(where_qual, query, &mut joins, &mut filters) }?;
    let relation_filters = filters
        .into_iter()
        .map(|(relation_oid, filter)| {
            let filter = match filter {
                PendingFilter::Ranges { input, range, .. } => FilterSpec::Ranges {
                    input: input.column,
                    ranges: vec![range],
                },
                PendingFilter::Mask(input) => FilterSpec::Mask {
                    input: input.column,
                    kind: MaskKind::Sql,
                },
                PendingFilter::Spatial { filter, .. } => filter,
            };
            (relation_oid, filter)
        })
        .collect();
    Ok((joins, relation_filters))
}

fn estimate_rows(rows: f64) -> u64 {
    if !rows.is_finite() || rows <= 0.0 {
        0
    } else if rows >= u64::MAX as f64 {
        u64::MAX
    } else {
        rows.ceil() as u64
    }
}

unsafe fn planner_relation(
    root: &pg_sys::PlannerInfo,
    varno: pg_sys::Index,
) -> Option<*mut pg_sys::RelOptInfo> {
    let varno_i32 = i32::try_from(varno).ok()?;
    if root.simple_rel_array.is_null() || varno == 0 || varno_i32 >= root.simple_rel_array_size {
        return None;
    }
    let offset = isize::try_from(varno).ok()?;
    // SAFETY: varno was checked against simple_rel_array_size and the array
    // pointer is non-null.
    let relation = unsafe { *root.simple_rel_array.offset(offset) };
    (!relation.is_null()).then_some(relation)
}

/// Correctness proof for one `IndexOptInfo` entry.
///
/// PostgreSQL's `src/backend/optimizer/util/plancat.c::has_unique_index`
/// explicitly ignores `indimmediate`, which is correct for estimates but not
/// for execution semantics: deferred uniqueness can be violated inside the
/// current transaction. The unified star executor therefore uses this
/// stricter predicate and also excludes partial/hypothetical indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub(super) struct IndexUniquenessFacts {
    pub unique: bool,
    pub immediate: bool,
    pub hypothetical: bool,
    pub nkeycolumns: i32,
    pub key_attno: i32,
    pub has_predicate: bool,
    pub ordinary_opfamily: bool,
}

pub(super) const fn index_proves_immediate_single_key_uniqueness(
    facts: IndexUniquenessFacts,
    required_attno: i32,
) -> bool {
    facts.unique
        && facts.immediate
        && !facts.hypothetical
        && facts.nkeycolumns == 1
        && facts.key_attno == required_attno
        && !facts.has_predicate
        && facts.ordinary_opfamily
}

unsafe fn relation_has_immediate_single_key_uniqueness(
    relation: *mut pg_sys::RelOptInfo,
    attno: i32,
    type_oid: pg_sys::Oid,
) -> bool {
    if relation.is_null() {
        return false;
    }
    // SAFETY: relation is a non-null planner-owned RelOptInfo.
    let indexes = unsafe { (*relation).indexlist };
    // SAFETY: type_oid is the catalog type for this relation attribute.
    let default_opclass = unsafe { pg_sys::GetDefaultOpClass(type_oid, pg_sys::BTREE_AM_OID) };
    if default_opclass == pg_sys::InvalidOid {
        return false;
    }
    // SAFETY: default_opclass is non-invalid and came from PostgreSQL.
    let default_opfamily = unsafe { pg_sys::get_opclass_family(default_opclass) };
    if default_opfamily == pg_sys::InvalidOid {
        return false;
    }
    for index in 0..list_len(indexes) {
        // SAFETY: index is bounded by list_len(indexes).
        let Some(index_info) = (unsafe { list_item::<pg_sys::IndexOptInfo>(indexes, index) })
        else {
            continue;
        };
        // SAFETY: list_item returned a non-null IndexOptInfo from indexlist.
        let index_info = unsafe { &*index_info };
        let key_attno = if index_info.indexkeys.is_null() || index_info.nkeycolumns <= 0 {
            0
        } else {
            // SAFETY: nkeycolumns > 0 and indexkeys is non-null.
            unsafe { *index_info.indexkeys }
        };
        let ordinary_opfamily = index_info.nkeycolumns > 0
            && !index_info.opfamily.is_null()
            // SAFETY: nkeycolumns > 0 is required by the proof predicate and
            // opfamily has exactly nkeycolumns entries.
            && unsafe { *index_info.opfamily } == default_opfamily;
        if index_proves_immediate_single_key_uniqueness(
            IndexUniquenessFacts {
                unique: index_info.unique,
                immediate: index_info.immediate,
                hypothetical: index_info.hypothetical,
                nkeycolumns: index_info.nkeycolumns,
                key_attno,
                has_predicate: !index_info.indpred.is_null(),
                ordinary_opfamily,
            },
            attno,
        ) {
            return true;
        }
    }
    false
}

unsafe fn relation_shapes(
    root: &pg_sys::PlannerInfo,
    query: &pg_sys::Query,
    inventory: &ExpressionInventory,
) -> Result<Vec<RelationShape>, ShapeDecline> {
    let mut referenced_attnos = BTreeMap::<pg_sys::Index, BTreeSet<i32>>::new();
    for var in &inventory.vars {
        // SAFETY: inventory only records T_Var pointers from this Query.
        if let Some(column) = unsafe { direct_var((*var).cast(), query) } {
            referenced_attnos
                .entry(column.varno)
                .or_default()
                .insert(column.column.attno);
        }
    }

    let mut relations = Vec::new();
    for index in 0..list_len(query.rtable) {
        // SAFETY: index is bounded by list_len(query.rtable).
        let rte = unsafe { list_item::<pg_sys::RangeTblEntry>(query.rtable, index) }.ok_or(
            ShapeDecline::UnsupportedRangeTableEntry {
                varno: u32::try_from(index + 1).unwrap_or(u32::MAX),
            },
        )?;
        // SAFETY: list_item returned a non-null RangeTblEntry.
        let rte_ref = unsafe { &*rte };
        let varno = u32::try_from(index + 1).unwrap_or(u32::MAX);
        if !preflight_range_table_entry(
            varno,
            // SAFETY: root/query/RTE share this active planner invocation.
            unsafe { preflight_rte_facts(root, query, varno, rte_ref) },
        )? {
            continue;
        }
        // SAFETY: planner_relation checks simple_rel_array bounds.
        let planner_rel = unsafe { planner_relation(root, varno) };
        let estimated_rows = planner_rel.map_or(0, |relation| {
            // SAFETY: relation comes from the in-bounds simple_rel_array.
            estimate_rows(unsafe { (*relation).tuples.max((*relation).rows) })
        });
        let mut unique_attnos = BTreeSet::new();
        let mut column_widths = BTreeMap::new();
        for attno in referenced_attnos.get(&varno).into_iter().flatten() {
            let Ok(attno_i16) = i16::try_from(*attno) else {
                return Err(ShapeDecline::UnsupportedColumn {
                    relation_oid: u32::from(rte_ref.relid),
                    attno: *attno,
                });
            };
            // SAFETY: relation/attribute identities were catalog-validated.
            let attribute_type = unsafe { pg_sys::get_atttype(rte_ref.relid, attno_i16) };
            if planner_rel.is_some_and(|relation| {
                // SAFETY: relation came from the checked simple_rel_array;
                // the helper walks only its planner-owned indexlist.
                unsafe {
                    relation_has_immediate_single_key_uniqueness(
                        relation,
                        i32::from(attno_i16),
                        attribute_type,
                    )
                }
            }) {
                unique_attnos.insert(*attno);
            }
            // SAFETY: relation OID and attno came from a catalog-validated
            // Var; lookup runs synchronously on the backend thread.
            let average = unsafe { pg_sys::get_attavgwidth(rte_ref.relid, attno_i16) };
            // SAFETY: same catalog identities as the average-width lookup.
            let fixed =
                unsafe { pg_sys::get_typlen(pg_sys::get_atttype(rte_ref.relid, attno_i16)) };
            let width = if average > 0 {
                average
            } else {
                i32::from(fixed)
            };
            if let Ok(width) = u32::try_from(width)
                && width > 0
            {
                column_widths.insert(*attno, width);
            }
        }
        relations.push(RelationShape {
            varno,
            relation_oid: u32::from(rte_ref.relid),
            estimated_rows,
            unique_attnos,
            column_widths,
            residency: RelationResidency::Unknown,
        });
    }
    Ok(relations)
}

pub(super) fn reject_table_sample(has_table_sample: bool) -> Result<(), ShapeDecline> {
    if has_table_sample {
        Err(ShapeDecline::TableSample)
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreflightRangeTableKind {
    BaseRelation,
    Synthetic,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PreflightRangeTableEntry {
    pub kind: PreflightRangeTableKind,
    pub eligible_base_relation: bool,
    pub has_table_sample: bool,
    pub permission_context_matches: bool,
}

/// A cleared subquery is metadata left by PostgreSQL's own pull-up pass, not
/// another input. Require independent planner inactivity evidence as well.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_excessive_bools)]
pub(super) struct PulledUpSubqueryFacts {
    pub is_subquery: bool,
    pub query_was_cleared: bool,
    pub security_barrier: bool,
    pub has_security_quals: bool,
    pub has_table_sample: bool,
    pub planner_slot_valid: bool,
    pub planner_slot_empty: bool,
    pub baserel_set_present: bool,
    pub is_baserel: bool,
}

pub(super) const fn is_pulled_up_subquery(facts: PulledUpSubqueryFacts) -> bool {
    facts.is_subquery
        && facts.query_was_cleared
        && !facts.security_barrier
        && !facts.has_security_quals
        && !facts.has_table_sample
        && facts.planner_slot_valid
        && facts.planner_slot_empty
        && facts.baserel_set_present
        && !facts.is_baserel
}

unsafe fn preflight_rte_facts(
    root: &pg_sys::PlannerInfo,
    query: &pg_sys::Query,
    varno: pg_sys::Index,
    rte: &pg_sys::RangeTblEntry,
) -> PreflightRangeTableEntry {
    let mut kind = preflight_kind(rte.rtekind);
    if rte.rtekind == pg_sys::RTEKind::RTE_SUBQUERY {
        let index = i32::try_from(varno).ok();
        let valid_slot = !root.simple_rel_array.is_null()
            && index.is_some_and(|index| index > 0 && index < root.simple_rel_array_size);
        // SAFETY: valid_slot proves the array/index; this only reads a slot.
        let empty_slot =
            valid_slot && unsafe { (*root.simple_rel_array.add(varno as usize)).is_null() };
        // SAFETY: all_baserels is a planner-owned bitmap (or NULL); index is
        // a checked nonnegative range-table identity, never a relation OID.
        let is_baserel =
            index.is_some_and(|index| unsafe { pg_sys::bms_is_member(index, root.all_baserels) });
        if is_pulled_up_subquery(PulledUpSubqueryFacts {
            is_subquery: true,
            query_was_cleared: rte.subquery.is_null(),
            security_barrier: rte.security_barrier,
            has_security_quals: !rte.securityQuals.is_null(),
            has_table_sample: !rte.tablesample.is_null(),
            planner_slot_valid: valid_slot,
            planner_slot_empty: empty_slot,
            baserel_set_present: !root.all_baserels.is_null(),
            is_baserel,
        }) {
            kind = PreflightRangeTableKind::Synthetic;
        }
    }
    let permission_context_matches = if kind == PreflightRangeTableKind::BaseRelation {
        let permission_index = usize::try_from(rte.perminfoindex)
            .ok()
            .and_then(|index| index.checked_sub(1));
        // SAFETY: list_item checks bounds in the current Query's permission
        // list. PostgreSQL retains these entries when pulling up a view/CTE.
        let info = permission_index.and_then(|index| unsafe {
            list_item::<pg_sys::RTEPermissionInfo>(query.rteperminfos, index)
        });
        info.is_some_and(|info| {
            // SAFETY: bounded non-NULL entry in this query's permission list.
            let info = unsafe { &*info };
            if info.checkAsUser != pg_sys::InvalidOid && root.glob.is_null() {
                return false;
            }
            info.relid == rte.relid
                && (info.checkAsUser == pg_sys::InvalidOid
                // SAFETY: backend-local effective role. Residency intentionally
                // checks that role; never substitute a view owner's identity.
                || info.checkAsUser == unsafe { pg_sys::GetUserId() })
        })
    } else {
        true
    };
    PreflightRangeTableEntry {
        kind,
        eligible_base_relation: rte.relid != pg_sys::InvalidOid
            && !rte.inh
            && rte.relkind as u8 == pg_sys::RELKIND_RELATION,
        has_table_sample: !rte.tablesample.is_null(),
        permission_context_matches,
    }
}

/// Preserve role-sensitive admission across prepared-plan reuse, including
/// negative-cache hits. Like PostgreSQL's useridiscurrent FDW gate, matching
/// checkAsUser to the loader's current role is a planning assumption.
pub(in crate::engine::ffi::planner_hooks) unsafe fn mark_permission_role_dependency(
    root: *mut pg_sys::PlannerInfo,
) {
    // SAFETY: hook-owned root; nullable fields are checked before dereference.
    if root.is_null() || unsafe { (*root).parse.is_null() || (*root).glob.is_null() } {
        return;
    }
    // SAFETY: parse was checked and is live for this planner invocation.
    let permissions = unsafe { (*(*root).parse).rteperminfos };
    for index in 0..list_len(permissions) {
        // SAFETY: bounded access to this query's permission list.
        if let Some(info) = unsafe { list_item::<pg_sys::RTEPermissionInfo>(permissions, index) }
            // SAFETY: list_item returns a non-NULL planner-owned permission node.
            && unsafe { (*info).checkAsUser } != pg_sys::InvalidOid
        {
            // SAFETY: glob is the shared planner state checked above.
            unsafe { (*(*root).glob).dependsOnRole = true };
            return;
        }
    }
}

fn preflight_kind(rtekind: pg_sys::RTEKind::Type) -> PreflightRangeTableKind {
    match rtekind {
        pg_sys::RTEKind::RTE_RELATION => PreflightRangeTableKind::BaseRelation,
        pg_sys::RTEKind::RTE_JOIN | pg_sys::RTEKind::RTE_RESULT | pg_sys::RTEKind::RTE_GROUP => {
            PreflightRangeTableKind::Synthetic
        }
        _ => PreflightRangeTableKind::Unsupported,
    }
}

/// Pure structural RTE gate shared by the cheap preflight and full extractor.
pub(super) fn preflight_range_table_entry(
    varno: pg_sys::Index,
    entry: PreflightRangeTableEntry,
) -> Result<bool, ShapeDecline> {
    if entry.kind == PreflightRangeTableKind::Synthetic {
        return Ok(false);
    }
    reject_table_sample(entry.has_table_sample)?;
    if entry.kind != PreflightRangeTableKind::BaseRelation || !entry.eligible_base_relation {
        return Err(ShapeDecline::UnsupportedRangeTableEntry { varno });
    }
    if !entry.permission_context_matches {
        return Err(ShapeDecline::UnsupportedPermissionContext { varno });
    }
    Ok(true)
}

/// Reject structurally unsupported range-table inputs without catalog access,
/// device discovery, residency inspection, or SPI.
///
/// # Safety
///
/// `root` must be null or a planner-owned pointer for the current invocation.
pub(super) unsafe fn preflight_base_relations(
    root: *mut pg_sys::PlannerInfo,
) -> Result<(), ShapeDecline> {
    // SAFETY: short-circuiting checks root for null before reading the parse
    // pointer from the caller-provided live PlannerInfo.
    if root.is_null() || unsafe { (*root).parse }.is_null() {
        return Err(ShapeDecline::NoAggregate);
    }
    // SAFETY: root and parse were checked above and are planner-owned.
    let query = unsafe { &*(*root).parse };
    if query.commandType != pg_sys::CmdType::CMD_SELECT {
        return Err(ShapeDecline::NotSelect);
    }
    if !query.hasAggs {
        return Err(ShapeDecline::NoAggregate);
    }
    for index in 0..list_len(query.rtable) {
        let varno = u32::try_from(index + 1).unwrap_or(u32::MAX);
        // SAFETY: index is bounded by list_len(query.rtable).
        let rte = unsafe { list_item::<pg_sys::RangeTblEntry>(query.rtable, index) }
            .ok_or(ShapeDecline::UnsupportedRangeTableEntry { varno })?;
        // SAFETY: list_item returned a non-null planner-owned RTE.
        let rte = unsafe { &*rte };
        preflight_range_table_entry(
            varno,
            // SAFETY: root, query and RTE share the same planner lifetime.
            unsafe { preflight_rte_facts(&*root, query, varno, rte) },
        )?;
    }
    Ok(())
}

/// Adapt planner-owned PostgreSQL nodes into the pure [`ShapeInput`].
///
/// # Safety
///
/// `root` and `output_rel` must remain valid throughout the call and must
/// belong to the current main-backend planner invocation.
pub(super) unsafe fn extract_input(
    root: *mut pg_sys::PlannerInfo,
    output_rel: *mut pg_sys::RelOptInfo,
    estimated_output_rows: u64,
    expected_reuses: NonZeroU32,
) -> Result<ShapeInput, ShapeDecline> {
    // SAFETY: root is read only after the explicit null guard; caller owns
    // the planner-pointer validity contract.
    if root.is_null() || unsafe { (*root).parse }.is_null() {
        return Err(ShapeDecline::NoAggregate);
    }
    // SAFETY: root is non-null and valid for this planner invocation.
    let root_ref = unsafe { &*root };
    // SAFETY: parse was checked non-null above and shares root's lifetime.
    let query = unsafe { &*root_ref.parse };
    if query.commandType != pg_sys::CmdType::CMD_SELECT {
        return Err(ShapeDecline::NotSelect);
    }
    if !query.hasAggs {
        return Err(ShapeDecline::NoAggregate);
    }

    // SAFETY: parse is the valid Query checked above; walk is synchronous.
    let inventory = unsafe { inventory_query(root_ref.parse) };
    let modifiers = ShapeModifiers {
        has_window_functions: query.hasWindowFuncs || inventory.saw_window_function,
        has_target_srfs: query.hasTargetSRFs,
        has_sublinks: query.hasSubLinks || inventory.saw_sublink,
        has_recursive_query: query.hasRecursive,
        has_modifying_cte: query.hasModifyingCTE,
        has_row_security: query.hasRowSecurity,
        has_distinct: !query.distinctClause.is_null(),
        has_grouping_sets: !query.groupingSets.is_null(),
        has_group_distinct: query.groupDistinct,
        // The neutral spec describes only the GPU aggregate. PostgreSQL owns
        // the post-aggregate HAVING expression and its evaluation semantics.
        has_having: false,
        has_set_operations: !query.setOperations.is_null(),
        has_row_marks: !query.rowMarks.is_null(),
    };
    super::builder::reject_modifiers(modifiers)?;

    // SAFETY: the internal list is allocated in this planner's memory context.
    let targets = unsafe { aggregate_scan_target_list(query) }?;
    // SAFETY: both expressions and target list belong to this live Query.
    if !query.havingQual.is_null()
        && unsafe {
            pg_sys::contain_volatile_functions(query.havingQual)
                || unsupported_having_input(query.havingQual, targets.cast())
        }
    {
        return Err(ShapeDecline::Having);
    }

    // SAFETY: all adapters inspect the same still-live planner Query.
    let groups = unsafe { group_keys(query) }?;
    let h3_fact = match groups.as_slice() {
        [
            PlannerGroupKey::Expression {
                source: GroupKeySource::H3CellToParent { cell, .. },
                ..
            },
        ] => Some(cell.relation_oid),
        _ => None,
    };
    // SAFETY: inventory pointers were collected synchronously from query.
    let (projections, aggregates) =
        unsafe { projections_and_aggregates(query, &inventory, targets, h3_fact) }?;
    let dense_where = aggregates.iter().any(|aggregate| {
        aggregate.expression == MeasureExpr::CountStar
            || matches!(aggregate.expression, MeasureExpr::Bytecode { .. })
            || matches!(aggregate.expression, MeasureExpr::Column(column)
            if matches!(column.type_oid, 20 | 21 | 23 | 1700))
            || matches!(aggregate.expression, MeasureExpr::Binary { lhs, rhs, .. }
                if lhs.type_oid == 23 && rhs.type_oid == 23)
    });
    // SAFETY: inventory pointers were collected synchronously from query.
    let (joins, relation_filters) =
        unsafe { joins_and_filters(root, query, &inventory, dense_where) }?;
    // SAFETY: root/query/inventory all belong to this planner invocation.
    let relations = unsafe { relation_shapes(root_ref, query, &inventory) }?;
    if output_rel.is_null() || estimated_output_rows == 0 {
        return Err(ShapeDecline::NoAggregate);
    }
    Ok(ShapeInput {
        relations,
        joins,
        group_keys: groups,
        aggregates,
        projections,
        relation_filters,
        estimated_output_rows,
        expected_reuses,
        modifiers,
    })
}

#[cfg(test)]
mod pure_tests {
    use super::*;

    #[test]
    fn dense_cardinalities_are_rounded_and_validated_individually() {
        assert_eq!(checked_cardinalities([]), Some(vec![]));
        assert_eq!(checked_cardinalities([10.0, 10.0, 10.0]), Some(vec![10; 3]));
        assert_eq!(checked_cardinalities([0.0, 2.1]), Some(vec![1, 3]));
        for invalid in [f64::NAN, f64::INFINITY, -1.0, u64::MAX as f64] {
            assert_eq!(checked_cardinalities([invalid]), None);
        }
    }

    #[test]
    fn filtered_star_count_uses_the_null_rejecting_predicate_input_only() {
        let input = test_column(1).column;
        let filter = FilterSpec::Ranges {
            input,
            ranges: vec![int4_range(-10, 10)],
        };
        for ranges in [vec![], vec![int4_range(-10, 10)]] {
            assert_eq!(
                lower_filtered_count(
                    MeasureExpr::CountStar,
                    AggregateKind::Count,
                    &FilterSpec::NullOrRanges { input, ranges }
                ),
                MeasureExpr::CountStar
            );
        }
        assert_eq!(
            lower_filtered_count(MeasureExpr::CountStar, AggregateKind::Count, &filter),
            MeasureExpr::Column(input)
        );
        assert_eq!(
            lower_filtered_count(
                MeasureExpr::CountStar,
                AggregateKind::Count,
                &FilterSpec::None
            ),
            MeasureExpr::CountStar
        );
        assert_eq!(
            lower_filtered_count(
                MeasureExpr::Column(test_column(2).column),
                AggregateKind::Count,
                &filter
            ),
            MeasureExpr::Column(test_column(2).column)
        );
        assert_eq!(
            lower_filtered_count(
                MeasureExpr::CountStar,
                AggregateKind::Count,
                &FilterSpec::Ranges {
                    input,
                    ranges: vec![]
                }
            ),
            MeasureExpr::CountStar
        );
    }

    fn test_column(attno: i32) -> PlannerColumn {
        PlannerColumn {
            varno: 1,
            column: ColumnRef {
                relation_oid: 42,
                attno,
                type_oid: u32::from(pg_sys::INT4OID),
            },
            type_modifier: -1,
            collation_oid: 0,
            collation_is_deterministic: true,
        }
    }

    fn int4_range(lo: i32, hi: i32) -> ScalarRange {
        ScalarRange {
            lo: ScalarValue::I32(lo),
            hi: ScalarValue::I32(hi),
        }
    }

    #[test]
    fn two_same_column_ranges_merge_to_one_bounded_interval() {
        let column = test_column(2);
        let mut filters = BTreeMap::new();
        add_filter(
            &mut filters,
            PendingFilter::Ranges {
                input: column,
                range: int4_range(200, i32::MAX),
                conjunct_count: 1,
            },
        )
        .expect("one scalar comparison remains supported");
        add_filter(
            &mut filters,
            PendingFilter::Ranges {
                input: column,
                range: int4_range(i32::MIN, 800),
                conjunct_count: 1,
            },
        )
        .expect("a second same-column bound should fuse into the interval");
        assert!(matches!(
            filters.get(&42),
            Some(PendingFilter::Ranges {
                input,
                range,
                conjunct_count: 2,
            }) if *input == column && *range == int4_range(200, 800)
        ));

        assert_eq!(
            add_filter(
                &mut filters,
                PendingFilter::Ranges {
                    input: column,
                    range: int4_range(250, i32::MAX),
                    conjunct_count: 1,
                },
            ),
            Err(ShapeDecline::MultipleRangePredicates),
            "the qualified fused shape is exactly two analyzed conjuncts"
        );
    }

    #[test]
    fn range_intersection_rejects_a_second_fact_column() {
        let mut filters = BTreeMap::new();
        add_filter(
            &mut filters,
            PendingFilter::Ranges {
                input: test_column(2),
                range: int4_range(200, i32::MAX),
                conjunct_count: 1,
            },
        )
        .expect("the first fact-column range should bind");
        assert_eq!(
            add_filter(
                &mut filters,
                PendingFilter::Ranges {
                    input: test_column(3),
                    range: int4_range(i32::MIN, 8),
                    conjunct_count: 1,
                },
            ),
            Err(ShapeDecline::MultipleFiltersPerRelation { relation_oid: 42 })
        );
    }

    #[test]
    fn range_intersection_does_not_expand_other_scalar_types() {
        let mut column = test_column(2);
        column.column.type_oid = u32::from(pg_sys::FLOAT8OID);
        let mut filters = BTreeMap::new();
        add_filter(
            &mut filters,
            PendingFilter::Ranges {
                input: column,
                range: ScalarRange {
                    lo: ScalarValue::F64(200.0),
                    hi: ScalarValue::F64(f64::INFINITY),
                },
                conjunct_count: 1,
            },
        )
        .expect("the first float range remains representable");
        assert_eq!(
            add_filter(
                &mut filters,
                PendingFilter::Ranges {
                    input: column,
                    range: ScalarRange {
                        lo: ScalarValue::F64(f64::NEG_INFINITY),
                        hi: ScalarValue::F64(800.0),
                    },
                    conjunct_count: 1,
                },
            ),
            Err(ShapeDecline::MultipleRangePredicates)
        );
    }

    #[test]
    fn range_intersection_rejects_empty_and_preserves_multi_range_reason() {
        let column = test_column(2);
        let mut empty = BTreeMap::new();
        add_filter(
            &mut empty,
            PendingFilter::Ranges {
                input: column,
                range: int4_range(900, i32::MAX),
                conjunct_count: 1,
            },
        )
        .expect("first bound");
        assert_eq!(
            add_filter(
                &mut empty,
                PendingFilter::Ranges {
                    input: column,
                    range: int4_range(i32::MIN, 800),
                    conjunct_count: 1,
                },
            ),
            Err(ShapeDecline::InvalidFilterRange)
        );

        let mut bounded = BTreeMap::new();
        for range in [int4_range(200, i32::MAX), int4_range(i32::MIN, 800)] {
            add_filter(
                &mut bounded,
                PendingFilter::Ranges {
                    input: column,
                    range,
                    conjunct_count: 1,
                },
            )
            .expect("two same-column bounds fuse");
        }
        assert_eq!(
            add_filter(
                &mut bounded,
                PendingFilter::Ranges {
                    input: test_column(3),
                    range: int4_range(i32::MIN, 8),
                    conjunct_count: 1,
                },
            ),
            Err(ShapeDecline::MultipleRangePredicates),
            "canonical SSBM range families retain their stable decline reason"
        );
    }

    #[test]
    fn scalar_bounds_cover_every_supported_planner_type() {
        let cases = [
            (
                ScalarValue::Bool(true),
                ScalarValue::Bool(false),
                ScalarValue::Bool(true),
            ),
            (
                ScalarValue::I32(7),
                ScalarValue::I32(i32::MIN),
                ScalarValue::I32(i32::MAX),
            ),
            (
                ScalarValue::I64(7),
                ScalarValue::I64(i64::MIN),
                ScalarValue::I64(i64::MAX),
            ),
            (
                ScalarValue::F32(7.0),
                ScalarValue::F32(f32::NEG_INFINITY),
                ScalarValue::F32(f32::INFINITY),
            ),
            (
                ScalarValue::F64(7.0),
                ScalarValue::F64(f64::NEG_INFINITY),
                ScalarValue::F64(f64::INFINITY),
            ),
            (
                ScalarValue::Date(7),
                ScalarValue::Date(i32::MIN),
                ScalarValue::Date(i32::MAX),
            ),
            (
                ScalarValue::Timestamp(7),
                ScalarValue::Timestamp(i64::MIN),
                ScalarValue::Timestamp(i64::MAX),
            ),
            (
                ScalarValue::TimestampTz(7),
                ScalarValue::TimestampTz(i64::MIN),
                ScalarValue::TimestampTz(i64::MAX),
            ),
        ];
        for (value, minimum, maximum) in cases {
            assert_eq!(scalar_min(value), minimum);
            assert_eq!(scalar_max(value), maximum);
        }
    }

    #[test]
    fn scalar_comparison_successor_and_predecessor_are_type_exact() {
        use std::cmp::Ordering;

        assert_eq!(
            scalar_cmp(ScalarValue::Bool(false), ScalarValue::Bool(true)),
            Some(Ordering::Less)
        );
        assert_eq!(
            scalar_cmp(ScalarValue::I32(2), ScalarValue::I32(1)),
            Some(Ordering::Greater)
        );
        assert_eq!(
            scalar_cmp(ScalarValue::I64(2), ScalarValue::I64(2)),
            Some(Ordering::Equal)
        );
        assert_eq!(
            scalar_cmp(ScalarValue::F32(1.0), ScalarValue::F32(2.0)),
            Some(Ordering::Less)
        );
        assert_eq!(
            scalar_cmp(ScalarValue::F64(2.0), ScalarValue::F64(1.0)),
            Some(Ordering::Greater)
        );
        assert_eq!(
            scalar_cmp(ScalarValue::Date(1), ScalarValue::Date(1)),
            Some(Ordering::Equal)
        );
        assert_eq!(
            scalar_cmp(ScalarValue::Timestamp(1), ScalarValue::Timestamp(2)),
            Some(Ordering::Less)
        );
        assert_eq!(
            scalar_cmp(ScalarValue::TimestampTz(2), ScalarValue::TimestampTz(1)),
            Some(Ordering::Greater)
        );
        assert_eq!(scalar_cmp(ScalarValue::I32(1), ScalarValue::I64(1)), None);
        assert_eq!(
            scalar_cmp(ScalarValue::F64(f64::NAN), ScalarValue::F64(1.0)),
            None
        );

        for (value, next, previous) in [
            (
                ScalarValue::I32(3),
                ScalarValue::I32(4),
                ScalarValue::I32(2),
            ),
            (
                ScalarValue::I64(3),
                ScalarValue::I64(4),
                ScalarValue::I64(2),
            ),
            (
                ScalarValue::Date(3),
                ScalarValue::Date(4),
                ScalarValue::Date(2),
            ),
            (
                ScalarValue::Timestamp(3),
                ScalarValue::Timestamp(4),
                ScalarValue::Timestamp(2),
            ),
            (
                ScalarValue::TimestampTz(3),
                ScalarValue::TimestampTz(4),
                ScalarValue::TimestampTz(2),
            ),
        ] {
            assert_eq!(scalar_next(value), Some(next));
            assert_eq!(scalar_previous(value), Some(previous));
        }
        for unsupported in [
            ScalarValue::Bool(false),
            ScalarValue::F32(1.0),
            ScalarValue::F64(1.0),
        ] {
            assert_eq!(scalar_next(unsupported), None);
            assert_eq!(scalar_previous(unsupported), None);
        }
        assert_eq!(scalar_next(ScalarValue::I32(i32::MAX)), None);
        assert_eq!(scalar_previous(ScalarValue::I32(i32::MIN)), None);
        assert_eq!(scalar_next(ScalarValue::I64(i64::MAX)), None);
        assert_eq!(scalar_previous(ScalarValue::I64(i64::MIN)), None);
        assert_eq!(scalar_next(ScalarValue::Date(i32::MAX)), None);
        assert_eq!(scalar_previous(ScalarValue::Date(i32::MIN)), None);
        assert_eq!(scalar_next(ScalarValue::Timestamp(i64::MAX)), None);
        assert_eq!(scalar_previous(ScalarValue::Timestamp(i64::MIN)), None);
        assert_eq!(scalar_next(ScalarValue::TimestampTz(i64::MAX)), None);
        assert_eq!(scalar_previous(ScalarValue::TimestampTz(i64::MIN)), None);
    }

    #[test]
    fn row_estimates_and_rte_kinds_fail_closed_at_boundaries() {
        assert_eq!(estimate_rows(f64::NAN), 0);
        assert_eq!(estimate_rows(f64::NEG_INFINITY), 0);
        assert_eq!(estimate_rows(-1.0), 0);
        assert_eq!(estimate_rows(0.0), 0);
        assert_eq!(estimate_rows(1.01), 2);
        assert_eq!(estimate_rows(f64::INFINITY), 0);
        assert_eq!(estimate_rows(u64::MAX as f64), u64::MAX);

        assert_eq!(
            preflight_kind(pg_sys::RTEKind::RTE_RELATION),
            PreflightRangeTableKind::BaseRelation
        );
        for synthetic in [
            pg_sys::RTEKind::RTE_JOIN,
            pg_sys::RTEKind::RTE_RESULT,
            pg_sys::RTEKind::RTE_GROUP,
        ] {
            assert_eq!(
                preflight_kind(synthetic),
                PreflightRangeTableKind::Synthetic
            );
        }
        assert_eq!(
            preflight_kind(pg_sys::RTEKind::RTE_FUNCTION),
            PreflightRangeTableKind::Unsupported
        );
    }
}
