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
        }
    }

    #[must_use]
    pub const fn from_i32(v: i32) -> Self {
        match v {
            0 => Self::Sum,
            1 => Self::Avg,
            2 => Self::Min,
            3 => Self::Max,
            4 => Self::Count,
            _ => Self::Passthrough,
        }
    }
}
