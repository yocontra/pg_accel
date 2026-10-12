//! Typed, forward-only scalar programs for resident expression producers.
//!
//! This is a separate contract from the legacy expression VM: selected
//! aggregates need exact error propagation and lazy CASE/COALESCE branches,
//! not a Boolean "uncertain" result. Validation proves types and stack shape
//! on every reachable path. No program can loop or execute an unchecked jump.
//! This module does not by itself admit SQL or provide a host execution path.

use std::fmt;

pub const SCALAR_PROGRAM_MAGIC: i32 = 0x5350_4731; // SPG1
pub const MAX_SCALAR_INPUTS: usize = 64;
pub const MAX_SCALAR_INSTRUCTIONS: usize = 256;
pub const MAX_SCALAR_STACK: usize = 32;

/// PostgreSQL DATE offsets from 2000-01-01; the upper finite bound is exclusive.
pub const DATE_MIN: i32 = -2_451_545;
pub const DATE_END: i32 = 2_145_031_949;

const fn valid_date(value: i32) -> bool {
    value == i32::MIN || value == i32::MAX || (value >= DATE_MIN && value < DATE_END)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarType {
    Bool,
    Int4,
    Date,
}

impl ScalarType {
    const fn wire(self) -> i32 {
        match self {
            Self::Bool => 1,
            Self::Int4 => 2,
            Self::Date => 6,
        }
    }

    fn from_wire(word: i32) -> Result<Self, ScalarProgramError> {
        match word {
            1 => Ok(Self::Bool),
            2 => Ok(Self::Int4),
            6 => Ok(Self::Date),
            _ => Err(ScalarProgramError::InvalidType),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarArithmetic {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

impl ScalarArithmetic {
    const fn index(self) -> i32 {
        match self {
            Self::Add => 0,
            Self::Sub => 1,
            Self::Mul => 2,
            Self::Div => 3,
            Self::Mod => 4,
        }
    }

    fn from_instruction(instruction: ScalarInstruction) -> Option<Self> {
        match instruction {
            ScalarInstruction::Add => Some(Self::Add),
            ScalarInstruction::Sub => Some(Self::Sub),
            ScalarInstruction::Mul => Some(Self::Mul),
            ScalarInstruction::Div => Some(Self::Div),
            ScalarInstruction::Mod => Some(Self::Mod),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarInstruction {
    Input(u32),
    /// Read the SQL predicate mask supplied by the enclosing WHERE consumer.
    /// This is not a column and is invalid without that execution context.
    PredicateMask,
    Int4(i32),
    Date(i32),
    Bool(bool),
    Null(ScalarType),
    Dup,
    /// Discard one value, including NULL (once-evaluated simple CASE test).
    Drop,
    /// Replace a NULL INT4 with a literal, retaining a non-NULL input.
    CoalesceInt4(i32),
    /// Checked INT4 arithmetic with a non-NULL literal RHS.
    Int4Immediate(ScalarArithmetic, i32),
    /// Checked INT4 arithmetic after replacing a NULL RHS with a literal.
    Int4CoalesceRhs(ScalarArithmetic, i32),
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Neg,
    DateAddDays,
    DateSubDays,
    DaysAddDate,
    DateDifference,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// SQL IS DISTINCT FROM: equal-typed operands, always non-NULL Boolean.
    IsDistinctFrom,
    /// SQL NULLIF: evaluate both operands, then null the LHS on equality.
    NullIf,
    /// PostgreSQL row-wise extrema ignore NULLs, but evaluate every operand.
    Greatest,
    Least,
    And,
    Or,
    Not,
    IsNull,
    IsNotNull,
    Jump(u32),
    /// Pop a Boolean; NULL and false take the branch (searched CASE).
    JumpIfNotTrue(u32),
    /// Pop a Boolean; only true takes the branch (short-circuit OR).
    JumpIfTrue(u32),
    /// Pop a Boolean; only false takes the branch (short-circuit AND).
    JumpIfFalse(u32),
    /// Retain a non-NULL value and jump, otherwise pop it (lazy COALESCE).
    JumpIfNotNull(u32),
    /// Pop two equal-typed values; branch when equality is false or NULL.
    EqJumpIfNotTrue(u32),
}

impl ScalarInstruction {
    fn target_mut(&mut self) -> Option<&mut u32> {
        match self {
            Self::Jump(target)
            | Self::JumpIfNotTrue(target)
            | Self::JumpIfTrue(target)
            | Self::JumpIfFalse(target)
            | Self::JumpIfNotNull(target)
            | Self::EqJumpIfNotTrue(target) => Some(target),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarProgramError {
    InvalidShape,
    InvalidHeader,
    InvalidType,
    UnknownOpcode,
    NonCanonicalOperand,
    InvalidInput,
    InvalidJump,
    UnreachableInstruction,
    StackUnderflow,
    StackOverflow,
    TypeMismatch,
    IncompatibleBranches,
    InvalidResult,
}

impl fmt::Display for ScalarProgramError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid scalar program: {self:?}")
    }
}

impl std::error::Error for ScalarProgramError {}

/// A validated program. Fields are private so decoded data cannot bypass proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalarProgram {
    inputs: Vec<ScalarType>,
    result: ScalarType,
    instructions: Vec<ScalarInstruction>,
    max_stack: usize,
}

/// Allocation-free wire proof for runtime binding. The same parser and
/// control-flow validator back the owned logical program and this snapshot.
pub struct ValidatedScalarWire {
    inputs: [ScalarType; MAX_SCALAR_INPUTS],
    input_count: usize,
    result: ScalarType,
    instructions: [ScalarInstruction; MAX_SCALAR_INSTRUCTIONS],
    instruction_count: usize,
    max_stack: usize,
}

impl ValidatedScalarWire {
    #[must_use]
    pub fn uses_predicate_mask(&self) -> bool {
        self.instructions[..self.instruction_count].contains(&ScalarInstruction::PredicateMask)
    }

    /// Prove a non-NULL result on every successful path, treating every input
    /// as nullable. This does not prove freedom from arithmetic errors: callers
    /// must retain expression evaluation. Forward edges permit one bounded,
    /// allocation-free dataflow pass; joins intersect definitely-non-NULL bits.
    #[must_use]
    pub fn result_is_non_null(&self) -> bool {
        use ScalarInstruction as I;
        #[derive(Clone, Copy)]
        struct State {
            bits: u32,
            depth: usize,
        }
        impl State {
            fn push(&mut self, non_null: bool) {
                let bit = 1_u32 << self.depth;
                self.bits = (self.bits & !bit) | if non_null { bit } else { 0 };
                self.depth += 1;
            }
            fn pop(&mut self) -> bool {
                self.depth -= 1;
                self.bits & (1_u32 << self.depth) != 0
            }
        }
        let mut states = [None; MAX_SCALAR_INSTRUCTIONS + 1];
        states[0] = Some(State { bits: 0, depth: 0 });
        let merge = |states: &mut [Option<State>], target: usize, incoming: State| {
            if let Some(previous) = &mut states[target] {
                debug_assert_eq!(previous.depth, incoming.depth);
                previous.bits &= incoming.bits;
            } else {
                states[target] = Some(incoming);
            }
        };
        for pc in 0..self.instruction_count {
            // This type's private fields can only come from full CFG proof.
            let Some(mut state) = states[pc] else {
                return false;
            };
            match self.instructions[pc] {
                I::Input(_) | I::Null(_) | I::PredicateMask => state.push(false),
                I::Int4(_) | I::Date(_) | I::Bool(_) => state.push(true),
                I::Dup => {
                    let non_null = state.bits & (1_u32 << (state.depth - 1)) != 0;
                    state.push(non_null);
                }
                I::Drop => {
                    state.pop();
                }
                I::Neg | I::Not | I::Int4Immediate(_, _) => {}
                I::Int4CoalesceRhs(_, _) => {
                    // The RHS becomes non-NULL; only the LHS controls strictness.
                    state.pop();
                }
                I::IsNull | I::IsNotNull | I::CoalesceInt4(_) => {
                    state.pop();
                    state.push(true);
                }
                I::IsDistinctFrom => {
                    state.pop();
                    state.pop();
                    state.push(true);
                }
                I::NullIf => {
                    state.pop();
                    state.pop();
                    state.push(false);
                }
                I::Greatest | I::Least => {
                    let rhs = state.pop();
                    let lhs = state.pop();
                    state.push(lhs || rhs);
                }
                I::Add
                | I::DateAddDays
                | I::DateSubDays
                | I::DaysAddDate
                | I::DateDifference
                | I::Sub
                | I::Mul
                | I::Div
                | I::Mod
                | I::Eq
                | I::Ne
                | I::Lt
                | I::Le
                | I::Gt
                | I::Ge
                | I::And
                | I::Or => {
                    let rhs = state.pop();
                    let lhs = state.pop();
                    state.push(lhs && rhs);
                }
                I::Jump(target) => {
                    merge(&mut states, target as usize, state);
                    continue;
                }
                I::JumpIfNotTrue(target) | I::JumpIfTrue(target) | I::JumpIfFalse(target) => {
                    state.pop();
                    merge(&mut states, target as usize, state);
                }
                I::JumpIfNotNull(target) => {
                    let mut taken = state;
                    taken.bits |= 1_u32 << (taken.depth - 1);
                    merge(&mut states, target as usize, taken);
                    state.pop();
                }
                I::EqJumpIfNotTrue(target) => {
                    state.pop();
                    state.pop();
                    merge(&mut states, target as usize, state);
                }
            }
            merge(&mut states, pc + 1, state);
        }
        states[self.instruction_count].is_some_and(|state| state.bits & 1 != 0)
    }

    /// Maximum instructions retired on any valid forward path. Branches are
    /// costed by their more expensive successor, not by executing both arms.
    /// This is a work bound, not a latency calibration or selectivity estimate.
    #[must_use]
    pub fn max_path_instructions(&self) -> usize {
        use ScalarInstruction as I;
        let mut remaining = [0_usize; MAX_SCALAR_INSTRUCTIONS + 1];
        for pc in (0..self.instruction_count).rev() {
            let successor = match self.instructions[pc] {
                I::Jump(target) => remaining[target as usize],
                I::JumpIfNotTrue(target)
                | I::JumpIfTrue(target)
                | I::JumpIfFalse(target)
                | I::EqJumpIfNotTrue(target)
                | I::JumpIfNotNull(target) => remaining[pc + 1].max(remaining[target as usize]),
                _ => remaining[pc + 1],
            };
            remaining[pc] = 1 + successor;
        }
        remaining[0]
    }

    #[must_use]
    pub fn inputs(&self) -> &[ScalarType] {
        &self.inputs[..self.input_count]
    }

    #[must_use]
    pub const fn result_type(&self) -> ScalarType {
        self.result
    }
}

/// Bounded validation scratch. Only the live prefix participates in joins:
/// popped slots can contain different types on otherwise compatible paths.
#[derive(Clone, Copy)]
struct TypeStack {
    values: [ScalarType; MAX_SCALAR_STACK],
    len: usize,
}

impl TypeStack {
    const fn empty() -> Self {
        Self {
            values: [ScalarType::Bool; MAX_SCALAR_STACK],
            len: 0,
        }
    }

    fn as_slice(&self) -> &[ScalarType] {
        &self.values[..self.len]
    }

    fn push(&mut self, value: ScalarType) -> Result<(), ScalarProgramError> {
        let slot = self
            .values
            .get_mut(self.len)
            .ok_or(ScalarProgramError::StackOverflow)?;
        *slot = value;
        self.len += 1;
        Ok(())
    }
}

fn pop(stack: &mut TypeStack) -> Result<ScalarType, ScalarProgramError> {
    stack.len = stack
        .len
        .checked_sub(1)
        .ok_or(ScalarProgramError::StackUnderflow)?;
    Ok(stack.values[stack.len])
}

fn expect_pop(stack: &mut TypeStack, expected: ScalarType) -> Result<(), ScalarProgramError> {
    if pop(stack)? == expected {
        Ok(())
    } else {
        Err(ScalarProgramError::TypeMismatch)
    }
}

fn merge(
    states: &mut [Option<TypeStack>],
    target: usize,
    stack: TypeStack,
) -> Result<(), ScalarProgramError> {
    match &states[target] {
        Some(previous) if previous.as_slice() != stack.as_slice() => {
            Err(ScalarProgramError::IncompatibleBranches)
        }
        Some(_) => Ok(()),
        None => {
            states[target] = Some(stack);
            Ok(())
        }
    }
}

impl ScalarProgram {
    /// Validate every instruction and control-flow edge, including both sides
    /// of branches with constant conditions. End-of-program is a legal target.
    pub fn new(
        inputs: Vec<ScalarType>,
        result: ScalarType,
        instructions: Vec<ScalarInstruction>,
    ) -> Result<Self, ScalarProgramError> {
        let max_stack = Self::validate_types(&inputs, result, &instructions)?;
        Ok(Self {
            inputs,
            result,
            instructions,
            max_stack,
        })
    }

    fn validate_types(
        inputs: &[ScalarType],
        result: ScalarType,
        instructions: &[ScalarInstruction],
    ) -> Result<usize, ScalarProgramError> {
        use ScalarInstruction as I;
        use ScalarType::{Bool, Int4};

        let count = instructions.len();
        if inputs.len() > MAX_SCALAR_INPUTS || count == 0 || count > MAX_SCALAR_INSTRUCTIONS {
            return Err(ScalarProgramError::InvalidShape);
        }
        let mut states = [None; MAX_SCALAR_INSTRUCTIONS + 1];
        states[0] = Some(TypeStack::empty());
        let mut max_stack = 0;
        for (pc, instruction) in instructions.iter().enumerate() {
            let mut stack = states[pc].ok_or(ScalarProgramError::UnreachableInstruction)?;
            let mut branch = None;
            let mut fallthrough = true;
            match *instruction {
                I::Input(index) => stack.push(
                    *inputs
                        .get(index as usize)
                        .ok_or(ScalarProgramError::InvalidInput)?,
                )?,
                I::Int4(_) => stack.push(Int4)?,
                I::Date(value) => {
                    if !valid_date(value) {
                        return Err(ScalarProgramError::NonCanonicalOperand);
                    }
                    stack.push(ScalarType::Date)?;
                }
                I::Bool(_) | I::PredicateMask => stack.push(Bool)?,
                I::Null(kind) => stack.push(kind)?,
                I::Dup => stack.push(
                    *stack
                        .as_slice()
                        .last()
                        .ok_or(ScalarProgramError::StackUnderflow)?,
                )?,
                I::Drop => {
                    pop(&mut stack)?;
                }
                I::Add | I::Sub | I::Mul | I::Div | I::Mod | I::Int4CoalesceRhs(_, _) => {
                    expect_pop(&mut stack, Int4)?;
                    expect_pop(&mut stack, Int4)?;
                    stack.push(Int4)?;
                }
                I::Neg | I::CoalesceInt4(_) | I::Int4Immediate(_, _) => {
                    expect_pop(&mut stack, Int4)?;
                    stack.push(Int4)?;
                }
                I::DateAddDays | I::DateSubDays | I::DaysAddDate | I::DateDifference => {
                    let (lhs, rhs, output) = match instruction {
                        I::DaysAddDate => (Int4, ScalarType::Date, ScalarType::Date),
                        I::DateDifference => (ScalarType::Date, ScalarType::Date, Int4),
                        _ => (ScalarType::Date, Int4, ScalarType::Date),
                    };
                    expect_pop(&mut stack, rhs)?;
                    expect_pop(&mut stack, lhs)?;
                    stack.push(output)?;
                }
                I::Eq | I::Ne | I::Lt | I::Le | I::Gt | I::Ge | I::IsDistinctFrom => {
                    let rhs = pop(&mut stack)?;
                    expect_pop(&mut stack, rhs)?;
                    stack.push(Bool)?;
                }
                I::NullIf | I::Greatest | I::Least => {
                    let rhs = pop(&mut stack)?;
                    expect_pop(&mut stack, rhs)?;
                    stack.push(rhs)?;
                }
                I::And | I::Or => {
                    expect_pop(&mut stack, Bool)?;
                    expect_pop(&mut stack, Bool)?;
                    stack.push(Bool)?;
                }
                I::Not => {
                    expect_pop(&mut stack, Bool)?;
                    stack.push(Bool)?;
                }
                I::IsNull | I::IsNotNull => {
                    pop(&mut stack)?;
                    stack.push(Bool)?;
                }
                I::Jump(target) => {
                    branch = Some((target, stack));
                    fallthrough = false;
                }
                I::JumpIfNotTrue(target) | I::JumpIfTrue(target) | I::JumpIfFalse(target) => {
                    expect_pop(&mut stack, Bool)?;
                    branch = Some((target, stack));
                }
                I::JumpIfNotNull(target) => {
                    branch = Some((target, stack));
                    pop(&mut stack)?;
                }
                I::EqJumpIfNotTrue(target) => {
                    let rhs = pop(&mut stack)?;
                    expect_pop(&mut stack, rhs)?;
                    branch = Some((target, stack));
                }
            }
            max_stack = max_stack.max(stack.len);
            if let Some((target, branch_stack)) = branch {
                let target = target as usize;
                if target <= pc || target > count {
                    return Err(ScalarProgramError::InvalidJump);
                }
                merge(&mut states, target, branch_stack)?;
            }
            if fallthrough {
                merge(&mut states, pc + 1, stack)?;
            }
        }
        if states[count].as_ref().map(TypeStack::as_slice) != Some(&[result]) {
            return Err(ScalarProgramError::InvalidResult);
        }
        Ok(max_stack)
    }

    /// Fuse adjacent operations without crossing a control-flow entry. This
    /// planning-only pass preserves evaluation order and errors, relocates all
    /// forward edges (including the exit), then revalidates the complete CFG.
    pub fn fuse(mut self) -> Result<Self, ScalarProgramError> {
        // Removing a COALESCE branch may expose a new within-block pair.
        // Every productive pass shrinks the bounded program; the fixed point
        // is idempotent and never crosses an entry from a surviving edge.
        loop {
            let previous_count = self.instructions.len();
            self = self.fuse_once()?;
            if self.instructions.len() == previous_count {
                return Ok(self);
            }
        }
    }

    fn fuse_once(self) -> Result<Self, ScalarProgramError> {
        use ScalarInstruction as I;
        let count = self.instructions.len();
        let mut entry = [false; MAX_SCALAR_INSTRUCTIONS + 1];
        for mut instruction in self.instructions.iter().copied() {
            if let Some(target) = instruction.target_mut() {
                entry[*target as usize] = true;
            }
        }
        let mut relocation = [0_u32; MAX_SCALAR_INSTRUCTIONS + 1];
        let mut code = Vec::with_capacity(count);
        let mut pc = 0;
        while pc < count {
            relocation[pc] = code.len() as u32;
            let fused = if pc + 1 < count && !entry[pc + 1] {
                match (self.instructions[pc], self.instructions[pc + 1]) {
                    (I::Eq, I::JumpIfNotTrue(target)) => Some(I::EqJumpIfNotTrue(target)),
                    (I::JumpIfNotNull(target), I::Int4(value)) if target as usize == pc + 2 => {
                        Some(I::CoalesceInt4(value))
                    }
                    (I::Int4(value), operation) => ScalarArithmetic::from_instruction(operation)
                        .map(|operation| I::Int4Immediate(operation, value)),
                    (I::CoalesceInt4(value), operation) => {
                        ScalarArithmetic::from_instruction(operation)
                            .map(|operation| I::Int4CoalesceRhs(operation, value))
                    }
                    _ => None,
                }
            } else {
                None
            };
            code.push(fused.unwrap_or(self.instructions[pc]));
            pc += if fused.is_some() { 2 } else { 1 };
        }
        relocation[count] = code.len() as u32;
        for instruction in &mut code {
            if let Some(target) = instruction.target_mut() {
                *target = relocation[*target as usize];
            }
        }
        Self::new(self.inputs, self.result, code)
    }

    #[must_use]
    pub fn inputs(&self) -> &[ScalarType] {
        &self.inputs
    }

    #[must_use]
    pub const fn result_type(&self) -> ScalarType {
        self.result
    }

    #[must_use]
    pub fn instructions(&self) -> &[ScalarInstruction] {
        &self.instructions
    }

    #[must_use]
    pub const fn max_stack(&self) -> usize {
        self.max_stack
    }

    /// SPG1: magic, result type, input count, input types, then opcode/operand
    /// pairs. Reserved operands are zero. There are no pointers or catalog OIDs.
    #[must_use]
    pub fn encode(&self) -> Vec<i32> {
        let mut words = vec![0; self.encoded_word_len()];
        self.write_wire(&mut words);
        words
    }

    #[must_use]
    pub fn encoded_word_len(&self) -> usize {
        3 + self.inputs.len() + self.instructions.len() * 2
    }

    /// Write into caller-owned bounded metadata without an allocation. Invalid
    /// destination lengths are rejected before modifying any output word.
    pub fn encode_into(&self, words: &mut [i32]) -> Result<(), ScalarProgramError> {
        if words.len() != self.encoded_word_len() {
            return Err(ScalarProgramError::InvalidShape);
        }
        self.write_wire(words);
        Ok(())
    }

    fn write_wire(&self, words: &mut [i32]) {
        use ScalarInstruction as I;
        words[..3].copy_from_slice(&[
            SCALAR_PROGRAM_MAGIC,
            self.result.wire(),
            self.inputs.len() as i32,
        ]);
        for (slot, kind) in words[3..3 + self.inputs.len()].iter_mut().zip(&self.inputs) {
            *slot = kind.wire();
        }
        for (index, instruction) in self.instructions.iter().enumerate() {
            let pair = match *instruction {
                I::Input(index) => (0, index as i32),
                I::PredicateMask => (8, 0),
                I::Int4(value) => (1, value),
                I::Date(value) => (7, value),
                I::Bool(value) => (2, i32::from(value)),
                I::Null(kind) => (3, kind.wire()),
                I::Dup => (4, 0),
                I::Drop => (5, 0),
                I::CoalesceInt4(value) => (6, value),
                I::Int4Immediate(operation, value) => (60 + operation.index(), value),
                I::Int4CoalesceRhs(operation, value) => (65 + operation.index(), value),
                I::Add => (10, 0),
                I::Sub => (11, 0),
                I::Mul => (12, 0),
                I::Div => (13, 0),
                I::Mod => (14, 0),
                I::Neg => (15, 0),
                I::DateAddDays => (70, 0),
                I::DateSubDays => (71, 0),
                I::DaysAddDate => (72, 0),
                I::DateDifference => (73, 0),
                I::Eq => (20, 0),
                I::Ne => (21, 0),
                I::Lt => (22, 0),
                I::Le => (23, 0),
                I::Gt => (24, 0),
                I::Ge => (25, 0),
                I::IsDistinctFrom => (26, 0),
                I::NullIf => (27, 0),
                I::Greatest => (28, 0),
                I::Least => (29, 0),
                I::And => (30, 0),
                I::Or => (31, 0),
                I::Not => (32, 0),
                I::IsNull => (40, 0),
                I::IsNotNull => (41, 0),
                I::Jump(target) => (50, target as i32),
                I::JumpIfNotTrue(target) => (51, target as i32),
                I::JumpIfTrue(target) => (52, target as i32),
                I::JumpIfFalse(target) => (53, target as i32),
                I::JumpIfNotNull(target) => (54, target as i32),
                I::EqJumpIfNotTrue(target) => (55, target as i32),
            };
            let offset = 3 + self.inputs.len() + 2 * index;
            words[offset..offset + 2].copy_from_slice(&<[i32; 2]>::from(pair));
        }
    }

    pub fn decode(words: &[i32]) -> Result<Self, ScalarProgramError> {
        let wire = Self::validate_wire(words)?;
        Ok(Self {
            inputs: wire.inputs().to_vec(),
            result: wire.result,
            instructions: wire.instructions[..wire.instruction_count].to_vec(),
            max_stack: wire.max_stack,
        })
    }

    /// Validate without heap allocation, including malformed input paths.
    pub fn validate_wire(words: &[i32]) -> Result<ValidatedScalarWire, ScalarProgramError> {
        use ScalarInstruction as I;
        if words.len() < 3 || words[0] != SCALAR_PROGRAM_MAGIC {
            return Err(ScalarProgramError::InvalidHeader);
        }
        let result = ScalarType::from_wire(words[1])?;
        let count = usize::try_from(words[2]).map_err(|_| ScalarProgramError::InvalidShape)?;
        if count > MAX_SCALAR_INPUTS || words.len() < 3 + count {
            return Err(ScalarProgramError::InvalidShape);
        }
        let body = &words[3 + count..];
        if body.is_empty()
            || !body.len().is_multiple_of(2)
            || body.len() / 2 > MAX_SCALAR_INSTRUCTIONS
        {
            return Err(ScalarProgramError::InvalidShape);
        }
        let mut inputs = [ScalarType::Bool; MAX_SCALAR_INPUTS];
        for (slot, word) in inputs.iter_mut().zip(&words[3..3 + count]) {
            *slot = ScalarType::from_wire(*word)?;
        }
        let mut instructions = [I::Int4(0); MAX_SCALAR_INSTRUCTIONS];
        let instruction_count = body.len() / 2;
        for (slot, pair) in instructions.iter_mut().zip(body.chunks_exact(2)) {
            let [opcode, operand] = [pair[0], pair[1]];
            let unsigned =
                || u32::try_from(operand).map_err(|_| ScalarProgramError::NonCanonicalOperand);
            let instruction = match opcode {
                0 => I::Input(unsigned()?),
                1 => I::Int4(operand),
                2 => I::Bool(match operand {
                    0 => false,
                    1 => true,
                    _ => return Err(ScalarProgramError::NonCanonicalOperand),
                }),
                3 => I::Null(ScalarType::from_wire(operand)?),
                6 => I::CoalesceInt4(operand),
                7 => I::Date(operand),
                60..=69 => {
                    let operation = match (opcode - 60) % 5 {
                        0 => ScalarArithmetic::Add,
                        1 => ScalarArithmetic::Sub,
                        2 => ScalarArithmetic::Mul,
                        3 => ScalarArithmetic::Div,
                        _ => ScalarArithmetic::Mod,
                    };
                    if opcode < 65 {
                        I::Int4Immediate(operation, operand)
                    } else {
                        I::Int4CoalesceRhs(operation, operand)
                    }
                }
                50 => I::Jump(unsigned()?),
                51 => I::JumpIfNotTrue(unsigned()?),
                52 => I::JumpIfTrue(unsigned()?),
                53 => I::JumpIfFalse(unsigned()?),
                54 => I::JumpIfNotNull(unsigned()?),
                55 => I::EqJumpIfNotTrue(unsigned()?),
                _ => {
                    let instruction = match opcode {
                        4 => I::Dup,
                        5 => I::Drop,
                        8 => I::PredicateMask,
                        10 => I::Add,
                        11 => I::Sub,
                        12 => I::Mul,
                        13 => I::Div,
                        14 => I::Mod,
                        15 => I::Neg,
                        70 => I::DateAddDays,
                        71 => I::DateSubDays,
                        72 => I::DaysAddDate,
                        73 => I::DateDifference,
                        20 => I::Eq,
                        21 => I::Ne,
                        22 => I::Lt,
                        23 => I::Le,
                        24 => I::Gt,
                        25 => I::Ge,
                        26 => I::IsDistinctFrom,
                        27 => I::NullIf,
                        28 => I::Greatest,
                        29 => I::Least,
                        30 => I::And,
                        31 => I::Or,
                        32 => I::Not,
                        40 => I::IsNull,
                        41 => I::IsNotNull,
                        _ => return Err(ScalarProgramError::UnknownOpcode),
                    };
                    if operand != 0 {
                        return Err(ScalarProgramError::NonCanonicalOperand);
                    }
                    instruction
                }
            };
            *slot = instruction;
        }
        let max_stack =
            Self::validate_types(&inputs[..count], result, &instructions[..instruction_count])?;
        Ok(ValidatedScalarWire {
            inputs,
            input_count: count,
            result,
            instructions,
            instruction_count,
            max_stack,
        })
    }
}

#[cfg(any(test, feature = "pg_test"))]
pub(crate) mod tests {
    use super::{
        MAX_SCALAR_INPUTS, MAX_SCALAR_INSTRUCTIONS, MAX_SCALAR_STACK, SCALAR_PROGRAM_MAGIC,
        ScalarInstruction, ScalarProgram, ScalarProgramError, ScalarType,
    };
    use ScalarInstruction as I;
    use ScalarProgramError as E;
    use ScalarType::{Bool, Int4};

    #[cfg(test)]
    fn program(inputs: &[ScalarType], result: ScalarType, code: &[I]) -> ScalarProgram {
        let program =
            ScalarProgram::new(inputs.to_vec(), result, code.to_vec()).expect("valid program");
        assert_eq!(
            ScalarProgram::decode(&program.encode()),
            Ok(program.clone())
        );
        assert_eq!(program.inputs(), inputs);
        assert_eq!(program.result_type(), result);
        assert_eq!(program.instructions(), code);
        program
    }

    #[test]
    fn date_types_bounds_and_non_null_proof_are_not_integer_reinterpretation() {
        use ScalarType::Date;
        for value in [
            i32::MIN + 1,
            super::DATE_MIN - 1,
            super::DATE_END,
            i32::MAX - 1,
        ] {
            assert_eq!(
                ScalarProgram::new(vec![], Date, vec![I::Date(value)]),
                Err(E::NonCanonicalOperand)
            );
            assert_eq!(
                ScalarProgram::decode(&[SCALAR_PROGRAM_MAGIC, 6, 0, 7, value]),
                Err(E::NonCanonicalOperand)
            );
        }
        for (op, lhs, rhs, result) in [
            (I::DateAddDays, Date, Int4, Date),
            (I::DateSubDays, Date, Int4, Date),
            (I::DaysAddDate, Int4, Date, Date),
            (I::DateDifference, Date, Date, Int4),
        ] {
            let nullable = program(&[lhs, rhs], result, &[I::Input(0), I::Input(1), op]);
            assert!(
                !ScalarProgram::validate_wire(&nullable.encode())
                    .expect("nullable date proof")
                    .result_is_non_null()
            );
            for a in [Bool, Int4, Date] {
                for b in [Bool, Int4, Date] {
                    let tested =
                        ScalarProgram::new(vec![a, b], result, vec![I::Input(0), I::Input(1), op]);
                    assert_eq!(tested.is_ok(), a == lhs && b == rhs);
                }
            }
            let literal = |kind| if kind == Date { I::Date(0) } else { I::Int4(1) };
            let required = program(&[], result, &[literal(lhs), literal(rhs), op]);
            assert!(
                ScalarProgram::validate_wire(&required.encode())
                    .expect("literal date proof")
                    .result_is_non_null()
            );
            let mut words = required.encode();
            *words.last_mut().expect("date opcode operand") = 1;
            assert_eq!(ScalarProgram::decode(&words), Err(E::NonCanonicalOperand));
        }
        for op in [I::Add, I::Sub, I::Mul, I::Div, I::Mod, I::Eq] {
            assert!(
                ScalarProgram::new(vec![Date, Int4], Int4, vec![I::Input(0), I::Input(1), op])
                    .is_err()
            );
        }
    }

    #[test]
    fn arithmetic_fusion_preserves_boundaries_nulls_proofs_and_fixed_point() {
        use super::ScalarArithmetic as A;
        let values = [i32::MIN, -65536, -46341, -1, 0, 1, 46341, 65536, i32::MAX];
        for (operation, arithmetic) in [
            (I::Add, A::Add),
            (I::Sub, A::Sub),
            (I::Mul, A::Mul),
            (I::Div, A::Div),
            (I::Mod, A::Mod),
        ] {
            for literal in values {
                for coalesced in [false, true] {
                    let original = if coalesced {
                        program(
                            &[Int4, Int4],
                            Int4,
                            &[
                                I::Input(0),
                                I::Input(1),
                                I::JumpIfNotNull(4),
                                I::Int4(literal),
                                operation,
                            ],
                        )
                    } else {
                        program(&[Int4], Int4, &[I::Input(0), I::Int4(literal), operation])
                    };
                    let fused = original.clone().fuse().expect("arithmetic fusion");
                    assert_eq!(
                        fused.instructions().last(),
                        Some(&if coalesced {
                            I::Int4CoalesceRhs(arithmetic, literal)
                        } else {
                            I::Int4Immediate(arithmetic, literal)
                        })
                    );
                    assert_eq!(ScalarProgram::decode(&fused.encode()), Ok(fused.clone()));
                    assert_eq!(fused.clone().fuse(), Ok(fused.clone()));
                    let proof = ScalarProgram::validate_wire(&fused.encode()).expect("proof");
                    assert!(!proof.result_is_non_null());
                    assert_eq!(proof.max_path_instructions(), if coalesced { 3 } else { 2 });
                    for lhs in std::iter::once(Value::Null(Int4)).chain(values.map(Value::Int4)) {
                        for rhs in std::iter::once(Value::Null(Int4)).chain(values.map(Value::Int4))
                        {
                            let inputs = if coalesced { vec![lhs, rhs] } else { vec![lhs] };
                            assert_eq!(evaluate(&original, &inputs), evaluate(&fused, &inputs));
                        }
                    }
                    let mut code = vec![I::Int4(1)];
                    if coalesced {
                        code.push(I::Input(0));
                    }
                    code.push(*fused.instructions().last().expect("fused arithmetic"));
                    let non_null = program(&[Int4], Int4, &code);
                    assert!(
                        ScalarProgram::validate_wire(&non_null.encode())
                            .expect("proof")
                            .result_is_non_null()
                    );
                }
                for fused in [
                    I::Int4Immediate(arithmetic, literal),
                    I::Int4CoalesceRhs(arithmetic, literal),
                ] {
                    assert_eq!(
                        ScalarProgram::new(vec![], Int4, vec![fused]),
                        Err(E::StackUnderflow)
                    );
                    assert_eq!(
                        ScalarProgram::new(vec![], Int4, vec![I::Bool(false), fused]),
                        Err(E::TypeMismatch)
                    );
                }
                assert_eq!(
                    ScalarProgram::new(
                        vec![],
                        Int4,
                        vec![I::Int4(1), I::Int4CoalesceRhs(arithmetic, literal)]
                    ),
                    Err(E::StackUnderflow)
                );
                assert_eq!(
                    ScalarProgram::new(
                        vec![],
                        Int4,
                        vec![
                            I::Bool(false),
                            I::Int4(1),
                            I::Int4CoalesceRhs(arithmetic, literal)
                        ]
                    ),
                    Err(E::TypeMismatch)
                );
            }
        }
        // The arithmetic operation is a join point, not part of either arm.
        for rhs in [I::Int4(7), I::CoalesceInt4(7)] {
            let mut code = vec![
                I::Input(0),
                I::Input(1),
                I::JumpIfNotTrue(5),
                I::Int4(3),
                I::Jump(6),
            ];
            if matches!(rhs, I::CoalesceInt4(_)) {
                code[2] = I::JumpIfNotTrue(5);
                code[4] = I::Jump(7);
                code.push(I::Input(0));
            }
            code.extend([rhs, I::Sub]);
            let original = program(&[Int4, Bool], Int4, &code);
            assert_eq!(original.clone().fuse(), Ok(original));
        }
    }

    #[test]
    fn fusion_relocates_branches_preserving_values_errors_and_non_null_proof() {
        for value in [i32::MIN, -1, 0, 1, i32::MAX] {
            let original = program(
                &[Int4, Int4],
                Int4,
                &[
                    I::Input(0),
                    I::Input(1),
                    I::Eq,
                    I::JumpIfNotTrue(7),
                    I::Input(0),
                    I::Neg,
                    I::Jump(10),
                    I::Input(1),
                    I::JumpIfNotNull(10),
                    I::Int4(value),
                ],
            );
            let fused = original.clone().fuse().expect("fusion proof");
            assert_eq!(
                fused.instructions(),
                &[
                    I::Input(0),
                    I::Input(1),
                    I::EqJumpIfNotTrue(6),
                    I::Input(0),
                    I::Neg,
                    I::Jump(8),
                    I::Input(1),
                    I::CoalesceInt4(value),
                ]
            );
            assert_eq!(ScalarProgram::decode(&fused.encode()), Ok(fused.clone()));
            let proof = ScalarProgram::validate_wire(&fused.encode()).expect("wire proof");
            assert_eq!(proof.max_path_instructions(), 6);
            assert!(
                !proof.result_is_non_null(),
                "equality does not refine nullable inputs"
            );
            for a in [Value::Null(Int4), Value::Int4(i32::MIN), Value::Int4(1)] {
                for b in [Value::Null(Int4), Value::Int4(i32::MIN), Value::Int4(1)] {
                    assert_eq!(evaluate(&original, &[a, b]), evaluate(&fused, &[a, b]));
                }
            }
            let coalesce = program(
                &[Int4],
                Int4,
                &[I::Input(0), I::JumpIfNotNull(3), I::Int4(value)],
            )
            .fuse()
            .expect("literal coalesce fusion");
            let proof = ScalarProgram::validate_wire(&coalesce.encode()).expect("coalesce proof");
            assert!(proof.result_is_non_null());
            assert_eq!(proof.max_path_instructions(), 2);
        }
        for a in [Value::Null(Bool), Value::Bool(false), Value::Bool(true)] {
            for b in [Value::Null(Bool), Value::Bool(false), Value::Bool(true)] {
                let original = program(
                    &[Bool, Bool],
                    Int4,
                    &[
                        I::Input(0),
                        I::Input(1),
                        I::Eq,
                        I::JumpIfNotTrue(6),
                        I::Int4(7),
                        I::Jump(7),
                        I::Int4(42),
                    ],
                );
                let fused = original.clone().fuse().expect("Boolean equality fusion");
                assert_eq!(evaluate(&original, &[a, b]), evaluate(&fused, &[a, b]));
            }
        }
    }

    #[test]
    fn fusion_never_removes_a_branch_entry() {
        let cases = [
            program(
                &[Bool],
                Int4,
                &[
                    I::Input(0),
                    I::Dup,
                    I::JumpIfNotTrue(7),
                    I::Drop,
                    I::Int4(1),
                    I::Int4(1),
                    I::Eq,
                    I::JumpIfNotTrue(10),
                    I::Int4(7),
                    I::Jump(11),
                    I::Int4(42),
                ],
            ),
            program(
                &[Bool, Int4],
                Int4,
                &[
                    I::Input(0),
                    I::JumpIfNotTrue(4),
                    I::Input(1),
                    I::JumpIfNotNull(5),
                    I::Int4(42),
                ],
            ),
        ];
        for original in cases {
            assert_eq!(original.clone().fuse(), Ok(original));
        }
    }

    #[test]
    fn fused_operations_reject_invalid_types_stacks_and_targets() {
        for (code, error) in [
            (vec![I::CoalesceInt4(0)], E::StackUnderflow),
            (vec![I::Bool(false), I::CoalesceInt4(0)], E::TypeMismatch),
            (vec![I::EqJumpIfNotTrue(1)], E::StackUnderflow),
            (vec![I::Int4(0), I::EqJumpIfNotTrue(2)], E::StackUnderflow),
            (
                vec![I::Int4(0), I::Bool(false), I::EqJumpIfNotTrue(3)],
                E::TypeMismatch,
            ),
            (
                vec![I::Int4(0), I::Int4(0), I::EqJumpIfNotTrue(2)],
                E::InvalidJump,
            ),
            (
                vec![I::Int4(0), I::Int4(0), I::EqJumpIfNotTrue(4)],
                E::InvalidJump,
            ),
        ] {
            assert_eq!(ScalarProgram::new(vec![], Int4, code), Err(error));
        }
        assert_eq!(
            ScalarProgram::decode(&[SCALAR_PROGRAM_MAGIC, 2, 0, 55, -1]),
            Err(E::NonCanonicalOperand)
        );
    }

    #[test]
    fn scalar_wire_round_trips_each_literal_type_and_input_boundary() {
        for value in [i32::MIN, -1, 0, 1, i32::MAX] {
            let p = program(&[], Int4, &[I::Int4(value)]);
            assert_eq!(p.encode(), [SCALAR_PROGRAM_MAGIC, 2, 0, 1, value]);
            assert_eq!(p.max_stack(), 1);
        }
        for value in [false, true] {
            program(&[], Bool, &[I::Bool(value)]);
        }
        for kind in [Bool, Int4] {
            program(&[], kind, &[I::Null(kind)]);
        }
        program(&[Bool, Int4], Int4, &[I::Input(1)]);
        program(&[Bool; MAX_SCALAR_INPUTS], Bool, &[I::Input(63)]);
    }

    #[test]
    fn scalar_wire_validation_uses_no_heap_at_all_capacity_limits() {
        use crate::engine::residency::{begin_test_allocation_count, finish_test_allocation_count};
        let mut instructions = vec![I::Input(63); MAX_SCALAR_STACK];
        instructions.extend(std::iter::repeat_n(I::Add, MAX_SCALAR_STACK - 1));
        instructions.resize(MAX_SCALAR_INSTRUCTIONS, I::Neg);
        let program = ScalarProgram::new(vec![Int4; MAX_SCALAR_INPUTS], Int4, instructions)
            .expect("all bounded capacities");
        let words = program.encode();
        begin_test_allocation_count();
        let proof = ScalarProgram::validate_wire(&words);
        let allocations = finish_test_allocation_count();
        let proof = proof.expect("valid bounded wire");
        assert_eq!(allocations, 0);
        assert_eq!(proof.inputs(), program.inputs());
        assert_eq!(proof.result_type(), Int4);
        assert_eq!(proof.max_stack, MAX_SCALAR_STACK);
        assert_eq!(proof.instruction_count, MAX_SCALAR_INSTRUCTIONS);
        begin_test_allocation_count();
        let non_null = proof.result_is_non_null();
        let allocations = finish_test_allocation_count();
        assert!(!non_null, "inputs stay nullable even at maximum capacity");
        assert_eq!(allocations, 0);
    }

    #[test]
    fn path_work_bound_counts_lazy_successors_without_allocating() {
        use crate::engine::residency::{begin_test_allocation_count, finish_test_allocation_count};
        for jump in [I::JumpIfNotTrue(6), I::JumpIfTrue(6), I::JumpIfFalse(6)] {
            let words = program(
                &[Bool],
                Int4,
                &[
                    I::Input(0),
                    jump,
                    I::Int4(1),
                    I::Int4(2),
                    I::Add,
                    I::Jump(7),
                    I::Int4(3),
                ],
            )
            .encode();
            let proof = ScalarProgram::validate_wire(&words).expect("branch proof");
            begin_test_allocation_count();
            let work = proof.max_path_instructions();
            let allocations = finish_test_allocation_count();
            assert_eq!(work, 6, "exclusive branches must not be summed");
            assert_eq!(allocations, 0);
        }
        let words = program(
            &[Int4],
            Int4,
            &[I::Input(0), I::JumpIfNotNull(4), I::Int4(2), I::Neg],
        )
        .encode();
        assert_eq!(
            ScalarProgram::validate_wire(&words)
                .expect("coalesce")
                .max_path_instructions(),
            4
        );
        let mut code = vec![I::Int4(1)];
        code.resize(MAX_SCALAR_INSTRUCTIONS, I::Neg);
        let words = program(&[], Int4, &code).encode();
        assert_eq!(
            ScalarProgram::validate_wire(&words)
                .expect("maximum")
                .max_path_instructions(),
            MAX_SCALAR_INSTRUCTIONS
        );
    }

    #[test]
    fn non_null_proof_preserves_nullable_paths_and_error_evaluation() {
        use crate::engine::residency::{begin_test_allocation_count, finish_test_allocation_count};
        let cases = [
            (vec![I::Input(0)], false),
            (vec![I::Input(0), I::Neg], false),
            (vec![I::Input(0), I::JumpIfNotNull(3), I::Int4(0)], true),
            (vec![I::Input(0), I::JumpIfNotNull(3), I::Input(1)], false),
            (
                vec![
                    I::Input(0),
                    I::JumpIfNotNull(5),
                    I::Input(1),
                    I::JumpIfNotNull(5),
                    I::Int4(0),
                ],
                true,
            ),
            (
                vec![
                    I::Input(0),
                    I::JumpIfNotNull(5),
                    I::Int4(1),
                    I::Int4(0),
                    I::Div,
                ],
                true,
            ),
            (
                vec![
                    I::Input(0),
                    I::JumpIfNotNull(3),
                    I::Int4(i32::MAX),
                    I::Int4(1),
                    I::Add,
                ],
                true,
            ),
            (
                vec![
                    I::Input(0),
                    I::IsNull,
                    I::JumpIfNotTrue(5),
                    I::Int4(7),
                    I::Jump(6),
                    I::Input(1),
                ],
                false,
            ),
            (
                vec![
                    I::Input(0),
                    I::IsNull,
                    I::JumpIfNotTrue(5),
                    I::Int4(7),
                    I::Jump(6),
                    I::Int4(9),
                ],
                true,
            ),
        ];
        for (code, expected) in cases {
            let program = program(&[Int4, Int4], Int4, &code);
            let proof = ScalarProgram::validate_wire(&program.encode()).expect("proof");
            begin_test_allocation_count();
            let non_null = proof.result_is_non_null();
            let allocations = finish_test_allocation_count();
            assert_eq!(non_null, expected, "{code:?}");
            assert_eq!(allocations, 0);
            let values = [
                Value::Null(Int4),
                Value::Int4(0),
                Value::Int4(-1),
                Value::Int4(i32::MIN),
                Value::Int4(i32::MAX),
            ];
            for lhs in &values {
                for rhs in &values {
                    if non_null {
                        assert!(
                            !matches!(evaluate(&program, &[*lhs, *rhs]), Ok(Value::Null(_))),
                            "{code:?}"
                        );
                    }
                }
            }
        }
        for op in [
            I::Add,
            I::Sub,
            I::Mul,
            I::Div,
            I::Mod,
            I::Eq,
            I::Ne,
            I::Lt,
            I::Le,
            I::Gt,
            I::Ge,
        ] {
            let result = if matches!(op, I::Add | I::Sub | I::Mul | I::Div | I::Mod) {
                Int4
            } else {
                Bool
            };
            for nullable in [false, true] {
                let code = [
                    if nullable { I::Input(0) } else { I::Int4(1) },
                    I::Int4(2),
                    op,
                ];
                let p = program(&[Int4], result, &code);
                assert_eq!(
                    ScalarProgram::validate_wire(&p.encode())
                        .expect("binary proof")
                        .result_is_non_null(),
                    !nullable
                );
            }
        }
        for op in [I::IsNull, I::IsNotNull] {
            let p = program(&[Int4], Bool, &[I::Input(0), op, I::Not, I::Dup, I::And]);
            assert!(
                ScalarProgram::validate_wire(&p.encode())
                    .expect("Boolean proof")
                    .result_is_non_null()
            );
        }
        let p = program(&[Bool], Bool, &[I::Input(0), I::Bool(true), I::Or]);
        assert!(
            !ScalarProgram::validate_wire(&p.encode())
                .expect("conservative Boolean proof")
                .result_is_non_null()
        );
    }

    #[test]
    fn distinct_is_non_null_but_does_not_elide_operand_errors() {
        for kind in [Int4, Bool, ScalarType::Date] {
            let p = program(
                &[kind, kind],
                Bool,
                &[I::Input(0), I::Input(1), I::IsDistinctFrom],
            );
            let words = p.encode();
            assert!(
                ScalarProgram::validate_wire(&words)
                    .expect("proof")
                    .result_is_non_null()
            );
            assert_eq!(
                evaluate(&p, &[Value::Null(kind), Value::Null(kind)]),
                Ok(Value::Bool(false))
            );
            let mut invalid = words;
            *invalid.last_mut().expect("operand") = 1;
            assert_eq!(
                ScalarProgram::validate_wire(&invalid).err(),
                Some(ScalarProgramError::NonCanonicalOperand)
            );
        }
        assert_eq!(
            ScalarProgram::new(
                vec![Int4, Bool],
                Bool,
                vec![I::Input(0), I::Input(1), I::IsDistinctFrom]
            )
            .err(),
            Some(ScalarProgramError::TypeMismatch)
        );
        let p = program(
            &[Int4],
            Bool,
            &[
                I::Null(Int4),
                I::Input(0),
                I::Int4(0),
                I::Div,
                I::IsDistinctFrom,
            ],
        );
        assert!(
            ScalarProgram::validate_wire(&p.encode())
                .expect("nonnull result proof")
                .result_is_non_null()
        );
        assert_eq!(evaluate(&p, &[Value::Int4(1)]), Err(Fault::DivisionByZero));
    }

    #[test]
    fn nullif_and_extrema_prove_types_nullability_and_retain_errors() {
        for op in [I::NullIf, I::Greatest, I::Least] {
            for kind in [Int4, Bool, ScalarType::Date] {
                let p = program(&[kind, kind], kind, &[I::Input(0), I::Input(1), op]);
                let mut words = p.encode();
                assert_eq!(ScalarProgram::decode(&words), Ok(p));
                assert!(
                    !ScalarProgram::validate_wire(&words)
                        .expect("proof")
                        .result_is_non_null()
                );
                *words.last_mut().expect("operand") = 1;
                assert_eq!(
                    ScalarProgram::validate_wire(&words).err(),
                    Some(ScalarProgramError::NonCanonicalOperand)
                );
            }
            assert_eq!(
                ScalarProgram::new(vec![Int4, Bool], Int4, vec![I::Input(0), I::Input(1), op])
                    .err(),
                Some(ScalarProgramError::TypeMismatch)
            );
            assert_eq!(
                ScalarProgram::new(vec![Int4], Int4, vec![I::Input(0), op]).err(),
                Some(ScalarProgramError::StackUnderflow)
            );
            let p = program(
                &[Int4],
                Int4,
                &[I::Int4(42), I::Input(0), I::Int4(0), I::Div, op],
            );
            assert_eq!(
                ScalarProgram::validate_wire(&p.encode())
                    .expect("proof")
                    .result_is_non_null(),
                op != I::NullIf
            );
            assert_eq!(evaluate(&p, &[Value::Int4(1)]), Err(Fault::DivisionByZero));
            let fused = p.clone().fuse().expect("fused proof");
            assert_eq!(
                evaluate(&fused, &[Value::Int4(1)]),
                Err(Fault::DivisionByZero)
            );
        }
    }

    #[test]
    fn scalar_branch_merge_ignores_popped_stack_slots() {
        program(
            &[Bool],
            Int4,
            &[
                I::Input(0),
                I::JumpIfNotTrue(6),
                I::Int4(7),
                I::Int4(8),
                I::Add,
                I::Jump(10),
                I::Int4(9),
                I::Bool(true),
                I::Not,
                I::JumpIfTrue(10),
            ],
        );
    }

    #[test]
    fn scalar_wire_round_trips_all_operators_and_branch_kinds() {
        for instruction in [I::Add, I::Sub, I::Mul, I::Div, I::Mod] {
            program(
                &[Int4, Int4],
                Int4,
                &[I::Input(0), I::Input(1), instruction],
            );
        }
        for instruction in [I::Eq, I::Ne, I::Lt, I::Le, I::Gt, I::Ge] {
            for kind in [Int4, Bool] {
                program(
                    &[kind, kind],
                    Bool,
                    &[I::Input(0), I::Input(1), instruction],
                );
            }
        }
        for instruction in [I::And, I::Or] {
            program(
                &[Bool, Bool],
                Bool,
                &[I::Input(0), I::Input(1), instruction],
            );
        }
        program(&[Int4], Int4, &[I::Input(0), I::Neg]);
        program(&[Bool], Bool, &[I::Input(0), I::Not]);
        for kind in [Int4, Bool] {
            for instruction in [I::IsNull, I::IsNotNull] {
                program(&[kind], Bool, &[I::Input(0), instruction]);
            }
        }
        for branch in [I::JumpIfNotTrue(4), I::JumpIfTrue(4), I::JumpIfFalse(4)] {
            program(
                &[Bool],
                Int4,
                &[I::Input(0), branch, I::Int4(1), I::Jump(5), I::Int4(2)],
            );
        }
        program(
            &[Int4],
            Int4,
            &[I::Input(0), I::JumpIfNotNull(3), I::Int4(4)],
        );
        program(&[Int4], Int4, &[I::Input(0), I::Dup, I::Add]);
    }

    #[test]
    fn scalar_validation_rejects_shape_type_and_control_flow_corruption() {
        let cases = [
            (vec![], E::InvalidShape),
            (vec![I::Input(1)], E::InvalidInput),
            (vec![I::Add], E::StackUnderflow),
            (vec![I::Dup], E::StackUnderflow),
            (vec![I::Drop], E::StackUnderflow),
            (vec![I::Int4(1), I::Drop], E::InvalidResult),
            (vec![I::Int4(1), I::Bool(true), I::Add], E::TypeMismatch),
            (vec![I::Int4(1), I::Bool(true), I::Eq], E::TypeMismatch),
            (vec![I::Int4(1), I::JumpIfNotTrue(2)], E::TypeMismatch),
            (vec![I::JumpIfNotNull(1)], E::StackUnderflow),
            (vec![I::Int4(1), I::Jump(0)], E::InvalidJump),
            (vec![I::Jump(u32::MAX)], E::InvalidJump),
            (vec![I::Int4(1), I::Jump(1)], E::InvalidJump),
            (
                vec![I::Int4(1), I::Jump(3), I::Int4(2)],
                E::UnreachableInstruction,
            ),
            (vec![I::Int4(1), I::Int4(2)], E::InvalidResult),
            (vec![I::Bool(true)], E::InvalidResult),
            (
                vec![
                    I::Bool(true),
                    I::JumpIfNotTrue(4),
                    I::Int4(1),
                    I::Jump(5),
                    I::Bool(false),
                ],
                E::IncompatibleBranches,
            ),
            (
                vec![I::Bool(true), I::JumpIfNotTrue(3), I::Int4(1)],
                E::IncompatibleBranches,
            ),
        ];
        for (code, expected) in cases {
            assert_eq!(
                ScalarProgram::new(vec![Int4], Int4, code.clone()),
                Err(expected),
                "{code:?}"
            );
        }
    }

    #[test]
    fn scalar_limits_accept_exact_boundaries_and_reject_one_more() {
        let mut code = vec![I::Int4(1); MAX_SCALAR_STACK];
        code.extend(vec![I::Add; MAX_SCALAR_STACK - 1]);
        assert_eq!(program(&[], Int4, &code).max_stack(), MAX_SCALAR_STACK);
        code.insert(0, I::Int4(1));
        assert_eq!(
            ScalarProgram::new(vec![], Int4, code),
            Err(E::StackOverflow)
        );
        let mut code = vec![I::Int4(1)];
        code.extend(vec![I::Neg; MAX_SCALAR_INSTRUCTIONS - 1]);
        program(&[], Int4, &code);
        code.push(I::Neg);
        assert_eq!(ScalarProgram::new(vec![], Int4, code), Err(E::InvalidShape));
        assert_eq!(
            ScalarProgram::new(vec![Int4; MAX_SCALAR_INPUTS + 1], Int4, vec![I::Input(0)]),
            Err(E::InvalidShape)
        );
    }

    #[test]
    fn scalar_decode_rejects_noncanonical_and_unknown_wire_values() {
        for words in [vec![], vec![0, 2, 0, 1, 7], vec![SCALAR_PROGRAM_MAGIC]] {
            assert_eq!(ScalarProgram::decode(&words), Err(E::InvalidHeader));
        }
        for (body, error) in [
            (vec![2, 7], E::NonCanonicalOperand),
            (vec![0, -1], E::NonCanonicalOperand),
            (vec![50, -1], E::NonCanonicalOperand),
            (vec![3, 99], E::InvalidType),
            (vec![4, 1], E::NonCanonicalOperand),
            (vec![5, 1], E::NonCanonicalOperand),
            (vec![999, 0], E::UnknownOpcode),
            (vec![1], E::InvalidShape),
            (vec![], E::InvalidShape),
        ] {
            let mut words = vec![SCALAR_PROGRAM_MAGIC, 2, 0];
            words.extend(body);
            assert_eq!(ScalarProgram::decode(&words), Err(error));
        }
        for count in [-1, 65, i32::MAX] {
            assert_eq!(
                ScalarProgram::decode(&[SCALAR_PROGRAM_MAGIC, 2, count]),
                Err(E::InvalidShape)
            );
        }
        assert_eq!(
            ScalarProgram::decode(&[SCALAR_PROGRAM_MAGIC, 2, 1, 99, 0, 0]),
            Err(E::InvalidType)
        );
        assert_eq!(
            ScalarProgram::decode(&[SCALAR_PROGRAM_MAGIC, 0, 0, 1, 0]),
            Err(E::InvalidType)
        );
    }

    // Test-only independent row oracle. Production must never use a CPU
    // interpreter as a hidden fallback for a selected GPU expression.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Value {
        Null(ScalarType),
        Bool(bool),
        Int4(i32),
        Date(i32),
    }

    impl Value {
        const fn kind(self) -> ScalarType {
            match self {
                Self::Null(kind) => kind,
                Self::Bool(_) => Bool,
                Self::Int4(_) => Int4,
                Self::Date(_) => ScalarType::Date,
            }
        }
        const fn is_null(self) -> bool {
            matches!(self, Self::Null(_))
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum Fault {
        Overflow,
        DivisionByZero,
        DateOutOfRange,
    }

    #[test]
    fn scalar_shared_device_corpus_is_canonical_and_matches_independent_oracle() {
        let corpus = include_str!("../../../../pgaccel-kernels/test/scalar_cases.txt");
        let mut count = 0;
        for line in corpus.lines().filter(|line| !line.starts_with('#')) {
            let parts = line.split('|').map(str::trim).collect::<Vec<_>>();
            assert_eq!(parts.len(), 4, "{line}");
            let numbers = |part: &str| {
                part.split_whitespace()
                    .map(|word| word.parse::<i32>().expect("fixture integer"))
                    .collect::<Vec<_>>()
            };
            let words = numbers(parts[1]);
            let program = ScalarProgram::decode(&words).expect(parts[0]);
            assert_eq!(program.encode(), words, "{} canonical wire", parts[0]);
            let input_words = numbers(parts[2]);
            assert_eq!(input_words.len() % 3, 0);
            let value = |triple: &[i32]| {
                let kind = ScalarType::from_wire(triple[0]).expect("fixture type");
                match (kind, triple[1], triple[2]) {
                    (_, 0, 1) => Value::Null(kind),
                    (Int4, value, 0) => Value::Int4(value),
                    (ScalarType::Date, value, 0) => Value::Date(value),
                    (Bool, 0, 0) => Value::Bool(false),
                    (Bool, 1, 0) => Value::Bool(true),
                    _ => panic!("invalid fixture value {triple:?}"),
                }
            };
            let inputs = input_words.chunks_exact(3).map(value).collect::<Vec<_>>();
            let expected = numbers(parts[3]);
            assert_eq!(expected.len(), 4);
            let expected = match expected[0] {
                0 => Ok(value(&expected[1..])),
                1 => Err(Fault::Overflow),
                2 => Err(Fault::DivisionByZero),
                4 => Err(Fault::DateOutOfRange),
                _ => panic!("unknown fixture status"),
            };
            assert_eq!(evaluate(&program, &inputs), expected, "{}", parts[0]);
            let fused = program.clone().fuse().expect("fused corpus proof");
            assert_eq!(evaluate(&fused, &inputs), expected, "{} fused", parts[0]);
            assert_eq!(fused.clone().fuse(), Ok(fused), "fusion is idempotent");
            if ScalarProgram::validate_wire(&words)
                .expect("corpus proof")
                .result_is_non_null()
            {
                assert!(!matches!(expected, Ok(Value::Null(_))), "{}", parts[0]);
            }
            count += 1;
        }
        assert_eq!(count, 468, "do not silently drop device parity cases");
    }

    pub fn evaluate(program: &ScalarProgram, inputs: &[Value]) -> Result<Value, Fault> {
        evaluate_with_predicate_mask(program, inputs, None)
    }

    #[test]
    fn predicate_mask_preserves_qualifier_position_nulls_and_errors() {
        let mask_first = ScalarProgram::new(
            vec![Int4],
            Bool,
            vec![
                I::PredicateMask,
                I::JumpIfNotTrue(8),
                I::Input(0),
                I::Int4(0),
                I::Div,
                I::Int4(0),
                I::Gt,
                I::Jump(9),
                I::Bool(false),
            ],
        )
        .expect("mask-first program");
        let scalar_first = ScalarProgram::new(
            vec![Int4],
            Bool,
            vec![
                I::Input(0),
                I::Int4(0),
                I::Div,
                I::Int4(0),
                I::Gt,
                I::JumpIfNotTrue(8),
                I::PredicateMask,
                I::Jump(9),
                I::Bool(false),
            ],
        )
        .expect("scalar-first program");
        for mask in [Value::Bool(false), Value::Null(Bool), Value::Bool(true)] {
            assert_eq!(
                evaluate_with_predicate_mask(&mask_first, &[Value::Int4(1)], Some(mask)),
                if mask == Value::Bool(true) {
                    Err(Fault::DivisionByZero)
                } else {
                    Ok(Value::Bool(false))
                }
            );
            assert_eq!(
                evaluate_with_predicate_mask(&scalar_first, &[Value::Int4(1)], Some(mask)),
                Err(Fault::DivisionByZero)
            );
        }
        let program = ScalarProgram::new(vec![], Bool, vec![I::PredicateMask]).expect("mask");
        let proof = ScalarProgram::validate_wire(&program.encode()).expect("proof");
        assert!(proof.uses_predicate_mask());
        assert!(!proof.result_is_non_null());
        assert_eq!(
            ScalarProgram::decode(&program.encode()),
            Ok(program.clone())
        );
        assert_eq!(program.clone().fuse(), Ok(program));
        assert!(ScalarProgram::decode(&[SCALAR_PROGRAM_MAGIC, 1, 0, 8, 1]).is_err());
    }

    pub fn evaluate_with_predicate_mask(
        program: &ScalarProgram,
        inputs: &[Value],
        mask: Option<Value>,
    ) -> Result<Value, Fault> {
        use Value as V;
        assert_eq!(
            inputs.iter().map(|value| value.kind()).collect::<Vec<_>>(),
            program.inputs
        );
        let mut stack = Vec::new();
        let mut pc = 0;
        let mut steps = 0;
        while pc < program.instructions.len() {
            steps += 1;
            assert!(
                steps <= program.instructions.len(),
                "validated programs cannot loop"
            );
            let mut instruction = program.instructions[pc];
            pc += 1;
            // Interpret fused operations via the independent checked Rust
            // oracle below. Transient oracle stack size need not match the VM.
            if let I::Int4Immediate(operation, literal) | I::Int4CoalesceRhs(operation, literal) =
                instruction
            {
                if matches!(instruction, I::Int4Immediate(_, _)) {
                    stack.push(V::Int4(literal));
                } else if stack.last().expect("typed RHS").is_null() {
                    *stack.last_mut().expect("typed RHS") = V::Int4(literal);
                }
                instruction = match operation {
                    super::ScalarArithmetic::Add => I::Add,
                    super::ScalarArithmetic::Sub => I::Sub,
                    super::ScalarArithmetic::Mul => I::Mul,
                    super::ScalarArithmetic::Div => I::Div,
                    super::ScalarArithmetic::Mod => I::Mod,
                };
            }
            match instruction {
                I::Input(index) => stack.push(inputs[index as usize]),
                I::PredicateMask => {
                    let value = mask.expect("predicate mask execution context");
                    assert_eq!(value.kind(), ScalarType::Bool);
                    stack.push(value);
                }
                I::Int4(value) => stack.push(V::Int4(value)),
                I::Date(value) => stack.push(V::Date(value)),
                I::Bool(value) => stack.push(V::Bool(value)),
                I::Null(kind) => stack.push(V::Null(kind)),
                I::Dup => stack.push(*stack.last().expect("typed stack")),
                I::Drop => {
                    stack.pop().expect("typed stack");
                }
                I::CoalesceInt4(value) => {
                    let top = stack.last_mut().expect("typed INT4");
                    if top.is_null() {
                        *top = V::Int4(value);
                    }
                }
                I::EqJumpIfNotTrue(target) => {
                    let rhs = stack.pop().expect("typed RHS");
                    let lhs = stack.pop().expect("typed LHS");
                    if lhs.is_null() || rhs.is_null() || lhs != rhs {
                        pc = target as usize;
                    }
                }
                I::Jump(target) => pc = target as usize,
                I::JumpIfNotTrue(target) | I::JumpIfTrue(target) | I::JumpIfFalse(target) => {
                    let value = stack.pop().expect("typed Boolean");
                    let jump = match instruction {
                        I::JumpIfNotTrue(_) => value != V::Bool(true),
                        I::JumpIfTrue(_) => value == V::Bool(true),
                        _ => value == V::Bool(false),
                    };
                    if jump {
                        pc = target as usize;
                    }
                }
                I::JumpIfNotNull(target) => {
                    if stack.last().expect("typed value").is_null() {
                        stack.pop();
                    } else {
                        pc = target as usize;
                    }
                }
                I::IsNull | I::IsNotNull => {
                    let is_null = stack.pop().expect("typed value").is_null();
                    stack.push(V::Bool(is_null == (instruction == I::IsNull)));
                }
                I::Neg | I::Not => {
                    let value = stack.pop().expect("typed value");
                    stack.push(match value {
                        V::Null(kind) => V::Null(kind),
                        V::Int4(value) => V::Int4(value.checked_neg().ok_or(Fault::Overflow)?),
                        V::Bool(value) => V::Bool(!value),
                        V::Date(_) => unreachable!("DATE has no unary negation"),
                    });
                }
                I::DateAddDays | I::DateSubDays | I::DaysAddDate | I::DateDifference => {
                    let rhs = stack.pop().expect("typed RHS");
                    let lhs = stack.pop().expect("typed LHS");
                    let kind = if instruction == I::DateDifference {
                        Int4
                    } else {
                        ScalarType::Date
                    };
                    if lhs.is_null() || rhs.is_null() {
                        stack.push(V::Null(kind));
                        continue;
                    }
                    let infinite = |date| date == i32::MIN || date == i32::MAX;
                    let value = match (instruction, lhs, rhs) {
                        (I::DateDifference, V::Date(a), V::Date(b)) => {
                            if infinite(a) || infinite(b) {
                                return Err(Fault::DateOutOfRange);
                            }
                            V::Int4(a.checked_sub(b).expect("finite date span fits INT4"))
                        }
                        (op, V::Date(date), V::Int4(days))
                        | (op @ I::DaysAddDate, V::Int4(days), V::Date(date)) => {
                            if infinite(date) {
                                V::Date(date)
                            } else {
                                let result = if op == I::DateSubDays {
                                    date.checked_sub(days)
                                } else {
                                    date.checked_add(days)
                                }
                                .filter(|value| (super::DATE_MIN..super::DATE_END).contains(value))
                                .ok_or(Fault::DateOutOfRange)?;
                                V::Date(result)
                            }
                        }
                        _ => unreachable!("typed date operation"),
                    };
                    stack.push(value);
                }
                _ => {
                    let rhs = stack.pop().expect("typed rhs");
                    let lhs = stack.pop().expect("typed lhs");
                    let value = match instruction {
                        I::And => {
                            if lhs == V::Bool(false) || rhs == V::Bool(false) {
                                V::Bool(false)
                            } else if lhs.is_null() || rhs.is_null() {
                                V::Null(Bool)
                            } else {
                                V::Bool(true)
                            }
                        }
                        I::Or => {
                            if lhs == V::Bool(true) || rhs == V::Bool(true) {
                                V::Bool(true)
                            } else if lhs.is_null() || rhs.is_null() {
                                V::Null(Bool)
                            } else {
                                V::Bool(false)
                            }
                        }
                        I::IsDistinctFrom => V::Bool(lhs != rhs),
                        I::NullIf => {
                            if !lhs.is_null() && lhs == rhs {
                                V::Null(lhs.kind())
                            } else {
                                lhs
                            }
                        }
                        I::Greatest | I::Least => {
                            if lhs.is_null() {
                                rhs
                            } else if rhs.is_null() {
                                lhs
                            } else {
                                let ordering = match (lhs, rhs) {
                                    (V::Int4(a), V::Int4(b)) | (V::Date(a), V::Date(b)) => {
                                        a.cmp(&b)
                                    }
                                    (V::Bool(a), V::Bool(b)) => a.cmp(&b),
                                    _ => unreachable!("type proof"),
                                };
                                if (instruction == I::Greatest && ordering.is_lt())
                                    || (instruction == I::Least && ordering.is_gt())
                                {
                                    rhs
                                } else {
                                    lhs
                                }
                            }
                        }
                        I::Eq | I::Ne | I::Lt | I::Le | I::Gt | I::Ge => {
                            if lhs.is_null() || rhs.is_null() {
                                V::Null(Bool)
                            } else {
                                let ordering = match (lhs, rhs) {
                                    (V::Int4(a), V::Int4(b)) => a.cmp(&b),
                                    (V::Date(a), V::Date(b)) => a.cmp(&b),
                                    (V::Bool(a), V::Bool(b)) => a.cmp(&b),
                                    _ => unreachable!("type proof"),
                                };
                                V::Bool(match instruction {
                                    I::Eq => ordering.is_eq(),
                                    I::Ne => !ordering.is_eq(),
                                    I::Lt => ordering.is_lt(),
                                    I::Le => !ordering.is_gt(),
                                    I::Gt => ordering.is_gt(),
                                    _ => !ordering.is_lt(),
                                })
                            }
                        }
                        _ => {
                            if lhs.is_null() || rhs.is_null() {
                                V::Null(Int4)
                            } else {
                                let (V::Int4(a), V::Int4(b)) = (lhs, rhs) else {
                                    unreachable!("type proof")
                                };
                                if matches!(instruction, I::Div | I::Mod) && b == 0 {
                                    return Err(Fault::DivisionByZero);
                                }
                                let value = match instruction {
                                    I::Add => a.checked_add(b),
                                    I::Sub => a.checked_sub(b),
                                    I::Mul => a.checked_mul(b),
                                    I::Div => a.checked_div(b),
                                    // PostgreSQL int4mod deliberately returns zero
                                    // for MIN % -1; unlike MIN / -1 it cannot overflow.
                                    I::Mod if b == -1 => Some(0),
                                    I::Mod => a.checked_rem(b),
                                    _ => unreachable!("binary instruction"),
                                };
                                V::Int4(value.ok_or(Fault::Overflow)?)
                            }
                        }
                    };
                    stack.push(value);
                }
            }
            assert!(stack.len() <= program.max_stack);
        }
        assert_eq!(stack.len(), 1);
        let result = stack[0];
        assert_eq!(result.kind(), program.result);
        Ok(result)
    }

    #[test]
    fn scalar_case_and_coalesce_do_not_evaluate_inactive_errors() {
        use Value as V;
        let case = program(
            &[Bool, Int4],
            Int4,
            &[
                I::Input(0),
                I::JumpIfNotTrue(6),
                I::Input(1),
                I::Int4(1),
                I::Add,
                I::Jump(7),
                I::Int4(42),
            ],
        );
        for condition in [V::Bool(false), V::Null(Bool)] {
            assert_eq!(
                evaluate(&case, &[condition, V::Int4(i32::MAX)]),
                Ok(V::Int4(42))
            );
        }
        assert_eq!(
            evaluate(&case, &[V::Bool(true), V::Int4(i32::MAX)]),
            Err(Fault::Overflow)
        );
        assert_eq!(
            evaluate(&case, &[V::Bool(true), V::Null(Int4)]),
            Ok(V::Null(Int4))
        );
        let coalesce = program(
            &[Int4, Int4],
            Int4,
            &[
                I::Input(0),
                I::JumpIfNotNull(5),
                I::Int4(7),
                I::Input(1),
                I::Div,
            ],
        );
        assert_eq!(
            evaluate(&coalesce, &[V::Int4(3), V::Int4(0)]),
            Ok(V::Int4(3))
        );
        assert_eq!(
            evaluate(&coalesce, &[V::Null(Int4), V::Int4(0)]),
            Err(Fault::DivisionByZero)
        );
        assert_eq!(
            evaluate(&coalesce, &[V::Null(Int4), V::Null(Int4)]),
            Ok(V::Null(Int4))
        );
    }

    #[test]
    fn scalar_checked_arithmetic_distinguishes_error_domains_and_nulls() {
        use Value as V;
        for (instruction, a, b, expected) in [
            (I::Add, i32::MAX, 1, Err(Fault::Overflow)),
            (I::Sub, i32::MIN, 1, Err(Fault::Overflow)),
            (I::Mul, i32::MAX, 2, Err(Fault::Overflow)),
            (I::Div, i32::MIN, -1, Err(Fault::Overflow)),
            (I::Mod, i32::MIN, -1, Ok(V::Int4(0))),
            (I::Div, 1, 0, Err(Fault::DivisionByZero)),
            (I::Mod, 1, 0, Err(Fault::DivisionByZero)),
            (I::Div, -7, 3, Ok(V::Int4(-2))),
            (I::Mod, -7, 3, Ok(V::Int4(-1))),
        ] {
            let p = program(
                &[Int4, Int4],
                Int4,
                &[I::Input(0), I::Input(1), instruction],
            );
            assert_eq!(evaluate(&p, &[V::Int4(a), V::Int4(b)]), expected);
            assert_eq!(
                evaluate(&p, &[V::Null(Int4), V::Int4(b)]),
                Ok(V::Null(Int4))
            );
            assert_eq!(
                evaluate(&p, &[V::Int4(a), V::Null(Int4)]),
                Ok(V::Null(Int4))
            );
        }
        // A row's NULL lhs does not suppress evaluation of the RHS. Use a
        // runtime input: PostgreSQL can fold a literal NULL + expression away
        // before execution, so the planner must lower already-simplified SQL.
        let p = program(
            &[Int4, Int4],
            Int4,
            &[I::Input(0), I::Input(1), I::Int4(0), I::Div, I::Add],
        );
        assert_eq!(
            evaluate(&p, &[V::Null(Int4), V::Int4(1)]),
            Err(Fault::DivisionByZero)
        );
        let p = program(&[Int4], Int4, &[I::Input(0), I::Neg]);
        assert_eq!(evaluate(&p, &[V::Int4(i32::MIN)]), Err(Fault::Overflow));
    }

    #[test]
    fn scalar_three_valued_logic_and_short_circuit_have_distinct_null_edges() {
        use Value as V;
        let values = [V::Bool(false), V::Bool(true), V::Null(Bool)];
        for (instruction, expected) in [
            (I::And, [0, 0, 0, 0, 1, 2, 0, 2, 2]),
            (I::Or, [0, 1, 2, 1, 1, 1, 2, 1, 2]),
        ] {
            let p = program(
                &[Bool, Bool],
                Bool,
                &[I::Input(0), I::Input(1), instruction],
            );
            for (a, lhs) in values.iter().enumerate() {
                for (b, rhs) in values.iter().enumerate() {
                    assert_eq!(evaluate(&p, &[*lhs, *rhs]), Ok(values[expected[a * 3 + b]]));
                }
            }
        }
        for (instruction, branch, skip_value) in [
            (I::And, I::JumpIfFalse(9), false),
            (I::Or, I::JumpIfTrue(9), true),
        ] {
            let p = program(
                &[Bool, Int4],
                Bool,
                &[
                    I::Input(0),
                    I::Dup,
                    branch,
                    I::Input(1),
                    I::Int4(0),
                    I::Div,
                    I::Int4(0),
                    I::Gt,
                    instruction,
                ],
            );
            assert_eq!(
                evaluate(&p, &[V::Bool(skip_value), V::Int4(1)]),
                Ok(V::Bool(skip_value))
            );
            assert_eq!(
                evaluate(&p, &[V::Null(Bool), V::Int4(1)]),
                Err(Fault::DivisionByZero)
            );
        }
    }

    #[test]
    fn scalar_decoder_mutations_never_panic_or_accept_noncanonical_round_trips() {
        use crate::engine::residency::{begin_test_allocation_count, finish_test_allocation_count};
        let validate_without_allocations = |words: &[i32]| {
            begin_test_allocation_count();
            let result = ScalarProgram::validate_wire(words);
            let allocations = finish_test_allocation_count();
            assert_eq!(
                allocations, 0,
                "wire validation must not allocate on errors either"
            );
            let decoded = ScalarProgram::decode(words);
            assert_eq!(result.as_ref().err(), decoded.as_ref().err());
            decoded
        };
        let original = program(
            &[Bool, Int4],
            Int4,
            &[
                I::Input(0),
                I::JumpIfNotTrue(4),
                I::Input(1),
                I::Jump(5),
                I::Null(Int4),
            ],
        )
        .encode();
        for index in 0..original.len() {
            for replacement in [i32::MIN, -1, 0, 1, 2, 3, 4, 5, 32, 64, 256, i32::MAX] {
                let mut mutated = original.clone();
                mutated[index] = replacement;
                if let Ok(decoded) = validate_without_allocations(&mutated) {
                    assert_eq!(decoded.encode(), mutated);
                }
            }
        }
        let mut seed = 0x849a_8f12_u32;
        for length in 0..600 {
            let mut words = Vec::new();
            for _ in 0..length {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                words.push(seed as i32);
            }
            if length >= 3 {
                words[..3].copy_from_slice(&[SCALAR_PROGRAM_MAGIC, 2, 0]);
            }
            if let Ok(decoded) = validate_without_allocations(&words) {
                assert_eq!(decoded.encode(), words);
            }
        }
        assert_eq!(
            format!("{}", E::InvalidInput),
            "invalid scalar program: InvalidInput"
        );
    }
}
