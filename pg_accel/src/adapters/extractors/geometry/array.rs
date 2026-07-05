//! `geometry[]` ArrayType extraction.

use std::borrow::Cow;

use pgrx::pg_sys::Datum;

use crate::adapters::extractors::array::{ParseError as ArrayParseError, PgArray, parse_array};
use crate::gpu::three_layer::ExtractedGeometry;

use super::{extract_geometry_from_bytes, point::extract_point_from_bytes};

/// One slot from a PostgreSQL `geometry[]`.
///
/// SQL NULL array elements are preserved as [`ExtractedGeom::Null`] so
/// callers that return arrays can keep their output cardinality aligned
/// with the input.
#[derive(Debug, Clone)]
pub enum ExtractedGeom {
    Null,
    Geometry {
        geom: ExtractedGeometry,
        point_xy: Option<(f64, f64)>,
    },
}

impl ExtractedGeom {
    #[must_use]
    pub fn point_xy(&self) -> Option<(f64, f64)> {
        match self {
            Self::Geometry { point_xy, .. } => *point_xy,
            Self::Null => None,
        }
    }
}

/// Errors returned while extracting a `geometry[]`.
#[derive(Debug, PartialEq, Eq)]
pub enum ExtractError {
    Array(ArrayParseError),
    InvalidElement { index: usize },
}

impl From<ArrayParseError> for ExtractError {
    fn from(value: ArrayParseError) -> Self {
        Self::Array(value)
    }
}

impl core::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Array(e) => write!(f, "{e}"),
            Self::InvalidElement { index } => {
                write!(f, "geometry[] element {index} is not valid GSERIALIZED")
            }
        }
    }
}

impl std::error::Error for ExtractError {}

/// Extract a 1-D PostgreSQL `geometry[]` Datum into geometry slots.
///
/// The generic `ArrayType` walker handles dimensionality, null bitmap, and
/// element alignment. This layer normalizes each varlena geometry element
/// before reusing the scalar GSERIALIZED extractors.
pub fn extract_geometry_array(datum: Datum) -> Result<Vec<ExtractedGeom>, ExtractError> {
    // SAFETY: callers route only PostgreSQL `geometry[]` Datums here on the
    // main backend thread, matching the scalar geometry extractor contract.
    let arr = unsafe { parse_array(datum) }?;
    extract_geometry_array_from_pg_array(&arr)
}

pub(crate) fn extract_geometry_array_from_pg_array(
    arr: &PgArray<'_>,
) -> Result<Vec<ExtractedGeom>, ExtractError> {
    let mut out = Vec::with_capacity(arr.nelems);

    for (index, elem) in arr.iter().enumerate() {
        let Some(bytes) = elem else {
            out.push(ExtractedGeom::Null);
            continue;
        };

        let bytes = normalize_array_varlena(bytes).ok_or(ExtractError::InvalidElement { index })?;
        let geom = extract_geometry_from_bytes(bytes.as_ref())
            .ok_or(ExtractError::InvalidElement { index })?;
        let point_xy = extract_point_from_bytes(bytes.as_ref());

        out.push(ExtractedGeom::Geometry { geom, point_xy });
    }

    Ok(out)
}

fn normalize_array_varlena(bytes: &[u8]) -> Option<Cow<'_, [u8]>> {
    let first = *bytes.first()?;

    if first & 0x01 == 0 {
        return Some(Cow::Borrowed(bytes));
    }

    let short_size = (first as usize) >> 1;
    if short_size < 1 || short_size > bytes.len() {
        return None;
    }

    let payload = &bytes[1..short_size];
    let total_size = payload.len() + 4;
    let mut normalized = Vec::with_capacity(total_size);
    normalized.extend_from_slice(&((total_size as u32) << 2).to_le_bytes());
    normalized.extend_from_slice(payload);
    Some(Cow::Owned(normalized))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod unit_tests {
    use pgrx::pg_sys;

    use crate::adapters::extractors::geometry::header::WKB_POINT_TYPE;
    use crate::gpu::three_layer::GeomType;

    use super::*;

    fn make_gserialized_point(x: f64, y: f64) -> Vec<u8> {
        let mut buf = Vec::new();
        let total_size: u32 = 28;
        buf.extend_from_slice(&(total_size << 2).to_le_bytes());
        buf.extend_from_slice(&[0, 0, 0, 0]);
        buf.extend_from_slice(&WKB_POINT_TYPE.to_le_bytes());
        buf.extend_from_slice(&x.to_le_bytes());
        buf.extend_from_slice(&y.to_le_bytes());
        buf
    }

    fn short_varlena_from_4b(bytes: &[u8]) -> Vec<u8> {
        let payload = &bytes[4..];
        let short_size = payload.len() + 1;
        let mut out = Vec::with_capacity(short_size);
        out.push(((short_size as u8) << 1) | 0x01);
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn extracts_points_and_preserves_null_slots() {
        let p1 = make_gserialized_point(1.25, 2.5);
        let p2 = make_gserialized_point(3.5, 4.75);
        let mut payload = Vec::new();
        payload.extend_from_slice(&p1);
        payload.extend_from_slice(&p2);
        let nullmap = [0b0000_0101_u8];

        let arr = PgArray {
            elem_type: pg_sys::Oid::from(0_u32),
            elem_len: -1,
            elem_align: pg_sys::TYPALIGN_INT,
            nelems: 3,
            nullmap: Some(&nullmap),
            payload: &payload,
        };

        let geoms = extract_geometry_array_from_pg_array(&arr).unwrap();
        assert_eq!(geoms.len(), 3);
        assert_eq!(geoms[0].point_xy(), Some((1.25, 2.5)));
        assert!(matches!(geoms[1], ExtractedGeom::Null));
        assert_eq!(geoms[2].point_xy(), Some((3.5, 4.75)));
    }

    #[test]
    fn accepts_short_varlena_geometry_elements() {
        let point = make_gserialized_point(9.0, 10.0);
        let short = short_varlena_from_4b(&point);

        let arr = PgArray {
            elem_type: pg_sys::Oid::from(0_u32),
            elem_len: -1,
            elem_align: pg_sys::TYPALIGN_INT,
            nelems: 1,
            nullmap: None,
            payload: &short,
        };

        let geoms = extract_geometry_array_from_pg_array(&arr).unwrap();
        assert_eq!(geoms.len(), 1);
        assert_eq!(geoms[0].point_xy(), Some((9.0, 10.0)));
        match &geoms[0] {
            ExtractedGeom::Geometry { geom, .. } => assert_eq!(geom.geom_type, GeomType::Point),
            ExtractedGeom::Null => panic!("expected geometry"),
        }
    }
}

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::{Spi, pg_test};

    use crate::adapters::extractors::geometry::extract_geometry_array;

    fn ensure_extension(name: &str) -> bool {
        let create_sql = format!("CREATE EXTENSION IF NOT EXISTS {name} CASCADE");
        if Spi::run(&create_sql).is_err() {
            return false;
        }
        let q = format!("SELECT count(*) FROM pg_extension WHERE extname = '{name}'");
        Spi::get_one::<i64>(&q).ok().flatten().unwrap_or(0) > 0
    }

    #[pg_test]
    fn extract_geometry_array_from_postgis_points() {
        if !ensure_extension("postgis") {
            return;
        }

        let geoms = Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT ARRAY[ST_Point(1, 2), NULL::geometry, ST_Point(3, 4)]::geometry[]",
                    Some(1),
                    &[],
                )
                .expect("geometry[] select succeeds")
                .first();
            let datum = table
                .get_datum_by_ordinal(1)
                .expect("first column datum")
                .expect("array datum is not null");
            extract_geometry_array(datum).expect("geometry[] extracts")
        });

        assert_eq!(geoms.len(), 3);
        assert_eq!(geoms[0].point_xy(), Some((1.0, 2.0)));
        assert!(matches!(geoms[1], super::ExtractedGeom::Null));
        assert_eq!(geoms[2].point_xy(), Some((3.0, 4.0)));
    }
}
