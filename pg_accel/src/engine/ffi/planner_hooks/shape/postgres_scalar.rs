//! Bounded PostgreSQL-node lowering into SPG1. This does not inject a path or
//! bypass descriptor capability/cost admission. Call on planner-simplified
//! nodes: PostgreSQL's constant-folding and error timing remain authoritative.

use pgrx::FromDatum;
use pgrx::pg_sys::{self, Node, NodeTag};

use crate::engine::spec::scalar::{
    MAX_SCALAR_INPUTS, MAX_SCALAR_INSTRUCTIONS, ScalarInstruction as I, ScalarProgram, ScalarType,
};

use super::postgres::{direct_var, list_item, list_len};
use super::{PlannerColumn, ShapeDecline};

const MAX_LOWERING_DEPTH: usize = 64;

/// Logical inputs retain range-table occurrence identity until the caller
/// proves its single-fact relation contract. No resident pointers are stored.
#[derive(Debug, Clone)]
pub struct LoweredScalarExpression {
    pub inputs: Vec<PlannerColumn>,
    pub program: ScalarProgram,
}

impl LoweredScalarExpression {
    /// Encode into the existing opaque measure-program envelope. SPG1 owns
    /// its inner version; this does not change the outer AQS6 grammar or make
    /// the descriptor executor capable of consuming the program.
    #[must_use]
    pub fn into_measure_expression(self) -> crate::engine::spec::MeasureExpr {
        crate::engine::spec::MeasureExpr::Bytecode {
            inputs: self.inputs.into_iter().map(|input| input.column).collect(),
            result_type_oid: u32::from(match self.program.result_type() {
                ScalarType::Bool => pg_sys::BOOLOID,
                ScalarType::Int4 => pg_sys::INT4OID,
                ScalarType::Date => pg_sys::DATEOID,
            }),
            program: self.program.encode(),
        }
    }
}

fn declined<T>() -> Result<T, ShapeDecline> {
    Err(ShapeDecline::UnsupportedMeasureExpression)
}

fn scalar_type(oid: pg_sys::Oid) -> Result<ScalarType, ShapeDecline> {
    match oid {
        pg_sys::INT4OID => Ok(ScalarType::Int4),
        pg_sys::BOOLOID => Ok(ScalarType::Bool),
        pg_sys::DATEOID => Ok(ScalarType::Date),
        _ => declined(),
    }
}

/// Exact built-in implementation identity, never the operator's spelling.
fn operation(function: u32) -> Option<(I, usize)> {
    Some(match function {
        pg_sys::F_INT4PL => (I::Add, 2),
        pg_sys::F_INT4MI => (I::Sub, 2),
        pg_sys::F_INT4MUL => (I::Mul, 2),
        pg_sys::F_INT4DIV => (I::Div, 2),
        pg_sys::F_INT4MOD => (I::Mod, 2),
        pg_sys::F_INT4UM => (I::Neg, 1),
        pg_sys::F_DATE_PLI => (I::DateAddDays, 2),
        pg_sys::F_DATE_MII => (I::DateSubDays, 2),
        pg_sys::F_INTEGER_PL_DATE => (I::DaysAddDate, 2),
        pg_sys::F_DATE_MI => (I::DateDifference, 2),
        pg_sys::F_INT4EQ | pg_sys::F_BOOLEQ | pg_sys::F_DATE_EQ => (I::Eq, 2),
        pg_sys::F_INT4NE | pg_sys::F_BOOLNE | pg_sys::F_DATE_NE => (I::Ne, 2),
        pg_sys::F_INT4LT | pg_sys::F_BOOLLT | pg_sys::F_DATE_LT => (I::Lt, 2),
        pg_sys::F_INT4LE | pg_sys::F_BOOLLE | pg_sys::F_DATE_LE => (I::Le, 2),
        pg_sys::F_INT4GT | pg_sys::F_BOOLGT | pg_sys::F_DATE_GT => (I::Gt, 2),
        pg_sys::F_INT4GE | pg_sys::F_BOOLGE | pg_sys::F_DATE_GE => (I::Ge, 2),
        _ => return None,
    })
}

#[derive(Default)]
struct Lowerer {
    inputs: Vec<PlannerColumn>,
    code: Vec<I>,
    visited: usize,
    predicate_mask_node: *mut Node,
}

impl Lowerer {
    fn finish(self, result: ScalarType) -> Result<LoweredScalarExpression, ShapeDecline> {
        let types = self
            .inputs
            .iter()
            .map(|input| scalar_type(pg_sys::Oid::from(input.column.type_oid)))
            .collect::<Result<Vec<_>, _>>()?;
        let program = ScalarProgram::new(types, result, self.code)
            .and_then(ScalarProgram::fuse)
            .map_err(|_| ShapeDecline::UnsupportedMeasureExpression)?;
        Ok(LoweredScalarExpression {
            inputs: self.inputs,
            program,
        })
    }

    fn emit(&mut self, instruction: I) -> Result<usize, ShapeDecline> {
        if self.code.len() >= MAX_SCALAR_INSTRUCTIONS {
            return declined();
        }
        let index = self.code.len();
        self.code.push(instruction);
        Ok(index)
    }

    fn patch(&mut self, index: usize) -> Result<(), ShapeDecline> {
        let target = self.code.len() as u32; // bounded by emit
        let Some(instruction) = self.code.get_mut(index) else {
            return declined();
        };
        *instruction = match *instruction {
            I::Jump(_) => I::Jump(target),
            I::JumpIfNotTrue(_) => I::JumpIfNotTrue(target),
            I::JumpIfTrue(_) => I::JumpIfTrue(target),
            I::JumpIfFalse(_) => I::JumpIfFalse(target),
            I::JumpIfNotNull(_) => I::JumpIfNotNull(target),
            _ => return declined(),
        };
        Ok(())
    }

    unsafe fn node(
        &mut self,
        node: *mut Node,
        query: &pg_sys::Query,
        depth: usize,
    ) -> Result<(), ShapeDecline> {
        if node.is_null() || depth >= MAX_LOWERING_DEPTH || self.visited >= MAX_SCALAR_INSTRUCTIONS
        {
            return declined();
        }
        self.visited += 1;
        if node == self.predicate_mask_node {
            self.emit(I::PredicateMask)?;
            return Ok(());
        }
        // SAFETY: node is an analyzed expression owned by the caller's active
        // planner tree; exprType reads its declared SQL result type.
        let result = scalar_type(unsafe { pg_sys::exprType(node) })?;
        // SAFETY: the caller supplies valid PostgreSQL nodes; each following
        // cast is guarded by this concrete NodeTag and stays in this call.
        match unsafe { (*node).type_ } {
            NodeTag::T_Var => {
                // SAFETY: node/query belong to the same active planner tree.
                let column = unsafe { direct_var(node, query) }
                    .ok_or(ShapeDecline::UnsupportedMeasureExpression)?;
                // ColumnRef alone cannot distinguish aliases of one relation.
                // Keep this single-input-relation producer fail-closed until
                // relation-occurrence identities exist throughout the spec.
                if self
                    .inputs
                    .first()
                    .is_some_and(|first| first.varno != column.varno)
                {
                    return declined();
                }
                let index =
                    if let Some(index) = self.inputs.iter().position(|input| *input == column) {
                        index
                    } else {
                        if self.inputs.len() >= MAX_SCALAR_INPUTS {
                            return declined();
                        }
                        self.inputs.push(column);
                        self.inputs.len() - 1
                    };
                self.emit(I::Input(index as u32))?;
            }
            NodeTag::T_Const => {
                // SAFETY: the NodeTag proves Const layout.
                let constant = unsafe { &*node.cast::<pg_sys::Const>() };
                if !constant.constbyval
                    || constant.constlen != if result == ScalarType::Bool { 1 } else { 4 }
                    || constant.constcollid != pg_sys::InvalidOid
                {
                    return declined();
                }
                let instruction = if constant.constisnull {
                    I::Null(result)
                } else {
                    // SAFETY: exact type, by-value representation and width
                    // were checked above before interpreting the Datum.
                    unsafe {
                        match result {
                            ScalarType::Int4 => I::Int4(pg_sys::DatumGetInt32(constant.constvalue)),
                            ScalarType::Date => I::Date(pg_sys::DatumGetInt32(constant.constvalue)),
                            ScalarType::Bool => I::Bool(pg_sys::DatumGetBool(constant.constvalue)),
                        }
                    }
                };
                self.emit(instruction)?;
            }
            NodeTag::T_OpExpr | NodeTag::T_DistinctExpr | NodeTag::T_NullIfExpr => {
                // SAFETY: DistinctExpr/NullIfExpr alias OpExpr; the tag proves layout.
                let op = unsafe { &*node.cast::<pg_sys::OpExpr>() };
                if op.opretset
                    || op.opcollid != pg_sys::InvalidOid
                    || op.inputcollid != pg_sys::InvalidOid
                {
                    return declined();
                }
                // SAFETY: opno is the analyzed operator catalog identity.
                let function = unsafe { pg_sys::get_opcode(op.opno) };
                if op.opfuncid != pg_sys::InvalidOid && op.opfuncid != function {
                    return declined();
                }
                let (mut instruction, arity) = operation(u32::from(function))
                    .ok_or(ShapeDecline::UnsupportedMeasureExpression)?;
                // SAFETY: same checked node. DistinctExpr references equality,
                // not inequality: only the exact built-in equality identities
                // above may receive the non-strict SQL NULL semantics.
                if unsafe { (*node).type_ } == NodeTag::T_DistinctExpr {
                    if instruction != I::Eq || arity != 2 || result != ScalarType::Bool {
                        return declined();
                    }
                    instruction = I::IsDistinctFrom;
                }
                // SAFETY: same checked node. NULLIF uses the exact equality
                // implementation but returns its first operand's declared type.
                if unsafe { (*node).type_ } == NodeTag::T_NullIfExpr {
                    if instruction != I::Eq || arity != 2 {
                        return declined();
                    }
                    instruction = I::NullIf;
                }
                if list_len(op.args) != arity {
                    return declined();
                }
                for index in 0..arity {
                    // SAFETY: the analyzed argument list is length-checked;
                    // recursive lowering retains the same planner lifetime.
                    unsafe { self.argument(op.args, index, query, depth)? };
                }
                self.emit(instruction)?;
            }
            NodeTag::T_ScalarArrayOpExpr => {
                // SAFETY: the tag proves layout; all operands retain the same
                // analyzed query lifetime throughout membership lowering.
                unsafe {
                    self.membership(&*node.cast::<pg_sys::ScalarArrayOpExpr>(), query, depth)?;
                };
            }
            NodeTag::T_BoolExpr => {
                // SAFETY: the NodeTag proves BoolExpr layout.
                let expr = unsafe { &*node.cast::<pg_sys::BoolExpr>() };
                let count = list_len(expr.args);
                if count == 0 || count > MAX_SCALAR_INSTRUCTIONS {
                    return declined();
                }
                // SAFETY: nonempty analyzed argument list, bounded recursion.
                unsafe { self.argument(expr.args, 0, query, depth)? };
                match expr.boolop {
                    pg_sys::BoolExprType::NOT_EXPR if count == 1 => {
                        self.emit(I::Not)?;
                    }
                    pg_sys::BoolExprType::AND_EXPR | pg_sys::BoolExprType::OR_EXPR => {
                        let is_and = expr.boolop == pg_sys::BoolExprType::AND_EXPR;
                        let mut ends = Vec::new();
                        for index in 1..count {
                            self.emit(I::Dup)?;
                            ends.push(self.emit(if is_and {
                                I::JumpIfFalse(0)
                            } else {
                                I::JumpIfTrue(0)
                            })?);
                            // SAFETY: index is bounded by the analyzed list.
                            unsafe { self.argument(expr.args, index, query, depth)? };
                            self.emit(if is_and { I::And } else { I::Or })?;
                        }
                        for end in ends {
                            self.patch(end)?;
                        }
                    }
                    _ => return declined(),
                }
            }
            NodeTag::T_CaseExpr => {
                // SAFETY: the NodeTag proves CaseExpr layout.
                let expr = unsafe { &*node.cast::<pg_sys::CaseExpr>() };
                let count = list_len(expr.args);
                if count > MAX_SCALAR_INSTRUCTIONS || expr.casecollid != pg_sys::InvalidOid {
                    return declined();
                }
                let test_type = if expr.arg.is_null() {
                    None
                } else {
                    // SAFETY: the analyzed CASE owns its test expression.
                    unsafe { self.node(expr.arg.cast(), query, depth + 1)? };
                    // SAFETY: same live, analyzed expression.
                    Some(scalar_type(unsafe { pg_sys::exprType(expr.arg.cast()) })?)
                };
                let mut ends = Vec::new();
                for index in 0..count {
                    // SAFETY: CaseExpr.args contains planner-owned CaseWhen
                    // nodes; list_item bounds-checks before dereferencing.
                    let branch = unsafe { list_item::<pg_sys::CaseWhen>(expr.args, index) }
                        .ok_or(ShapeDecline::UnsupportedMeasureExpression)?;
                    // SAFETY: returned pointer is a live CaseWhen list entry.
                    let branch = unsafe { &*branch };
                    if branch.xpr.type_ != NodeTag::T_CaseWhen {
                        return declined();
                    }
                    // SAFETY: condition/result belong to the active CaseWhen.
                    unsafe {
                        if let Some(test_type) = test_type {
                            self.simple_case_condition(
                                branch.expr.cast(),
                                test_type,
                                index + 1 < count,
                                query,
                                depth + 1,
                            )?;
                        } else {
                            self.node(branch.expr.cast(), query, depth + 1)?;
                        }
                    };
                    let next = self.emit(I::JumpIfNotTrue(0))?;
                    if test_type.is_some() && index + 1 < count {
                        self.emit(I::Drop)?;
                    }
                    // SAFETY: result belongs to the same active tree.
                    unsafe { self.node(branch.result.cast(), query, depth + 1)? };
                    ends.push(self.emit(I::Jump(0))?);
                    self.patch(next)?;
                }
                if test_type.is_some() && count == 0 {
                    self.emit(I::Drop)?;
                }
                // SAFETY: analyzed CASE has a typed default (possibly NULL).
                unsafe { self.node(expr.defresult.cast(), query, depth + 1)? };
                for end in ends {
                    self.patch(end)?;
                }
            }
            NodeTag::T_MinMaxExpr => {
                // SAFETY: the tag proves MinMaxExpr layout in this active tree.
                let expr = unsafe { &*node.cast::<pg_sys::MinMaxExpr>() };
                let count = list_len(expr.args);
                if count == 0
                    || count > MAX_SCALAR_INSTRUCTIONS
                    || expr.minmaxcollid != pg_sys::InvalidOid
                    || expr.inputcollid != pg_sys::InvalidOid
                {
                    return declined();
                }
                let instruction = match expr.op {
                    pg_sys::MinMaxOp::IS_GREATEST => I::Greatest,
                    pg_sys::MinMaxOp::IS_LEAST => I::Least,
                    _ => return declined(),
                };
                let expected_comparison = match result {
                    ScalarType::Int4 => pg_sys::F_BTINT4CMP,
                    ScalarType::Bool => pg_sys::F_BTBOOLCMP,
                    ScalarType::Date => pg_sys::F_DATE_CMP,
                };
                // PostgreSQL's MinMax executor resolves its comparison through
                // the type cache, not an OpExpr. Reject a replaced comparator.
                // SAFETY: exact built-in type, lookup on the backend thread.
                let entry = unsafe {
                    pg_sys::lookup_type_cache(expr.minmaxtype, pg_sys::TYPECACHE_CMP_PROC as i32)
                };
                // SAFETY: non-NULL cache entry has CMP_PROC populated above.
                if entry.is_null() || unsafe { u32::from((*entry).cmp_proc) } != expected_comparison
                {
                    return declined();
                }
                for index in 0..count {
                    // SAFETY: bounded analyzed list, same query lifetime.
                    unsafe { self.argument(expr.args, index, query, depth)? };
                    if index > 0 {
                        // Built-in INT4/BOOL/DATE comparisons cannot throw or
                        // have side effects. Folding keeps bounded stack use
                        // while still evaluating every argument left to right.
                        self.emit(instruction)?;
                    }
                }
            }
            NodeTag::T_CoalesceExpr => {
                // SAFETY: the NodeTag proves CoalesceExpr layout.
                let expr = unsafe { &*node.cast::<pg_sys::CoalesceExpr>() };
                let count = list_len(expr.args);
                if count == 0
                    || count > MAX_SCALAR_INSTRUCTIONS
                    || expr.coalescecollid != pg_sys::InvalidOid
                {
                    return declined();
                }
                let mut ends = Vec::new();
                for index in 0..count {
                    // SAFETY: index is bounded by the analyzed argument list.
                    unsafe { self.argument(expr.args, index, query, depth)? };
                    if index + 1 < count {
                        ends.push(self.emit(I::JumpIfNotNull(0))?);
                    }
                }
                for end in ends {
                    self.patch(end)?;
                }
            }
            NodeTag::T_NullTest => {
                // SAFETY: the NodeTag proves NullTest layout.
                let expr = unsafe { &*node.cast::<pg_sys::NullTest>() };
                if expr.argisrow {
                    return declined();
                }
                // SAFETY: argument belongs to this scalar NullTest.
                unsafe { self.node(expr.arg.cast(), query, depth + 1)? };
                self.emit(match expr.nulltesttype {
                    pg_sys::NullTestType::IS_NULL => I::IsNull,
                    pg_sys::NullTestType::IS_NOT_NULL => I::IsNotNull,
                    _ => return declined(),
                })?;
            }
            NodeTag::T_BooleanTest => {
                // SAFETY: NodeTag proves BooleanTest layout and its argument
                // belongs to this analyzed planner tree.
                let expr = unsafe { &*node.cast::<pg_sys::BooleanTest>() };
                if result != ScalarType::Bool || expr.arg.is_null()
                    // SAFETY: checked non-NULL analyzed argument in this tree.
                    || unsafe { pg_sys::exprType(expr.arg.cast()) } != pg_sys::BOOLOID
                {
                    return declined();
                }
                // SAFETY: same analyzed Boolean argument, checked above.
                unsafe { self.node(expr.arg.cast(), query, depth + 1)? };
                use pg_sys::BoolTestType as B;
                match expr.booltesttype {
                    B::IS_UNKNOWN => {
                        self.emit(I::IsNull)?;
                    }
                    B::IS_NOT_UNKNOWN => {
                        self.emit(I::IsNotNull)?;
                    }
                    B::IS_TRUE | B::IS_NOT_TRUE | B::IS_FALSE | B::IS_NOT_FALSE => {
                        if matches!(expr.booltesttype, B::IS_FALSE | B::IS_NOT_FALSE) {
                            self.emit(I::Not)?;
                        }
                        let inverted =
                            matches!(expr.booltesttype, B::IS_NOT_TRUE | B::IS_NOT_FALSE);
                        let rejected = self.emit(I::JumpIfNotTrue(0))?;
                        self.emit(I::Bool(!inverted))?;
                        let end = self.emit(I::Jump(0))?;
                        self.patch(rejected)?;
                        self.emit(I::Bool(inverted))?;
                        self.patch(end)?;
                    }
                    _ => return declined(),
                }
            }
            // No implicit/explicit cast stripping: domains, narrowing casts,
            // custom functions, Params, SRFs and subqueries stay unsupported.
            _ => return declined(),
        }
        Ok(())
    }

    /// Constant-list IN / NOT IN. Only exact built-in equality/inequality is
    /// accepted. Sorting/deduplication is safe for these nonthrowing comparisons,
    /// but the scalar input itself must still execute exactly once, even for an
    /// empty or NULL array. Array dimensions/lower bounds do not affect ANY/ALL.
    unsafe fn membership(
        &mut self,
        expr: &pg_sys::ScalarArrayOpExpr,
        query: &pg_sys::Query,
        depth: usize,
    ) -> Result<(), ShapeDecline> {
        if list_len(expr.args) != 2 || expr.inputcollid != pg_sys::InvalidOid {
            return declined();
        }
        // SAFETY: both indices are within the checked analyzed argument list.
        let left = unsafe { list_item::<Node>(expr.args, 0) }
            .ok_or(ShapeDecline::UnsupportedMeasureExpression)?;
        // SAFETY: same checked list and planner lifetime.
        let right = unsafe { list_item::<Node>(expr.args, 1) }
            .ok_or(ShapeDecline::UnsupportedMeasureExpression)?;
        // SAFETY: operands and operator are from the active analyzed tree.
        let kind = scalar_type(unsafe { pg_sys::exprType(left) })?;
        // SAFETY: opno is the analyzed operator's catalog identity.
        let function = unsafe { pg_sys::get_opcode(expr.opno) };
        let (equal, unequal, hash, array_type) = match kind {
            // DATE array extraction is a separate admission; comparisons and
            // checked date arithmetic do not authorize interpreting new arrays.
            ScalarType::Date => return declined(),
            ScalarType::Int4 => (
                pg_sys::F_INT4EQ,
                pg_sys::F_INT4NE,
                pg_sys::F_HASHINT4,
                pg_sys::INT4ARRAYOID,
            ),
            ScalarType::Bool => (
                pg_sys::F_BOOLEQ,
                pg_sys::F_BOOLNE,
                pg_sys::F_HASHBOOL,
                pg_sys::BOOLARRAYOID,
            ),
        };
        if u32::from(function) != if expr.useOr { equal } else { unequal }
            || (expr.opfuncid != pg_sys::InvalidOid && expr.opfuncid != function)
            || (expr.hashfuncid != pg_sys::InvalidOid && u32::from(expr.hashfuncid) != hash)
            || (expr.negfuncid != pg_sys::InvalidOid
                && (expr.useOr || u32::from(expr.negfuncid) != equal))
            // SAFETY: right is an analyzed expression; guard the Const cast.
            || unsafe { (*right).type_ } != NodeTag::T_Const
        {
            return declined();
        }
        // SAFETY: the checked tag proves Const layout.
        let constant = unsafe { &*right.cast::<pg_sys::Const>() };
        if constant.consttype != array_type
            || constant.constbyval
            || constant.constlen != -1
            || constant.constcollid != pg_sys::InvalidOid
        {
            return declined();
        }
        let values = if constant.constisnull {
            None
        } else {
            Some(match kind {
                ScalarType::Date => return declined(),
                ScalarType::Int4 => {
                    // SAFETY: exact non-NULL, by-reference INT4[] Const Datum.
                    let array =
                        unsafe { pgrx::Array::<i32>::from_datum(constant.constvalue, false) }
                            .ok_or(ShapeDecline::UnsupportedMeasureExpression)?;
                    if array.len() > MAX_SCALAR_INSTRUCTIONS {
                        return declined();
                    }
                    array.iter().collect::<Vec<_>>()
                }
                ScalarType::Bool => {
                    // SAFETY: exact non-NULL, by-reference BOOL[] Const Datum.
                    let array =
                        unsafe { pgrx::Array::<bool>::from_datum(constant.constvalue, false) }
                            .ok_or(ShapeDecline::UnsupportedMeasureExpression)?;
                    if array.len() > MAX_SCALAR_INSTRUCTIONS {
                        return declined();
                    }
                    array
                        .iter()
                        .map(|value| value.map(i32::from))
                        .collect::<Vec<_>>()
                }
            })
        };
        // SAFETY: left belongs to the same analyzed tree, with bounded recursion.
        unsafe { self.node(left, query, depth + 1)? };
        self.membership_values(kind, values)?;
        if !expr.useOr {
            self.emit(I::Not)?;
        }
        Ok(())
    }

    fn membership_values(
        &mut self,
        kind: ScalarType,
        values: Option<Vec<Option<i32>>>,
    ) -> Result<(), ShapeDecline> {
        let Some(values) = values else {
            self.emit(I::Drop)?;
            self.emit(I::Null(ScalarType::Bool))?;
            return Ok(());
        };
        let has_null = values.iter().any(Option::is_none);
        let mut values = values.into_iter().flatten().collect::<Vec<_>>();
        values.sort_unstable();
        values.dedup();
        let literal = |value| match kind {
            ScalarType::Int4 => I::Int4(value),
            ScalarType::Date => I::Date(value),
            ScalarType::Bool => I::Bool(value != 0),
        };
        if values.is_empty() {
            self.emit(I::Drop)?;
            self.emit(if has_null {
                I::Null(ScalarType::Bool)
            } else {
                I::Bool(false)
            })?;
        } else if !has_null && values.len() == 1 {
            self.emit(literal(values[0]))?;
            self.emit(I::Eq)?;
        } else if kind == ScalarType::Bool && values.len() == 2 {
            // Both Boolean values are members: true for non-NULL, NULL for NULL.
            self.emit(I::Dup)?;
            self.emit(I::Eq)?;
        } else if !has_null
            && values
                .windows(2)
                .all(|pair| i64::from(pair[1]) == i64::from(pair[0]) + 1)
        {
            // A consecutive INT4 set is exactly a closed interval. Compare
            // directly, never subtract a bound (which could overflow).
            self.emit(I::Dup)?;
            self.emit(literal(values[0]))?;
            self.emit(I::Lt)?;
            let below = self.emit(I::JumpIfTrue(0))?;
            self.emit(literal(values[values.len() - 1]))?;
            self.emit(I::Le)?;
            let end = self.emit(I::Jump(0))?;
            self.patch(below)?;
            self.emit(I::Drop)?;
            self.emit(I::Bool(false))?;
            self.patch(end)?;
        } else {
            let mut hits = Vec::new();
            for value in values {
                self.emit(I::Dup)?;
                self.emit(literal(value))?;
                self.emit(I::Eq)?;
                let next = self.emit(I::JumpIfNotTrue(0))?;
                hits.push(self.emit(I::Jump(0))?);
                self.patch(next)?;
            }
            if has_null {
                self.emit(I::Drop)?;
                self.emit(I::Null(ScalarType::Bool))?;
            } else {
                // NOT (x=x) is false for every non-NULL supported value and
                // NULL for NULL. It uses the saved value, not a reevaluation.
                self.emit(I::Dup)?;
                self.emit(I::Eq)?;
                self.emit(I::Not)?;
            }
            let end = self.emit(I::Jump(0))?;
            for hit in hits {
                self.patch(hit)?;
            }
            self.emit(I::Drop)?;
            self.emit(I::Bool(true))?;
            self.patch(end)?;
        }
        Ok(())
    }

    /// The test value is on top of the stack at each WHEN. Accept only the
    /// parser's built-in equality or its Boolean simplifications; never let a
    /// CaseTestExpr escape its lexical CASE or reevaluate the test expression.
    /// The final comparison consumes that saved value: neither its result nor
    /// the default needs it. This avoids a redundant DUP/DROP on either path.
    unsafe fn simple_case_condition(
        &mut self,
        node: *mut Node,
        test_type: ScalarType,
        retain_test: bool,
        query: &pg_sys::Query,
        depth: usize,
    ) -> Result<(), ShapeDecline> {
        if node.is_null() || depth >= MAX_LOWERING_DEPTH || self.visited >= MAX_SCALAR_INSTRUCTIONS
        {
            return declined();
        }
        self.visited += 1;
        // SAFETY: node belongs to the current analyzed CASE condition.
        match unsafe { (*node).type_ } {
            NodeTag::T_CaseTestExpr if test_type == ScalarType::Bool => {
                // SAFETY: the tag proves CaseTestExpr layout.
                let test = unsafe { &*node.cast::<pg_sys::CaseTestExpr>() };
                if test.typeId != pg_sys::BOOLOID || test.collation != pg_sys::InvalidOid {
                    return declined();
                }
                if retain_test {
                    self.emit(I::Dup)?;
                }
            }
            NodeTag::T_OpExpr => {
                // SAFETY: tag proves OpExpr layout; its list remains live.
                let op = unsafe { &*node.cast::<pg_sys::OpExpr>() };
                // SAFETY: list_item checks the list length before indexing.
                let left = unsafe { list_item::<Node>(op.args, 0) }
                    .ok_or(ShapeDecline::UnsupportedMeasureExpression)?;
                // SAFETY: the analyzed operator owns its argument node.
                if unsafe { (*left).type_ } != NodeTag::T_CaseTestExpr {
                    // No local access: ordinary lowering still rejects any
                    // CaseTestExpr buried in an unqualified expression.
                    if !retain_test {
                        self.emit(I::Drop)?;
                    }
                    // SAFETY: same live analyzed condition and query.
                    return unsafe { self.node(node, query, depth) };
                }
                // SAFETY: guarded tag and analyzed operator catalog identity.
                let test = unsafe { &*left.cast::<pg_sys::CaseTestExpr>() };
                // SAFETY: opno is a live analyzed operator OID.
                let function = unsafe { pg_sys::get_opcode(op.opno) };
                let expected = match test_type {
                    ScalarType::Int4 => pg_sys::F_INT4EQ,
                    ScalarType::Date => pg_sys::F_DATE_EQ,
                    ScalarType::Bool => pg_sys::F_BOOLEQ,
                };
                if u32::from(function) != expected
                    || (op.opfuncid != pg_sys::InvalidOid && op.opfuncid != function)
                    || op.opretset
                    || op.opresulttype != pg_sys::BOOLOID
                    || op.opcollid != pg_sys::InvalidOid
                    || op.inputcollid != pg_sys::InvalidOid
                    || list_len(op.args) != 2
                    || scalar_type(test.typeId)? != test_type
                    || test.collation != pg_sys::InvalidOid
                {
                    return declined();
                }
                if retain_test {
                    self.emit(I::Dup)?;
                }
                // SAFETY: the equality has exactly two analyzed arguments.
                unsafe { self.argument(op.args, 1, query, depth)? };
                self.emit(I::Eq)?;
            }
            NodeTag::T_BoolExpr if test_type == ScalarType::Bool => {
                // SAFETY: tag proves BoolExpr layout.
                let expr = unsafe { &*node.cast::<pg_sys::BoolExpr>() };
                // SAFETY: list_item bounds-checks the analyzed list.
                let arg = unsafe { list_item::<Node>(expr.args, 0) }
                    .ok_or(ShapeDecline::UnsupportedMeasureExpression)?;
                // SAFETY: arg is owned by this live analyzed Boolean node.
                if expr.boolop != pg_sys::BoolExprType::NOT_EXPR
                    || list_len(expr.args) != 1
                    || unsafe { (*arg).type_ } != NodeTag::T_CaseTestExpr
                {
                    if !retain_test {
                        self.emit(I::Drop)?;
                    }
                    // SAFETY: same live analyzed condition and query.
                    return unsafe { self.node(node, query, depth) };
                }
                // SAFETY: exact Boolean simplification, same lexical local.
                unsafe {
                    self.simple_case_condition(arg, test_type, retain_test, query, depth + 1)?;
                };
                self.emit(I::Not)?;
            }
            // Simplification can remove the local entirely (e.g. WHEN NULL).
            // Nested CASE nodes manage their own test stack slot.
            _ => {
                if !retain_test {
                    self.emit(I::Drop)?;
                }
                // SAFETY: same live analyzed condition and query.
                return unsafe { self.node(node, query, depth) };
            }
        }
        Ok(())
    }

    unsafe fn argument(
        &mut self,
        args: *mut pg_sys::List,
        index: usize,
        query: &pg_sys::Query,
        depth: usize,
    ) -> Result<(), ShapeDecline> {
        // SAFETY: args belongs to the active expression; list_item bounds-checks.
        let node = unsafe { list_item::<Node>(args, index) }
            .ok_or(ShapeDecline::UnsupportedMeasureExpression)?;
        // SAFETY: argument has the caller's planner lifetime.
        unsafe { self.node(node, query, depth + 1) }
    }
}

/// Lower a planner-simplified expression without changing SQL admission.
///
/// # Safety
/// `node` and `query` must belong to the same live analyzed PostgreSQL query,
/// on the backend main thread. They must outlive this synchronous call.
pub unsafe fn lower_scalar_expression(
    node: *mut Node,
    query: &pg_sys::Query,
) -> Result<LoweredScalarExpression, ShapeDecline> {
    // Planner implicit-AND lists are not expressions accepted by exprType.
    // WHERE has a separate entry point that handles their ExecQual semantics.
    // SAFETY: the guarded pointer is a live Node under the caller's contract.
    if node.is_null() || unsafe { (*node).type_ } == NodeTag::T_List {
        return declined();
    }
    // SAFETY: node is live analyzed planner memory under the caller's contract.
    let result = scalar_type(unsafe { pg_sys::exprType(node) })?;
    let mut lowerer = Lowerer::default();
    // SAFETY: forwards the same valid planner tree and bounded initial depth.
    unsafe { lowerer.node(node, query, 0)? };
    lowerer.finish(result)
}

/// Lower a complete WHERE, including the planner's implicit-AND list. ExecQual
/// rejects both FALSE and NULL immediately for each implicit conjunct.
/// Match createplan.c's stable per-tuple-cost ordering of ordinary scan quals.
/// This is not an expression-tree rewrite: nested CASE/Boolean order is kept.
///
/// # Safety
/// `root`, `node` and `query` are the same live planner tree on the backend main
/// thread. The caller must have rejected row-security/security-barrier shapes.
pub(super) unsafe fn lower_where_expression(
    root: *mut pg_sys::PlannerInfo,
    node: *mut Node,
    query: &pg_sys::Query,
) -> Result<LoweredScalarExpression, ShapeDecline> {
    // SAFETY: same planner tree; no spatial substitution in ordinary WHERE.
    unsafe { lower_spatial_where_expression(root, node, query, std::ptr::null_mut()) }
}

/// The non-NULL mask node must be the catalog-validated spatial predicate in
/// this same WHERE tree. Its exact SQL mask is bound only at execution time.
pub(super) unsafe fn lower_spatial_where_expression(
    root: *mut pg_sys::PlannerInfo,
    node: *mut Node,
    query: &pg_sys::Query,
    predicate_mask_node: *mut Node,
) -> Result<LoweredScalarExpression, ShapeDecline> {
    if root.is_null() || node.is_null() {
        return declined();
    }
    // SAFETY: non-NULL planner node, checked before interpreting list layout.
    if unsafe { (*node).type_ } != NodeTag::T_List {
        // SAFETY: same live planner expression and query.
        let mut lowerer = Lowerer {
            predicate_mask_node,
            ..Lowerer::default()
        };
        // SAFETY: same live Boolean expression and validated optional spatial node.
        unsafe { lowerer.node(node, query, 0)? };
        let lowered = lowerer.finish(ScalarType::Bool)?;
        return if lowered.program.result_type() == ScalarType::Bool {
            Ok(lowered)
        } else {
            declined()
        };
    }
    let args = node.cast::<pg_sys::List>();
    let count = list_len(args);
    super::postgres::bounded_qual_conjunct_indices(count)?;
    let mut conjuncts = Vec::with_capacity(count);
    for index in 0..count {
        // SAFETY: bounded index within the checked planner-owned List.
        let item =
            unsafe { list_item::<Node>(args, index) }.ok_or(ShapeDecline::UnsupportedPredicate)?;
        // SAFETY: List elements are analyzed qualification expressions.
        if unsafe { pg_sys::exprType(item) } != pg_sys::BOOLOID {
            return declined();
        }
        // SAFETY: QualCost is a plain output struct; item/root share the live
        // planner invocation. PostgreSQL performs the authoritative function,
        // operator and nested-expression cost calculation on the backend thread.
        let mut cost: pg_sys::QualCost = unsafe { std::mem::zeroed() };
        // SAFETY: valid output pointer and analyzed qualifier/planner pointers.
        unsafe { pg_sys::cost_qual_eval_node(&raw mut cost, item, root) };
        conjuncts.push((cost.per_tuple, item));
    }
    order_where_conjuncts(&mut conjuncts)?;
    let mut lowerer = Lowerer {
        predicate_mask_node,
        ..Lowerer::default()
    };
    let mut rejected = Vec::with_capacity(count.saturating_sub(1));
    for (index, (_, item)) in conjuncts.into_iter().enumerate() {
        // SAFETY: same lowerer preserves identity across every conjunct.
        unsafe { lowerer.node(item, query, 0)? };
        if index + 1 != count {
            rejected.push(lowerer.emit(I::JumpIfNotTrue(0))?);
        }
    }
    if !rejected.is_empty() {
        let end = lowerer.emit(I::Jump(0))?;
        for branch in rejected {
            lowerer.patch(branch)?;
        }
        lowerer.emit(I::Bool(false))?;
        lowerer.patch(end)?;
    }
    lowerer.finish(ScalarType::Bool)
}

fn order_where_conjuncts<T>(conjuncts: &mut [(f64, T)]) -> Result<(), ShapeDecline> {
    if conjuncts
        .iter()
        .any(|(cost, _)| !cost.is_finite() || *cost < 0.0)
    {
        return Err(ShapeDecline::UnsupportedPredicate);
    }
    // Stable equal-cost order is part of PostgreSQL's order_qual_clauses
    // contract. Treat signed zero equally, as its comparison does.
    conjuncts.sort_by(|(left, _), (right, _)| {
        left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(())
}

/// Compile FILTER before its argument, yielding typed NULL for false/UNKNOWN.
/// For the caller's qualified strict aggregates this skips transition input
/// without removing the group. A missing argument means COUNT(*) and yields
/// literal 1 on accepted rows. One lowerer checks occurrence identity and
/// deduplicates columns across the predicate AND argument, not independently.
///
/// # Safety
/// The non-NULL predicate, optional argument and query belong to the same live,
/// planner-simplified tree on the backend main thread for this entire call.
pub(super) unsafe fn lower_filtered_scalar_expression(
    predicate: *mut Node,
    argument: Option<*mut Node>,
    query: &pg_sys::Query,
) -> Result<LoweredScalarExpression, ShapeDecline> {
    // SAFETY: guarded non-NULL live node under the caller's contract.
    if predicate.is_null() || unsafe { pg_sys::exprType(predicate) } != pg_sys::BOOLOID {
        return declined();
    }
    let result = if let Some(argument) = argument {
        if argument.is_null() {
            return declined();
        }
        // SAFETY: argument is non-NULL analyzed planner memory.
        scalar_type(unsafe { pg_sys::exprType(argument) })?
    } else {
        ScalarType::Int4
    };
    let mut lowerer = Lowerer::default();
    // SAFETY: same live analyzed predicate/query, bounded initial depth.
    unsafe { lowerer.node(predicate, query, 0)? };
    let rejected = lowerer.emit(I::JumpIfNotTrue(0))?;
    if let Some(argument) = argument {
        // SAFETY: same tree and lowerer; cross-expression aliases are checked.
        unsafe { lowerer.node(argument, query, 0)? };
    } else {
        lowerer.emit(I::Int4(1))?;
    }
    let end = lowerer.emit(I::Jump(0))?;
    lowerer.patch(rejected)?;
    lowerer.emit(I::Null(result))?;
    lowerer.patch(end)?;
    lowerer.finish(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::spec::scalar::tests::{Value, evaluate};

    #[test]
    fn where_conjunct_order_is_stable_and_uses_per_tuple_cost() {
        let mut conjuncts = [
            (0.02, 'a'),
            (0.0, 'b'),
            (0.01, 'c'),
            (-0.0, 'd'),
            (0.02, 'e'),
        ];
        order_where_conjuncts(&mut conjuncts).expect("valid costs");
        assert_eq!(conjuncts.map(|(_, item)| item), ['b', 'd', 'c', 'a', 'e']);
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.01] {
            assert!(order_where_conjuncts(&mut [(invalid, ())]).is_err());
        }
    }

    fn membership(
        kind: ScalarType,
        values: Option<Vec<Option<i32>>>,
        negate: bool,
    ) -> ScalarProgram {
        let mut lowerer = Lowerer::default();
        lowerer.emit(I::Input(0)).expect("input");
        lowerer.membership_values(kind, values).expect("membership");
        if negate {
            lowerer.emit(I::Not).expect("negation");
        }
        ScalarProgram::new(vec![kind], ScalarType::Bool, lowerer.code)
            .expect("typed membership CFG")
            .fuse()
            .expect("fused membership CFG")
    }

    #[test]
    fn membership_program_matches_three_valued_set_oracle() {
        for kind in [ScalarType::Int4, ScalarType::Bool] {
            let domain = if kind == ScalarType::Int4 {
                vec![
                    None,
                    Some(i32::MIN),
                    Some(-1),
                    Some(0),
                    Some(1),
                    Some(i32::MAX),
                ]
            } else {
                vec![None, Some(0), Some(1)]
            };
            for bits in 0..(1 << domain.len()) {
                let mut members = domain
                    .iter()
                    .enumerate()
                    .filter_map(|(index, value)| (bits & (1 << index) != 0).then_some(*value))
                    .collect::<Vec<_>>();
                // Membership is independent of order and duplicate constants.
                members.reverse();
                members.extend(members.clone());
                for negate in [false, true] {
                    let program = membership(kind, Some(members.clone()), negate);
                    for value in &domain {
                        let result = if members.is_empty() {
                            Some(false)
                        } else if value.is_none() {
                            None
                        } else if members.contains(value) {
                            Some(true)
                        } else if members.contains(&None) {
                            None
                        } else {
                            Some(false)
                        };
                        let expected = result.map_or(Value::Null(ScalarType::Bool), |found| {
                            Value::Bool(found != negate)
                        });
                        let input = match (kind, value) {
                            (_, None) => Value::Null(kind),
                            (ScalarType::Int4, Some(value)) => Value::Int4(*value),
                            (ScalarType::Date, Some(value)) => Value::Date(*value),
                            (ScalarType::Bool, Some(value)) => Value::Bool(*value != 0),
                        };
                        assert_eq!(
                            evaluate(&program, &[input]),
                            Ok(expected),
                            "{kind:?} {members:?} {value:?} negate={negate}"
                        );
                    }
                }
            }
            let program = membership(kind, None, false);
            assert_eq!(
                evaluate(&program, &[Value::Null(kind)]),
                Ok(Value::Null(ScalarType::Bool))
            );
        }
    }

    #[test]
    fn membership_compacts_intervals_without_overflow_or_lost_nulls() {
        for start in [i32::MIN, -128, i32::MAX - 255] {
            let values = (0..256)
                .map(|offset| Some(start + offset))
                .collect::<Vec<_>>();
            let program = membership(ScalarType::Int4, Some(values.clone()), false);
            assert_eq!(
                program.instructions().len(),
                10,
                "one bounded interval, not 256 tests"
            );
            for input in [
                None,
                Some(i32::MIN),
                Some(i32::MAX),
                Some(-1),
                Some(0),
                Some(start),
                Some(start + 255),
            ] {
                let expected = input.map_or(Value::Null(ScalarType::Bool), |value| {
                    Value::Bool(values.contains(&Some(value)))
                });
                let input = input.map_or(Value::Null(ScalarType::Int4), Value::Int4);
                assert_eq!(evaluate(&program, &[input]), Ok(expected));
            }
        }
        let mut lowerer = Lowerer::default();
        lowerer.emit(I::Input(0)).expect("input");
        assert!(
            lowerer
                .membership_values(
                    ScalarType::Int4,
                    Some((0..256).map(|i| Some(i * 2)).collect())
                )
                .is_err(),
            "sparse sets retain the instruction cap"
        );
    }
}
