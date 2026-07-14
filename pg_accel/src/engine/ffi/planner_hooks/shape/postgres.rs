//! PostgreSQL-node adapter for the neutral shape builder.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::c_void;
use std::num::NonZeroU32;

use pgrx::pg_sys::{self, Node, NodeTag};

use crate::engine::spec::{
    AggregateKind, AggregateOutput, AggregateSource, BinaryMeasureOp, ColumnRef, FilterSpec,
    MaskKind, MeasureExpr, ScalarRange, ScalarValue,
};

use super::{
    AggregateExpr, EquiJoin, InputProjection, OutputMetadata, PlannerColumn, PlannerGroupKey,
    RelationResidency, RelationShape, ShapeDecline, ShapeInput, ShapeModifiers,
};

#[derive(Default)]
struct ExpressionInventory {
    aggregate_nodes: Vec<*mut pg_sys::Aggref>,
    join_nodes: Vec<*mut pg_sys::JoinExpr>,
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
    unsafe { pg_sys::expression_tree_walker_impl(node, Some(inventory_walker), context) }
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

fn list_len(list: *mut pg_sys::List) -> usize {
    if list.is_null() {
        0
    } else {
        // SAFETY: non-null List pointers passed here belong to the active
        // planner tree and remain valid for the synchronous extraction.
        usize::try_from(unsafe { pg_sys::list_length(list) }).unwrap_or(usize::MAX)
    }
}

unsafe fn list_item<T>(list: *mut pg_sys::List, index: usize) -> Option<*mut T> {
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

unsafe fn direct_var(node: *mut Node, query: &pg_sys::Query) -> Option<PlannerColumn> {
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

unsafe fn group_keys(query: &pg_sys::Query) -> Result<Vec<PlannerGroupKey>, ShapeDecline> {
    let mut groups = Vec::with_capacity(list_len(query.groupClause));
    for index in 0..list_len(query.groupClause) {
        // SAFETY: index is bounded by list_len(groupClause).
        let clause = unsafe { list_item::<pg_sys::SortGroupClause>(query.groupClause, index) }
            .ok_or(ShapeDecline::UnsupportedGroupExpression)?;
        // SAFETY: clause is a valid SortGroupClause from groupClause.
        let clause = unsafe { &*clause };
        let sort_ref = clause.tleSortGroupRef;
        let mut found = None;
        for target_index in 0..list_len(query.targetList) {
            // SAFETY: target_index is bounded by list_len(targetList).
            let (target, expr) = unsafe { target_entry_expr(query.targetList, target_index) }
                .ok_or(ShapeDecline::UnsupportedGroupExpression)?;
            // SAFETY: target_entry_expr returned a valid TargetEntry.
            if unsafe { (*target).ressortgroupref } == sort_ref {
                // SAFETY: expr belongs to this Query's target list.
                if let Some(column) = unsafe { direct_var(expr, query) } {
                    // SAFETY: expr is a non-null analyzed target expression.
                    let expression_collation = unsafe { pg_sys::exprCollation(expr) };
                    // SAFETY: the column type is catalog-validated by direct_var.
                    let ordinary = unsafe {
                        default_equality_operator(pg_sys::Oid::from(column.column.type_oid))
                    }
                    .ok_or(ShapeDecline::UnsupportedGroupExpression)?;
                    // SAFETY: clause.eqop and the validated type OID came from
                    // this analyzed SortGroupClause.
                    let hashable = unsafe {
                        pg_sys::op_hashjoinable(
                            clause.eqop,
                            pg_sys::Oid::from(column.column.type_oid),
                        )
                    };
                    if pg_sys::Oid::from(column.collation_oid) != expression_collation
                        || !clause.hashable
                        || !is_ordinary_hash_equality(clause.eqop, ordinary, hashable)
                    {
                        return Err(ShapeDecline::UnsupportedGroupExpression);
                    }
                    found = Some(column);
                }
                break;
            }
        }
        let group = found.ok_or(ShapeDecline::UnsupportedGroupExpression)?;
        let group = PlannerGroupKey::Column(group);
        if groups.contains(&group) {
            return Err(ShapeDecline::UnsupportedGroupExpression);
        }
        groups.push(group);
    }
    Ok(groups)
}

pub(super) fn classify_aggregate(aggregate_oid: u32) -> Option<AggregateKind> {
    match aggregate_oid {
        pg_sys::F_SUM_INT2 | pg_sys::F_SUM_INT4 | pg_sys::F_SUM_FLOAT4 | pg_sys::F_SUM_FLOAT8 => {
            Some(AggregateKind::Sum)
        }
        pg_sys::F_COUNT_ANY | pg_sys::F_COUNT_ => Some(AggregateKind::Count),
        pg_sys::F_MIN_INT2
        | pg_sys::F_MIN_INT4
        | pg_sys::F_MIN_INT8
        | pg_sys::F_MIN_FLOAT4
        | pg_sys::F_MIN_FLOAT8
        | pg_sys::F_MIN_DATE
        | pg_sys::F_MIN_TIMESTAMP
        | pg_sys::F_MIN_TIMESTAMPTZ => Some(AggregateKind::Min),
        pg_sys::F_MAX_INT2
        | pg_sys::F_MAX_INT4
        | pg_sys::F_MAX_INT8
        | pg_sys::F_MAX_FLOAT4
        | pg_sys::F_MAX_FLOAT8
        | pg_sys::F_MAX_DATE
        | pg_sys::F_MAX_TIMESTAMP
        | pg_sys::F_MAX_TIMESTAMPTZ => Some(AggregateKind::Max),
        pg_sys::F_AVG_FLOAT4 | pg_sys::F_AVG_FLOAT8 => Some(AggregateKind::Avg),
        pg_sys::F_STDDEV_FLOAT8 | pg_sys::F_STDDEV_SAMP_FLOAT8 => Some(AggregateKind::StddevSamp),
        _ => None,
    }
}

pub(super) fn needs_numeric_accumulator(aggregate_oid: u32) -> bool {
    matches!(
        aggregate_oid,
        pg_sys::F_SUM_INT8
            | pg_sys::F_SUM_NUMERIC
            | pg_sys::F_AVG_INT2
            | pg_sys::F_AVG_INT4
            | pg_sys::F_AVG_INT8
            | pg_sys::F_AVG_NUMERIC
            | pg_sys::F_AVG_INTERVAL
    )
}

fn classify_binary_function(function_oid: u32) -> Option<BinaryMeasureOp> {
    match function_oid {
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

unsafe fn parse_aggregate(
    node: *mut Node,
    query: &pg_sys::Query,
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
        || !aggregate.aggfilter.is_null()
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
    Ok(AggregateExpr {
        expression,
        output: AggregateOutput {
            source: AggregateSource::Value,
            kind,
        },
        filter: FilterSpec::None,
    })
}

unsafe fn column_is_nullable(column: PlannerColumn) -> bool {
    // SAFETY: catalog lookup runs on the backend thread; relation OID and
    // attno came from a catalog-validated Var.
    let tuple = unsafe {
        pg_sys::SearchSysCache2(
            pg_sys::SysCacheIdentifier::ATTNUM as i32,
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
) -> Result<(Vec<InputProjection>, Vec<AggregateExpr>), ShapeDecline> {
    let mut projections = Vec::new();
    let mut aggregates = Vec::new();
    let mut direct_aggregate_nodes = BTreeSet::new();
    for index in 0..list_len(query.targetList) {
        // SAFETY: index is bounded by list_len(targetList).
        let (target, expression) = unsafe { target_entry_expr(query.targetList, index) }
            .ok_or(ShapeDecline::UnsupportedProjection)?;
        // SAFETY: target_entry_expr returned a valid TargetEntry.
        if unsafe { (*target).resjunk } {
            continue;
        }
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
        let aggregate = unsafe { parse_aggregate(stripped, query) }?;
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
        direct_aggregate_nodes.insert(stripped.addr());
    }
    if inventory
        .aggregate_nodes
        .iter()
        .any(|aggregate| !direct_aggregate_nodes.contains(&aggregate.addr()))
    {
        return Err(ShapeDecline::UnsupportedProjection);
    }
    Ok((projections, aggregates))
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

#[derive(Debug, Clone)]
enum PendingFilter {
    Ranges {
        input: PlannerColumn,
        range: ScalarRange,
    },
    Mask(PlannerColumn),
}

fn merge_range(left: ScalarRange, right: ScalarRange) -> Result<ScalarRange, ShapeDecline> {
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
    if scalar_cmp(lo, hi).is_none_or(|ordering| ordering.is_gt()) {
        return Err(ShapeDecline::InvalidFilterRange);
    }
    Ok(ScalarRange { lo, hi })
}

fn add_filter(
    filters: &mut BTreeMap<u32, PendingFilter>,
    filter: PendingFilter,
) -> Result<(), ShapeDecline> {
    let (relation_oid, column) = match &filter {
        PendingFilter::Ranges { input, .. } | PendingFilter::Mask(input) => {
            (input.column.relation_oid, *input)
        }
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
            },
            PendingFilter::Ranges {
                input: new_input,
                range: new_range,
            },
        ) if *input == new_input => {
            *existing_range = merge_range(*existing_range, new_range)?;
            Ok(())
        }
        (PendingFilter::Mask(existing_column), PendingFilter::Mask(new_column))
            if *existing_column == new_column =>
        {
            Ok(())
        }
        _ => Err(ShapeDecline::MultipleFiltersPerRelation {
            relation_oid: column.column.relation_oid,
        }),
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
        NodeTag::T_BoolExpr => {
            // SAFETY: the NodeTag arm proves the BoolExpr layout.
            let boolean = unsafe { &*node.cast::<pg_sys::BoolExpr>() };
            if boolean.boolop != pg_sys::BoolExprType::AND_EXPR {
                return Err(ShapeDecline::UnsupportedPredicate);
            }
            for index in 0..list_len(boolean.args) {
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

unsafe fn joins_and_filters(
    query: &pg_sys::Query,
    inventory: &ExpressionInventory,
) -> Result<ExtractedPredicates, ShapeDecline> {
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
    if !query.jointree.is_null() {
        // SAFETY: jointree was checked non-null and belongs to query.
        let where_qual = unsafe { (*query.jointree).quals };
        // SAFETY: where_qual and query belong to the same planner tree.
        unsafe { parse_qual(where_qual, query, &mut joins, &mut filters) }?;
    }
    let relation_filters = filters
        .into_iter()
        .map(|(relation_oid, filter)| {
            let filter = match filter {
                PendingFilter::Ranges { input, range } => FilterSpec::Ranges {
                    input: input.column,
                    ranges: vec![range],
                },
                PendingFilter::Mask(input) => FilterSpec::Mask {
                    input: input.column,
                    kind: MaskKind::Sql,
                },
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
            PreflightRangeTableEntry {
                kind: preflight_kind(rte_ref.rtekind),
                eligible_base_relation: rte_ref.relid != pg_sys::InvalidOid
                    && !rte_ref.inh
                    && rte_ref.relkind == pg_sys::RELKIND_RELATION as i8,
                has_table_sample: !rte_ref.tablesample.is_null(),
            },
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
            PreflightRangeTableEntry {
                kind: preflight_kind(rte.rtekind),
                eligible_base_relation: rte.relid != pg_sys::InvalidOid
                    && !rte.inh
                    && rte.relkind == pg_sys::RELKIND_RELATION as i8,
                has_table_sample: !rte.tablesample.is_null(),
            },
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
        has_having: !query.havingQual.is_null(),
        has_set_operations: !query.setOperations.is_null(),
        has_row_marks: !query.rowMarks.is_null(),
    };
    super::builder::reject_modifiers(modifiers)?;

    // SAFETY: all adapters inspect the same still-live planner Query.
    let groups = unsafe { group_keys(query) }?;
    // SAFETY: inventory pointers were collected synchronously from query.
    let (projections, aggregates) = unsafe { projections_and_aggregates(query, &inventory) }?;
    // SAFETY: inventory pointers were collected synchronously from query.
    let (joins, relation_filters) = unsafe { joins_and_filters(query, &inventory) }?;
    // SAFETY: root/query/inventory all belong to this planner invocation.
    let relations = unsafe { relation_shapes(root_ref, query, &inventory) }?;
    let estimated_output_rows = if output_rel.is_null() {
        0
    } else {
        // SAFETY: output_rel was checked non-null and is valid by contract.
        estimate_rows(unsafe { (*output_rel).rows })
    };
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
