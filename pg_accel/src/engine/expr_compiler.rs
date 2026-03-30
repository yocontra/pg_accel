//! Expression compiler: converts PostgreSQL expression trees to GPU bytecode.
//!
//! Called during `BeginCustomScan` (executor phase), NOT during planning.
//! The planner only checks whether expressions COULD benefit from GPU eval
//! using a lightweight heuristic. Actual compilation is deferred here.
//!
//! # Three-tier compilation
//!
//! 1. **Template match** — common patterns (col > const, BETWEEN, IN) use
//!    pre-compiled C kernels with zero interpretation overhead.
//! 2. **Bytecode** — general expressions compile to a stack-based program
//!    evaluated by the GPU interpreter.
//! 3. **CpuFallback** — unsupported nodes (string collation, numeric type,
//!    custom functions) fall back to PG's executor.

use crate::gpu::{PgaccelExprInstruction, PgaccelVal, PgaccelValTag};

/// Maximum stack depth the GPU interpreter supports.
const MAX_STACK_DEPTH: usize = 64;

// ── Opcode constants (must match pgaccel_expr.h) ────────────────────────

/// Expression opcode constants mirroring `pgaccel_expr_opcode` in C.
#[allow(dead_code)]
pub mod opcode {
    pub const LOAD_COL: u16 = 0;
    pub const LOAD_CONST: u16 = 1;
    pub const LOAD_NULL: u16 = 2;

    pub const ADD_I32: u16 = 10;
    pub const ADD_I64: u16 = 11;
    pub const ADD_F32: u16 = 12;
    pub const ADD_F64: u16 = 13;
    pub const SUB_I32: u16 = 14;
    pub const SUB_I64: u16 = 15;
    pub const SUB_F32: u16 = 16;
    pub const SUB_F64: u16 = 17;
    pub const MUL_I32: u16 = 18;
    pub const MUL_I64: u16 = 19;
    pub const MUL_F32: u16 = 20;
    pub const MUL_F64: u16 = 21;
    pub const DIV_I32: u16 = 22;
    pub const DIV_I64: u16 = 23;
    pub const DIV_F32: u16 = 24;
    pub const DIV_F64: u16 = 25;
    pub const MOD_I32: u16 = 26;
    pub const MOD_I64: u16 = 27;
    pub const NEG_I32: u16 = 28;
    pub const NEG_I64: u16 = 29;
    pub const NEG_F32: u16 = 30;
    pub const NEG_F64: u16 = 31;

    pub const EQ: u16 = 40;
    pub const NE: u16 = 41;
    pub const LT: u16 = 42;
    pub const LE: u16 = 43;
    pub const GT: u16 = 44;
    pub const GE: u16 = 45;

    pub const AND: u16 = 50;
    pub const OR: u16 = 51;
    pub const NOT: u16 = 52;

    pub const IS_NULL: u16 = 60;
    pub const IS_NOT_NULL: u16 = 61;

    pub const CAST_I32_I64: u16 = 70;
    pub const CAST_I32_F64: u16 = 71;
    pub const CAST_I64_F64: u16 = 72;
    pub const CAST_F32_F64: u16 = 73;
    pub const CAST_F64_F32: u16 = 74;
    pub const CAST_BOOL_I32: u16 = 75;

    pub const ABS_I32: u16 = 80;
    pub const ABS_I64: u16 = 81;
    pub const ABS_F64: u16 = 82;
    pub const SQRT_F64: u16 = 83;
    pub const CEIL_F64: u16 = 84;
    pub const FLOOR_F64: u16 = 85;
    pub const ROUND_F64: u16 = 86;

    pub const POW_F64: u16 = 90;

    pub const JUMP_IF_FALSE: u16 = 100;
    pub const JUMP: u16 = 101;

    pub const COALESCE: u16 = 110;
}

// ── Compiled expression variants ────────────────────────────────────────

/// Result of compiling a PG expression tree.
pub enum CompiledExpr {
    /// Pre-compiled template kernel — fastest path.
    Template(TemplateKernel),
    /// General bytecode program for the GPU interpreter.
    Bytecode(ExprProgram),
    /// Expression cannot be evaluated on GPU — use PG's executor.
    CpuFallback,
}

/// Pre-compiled template kernel matching a common pattern.
pub enum TemplateKernel {
    /// `col <cmp> const` — single comparison.
    CmpConst {
        col_idx: u32,
        cmp_opcode: u16,
        const_val: f64,
    },
    /// `col BETWEEN lo AND hi`.
    Between { col_idx: u32, lo: f64, hi: f64 },
    /// `col IN (v0, v1, ..., vN)` — up to 16 values.
    InList { col_idx: u32, values: Vec<f64> },
    /// `col IS NULL` or `col IS NOT NULL`.
    IsNull { col_idx: u32, check_not_null: bool },
    /// `col1 <cmp1> const1 AND col2 <cmp2> const2`.
    TwoPredAnd {
        col1_idx: u32,
        cmp1_opcode: u16,
        const1_val: f64,
        col2_idx: u32,
        cmp2_opcode: u16,
        const2_val: f64,
    },
}

/// Bytecode program ready for the GPU interpreter.
pub struct ExprProgram {
    pub instructions: Vec<PgaccelExprInstruction>,
    pub const_pool: Vec<PgaccelVal>,
    pub max_stack: usize,
    pub num_cols: usize,
    /// Column indices referenced by the program (for selective transposition).
    pub referenced_cols: Vec<usize>,
}

// ── Bytecode builder ────────────────────────────────────────────────────

/// Builder for constructing an `ExprProgram` incrementally.
pub struct ExprProgramBuilder {
    instructions: Vec<PgaccelExprInstruction>,
    const_pool: Vec<PgaccelVal>,
    num_cols: usize,
    referenced_cols: Vec<usize>,
    stack_depth: usize,
    max_stack: usize,
}

impl ExprProgramBuilder {
    /// Create a new builder for an expression over `num_cols` input columns.
    #[must_use]
    pub fn new(num_cols: usize) -> Self {
        Self {
            instructions: Vec::new(),
            const_pool: Vec::new(),
            num_cols,
            referenced_cols: Vec::new(),
            stack_depth: 0,
            max_stack: 0,
        }
    }

    /// Emit an instruction.
    pub fn emit(&mut self, opcode: u16, arg: u32) {
        self.instructions.push(PgaccelExprInstruction {
            opcode,
            pad: 0,
            arg,
        });
    }

    /// Emit LOAD_COL and track the column reference.
    pub fn emit_load_col(&mut self, col_idx: u32) {
        if !self.referenced_cols.contains(&(col_idx as usize)) {
            self.referenced_cols.push(col_idx as usize);
        }
        self.emit(opcode::LOAD_COL, col_idx);
        self.push_stack();
    }

    /// Emit LOAD_CONST with a value added to the constant pool.
    pub fn emit_load_const(&mut self, val: PgaccelVal) -> u32 {
        let idx = self.const_pool.len() as u32;
        self.const_pool.push(val);
        self.emit(opcode::LOAD_CONST, idx);
        self.push_stack();
        idx
    }

    /// Emit LOAD_NULL.
    pub fn emit_load_null(&mut self) {
        self.emit(opcode::LOAD_NULL, 0);
        self.push_stack();
    }

    /// Emit a binary op (pops 2, pushes 1).
    pub fn emit_binop(&mut self, opcode: u16) {
        self.emit(opcode, 0);
        self.pop_stack(); // net: pop 2 push 1 = pop 1
    }

    /// Emit a unary op (pops 1, pushes 1) — no stack change.
    pub fn emit_unaryop(&mut self, opcode: u16) {
        self.emit(opcode, 0);
    }

    /// Current instruction count (for jump targets).
    #[must_use]
    pub fn current_pc(&self) -> u32 {
        self.instructions.len() as u32
    }

    /// Patch a jump instruction's target.
    pub fn patch_jump(&mut self, pc: u32, target: u32) {
        if let Some(inst) = self.instructions.get_mut(pc as usize) {
            inst.arg = target;
        }
    }

    fn push_stack(&mut self) {
        self.stack_depth += 1;
        if self.stack_depth > self.max_stack {
            self.max_stack = self.stack_depth;
        }
    }

    fn pop_stack(&mut self) {
        self.stack_depth = self.stack_depth.saturating_sub(1);
    }

    /// Build the final program. Returns `None` if stack depth exceeds limit.
    #[must_use]
    pub fn build(self) -> Option<ExprProgram> {
        if self.max_stack > MAX_STACK_DEPTH {
            return None;
        }
        Some(ExprProgram {
            instructions: self.instructions,
            const_pool: self.const_pool,
            max_stack: self.max_stack,
            num_cols: self.num_cols,
            referenced_cols: self.referenced_cols,
        })
    }
}

// ── Lightweight compilability heuristic (for planner) ───────────────────

/// Quick check whether an expression tree looks compilable to GPU bytecode.
///
/// Called from the planner hook. Does NOT walk the full tree — just checks
/// the top-level node type and estimated row count. Returns `true` if the
/// expression is worth attempting compilation in `BeginCustomScan`.
#[must_use]
pub fn looks_compilable(estimated_rows: f64, num_clauses: usize) -> bool {
    // Must have meaningful row count and at least one clause
    estimated_rows >= 1000.0 && num_clauses > 0
}

/// Map a PostgreSQL comparison operator OID to a GPU comparison opcode.
///
/// Returns `None` for non-comparison operators.
#[must_use]
pub fn pg_cmp_op_to_opcode(op_name: &str) -> Option<u16> {
    match op_name {
        "=" => Some(opcode::EQ),
        "<>" | "!=" => Some(opcode::NE),
        "<" => Some(opcode::LT),
        "<=" => Some(opcode::LE),
        ">" => Some(opcode::GT),
        ">=" => Some(opcode::GE),
        _ => None,
    }
}

/// Determine the appropriate arithmetic opcode for a given operation and type.
///
/// Returns `None` if the type is not supported on GPU.
#[must_use]
pub fn arithmetic_opcode(op: &str, val_tag: PgaccelValTag) -> Option<u16> {
    match (op, val_tag) {
        ("+", PgaccelValTag::Int32) => Some(opcode::ADD_I32),
        ("+", PgaccelValTag::Int64) => Some(opcode::ADD_I64),
        ("+", PgaccelValTag::Float32) => Some(opcode::ADD_F32),
        ("+", PgaccelValTag::Float64) => Some(opcode::ADD_F64),
        ("-", PgaccelValTag::Int32) => Some(opcode::SUB_I32),
        ("-", PgaccelValTag::Int64) => Some(opcode::SUB_I64),
        ("-", PgaccelValTag::Float32) => Some(opcode::SUB_F32),
        ("-", PgaccelValTag::Float64) => Some(opcode::SUB_F64),
        ("*", PgaccelValTag::Int32) => Some(opcode::MUL_I32),
        ("*", PgaccelValTag::Int64) => Some(opcode::MUL_I64),
        ("*", PgaccelValTag::Float32) => Some(opcode::MUL_F32),
        ("*", PgaccelValTag::Float64) => Some(opcode::MUL_F64),
        ("/", PgaccelValTag::Int32) => Some(opcode::DIV_I32),
        ("/", PgaccelValTag::Int64) => Some(opcode::DIV_I64),
        ("/", PgaccelValTag::Float32) => Some(opcode::DIV_F32),
        ("/", PgaccelValTag::Float64) => Some(opcode::DIV_F64),
        ("%", PgaccelValTag::Int32) => Some(opcode::MOD_I32),
        ("%", PgaccelValTag::Int64) => Some(opcode::MOD_I64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_simple_compare() {
        let mut b = ExprProgramBuilder::new(2);
        b.emit_load_col(0);
        b.emit_load_const(PgaccelVal::from_i32(42));
        b.emit_binop(opcode::GT);
        let prog = b.build().expect("should build");
        assert_eq!(prog.instructions.len(), 3);
        assert_eq!(prog.const_pool.len(), 1);
        assert_eq!(prog.max_stack, 2);
        assert_eq!(prog.referenced_cols, vec![0]);
    }

    #[test]
    fn builder_stack_overflow_rejected() {
        let mut b = ExprProgramBuilder::new(1);
        // Push 65 values — exceeds MAX_STACK_DEPTH of 64
        for i in 0..65 {
            b.emit_load_const(PgaccelVal::from_i32(i));
        }
        assert!(b.build().is_none());
    }

    #[test]
    fn looks_compilable_basic() {
        assert!(looks_compilable(10000.0, 1));
        assert!(!looks_compilable(100.0, 1));
        assert!(!looks_compilable(10000.0, 0));
    }

    #[test]
    fn cmp_op_mapping() {
        assert_eq!(pg_cmp_op_to_opcode("="), Some(opcode::EQ));
        assert_eq!(pg_cmp_op_to_opcode(">"), Some(opcode::GT));
        assert_eq!(pg_cmp_op_to_opcode("LIKE"), None);
    }

    #[test]
    fn arithmetic_opcode_mapping() {
        assert_eq!(
            arithmetic_opcode("+", PgaccelValTag::Int32),
            Some(opcode::ADD_I32)
        );
        assert_eq!(
            arithmetic_opcode("/", PgaccelValTag::Float64),
            Some(opcode::DIV_F64)
        );
        assert_eq!(arithmetic_opcode("+", PgaccelValTag::Null), None);
    }

    #[test]
    fn jump_patching() {
        let mut b = ExprProgramBuilder::new(1);
        b.emit_load_col(0);
        let jump_pc = b.current_pc();
        b.emit(opcode::JUMP_IF_FALSE, 0); // placeholder
        b.emit_load_const(PgaccelVal::from_i32(1));
        let target = b.current_pc();
        b.patch_jump(jump_pc, target);
        let prog = b.build().expect("should build");
        assert_eq!(prog.instructions[jump_pc as usize].arg, target);
    }
}
