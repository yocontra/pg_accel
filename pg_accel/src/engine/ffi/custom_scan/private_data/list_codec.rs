//! Typed helpers for PostgreSQL `List *` integer wire layouts.

use std::ffi::c_int;

use pgrx::pg_sys;

enum IntListSource<'a> {
    #[allow(dead_code)] // exercised by strict unit decoders; runtime uses PG List storage.
    Slice(&'a [c_int]),
    PgList {
        list: *mut pg_sys::List,
        len: usize,
    },
}

/// Read-only integer list view.
///
/// Production constructors reject non-`Integer` nodes before any payload is
/// read. Strict decoders use [`get`](Self::get) and never infer an omitted
/// field as zero.
pub(super) struct IntListReader<'a> {
    source: IntListSource<'a>,
}

impl<'a> IntListReader<'a> {
    #[must_use]
    #[allow(dead_code)] // exercised by strict unit decoders; runtime uses PG List storage.
    pub(super) const fn from_slice(fields: &'a [c_int]) -> Self {
        Self {
            source: IntListSource::Slice(fields),
        }
    }

    /// Build a reader over a PostgreSQL `List *` of `Integer` nodes.
    ///
    /// # Safety
    ///
    /// `list` must be null or a valid PG `List` of `Integer` nodes.
    #[must_use]
    pub(super) unsafe fn from_pg_list(list: *mut pg_sys::List) -> Self {
        let len = if list.is_null() {
            0
        } else {
            // SAFETY: caller guarantees `list` is a valid List when non-null.
            unsafe { pg_sys::list_length(list) as usize }
        };
        for index in 0..len {
            // SAFETY: caller guarantees `list` is a valid PostgreSQL List.
            let node = unsafe { pg_sys::list_nth(list, index as c_int) };
            if node.is_null() {
                pgrx::error!("pg_accel: null node at custom_private index {index}");
            }
            // SAFETY: every PostgreSQL node starts with a NodeTag.
            let tag = unsafe { (*node.cast::<pg_sys::Node>()).type_ };
            if tag != pg_sys::NodeTag::T_Integer {
                pgrx::error!("pg_accel: non-Integer node at custom_private index {index}: {tag:?}");
            }
        }
        Self {
            source: IntListSource::PgList { list, len },
        }
    }

    #[must_use]
    pub(super) const fn len(&self) -> usize {
        match self.source {
            IntListSource::Slice(fields) => fields.len(),
            IntListSource::PgList { len, .. } => len,
        }
    }

    #[must_use]
    pub(super) fn contains_range(&self, start: usize, width: usize) -> bool {
        start
            .checked_add(width)
            .is_some_and(|end| end <= self.len())
    }

    #[must_use]
    pub(super) fn get(&self, index: usize) -> Option<c_int> {
        if index >= self.len() {
            return None;
        }
        Some(match self.source {
            IntListSource::Slice(fields) => fields[index],
            IntListSource::PgList { list, .. } => {
                // SAFETY: bounds checked above and caller guaranteed Integer nodes.
                let node = unsafe { pg_sys::list_nth(list, index as c_int) };
                // SAFETY: `from_pg_list` checked every node and PostgreSQL does
                // not mutate a plan's private list during one decode.
                unsafe { (*node.cast::<pg_sys::Integer>()).ival }
            }
        })
    }

    #[must_use]
    pub(super) fn int_at(&self, index: usize) -> c_int {
        self.get(index).unwrap_or_else(|| {
            pgrx::error!(
                "pg_accel: custom_private decoder read word {index} past exact length {}",
                self.len()
            )
        })
    }

    #[must_use]
    #[cfg(feature = "pg_test")]
    pub(super) fn cursor_at(&self, index: usize) -> IntListCursor<'_, 'a> {
        IntListCursor {
            reader: self,
            index,
        }
    }
}

/// Sequential reader for self-describing sections of an integer list.
#[cfg(feature = "pg_test")]
pub(super) struct IntListCursor<'reader, 'source> {
    reader: &'reader IntListReader<'source>,
    index: usize,
}

#[cfg(feature = "pg_test")]
impl IntListCursor<'_, '_> {
    #[must_use]
    pub(super) const fn position(&self) -> usize {
        self.index
    }

    pub(super) fn read_int(&mut self) -> c_int {
        let value = self.reader.int_at(self.index);
        self.index += 1;
        value
    }

    pub(super) fn read_usize(&mut self) -> usize {
        self.read_int() as usize
    }

    pub(super) fn read_u32(&mut self) -> u32 {
        self.read_int() as u32
    }

    pub(super) fn read_oid(&mut self) -> pg_sys::Oid {
        pg_sys::Oid::from(self.read_u32())
    }

    pub(super) fn read_i64_halves(&mut self) -> i64 {
        let hi = self.read_u32();
        let lo = self.read_u32();
        (((hi as u64) << 32) | lo as u64) as i64
    }
}

/// Builder for PostgreSQL `List *` integer layouts.
///
/// Construct this only while PostgreSQL's current memory context is valid.
pub(super) struct PgListWriter {
    list: *mut pg_sys::List,
}

impl PgListWriter {
    #[must_use]
    pub(super) const fn from_existing(list: *mut pg_sys::List) -> Self {
        Self { list }
    }

    #[must_use]
    pub(super) const fn into_list(self) -> *mut pg_sys::List {
        self.list
    }

    pub(super) fn push_int(&mut self, value: c_int) {
        // SAFETY: caller constructed this writer in a valid PG memory context.
        unsafe {
            self.list = pg_sys::lappend(self.list, pg_sys::makeInteger(value).cast());
        }
    }

    pub(super) fn push_bool(&mut self, value: bool) {
        self.push_int(c_int::from(value));
    }

    #[cfg(feature = "pg_test")]
    pub(super) fn push_len(&mut self, value: usize) {
        self.push_int(c_int::try_from(value).unwrap_or_else(|_| {
            pgrx::error!("pg_accel: private-data length {value} exceeds i32 wire capacity")
        }));
    }

    pub(super) fn push_u32(&mut self, value: u32) {
        self.push_int(value as c_int);
    }

    #[cfg(feature = "pg_test")]
    pub(super) fn push_oid(&mut self, value: pg_sys::Oid) {
        self.push_u32(u32::from(value));
    }

    #[cfg(feature = "pg_test")]
    pub(super) fn push_i64_halves(&mut self, value: i64) {
        let value = value as u64;
        self.push_int((value >> 32) as c_int);
        self.push_int(value as u32 as c_int);
    }
}
