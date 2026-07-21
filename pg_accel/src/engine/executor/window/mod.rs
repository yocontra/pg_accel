//! Window plan descriptors retained for private-data wire compatibility.
//!
//! The host-staged window executor was retired with its planner injector.
//! These types remain because older numeric strategy tags and descriptor
//! payloads must continue to decode deterministically and fail closed.

/// Window function discriminant stored in `custom_private`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowFunc {
    RowNumber,
    Rank,
    DenseRank,
    Sum,
    Count,
    Lag,
    Lead,
}

impl WindowFunc {
    #[must_use]
    pub const fn to_i32(self) -> i32 {
        match self {
            Self::RowNumber => 0,
            Self::Rank => 1,
            Self::DenseRank => 2,
            Self::Sum => 3,
            Self::Count => 4,
            Self::Lag => 5,
            Self::Lead => 6,
        }
    }

    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::RowNumber),
            1 => Some(Self::Rank),
            2 => Some(Self::DenseRank),
            3 => Some(Self::Sum),
            4 => Some(Self::Count),
            5 => Some(Self::Lag),
            6 => Some(Self::Lead),
            _ => None,
        }
    }
}

/// Serialized specification for one retired window function.
#[derive(Debug, Clone)]
pub struct WindowFuncSpec {
    pub func: WindowFunc,
    pub partition_attno: i32,
    pub order_attno: i32,
    pub value_attno: i32,
    pub offset: i32,
    pub default_val: f64,
    pub result_type_oid: u32,
    pub uses_fp64: bool,
}

/// Integer fields in one serialized window descriptor.
pub const WINDOW_SPEC_INTS: usize = 8;
