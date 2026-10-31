//! Bounded, pointer-free control for adaptive H3 COUNT attempts.
//!
//! Capacity is an allocation budget, never a cardinality proof. Failed attempts
//! cannot publish output or teach a bound. Prefixes split exact key bits, so
//! even adversarial hash collisions terminate after at most 64 levels.

use crate::gpu::HashCountPartition;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Node {
    All,
    Null,
    Prefix { bits: u32, value: u64 },
}

/// No row-sized control storage: at most one pending sibling per key bit and
/// a separate NULL branch. Every successful node is retired exactly once.
pub(super) struct HashPartitionDriver {
    current: Option<Node>,
    pending: [Option<Node>; 65],
    pending_len: usize,
    capacity: usize,
    maximum: usize,
    pressure_retry_used: bool,
    pub overflowed: bool,
    pub completed_leaves: u64,
    pub selected_rows: u64,
    whole_rows: Option<u64>,
}

impl HashPartitionDriver {
    pub(super) fn new(initial: usize, maximum: usize) -> Result<Self, String> {
        if initial == 0 || maximum == 0 || initial > maximum {
            return Err("invalid adaptive hash capacity interval".into());
        }
        Ok(Self {
            current: Some(Node::All),
            pending: [None; 65],
            pending_len: 0,
            capacity: initial,
            maximum,
            pressure_retry_used: false,
            overflowed: false,
            completed_leaves: 0,
            selected_rows: 0,
            whole_rows: None,
        })
    }

    pub(super) fn attempt(&self) -> Result<Option<(HashCountPartition, usize)>, String> {
        let Some(node) = self.current else {
            return Ok(None);
        };
        let control = match node {
            Node::All => HashCountPartition::ALL,
            Node::Null => HashCountPartition::NULL,
            Node::Prefix { bits, value } => {
                HashCountPartition::prefix(bits, value).map_err(|error| error.to_string())?
            }
        };
        Ok(Some((control, self.capacity)))
    }

    /// Called only for exact typed native HASH_CAPACITY, with no published rows.
    /// A failed read-only budget quote may set grow=false without allocating.
    pub(super) fn capacity_overflow(&mut self, grow: bool) -> Result<(), String> {
        self.overflowed = true;
        if grow && self.capacity < self.maximum {
            self.capacity = self.next_capacity();
            return Ok(());
        }
        match self.current.ok_or("overflow after adaptive hash EOF")? {
            Node::All => {
                self.push(Node::Null)?;
                self.current = Some(Node::Prefix { bits: 0, value: 0 });
            }
            Node::Prefix { bits, value } if bits < 64 => {
                let next = bits + 1;
                self.push(Node::Prefix {
                    bits: next,
                    value: value | (1_u64 << (64 - next)),
                })?;
                self.current = Some(Node::Prefix { bits: next, value });
            }
            Node::Null | Node::Prefix { .. } => {
                return Err("single-key hash leaf reported capacity overflow".into());
            }
        }
        Ok(())
    }

    pub(super) fn next_capacity(&self) -> usize {
        self.capacity.saturating_mul(2).min(self.maximum)
    }

    /// At most one replacement-allocation race recovery for the WHOLE query.
    /// Shrink also caps later growth, so the same rejected size is not retried.
    pub(super) fn pressure_shrink(&mut self) -> Result<(), String> {
        if self.pressure_retry_used || self.capacity <= 1 {
            return Err("adaptive hash replacement memory admission failed".into());
        }
        self.pressure_retry_used = true;
        self.capacity /= 2;
        self.maximum = self.capacity;
        self.overflowed = true;
        Ok(())
    }

    /// Only successful native calls reach this boundary. The whole-WHERE
    /// population must be stable across disjoint leaves, with exact final sum.
    pub(super) fn complete_population_leaf(
        &mut self,
        selected: u64,
        whole: u64,
        input: u64,
    ) -> Result<(), String> {
        let node = self
            .current
            .ok_or("duplicate adaptive hash leaf completion")?;
        if whole > input
            || selected > whole
            || self.whole_rows.is_some_and(|prior| prior != whole)
            || (node == Node::All && selected != whole)
        {
            return Err("adaptive hash population contract mismatch".into());
        }
        let sum = self
            .selected_rows
            .checked_add(selected)
            .ok_or("adaptive hash population overflow")?;
        if sum > whole || (self.pending_len == 0 && sum != whole) {
            return Err("adaptive partitions do not cover WHERE population".into());
        }
        self.complete_leaf(selected)?;
        self.whole_rows = Some(whole);
        Ok(())
    }

    fn complete_leaf(&mut self, selected: u64) -> Result<(), String> {
        if self.current.take().is_none() {
            return Err("duplicate adaptive hash leaf completion".into());
        }
        self.selected_rows = self
            .selected_rows
            .checked_add(selected)
            .ok_or("adaptive hash population overflow")?;
        self.completed_leaves = self
            .completed_leaves
            .checked_add(1)
            .ok_or("adaptive hash leaf counter overflow")?;
        if self.pending_len != 0 {
            self.pending_len -= 1;
            self.current = self.pending[self.pending_len].take();
        }
        Ok(())
    }

    fn push(&mut self, node: Node) -> Result<(), String> {
        let slot = self
            .pending
            .get_mut(self.pending_len)
            .ok_or("adaptive hash prefix stack exceeded 64 bits plus NULL")?;
        *slot = Some(node);
        self.pending_len += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn where_population_is_stable_exhaustive_and_success_only() {
        let mut driver = HashPartitionDriver::new(1, 1).expect("capacity");
        assert!(driver.complete_population_leaf(3, 4, 8).is_err());
        assert_eq!(driver.completed_leaves, 0);
        driver.capacity_overflow(false).expect("split failed root");
        assert_eq!(driver.whole_rows, None, "failed root contributes nothing");
        driver
            .complete_population_leaf(3, 4, 8)
            .expect("nonnull leaf");
        assert!(driver.complete_population_leaf(1, 5, 8).is_err());
        assert!(driver.complete_population_leaf(0, 4, 8).is_err());
        assert!(driver.complete_population_leaf(2, 4, 8).is_err());
        assert_eq!(driver.completed_leaves, 1);
        driver
            .complete_population_leaf(1, 4, 8)
            .expect("NULL leaf closes exact sum");
        assert_eq!(driver.selected_rows, 4);
        assert!(driver.attempt().expect("EOF").is_none());
        assert!(driver.complete_population_leaf(0, 4, 8).is_err());
        for input in [0, 8] {
            let mut empty = HashPartitionDriver::new(1, 1).expect("capacity");
            empty
                .complete_population_leaf(0, 0, input)
                .expect("empty or all rejected");
            assert!(empty.attempt().expect("EOF").is_none());
        }
        let mut overflow = HashPartitionDriver::new(1, 1).expect("capacity");
        overflow.capacity_overflow(false).expect("split");
        overflow.selected_rows = u64::MAX;
        assert!(
            overflow
                .complete_population_leaf(1, u64::MAX, u64::MAX)
                .is_err()
        );
        assert_eq!(overflow.completed_leaves, 0);
        assert!(overflow.complete_population_leaf(0, 10, 9).is_err());
    }

    #[test]
    fn growth_then_exact_prefixes_have_bounded_depth_and_no_eof_redispatch() {
        let mut driver = HashPartitionDriver::new(1, 3).expect("qualification fixture invariant");
        driver
            .capacity_overflow(true)
            .expect("qualification fixture invariant");
        assert_eq!(driver.capacity, 2);
        driver
            .capacity_overflow(true)
            .expect("qualification fixture invariant");
        assert_eq!(driver.capacity, 3);
        driver
            .capacity_overflow(true)
            .expect("qualification fixture invariant");
        for _ in 0..64 {
            driver
                .capacity_overflow(false)
                .expect("qualification fixture invariant");
        }
        assert_eq!(driver.pending_len, 65);
        assert!(driver.capacity_overflow(false).is_err());
        for _ in 0..66 {
            driver
                .complete_leaf(1)
                .expect("qualification fixture invariant");
        }
        assert!(
            driver
                .attempt()
                .expect("qualification fixture invariant")
                .is_none()
        );
        assert!(driver.complete_leaf(0).is_err());
        assert_eq!(driver.selected_rows, 66);
    }

    #[test]
    fn pressure_allowance_is_query_wide_and_never_raises_caps() {
        let mut driver = HashPartitionDriver::new(4, 16).expect("qualification fixture invariant");
        driver
            .pressure_shrink()
            .expect("qualification fixture invariant");
        assert_eq!((driver.capacity, driver.maximum), (2, 2));
        driver
            .capacity_overflow(true)
            .expect("qualification fixture invariant");
        driver
            .complete_leaf(0)
            .expect("qualification fixture invariant");
        assert!(driver.pressure_shrink().is_err());
        assert!(HashPartitionDriver::new(0, 1).is_err());
        assert!(HashPartitionDriver::new(2, 1).is_err());
    }

    #[test]
    fn exhaustive_small_domain_remains_disjoint_after_completed_leaf_and_shrink() {
        // Unequal duplicate populations catch accidental counting of groups as
        // rows. Include zero, the high bit, and an independent NULL branch.
        let keys: Vec<Option<u64>> = (0..16_u64)
            .map(|key| Some(key << 60))
            .chain(std::iter::once(None))
            .collect();
        let mut visits = [0_u32; 17];
        let mut driver = HashPartitionDriver::new(2, 2).expect("qualification fixture invariant");
        let mut shrunk = false;
        let mut attempts = 0;
        while let Some(node) = driver.current {
            attempts += 1;
            assert!(
                attempts < 100,
                "bounded exact-prefix traversal must terminate"
            );
            let selected: Vec<usize> = keys
                .iter()
                .enumerate()
                .filter_map(|(index, key)| {
                    let included = match (node, key) {
                        (Node::All, _) | (Node::Null, None) => true,
                        (Node::Prefix { bits: 0, .. }, Some(_)) => true,
                        (Node::Prefix { bits, value }, Some(key)) => {
                            key & (u64::MAX << (64 - bits)) == value
                        }
                        _ => false,
                    };
                    included.then_some(index)
                })
                .collect();
            if selected.len() > driver.capacity {
                driver
                    .capacity_overflow(false)
                    .expect("qualification fixture invariant");
                continue;
            }
            let population = selected.iter().map(|index| (*index + 1) as u64).sum();
            for index in selected {
                visits[index] += 1;
            }
            driver
                .complete_leaf(population)
                .expect("qualification fixture invariant");
            if !shrunk && driver.current.is_some() {
                driver
                    .pressure_shrink()
                    .expect("qualification fixture invariant");
                shrunk = true;
            }
        }
        assert!(shrunk);
        assert_eq!(visits, [1; 17]);
        assert_eq!(driver.selected_rows, (1..=17).sum::<u64>());
        assert!(
            driver
                .attempt()
                .expect("qualification fixture invariant")
                .is_none()
        );
        assert!(driver.pressure_shrink().is_err());
    }
}
