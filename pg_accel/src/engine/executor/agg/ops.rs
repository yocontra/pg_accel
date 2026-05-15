//! `AggOp` — which aggregate operation to perform (SUM / AVG / MIN / MAX / COUNT / Passthrough).
//!
//! Encoded as an integer for serialization into `custom_private`.

/// Which aggregate operation to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggOp {
    /// SUM aggregate.
    Sum,
    /// AVG aggregate (sum + count).
    Avg,
    /// MIN aggregate.
    Min,
    /// MAX aggregate.
    Max,
    /// COUNT aggregate.
    Count,
    /// Unknown / passthrough.
    Passthrough,
    /// `STDDEV_SAMP` aggregate (sample standard deviation).
    StddevSamp,
    /// `STDDEV_POP` aggregate (population standard deviation).
    StddevPop,
    /// `VAR_SAMP` aggregate (sample variance).
    VarSamp,
    /// `VAR_POP` aggregate (population variance).
    VarPop,
    /// `BIT_AND` aggregate (bitwise AND over integer column).
    BitAnd,
    /// `BIT_OR` aggregate (bitwise OR over integer column).
    BitOr,
    /// `BIT_XOR` aggregate (bitwise XOR over integer column). Available in
    /// PostgreSQL 14+; transition function is `int{2,4,8}xor`.
    BitXor,
    /// `BOOL_AND` aggregate (logical AND over bool column).
    BoolAnd,
    /// `BOOL_OR` aggregate (logical OR over bool column).
    BoolOr,
}

/// Encode `AggOp` as an integer for serialization into `custom_private`.
impl AggOp {
    #[must_use]
    pub const fn to_i32(self) -> i32 {
        match self {
            Self::Sum => 0,
            Self::Avg => 1,
            Self::Min => 2,
            Self::Max => 3,
            Self::Count => 4,
            Self::Passthrough => 5,
            Self::StddevSamp => 6,
            Self::StddevPop => 7,
            Self::VarSamp => 8,
            Self::VarPop => 9,
            Self::BitAnd => 10,
            Self::BitOr => 11,
            Self::BoolAnd => 12,
            Self::BoolOr => 13,
            Self::BitXor => 14,
        }
    }

    #[must_use]
    pub const fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::Sum),
            1 => Some(Self::Avg),
            2 => Some(Self::Min),
            3 => Some(Self::Max),
            4 => Some(Self::Count),
            5 => Some(Self::Passthrough),
            6 => Some(Self::StddevSamp),
            7 => Some(Self::StddevPop),
            8 => Some(Self::VarSamp),
            9 => Some(Self::VarPop),
            10 => Some(Self::BitAnd),
            11 => Some(Self::BitOr),
            12 => Some(Self::BoolAnd),
            13 => Some(Self::BoolOr),
            14 => Some(Self::BitXor),
            _ => None,
        }
    }
}
