//! JQS1: pointer-free semantics for a two-input resident equijoin stage.
//!
//! Occurrences identify range-table entries, NOT physical tables. Two aliases
//! can share resident storage without sharing their logical row identity.
//! This staged contract is not PostgreSQL path admission. It represents only
//! exact integer equality and direct projections: no residual ON/WHERE quals,
//! casts, security conditions, parameterization or arbitrary pipeline graph.

pub const MAX_JOIN_KEYS: usize = 4;
pub const MAX_JOIN_COLUMNS: usize = 16;
const MAGIC: i32 = 0x4a51_5331; // JQS1
const VERSION: i32 = 1;
const HEADER: usize = 10;
pub const MAX_JOIN_SPEC_WORDS: usize = HEADER + MAX_JOIN_KEYS * 6 + MAX_JOIN_COLUMNS * 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelationOccurrence {
    pub id: u32,
    pub relation_oid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OccurrenceColumn {
    pub occurrence: u32,
    pub attno: i32,
    pub type_oid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum JoinKind {
    Inner = 0,
    Left = 1,
    Semi = 2,
    Anti = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinKey {
    pub outer: OccurrenceColumn,
    pub inner: OccurrenceColumn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinQuerySpec {
    pub outer: RelationOccurrence,
    pub inner: RelationOccurrence,
    pub kind: JoinKind,
    pub keys: Vec<JoinKey>,
    /// Ordered consumer lanes; repeated columns are legal. SQL result slots
    /// and their typmods/collations belong to the eventual consumer contract.
    pub columns: Vec<OccurrenceColumn>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinSpecError(pub &'static str);

impl std::fmt::Display for JoinSpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for JoinSpecError {}

#[cfg_attr(not(any(test, feature = "pg_test")), allow(dead_code))] // Staged join wire; no SQL admission yet.
impl JoinQuerySpec {
    pub fn relation(&self, occurrence: u32) -> Result<RelationOccurrence, JoinSpecError> {
        if occurrence == self.outer.id {
            Ok(self.outer)
        } else if occurrence == self.inner.id {
            Ok(self.inner)
        } else {
            Err(JoinSpecError(
                "join column references an unknown occurrence",
            ))
        }
    }

    fn all_columns(&self) -> impl Iterator<Item = OccurrenceColumn> + '_ {
        self.keys
            .iter()
            .flat_map(|key| [key.outer, key.inner])
            .chain(self.columns.iter().copied())
    }

    pub fn validate(&self) -> Result<(), JoinSpecError> {
        for relation in [self.outer, self.inner] {
            if relation.id == 0 || relation.id > i32::MAX as u32 || relation.relation_oid == 0 {
                return Err(JoinSpecError("invalid join relation occurrence"));
            }
        }
        if self.outer.id == self.inner.id {
            return Err(JoinSpecError(
                "join inputs must have distinct occurrence identities",
            ));
        }
        if self.keys.is_empty() || self.keys.len() > MAX_JOIN_KEYS {
            return Err(JoinSpecError("join requires one to four equality keys"));
        }
        if self.columns.is_empty() || self.columns.len() > MAX_JOIN_COLUMNS {
            return Err(JoinSpecError(
                "join requires one to sixteen consumer columns",
            ));
        }
        for (index, key) in self.keys.iter().enumerate() {
            if key.outer.occurrence != self.outer.id || key.inner.occurrence != self.inner.id {
                return Err(JoinSpecError(
                    "join keys must follow outer/inner occurrence orientation",
                ));
            }
            if !matches!(key.outer.type_oid, 20 | 23) || key.outer.type_oid != key.inner.type_oid {
                return Err(JoinSpecError(
                    "join equality requires matching INT4 or INT8 types",
                ));
            }
            if self.keys[..index].contains(key) {
                return Err(JoinSpecError("duplicate join equality key"));
            }
        }
        for column in &self.columns {
            if matches!(self.kind, JoinKind::Semi | JoinKind::Anti)
                && column.occurrence != self.outer.id
            {
                return Err(JoinSpecError("semi/anti joins cannot expose inner columns"));
            }
        }
        // Bounded metadata-only quadratic check (at most 24 references). Even
        // across aliases, one physical attribute cannot have conflicting types.
        for column in self.all_columns() {
            let relation = self.relation(column.occurrence)?;
            if !(1..=1600).contains(&column.attno)
                || !matches!(column.type_oid, 16 | 20 | 23 | 1082)
            {
                return Err(JoinSpecError("unsupported join column attribute or type"));
            }
            for other in self.all_columns() {
                if self.relation(other.occurrence)?.relation_oid == relation.relation_oid
                    && other.attno == column.attno
                    && other.type_oid != column.type_oid
                {
                    return Err(JoinSpecError("physical join column has conflicting types"));
                }
            }
        }
        Ok(())
    }

    /// Canonical physical dependencies: aliases share one sorted OID/attribute
    /// entry, but the logical spec and wire never erase their occurrences.
    pub fn physical_columns(&self) -> Result<Vec<super::ColumnRef>, JoinSpecError> {
        self.validate()?;
        let mut columns = self
            .all_columns()
            .map(|column| {
                Ok(super::ColumnRef {
                    relation_oid: self.relation(column.occurrence)?.relation_oid,
                    attno: column.attno,
                    type_oid: column.type_oid,
                })
            })
            .collect::<Result<Vec<_>, JoinSpecError>>()?;
        columns.sort_by_key(|column| (column.relation_oid, column.attno));
        columns.dedup();
        Ok(columns)
    }

    pub fn encode(&self) -> Result<Vec<i32>, JoinSpecError> {
        self.validate()?;
        let length = HEADER + self.keys.len() * 6 + self.columns.len() * 3;
        let mut words = Vec::with_capacity(length);
        words.extend([
            MAGIC,
            VERSION,
            length as i32,
            self.kind as i32,
            self.outer.id as i32,
            self.outer.relation_oid as i32,
            self.inner.id as i32,
            self.inner.relation_oid as i32,
            self.keys.len() as i32,
            self.columns.len() as i32,
        ]);
        for column in self.all_columns() {
            words.extend([
                column.occurrence as i32,
                column.attno,
                column.type_oid as i32,
            ]);
        }
        Ok(words)
    }

    pub fn decode(words: &[i32]) -> Result<Self, JoinSpecError> {
        // Counts, total size and tags are checked before allocating containers
        // or slicing any variable section. OIDs preserve all unsigned bits.
        if !(HEADER..=MAX_JOIN_SPEC_WORDS).contains(&words.len()) {
            return Err(JoinSpecError("invalid join wire length"));
        }
        if words[0] != MAGIC || words[1] != VERSION || words[2] != words.len() as i32 {
            return Err(JoinSpecError("invalid join wire header"));
        }
        let kind = match words[3] {
            0 => JoinKind::Inner,
            1 => JoinKind::Left,
            2 => JoinKind::Semi,
            3 => JoinKind::Anti,
            _ => return Err(JoinSpecError("unknown join kind")),
        };
        if !(1..=MAX_JOIN_KEYS as i32).contains(&words[8])
            || !(1..=MAX_JOIN_COLUMNS as i32).contains(&words[9])
        {
            return Err(JoinSpecError("invalid join wire counts"));
        }
        let keys = words[8] as usize;
        let columns = words[9] as usize;
        if words.len() != HEADER + keys * 6 + columns * 3 {
            return Err(JoinSpecError("join wire sections do not match length"));
        }
        let column = |offset: usize| OccurrenceColumn {
            occurrence: words[offset] as u32,
            attno: words[offset + 1],
            type_oid: words[offset + 2] as u32,
        };
        let spec = Self {
            outer: RelationOccurrence {
                id: words[4] as u32,
                relation_oid: words[5] as u32,
            },
            inner: RelationOccurrence {
                id: words[6] as u32,
                relation_oid: words[7] as u32,
            },
            kind,
            keys: (0..keys)
                .map(|index| JoinKey {
                    outer: column(HEADER + index * 6),
                    inner: column(HEADER + index * 6 + 3),
                })
                .collect(),
            columns: (0..columns)
                .map(|index| column(HEADER + keys * 6 + index * 3))
                .collect(),
        };
        spec.validate()?;
        Ok(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> JoinQuerySpec {
        let outer = OccurrenceColumn {
            occurrence: 2,
            attno: 1,
            type_oid: 23,
        };
        let inner = OccurrenceColumn {
            occurrence: 7,
            attno: 2,
            type_oid: 23,
        };
        JoinQuerySpec {
            outer: RelationOccurrence {
                id: 2,
                relation_oid: u32::MAX,
            },
            inner: RelationOccurrence {
                id: 7,
                relation_oid: u32::MAX,
            },
            kind: JoinKind::Left,
            keys: vec![JoinKey { outer, inner }],
            columns: vec![inner, outer, inner],
        }
    }

    #[test]
    fn occurrence_wire_preserves_self_aliases_and_projection_order() {
        let spec = spec();
        assert_eq!(
            JoinQuerySpec::decode(&spec.encode().expect("encode")).expect("decode"),
            spec
        );
        let columns = spec.physical_columns().expect("physical dependencies");
        assert_eq!(columns.len(), 2);
        assert_eq!(columns[0].attno, 1);
        assert_eq!(columns[1].attno, 2);
        assert_eq!(spec.relation(7), Ok(spec.inner));
        assert!(spec.relation(1).is_err());
        let mut other = spec.clone();
        other.inner.id = 8;
        other.keys[0].inner.occurrence = 8;
        other.columns[0].occurrence = 8;
        other.columns[2].occurrence = 8;
        assert_eq!(other.physical_columns(), spec.physical_columns());
        assert_ne!(
            other.encode(),
            spec.encode(),
            "aliases remain part of plan identity"
        );
    }

    #[test]
    fn occurrence_spec_rejects_invalid_semantics() {
        let base = spec();
        let reject = |mutate: fn(&mut JoinQuerySpec)| {
            let mut spec = base.clone();
            mutate(&mut spec);
            assert!(spec.validate().is_err(), "{spec:?}");
            assert!(spec.encode().is_err());
        };
        reject(|s| s.inner.id = s.outer.id);
        reject(|s| s.outer.id = 0);
        reject(|s| s.inner.id = u32::MAX);
        reject(|s| s.outer.relation_oid = 0);
        reject(|s| s.keys.clear());
        reject(|s| s.keys = vec![s.keys[0]; 5]);
        reject(|s| s.keys.push(s.keys[0]));
        reject(|s| s.columns.clear());
        reject(|s| s.columns = vec![s.columns[0]; 17]);
        reject(|s| s.keys[0].outer.occurrence = s.inner.id);
        reject(|s| s.keys[0].inner.type_oid = 20);
        reject(|s| {
            s.keys[0].outer.type_oid = 1082;
            s.keys[0].inner.type_oid = 1082;
        });
        reject(|s| s.columns[0].occurrence = 99);
        reject(|s| s.columns[0].attno = 0);
        reject(|s| s.columns[0].attno = -1);
        reject(|s| s.columns[0].attno = 1601);
        reject(|s| s.columns[0].type_oid = 25);
        reject(|s| s.columns[0].type_oid = 20);
        reject(|s| {
            s.columns[0].occurrence = s.outer.id;
            s.columns[0].type_oid = 20;
        });
        reject(|s| s.kind = JoinKind::Semi);
        reject(|s| s.kind = JoinKind::Anti);
    }

    #[test]
    fn occurrence_wire_rejects_truncation_extra_words_and_corrupt_counts() {
        let words = spec().encode().expect("wire");
        for length in 0..words.len() {
            assert!(JoinQuerySpec::decode(&words[..length]).is_err());
        }
        for (index, value) in [
            (0, 0),
            (1, 2),
            (2, 0),
            (3, -1),
            (3, 4),
            (4, -1),
            (5, 0),
            (8, -1),
            (8, 0),
            (8, 5),
            (8, i32::MAX),
            (9, -1),
            (9, 0),
            (9, 17),
            (9, i32::MAX),
            (10, 0),
            (11, -1),
            (12, 25),
        ] {
            let mut corrupt = words.clone();
            corrupt[index] = value;
            assert!(
                JoinQuerySpec::decode(&corrupt).is_err(),
                "index {index}: {value}"
            );
        }
        let mut extra = words;
        extra.push(0);
        extra[2] = extra.len() as i32;
        assert!(JoinQuerySpec::decode(&extra).is_err());
        assert!(JoinQuerySpec::decode(&[0; MAX_JOIN_SPEC_WORDS + 1]).is_err());
    }

    #[test]
    fn occurrence_wire_covers_every_kind_and_maximum_width() {
        for kind in [
            JoinKind::Inner,
            JoinKind::Left,
            JoinKind::Semi,
            JoinKind::Anti,
        ] {
            let mut spec = spec();
            spec.kind = kind;
            spec.inner.relation_oid = 10;
            spec.keys = (1..=4)
                .map(|attno| JoinKey {
                    outer: OccurrenceColumn {
                        occurrence: 2,
                        attno,
                        type_oid: if attno % 2 == 0 { 20 } else { 23 },
                    },
                    inner: OccurrenceColumn {
                        occurrence: 7,
                        attno,
                        type_oid: if attno % 2 == 0 { 20 } else { 23 },
                    },
                })
                .collect();
            spec.columns = (0..16)
                .map(|index| OccurrenceColumn {
                    occurrence: if index % 2 == 0 || matches!(kind, JoinKind::Semi | JoinKind::Anti)
                    {
                        2
                    } else {
                        7
                    },
                    attno: index + 5,
                    type_oid: [16, 20, 23, 1082][index as usize % 4],
                })
                .collect();
            let words = spec.encode().expect("maximum shape");
            assert_eq!(words.len(), MAX_JOIN_SPEC_WORDS);
            assert_eq!(JoinQuerySpec::decode(&words), Ok(spec));
        }
    }
}
