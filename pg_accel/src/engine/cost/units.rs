//! Explicit units used by cost-model scaffolding.

/// Conversion factor for the convention documented by the existing planner
/// cost constants: one PostgreSQL cost unit is treated as roughly one
/// millisecond when deriving GPU overheads.
pub const MICROS_PER_PG_COST_UNIT: f64 = 1_000.0;

/// PostgreSQL planner cost units.
#[derive(Debug, Clone, Copy, Default, PartialEq, PartialOrd)]
pub struct PgCost(f64);

impl PgCost {
    /// Zero cost.
    pub const ZERO: Self = Self(0.0);

    /// Wrap a raw PostgreSQL planner cost value.
    #[must_use]
    pub const fn new(value: f64) -> Self {
        Self(value)
    }

    /// Return the raw PostgreSQL planner cost value.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }

    /// Convert a wall-clock microsecond estimate into planner cost units.
    #[must_use]
    pub fn from_micros(micros: Micros) -> Self {
        Self(micros.get() as f64 / MICROS_PER_PG_COST_UNIT)
    }

    /// Convert planner cost units into a wall-clock microsecond estimate.
    #[must_use]
    pub fn to_micros(self) -> f64 {
        self.0 * MICROS_PER_PG_COST_UNIT
    }
}

impl From<f64> for PgCost {
    fn from(value: f64) -> Self {
        Self::new(value)
    }
}

impl From<PgCost> for f64 {
    fn from(value: PgCost) -> Self {
        value.get()
    }
}

/// Row-count cardinality.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rows(usize);

impl Rows {
    /// Wrap a raw row count.
    #[must_use]
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Return the raw row count.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

impl From<usize> for Rows {
    fn from(value: usize) -> Self {
        Self::new(value)
    }
}

impl From<Rows> for usize {
    fn from(value: Rows) -> Self {
        value.get()
    }
}

/// Byte count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bytes(usize);

impl Bytes {
    /// Wrap a raw byte count.
    #[must_use]
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Return the raw byte count.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

impl From<usize> for Bytes {
    fn from(value: usize) -> Self {
        Self::new(value)
    }
}

impl From<Bytes> for usize {
    fn from(value: Bytes) -> Self {
        value.get()
    }
}

/// Wall-clock microseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Micros(u64);

impl Micros {
    /// Wrap a raw microsecond count.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the raw microsecond count.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for Micros {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}

impl From<Micros> for u64 {
    fn from(value: Micros) -> Self {
        value.get()
    }
}

/// Product-style work metric, such as vertices times rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkProduct(u64);

impl WorkProduct {
    /// Wrap a raw work-product value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the raw work-product value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for WorkProduct {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}

impl From<WorkProduct> for u64 {
    fn from(value: WorkProduct) -> Self {
        value.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg_cost_round_trips_raw_value() {
        let cost = PgCost::from(12.5);
        assert_eq!(cost.get(), 12.5);
        assert_eq!(f64::from(cost), 12.5);
    }

    #[test]
    fn pg_cost_converts_micros_by_documented_cost_unit() {
        let micros = Micros::new(2_500);
        let cost = PgCost::from_micros(micros);
        assert_eq!(cost.get(), 2.5);
        assert_eq!(cost.to_micros(), 2_500.0);
    }

    #[test]
    fn integer_units_round_trip_raw_values() {
        assert_eq!(usize::from(Rows::from(42)), 42);
        assert_eq!(usize::from(Bytes::from(4096)), 4096);
        assert_eq!(u64::from(Micros::from(100)), 100);
        assert_eq!(u64::from(WorkProduct::from(123_456)), 123_456);
    }
}
