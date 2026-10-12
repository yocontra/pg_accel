//! Exact three-valued predicate algebra over non-NULL INT4 intervals.
//! Separate TRUE/FALSE sets are essential: complementing the TRUE set alone
//! would incorrectly turn UNKNOWN (for example IN with a NULL member) TRUE.

use super::ShapeDecline;

type Ranges = Vec<(i32, i32)>;
const MAX_INTERMEDIATE_RANGES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Truth {
    False,
    True,
    Unknown,
}

impl Truth {
    fn not(self) -> Self {
        match self {
            Self::False => Self::True,
            Self::True => Self::False,
            Self::Unknown => Self::Unknown,
        }
    }

    fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::False, _) | (_, Self::False) => Self::False,
            (Self::True, Self::True) => Self::True,
            _ => Self::Unknown,
        }
    }
}

fn union(mut left: Ranges, right: Ranges) -> Result<Ranges, ShapeDecline> {
    left.extend(right);
    left.sort_unstable();
    let mut result: Ranges = Vec::new();
    for (lo, hi) in left {
        if let Some(last) = result.last_mut()
            && i64::from(lo) <= i64::from(last.1) + 1
        {
            last.1 = last.1.max(hi);
        } else {
            result.push((lo, hi));
        }
    }
    if result.len() > MAX_INTERMEDIATE_RANGES {
        return Err(ShapeDecline::UnsupportedAggregateModifier);
    }
    Ok(result)
}

fn intersection(left: Ranges, right: Ranges) -> Result<Ranges, ShapeDecline> {
    let (mut a, mut b) = (0, 0);
    let mut result = Vec::new();
    while a < left.len() && b < right.len() {
        let lo = left[a].0.max(right[b].0);
        let hi = left[a].1.min(right[b].1);
        if lo <= hi {
            result.push((lo, hi));
        }
        if left[a].1 < right[b].1 {
            a += 1;
        } else {
            b += 1;
        }
    }
    if result.len() > MAX_INTERMEDIATE_RANGES {
        return Err(ShapeDecline::UnsupportedAggregateModifier);
    }
    Ok(result)
}

fn complement(ranges: &[(i32, i32)]) -> Ranges {
    let mut next = i64::from(i32::MIN);
    let mut result = Vec::new();
    for &(lo, hi) in ranges {
        if next < i64::from(lo) {
            result.push((next as i32, lo - 1));
        }
        next = i64::from(hi) + 1;
    }
    if next <= i64::from(i32::MAX) {
        result.push((next as i32, i32::MAX));
    }
    result
}

#[derive(Debug, Clone)]
pub(super) struct Int4Predicate {
    pub true_ranges: Ranges,
    false_ranges: Ranges,
    pub null_truth: Truth,
}

impl Int4Predicate {
    pub fn is_null() -> Self {
        Self {
            true_ranges: vec![],
            false_ranges: vec![(i32::MIN, i32::MAX)],
            null_truth: Truth::True,
        }
    }

    pub fn constant(truth: Truth) -> Self {
        Self {
            true_ranges: if truth == Truth::True {
                vec![(i32::MIN, i32::MAX)]
            } else {
                vec![]
            },
            false_ranges: if truth == Truth::False {
                vec![(i32::MIN, i32::MAX)]
            } else {
                vec![]
            },
            null_truth: truth,
        }
    }

    /// PostgreSQL's five btree strategy numbers; None is a SQL NULL constant.
    pub fn comparison(value: Option<i32>, strategy: i32) -> Result<Self, ShapeDecline> {
        if !(1..=5).contains(&strategy) {
            return Err(ShapeDecline::UnsupportedAggregateModifier);
        }
        let Some(value) = value else {
            return Ok(Self::constant(Truth::Unknown));
        };
        let true_ranges = match strategy {
            1 => value
                .checked_sub(1)
                .map(|hi| (i32::MIN, hi))
                .into_iter()
                .collect(),
            2 => vec![(i32::MIN, value)],
            3 => vec![(value, value)],
            4 => vec![(value, i32::MAX)],
            5 => value
                .checked_add(1)
                .map(|lo| (lo, i32::MAX))
                .into_iter()
                .collect(),
            _ => unreachable!("strategy was checked"),
        };
        Ok(Self {
            false_ranges: complement(&true_ranges),
            true_ranges,
            null_truth: Truth::Unknown,
        })
    }

    pub fn not(self) -> Self {
        Self {
            true_ranges: self.false_ranges,
            false_ranges: self.true_ranges,
            null_truth: self.null_truth.not(),
        }
    }

    pub fn and(self, other: Self) -> Result<Self, ShapeDecline> {
        Ok(Self {
            true_ranges: intersection(self.true_ranges, other.true_ranges)?,
            false_ranges: union(self.false_ranges, other.false_ranges)?,
            null_truth: self.null_truth.and(other.null_truth),
        })
    }

    pub fn or(self, other: Self) -> Result<Self, ShapeDecline> {
        self.not().and(other.not()).map(Self::not)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(predicate: &Int4Predicate, input: Option<i32>) -> Truth {
        let Some(input) = input else {
            return predicate.null_truth;
        };
        let contains = |ranges: &Ranges| ranges.iter().any(|&(lo, hi)| lo <= input && input <= hi);
        assert!(!(contains(&predicate.true_ranges) && contains(&predicate.false_ranges)));
        if contains(&predicate.true_ranges) {
            Truth::True
        } else if contains(&predicate.false_ranges) {
            Truth::False
        } else {
            Truth::Unknown
        }
    }

    #[test]
    fn null_tests_keep_two_valued_null_semantics_under_composition() {
        let is_null = Int4Predicate::is_null();
        let is_not_null = is_null.clone().not();
        for input in [None, Some(i32::MIN), Some(0), Some(i32::MAX)] {
            assert_eq!(
                value(&is_null, input),
                if input.is_none() {
                    Truth::True
                } else {
                    Truth::False
                }
            );
            assert_eq!(
                value(&is_not_null, input),
                if input.is_none() {
                    Truth::False
                } else {
                    Truth::True
                }
            );
            let positive = Int4Predicate::comparison(Some(0), 5).expect("greater than");
            let null_or_positive = is_null.clone().or(positive.clone()).expect("or");
            let present_positive = is_not_null.clone().and(positive).expect("and");
            assert_eq!(
                value(&null_or_positive, input),
                if input.is_none_or(|v| v > 0) {
                    Truth::True
                } else {
                    Truth::False
                }
            );
            assert_eq!(
                value(&present_positive, input),
                if input.is_some_and(|v| v > 0) {
                    Truth::True
                } else {
                    Truth::False
                }
            );
        }
    }

    #[test]
    fn boolean_interval_algebra_matches_three_valued_truth_tables() {
        let mut predicates = vec![
            Int4Predicate::constant(Truth::True),
            Int4Predicate::constant(Truth::False),
            Int4Predicate::constant(Truth::Unknown),
        ];
        for constant in [
            None,
            Some(i32::MIN),
            Some(-1),
            Some(0),
            Some(1),
            Some(i32::MAX),
        ] {
            for strategy in 1..=5 {
                predicates.push(Int4Predicate::comparison(constant, strategy).expect("comparison"));
            }
        }
        for left in &predicates {
            for right in &predicates {
                let and = left.clone().and(right.clone()).expect("and");
                let or = left.clone().or(right.clone()).expect("or");
                let not = or.clone().not();
                for input in [
                    None,
                    Some(i32::MIN),
                    Some(i32::MIN + 1),
                    Some(-2),
                    Some(-1),
                    Some(0),
                    Some(1),
                    Some(2),
                    Some(i32::MAX - 1),
                    Some(i32::MAX),
                ] {
                    let (a, b) = (value(left, input), value(right, input));
                    assert_eq!(value(&and, input), a.and(b));
                    assert_eq!(value(&or, input), a.not().and(b.not()).not());
                    assert_eq!(value(&not, input), a.not().and(b.not()));
                }
            }
        }
    }

    #[test]
    fn comparison_ranges_match_integer_ordering_including_extreme_strict_bounds() {
        let probes = [i32::MIN, i32::MIN + 1, -1, 0, 1, i32::MAX - 1, i32::MAX];
        for constant in probes {
            for strategy in 1..=5 {
                let predicate =
                    Int4Predicate::comparison(Some(constant), strategy).expect("comparison");
                assert_eq!(value(&predicate, None), Truth::Unknown);
                for input in probes {
                    let expected = match strategy {
                        1 => input < constant,
                        2 => input <= constant,
                        3 => input == constant,
                        4 => input >= constant,
                        5 => input > constant,
                        _ => unreachable!(),
                    };
                    assert_eq!(
                        value(&predicate, Some(input)),
                        if expected { Truth::True } else { Truth::False }
                    );
                }
            }
        }
    }

    #[test]
    fn in_null_members_do_not_become_true_under_negation() {
        let membership = Int4Predicate::comparison(Some(7), 3)
            .expect("equality")
            .or(Int4Predicate::constant(Truth::Unknown))
            .expect("IN");
        assert_eq!(membership.true_ranges, vec![(7, 7)]);
        assert_eq!(value(&membership, Some(8)), Truth::Unknown);
        assert!(membership.not().true_ranges.is_empty());
    }

    #[test]
    fn interval_union_coalesces_duplicates_adjacency_and_endpoints() {
        assert_eq!(
            union(
                vec![(1, 2), (i32::MAX, i32::MAX)],
                vec![(2, 3), (4, 4), (i32::MIN, i32::MIN)]
            )
            .expect("union"),
            vec![(i32::MIN, i32::MIN), (1, 4), (i32::MAX, i32::MAX)]
        );
        assert_eq!(
            complement(&[(i32::MIN, i32::MAX)]),
            Vec::<(i32, i32)>::new()
        );
        assert_eq!(complement(&[]), vec![(i32::MIN, i32::MAX)]);
        assert!(union((0..257).map(|x| (x * 2, x * 2)).collect(), vec![]).is_err());
    }
}
