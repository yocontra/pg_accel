//! Backend-local resident OLAP caches.
//!
//! Benchmark setup can explicitly load reusable resident OLAP source buffers,
//! then canonical SQL may select a CustomScan only while the matching columns
//! are present in backend-local GPU-readable memory.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

use pgrx::{pg_sys, prelude::*};

use crate::gpu::{
    self, ExprDeviceBuffer, ExprTemplateSsbmQ1Scratch, GpuHashTable, PgaccelExprUsmCol,
    PgaccelKeyType, PgaccelValTag,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SsbmQ1DatePredicate {
    Year(i32),
    YearMonthNum(i32),
    YearWeek { year: i32, week: i32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SsbmQ2Variant {
    Q2_1,
    Q2_2,
    Q2_3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SsbmQ3Variant {
    Q3_1,
    Q3_2,
    Q3_3,
    Q3_4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SsbmQ4Variant {
    Q4_1,
    Q4_2,
    Q4_3,
}

impl SsbmQ2Variant {
    #[must_use]
    pub const fn to_i32(self) -> i32 {
        match self {
            Self::Q2_1 => 1,
            Self::Q2_2 => 2,
            Self::Q2_3 => 3,
        }
    }

    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            1 => Some(Self::Q2_1),
            2 => Some(Self::Q2_2),
            3 => Some(Self::Q2_3),
            _ => None,
        }
    }

    const fn supplier_region(self) -> &'static str {
        match self {
            Self::Q2_1 => "AMERICA",
            Self::Q2_2 => "ASIA",
            Self::Q2_3 => "EUROPE",
        }
    }

    fn part_matches(self, row: &SsbmPartRow) -> bool {
        match self {
            Self::Q2_1 => row.category == "MFGR#12",
            Self::Q2_2 => row.brand1.as_str() >= "MFGR#2221" && row.brand1.as_str() <= "MFGR#2228",
            Self::Q2_3 => row.brand1 == "MFGR#2239",
        }
    }
}

impl SsbmQ3Variant {
    #[must_use]
    pub const fn to_i32(self) -> i32 {
        match self {
            Self::Q3_1 => 1,
            Self::Q3_2 => 2,
            Self::Q3_3 => 3,
            Self::Q3_4 => 4,
        }
    }

    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            1 => Some(Self::Q3_1),
            2 => Some(Self::Q3_2),
            3 => Some(Self::Q3_3),
            4 => Some(Self::Q3_4),
            _ => None,
        }
    }

    #[must_use]
    pub const fn uses_nation_labels(self) -> bool {
        matches!(self, Self::Q3_1)
    }

    fn date_matches(self, row: &SsbmDateRow) -> bool {
        match self {
            Self::Q3_1 | Self::Q3_2 | Self::Q3_3 => (1992..=1997).contains(&row.year),
            Self::Q3_4 => row.yearmonth == "Dec1997",
        }
    }

    fn customer_matches(self, row: &SsbmCustomerRow) -> bool {
        match self {
            Self::Q3_1 => row.region == "ASIA",
            Self::Q3_2 => row.nation == "UNITED STATES",
            Self::Q3_3 | Self::Q3_4 => row.city == "UNITED ST0" || row.city == "UNITED ST1",
        }
    }

    fn supplier_matches(self, row: &SsbmSupplierRow) -> bool {
        match self {
            Self::Q3_1 => row.region == "ASIA",
            Self::Q3_2 => row.nation == "UNITED STATES",
            Self::Q3_3 | Self::Q3_4 => row.city == "UNITED ST0" || row.city == "UNITED ST1",
        }
    }
}

impl SsbmQ4Variant {
    #[must_use]
    pub const fn to_i32(self) -> i32 {
        match self {
            Self::Q4_1 => 1,
            Self::Q4_2 => 2,
            Self::Q4_3 => 3,
        }
    }

    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            1 => Some(Self::Q4_1),
            2 => Some(Self::Q4_2),
            3 => Some(Self::Q4_3),
            _ => None,
        }
    }

    #[must_use]
    pub const fn group_geo_source(self) -> i32 {
        match self {
            Self::Q4_1 => 1,
            Self::Q4_2 | Self::Q4_3 => 2,
        }
    }

    #[must_use]
    pub const fn emits_part_label(self) -> bool {
        matches!(self, Self::Q4_2 | Self::Q4_3)
    }

    fn date_matches(self, row: &SsbmDateRow) -> bool {
        match self {
            Self::Q4_1 => true,
            Self::Q4_2 | Self::Q4_3 => row.year == 1997 || row.year == 1998,
        }
    }

    fn customer_matches(self, row: &SsbmCustomerRow) -> bool {
        row.region == "AMERICA"
    }

    fn supplier_matches(self, row: &SsbmSupplierRow) -> bool {
        match self {
            Self::Q4_1 | Self::Q4_2 => row.region == "AMERICA",
            Self::Q4_3 => row.nation == "UNITED STATES",
        }
    }

    fn part_matches(self, row: &SsbmPartRow) -> bool {
        match self {
            Self::Q4_1 | Self::Q4_2 => row.mfgr == "MFGR#1" || row.mfgr == "MFGR#2",
            Self::Q4_3 => row.category == "MFGR#14",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SsbmDateRow {
    datekey: i32,
    year: i32,
    yearmonth: String,
    yearmonthnum: i32,
    weeknuminyear: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SsbmPartRow {
    partkey: i32,
    mfgr: String,
    category: String,
    brand1: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SsbmSupplierRow {
    suppkey: i32,
    city: String,
    nation: String,
    region: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SsbmCustomerRow {
    custkey: i32,
    city: String,
    nation: String,
    region: String,
}

pub struct SsbmQ1ResidentCache {
    fact_rel_oid: pg_sys::Oid,
    date_rel_oid: pg_sys::Oid,
    part_rel_oid: pg_sys::Oid,
    customer_rel_oid: pg_sys::Oid,
    supplier_rel_oid: pg_sys::Oid,
    row_count: usize,
    orderdate: ExprDeviceBuffer<i32>,
    discount: ExprDeviceBuffer<i32>,
    quantity: ExprDeviceBuffer<i32>,
    extendedprice: ExprDeviceBuffer<i32>,
    partkey: ExprDeviceBuffer<i32>,
    custkey: ExprDeviceBuffer<i32>,
    suppkey: ExprDeviceBuffer<i32>,
    revenue: ExprDeviceBuffer<i32>,
    supplycost: ExprDeviceBuffer<i32>,
    scratch: SsbmQ1ScratchBuffers,
    date_rows: Vec<SsbmDateRow>,
    date_key_min: i32,
    date_year_by_offset: ExprDeviceBuffer<i32>,
    year_min: i32,
    year_count: i32,
    part_rows: Vec<SsbmPartRow>,
    customer_rows: Vec<SsbmCustomerRow>,
    supplier_rows: Vec<SsbmSupplierRow>,
    brand_values: Vec<String>,
    part_brand_code_by_key: ExprDeviceBuffer<i32>,
    customer_key_count: usize,
    supplier_key_count: usize,
    date_filter_cache: RefCell<Vec<SsbmQ1CachedDateFilter>>,
    q2_filter_cache: RefCell<Vec<SsbmQ2CachedFilter>>,
    q3_filter_cache: RefCell<Vec<SsbmQ3CachedFilter>>,
    q4_filter_cache: RefCell<Vec<SsbmQ4CachedFilter>>,
    q4_scratch_profit_lo: ExprDeviceBuffer<u32>,
    q4_scratch_profit_hi: ExprDeviceBuffer<u32>,
    q4_scratch_count: ExprDeviceBuffer<u32>,
    q4_scratch_group_capacity: usize,
}

struct SsbmQ1ScratchBuffers {
    item_capacity: usize,
    revenue_a: ExprDeviceBuffer<i64>,
    count_a: ExprDeviceBuffer<i64>,
    revenue_b: ExprDeviceBuffer<i64>,
    count_b: ExprDeviceBuffer<i64>,
}

pub struct ResidentDenseGroupAggCache {
    rel_oid: pg_sys::Oid,
    group_attno: i32,
    value_attno: i32,
    value_rhs_attno: Option<i32>,
    filter_attno: Option<i32>,
    row_count: usize,
    group_min: i32,
    group_count: i32,
    group_output: ResidentGroupKeyOutput,
    group_keys: ExprDeviceBuffer<i32>,
    measure_op: ResidentMeasureOp,
    values: ExprDeviceBuffer<f64>,
    value_nulls: Option<ExprDeviceBuffer<u8>>,
    value_rhs: Option<ExprDeviceBuffer<f64>>,
    value_rhs_nulls: Option<ExprDeviceBuffer<u8>>,
    filter: Option<ExprDeviceBuffer<u8>>,
    filtered_row_count: usize,
    filtered_group_keys: Option<ExprDeviceBuffer<i32>>,
    filtered_values: Option<ExprDeviceBuffer<f64>>,
    filtered_value_nulls: Option<ExprDeviceBuffer<u8>>,
    filtered_value_rhs: Option<ExprDeviceBuffer<f64>>,
    filtered_value_rhs_nulls: Option<ExprDeviceBuffer<u8>>,
    scratch_sum: ExprDeviceBuffer<f64>,
    scratch_min: ExprDeviceBuffer<f64>,
    scratch_max: ExprDeviceBuffer<f64>,
    scratch_count: ExprDeviceBuffer<u32>,
    scratch_group_start: ExprDeviceBuffer<u32>,
    scratch_group_cursor: ExprDeviceBuffer<u32>,
    scratch_sorted_group: ExprDeviceBuffer<i32>,
    scratch_row_index: ExprDeviceBuffer<u32>,
    scratch_partial_sum: Option<ExprDeviceBuffer<f64>>,
    scratch_partial_min: Option<ExprDeviceBuffer<f64>>,
    scratch_partial_max: Option<ExprDeviceBuffer<f64>>,
    scratch_partial_count: Option<ExprDeviceBuffer<u32>>,
}

pub struct ResidentStarDimGroupAggCache {
    fact_rel_oid: pg_sys::Oid,
    dim_rel_oid: pg_sys::Oid,
    fact_key_attno: i32,
    fact_value_attno: i32,
    dim_key_attno: i32,
    dim_group_attno: i32,
    dim_filter_attno: i32,
    fact_value_cmp_opcode: u16,
    fact_value_cmp_const: f64,
    dim_filter_cmp_opcode: u16,
    dim_filter_cmp_const: f64,
    row_count: usize,
    group_count: i32,
    group_output: ResidentGroupKeyOutput,
    fact_keys: ExprDeviceBuffer<i32>,
    fact_key_nulls: Option<ExprDeviceBuffer<u8>>,
    values: ExprDeviceBuffer<f64>,
    value_nulls: Option<ExprDeviceBuffer<u8>>,
    dim_match_by_key: ExprDeviceBuffer<u8>,
    dim_group_code_by_key: ExprDeviceBuffer<i32>,
    dim_key_count: usize,
    projected_group_keys: ExprDeviceBuffer<i32>,
    projected_values: ExprDeviceBuffer<f64>,
    scratch_sum: ExprDeviceBuffer<f64>,
    scratch_min: ExprDeviceBuffer<f64>,
    scratch_max: ExprDeviceBuffer<f64>,
    scratch_count: ExprDeviceBuffer<u32>,
    scratch_group_start: ExprDeviceBuffer<u32>,
    scratch_group_cursor: ExprDeviceBuffer<u32>,
    scratch_sorted_group: ExprDeviceBuffer<i32>,
    scratch_row_index: ExprDeviceBuffer<u32>,
    scratch_partial_sum: Option<ExprDeviceBuffer<f64>>,
    scratch_partial_min: Option<ExprDeviceBuffer<f64>>,
    scratch_partial_max: Option<ExprDeviceBuffer<f64>>,
    scratch_partial_count: Option<ExprDeviceBuffer<u32>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidentDenseGroupAggCacheShape {
    pub rel_oid: pg_sys::Oid,
    pub group_attno: i32,
    pub value_attno: i32,
    pub value_rhs_attno: Option<i32>,
    pub filter_attno: Option<i32>,
    pub measure_op: ResidentMeasureOp,
}

impl ResidentDenseGroupAggCacheShape {
    #[must_use]
    pub const fn requires_rhs(self) -> bool {
        self.value_rhs_attno.is_some()
    }

    #[must_use]
    pub const fn has_filter(self) -> bool {
        self.filter_attno.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResidentStarDimGroupAggCacheShape {
    pub fact_rel_oid: pg_sys::Oid,
    pub dim_rel_oid: pg_sys::Oid,
    pub fact_key_attno: i32,
    pub fact_value_attno: i32,
    pub dim_key_attno: i32,
    pub dim_group_attno: i32,
    pub dim_filter_attno: i32,
    pub fact_value_cmp_opcode: u16,
    pub fact_value_cmp_const: f64,
    pub dim_filter_cmp_opcode: u16,
    pub dim_filter_cmp_const: f64,
}

impl Eq for ResidentStarDimGroupAggCacheShape {}

pub struct ResidentHashJoinCountCache {
    outer_rel_oid: pg_sys::Oid,
    inner_rel_oid: pg_sys::Oid,
    outer_attno: i32,
    inner_attno: i32,
    key_type: PgaccelKeyType,
    outer_rows: usize,
    inner_rows: usize,
    outer_i32: Option<ExprDeviceBuffer<i32>>,
    inner_i32: Option<ExprDeviceBuffer<i32>>,
    outer_i64: Option<ExprDeviceBuffer<i64>>,
    inner_i64: Option<ExprDeviceBuffer<i64>>,
    outer_nulls: Option<ExprDeviceBuffer<u8>>,
    inner_nulls: Option<ExprDeviceBuffer<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidentHashJoinCountCacheShape {
    pub outer_rel_oid: pg_sys::Oid,
    pub inner_rel_oid: pg_sys::Oid,
    pub outer_attno: i32,
    pub inner_attno: i32,
    pub key_type: PgaccelKeyType,
    pub outer_rows: usize,
    pub inner_rows: usize,
}

impl ResidentHashJoinCountCache {
    #[must_use]
    pub const fn shape(&self) -> ResidentHashJoinCountCacheShape {
        ResidentHashJoinCountCacheShape {
            outer_rel_oid: self.outer_rel_oid,
            inner_rel_oid: self.inner_rel_oid,
            outer_attno: self.outer_attno,
            inner_attno: self.inner_attno,
            key_type: self.key_type,
            outer_rows: self.outer_rows,
            inner_rows: self.inner_rows,
        }
    }

    #[must_use]
    pub fn matches(
        &self,
        outer_rel_oid: pg_sys::Oid,
        outer_attno: i32,
        inner_rel_oid: pg_sys::Oid,
        inner_attno: i32,
        key_type: PgaccelKeyType,
    ) -> bool {
        self.outer_rel_oid == outer_rel_oid
            && self.outer_attno == outer_attno
            && self.inner_rel_oid == inner_rel_oid
            && self.inner_attno == inner_attno
            && self.key_type == key_type
    }

    fn outer_keys_ptr(&self) -> *const std::ffi::c_void {
        match self.key_type {
            PgaccelKeyType::Int32 => self
                .outer_i32
                .as_ref()
                .map_or(std::ptr::null(), ExprDeviceBuffer::as_ptr)
                .cast(),
            PgaccelKeyType::Int64 => self
                .outer_i64
                .as_ref()
                .map_or(std::ptr::null(), ExprDeviceBuffer::as_ptr)
                .cast(),
            PgaccelKeyType::Float64 | PgaccelKeyType::Uuid | PgaccelKeyType::Inet => {
                std::ptr::null()
            }
        }
    }

    fn inner_keys_ptr(&self) -> *const std::ffi::c_void {
        match self.key_type {
            PgaccelKeyType::Int32 => self
                .inner_i32
                .as_ref()
                .map_or(std::ptr::null(), ExprDeviceBuffer::as_ptr)
                .cast(),
            PgaccelKeyType::Int64 => self
                .inner_i64
                .as_ref()
                .map_or(std::ptr::null(), ExprDeviceBuffer::as_ptr)
                .cast(),
            PgaccelKeyType::Float64 | PgaccelKeyType::Uuid | PgaccelKeyType::Inet => {
                std::ptr::null()
            }
        }
    }

    fn outer_nulls_ptr(&self) -> *const u8 {
        self.outer_nulls
            .as_ref()
            .map_or(std::ptr::null(), ExprDeviceBuffer::as_ptr)
    }

    fn inner_nulls_ptr(&self) -> *const u8 {
        self.inner_nulls
            .as_ref()
            .map_or(std::ptr::null(), ExprDeviceBuffer::as_ptr)
    }

    pub fn count_matches(&self) -> Option<usize> {
        let table = GpuHashTable::build_device_count(
            self.inner_keys_ptr(),
            self.inner_nulls_ptr(),
            self.inner_rows,
            self.key_type,
        )?;
        table.count_matches_device(
            self.outer_keys_ptr(),
            self.outer_nulls_ptr(),
            self.outer_rows,
        )
    }
}

pub struct ResidentH3GroupAggCache {
    rel_oid: pg_sys::Oid,
    row_count: usize,
    kind: ResidentH3GroupedCountKind,
    input: ResidentH3GroupAggInput,
}

enum ResidentH3GroupAggInput {
    LatLngCells {
        resolution: i32,
        cells: ExprDeviceBuffer<i64>,
    },
    Cell {
        cells: ExprDeviceBuffer<u64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentH3GroupedCountKind {
    LatLngToCell,
    CellToParent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentMeasureOp {
    Column,
    Mul,
    Sub,
    StatsPair,
}

impl ResidentMeasureOp {
    #[must_use]
    pub const fn to_i32(self) -> i32 {
        match self {
            Self::Column => 0,
            Self::Mul => 1,
            Self::Sub => 2,
            Self::StatsPair => 3,
        }
    }

    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Column),
            1 => Some(Self::Mul),
            2 => Some(Self::Sub),
            3 => Some(Self::StatsPair),
            _ => None,
        }
    }
}

impl ResidentH3GroupedCountKind {
    #[must_use]
    pub const fn to_i32(self) -> i32 {
        match self {
            Self::LatLngToCell => 1,
            Self::CellToParent => 2,
        }
    }

    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            1 => Some(Self::LatLngToCell),
            2 => Some(Self::CellToParent),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResidentGroupKeyOutput {
    Int4Dense { min: i32 },
    Int4Dictionary { keys: Vec<i32> },
    TextDictionary { labels: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResidentGroupKeySource {
    Int4,
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResidentGroupKeyValue {
    Int4(i32),
    Text(String),
}

struct LoadedResidentGroupKeys {
    codes: Vec<i32>,
    min: i32,
    count: i32,
    output: ResidentGroupKeyOutput,
}

pub struct SsbmQ1ResolvedDateFilter {
    pub orderdate_lo: i32,
    pub orderdate_hi: i32,
    pub orderdate_keys: *const i32,
    pub orderdate_key_count: usize,
}

pub struct SsbmQ2ResolvedFilter {
    pub part_match_by_key: *const u8,
    pub part_key_count: usize,
    pub supplier_match_by_key: *const u8,
    pub supplier_key_count: usize,
}

pub struct SsbmQ3ResolvedFilter<'a> {
    pub date_match_by_offset: *const u8,
    pub date_year_count: usize,
    pub customer_group_code_by_key: *const i32,
    pub customer_match_by_key: *const u8,
    pub customer_key_count: usize,
    pub supplier_group_code_by_key: *const i32,
    pub supplier_match_by_key: *const u8,
    pub supplier_key_count: usize,
    pub customer_labels: &'a [String],
    pub supplier_labels: &'a [String],
}

pub struct SsbmQ4ResolvedFilter<'a> {
    pub date_match_by_offset: *const u8,
    pub date_year_count: usize,
    pub customer_group_code_by_key: *const i32,
    pub customer_match_by_key: *const u8,
    pub customer_key_count: usize,
    pub supplier_group_code_by_key: *const i32,
    pub supplier_match_by_key: *const u8,
    pub supplier_key_count: usize,
    pub part_group_code_by_key: *const i32,
    pub part_match_by_key: *const u8,
    pub part_key_count: usize,
    pub group_geo_source: i32,
    pub geo_labels: &'a [String],
    pub part_labels: &'a [String],
}

struct SsbmQ1CachedDateFilter {
    predicate: SsbmQ1DatePredicate,
    orderdate_lo: i32,
    orderdate_hi: i32,
    key_buffer: Option<ExprDeviceBuffer<i32>>,
    key_count: usize,
}

struct SsbmQ2CachedFilter {
    variant: SsbmQ2Variant,
    part: ResidentStarDimensionFilter,
    supplier: ResidentStarDimensionFilter,
}

struct SsbmQ3CachedFilter {
    variant: SsbmQ3Variant,
    date_match_by_offset: ExprDeviceBuffer<u8>,
    customer: ResidentStarDimensionFilter,
    supplier: ResidentStarDimensionFilter,
}

struct SsbmQ4CachedFilter {
    variant: SsbmQ4Variant,
    date_match_by_offset: ExprDeviceBuffer<u8>,
    customer: ResidentStarDimensionFilter,
    supplier: ResidentStarDimensionFilter,
    part: ResidentStarDimensionFilter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResidentStarDimensionHostFilter {
    match_by_key: Vec<u8>,
    group_code_by_key: Option<Vec<i32>>,
    labels: Vec<String>,
}

struct ResidentStarDimensionFilter {
    match_by_key: ExprDeviceBuffer<u8>,
    group_code_by_key: Option<ExprDeviceBuffer<i32>>,
    key_count: usize,
    labels: Vec<String>,
}

impl SsbmQ1ResidentCache {
    #[must_use]
    pub const fn row_count(&self) -> usize {
        self.row_count
    }

    #[must_use]
    pub const fn fact_rel_oid(&self) -> pg_sys::Oid {
        self.fact_rel_oid
    }

    #[must_use]
    pub const fn date_rel_oid(&self) -> pg_sys::Oid {
        self.date_rel_oid
    }

    #[must_use]
    pub const fn part_rel_oid(&self) -> pg_sys::Oid {
        self.part_rel_oid
    }

    #[must_use]
    pub const fn customer_rel_oid(&self) -> pg_sys::Oid {
        self.customer_rel_oid
    }

    #[must_use]
    pub const fn supplier_rel_oid(&self) -> pg_sys::Oid {
        self.supplier_rel_oid
    }

    #[must_use]
    pub fn orderdate_col(&self) -> PgaccelExprUsmCol {
        gpu::expr_device_col(&self.orderdate, PgaccelValTag::Int32)
    }

    #[must_use]
    pub fn discount_col(&self) -> PgaccelExprUsmCol {
        gpu::expr_device_col(&self.discount, PgaccelValTag::Int32)
    }

    #[must_use]
    pub fn quantity_col(&self) -> PgaccelExprUsmCol {
        gpu::expr_device_col(&self.quantity, PgaccelValTag::Int32)
    }

    #[must_use]
    pub fn extendedprice_col(&self) -> PgaccelExprUsmCol {
        gpu::expr_device_col(&self.extendedprice, PgaccelValTag::Int32)
    }

    #[must_use]
    pub fn partkey_col(&self) -> PgaccelExprUsmCol {
        gpu::expr_device_col(&self.partkey, PgaccelValTag::Int32)
    }

    #[must_use]
    pub fn custkey_col(&self) -> PgaccelExprUsmCol {
        gpu::expr_device_col(&self.custkey, PgaccelValTag::Int32)
    }

    #[must_use]
    pub fn suppkey_col(&self) -> PgaccelExprUsmCol {
        gpu::expr_device_col(&self.suppkey, PgaccelValTag::Int32)
    }

    #[must_use]
    pub fn revenue_col(&self) -> PgaccelExprUsmCol {
        gpu::expr_device_col(&self.revenue, PgaccelValTag::Int32)
    }

    #[must_use]
    pub fn supplycost_col(&self) -> PgaccelExprUsmCol {
        gpu::expr_device_col(&self.supplycost, PgaccelValTag::Int32)
    }

    #[must_use]
    pub fn q4_scratch_profit_lo_ptr(&self) -> *mut u32 {
        self.q4_scratch_profit_lo.as_mut_ptr()
    }

    #[must_use]
    pub fn q4_scratch_profit_hi_ptr(&self) -> *mut u32 {
        self.q4_scratch_profit_hi.as_mut_ptr()
    }

    #[must_use]
    pub fn q4_scratch_count_ptr(&self) -> *mut u32 {
        self.q4_scratch_count.as_mut_ptr()
    }

    #[must_use]
    pub const fn q4_scratch_group_capacity(&self) -> usize {
        self.q4_scratch_group_capacity
    }

    #[must_use]
    pub const fn date_key_min(&self) -> i32 {
        self.date_key_min
    }

    #[must_use]
    pub fn date_year_by_offset_ptr(&self) -> *const i32 {
        self.date_year_by_offset.as_ptr()
    }

    #[must_use]
    pub fn date_year_count(&self) -> usize {
        self.date_year_by_offset.len()
    }

    #[must_use]
    pub fn part_brand_code_by_key_ptr(&self) -> *const i32 {
        self.part_brand_code_by_key.as_ptr()
    }

    #[must_use]
    pub fn part_key_count(&self) -> usize {
        self.part_brand_code_by_key.len()
    }

    #[must_use]
    pub const fn year_min(&self) -> i32 {
        self.year_min
    }

    #[must_use]
    pub const fn year_count(&self) -> i32 {
        self.year_count
    }

    #[must_use]
    pub fn brand_count(&self) -> i32 {
        i32::try_from(self.brand_values.len()).unwrap_or_else(|_| {
            pgrx::error!(
                "pg_accel: SSBM brand dictionary has {} entries, exceeding the int32 kernel \
                 ABI; refusing to dispatch with a clamped brand count",
                self.brand_values.len()
            )
        })
    }

    #[must_use]
    pub fn brand_at(&self, code: usize) -> Option<&str> {
        self.brand_values.get(code).map(String::as_str)
    }

    #[must_use]
    pub fn scratch(&self) -> ExprTemplateSsbmQ1Scratch<'_> {
        self.scratch.as_template_scratch()
    }

    #[must_use]
    pub fn date_keys(&self, predicate: SsbmQ1DatePredicate) -> Vec<i32> {
        date_keys_from_rows(&self.date_rows, predicate)
    }

    pub fn with_date_filter<R>(
        &self,
        predicate: SsbmQ1DatePredicate,
        f: impl FnOnce(SsbmQ1ResolvedDateFilter) -> R,
    ) -> Result<R, String> {
        let mut filters = self.date_filter_cache.borrow_mut();
        let idx = if let Some(idx) = filters
            .iter()
            .position(|filter| filter.predicate == predicate)
        {
            idx
        } else {
            let keys = self.date_keys(predicate);
            filters.push(SsbmQ1CachedDateFilter::from_keys(predicate, keys)?);
            filters.len() - 1
        };
        Ok(f(filters[idx].resolved()))
    }

    pub fn with_q2_filter<R>(
        &self,
        variant: SsbmQ2Variant,
        f: impl FnOnce(SsbmQ2ResolvedFilter) -> R,
    ) -> Result<R, String> {
        let mut filters = self.q2_filter_cache.borrow_mut();
        let idx = if let Some(idx) = filters.iter().position(|filter| filter.variant == variant) {
            idx
        } else {
            filters.push(SsbmQ2CachedFilter::from_rows(
                variant,
                &self.part_rows,
                self.part_key_count(),
                &self.supplier_rows,
                self.supplier_key_count,
            )?);
            filters.len() - 1
        };
        Ok(f(filters[idx].resolved()))
    }

    pub fn with_q3_filter<R>(
        &self,
        variant: SsbmQ3Variant,
        f: impl FnOnce(SsbmQ3ResolvedFilter<'_>) -> R,
    ) -> Result<R, String> {
        let mut filters = self.q3_filter_cache.borrow_mut();
        let idx = if let Some(idx) = filters.iter().position(|filter| filter.variant == variant) {
            idx
        } else {
            filters.push(SsbmQ3CachedFilter::from_rows(
                variant,
                &self.date_rows,
                self.date_key_min,
                self.date_year_by_offset.len(),
                &self.customer_rows,
                self.customer_key_count,
                &self.supplier_rows,
                self.supplier_key_count,
            )?);
            filters.len() - 1
        };
        Ok(f(filters[idx].resolved()))
    }

    pub fn with_q4_filter<R>(
        &self,
        variant: SsbmQ4Variant,
        f: impl FnOnce(SsbmQ4ResolvedFilter<'_>) -> R,
    ) -> Result<R, String> {
        let mut filters = self.q4_filter_cache.borrow_mut();
        let idx = if let Some(idx) = filters.iter().position(|filter| filter.variant == variant) {
            idx
        } else {
            filters.push(SsbmQ4CachedFilter::from_rows(
                variant,
                &self.date_rows,
                self.date_key_min,
                self.date_year_by_offset.len(),
                &self.customer_rows,
                self.customer_key_count,
                &self.supplier_rows,
                self.supplier_key_count,
                &self.part_rows,
                self.part_key_count(),
            )?);
            filters.len() - 1
        };
        Ok(f(filters[idx].resolved()))
    }
}

impl SsbmQ1ScratchBuffers {
    fn new(row_count: usize) -> Result<Self, String> {
        let item_capacity = gpu::expr_template_ssbm_q1_scratch_items(row_count);
        if item_capacity == 0 {
            return Err("SSBM Q1 scratch sizing returned zero for non-empty cache".to_owned());
        }

        Ok(Self {
            item_capacity,
            revenue_a: alloc_device_i64(item_capacity, "ssbm_q1_revenue_a_scratch")?,
            count_a: alloc_device_i64(item_capacity, "ssbm_q1_count_a_scratch")?,
            revenue_b: alloc_device_i64(item_capacity, "ssbm_q1_revenue_b_scratch")?,
            count_b: alloc_device_i64(item_capacity, "ssbm_q1_count_b_scratch")?,
        })
    }

    fn as_template_scratch(&self) -> ExprTemplateSsbmQ1Scratch<'_> {
        debug_assert_eq!(self.revenue_a.len(), self.item_capacity);
        debug_assert_eq!(self.count_a.len(), self.item_capacity);
        debug_assert_eq!(self.revenue_b.len(), self.item_capacity);
        debug_assert_eq!(self.count_b.len(), self.item_capacity);
        ExprTemplateSsbmQ1Scratch {
            revenue_a: &self.revenue_a,
            count_a: &self.count_a,
            revenue_b: &self.revenue_b,
            count_b: &self.count_b,
        }
    }
}

impl ResidentDenseGroupAggCache {
    #[must_use]
    pub const fn rel_oid(&self) -> pg_sys::Oid {
        self.rel_oid
    }

    #[must_use]
    pub const fn shape(&self) -> ResidentDenseGroupAggCacheShape {
        ResidentDenseGroupAggCacheShape {
            rel_oid: self.rel_oid,
            group_attno: self.group_attno,
            value_attno: self.value_attno,
            value_rhs_attno: self.value_rhs_attno,
            filter_attno: self.filter_attno,
            measure_op: self.measure_op,
        }
    }

    #[must_use]
    pub const fn row_count(&self) -> usize {
        self.row_count
    }

    #[must_use]
    pub const fn group_min(&self) -> i32 {
        self.group_min
    }

    #[must_use]
    pub const fn group_count(&self) -> i32 {
        self.group_count
    }

    #[must_use]
    pub const fn group_output(&self) -> &ResidentGroupKeyOutput {
        &self.group_output
    }

    #[must_use]
    pub fn group_col(&self) -> PgaccelExprUsmCol {
        gpu::expr_device_col(&self.group_keys, PgaccelValTag::Int32)
    }

    #[must_use]
    pub fn value_col(&self) -> PgaccelExprUsmCol {
        gpu::expr_device_col_with_nulls(
            &self.values,
            self.value_nulls.as_ref(),
            PgaccelValTag::Float64,
        )
    }

    #[must_use]
    pub fn value_rhs_col(&self) -> Option<PgaccelExprUsmCol> {
        self.value_rhs.as_ref().map(|values| {
            gpu::expr_device_col_with_nulls(
                values,
                self.value_rhs_nulls.as_ref(),
                PgaccelValTag::Float64,
            )
        })
    }

    #[must_use]
    pub const fn measure_op(&self) -> ResidentMeasureOp {
        self.measure_op
    }

    #[must_use]
    pub fn filter_col(&self) -> Option<PgaccelExprUsmCol> {
        self.filter
            .as_ref()
            .map(|filter| gpu::expr_device_col(filter, PgaccelValTag::Bool))
    }

    #[must_use]
    pub const fn filtered_row_count(&self) -> usize {
        self.filtered_row_count
    }

    #[must_use]
    pub fn filtered_group_col(&self) -> Option<PgaccelExprUsmCol> {
        self.filtered_group_keys
            .as_ref()
            .map(|groups| gpu::expr_device_col(groups, PgaccelValTag::Int32))
    }

    #[must_use]
    pub fn filtered_value_col(&self) -> Option<PgaccelExprUsmCol> {
        self.filtered_values.as_ref().map(|values| {
            gpu::expr_device_col_with_nulls(
                values,
                self.filtered_value_nulls.as_ref(),
                PgaccelValTag::Float64,
            )
        })
    }

    #[must_use]
    pub fn filtered_value_rhs_col(&self) -> Option<PgaccelExprUsmCol> {
        self.filtered_value_rhs.as_ref().map(|values| {
            gpu::expr_device_col_with_nulls(
                values,
                self.filtered_value_rhs_nulls.as_ref(),
                PgaccelValTag::Float64,
            )
        })
    }

    #[must_use]
    pub fn scratch_sum_ptr(&self) -> *mut f64 {
        self.scratch_sum.as_mut_ptr()
    }

    #[must_use]
    pub fn scratch_min_ptr(&self) -> *mut f64 {
        self.scratch_min.as_mut_ptr()
    }

    #[must_use]
    pub fn scratch_max_ptr(&self) -> *mut f64 {
        self.scratch_max.as_mut_ptr()
    }

    #[must_use]
    pub fn scratch_count_ptr(&self) -> *mut u32 {
        self.scratch_count.as_mut_ptr()
    }

    #[must_use]
    pub fn scratch_group_start_ptr(&self) -> *mut u32 {
        self.scratch_group_start.as_mut_ptr()
    }

    #[must_use]
    pub fn scratch_group_cursor_ptr(&self) -> *mut u32 {
        self.scratch_group_cursor.as_mut_ptr()
    }

    #[must_use]
    pub fn scratch_group_capacity(&self) -> usize {
        self.scratch_sum
            .len()
            .min(self.scratch_min.len())
            .min(self.scratch_max.len())
            .min(self.scratch_count.len())
            .min(self.scratch_group_start.len())
            .min(self.scratch_group_cursor.len())
    }

    #[must_use]
    pub fn scratch_sorted_group_ptr(&self) -> *mut i32 {
        self.scratch_sorted_group.as_mut_ptr()
    }

    #[must_use]
    pub fn scratch_row_index_ptr(&self) -> *mut u32 {
        self.scratch_row_index.as_mut_ptr()
    }

    #[must_use]
    pub fn scratch_row_capacity(&self) -> usize {
        self.scratch_sorted_group
            .len()
            .min(self.scratch_row_index.len())
    }

    #[must_use]
    pub fn scratch_partial_sum_ptr(&self) -> *mut f64 {
        self.scratch_partial_sum
            .as_ref()
            .map_or(std::ptr::null_mut(), ExprDeviceBuffer::as_mut_ptr)
    }

    #[must_use]
    pub fn scratch_partial_min_ptr(&self) -> *mut f64 {
        self.scratch_partial_min
            .as_ref()
            .map_or(std::ptr::null_mut(), ExprDeviceBuffer::as_mut_ptr)
    }

    #[must_use]
    pub fn scratch_partial_max_ptr(&self) -> *mut f64 {
        self.scratch_partial_max
            .as_ref()
            .map_or(std::ptr::null_mut(), ExprDeviceBuffer::as_mut_ptr)
    }

    #[must_use]
    pub fn scratch_partial_count_ptr(&self) -> *mut u32 {
        self.scratch_partial_count
            .as_ref()
            .map_or(std::ptr::null_mut(), ExprDeviceBuffer::as_mut_ptr)
    }

    #[must_use]
    pub fn scratch_partial_capacity(&self) -> usize {
        match (
            self.scratch_partial_sum.as_ref(),
            self.scratch_partial_min.as_ref(),
            self.scratch_partial_max.as_ref(),
            self.scratch_partial_count.as_ref(),
        ) {
            (Some(sum), Some(min), Some(max), Some(count)) => {
                sum.len().min(min.len()).min(max.len()).min(count.len())
            }
            _ => 0,
        }
    }
}

impl ResidentStarDimGroupAggCache {
    #[must_use]
    pub const fn shape(&self) -> ResidentStarDimGroupAggCacheShape {
        ResidentStarDimGroupAggCacheShape {
            fact_rel_oid: self.fact_rel_oid,
            dim_rel_oid: self.dim_rel_oid,
            fact_key_attno: self.fact_key_attno,
            fact_value_attno: self.fact_value_attno,
            dim_key_attno: self.dim_key_attno,
            dim_group_attno: self.dim_group_attno,
            dim_filter_attno: self.dim_filter_attno,
            fact_value_cmp_opcode: self.fact_value_cmp_opcode,
            fact_value_cmp_const: self.fact_value_cmp_const,
            dim_filter_cmp_opcode: self.dim_filter_cmp_opcode,
            dim_filter_cmp_const: self.dim_filter_cmp_const,
        }
    }

    #[must_use]
    pub const fn row_count(&self) -> usize {
        self.row_count
    }

    #[must_use]
    pub const fn group_count(&self) -> i32 {
        self.group_count
    }

    #[must_use]
    pub const fn group_min(&self) -> i32 {
        0
    }

    #[must_use]
    pub const fn group_output(&self) -> &ResidentGroupKeyOutput {
        &self.group_output
    }

    #[must_use]
    pub fn fact_key_col(&self) -> PgaccelExprUsmCol {
        gpu::expr_device_col_with_nulls(
            &self.fact_keys,
            self.fact_key_nulls.as_ref(),
            PgaccelValTag::Int32,
        )
    }

    #[must_use]
    pub fn value_col(&self) -> PgaccelExprUsmCol {
        gpu::expr_device_col_with_nulls(
            &self.values,
            self.value_nulls.as_ref(),
            PgaccelValTag::Float64,
        )
    }

    #[must_use]
    pub fn projected_group_col(&self) -> PgaccelExprUsmCol {
        gpu::expr_device_col(&self.projected_group_keys, PgaccelValTag::Int32)
    }

    #[must_use]
    pub fn projected_value_col(&self) -> PgaccelExprUsmCol {
        gpu::expr_device_col(&self.projected_values, PgaccelValTag::Float64)
    }

    #[must_use]
    pub fn dim_match_ptr(&self) -> *const u8 {
        self.dim_match_by_key.as_ptr()
    }

    #[must_use]
    pub fn dim_group_code_ptr(&self) -> *const i32 {
        self.dim_group_code_by_key.as_ptr()
    }

    #[must_use]
    pub const fn dim_key_count(&self) -> usize {
        self.dim_key_count
    }

    #[must_use]
    pub fn projected_group_ptr(&self) -> *mut i32 {
        self.projected_group_keys.as_mut_ptr()
    }

    #[must_use]
    pub fn projected_value_ptr(&self) -> *mut f64 {
        self.projected_values.as_mut_ptr()
    }

    #[must_use]
    pub fn projected_group_capacity(&self) -> usize {
        self.projected_group_keys
            .len()
            .min(self.projected_values.len())
    }

    #[must_use]
    pub fn scratch_sum_ptr(&self) -> *mut f64 {
        self.scratch_sum.as_mut_ptr()
    }

    #[must_use]
    pub fn scratch_min_ptr(&self) -> *mut f64 {
        self.scratch_min.as_mut_ptr()
    }

    #[must_use]
    pub fn scratch_max_ptr(&self) -> *mut f64 {
        self.scratch_max.as_mut_ptr()
    }

    #[must_use]
    pub fn scratch_count_ptr(&self) -> *mut u32 {
        self.scratch_count.as_mut_ptr()
    }

    #[must_use]
    pub fn scratch_group_start_ptr(&self) -> *mut u32 {
        self.scratch_group_start.as_mut_ptr()
    }

    #[must_use]
    pub fn scratch_group_cursor_ptr(&self) -> *mut u32 {
        self.scratch_group_cursor.as_mut_ptr()
    }

    #[must_use]
    pub fn scratch_group_capacity(&self) -> usize {
        self.scratch_sum
            .len()
            .min(self.scratch_min.len())
            .min(self.scratch_max.len())
            .min(self.scratch_count.len())
            .min(self.scratch_group_start.len())
            .min(self.scratch_group_cursor.len())
    }

    #[must_use]
    pub fn scratch_sorted_group_ptr(&self) -> *mut i32 {
        self.scratch_sorted_group.as_mut_ptr()
    }

    #[must_use]
    pub fn scratch_row_index_ptr(&self) -> *mut u32 {
        self.scratch_row_index.as_mut_ptr()
    }

    #[must_use]
    pub fn scratch_row_capacity(&self) -> usize {
        self.scratch_sorted_group
            .len()
            .min(self.scratch_row_index.len())
    }

    #[must_use]
    pub fn scratch_partial_sum_ptr(&self) -> *mut f64 {
        self.scratch_partial_sum
            .as_ref()
            .map_or(std::ptr::null_mut(), ExprDeviceBuffer::as_mut_ptr)
    }

    #[must_use]
    pub fn scratch_partial_min_ptr(&self) -> *mut f64 {
        self.scratch_partial_min
            .as_ref()
            .map_or(std::ptr::null_mut(), ExprDeviceBuffer::as_mut_ptr)
    }

    #[must_use]
    pub fn scratch_partial_max_ptr(&self) -> *mut f64 {
        self.scratch_partial_max
            .as_ref()
            .map_or(std::ptr::null_mut(), ExprDeviceBuffer::as_mut_ptr)
    }

    #[must_use]
    pub fn scratch_partial_count_ptr(&self) -> *mut u32 {
        self.scratch_partial_count
            .as_ref()
            .map_or(std::ptr::null_mut(), ExprDeviceBuffer::as_mut_ptr)
    }

    #[must_use]
    pub fn scratch_partial_capacity(&self) -> usize {
        match (
            self.scratch_partial_sum.as_ref(),
            self.scratch_partial_min.as_ref(),
            self.scratch_partial_max.as_ref(),
            self.scratch_partial_count.as_ref(),
        ) {
            (Some(sum), Some(min), Some(max), Some(count)) => {
                sum.len().min(min.len()).min(max.len()).min(count.len())
            }
            _ => 0,
        }
    }
}

impl ResidentH3GroupAggCache {
    #[must_use]
    pub const fn rel_oid(&self) -> pg_sys::Oid {
        self.rel_oid
    }

    #[must_use]
    pub const fn row_count(&self) -> usize {
        self.row_count
    }

    #[must_use]
    pub const fn kind(&self) -> ResidentH3GroupedCountKind {
        self.kind
    }

    #[must_use]
    pub fn lat_lng_cell_buffer(&self, resolution: i32) -> Option<&ExprDeviceBuffer<i64>> {
        match &self.input {
            ResidentH3GroupAggInput::LatLngCells {
                resolution: cached_resolution,
                cells,
            } if *cached_resolution == resolution => Some(cells),
            ResidentH3GroupAggInput::LatLngCells { .. } => None,
            ResidentH3GroupAggInput::Cell { .. } => None,
        }
    }

    #[must_use]
    pub fn cell_buffer(&self) -> Option<&ExprDeviceBuffer<u64>> {
        match &self.input {
            ResidentH3GroupAggInput::LatLngCells { .. } => None,
            ResidentH3GroupAggInput::Cell { cells } => Some(cells),
        }
    }
}

impl ResidentStarDimensionHostFilter {
    fn match_only<Row>(
        rows: &[Row],
        key_count: usize,
        key: impl Fn(&Row) -> i32,
        matches: impl Fn(&Row) -> bool,
    ) -> Self {
        let mut match_by_key = vec![0u8; key_count];
        for row in rows {
            let Ok(key) = usize::try_from(key(row)) else {
                continue;
            };
            if key < match_by_key.len() && matches(row) {
                match_by_key[key] = 1;
            }
        }
        Self {
            match_by_key,
            group_code_by_key: None,
            labels: Vec::new(),
        }
    }

    fn grouped_by_label<Row>(
        rows: &[Row],
        key_count: usize,
        key: impl Fn(&Row) -> i32,
        matches: impl Fn(&Row) -> bool,
        label: impl Fn(&Row) -> &str,
        empty_labels_error: &'static str,
        missing_label_error: &'static str,
    ) -> Result<Self, String> {
        let labels: Vec<String> = rows
            .iter()
            .filter(|row| matches(row))
            .map(|row| label(row).to_owned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if labels.is_empty() {
            return Err(empty_labels_error.to_owned());
        }
        if labels.len() > i32::MAX as usize {
            return Err("SSBM grouping label count exceeds int32 code space".to_owned());
        }

        let label_index: BTreeMap<&str, i32> = labels
            .iter()
            .enumerate()
            .map(|(idx, label)| {
                i32::try_from(idx)
                    .map(|code| (label.as_str(), code))
                    .map_err(|_| "SSBM grouping label index exceeds int32 code space".to_owned())
            })
            .collect::<Result<_, String>>()?;
        let mut match_by_key = vec![0u8; key_count];
        let mut group_code_by_key = vec![-1i32; key_count];
        for row in rows {
            let Ok(key) = usize::try_from(key(row)) else {
                continue;
            };
            if key >= match_by_key.len() || !matches(row) {
                continue;
            }
            let label = label(row);
            match_by_key[key] = 1;
            group_code_by_key[key] = *label_index.get(label).ok_or(missing_label_error)?;
        }

        Ok(Self {
            match_by_key,
            group_code_by_key: Some(group_code_by_key),
            labels,
        })
    }

    fn grouped_by_fixed_code<Row>(
        rows: &[Row],
        key_count: usize,
        labels: Vec<String>,
        code: i32,
        key: impl Fn(&Row) -> i32,
        matches: impl Fn(&Row) -> bool,
    ) -> Self {
        let mut match_by_key = vec![0u8; key_count];
        let mut group_code_by_key = vec![-1i32; key_count];
        for row in rows {
            let Ok(key) = usize::try_from(key(row)) else {
                continue;
            };
            if key < match_by_key.len() && matches(row) {
                match_by_key[key] = 1;
                group_code_by_key[key] = code;
            }
        }
        Self {
            match_by_key,
            group_code_by_key: Some(group_code_by_key),
            labels,
        }
    }
}

impl ResidentStarDimensionFilter {
    fn from_host(
        host: ResidentStarDimensionHostFilter,
        match_buffer_name: &str,
        group_buffer_name: Option<&str>,
    ) -> Result<Self, String> {
        let key_count = host.match_by_key.len();
        let group_code_by_key = match (host.group_code_by_key, group_buffer_name) {
            (Some(group_code_by_key), Some(group_buffer_name)) => {
                Some(alloc_and_copy_i32(&group_code_by_key, group_buffer_name)?)
            }
            (Some(_), None) => {
                return Err("resident star grouped dimension missing group buffer name".to_owned());
            }
            (None, _) => None,
        };
        Ok(Self {
            match_by_key: alloc_and_copy_u8(&host.match_by_key, match_buffer_name)?,
            group_code_by_key,
            key_count,
            labels: host.labels,
        })
    }

    fn match_only<Row>(
        rows: &[Row],
        key_count: usize,
        key: impl Fn(&Row) -> i32,
        matches: impl Fn(&Row) -> bool,
        match_buffer_name: &str,
    ) -> Result<Self, String> {
        Self::from_host(
            ResidentStarDimensionHostFilter::match_only(rows, key_count, key, matches),
            match_buffer_name,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn grouped_by_label<Row>(
        rows: &[Row],
        key_count: usize,
        key: impl Fn(&Row) -> i32,
        matches: impl Fn(&Row) -> bool,
        label: impl Fn(&Row) -> &str,
        empty_labels_error: &'static str,
        missing_label_error: &'static str,
        match_buffer_name: &str,
        group_buffer_name: &str,
    ) -> Result<Self, String> {
        Self::from_host(
            ResidentStarDimensionHostFilter::grouped_by_label(
                rows,
                key_count,
                key,
                matches,
                label,
                empty_labels_error,
                missing_label_error,
            )?,
            match_buffer_name,
            Some(group_buffer_name),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn grouped_by_fixed_code<Row>(
        rows: &[Row],
        key_count: usize,
        labels: Vec<String>,
        code: i32,
        key: impl Fn(&Row) -> i32,
        matches: impl Fn(&Row) -> bool,
        match_buffer_name: &str,
        group_buffer_name: &str,
    ) -> Result<Self, String> {
        Self::from_host(
            ResidentStarDimensionHostFilter::grouped_by_fixed_code(
                rows, key_count, labels, code, key, matches,
            ),
            match_buffer_name,
            Some(group_buffer_name),
        )
    }

    fn match_ptr(&self) -> *const u8 {
        self.match_by_key.as_ptr()
    }

    fn group_code_ptr(&self) -> *const i32 {
        self.group_code_by_key.as_ref().map_or_else(
            || {
                pgrx::error!(
                    "pg_accel: resident star grouped dimension filter is missing its \
                     group-code buffer; the grouped SSBM kernel cannot dispatch"
                )
            },
            ExprDeviceBuffer::as_ptr,
        )
    }

    const fn key_count(&self) -> usize {
        self.key_count
    }

    fn labels(&self) -> &[String] {
        &self.labels
    }
}

fn ssbm_date_offset_match_filter(
    rows: &[SsbmDateRow],
    date_key_min: i32,
    date_key_count: usize,
    matches: impl Fn(&SsbmDateRow) -> bool,
    buffer_name: &str,
) -> Result<ExprDeviceBuffer<u8>, String> {
    let mut date_match = vec![0u8; date_key_count];
    for row in rows {
        let offset = i64::from(row.datekey) - i64::from(date_key_min);
        let Ok(offset) = usize::try_from(offset) else {
            continue;
        };
        if offset < date_match.len() && matches(row) {
            date_match[offset] = 1;
        }
    }
    alloc_and_copy_u8(&date_match, buffer_name)
}

impl SsbmQ1CachedDateFilter {
    fn from_keys(predicate: SsbmQ1DatePredicate, keys: Vec<i32>) -> Result<Self, String> {
        if keys.is_empty() {
            return Ok(Self {
                predicate,
                orderdate_lo: 1,
                orderdate_hi: 0,
                key_buffer: None,
                key_count: 0,
            });
        }

        if let Some((orderdate_lo, orderdate_hi)) = contiguous_key_range(&keys) {
            return Ok(Self {
                predicate,
                orderdate_lo,
                orderdate_hi,
                key_buffer: None,
                key_count: 0,
            });
        }

        let key_count = keys.len();
        Ok(Self {
            predicate,
            orderdate_lo: 0,
            orderdate_hi: 0,
            key_buffer: Some(alloc_and_copy_i32(&keys, "ssbm_q1_date_filter_membership")?),
            key_count,
        })
    }

    fn resolved(&self) -> SsbmQ1ResolvedDateFilter {
        SsbmQ1ResolvedDateFilter {
            orderdate_lo: self.orderdate_lo,
            orderdate_hi: self.orderdate_hi,
            orderdate_keys: self
                .key_buffer
                .as_ref()
                .map_or(std::ptr::null(), ExprDeviceBuffer::as_ptr),
            orderdate_key_count: self.key_count,
        }
    }
}

impl SsbmQ2CachedFilter {
    fn from_rows(
        variant: SsbmQ2Variant,
        part_rows: &[SsbmPartRow],
        part_key_count: usize,
        supplier_rows: &[SsbmSupplierRow],
        supplier_key_count: usize,
    ) -> Result<Self, String> {
        if part_key_count == 0 || supplier_key_count == 0 {
            return Err("SSBM Q2 dimension key map is empty".to_owned());
        }

        let region = variant.supplier_region();
        Ok(Self {
            variant,
            part: ResidentStarDimensionFilter::match_only(
                part_rows,
                part_key_count,
                |row| row.partkey,
                |row| variant.part_matches(row),
                "ssbm_q2_part_filter",
            )?,
            supplier: ResidentStarDimensionFilter::match_only(
                supplier_rows,
                supplier_key_count,
                |row| row.suppkey,
                |row| row.region == region,
                "ssbm_q2_supplier_filter",
            )?,
        })
    }

    fn resolved(&self) -> SsbmQ2ResolvedFilter {
        SsbmQ2ResolvedFilter {
            part_match_by_key: self.part.match_ptr(),
            part_key_count: self.part.key_count(),
            supplier_match_by_key: self.supplier.match_ptr(),
            supplier_key_count: self.supplier.key_count(),
        }
    }
}

impl SsbmQ3CachedFilter {
    // Builds the cached filter from each dimension table's rows plus their
    // dense-key extents; every argument is a distinct required input.
    #[allow(clippy::too_many_arguments)]
    fn from_rows(
        variant: SsbmQ3Variant,
        date_rows: &[SsbmDateRow],
        date_key_min: i32,
        date_year_count: usize,
        customer_rows: &[SsbmCustomerRow],
        customer_key_count: usize,
        supplier_rows: &[SsbmSupplierRow],
        supplier_key_count: usize,
    ) -> Result<Self, String> {
        if date_year_count == 0 || customer_key_count == 0 || supplier_key_count == 0 {
            return Err("SSBM Q3 dimension key map is empty".to_owned());
        }

        Ok(Self {
            variant,
            date_match_by_offset: ssbm_date_offset_match_filter(
                date_rows,
                date_key_min,
                date_year_count,
                |row| variant.date_matches(row),
                "ssbm_q3_date_filter",
            )?,
            customer: ResidentStarDimensionFilter::grouped_by_label(
                customer_rows,
                customer_key_count,
                |row| row.custkey,
                |row| variant.customer_matches(row),
                |row| {
                    if variant.uses_nation_labels() {
                        row.nation.as_str()
                    } else {
                        row.city.as_str()
                    }
                },
                "SSBM Q3 variant produced no grouping labels",
                "missing SSBM Q3 customer label code",
                "ssbm_q3_customer_filter",
                "ssbm_q3_customer_group_code",
            )?,
            supplier: ResidentStarDimensionFilter::grouped_by_label(
                supplier_rows,
                supplier_key_count,
                |row| row.suppkey,
                |row| variant.supplier_matches(row),
                |row| {
                    if variant.uses_nation_labels() {
                        row.nation.as_str()
                    } else {
                        row.city.as_str()
                    }
                },
                "SSBM Q3 variant produced no grouping labels",
                "missing SSBM Q3 supplier label code",
                "ssbm_q3_supplier_filter",
                "ssbm_q3_supplier_group_code",
            )?,
        })
    }

    fn resolved(&self) -> SsbmQ3ResolvedFilter<'_> {
        SsbmQ3ResolvedFilter {
            date_match_by_offset: self.date_match_by_offset.as_ptr(),
            date_year_count: self.date_match_by_offset.len(),
            customer_group_code_by_key: self.customer.group_code_ptr(),
            customer_match_by_key: self.customer.match_ptr(),
            customer_key_count: self.customer.key_count(),
            supplier_group_code_by_key: self.supplier.group_code_ptr(),
            supplier_match_by_key: self.supplier.match_ptr(),
            supplier_key_count: self.supplier.key_count(),
            customer_labels: self.customer.labels(),
            supplier_labels: self.supplier.labels(),
        }
    }
}

impl SsbmQ4CachedFilter {
    // Q4 spans all four dimension tables, so the builder takes each table's rows
    // and dense-key extent; every argument is a distinct required input.
    #[allow(clippy::too_many_arguments)]
    fn from_rows(
        variant: SsbmQ4Variant,
        date_rows: &[SsbmDateRow],
        date_key_min: i32,
        date_year_count: usize,
        customer_rows: &[SsbmCustomerRow],
        customer_key_count: usize,
        supplier_rows: &[SsbmSupplierRow],
        supplier_key_count: usize,
        part_rows: &[SsbmPartRow],
        part_key_count: usize,
    ) -> Result<Self, String> {
        if date_year_count == 0
            || customer_key_count == 0
            || supplier_key_count == 0
            || part_key_count == 0
        {
            return Err("SSBM Q4 dimension key map is empty".to_owned());
        }

        Ok(Self {
            variant,
            date_match_by_offset: ssbm_date_offset_match_filter(
                date_rows,
                date_key_min,
                date_year_count,
                |row| variant.date_matches(row),
                "ssbm_q4_date_filter",
            )?,
            customer: if variant.group_geo_source() == 1 {
                ResidentStarDimensionFilter::grouped_by_label(
                    customer_rows,
                    customer_key_count,
                    |row| row.custkey,
                    |row| variant.customer_matches(row),
                    |row| row.nation.as_str(),
                    "SSBM Q4 variant produced no grouping labels",
                    "missing SSBM Q4 customer geo label code",
                    "ssbm_q4_customer_filter",
                    "ssbm_q4_customer_group_code",
                )?
            } else {
                ResidentStarDimensionFilter::grouped_by_fixed_code(
                    customer_rows,
                    customer_key_count,
                    Vec::new(),
                    0,
                    |row| row.custkey,
                    |row| variant.customer_matches(row),
                    "ssbm_q4_customer_filter",
                    "ssbm_q4_customer_group_code",
                )?
            },
            supplier: if variant.group_geo_source() == 2 {
                ResidentStarDimensionFilter::grouped_by_label(
                    supplier_rows,
                    supplier_key_count,
                    |row| row.suppkey,
                    |row| variant.supplier_matches(row),
                    |row| {
                        if matches!(variant, SsbmQ4Variant::Q4_2) {
                            row.nation.as_str()
                        } else {
                            row.city.as_str()
                        }
                    },
                    "SSBM Q4 variant produced no grouping labels",
                    "missing SSBM Q4 supplier geo label code",
                    "ssbm_q4_supplier_filter",
                    "ssbm_q4_supplier_group_code",
                )?
            } else {
                ResidentStarDimensionFilter::grouped_by_fixed_code(
                    supplier_rows,
                    supplier_key_count,
                    Vec::new(),
                    0,
                    |row| row.suppkey,
                    |row| variant.supplier_matches(row),
                    "ssbm_q4_supplier_filter",
                    "ssbm_q4_supplier_group_code",
                )?
            },
            part: match variant {
                SsbmQ4Variant::Q4_1 => ResidentStarDimensionFilter::grouped_by_fixed_code(
                    part_rows,
                    part_key_count,
                    vec![String::new()],
                    0,
                    |row| row.partkey,
                    |row| variant.part_matches(row),
                    "ssbm_q4_part_filter",
                    "ssbm_q4_part_group_code",
                )?,
                SsbmQ4Variant::Q4_2 => ResidentStarDimensionFilter::grouped_by_label(
                    part_rows,
                    part_key_count,
                    |row| row.partkey,
                    |row| variant.part_matches(row),
                    |row| row.category.as_str(),
                    "SSBM Q4 variant produced no grouping labels",
                    "missing SSBM Q4 part category label code",
                    "ssbm_q4_part_filter",
                    "ssbm_q4_part_group_code",
                )?,
                SsbmQ4Variant::Q4_3 => ResidentStarDimensionFilter::grouped_by_label(
                    part_rows,
                    part_key_count,
                    |row| row.partkey,
                    |row| variant.part_matches(row),
                    |row| row.brand1.as_str(),
                    "SSBM Q4 variant produced no grouping labels",
                    "missing SSBM Q4 part brand label code",
                    "ssbm_q4_part_filter",
                    "ssbm_q4_part_group_code",
                )?,
            },
        })
    }

    fn geo_labels(&self) -> &[String] {
        match self.variant.group_geo_source() {
            1 => self.customer.labels(),
            2 => self.supplier.labels(),
            _ => &[],
        }
    }

    fn part_labels(&self) -> &[String] {
        self.part.labels()
    }

    fn resolved(&self) -> SsbmQ4ResolvedFilter<'_> {
        SsbmQ4ResolvedFilter {
            date_match_by_offset: self.date_match_by_offset.as_ptr(),
            date_year_count: self.date_match_by_offset.len(),
            customer_group_code_by_key: self.customer.group_code_ptr(),
            customer_match_by_key: self.customer.match_ptr(),
            customer_key_count: self.customer.key_count(),
            supplier_group_code_by_key: self.supplier.group_code_ptr(),
            supplier_match_by_key: self.supplier.match_ptr(),
            supplier_key_count: self.supplier.key_count(),
            part_group_code_by_key: self.part.group_code_ptr(),
            part_match_by_key: self.part.match_ptr(),
            part_key_count: self.part.key_count(),
            group_geo_source: self.variant.group_geo_source(),
            geo_labels: self.geo_labels(),
            part_labels: self.part_labels(),
        }
    }
}

fn date_keys_from_rows(rows: &[SsbmDateRow], predicate: SsbmQ1DatePredicate) -> Vec<i32> {
    let mut keys: Vec<i32> = rows
        .iter()
        .filter(|row| match predicate {
            SsbmQ1DatePredicate::Year(year) => row.year == year,
            SsbmQ1DatePredicate::YearMonthNum(yearmonthnum) => row.yearmonthnum == yearmonthnum,
            SsbmQ1DatePredicate::YearWeek { year, week } => {
                row.year == year && row.weeknuminyear == week
            }
        })
        .map(|row| row.datekey)
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

fn contiguous_key_range(keys: &[i32]) -> Option<(i32, i32)> {
    let (&first, rest) = keys.split_first()?;
    let last = rest
        .iter()
        .try_fold(first, |prev, &key| (key == prev + 1).then_some(key))?;
    Some((first, last))
}

thread_local! {
    static SSBM_Q1_CACHE: RefCell<Option<SsbmQ1ResidentCache>> = const { RefCell::new(None) };
    static RESIDENT_DENSE_GROUP_AGG_CACHE: RefCell<Option<ResidentDenseGroupAggCache>> = const { RefCell::new(None) };
    static RESIDENT_STAR_DIM_GROUP_AGG_CACHE: RefCell<Option<ResidentStarDimGroupAggCache>> = const { RefCell::new(None) };
    static RESIDENT_H3_GROUP_AGG_CACHE: RefCell<Option<ResidentH3GroupAggCache>> = const { RefCell::new(None) };
    static RESIDENT_HASH_JOIN_COUNT_CACHE: RefCell<Option<ResidentHashJoinCountCache>> = const { RefCell::new(None) };
}

// ---------------------------------------------------------------------------
// Relcache invalidation (stopgap until residency v2 generation counters)
// ---------------------------------------------------------------------------
//
// The resident caches above are backend-local snapshots keyed by relation OID.
// A `CacheRegisterRelcacheCallback` callback records every relcache
// invalidation that could affect a cached relation (TRUNCATE, ALTER TABLE,
// DROP TABLE, VACUUM FULL, CLUSTER, ...) into fixed-capacity `Cell` storage,
// and `process_relcache_invalidations()` — called at the top of every cache
// accessor — drops the matching slots before the planner or executor can see
// stale device data.
//
// Known limitations (documented, not hidden):
//
// 1. Plain row-level DML (INSERT/UPDATE/DELETE) does NOT fire relcache
//    invalidation, so it is NOT detected by this stopgap. There is no cheap
//    reliable backend-local signal for it: `pg_class.relfilenode` only
//    changes on rewrites, `RelationGetNumberOfBlocks` misses updates/deletes
//    and free-space-map reuse, and pgstat counters are flushed
//    asynchronously. Residency v2 will add trigger-backed generation
//    counters; until then every successful load emits a
//    once-per-session-per-relation WARNING pointing at the manual
//    `pg_accel_clear_*` functions (see `warn_dml_staleness_once`).
// 2. TRUNCATE of a table created in the *current* (sub)transaction takes
//    PostgreSQL's non-transactional in-place shortcut
//    (`ExecuteTruncateGuts`: `rd_createSubid == mySubid` →
//    `heap_truncate_one_rel`), which fires no relcache invalidation. A cache
//    loaded from that table earlier in the same transaction is not cleared.
//    TRUNCATE of any table that predates the current transaction rewrites
//    the relfilenode and IS invalidated (covered by
//    `tests/phase2_cache.rs`).

/// Capacity of the pending-invalidation scratch. Sized for the handful of
/// relations the resident caches can reference (SSBM uses five); overflow
/// degrades to a conservative clear-everything, never to a missed
/// invalidation.
const PENDING_RELCACHE_INVAL_CAP: usize = 16;

#[derive(Clone, Copy)]
struct PendingRelcacheInvals {
    relids: [u32; PENDING_RELCACHE_INVAL_CAP],
    len: usize,
}

impl PendingRelcacheInvals {
    const fn empty() -> Self {
        Self {
            relids: [0; PENDING_RELCACHE_INVAL_CAP],
            len: 0,
        }
    }

    fn contains(&self, relid: u32) -> bool {
        self.relids[..self.len].contains(&relid)
    }
}

thread_local! {
    static RELCACHE_CALLBACK_REGISTERED: Cell<bool> = const { Cell::new(false) };
    static RESIDENT_CACHE_LOAD_IN_PROGRESS: Cell<bool> = const { Cell::new(false) };
    static PENDING_RELCACHE_CLEAR_ALL: Cell<bool> = const { Cell::new(false) };
    static PENDING_RELCACHE_INVALS: Cell<PendingRelcacheInvals> =
        const { Cell::new(PendingRelcacheInvals::empty()) };
    static DML_STALENESS_WARNED_RELIDS: RefCell<BTreeSet<u32>> =
        const { RefCell::new(BTreeSet::new()) };
}

/// Relation OIDs referenced by whatever is in each cache slot right now.
/// Returns `true` when the slot holds a cache referencing `relid`. A slot
/// whose `RefCell` is currently borrowed reports `true` (conservative): the
/// callback must never panic, so it cannot wait for the borrow.
fn relid_matches_any_resident_cache(relid: pg_sys::Oid) -> bool {
    let ssbm = SSBM_Q1_CACHE.try_with(|slot| match slot.try_borrow() {
        Ok(borrow) => borrow.as_ref().is_some_and(|cache| {
            [
                cache.fact_rel_oid,
                cache.date_rel_oid,
                cache.part_rel_oid,
                cache.customer_rel_oid,
                cache.supplier_rel_oid,
            ]
            .contains(&relid)
        }),
        Err(_) => true,
    });
    let dense = RESIDENT_DENSE_GROUP_AGG_CACHE.try_with(|slot| match slot.try_borrow() {
        Ok(borrow) => borrow.as_ref().is_some_and(|cache| cache.rel_oid == relid),
        Err(_) => true,
    });
    let star = RESIDENT_STAR_DIM_GROUP_AGG_CACHE.try_with(|slot| match slot.try_borrow() {
        Ok(borrow) => borrow
            .as_ref()
            .is_some_and(|cache| cache.fact_rel_oid == relid || cache.dim_rel_oid == relid),
        Err(_) => true,
    });
    let h3 = RESIDENT_H3_GROUP_AGG_CACHE.try_with(|slot| match slot.try_borrow() {
        Ok(borrow) => borrow.as_ref().is_some_and(|cache| cache.rel_oid == relid),
        Err(_) => true,
    });
    let hashjoin = RESIDENT_HASH_JOIN_COUNT_CACHE.try_with(|slot| match slot.try_borrow() {
        Ok(borrow) => borrow
            .as_ref()
            .is_some_and(|cache| cache.outer_rel_oid == relid || cache.inner_rel_oid == relid),
        Err(_) => true,
    });
    // `try_with` fails only during thread-local teardown (backend exit); the
    // caches die with the backend, so a miss there is harmless either way.
    [ssbm, dense, star, h3, hashjoin]
        .into_iter()
        .any(|hit| hit.unwrap_or(false))
}

/// Relcache invalidation callback: records which relation OIDs were
/// invalidated so the next resident-cache access can drop stale slots.
///
/// This runs inside PostgreSQL's cache-invalidation processing (command end,
/// lock acquisition, transaction abort). It must be minimal and panic-free:
/// no PostgreSQL calls (no elog/palloc — an ERROR raised here would corrupt
/// invalidation processing), no allocation, and no freeing of GPU device
/// memory — dropping an `ExprDeviceBuffer` calls into the SYCL runtime, which
/// has no safety contract inside abort-time invalidation processing. The
/// relid is recorded into fixed-capacity `Cell` storage instead, and the
/// matching cache slots are dropped lazily by
/// `process_relcache_invalidations()` on the next cache access, which runs in
/// normal planner/executor context.
unsafe extern "C-unwind" fn resident_cache_relcache_callback(
    _arg: pg_sys::Datum,
    relid: pg_sys::Oid,
) {
    // All thread-local access uses `try_with`/`try_borrow` so this function
    // cannot panic (an unwind escaping a raw C callback into PostgreSQL's
    // inval machinery would be undefined behavior).
    if relid == pg_sys::InvalidOid {
        // InvalidOid means "every relation" (sinval queue overflow).
        let _ = PENDING_RELCACHE_CLEAR_ALL.try_with(|flag| flag.set(true));
        return;
    }
    // Skip relids that cannot affect any resident cache so the fixed-size
    // pending list is not churned by unrelated DDL. While a load is in
    // progress the slot under (re)construction is not yet visible, so record
    // unconditionally — the post-install drain re-checks against the freshly
    // stored cache.
    let load_in_progress = RESIDENT_CACHE_LOAD_IN_PROGRESS
        .try_with(Cell::get)
        .unwrap_or(false);
    if !load_in_progress && !relid_matches_any_resident_cache(relid) {
        return;
    }
    let _ = PENDING_RELCACHE_INVALS.try_with(|pending_cell| {
        let mut pending = pending_cell.get();
        let relid_raw = u32::from(relid);
        if pending.contains(relid_raw) {
            return;
        }
        if pending.len == PENDING_RELCACHE_INVAL_CAP {
            // Overflow degrades to clear-everything, never to a missed
            // invalidation.
            let _ = PENDING_RELCACHE_CLEAR_ALL.try_with(|flag| flag.set(true));
            return;
        }
        pending.relids[pending.len] = relid_raw;
        pending.len += 1;
        pending_cell.set(pending);
    });
}

/// Idempotently register the relcache invalidation callback for this backend.
///
/// Called lazily from every `pg_accel_load_*` entry point (rather than from
/// `_PG_init`) so this module stays self-contained. PostgreSQL keeps the
/// registration for the life of the backend; there is no unregister API.
fn ensure_relcache_invalidation_callback_registered() {
    if RELCACHE_CALLBACK_REGISTERED.get() {
        return;
    }
    // SAFETY: called on the main backend thread from a `#[pg_extern]`
    // function. `CacheRegisterRelcacheCallback` appends to inval.c's
    // backend-local callback array; the function pointer is a static item and
    // the zero Datum arg needs no lifetime, so both remain valid for the
    // backend lifetime. PostgreSQL raises FATAL if more than
    // MAX_RELCACHE_CALLBACKS registrations occur; the registered flag keeps
    // this to exactly one per backend.
    unsafe {
        pg_sys::CacheRegisterRelcacheCallback(
            Some(resident_cache_relcache_callback),
            pg_sys::Datum::from(0),
        );
    }
    RELCACHE_CALLBACK_REGISTERED.set(true);
}

/// Drop any resident cache slots invalidated since the last access.
///
/// Called at the top of every resident-cache accessor (planner recognizers,
/// executor lookups, row-count SRFs) and around cache loads. Runs in normal
/// backend context, so dropping `ExprDeviceBuffer`s (freeing GPU USM memory)
/// is safe here — unlike in the relcache callback itself. Device memory for
/// an invalidated cache is therefore reclaimed on the next access, not at
/// invalidation time; a cache that is never touched again is freed at backend
/// exit.
fn process_relcache_invalidations() {
    let clear_all = PENDING_RELCACHE_CLEAR_ALL.replace(false);
    let pending = PENDING_RELCACHE_INVALS.replace(PendingRelcacheInvals::empty());
    if !clear_all && pending.len == 0 {
        return;
    }
    let invalidated = |cache_relids: &[pg_sys::Oid]| -> bool {
        clear_all
            || cache_relids
                .iter()
                .any(|oid| pending.contains(u32::from(*oid)))
    };

    SSBM_Q1_CACHE.with(|slot| {
        let stale = slot.borrow().as_ref().is_some_and(|cache| {
            invalidated(&[
                cache.fact_rel_oid,
                cache.date_rel_oid,
                cache.part_rel_oid,
                cache.customer_rel_oid,
                cache.supplier_rel_oid,
            ])
        });
        if stale {
            *slot.borrow_mut() = None;
            pgrx::debug1!("pg_accel: SSBM resident cache dropped after relcache invalidation");
        }
    });
    RESIDENT_DENSE_GROUP_AGG_CACHE.with(|slot| {
        let stale = slot
            .borrow()
            .as_ref()
            .is_some_and(|cache| invalidated(&[cache.rel_oid]));
        if stale {
            *slot.borrow_mut() = None;
            pgrx::debug1!(
                "pg_accel: resident dense groupagg cache dropped after relcache invalidation"
            );
        }
    });
    RESIDENT_STAR_DIM_GROUP_AGG_CACHE.with(|slot| {
        let stale = slot
            .borrow()
            .as_ref()
            .is_some_and(|cache| invalidated(&[cache.fact_rel_oid, cache.dim_rel_oid]));
        if stale {
            *slot.borrow_mut() = None;
            pgrx::debug1!(
                "pg_accel: resident star groupagg cache dropped after relcache invalidation"
            );
        }
    });
    RESIDENT_H3_GROUP_AGG_CACHE.with(|slot| {
        let stale = slot
            .borrow()
            .as_ref()
            .is_some_and(|cache| invalidated(&[cache.rel_oid]));
        if stale {
            *slot.borrow_mut() = None;
            pgrx::debug1!(
                "pg_accel: resident H3 groupagg cache dropped after relcache invalidation"
            );
        }
    });
    RESIDENT_HASH_JOIN_COUNT_CACHE.with(|slot| {
        let stale = slot
            .borrow()
            .as_ref()
            .is_some_and(|cache| invalidated(&[cache.outer_rel_oid, cache.inner_rel_oid]));
        if stale {
            *slot.borrow_mut() = None;
            pgrx::debug1!(
                "pg_accel: resident hashjoin count cache dropped after relcache invalidation"
            );
        }
    });
}

/// Marks a resident-cache load as in progress so the relcache callback
/// records every invalidation unconditionally (the slot under construction is
/// not yet visible for OID matching). The flag is reset on drop, including
/// when a load error unwinds.
struct ResidentCacheLoadGuard;

impl ResidentCacheLoadGuard {
    fn begin() -> Self {
        RESIDENT_CACHE_LOAD_IN_PROGRESS.set(true);
        Self
    }
}

impl Drop for ResidentCacheLoadGuard {
    fn drop(&mut self) {
        RESIDENT_CACHE_LOAD_IN_PROGRESS.set(false);
    }
}

/// Emit the DML staleness contract once per session per relation: relcache
/// invalidation covers DDL/TRUNCATE/VACUUM FULL, but plain INSERT/UPDATE/
/// DELETE is invisible to this stopgap (see the module comment above for why
/// no cheap reliable detection exists) and requires a manual clear or reload
/// until residency v2 lands generation counters.
fn warn_dml_staleness_once(rel_oids: &[pg_sys::Oid]) {
    DML_STALENESS_WARNED_RELIDS.with(|warned| {
        let mut warned = warned.borrow_mut();
        for oid in rel_oids {
            let raw = u32::from(*oid);
            if warned.insert(raw) {
                pgrx::warning!(
                    "pg_accel: resident cache loaded for relation OID {raw}: DDL, TRUNCATE, \
                     and VACUUM FULL invalidate it automatically, but plain DML \
                     (INSERT/UPDATE/DELETE) does not; after row-level changes, reload the \
                     cache or call the matching pg_accel_clear_* function, or queries may \
                     return stale results until residency v2 adds DML invalidation"
                );
            }
        }
    });
}

/// Number of rows an interruptible SPI load loop scans between
/// `CHECK_FOR_INTERRUPTS()` calls (safety rule #7 cadence).
const LOAD_INTERRUPT_CHECK_ROWS: usize = 8192;

/// The grouped-count output lanes of every resident kernel are `uint32_t`
/// (`pgaccel_expr.h`: `uint32_t* out_count_by_group`). A per-group count can
/// never exceed the cache's total row count, so bounding the row count at
/// load time honestly prevents 2^32 count wraparound instead of clamping or
/// silently truncating at emission.
fn ensure_resident_row_count_fits_u32(row_count: usize, table: &str) -> Result<(), String> {
    if u32::try_from(row_count).is_ok() {
        Ok(())
    } else {
        Err(format!(
            "{table} has {row_count} rows; resident cache grouped-count lanes are 32-bit in \
             the kernel ABI (uint32_t out_count_by_group), refusing to load a cache that \
             could wrap per-group counts"
        ))
    }
}

#[must_use]
pub fn ssbm_q1_cache_loaded_for(fact_rel_oid: pg_sys::Oid, date_rel_oid: pg_sys::Oid) -> bool {
    process_relcache_invalidations();
    SSBM_Q1_CACHE.with(|slot| {
        slot.borrow().as_ref().is_some_and(|cache| {
            cache.fact_rel_oid() == fact_rel_oid && cache.date_rel_oid() == date_rel_oid
        })
    })
}

#[must_use]
pub fn ssbm_q2_cache_loaded_for(
    fact_rel_oid: pg_sys::Oid,
    date_rel_oid: pg_sys::Oid,
    part_rel_oid: pg_sys::Oid,
    supplier_rel_oid: pg_sys::Oid,
) -> bool {
    process_relcache_invalidations();
    SSBM_Q1_CACHE.with(|slot| {
        slot.borrow().as_ref().is_some_and(|cache| {
            cache.fact_rel_oid() == fact_rel_oid
                && cache.date_rel_oid() == date_rel_oid
                && cache.part_rel_oid() == part_rel_oid
                && cache.supplier_rel_oid() == supplier_rel_oid
        })
    })
}

#[must_use]
pub fn ssbm_q3_cache_loaded_for(
    fact_rel_oid: pg_sys::Oid,
    date_rel_oid: pg_sys::Oid,
    customer_rel_oid: pg_sys::Oid,
    supplier_rel_oid: pg_sys::Oid,
) -> bool {
    process_relcache_invalidations();
    SSBM_Q1_CACHE.with(|slot| {
        slot.borrow().as_ref().is_some_and(|cache| {
            cache.fact_rel_oid() == fact_rel_oid
                && cache.date_rel_oid() == date_rel_oid
                && cache.customer_rel_oid() == customer_rel_oid
                && cache.supplier_rel_oid() == supplier_rel_oid
        })
    })
}

#[must_use]
pub fn ssbm_q4_cache_loaded_for(
    fact_rel_oid: pg_sys::Oid,
    date_rel_oid: pg_sys::Oid,
    part_rel_oid: pg_sys::Oid,
    customer_rel_oid: pg_sys::Oid,
    supplier_rel_oid: pg_sys::Oid,
) -> bool {
    process_relcache_invalidations();
    SSBM_Q1_CACHE.with(|slot| {
        slot.borrow().as_ref().is_some_and(|cache| {
            cache.fact_rel_oid() == fact_rel_oid
                && cache.date_rel_oid() == date_rel_oid
                && cache.part_rel_oid() == part_rel_oid
                && cache.customer_rel_oid() == customer_rel_oid
                && cache.supplier_rel_oid() == supplier_rel_oid
        })
    })
}

#[must_use]
pub fn ssbm_q1_cache_rows() -> usize {
    process_relcache_invalidations();
    SSBM_Q1_CACHE.with(|slot| {
        slot.borrow()
            .as_ref()
            .map_or(0, SsbmQ1ResidentCache::row_count)
    })
}

#[must_use]
pub fn resident_dense_groupagg_cache_loaded_for_shape(
    rel_oid: pg_sys::Oid,
    measure_op: ResidentMeasureOp,
    requires_rhs: bool,
    requires_filter: bool,
) -> bool {
    process_relcache_invalidations();
    RESIDENT_DENSE_GROUP_AGG_CACHE.with(|slot| {
        slot.borrow().as_ref().is_some_and(|cache| {
            cache.rel_oid() == rel_oid
                && cache.measure_op == measure_op
                && cache.value_rhs.is_some() == requires_rhs
                && (!requires_filter || cache.filter.is_some())
        })
    })
}

#[must_use]
pub fn resident_dense_groupagg_cache_shape(
    rel_oid: pg_sys::Oid,
) -> Option<ResidentDenseGroupAggCacheShape> {
    process_relcache_invalidations();
    RESIDENT_DENSE_GROUP_AGG_CACHE.with(|slot| {
        slot.borrow()
            .as_ref()
            .filter(|cache| cache.rel_oid() == rel_oid)
            .map(ResidentDenseGroupAggCache::shape)
    })
}

#[must_use]
pub fn resident_dense_groupagg_cache_rows() -> usize {
    process_relcache_invalidations();
    RESIDENT_DENSE_GROUP_AGG_CACHE.with(|slot| {
        slot.borrow()
            .as_ref()
            .map_or(0, ResidentDenseGroupAggCache::row_count)
    })
}

pub fn with_resident_dense_groupagg_cache<R>(
    rel_oid: pg_sys::Oid,
    measure_op: ResidentMeasureOp,
    requires_rhs: bool,
    requires_filter: bool,
    f: impl FnOnce(&ResidentDenseGroupAggCache) -> R,
) -> Option<R> {
    process_relcache_invalidations();
    RESIDENT_DENSE_GROUP_AGG_CACHE.with(|slot| {
        let borrow = slot.borrow();
        let cache = borrow.as_ref()?;
        (cache.rel_oid() == rel_oid
            && cache.measure_op == measure_op
            && cache.value_rhs.is_some() == requires_rhs
            && (!requires_filter || cache.filter.is_some()))
        .then(|| f(cache))
    })
}

#[must_use]
pub fn resident_star_dim_groupagg_cache_shape() -> Option<ResidentStarDimGroupAggCacheShape> {
    process_relcache_invalidations();
    RESIDENT_STAR_DIM_GROUP_AGG_CACHE.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(ResidentStarDimGroupAggCache::shape)
    })
}

#[must_use]
pub fn resident_star_dim_groupagg_cache_rows() -> usize {
    process_relcache_invalidations();
    RESIDENT_STAR_DIM_GROUP_AGG_CACHE.with(|slot| {
        slot.borrow()
            .as_ref()
            .map_or(0, ResidentStarDimGroupAggCache::row_count)
    })
}

#[must_use]
pub fn resident_star_dim_groupagg_cache_dim_key_count() -> usize {
    process_relcache_invalidations();
    RESIDENT_STAR_DIM_GROUP_AGG_CACHE.with(|slot| {
        slot.borrow()
            .as_ref()
            .map_or(0, ResidentStarDimGroupAggCache::dim_key_count)
    })
}

pub fn with_resident_star_dim_groupagg_cache<R>(
    shape: ResidentStarDimGroupAggCacheShape,
    f: impl FnOnce(&ResidentStarDimGroupAggCache) -> R,
) -> Option<R> {
    process_relcache_invalidations();
    RESIDENT_STAR_DIM_GROUP_AGG_CACHE.with(|slot| {
        let borrow = slot.borrow();
        let cache = borrow.as_ref()?;
        (cache.shape() == shape).then(|| f(cache))
    })
}

#[must_use]
pub fn resident_hashjoin_count_cache_loaded_for(
    outer_rel_oid: pg_sys::Oid,
    outer_attno: i32,
    inner_rel_oid: pg_sys::Oid,
    inner_attno: i32,
    key_type: PgaccelKeyType,
) -> bool {
    process_relcache_invalidations();
    RESIDENT_HASH_JOIN_COUNT_CACHE.with(|slot| {
        slot.borrow().as_ref().is_some_and(|cache| {
            cache.matches(
                outer_rel_oid,
                outer_attno,
                inner_rel_oid,
                inner_attno,
                key_type,
            )
        })
    })
}

#[must_use]
pub fn resident_hashjoin_count_cache_shape() -> Option<ResidentHashJoinCountCacheShape> {
    process_relcache_invalidations();
    RESIDENT_HASH_JOIN_COUNT_CACHE.with(|slot| slot.borrow().as_ref().map(|cache| cache.shape()))
}

#[must_use]
pub fn resident_hashjoin_count_cache_rows() -> usize {
    process_relcache_invalidations();
    RESIDENT_HASH_JOIN_COUNT_CACHE.with(|slot| {
        slot.borrow()
            .as_ref()
            .map_or(0, |cache| cache.shape().outer_rows)
    })
}

pub fn with_resident_hashjoin_count_cache<R>(
    outer_rel_oid: pg_sys::Oid,
    outer_attno: i32,
    inner_rel_oid: pg_sys::Oid,
    inner_attno: i32,
    key_type: PgaccelKeyType,
    f: impl FnOnce(&ResidentHashJoinCountCache) -> R,
) -> Option<R> {
    process_relcache_invalidations();
    RESIDENT_HASH_JOIN_COUNT_CACHE.with(|slot| {
        let borrow = slot.borrow();
        let cache = borrow.as_ref()?;
        cache
            .matches(
                outer_rel_oid,
                outer_attno,
                inner_rel_oid,
                inner_attno,
                key_type,
            )
            .then(|| f(cache))
    })
}

#[must_use]
pub fn resident_h3_groupagg_cache_loaded_for(
    rel_oid: pg_sys::Oid,
    kind: ResidentH3GroupedCountKind,
    resolution: i32,
) -> bool {
    process_relcache_invalidations();
    RESIDENT_H3_GROUP_AGG_CACHE.with(|slot| {
        slot.borrow().as_ref().is_some_and(|cache| {
            cache.rel_oid() == rel_oid
                && cache.kind() == kind
                && match kind {
                    ResidentH3GroupedCountKind::LatLngToCell => {
                        cache.lat_lng_cell_buffer(resolution).is_some()
                    }
                    ResidentH3GroupedCountKind::CellToParent => cache.cell_buffer().is_some(),
                }
        })
    })
}

#[must_use]
pub fn resident_h3_groupagg_cache_rows() -> usize {
    process_relcache_invalidations();
    RESIDENT_H3_GROUP_AGG_CACHE.with(|slot| {
        slot.borrow()
            .as_ref()
            .map_or(0, ResidentH3GroupAggCache::row_count)
    })
}

pub fn with_resident_h3_groupagg_cache<R>(
    rel_oid: pg_sys::Oid,
    kind: ResidentH3GroupedCountKind,
    f: impl FnOnce(&ResidentH3GroupAggCache) -> R,
) -> Option<R> {
    process_relcache_invalidations();
    RESIDENT_H3_GROUP_AGG_CACHE.with(|slot| {
        let borrow = slot.borrow();
        let cache = borrow.as_ref()?;
        (cache.rel_oid() == rel_oid && cache.kind() == kind).then(|| f(cache))
    })
}

pub fn with_ssbm_q1_cache<R>(
    fact_rel_oid: pg_sys::Oid,
    date_rel_oid: pg_sys::Oid,
    f: impl FnOnce(&SsbmQ1ResidentCache) -> R,
) -> Option<R> {
    process_relcache_invalidations();
    SSBM_Q1_CACHE.with(|slot| {
        let borrow = slot.borrow();
        let cache = borrow.as_ref()?;
        (cache.fact_rel_oid() == fact_rel_oid && cache.date_rel_oid() == date_rel_oid)
            .then(|| f(cache))
    })
}

pub fn with_ssbm_q2_cache<R>(
    fact_rel_oid: pg_sys::Oid,
    date_rel_oid: pg_sys::Oid,
    part_rel_oid: pg_sys::Oid,
    supplier_rel_oid: pg_sys::Oid,
    f: impl FnOnce(&SsbmQ1ResidentCache) -> R,
) -> Option<R> {
    process_relcache_invalidations();
    SSBM_Q1_CACHE.with(|slot| {
        let borrow = slot.borrow();
        let cache = borrow.as_ref()?;
        (cache.fact_rel_oid() == fact_rel_oid
            && cache.date_rel_oid() == date_rel_oid
            && cache.part_rel_oid() == part_rel_oid
            && cache.supplier_rel_oid() == supplier_rel_oid)
            .then(|| f(cache))
    })
}

pub fn with_ssbm_q3_cache<R>(
    fact_rel_oid: pg_sys::Oid,
    date_rel_oid: pg_sys::Oid,
    customer_rel_oid: pg_sys::Oid,
    supplier_rel_oid: pg_sys::Oid,
    f: impl FnOnce(&SsbmQ1ResidentCache) -> R,
) -> Option<R> {
    process_relcache_invalidations();
    SSBM_Q1_CACHE.with(|slot| {
        let borrow = slot.borrow();
        let cache = borrow.as_ref()?;
        (cache.fact_rel_oid() == fact_rel_oid
            && cache.date_rel_oid() == date_rel_oid
            && cache.customer_rel_oid() == customer_rel_oid
            && cache.supplier_rel_oid() == supplier_rel_oid)
            .then(|| f(cache))
    })
}

pub fn with_ssbm_q4_cache<R>(
    fact_rel_oid: pg_sys::Oid,
    date_rel_oid: pg_sys::Oid,
    part_rel_oid: pg_sys::Oid,
    customer_rel_oid: pg_sys::Oid,
    supplier_rel_oid: pg_sys::Oid,
    f: impl FnOnce(&SsbmQ1ResidentCache) -> R,
) -> Option<R> {
    process_relcache_invalidations();
    SSBM_Q1_CACHE.with(|slot| {
        let borrow = slot.borrow();
        let cache = borrow.as_ref()?;
        (cache.fact_rel_oid() == fact_rel_oid
            && cache.date_rel_oid() == date_rel_oid
            && cache.part_rel_oid() == part_rel_oid
            && cache.customer_rel_oid() == customer_rel_oid
            && cache.supplier_rel_oid() == supplier_rel_oid)
            .then(|| f(cache))
    })
}

fn relation_oid(regclass: &str) -> Result<pg_sys::Oid, String> {
    let escaped = regclass.replace('\'', "''");
    let query = format!("SELECT '{escaped}'::regclass::oid::int8");
    let oid_i64 = Spi::get_one::<i64>(&query)
        .map_err(|err| format!("failed to resolve {regclass}: {err:?}"))?
        .ok_or_else(|| format!("{regclass} resolved to NULL"))?;
    let oid_u32 = u32::try_from(oid_i64).map_err(|_| format!("{regclass} OID out of range"))?;
    Ok(pg_sys::Oid::from(oid_u32))
}

fn relation_qualified_name(rel_oid: pg_sys::Oid, table: &str) -> Result<String, String> {
    let query = format!(
        "SELECT pg_catalog.quote_ident(n.nspname) || '.' || pg_catalog.quote_ident(c.relname) \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.oid = {}::oid",
        u32::from(rel_oid)
    );
    Spi::get_one::<String>(&query)
        .map_err(|err| format!("failed to quote relation {table}: {err:?}"))?
        .ok_or_else(|| format!("{table} relation name not found"))
}

fn relation_attno(rel_oid: pg_sys::Oid, table: &str, column: &str) -> Result<i32, String> {
    let escaped_column = column.replace('\'', "''");
    let query = format!(
        "SELECT attnum::int4 \
         FROM pg_catalog.pg_attribute \
         WHERE attrelid = {}::oid \
           AND attname = '{}' \
           AND attnum > 0 \
           AND NOT attisdropped",
        u32::from(rel_oid),
        escaped_column,
    );
    Spi::get_one::<i32>(&query)
        .map_err(|err| format!("failed to resolve {table}.{column} attnum: {err:?}"))?
        .ok_or_else(|| format!("{table}.{column} attnum not found"))
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn alloc_and_copy_i32(values: &[i32], label: &str) -> Result<ExprDeviceBuffer<i32>, String> {
    ExprDeviceBuffer::copy_from_slice(values)
        .ok_or_else(|| format!("device allocation/copy failed for {label}"))
}

fn alloc_and_copy_u8(values: &[u8], label: &str) -> Result<ExprDeviceBuffer<u8>, String> {
    ExprDeviceBuffer::copy_from_slice(values)
        .ok_or_else(|| format!("device allocation/copy failed for {label}"))
}

fn alloc_and_copy_f64(values: &[f64], label: &str) -> Result<ExprDeviceBuffer<f64>, String> {
    ExprDeviceBuffer::copy_from_slice(values)
        .ok_or_else(|| format!("device allocation/copy failed for {label}"))
}

fn alloc_and_copy_i64(values: &[i64], label: &str) -> Result<ExprDeviceBuffer<i64>, String> {
    ExprDeviceBuffer::copy_from_slice(values)
        .ok_or_else(|| format!("device allocation/copy failed for {label}"))
}

#[allow(dead_code)] // reason: retained for future fully-GPU H3 lat/lng kernels; exact-key caches use i64 cells today.
fn alloc_and_copy_f32(values: &[f32], label: &str) -> Result<ExprDeviceBuffer<f32>, String> {
    ExprDeviceBuffer::copy_from_slice(values)
        .ok_or_else(|| format!("device allocation/copy failed for {label}"))
}

fn alloc_and_copy_u64(values: &[u64], label: &str) -> Result<ExprDeviceBuffer<u64>, String> {
    ExprDeviceBuffer::copy_from_slice(values)
        .ok_or_else(|| format!("device allocation/copy failed for {label}"))
}

fn alloc_device_i32(len: usize, label: &str) -> Result<ExprDeviceBuffer<i32>, String> {
    ExprDeviceBuffer::new(len).ok_or_else(|| format!("device allocation failed for {label}"))
}

fn alloc_device_i64(len: usize, label: &str) -> Result<ExprDeviceBuffer<i64>, String> {
    ExprDeviceBuffer::new(len).ok_or_else(|| format!("device allocation failed for {label}"))
}

fn alloc_device_f64(len: usize, label: &str) -> Result<ExprDeviceBuffer<f64>, String> {
    ExprDeviceBuffer::new(len).ok_or_else(|| format!("device allocation failed for {label}"))
}

fn alloc_device_u32(len: usize, label: &str) -> Result<ExprDeviceBuffer<u32>, String> {
    ExprDeviceBuffer::new(len).ok_or_else(|| format!("device allocation failed for {label}"))
}

const RESIDENT_DENSE_GROUP_BLOCKED_MIN_ROWS: usize = 262_144;
const RESIDENT_DENSE_GROUP_ONE_PASS_MIN_ROWS: usize = 8_192;
// Twin of `RESIDENT_DENSE_GROUPED_F64_PREDICATE_WIDE_ENABLED` in
// `executor/olap.rs`: the predicate-wide kernel lane is disabled (previously
// hidden behind a `usize::MAX` min-rows threshold — see TODO-REVIEW.md P1),
// so its scratch sizing is disabled here in lockstep.
const RESIDENT_DENSE_GROUP_PREDICATE_WIDE_ENABLED: bool = false;
const RESIDENT_DENSE_GROUP_PREDICATE_WIDE_MAX_GROUPS: usize = 256;
const RESIDENT_DENSE_GROUP_SIMPLE_WIDE_MIN_ROWS: usize = 8_192;
const RESIDENT_DENSE_GROUP_SIMPLE_WIDE_MAX_ROWS: usize = 262_144;
const RESIDENT_DENSE_GROUP_SIMPLE_WIDE_HIGH_GROUP_MAX_ROWS: usize = 65_536;
const RESIDENT_DENSE_GROUP_SIMPLE_WIDE_HIGH_GROUP_MIN_GROUPS: usize = 2_049;
const RESIDENT_DENSE_GROUP_SIMPLE_WIDE_MAX_GROUPS: usize = 16_384;
const RESIDENT_DENSE_GROUP_SIMPLE_WIDE_BLOCK_ROWS: usize = 16_384;
const RESIDENT_DENSE_GROUP_SORT_MIN_GROUPS: usize = 512;
const RESIDENT_DENSE_GROUP_ONE_PASS_MAX_GROUPS: usize = 256;
const RESIDENT_DENSE_GROUP_PREDICATE_WIDE_BLOCK_ROWS: usize = 1024;
const RESIDENT_DENSE_GROUP_ONE_PASS_BLOCK_ROWS: usize = 8192;
const RESIDENT_DENSE_GROUP_BLOCK_ROWS: usize = 4096;

fn resident_groupagg_partial_capacity(
    row_count: usize,
    group_capacity: usize,
) -> Result<usize, String> {
    let may_use_one_pass = row_count >= RESIDENT_DENSE_GROUP_ONE_PASS_MIN_ROWS
        && group_capacity <= RESIDENT_DENSE_GROUP_ONE_PASS_MAX_GROUPS;
    let may_use_predicate_wide = RESIDENT_DENSE_GROUP_PREDICATE_WIDE_ENABLED
        && group_capacity <= RESIDENT_DENSE_GROUP_PREDICATE_WIDE_MAX_GROUPS;
    let may_use_simple_wide = row_count >= RESIDENT_DENSE_GROUP_SIMPLE_WIDE_MIN_ROWS
        && group_capacity <= RESIDENT_DENSE_GROUP_SIMPLE_WIDE_MAX_GROUPS
        && if group_capacity >= RESIDENT_DENSE_GROUP_SIMPLE_WIDE_HIGH_GROUP_MIN_GROUPS {
            row_count < RESIDENT_DENSE_GROUP_SIMPLE_WIDE_HIGH_GROUP_MAX_ROWS
        } else {
            row_count < RESIDENT_DENSE_GROUP_SIMPLE_WIDE_MAX_ROWS
        };
    let may_use_blocked = row_count >= RESIDENT_DENSE_GROUP_BLOCKED_MIN_ROWS
        && group_capacity < RESIDENT_DENSE_GROUP_SORT_MIN_GROUPS;
    if !may_use_one_pass && !may_use_predicate_wide && !may_use_simple_wide && !may_use_blocked {
        return Ok(0);
    }
    let block_rows = if may_use_predicate_wide {
        RESIDENT_DENSE_GROUP_PREDICATE_WIDE_BLOCK_ROWS
    } else if may_use_simple_wide {
        RESIDENT_DENSE_GROUP_SIMPLE_WIDE_BLOCK_ROWS
    } else if may_use_one_pass {
        RESIDENT_DENSE_GROUP_ONE_PASS_BLOCK_ROWS
    } else {
        RESIDENT_DENSE_GROUP_BLOCK_ROWS
    };
    let row_block_count =
        row_count / block_rows + usize::from(!row_count.is_multiple_of(block_rows));
    row_block_count
        .checked_mul(group_capacity)
        .ok_or_else(|| "resident groupagg partial scratch capacity overflow".to_string())
}

fn dimension_key_count(keys: impl Iterator<Item = i32>, label: &str) -> Result<usize, String> {
    let max_key = keys
        .filter_map(|key| usize::try_from(key).ok())
        .max()
        .ok_or_else(|| format!("{label} has no non-negative keys"))?;
    max_key
        .checked_add(1)
        .filter(|count| *count > 0)
        .ok_or_else(|| format!("{label} key map size overflow"))
}

fn build_date_year_lookup(rows: &[SsbmDateRow]) -> Result<(i32, Vec<i32>, i32, i32), String> {
    let date_key_min = rows
        .iter()
        .map(|row| row.datekey)
        .min()
        .ok_or("ssbm_date has no date keys")?;
    let date_key_max = rows
        .iter()
        .map(|row| row.datekey)
        .max()
        .ok_or("ssbm_date has no date keys")?;
    let date_span = i64::from(date_key_max) - i64::from(date_key_min) + 1;
    let date_count = usize::try_from(date_span)
        .ok()
        .filter(|count| *count > 0)
        .ok_or("ssbm_date key lookup size overflow")?;
    let mut date_year_by_offset = vec![0i32; date_count];
    for row in rows {
        let offset = usize::try_from(i64::from(row.datekey) - i64::from(date_key_min))
            .map_err(|_| "ssbm_date key offset underflow")?;
        if offset < date_year_by_offset.len() {
            date_year_by_offset[offset] = row.year;
        }
    }

    let year_min = rows
        .iter()
        .map(|row| row.year)
        .min()
        .ok_or("ssbm_date has no years")?;
    let year_max = rows
        .iter()
        .map(|row| row.year)
        .max()
        .ok_or("ssbm_date has no years")?;
    let year_span = i64::from(year_max) - i64::from(year_min) + 1;
    let year_count = i32::try_from(year_span)
        .ok()
        .filter(|count| *count > 0)
        .ok_or("ssbm_date year lookup size overflow")?;
    Ok((date_key_min, date_year_by_offset, year_min, year_count))
}

fn build_part_brand_lookup(rows: &[SsbmPartRow]) -> Result<(Vec<String>, Vec<i32>), String> {
    let part_key_count =
        dimension_key_count(rows.iter().map(|row| row.partkey), "ssbm_part.p_partkey")?;
    let brand_values: Vec<String> = rows
        .iter()
        .map(|row| row.brand1.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if brand_values.is_empty() {
        return Err("ssbm_part has no brand values".to_owned());
    }
    let brand_index: BTreeMap<&str, i32> = brand_values
        .iter()
        .enumerate()
        .map(|(idx, brand)| {
            i32::try_from(idx)
                .map(|code| (brand.as_str(), code))
                .map_err(|_| "too many SSBM part brands for int32 code space")
        })
        .collect::<Result<_, _>>()?;

    let mut part_brand_code_by_key = vec![-1i32; part_key_count];
    for row in rows {
        let Ok(key) = usize::try_from(row.partkey) else {
            continue;
        };
        if key < part_brand_code_by_key.len() {
            part_brand_code_by_key[key] = *brand_index
                .get(row.brand1.as_str())
                .ok_or("missing SSBM part brand code")?;
        }
    }
    Ok((brand_values, part_brand_code_by_key))
}

// Loads a resident grouped-aggregate cache; the column names, key source,
// measure op, and nullability are all distinct required inputs to the load.
#[allow(clippy::too_many_arguments)]
fn load_resident_groupagg_cache(
    table: &str,
    group_col: &str,
    group_key_source: ResidentGroupKeySource,
    value_col: &str,
    value_rhs_col: Option<&str>,
    measure_op: ResidentMeasureOp,
    filter_col: Option<&str>,
    allow_nullable_f64: bool,
) -> Result<ResidentDenseGroupAggCache, String> {
    let rel_oid = relation_oid(table)?;
    let qualified_table = relation_qualified_name(rel_oid, table)?;
    let group_attno = relation_attno(rel_oid, table, group_col)?;
    let value_attno = relation_attno(rel_oid, table, value_col)?;
    let value_rhs_attno = value_rhs_col
        .map(|col| relation_attno(rel_oid, table, col))
        .transpose()?;
    let filter_attno = filter_col
        .map(|col| relation_attno(rel_oid, table, col))
        .transpose()?;
    if measure_op != ResidentMeasureOp::Column && value_rhs_col.is_none() {
        return Err(format!(
            "{table}.{value_col} expression measure requires a right-hand column"
        ));
    }
    let mut select_cols = vec![group_col, value_col];
    if let Some(rhs_col) = value_rhs_col {
        select_cols.push(rhs_col);
    }
    if let Some(filter_col) = filter_col {
        select_cols.push(filter_col);
    }
    let quoted_cols = select_cols
        .iter()
        .map(|col| quote_identifier(col))
        .collect::<Vec<_>>();
    let query = format!("SELECT {} FROM {qualified_table}", quoted_cols.join(", "));

    let (group_keys, values, value_nulls, value_rhs, value_rhs_nulls, filter) =
        crate::engine::ffi::planner_hooks::with_planner_hooks_suspended(|| {
            Spi::connect(|client| {
                let table_rows = client
                    .select(&query, None, &[])
                    .map_err(|err| format!("failed to scan {table}: {err:?}"))?;

                let mut group_keys = Vec::new();
                let mut values = Vec::new();
                let mut value_nulls = Vec::new();
                let mut saw_value_null = false;
                let mut value_rhs = value_rhs_col.map(|_| Vec::new());
                let mut value_rhs_nulls = value_rhs_col.map(|_| Vec::new());
                let mut saw_rhs_null = false;
                let mut filter = filter_col.map(|_| Vec::new());
                let mut scanned_rows: usize = 0;
                for row in table_rows {
                    scanned_rows += 1;
                    if scanned_rows.is_multiple_of(LOAD_INTERRUPT_CHECK_ROWS) {
                        pgrx::check_for_interrupts!();
                    }
                    let group_key = match group_key_source {
                        ResidentGroupKeySource::Int4 => ResidentGroupKeyValue::Int4(
                            row.get::<i32>(1)
                                .map_err(|err| format!("{group_col} read failed: {err:?}"))?
                                .ok_or_else(|| format!("{group_col} is NULL"))?,
                        ),
                        ResidentGroupKeySource::Text => ResidentGroupKeyValue::Text(
                            row.get::<String>(1)
                                .map_err(|err| format!("{group_col} read failed: {err:?}"))?
                                .ok_or_else(|| format!("{group_col} is NULL"))?,
                        ),
                    };
                    group_keys.push(group_key);
                    let value = read_nullable_resident_f64(&row, 2, value_col)?;
                    if value.is_null && !allow_nullable_f64 {
                        return Err(format!("{value_col} is NULL"));
                    }
                    values.push(value.value);
                    value_nulls.push(u8::from(value.is_null));
                    saw_value_null |= value.is_null;
                    let mut next_idx = 3;
                    if let (Some(rhs_values), Some(rhs_nulls)) =
                        (value_rhs.as_mut(), value_rhs_nulls.as_mut())
                    {
                        let rhs_col = value_rhs_col.unwrap_or("<rhs>");
                        let rhs_value = read_nullable_resident_f64(&row, next_idx, rhs_col)?;
                        if rhs_value.is_null && !allow_nullable_f64 {
                            return Err(format!("{rhs_col} is NULL"));
                        }
                        rhs_values.push(rhs_value.value);
                        rhs_nulls.push(u8::from(rhs_value.is_null));
                        saw_rhs_null |= rhs_value.is_null;
                        next_idx += 1;
                    }
                    if let Some(filter_values) = filter.as_mut() {
                        let filter_name = filter_col.unwrap_or("<filter>");
                        let selected = row
                            .get::<bool>(next_idx)
                            .map_err(|err| format!("{filter_name} read failed: {err:?}"))?
                            .ok_or_else(|| format!("{filter_name} is NULL"))?;
                        filter_values.push(u8::from(selected));
                    }
                }
                Ok::<_, String>((
                    group_keys,
                    values,
                    saw_value_null.then_some(value_nulls),
                    value_rhs,
                    saw_rhs_null.then_some(value_rhs_nulls.unwrap_or_default()),
                    filter,
                ))
            })
        })?;

    if group_keys.is_empty() {
        return Err(format!(
            "{table} has no rows; refusing resident groupagg cache"
        ));
    }
    let loaded_groups = encode_resident_group_keys(group_keys, group_key_source, table, group_col)?;
    let LoadedResidentGroupKeys {
        codes,
        min,
        count,
        output,
    } = loaded_groups;
    if codes.len() != values.len() {
        return Err(format!(
            "{table}.{group_col} encoded {} group keys for {} values",
            codes.len(),
            values.len()
        ));
    }
    if let Some(rhs_values) = value_rhs.as_ref()
        && rhs_values.len() != values.len()
    {
        return Err(format!(
            "{table}.{group_col} encoded {} group keys for {} rhs values",
            codes.len(),
            rhs_values.len()
        ));
    }
    let group_capacity = usize::try_from(count)
        .map_err(|_| format!("{table}.{group_col} group count is negative"))?;
    if group_capacity == 0 {
        return Err(format!("{table}.{group_col} has empty group domain"));
    }
    let row_count = codes.len();
    ensure_resident_row_count_fits_u32(row_count, table)?;
    let partial_capacity = resident_groupagg_partial_capacity(row_count, group_capacity)?;
    let (
        filtered_row_count,
        filtered_group_keys,
        filtered_values,
        filtered_value_nulls,
        filtered_value_rhs,
        filtered_value_rhs_nulls,
    ) = if let Some(filter_values) = filter.as_ref() {
        let selected_rows: Vec<usize> = filter_values
            .iter()
            .enumerate()
            .filter_map(|(idx, selected)| (*selected != 0).then_some(idx))
            .collect();
        if selected_rows.is_empty() {
            (0, None, None, None, None, None)
        } else {
            let compact_codes: Vec<i32> = selected_rows.iter().map(|&idx| codes[idx]).collect();
            let compact_values: Vec<f64> = selected_rows.iter().map(|&idx| values[idx]).collect();
            let compact_value_nulls = value_nulls.as_ref().map(|nulls| {
                selected_rows
                    .iter()
                    .map(|&idx| nulls[idx])
                    .collect::<Vec<_>>()
            });
            let compact_rhs = value_rhs.as_ref().map(|rhs_values| {
                selected_rows
                    .iter()
                    .map(|&idx| rhs_values[idx])
                    .collect::<Vec<_>>()
            });
            let compact_rhs_nulls = value_rhs_nulls.as_ref().map(|nulls| {
                selected_rows
                    .iter()
                    .map(|&idx| nulls[idx])
                    .collect::<Vec<_>>()
            });
            (
                selected_rows.len(),
                Some(alloc_and_copy_i32(
                    &compact_codes,
                    &format!("{group_col}_filtered"),
                )?),
                Some(alloc_and_copy_f64(
                    &compact_values,
                    &format!("{value_col}_filtered"),
                )?),
                compact_value_nulls
                    .as_deref()
                    .map(|nulls| alloc_and_copy_u8(nulls, &format!("{value_col}_filtered_nulls")))
                    .transpose()?,
                compact_rhs
                    .as_deref()
                    .map(|rhs| {
                        alloc_and_copy_f64(
                            rhs,
                            &format!("{}_filtered", value_rhs_col.unwrap_or("rhs_value")),
                        )
                    })
                    .transpose()?,
                compact_rhs_nulls
                    .as_deref()
                    .map(|nulls| {
                        alloc_and_copy_u8(
                            nulls,
                            &format!("{}_filtered_nulls", value_rhs_col.unwrap_or("rhs_value")),
                        )
                    })
                    .transpose()?,
            )
        }
    } else {
        (0, None, None, None, None, None)
    };
    Ok(ResidentDenseGroupAggCache {
        rel_oid,
        group_attno,
        value_attno,
        value_rhs_attno,
        filter_attno,
        row_count,
        group_min: min,
        group_count: count,
        group_output: output,
        group_keys: alloc_and_copy_i32(&codes, group_col)?,
        measure_op,
        values: alloc_and_copy_f64(&values, value_col)?,
        value_nulls: value_nulls
            .as_deref()
            .map(|nulls| alloc_and_copy_u8(nulls, &format!("{value_col}_nulls")))
            .transpose()?,
        value_rhs: value_rhs
            .as_deref()
            .map(|values| alloc_and_copy_f64(values, value_rhs_col.unwrap_or("rhs_value")))
            .transpose()?,
        value_rhs_nulls: value_rhs_nulls
            .as_deref()
            .map(|nulls| {
                alloc_and_copy_u8(
                    nulls,
                    &format!("{}_nulls", value_rhs_col.unwrap_or("rhs_value")),
                )
            })
            .transpose()?,
        filter: filter
            .as_deref()
            .map(|values| alloc_and_copy_u8(values, filter_col.unwrap_or("filter")))
            .transpose()?,
        filtered_row_count,
        filtered_group_keys,
        filtered_values,
        filtered_value_nulls,
        filtered_value_rhs,
        filtered_value_rhs_nulls,
        scratch_sum: alloc_device_f64(group_capacity, "resident_groupagg_sum_scratch")?,
        scratch_min: alloc_device_f64(group_capacity, "resident_groupagg_min_scratch")?,
        scratch_max: alloc_device_f64(group_capacity, "resident_groupagg_max_scratch")?,
        scratch_count: alloc_device_u32(group_capacity, "resident_groupagg_count_scratch")?,
        scratch_group_start: alloc_device_u32(
            group_capacity,
            "resident_groupagg_group_start_scratch",
        )?,
        scratch_group_cursor: alloc_device_u32(
            group_capacity,
            "resident_groupagg_group_cursor_scratch",
        )?,
        scratch_sorted_group: alloc_device_i32(
            row_count,
            "resident_groupagg_sorted_group_scratch",
        )?,
        scratch_row_index: alloc_device_u32(row_count, "resident_groupagg_row_index_scratch")?,
        scratch_partial_sum: (partial_capacity > 0)
            .then(|| alloc_device_f64(partial_capacity, "resident_groupagg_partial_sum_scratch"))
            .transpose()?,
        scratch_partial_min: (partial_capacity > 0)
            .then(|| alloc_device_f64(partial_capacity, "resident_groupagg_partial_min_scratch"))
            .transpose()?,
        scratch_partial_max: (partial_capacity > 0)
            .then(|| alloc_device_f64(partial_capacity, "resident_groupagg_partial_max_scratch"))
            .transpose()?,
        scratch_partial_count: (partial_capacity > 0)
            .then(|| alloc_device_u32(partial_capacity, "resident_groupagg_partial_count_scratch"))
            .transpose()?,
    })
}

fn cache_cmp_passes_f64(lhs: f64, opcode: u16, rhs: f64) -> Result<bool, String> {
    use crate::engine::expr_compiler::opcode;

    Ok(match opcode {
        opcode::EQ => lhs == rhs,
        opcode::NE => lhs != rhs,
        opcode::LT => lhs < rhs,
        opcode::LE => lhs <= rhs,
        opcode::GT => lhs > rhs,
        opcode::GE => lhs >= rhs,
        opcode::ALWAYS_TRUE => true,
        _ => return Err(format!("unsupported comparison opcode {opcode}")),
    })
}

#[allow(clippy::too_many_arguments)]
fn load_resident_star_dim_groupagg_cache(
    fact_table: &str,
    fact_key_col: &str,
    fact_value_col: &str,
    fact_value_cmp_opcode: u16,
    fact_value_cmp_const: f64,
    dim_table: &str,
    dim_key_col: &str,
    dim_filter_col: &str,
    dim_filter_cmp_opcode: u16,
    dim_filter_const: f64,
    dim_group_col: &str,
    dim_group_key_source: ResidentGroupKeySource,
    allow_nullable_f64: bool,
) -> Result<ResidentStarDimGroupAggCache, String> {
    if fact_value_cmp_const.is_nan() || dim_filter_const.is_nan() {
        return Err("resident star groupagg predicates cannot compare against NaN".to_owned());
    }
    let dim_filter_is_always_true =
        dim_filter_cmp_opcode == crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
    let fact_rel_oid = relation_oid(fact_table)?;
    let dim_rel_oid = relation_oid(dim_table)?;
    let qualified_fact = relation_qualified_name(fact_rel_oid, fact_table)?;
    let qualified_dim = relation_qualified_name(dim_rel_oid, dim_table)?;
    let fact_key_attno = relation_attno(fact_rel_oid, fact_table, fact_key_col)?;
    let fact_value_attno = relation_attno(fact_rel_oid, fact_table, fact_value_col)?;
    let dim_key_attno = relation_attno(dim_rel_oid, dim_table, dim_key_col)?;
    let dim_filter_attno = relation_attno(dim_rel_oid, dim_table, dim_filter_col)?;
    let dim_group_attno = relation_attno(dim_rel_oid, dim_table, dim_group_col)?;

    let fact_query = format!(
        "SELECT {}, {} FROM {qualified_fact}",
        quote_identifier(fact_key_col),
        quote_identifier(fact_value_col),
    );
    let dim_query = if dim_filter_is_always_true {
        format!(
            "SELECT {}, {} FROM {qualified_dim}",
            quote_identifier(dim_key_col),
            quote_identifier(dim_group_col),
        )
    } else {
        format!(
            "SELECT {}, {}, {} FROM {qualified_dim}",
            quote_identifier(dim_key_col),
            quote_identifier(dim_filter_col),
            quote_identifier(dim_group_col),
        )
    };

    let (fact_keys, fact_key_nulls, values, value_nulls) =
        crate::engine::ffi::planner_hooks::with_planner_hooks_suspended(|| {
            Spi::connect(|client| {
                let rows = client
                    .select(&fact_query, None, &[])
                    .map_err(|err| format!("failed to scan {fact_table}: {err:?}"))?;
                let mut fact_keys = Vec::new();
                let mut fact_key_nulls = Vec::new();
                let mut saw_key_null = false;
                let mut values = Vec::new();
                let mut value_nulls = Vec::new();
                let mut saw_value_null = false;
                let mut scanned_rows: usize = 0;
                for row in rows {
                    scanned_rows += 1;
                    if scanned_rows.is_multiple_of(LOAD_INTERRUPT_CHECK_ROWS) {
                        pgrx::check_for_interrupts!();
                    }
                    if let Some(key) = row
                        .get::<i32>(1)
                        .map_err(|err| format!("{fact_key_col} read failed: {err:?}"))?
                    {
                        fact_keys.push(key);
                        fact_key_nulls.push(0);
                    } else {
                        fact_keys.push(0);
                        fact_key_nulls.push(1);
                        saw_key_null = true;
                    }
                    let value = read_nullable_resident_f64(&row, 2, fact_value_col)?;
                    if value.is_null && !allow_nullable_f64 {
                        return Err(format!("{fact_value_col} is NULL"));
                    }
                    values.push(value.value);
                    value_nulls.push(u8::from(value.is_null));
                    saw_value_null |= value.is_null;
                }
                Ok::<_, String>((
                    fact_keys,
                    saw_key_null.then_some(fact_key_nulls),
                    values,
                    saw_value_null.then_some(value_nulls),
                ))
            })
        })?;

    if fact_keys.is_empty() {
        return Err(format!(
            "{fact_table} has no rows; refusing resident star groupagg cache"
        ));
    }
    if fact_keys.len() != values.len() {
        return Err(format!(
            "{fact_table}.{fact_key_col} loaded {} keys for {} values",
            fact_keys.len(),
            values.len()
        ));
    }

    let (dim_keys, matched_dim_keys, matched_groups) =
        crate::engine::ffi::planner_hooks::with_planner_hooks_suspended(|| {
            Spi::connect(|client| {
                let rows = client
                    .select(&dim_query, None, &[])
                    .map_err(|err| format!("failed to scan {dim_table}: {err:?}"))?;
                let mut dim_keys = Vec::new();
                let mut matched_dim_keys = Vec::new();
                let mut matched_groups = Vec::new();
                let mut scanned_rows: usize = 0;
                for row in rows {
                    scanned_rows += 1;
                    if scanned_rows.is_multiple_of(LOAD_INTERRUPT_CHECK_ROWS) {
                        pgrx::check_for_interrupts!();
                    }
                    let dim_key = row
                        .get::<i32>(1)
                        .map_err(|err| format!("{dim_key_col} read failed: {err:?}"))?
                        .ok_or_else(|| format!("{dim_key_col} is NULL"))?;
                    dim_keys.push(dim_key);
                    let group_idx = if dim_filter_is_always_true {
                        2
                    } else {
                        let filter_value = row
                            .get::<i32>(2)
                            .map_err(|err| format!("{dim_filter_col} read failed: {err:?}"))?
                            .ok_or_else(|| format!("{dim_filter_col} is NULL"))?;
                        if !cache_cmp_passes_f64(
                            f64::from(filter_value),
                            dim_filter_cmp_opcode,
                            dim_filter_const,
                        )? {
                            continue;
                        }
                        3
                    };
                    let group = match dim_group_key_source {
                        ResidentGroupKeySource::Int4 => ResidentGroupKeyValue::Int4(
                            row.get::<i32>(group_idx)
                                .map_err(|err| format!("{dim_group_col} read failed: {err:?}"))?
                                .ok_or_else(|| format!("{dim_group_col} is NULL"))?,
                        ),
                        ResidentGroupKeySource::Text => ResidentGroupKeyValue::Text(
                            row.get::<String>(group_idx)
                                .map_err(|err| format!("{dim_group_col} read failed: {err:?}"))?
                                .ok_or_else(|| format!("{dim_group_col} is NULL"))?,
                        ),
                    };
                    matched_dim_keys.push(dim_key);
                    matched_groups.push(group);
                }
                Ok::<_, String>((dim_keys, matched_dim_keys, matched_groups))
            })
        })?;

    if matched_dim_keys.is_empty() {
        return Err(format!(
            "{dim_table}.{dim_filter_col} predicate produced no resident dimension groups"
        ));
    }
    let dim_key_count = dimension_key_count(dim_keys.iter().copied(), dim_key_col)?;
    let loaded_groups = encode_resident_group_keys(
        matched_groups,
        dim_group_key_source,
        dim_table,
        dim_group_col,
    )?;
    let LoadedResidentGroupKeys {
        codes,
        min,
        count,
        output,
    } = loaded_groups;
    if min != 0 {
        return Err(format!(
            "{dim_table}.{dim_group_col} star group codes must be zero-based"
        ));
    }
    if codes.len() != matched_dim_keys.len() {
        return Err(format!(
            "{dim_table}.{dim_group_col} encoded {} groups for {} matched keys",
            codes.len(),
            matched_dim_keys.len()
        ));
    }
    let mut dim_match_by_key = vec![0u8; dim_key_count];
    let mut dim_group_code_by_key = vec![-1i32; dim_key_count];
    for (dim_key, group_code) in matched_dim_keys.iter().copied().zip(codes.iter().copied()) {
        let Ok(key_idx) = usize::try_from(dim_key) else {
            continue;
        };
        if key_idx >= dim_key_count {
            continue;
        }
        if dim_match_by_key[key_idx] != 0 {
            return Err(format!(
                "{dim_table}.{dim_key_col} has duplicate key {dim_key}"
            ));
        }
        dim_match_by_key[key_idx] = 1;
        dim_group_code_by_key[key_idx] = group_code;
    }

    let group_capacity =
        usize::try_from(count).map_err(|_| format!("{dim_group_col} group count is negative"))?;
    if group_capacity == 0 {
        return Err(format!(
            "{dim_table}.{dim_group_col} has empty group domain"
        ));
    }
    let row_count = fact_keys.len();
    ensure_resident_row_count_fits_u32(row_count, fact_table)?;
    let partial_capacity = resident_groupagg_partial_capacity(row_count, group_capacity)?;

    Ok(ResidentStarDimGroupAggCache {
        fact_rel_oid,
        dim_rel_oid,
        fact_key_attno,
        fact_value_attno,
        dim_key_attno,
        dim_group_attno,
        dim_filter_attno,
        fact_value_cmp_opcode,
        fact_value_cmp_const,
        dim_filter_cmp_opcode,
        dim_filter_cmp_const: dim_filter_const,
        row_count,
        group_count: count,
        group_output: output,
        fact_keys: alloc_and_copy_i32(&fact_keys, fact_key_col)?,
        fact_key_nulls: fact_key_nulls
            .as_deref()
            .map(|nulls| alloc_and_copy_u8(nulls, &format!("{fact_key_col}_nulls")))
            .transpose()?,
        values: alloc_and_copy_f64(&values, fact_value_col)?,
        value_nulls: value_nulls
            .as_deref()
            .map(|nulls| alloc_and_copy_u8(nulls, &format!("{fact_value_col}_nulls")))
            .transpose()?,
        dim_match_by_key: alloc_and_copy_u8(&dim_match_by_key, dim_filter_col)?,
        dim_group_code_by_key: alloc_and_copy_i32(&dim_group_code_by_key, dim_group_col)?,
        dim_key_count,
        projected_group_keys: alloc_device_i32(row_count, "resident_star_projected_group_keys")?,
        projected_values: alloc_device_f64(row_count, "resident_star_projected_values")?,
        scratch_sum: alloc_device_f64(group_capacity, "resident_star_groupagg_sum_scratch")?,
        scratch_min: alloc_device_f64(group_capacity, "resident_star_groupagg_min_scratch")?,
        scratch_max: alloc_device_f64(group_capacity, "resident_star_groupagg_max_scratch")?,
        scratch_count: alloc_device_u32(group_capacity, "resident_star_groupagg_count_scratch")?,
        scratch_group_start: alloc_device_u32(
            group_capacity,
            "resident_star_groupagg_group_start_scratch",
        )?,
        scratch_group_cursor: alloc_device_u32(
            group_capacity,
            "resident_star_groupagg_group_cursor_scratch",
        )?,
        scratch_sorted_group: alloc_device_i32(
            row_count,
            "resident_star_groupagg_sorted_group_scratch",
        )?,
        scratch_row_index: alloc_device_u32(row_count, "resident_star_groupagg_row_index_scratch")?,
        scratch_partial_sum: (partial_capacity > 0)
            .then(|| alloc_device_f64(partial_capacity, "resident_star_groupagg_partial_sum"))
            .transpose()?,
        scratch_partial_min: (partial_capacity > 0)
            .then(|| alloc_device_f64(partial_capacity, "resident_star_groupagg_partial_min"))
            .transpose()?,
        scratch_partial_max: (partial_capacity > 0)
            .then(|| alloc_device_f64(partial_capacity, "resident_star_groupagg_partial_max"))
            .transpose()?,
        scratch_partial_count: (partial_capacity > 0)
            .then(|| alloc_device_u32(partial_capacity, "resident_star_groupagg_partial_count"))
            .transpose()?,
    })
}

fn load_resident_f64_reduce_cache(
    table: &str,
    value_col: &str,
    allow_nullable_f64: bool,
) -> Result<ResidentDenseGroupAggCache, String> {
    let rel_oid = relation_oid(table)?;
    let qualified_table = relation_qualified_name(rel_oid, table)?;
    let value_attno = relation_attno(rel_oid, table, value_col)?;
    let query = format!(
        "SELECT {} FROM {qualified_table}",
        quote_identifier(value_col)
    );

    let (values, value_nulls) =
        crate::engine::ffi::planner_hooks::with_planner_hooks_suspended(|| {
            Spi::connect(|client| {
                let table_rows = client
                    .select(&query, None, &[])
                    .map_err(|err| format!("failed to scan {table}: {err:?}"))?;

                let mut values = Vec::new();
                let mut value_nulls = Vec::new();
                let mut saw_value_null = false;
                let mut scanned_rows: usize = 0;
                for row in table_rows {
                    scanned_rows += 1;
                    if scanned_rows.is_multiple_of(LOAD_INTERRUPT_CHECK_ROWS) {
                        pgrx::check_for_interrupts!();
                    }
                    let value = read_nullable_resident_f64(&row, 1, value_col)?;
                    if value.is_null && !allow_nullable_f64 {
                        return Err(format!("{value_col} is NULL"));
                    }
                    values.push(value.value);
                    value_nulls.push(u8::from(value.is_null));
                    saw_value_null |= value.is_null;
                }
                Ok::<_, String>((values, saw_value_null.then_some(value_nulls)))
            })
        })?;

    if values.is_empty() {
        return Err(format!(
            "{table} has no rows; refusing resident f64 reduce cache"
        ));
    }

    let row_count = values.len();
    ensure_resident_row_count_fits_u32(row_count, table)?;
    let group_capacity = 1usize;
    let partial_capacity = resident_groupagg_partial_capacity(row_count, group_capacity)?;
    let group_keys = vec![0i32; row_count];

    Ok(ResidentDenseGroupAggCache {
        rel_oid,
        group_attno: 0,
        value_attno,
        value_rhs_attno: None,
        filter_attno: None,
        row_count,
        group_min: 0,
        group_count: 1,
        group_output: ResidentGroupKeyOutput::Int4Dense { min: 0 },
        group_keys: alloc_and_copy_i32(&group_keys, "resident_f64_reduce_single_group")?,
        measure_op: ResidentMeasureOp::Column,
        values: alloc_and_copy_f64(&values, value_col)?,
        value_nulls: value_nulls
            .as_deref()
            .map(|nulls| alloc_and_copy_u8(nulls, &format!("{value_col}_nulls")))
            .transpose()?,
        value_rhs: None,
        value_rhs_nulls: None,
        filter: None,
        filtered_row_count: 0,
        filtered_group_keys: None,
        filtered_values: None,
        filtered_value_nulls: None,
        filtered_value_rhs: None,
        filtered_value_rhs_nulls: None,
        scratch_sum: alloc_device_f64(group_capacity, "resident_f64_reduce_sum_scratch")?,
        scratch_min: alloc_device_f64(group_capacity, "resident_f64_reduce_min_scratch")?,
        scratch_max: alloc_device_f64(group_capacity, "resident_f64_reduce_max_scratch")?,
        scratch_count: alloc_device_u32(group_capacity, "resident_f64_reduce_count_scratch")?,
        scratch_group_start: alloc_device_u32(
            group_capacity,
            "resident_f64_reduce_group_start_scratch",
        )?,
        scratch_group_cursor: alloc_device_u32(
            group_capacity,
            "resident_f64_reduce_group_cursor_scratch",
        )?,
        scratch_sorted_group: alloc_device_i32(row_count, "resident_f64_reduce_sorted_group")?,
        scratch_row_index: alloc_device_u32(row_count, "resident_f64_reduce_row_index")?,
        scratch_partial_sum: (partial_capacity > 0)
            .then(|| alloc_device_f64(partial_capacity, "resident_f64_reduce_partial_sum"))
            .transpose()?,
        scratch_partial_min: (partial_capacity > 0)
            .then(|| alloc_device_f64(partial_capacity, "resident_f64_reduce_partial_min"))
            .transpose()?,
        scratch_partial_max: (partial_capacity > 0)
            .then(|| alloc_device_f64(partial_capacity, "resident_f64_reduce_partial_max"))
            .transpose()?,
        scratch_partial_count: (partial_capacity > 0)
            .then(|| alloc_device_u32(partial_capacity, "resident_f64_reduce_partial_count"))
            .transpose()?,
    })
}

enum LoadedResidentHashJoinKeys {
    Int32 {
        values: Vec<i32>,
        nulls: Option<Vec<u8>>,
    },
    Int64 {
        values: Vec<i64>,
        nulls: Option<Vec<u8>>,
    },
}

impl LoadedResidentHashJoinKeys {
    #[must_use]
    fn len(&self) -> usize {
        match self {
            Self::Int32 { values, .. } => values.len(),
            Self::Int64 { values, .. } => values.len(),
        }
    }
}

fn parse_resident_hashjoin_key_type(input: &str) -> Result<PgaccelKeyType, String> {
    match input.trim().to_ascii_lowercase().as_str() {
        "int4" | "int32" | "integer" => Ok(PgaccelKeyType::Int32),
        "int8" | "int64" | "bigint" => Ok(PgaccelKeyType::Int64),
        other => Err(format!(
            "unsupported resident hashjoin key type {other}; expected int4 or int8"
        )),
    }
}

fn read_resident_hashjoin_keys(
    table: &str,
    key_col: &str,
    key_type: PgaccelKeyType,
) -> Result<LoadedResidentHashJoinKeys, String> {
    let rel_oid = relation_oid(table)?;
    let qualified = relation_qualified_name(rel_oid, table)?;
    let query = format!("SELECT {} FROM {qualified}", quote_identifier(key_col));

    crate::engine::ffi::planner_hooks::with_planner_hooks_suspended(|| {
        Spi::connect(|client| {
            let table_rows = client
                .select(&query, None, &[])
                .map_err(|err| format!("failed to scan {table}: {err:?}"))?;
            match key_type {
                PgaccelKeyType::Int32 => {
                    let mut values = Vec::new();
                    let mut nulls = Vec::new();
                    let mut saw_null = false;
                    let mut scanned_rows: usize = 0;
                    for row in table_rows {
                        scanned_rows += 1;
                        if scanned_rows.is_multiple_of(LOAD_INTERRUPT_CHECK_ROWS) {
                            pgrx::check_for_interrupts!();
                        }
                        let value = row
                            .get::<i32>(1)
                            .map_err(|err| format!("{table}.{key_col} read failed: {err:?}"))?;
                        if let Some(value) = value {
                            values.push(value);
                            nulls.push(0);
                        } else {
                            values.push(0);
                            nulls.push(1);
                            saw_null = true;
                        }
                    }
                    Ok(LoadedResidentHashJoinKeys::Int32 {
                        values,
                        nulls: saw_null.then_some(nulls),
                    })
                }
                PgaccelKeyType::Int64 => {
                    let mut values = Vec::new();
                    let mut nulls = Vec::new();
                    let mut saw_null = false;
                    let mut scanned_rows: usize = 0;
                    for row in table_rows {
                        scanned_rows += 1;
                        if scanned_rows.is_multiple_of(LOAD_INTERRUPT_CHECK_ROWS) {
                            pgrx::check_for_interrupts!();
                        }
                        let value = row
                            .get::<i64>(1)
                            .map_err(|err| format!("{table}.{key_col} read failed: {err:?}"))?;
                        if let Some(value) = value {
                            values.push(value);
                            nulls.push(0);
                        } else {
                            values.push(0);
                            nulls.push(1);
                            saw_null = true;
                        }
                    }
                    Ok(LoadedResidentHashJoinKeys::Int64 {
                        values,
                        nulls: saw_null.then_some(nulls),
                    })
                }
                PgaccelKeyType::Float64 | PgaccelKeyType::Uuid | PgaccelKeyType::Inet => {
                    Err("resident hashjoin cache supports int4/int8 keys only".to_owned())
                }
            }
        })
    })
}

fn load_resident_hashjoin_count_cache(
    outer_table: &str,
    outer_key_col: &str,
    inner_table: &str,
    inner_key_col: &str,
    key_type: PgaccelKeyType,
) -> Result<ResidentHashJoinCountCache, String> {
    let outer_rel_oid = relation_oid(outer_table)?;
    let inner_rel_oid = relation_oid(inner_table)?;
    let outer_attno = relation_attno(outer_rel_oid, outer_table, outer_key_col)?;
    let inner_attno = relation_attno(inner_rel_oid, inner_table, inner_key_col)?;
    let outer = read_resident_hashjoin_keys(outer_table, outer_key_col, key_type)?;
    let inner = read_resident_hashjoin_keys(inner_table, inner_key_col, key_type)?;
    if outer.len() == 0 {
        return Err(format!(
            "{outer_table} has no rows; refusing resident hashjoin cache"
        ));
    }
    if inner.len() == 0 {
        return Err(format!(
            "{inner_table} has no rows; refusing resident hashjoin cache"
        ));
    }

    let outer_rows = outer.len();
    let inner_rows = inner.len();
    let mut cache = ResidentHashJoinCountCache {
        outer_rel_oid,
        inner_rel_oid,
        outer_attno,
        inner_attno,
        key_type,
        outer_rows,
        inner_rows,
        outer_i32: None,
        inner_i32: None,
        outer_i64: None,
        inner_i64: None,
        outer_nulls: None,
        inner_nulls: None,
    };

    match outer {
        LoadedResidentHashJoinKeys::Int32 { values, nulls } => {
            cache.outer_i32 = Some(alloc_and_copy_i32(&values, outer_key_col)?);
            cache.outer_nulls = nulls
                .as_deref()
                .map(|nulls| alloc_and_copy_u8(nulls, &format!("{outer_key_col}_nulls")))
                .transpose()?;
        }
        LoadedResidentHashJoinKeys::Int64 { values, nulls } => {
            cache.outer_i64 = Some(alloc_and_copy_i64(&values, outer_key_col)?);
            cache.outer_nulls = nulls
                .as_deref()
                .map(|nulls| alloc_and_copy_u8(nulls, &format!("{outer_key_col}_nulls")))
                .transpose()?;
        }
    }

    match inner {
        LoadedResidentHashJoinKeys::Int32 { values, nulls } => {
            cache.inner_i32 = Some(alloc_and_copy_i32(&values, inner_key_col)?);
            cache.inner_nulls = nulls
                .as_deref()
                .map(|nulls| alloc_and_copy_u8(nulls, &format!("{inner_key_col}_nulls")))
                .transpose()?;
        }
        LoadedResidentHashJoinKeys::Int64 { values, nulls } => {
            cache.inner_i64 = Some(alloc_and_copy_i64(&values, inner_key_col)?);
            cache.inner_nulls = nulls
                .as_deref()
                .map(|nulls| alloc_and_copy_u8(nulls, &format!("{inner_key_col}_nulls")))
                .transpose()?;
        }
    }

    Ok(cache)
}

#[derive(Debug, Clone, Copy)]
struct NullableResidentF64 {
    value: f64,
    is_null: bool,
}

fn read_nullable_resident_f64(
    row: &pgrx::spi::SpiHeapTupleData<'_>,
    idx: usize,
    col: &str,
) -> Result<NullableResidentF64, String> {
    let Some(value) = row
        .get::<f64>(idx)
        .map_err(|err| format!("{col} read failed: {err:?}"))?
    else {
        return Ok(NullableResidentF64 {
            value: 0.0,
            is_null: true,
        });
    };
    if value.is_nan() {
        return Err(format!(
            "{col} is NaN; resident f64 groupagg cache requires PostgreSQL-comparable values"
        ));
    }
    Ok(NullableResidentF64 {
        value,
        is_null: false,
    })
}

fn encode_resident_group_keys(
    group_keys: Vec<ResidentGroupKeyValue>,
    source: ResidentGroupKeySource,
    table: &str,
    group_col: &str,
) -> Result<LoadedResidentGroupKeys, String> {
    match source {
        ResidentGroupKeySource::Int4 => {
            let keys = group_keys
                .into_iter()
                .map(|key| match key {
                    ResidentGroupKeyValue::Int4(value) => Ok(value),
                    ResidentGroupKeyValue::Text(_) => Err(format!(
                        "{table}.{group_col} produced text keys for int4 resident groupagg"
                    )),
                })
                .collect::<Result<Vec<_>, _>>()?;
            encode_resident_int4_group_keys(keys, table, group_col)
        }
        ResidentGroupKeySource::Text => {
            let labels = group_keys
                .into_iter()
                .map(|key| match key {
                    ResidentGroupKeyValue::Text(value) => Ok(value),
                    ResidentGroupKeyValue::Int4(_) => Err(format!(
                        "{table}.{group_col} produced int4 keys for text resident groupagg"
                    )),
                })
                .collect::<Result<Vec<_>, _>>()?;
            encode_resident_text_group_keys(labels, table, group_col)
        }
    }
}

fn encode_resident_int4_group_keys(
    keys: Vec<i32>,
    table: &str,
    group_col: &str,
) -> Result<LoadedResidentGroupKeys, String> {
    let key_min = *keys
        .iter()
        .min()
        .ok_or_else(|| format!("{table}.{group_col} has no group keys"))?;
    let key_max = *keys
        .iter()
        .max()
        .ok_or_else(|| format!("{table}.{group_col} has no group keys"))?;
    if key_max < key_min {
        return Err(format!("{table}.{group_col} has invalid group key range"));
    }
    let range_i64 = i64::from(key_max) - i64::from(key_min) + 1;
    let range = usize::try_from(range_i64)
        .map_err(|_| format!("{table}.{group_col} group range is too large"))?;
    let distinct: Vec<i32> = keys
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if range == distinct.len() {
        let count =
            i32::try_from(range).map_err(|_| format!("{table}.{group_col} has too many groups"))?;
        return Ok(LoadedResidentGroupKeys {
            codes: keys,
            min: key_min,
            count,
            output: ResidentGroupKeyOutput::Int4Dense { min: key_min },
        });
    }

    let code_by_key: BTreeMap<i32, i32> = distinct
        .iter()
        .copied()
        .enumerate()
        .map(|(idx, key)| {
            i32::try_from(idx)
                .map(|code| (key, code))
                .map_err(|_| format!("{table}.{group_col} has too many groups"))
        })
        .collect::<Result<_, _>>()?;
    let codes = keys
        .iter()
        .map(|key| {
            code_by_key
                .get(key)
                .copied()
                .ok_or_else(|| format!("{table}.{group_col} missing dictionary code"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let count = i32::try_from(distinct.len())
        .map_err(|_| format!("{table}.{group_col} has too many groups"))?;
    Ok(LoadedResidentGroupKeys {
        codes,
        min: 0,
        count,
        output: ResidentGroupKeyOutput::Int4Dictionary { keys: distinct },
    })
}

fn encode_resident_text_group_keys(
    labels: Vec<String>,
    table: &str,
    group_col: &str,
) -> Result<LoadedResidentGroupKeys, String> {
    let distinct: Vec<String> = labels
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if distinct.is_empty() {
        return Err(format!("{table}.{group_col} has no group keys"));
    }
    let code_by_label: BTreeMap<String, i32> = distinct
        .iter()
        .cloned()
        .enumerate()
        .map(|(idx, label)| {
            i32::try_from(idx)
                .map(|code| (label, code))
                .map_err(|_| format!("{table}.{group_col} has too many groups"))
        })
        .collect::<Result<_, _>>()?;
    let codes = labels
        .iter()
        .map(|label| {
            code_by_label
                .get(label)
                .copied()
                .ok_or_else(|| format!("{table}.{group_col} missing dictionary code"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let count = i32::try_from(distinct.len())
        .map_err(|_| format!("{table}.{group_col} has too many groups"))?;
    Ok(LoadedResidentGroupKeys {
        codes,
        min: 0,
        count,
        output: ResidentGroupKeyOutput::TextDictionary { labels: distinct },
    })
}

fn load_resident_h3_latlng_groupagg_cache(
    table: &str,
    point_col: &str,
    resolution: i32,
) -> Result<ResidentH3GroupAggCache, String> {
    let rel_oid = relation_oid(table)?;
    if !(0..=15).contains(&resolution) {
        return Err(format!("invalid H3 resolution {resolution} for {table}"));
    }
    let query =
        format!("SELECT public.h3_lat_lng_to_cell({point_col}, {resolution})::bigint FROM {table}");
    let mut cells = crate::engine::ffi::planner_hooks::with_planner_hooks_suspended(|| {
        Spi::connect(|client| {
            let table_rows = client
                .select(&query, None, &[])
                .map_err(|err| format!("failed to scan {table}: {err:?}"))?;

            let mut cells = Vec::new();
            let mut scanned_rows: usize = 0;
            for row in table_rows {
                scanned_rows += 1;
                if scanned_rows.is_multiple_of(LOAD_INTERRUPT_CHECK_ROWS) {
                    pgrx::check_for_interrupts!();
                }
                cells.push(
                    row.get::<i64>(1)
                        .map_err(|err| format!("{point_col} H3 cell read failed: {err:?}"))?
                        .ok_or_else(|| format!("{point_col} H3 cell is NULL"))?,
                );
            }
            Ok::<_, String>(cells)
        })
    })?;

    if cells.is_empty() {
        return Err(format!(
            "{table} has no rows; refusing resident H3 lat/lng cache"
        ));
    }
    cells.sort_unstable();
    let row_count = cells.len();

    Ok(ResidentH3GroupAggCache {
        rel_oid,
        row_count,
        kind: ResidentH3GroupedCountKind::LatLngToCell,
        input: ResidentH3GroupAggInput::LatLngCells {
            resolution,
            cells: alloc_and_copy_i64(&cells, "resident_h3_latlng_cells")?,
        },
    })
}

fn load_resident_h3_parent_groupagg_cache(
    table: &str,
    cell_col: &str,
) -> Result<ResidentH3GroupAggCache, String> {
    let rel_oid = relation_oid(table)?;
    let query = format!("SELECT {cell_col}::bigint FROM {table}");
    let cells = crate::engine::ffi::planner_hooks::with_planner_hooks_suspended(|| {
        Spi::connect(|client| {
            let table_rows = client
                .select(&query, None, &[])
                .map_err(|err| format!("failed to scan {table}: {err:?}"))?;

            let mut cells = Vec::new();
            let mut scanned_rows: usize = 0;
            for row in table_rows {
                scanned_rows += 1;
                if scanned_rows.is_multiple_of(LOAD_INTERRUPT_CHECK_ROWS) {
                    pgrx::check_for_interrupts!();
                }
                let cell = row
                    .get::<i64>(1)
                    .map_err(|err| format!("{cell_col} read failed: {err:?}"))?
                    .ok_or_else(|| format!("{cell_col} is NULL"))?;
                cells.push(
                    u64::try_from(cell)
                        .map_err(|_| format!("{table}.{cell_col} produced negative h3index"))?,
                );
            }
            Ok::<_, String>(cells)
        })
    })?;

    if cells.is_empty() {
        return Err(format!(
            "{table} has no rows; refusing resident H3 parent cache"
        ));
    }
    let row_count = cells.len();
    Ok(ResidentH3GroupAggCache {
        rel_oid,
        row_count,
        kind: ResidentH3GroupedCountKind::CellToParent,
        input: ResidentH3GroupAggInput::Cell {
            cells: alloc_and_copy_u64(&cells, "resident_h3_cells")?,
        },
    })
}

fn load_ssbm_q1_cache() -> Result<SsbmQ1ResidentCache, String> {
    let fact_rel_oid = relation_oid("ssbm_lineorder")?;
    let date_rel_oid = relation_oid("ssbm_date")?;
    let part_rel_oid = relation_oid("ssbm_part")?;
    let customer_rel_oid = relation_oid("ssbm_customer")?;
    let supplier_rel_oid = relation_oid("ssbm_supplier")?;

    let (
        orderdate,
        discount,
        quantity,
        extendedprice,
        partkey,
        custkey,
        suppkey,
        revenue,
        supplycost,
    ) = crate::engine::ffi::planner_hooks::with_planner_hooks_suspended(|| {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT lo_orderdate, lo_discount, lo_quantity, lo_extendedprice, \
                            lo_partkey, lo_custkey, lo_suppkey, lo_revenue, lo_supplycost \
                     FROM ssbm_lineorder",
                    None,
                    &[],
                )
                .map_err(|err| format!("failed to scan ssbm_lineorder: {err:?}"))?;

            let mut orderdate = Vec::new();
            let mut discount = Vec::new();
            let mut quantity = Vec::new();
            let mut extendedprice = Vec::new();
            let mut partkey = Vec::new();
            let mut custkey = Vec::new();
            let mut suppkey = Vec::new();
            let mut revenue = Vec::new();
            let mut supplycost = Vec::new();
            let mut scanned_rows: usize = 0;
            for row in table {
                scanned_rows += 1;
                if scanned_rows.is_multiple_of(LOAD_INTERRUPT_CHECK_ROWS) {
                    pgrx::check_for_interrupts!();
                }
                orderdate.push(
                    row.get::<i32>(1)
                        .map_err(|err| format!("lo_orderdate read failed: {err:?}"))?
                        .ok_or("lo_orderdate is NULL")?,
                );
                discount.push(
                    row.get::<i32>(2)
                        .map_err(|err| format!("lo_discount read failed: {err:?}"))?
                        .ok_or("lo_discount is NULL")?,
                );
                quantity.push(
                    row.get::<i32>(3)
                        .map_err(|err| format!("lo_quantity read failed: {err:?}"))?
                        .ok_or("lo_quantity is NULL")?,
                );
                extendedprice.push(
                    row.get::<i32>(4)
                        .map_err(|err| format!("lo_extendedprice read failed: {err:?}"))?
                        .ok_or("lo_extendedprice is NULL")?,
                );
                partkey.push(
                    row.get::<i32>(5)
                        .map_err(|err| format!("lo_partkey read failed: {err:?}"))?
                        .ok_or("lo_partkey is NULL")?,
                );
                custkey.push(
                    row.get::<i32>(6)
                        .map_err(|err| format!("lo_custkey read failed: {err:?}"))?
                        .ok_or("lo_custkey is NULL")?,
                );
                suppkey.push(
                    row.get::<i32>(7)
                        .map_err(|err| format!("lo_suppkey read failed: {err:?}"))?
                        .ok_or("lo_suppkey is NULL")?,
                );
                revenue.push(
                    row.get::<i32>(8)
                        .map_err(|err| format!("lo_revenue read failed: {err:?}"))?
                        .ok_or("lo_revenue is NULL")?,
                );
                supplycost.push(
                    row.get::<i32>(9)
                        .map_err(|err| format!("lo_supplycost read failed: {err:?}"))?
                        .ok_or("lo_supplycost is NULL")?,
                );
            }
            Ok::<_, String>((
                orderdate,
                discount,
                quantity,
                extendedprice,
                partkey,
                custkey,
                suppkey,
                revenue,
                supplycost,
            ))
        })
    })?;

    if orderdate.is_empty() {
        return Err(
            "ssbm_lineorder has no rows; refusing to create empty resident cache".to_owned(),
        );
    }

    let date_rows = crate::engine::ffi::planner_hooks::with_planner_hooks_suspended(|| {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT d_datekey, d_year, d_yearmonth, d_yearmonthnum, d_weeknuminyear \
                     FROM ssbm_date",
                    None,
                    &[],
                )
                .map_err(|err| format!("failed to scan ssbm_date: {err:?}"))?;

            let mut rows = Vec::new();
            let mut scanned_rows: usize = 0;
            for row in table {
                scanned_rows += 1;
                if scanned_rows.is_multiple_of(LOAD_INTERRUPT_CHECK_ROWS) {
                    pgrx::check_for_interrupts!();
                }
                rows.push(SsbmDateRow {
                    datekey: row
                        .get::<i32>(1)
                        .map_err(|err| format!("d_datekey read failed: {err:?}"))?
                        .ok_or("d_datekey is NULL")?,
                    year: row
                        .get::<i32>(2)
                        .map_err(|err| format!("d_year read failed: {err:?}"))?
                        .ok_or("d_year is NULL")?,
                    yearmonth: row
                        .get::<String>(3)
                        .map_err(|err| format!("d_yearmonth read failed: {err:?}"))?
                        .ok_or("d_yearmonth is NULL")?,
                    yearmonthnum: row
                        .get::<i32>(4)
                        .map_err(|err| format!("d_yearmonthnum read failed: {err:?}"))?
                        .ok_or("d_yearmonthnum is NULL")?,
                    weeknuminyear: row
                        .get::<i32>(5)
                        .map_err(|err| format!("d_weeknuminyear read failed: {err:?}"))?
                        .ok_or("d_weeknuminyear is NULL")?,
                });
            }
            Ok::<_, String>(rows)
        })
    })?;

    if date_rows.is_empty() {
        return Err("ssbm_date has no rows; refusing to create SSBM Q1 cache".to_owned());
    }

    let part_rows = crate::engine::ffi::planner_hooks::with_planner_hooks_suspended(|| {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT p_partkey, p_mfgr, p_category, p_brand1 FROM ssbm_part",
                    None,
                    &[],
                )
                .map_err(|err| format!("failed to scan ssbm_part: {err:?}"))?;

            let mut rows = Vec::new();
            let mut scanned_rows: usize = 0;
            for row in table {
                scanned_rows += 1;
                if scanned_rows.is_multiple_of(LOAD_INTERRUPT_CHECK_ROWS) {
                    pgrx::check_for_interrupts!();
                }
                rows.push(SsbmPartRow {
                    partkey: row
                        .get::<i32>(1)
                        .map_err(|err| format!("p_partkey read failed: {err:?}"))?
                        .ok_or("p_partkey is NULL")?,
                    mfgr: row
                        .get::<String>(2)
                        .map_err(|err| format!("p_mfgr read failed: {err:?}"))?
                        .ok_or("p_mfgr is NULL")?,
                    category: row
                        .get::<String>(3)
                        .map_err(|err| format!("p_category read failed: {err:?}"))?
                        .ok_or("p_category is NULL")?,
                    brand1: row
                        .get::<String>(4)
                        .map_err(|err| format!("p_brand1 read failed: {err:?}"))?
                        .ok_or("p_brand1 is NULL")?,
                });
            }
            Ok::<_, String>(rows)
        })
    })?;

    if part_rows.is_empty() {
        return Err("ssbm_part has no rows; refusing to create SSBM resident cache".to_owned());
    }

    let customer_rows = crate::engine::ffi::planner_hooks::with_planner_hooks_suspended(|| {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT c_custkey, c_city, c_nation, c_region FROM ssbm_customer",
                    None,
                    &[],
                )
                .map_err(|err| format!("failed to scan ssbm_customer: {err:?}"))?;

            let mut rows = Vec::new();
            let mut scanned_rows: usize = 0;
            for row in table {
                scanned_rows += 1;
                if scanned_rows.is_multiple_of(LOAD_INTERRUPT_CHECK_ROWS) {
                    pgrx::check_for_interrupts!();
                }
                rows.push(SsbmCustomerRow {
                    custkey: row
                        .get::<i32>(1)
                        .map_err(|err| format!("c_custkey read failed: {err:?}"))?
                        .ok_or("c_custkey is NULL")?,
                    city: row
                        .get::<String>(2)
                        .map_err(|err| format!("c_city read failed: {err:?}"))?
                        .ok_or("c_city is NULL")?,
                    nation: row
                        .get::<String>(3)
                        .map_err(|err| format!("c_nation read failed: {err:?}"))?
                        .ok_or("c_nation is NULL")?,
                    region: row
                        .get::<String>(4)
                        .map_err(|err| format!("c_region read failed: {err:?}"))?
                        .ok_or("c_region is NULL")?,
                });
            }
            Ok::<_, String>(rows)
        })
    })?;

    if customer_rows.is_empty() {
        return Err("ssbm_customer has no rows; refusing to create SSBM resident cache".to_owned());
    }

    let supplier_rows = crate::engine::ffi::planner_hooks::with_planner_hooks_suspended(|| {
        Spi::connect(|client| {
            let table = client
                .select(
                    "SELECT s_suppkey, s_city, s_nation, s_region FROM ssbm_supplier",
                    None,
                    &[],
                )
                .map_err(|err| format!("failed to scan ssbm_supplier: {err:?}"))?;

            let mut rows = Vec::new();
            let mut scanned_rows: usize = 0;
            for row in table {
                scanned_rows += 1;
                if scanned_rows.is_multiple_of(LOAD_INTERRUPT_CHECK_ROWS) {
                    pgrx::check_for_interrupts!();
                }
                rows.push(SsbmSupplierRow {
                    suppkey: row
                        .get::<i32>(1)
                        .map_err(|err| format!("s_suppkey read failed: {err:?}"))?
                        .ok_or("s_suppkey is NULL")?,
                    city: row
                        .get::<String>(2)
                        .map_err(|err| format!("s_city read failed: {err:?}"))?
                        .ok_or("s_city is NULL")?,
                    nation: row
                        .get::<String>(3)
                        .map_err(|err| format!("s_nation read failed: {err:?}"))?
                        .ok_or("s_nation is NULL")?,
                    region: row
                        .get::<String>(4)
                        .map_err(|err| format!("s_region read failed: {err:?}"))?
                        .ok_or("s_region is NULL")?,
                });
            }
            Ok::<_, String>(rows)
        })
    })?;

    if supplier_rows.is_empty() {
        return Err("ssbm_supplier has no rows; refusing to create SSBM resident cache".to_owned());
    }

    let (date_key_min, date_year_by_offset, year_min, year_count) =
        build_date_year_lookup(&date_rows)?;
    let (brand_values, part_brand_code_by_key) = build_part_brand_lookup(&part_rows)?;
    let customer_key_count = dimension_key_count(
        customer_rows.iter().map(|row| row.custkey),
        "ssbm_customer.c_custkey",
    )?;
    let supplier_key_count = dimension_key_count(
        supplier_rows.iter().map(|row| row.suppkey),
        "ssbm_supplier.s_suppkey",
    )?;

    let mut q2_filter_cache = Vec::new();
    for variant in [
        SsbmQ2Variant::Q2_1,
        SsbmQ2Variant::Q2_2,
        SsbmQ2Variant::Q2_3,
    ] {
        match SsbmQ2CachedFilter::from_rows(
            variant,
            &part_rows,
            part_brand_code_by_key.len(),
            &supplier_rows,
            supplier_key_count,
        ) {
            Ok(filter) => q2_filter_cache.push(filter),
            Err(err) => pgrx::warning!(
                "pg_accel: SSBM Q2 variant {variant:?} cached filter prebuild failed: {err}; \
                 the filter will be rebuilt on demand at execution"
            ),
        }
    }

    let mut q3_filter_cache = Vec::new();
    for variant in [
        SsbmQ3Variant::Q3_1,
        SsbmQ3Variant::Q3_2,
        SsbmQ3Variant::Q3_3,
        SsbmQ3Variant::Q3_4,
    ] {
        match SsbmQ3CachedFilter::from_rows(
            variant,
            &date_rows,
            date_key_min,
            date_year_by_offset.len(),
            &customer_rows,
            customer_key_count,
            &supplier_rows,
            supplier_key_count,
        ) {
            Ok(filter) => q3_filter_cache.push(filter),
            Err(err) => pgrx::warning!(
                "pg_accel: SSBM Q3 variant {variant:?} cached filter prebuild failed: {err}; \
                 the filter will be rebuilt on demand at execution"
            ),
        }
    }

    let mut q4_filter_cache = Vec::new();
    for variant in [
        SsbmQ4Variant::Q4_1,
        SsbmQ4Variant::Q4_2,
        SsbmQ4Variant::Q4_3,
    ] {
        match SsbmQ4CachedFilter::from_rows(
            variant,
            &date_rows,
            date_key_min,
            date_year_by_offset.len(),
            &customer_rows,
            customer_key_count,
            &supplier_rows,
            supplier_key_count,
            &part_rows,
            part_brand_code_by_key.len(),
        ) {
            Ok(filter) => q4_filter_cache.push(filter),
            Err(err) => pgrx::warning!(
                "pg_accel: SSBM Q4 variant {variant:?} cached filter prebuild failed: {err}; \
                 the filter will be rebuilt on demand at execution"
            ),
        }
    }
    let year_count_usize = usize::try_from(year_count)
        .map_err(|_| "SSBM date year count is negative; refusing Q4 scratch allocation")?;
    let q4_scratch_group_capacity = q4_filter_cache
        .iter()
        .filter_map(|filter| {
            year_count_usize
                .checked_mul(filter.geo_labels().len())?
                .checked_mul(filter.part_labels().len())
        })
        .max()
        .unwrap_or(1)
        .max(1);

    let row_count = orderdate.len();
    ensure_resident_row_count_fits_u32(row_count, "ssbm_lineorder")?;
    Ok(SsbmQ1ResidentCache {
        fact_rel_oid,
        date_rel_oid,
        part_rel_oid,
        customer_rel_oid,
        supplier_rel_oid,
        row_count,
        orderdate: alloc_and_copy_i32(&orderdate, "lo_orderdate")?,
        discount: alloc_and_copy_i32(&discount, "lo_discount")?,
        quantity: alloc_and_copy_i32(&quantity, "lo_quantity")?,
        extendedprice: alloc_and_copy_i32(&extendedprice, "lo_extendedprice")?,
        partkey: alloc_and_copy_i32(&partkey, "lo_partkey")?,
        custkey: alloc_and_copy_i32(&custkey, "lo_custkey")?,
        suppkey: alloc_and_copy_i32(&suppkey, "lo_suppkey")?,
        revenue: alloc_and_copy_i32(&revenue, "lo_revenue")?,
        supplycost: alloc_and_copy_i32(&supplycost, "lo_supplycost")?,
        scratch: SsbmQ1ScratchBuffers::new(row_count)?,
        date_rows,
        date_key_min,
        date_year_by_offset: alloc_and_copy_i32(&date_year_by_offset, "ssbm_date_year_lookup")?,
        year_min,
        year_count,
        part_rows,
        customer_rows,
        supplier_rows,
        brand_values,
        part_brand_code_by_key: alloc_and_copy_i32(
            &part_brand_code_by_key,
            "ssbm_part_brand_code_lookup",
        )?,
        customer_key_count,
        supplier_key_count,
        date_filter_cache: RefCell::new(Vec::new()),
        q2_filter_cache: RefCell::new(q2_filter_cache),
        q3_filter_cache: RefCell::new(q3_filter_cache),
        q4_filter_cache: RefCell::new(q4_filter_cache),
        q4_scratch_profit_lo: ExprDeviceBuffer::new(q4_scratch_group_capacity)
            .ok_or("device allocation failed for SSBM Q4 profit low scratch")?,
        q4_scratch_profit_hi: ExprDeviceBuffer::new(q4_scratch_group_capacity)
            .ok_or("device allocation failed for SSBM Q4 profit high scratch")?,
        q4_scratch_count: ExprDeviceBuffer::new(q4_scratch_group_capacity)
            .ok_or("device allocation failed for SSBM Q4 count scratch")?,
        q4_scratch_group_capacity,
    })
}

#[pg_extern]
fn pg_accel_load_ssbm_q1_cache() -> i64 {
    ensure_relcache_invalidation_callback_registered();
    process_relcache_invalidations();
    SSBM_Q1_CACHE.with(|slot| {
        *slot.borrow_mut() = None;
    });
    let loaded = {
        let _guard = ResidentCacheLoadGuard::begin();
        load_ssbm_q1_cache()
    };
    match loaded {
        Ok(cache) => {
            let rows = cache.row_count() as i64;
            let rel_oids = [
                cache.fact_rel_oid,
                cache.date_rel_oid,
                cache.part_rel_oid,
                cache.customer_rel_oid,
                cache.supplier_rel_oid,
            ];
            SSBM_Q1_CACHE.with(|slot| {
                *slot.borrow_mut() = Some(cache);
            });
            // Apply invalidations that arrived while the load's SPI scans ran;
            // they may target the relations just loaded.
            process_relcache_invalidations();
            warn_dml_staleness_once(&rel_oids);
            rows
        }
        Err(err) => pgrx::error!("pg_accel: failed to load SSBM Q1 resident cache: {err}"),
    }
}

#[pg_extern]
fn pg_accel_clear_ssbm_q1_cache() {
    SSBM_Q1_CACHE.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

#[pg_extern]
fn pg_accel_ssbm_q1_cache_rows() -> i64 {
    ssbm_q1_cache_rows() as i64
}

#[allow(clippy::too_many_arguments)]
fn install_resident_groupagg_cache(
    table: &str,
    group_col: &str,
    group_key_source: ResidentGroupKeySource,
    value_col: &str,
    value_rhs_col: Option<&str>,
    measure_op: ResidentMeasureOp,
    filter_col: Option<&str>,
    allow_nullable_f64: bool,
) -> i64 {
    ensure_relcache_invalidation_callback_registered();
    process_relcache_invalidations();
    RESIDENT_DENSE_GROUP_AGG_CACHE.with(|slot| {
        *slot.borrow_mut() = None;
    });
    let loaded = {
        let _guard = ResidentCacheLoadGuard::begin();
        load_resident_groupagg_cache(
            table,
            group_col,
            group_key_source,
            value_col,
            value_rhs_col,
            measure_op,
            filter_col,
            allow_nullable_f64,
        )
    };
    match loaded {
        Ok(cache) => {
            let rows = cache.row_count() as i64;
            let rel_oids = [cache.rel_oid];
            RESIDENT_DENSE_GROUP_AGG_CACHE.with(|slot| *slot.borrow_mut() = Some(cache));
            process_relcache_invalidations();
            warn_dml_staleness_once(&rel_oids);
            rows
        }
        Err(err) => {
            pgrx::error!("pg_accel: failed to load resident groupagg cache: {err}")
        }
    }
}

fn install_resident_f64_reduce_cache(
    table: &str,
    value_col: &str,
    allow_nullable_f64: bool,
) -> i64 {
    ensure_relcache_invalidation_callback_registered();
    process_relcache_invalidations();
    RESIDENT_DENSE_GROUP_AGG_CACHE.with(|slot| {
        *slot.borrow_mut() = None;
    });
    let loaded = {
        let _guard = ResidentCacheLoadGuard::begin();
        load_resident_f64_reduce_cache(table, value_col, allow_nullable_f64)
    };
    match loaded {
        Ok(cache) => {
            let rows = cache.row_count() as i64;
            let rel_oids = [cache.rel_oid];
            RESIDENT_DENSE_GROUP_AGG_CACHE.with(|slot| *slot.borrow_mut() = Some(cache));
            process_relcache_invalidations();
            warn_dml_staleness_once(&rel_oids);
            rows
        }
        Err(err) => {
            pgrx::error!("pg_accel: failed to load resident f64 reduce cache: {err}")
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn install_resident_star_dim_groupagg_cache(
    fact_table: &str,
    fact_key_col: &str,
    fact_value_col: &str,
    fact_value_cmp_opcode: u16,
    fact_value_cmp_const: f64,
    dim_table: &str,
    dim_key_col: &str,
    dim_filter_col: &str,
    dim_filter_cmp_opcode: u16,
    dim_filter_const: f64,
    dim_group_col: &str,
    dim_group_key_source: ResidentGroupKeySource,
    allow_nullable_f64: bool,
) -> i64 {
    ensure_relcache_invalidation_callback_registered();
    process_relcache_invalidations();
    RESIDENT_STAR_DIM_GROUP_AGG_CACHE.with(|slot| {
        *slot.borrow_mut() = None;
    });
    let loaded = {
        let _guard = ResidentCacheLoadGuard::begin();
        load_resident_star_dim_groupagg_cache(
            fact_table,
            fact_key_col,
            fact_value_col,
            fact_value_cmp_opcode,
            fact_value_cmp_const,
            dim_table,
            dim_key_col,
            dim_filter_col,
            dim_filter_cmp_opcode,
            dim_filter_const,
            dim_group_col,
            dim_group_key_source,
            allow_nullable_f64,
        )
    };
    match loaded {
        Ok(cache) => {
            let rows = cache.row_count() as i64;
            let rel_oids = [cache.fact_rel_oid, cache.dim_rel_oid];
            RESIDENT_STAR_DIM_GROUP_AGG_CACHE.with(|slot| *slot.borrow_mut() = Some(cache));
            process_relcache_invalidations();
            warn_dml_staleness_once(&rel_oids);
            rows
        }
        Err(err) => {
            pgrx::error!("pg_accel: failed to load resident star groupagg cache: {err}")
        }
    }
}

fn install_resident_hashjoin_count_cache(
    outer_table: &str,
    outer_key_col: &str,
    inner_table: &str,
    inner_key_col: &str,
    key_type: PgaccelKeyType,
) -> i64 {
    ensure_relcache_invalidation_callback_registered();
    process_relcache_invalidations();
    RESIDENT_HASH_JOIN_COUNT_CACHE.with(|slot| {
        *slot.borrow_mut() = None;
    });
    let loaded = {
        let _guard = ResidentCacheLoadGuard::begin();
        load_resident_hashjoin_count_cache(
            outer_table,
            outer_key_col,
            inner_table,
            inner_key_col,
            key_type,
        )
    };
    match loaded {
        Ok(cache) => {
            let rows = cache.shape().outer_rows as i64;
            let rel_oids = [cache.outer_rel_oid, cache.inner_rel_oid];
            RESIDENT_HASH_JOIN_COUNT_CACHE.with(|slot| *slot.borrow_mut() = Some(cache));
            process_relcache_invalidations();
            warn_dml_staleness_once(&rel_oids);
            rows
        }
        Err(err) => {
            pgrx::error!("pg_accel: failed to load resident hashjoin count cache: {err}")
        }
    }
}

fn parse_resident_group_key_source(value: &str) -> Result<ResidentGroupKeySource, String> {
    match value.to_ascii_lowercase().as_str() {
        "int4" | "int32" | "integer" => Ok(ResidentGroupKeySource::Int4),
        "text" => Ok(ResidentGroupKeySource::Text),
        _ => Err(format!(
            "unsupported resident group key type `{value}`; expected int4 or text"
        )),
    }
}

fn parse_resident_measure_op(value: &str) -> Result<ResidentMeasureOp, String> {
    match value.to_ascii_lowercase().as_str() {
        "column" | "direct" => Ok(ResidentMeasureOp::Column),
        "mul" | "multiply" => Ok(ResidentMeasureOp::Mul),
        "sub" | "subtract" => Ok(ResidentMeasureOp::Sub),
        "stats_pair" | "two_measure_stats" | "grouped_stats" => Ok(ResidentMeasureOp::StatsPair),
        _ => Err(format!(
            "unsupported resident measure op `{value}`; expected column, mul, sub, or stats_pair"
        )),
    }
}

fn parse_resident_cmp_opcode(value: &str) -> Result<u16, String> {
    if value.eq_ignore_ascii_case("always_true") || value.eq_ignore_ascii_case("true") {
        return Ok(crate::engine::expr_compiler::opcode::ALWAYS_TRUE);
    }
    crate::engine::expr_compiler::pg_cmp_op_to_opcode(value).ok_or_else(|| {
        format!("unsupported resident comparison operator `{value}`; expected =, <>, <, <=, >, >=")
    })
}

fn install_resident_h3_latlng_groupagg_cache(table: &str, point_col: &str, resolution: i32) -> i64 {
    ensure_relcache_invalidation_callback_registered();
    process_relcache_invalidations();
    RESIDENT_H3_GROUP_AGG_CACHE.with(|slot| {
        *slot.borrow_mut() = None;
    });
    let loaded = {
        let _guard = ResidentCacheLoadGuard::begin();
        load_resident_h3_latlng_groupagg_cache(table, point_col, resolution)
    };
    match loaded {
        Ok(cache) => {
            let rows = cache.row_count() as i64;
            let rel_oids = [cache.rel_oid];
            RESIDENT_H3_GROUP_AGG_CACHE.with(|slot| {
                *slot.borrow_mut() = Some(cache);
            });
            process_relcache_invalidations();
            warn_dml_staleness_once(&rel_oids);
            rows
        }
        Err(err) => pgrx::error!("pg_accel: failed to load resident H3 lat/lng cache: {err}"),
    }
}

fn install_resident_h3_parent_groupagg_cache(table: &str, cell_col: &str) -> i64 {
    ensure_relcache_invalidation_callback_registered();
    process_relcache_invalidations();
    RESIDENT_H3_GROUP_AGG_CACHE.with(|slot| {
        *slot.borrow_mut() = None;
    });
    let loaded = {
        let _guard = ResidentCacheLoadGuard::begin();
        load_resident_h3_parent_groupagg_cache(table, cell_col)
    };
    match loaded {
        Ok(cache) => {
            let rows = cache.row_count() as i64;
            let rel_oids = [cache.rel_oid];
            RESIDENT_H3_GROUP_AGG_CACHE.with(|slot| {
                *slot.borrow_mut() = Some(cache);
            });
            process_relcache_invalidations();
            warn_dml_staleness_once(&rel_oids);
            rows
        }
        Err(err) => pgrx::error!("pg_accel: failed to load resident H3 parent cache: {err}"),
    }
}

fn parse_resident_h3_groupagg_kind(value: &str) -> Result<ResidentH3GroupedCountKind, String> {
    match value.to_ascii_lowercase().as_str() {
        "latlng" | "lat_lng" | "latlng_to_cell" | "h3_latlng_to_cell" => {
            Ok(ResidentH3GroupedCountKind::LatLngToCell)
        }
        "cell" | "cell_to_parent" | "h3_cell_to_parent" | "parent" => {
            Ok(ResidentH3GroupedCountKind::CellToParent)
        }
        _ => Err(format!(
            "unsupported resident H3 groupagg kind `{value}`; expected latlng_to_cell or cell_to_parent"
        )),
    }
}

#[pg_extern]
#[allow(clippy::too_many_arguments)]
fn pg_accel_load_resident_groupagg_cache(
    table_name: String,
    group_col: String,
    group_key_type: String,
    value_col: String,
    value_rhs_col: Option<String>,
    measure_op: String,
    filter_col: Option<String>,
    allow_nullable_f64: bool,
) -> i64 {
    let group_key_source = parse_resident_group_key_source(&group_key_type)
        .unwrap_or_else(|err| pgrx::error!("{err}"));
    let measure_op =
        parse_resident_measure_op(&measure_op).unwrap_or_else(|err| pgrx::error!("{err}"));
    install_resident_groupagg_cache(
        &table_name,
        &group_col,
        group_key_source,
        &value_col,
        value_rhs_col.as_deref(),
        measure_op,
        filter_col.as_deref(),
        allow_nullable_f64,
    )
}

#[pg_extern]
fn pg_accel_load_resident_f64_reduce_cache(
    table_name: String,
    value_col: String,
    allow_nullable_f64: bool,
) -> i64 {
    install_resident_f64_reduce_cache(&table_name, &value_col, allow_nullable_f64)
}

#[pg_extern]
#[allow(clippy::too_many_arguments)]
fn pg_accel_load_resident_star_dim_groupagg_cache(
    fact_table_name: String,
    fact_key_col: String,
    fact_value_col: String,
    fact_value_cmp_op: String,
    fact_value_cmp_const: f64,
    dim_table_name: String,
    dim_key_col: String,
    dim_filter_col: String,
    dim_filter_cmp_op: String,
    dim_filter_cmp_const: f64,
    dim_group_col: String,
    dim_group_key_type: String,
    allow_nullable_f64: bool,
) -> i64 {
    let fact_value_cmp_opcode =
        parse_resident_cmp_opcode(&fact_value_cmp_op).unwrap_or_else(|err| pgrx::error!("{err}"));
    let dim_filter_cmp_opcode =
        parse_resident_cmp_opcode(&dim_filter_cmp_op).unwrap_or_else(|err| pgrx::error!("{err}"));
    let dim_group_key_source = parse_resident_group_key_source(&dim_group_key_type)
        .unwrap_or_else(|err| pgrx::error!("{err}"));
    install_resident_star_dim_groupagg_cache(
        &fact_table_name,
        &fact_key_col,
        &fact_value_col,
        fact_value_cmp_opcode,
        fact_value_cmp_const,
        &dim_table_name,
        &dim_key_col,
        &dim_filter_col,
        dim_filter_cmp_opcode,
        dim_filter_cmp_const,
        &dim_group_col,
        dim_group_key_source,
        allow_nullable_f64,
    )
}

#[pg_extern]
fn pg_accel_load_resident_h3_groupagg_cache(
    table_name: String,
    input_col: String,
    input_kind: String,
    resolution: i32,
) -> i64 {
    match parse_resident_h3_groupagg_kind(&input_kind).unwrap_or_else(|err| pgrx::error!("{err}")) {
        ResidentH3GroupedCountKind::LatLngToCell => {
            install_resident_h3_latlng_groupagg_cache(&table_name, &input_col, resolution)
        }
        ResidentH3GroupedCountKind::CellToParent => {
            install_resident_h3_parent_groupagg_cache(&table_name, &input_col)
        }
    }
}

#[pg_extern]
fn pg_accel_load_resident_hashjoin_count_cache(
    outer_table_name: String,
    outer_key_col: String,
    inner_table_name: String,
    inner_key_col: String,
    key_type: String,
) -> i64 {
    let key_type =
        parse_resident_hashjoin_key_type(&key_type).unwrap_or_else(|err| pgrx::error!("{err}"));
    install_resident_hashjoin_count_cache(
        &outer_table_name,
        &outer_key_col,
        &inner_table_name,
        &inner_key_col,
        key_type,
    )
}

#[pg_extern]
fn pg_accel_clear_resident_groupagg_cache() {
    RESIDENT_DENSE_GROUP_AGG_CACHE.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

#[pg_extern]
fn pg_accel_clear_resident_star_dim_groupagg_cache() {
    RESIDENT_STAR_DIM_GROUP_AGG_CACHE.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

#[pg_extern]
fn pg_accel_clear_resident_h3_groupagg_cache() {
    RESIDENT_H3_GROUP_AGG_CACHE.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

#[pg_extern]
fn pg_accel_clear_resident_hashjoin_count_cache() {
    RESIDENT_HASH_JOIN_COUNT_CACHE.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

#[pg_extern]
fn pg_accel_resident_groupagg_cache_rows() -> i64 {
    resident_dense_groupagg_cache_rows() as i64
}

#[pg_extern]
fn pg_accel_resident_star_dim_groupagg_cache_rows() -> i64 {
    resident_star_dim_groupagg_cache_rows() as i64
}

#[pg_extern]
fn pg_accel_resident_h3_groupagg_cache_rows() -> i64 {
    resident_h3_groupagg_cache_rows() as i64
}

#[pg_extern]
fn pg_accel_resident_hashjoin_count_cache_rows() -> i64 {
    resident_hashjoin_count_cache_rows() as i64
}

#[cfg(test)]
mod tests {
    use super::{
        ResidentStarDimensionHostFilter, SsbmCustomerRow, SsbmDateRow, SsbmPartRow,
        SsbmQ1DatePredicate, SsbmQ1ResidentCache, SsbmQ3Variant, SsbmQ4Variant,
        contiguous_key_range,
    };

    fn date_keys_for(rows: Vec<SsbmDateRow>, predicate: SsbmQ1DatePredicate) -> Vec<i32> {
        struct DateOnlyCache {
            date_rows: Vec<SsbmDateRow>,
        }
        impl DateOnlyCache {
            fn date_keys(&self, predicate: SsbmQ1DatePredicate) -> Vec<i32> {
                let mut keys: Vec<i32> = self
                    .date_rows
                    .iter()
                    .filter(|row| match predicate {
                        SsbmQ1DatePredicate::Year(year) => row.year == year,
                        SsbmQ1DatePredicate::YearMonthNum(yearmonthnum) => {
                            row.yearmonthnum == yearmonthnum
                        }
                        SsbmQ1DatePredicate::YearWeek { year, week } => {
                            row.year == year && row.weeknuminyear == week
                        }
                    })
                    .map(|row| row.datekey)
                    .collect();
                keys.sort_unstable();
                keys.dedup();
                keys
            }
        }
        DateOnlyCache { date_rows: rows }.date_keys(predicate)
    }

    #[test]
    fn date_key_filter_matches_q1_predicates() {
        let rows = vec![
            SsbmDateRow {
                datekey: 10,
                year: 1993,
                yearmonth: "Jan1993".to_owned(),
                yearmonthnum: 199301,
                weeknuminyear: 1,
            },
            SsbmDateRow {
                datekey: 11,
                year: 1993,
                yearmonth: "Feb1993".to_owned(),
                yearmonthnum: 199302,
                weeknuminyear: 6,
            },
            SsbmDateRow {
                datekey: 20,
                year: 1994,
                yearmonth: "Jan1994".to_owned(),
                yearmonthnum: 199401,
                weeknuminyear: 6,
            },
            SsbmDateRow {
                datekey: 20,
                year: 1994,
                yearmonth: "Jan1994".to_owned(),
                yearmonthnum: 199401,
                weeknuminyear: 6,
            },
        ];

        assert_eq!(
            date_keys_for(rows.clone(), SsbmQ1DatePredicate::Year(1993)),
            vec![10, 11]
        );
        assert_eq!(
            date_keys_for(rows.clone(), SsbmQ1DatePredicate::YearMonthNum(199401)),
            vec![20]
        );
        assert_eq!(
            date_keys_for(
                rows,
                SsbmQ1DatePredicate::YearWeek {
                    year: 1994,
                    week: 6,
                },
            ),
            vec![20]
        );
    }

    #[test]
    fn public_date_predicate_type_stays_copyable() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<SsbmQ1DatePredicate>();
        let _ = std::mem::size_of::<SsbmQ1ResidentCache>();
    }

    #[test]
    fn contiguous_date_keys_collapse_to_range() {
        assert_eq!(
            contiguous_key_range(&[19930101]),
            Some((19930101, 19930101))
        );
        assert_eq!(
            contiguous_key_range(&[19930101, 19930102, 19930103]),
            Some((19930101, 19930103))
        );
        assert_eq!(contiguous_key_range(&[19930101, 19930103]), None);
        assert_eq!(contiguous_key_range(&[]), None);
    }

    #[derive(Debug)]
    struct DimensionRow {
        key: i32,
        selected: bool,
        label: &'static str,
    }

    #[test]
    fn resident_star_match_only_ignores_invalid_keys() {
        let rows = vec![
            DimensionRow {
                key: -1,
                selected: true,
                label: "ignored",
            },
            DimensionRow {
                key: 0,
                selected: true,
                label: "a",
            },
            DimensionRow {
                key: 1,
                selected: false,
                label: "b",
            },
            DimensionRow {
                key: 2,
                selected: true,
                label: "c",
            },
            DimensionRow {
                key: 8,
                selected: true,
                label: "ignored",
            },
        ];

        let filter = ResidentStarDimensionHostFilter::match_only(
            &rows,
            3,
            |row| row.key,
            |row| row.selected,
        );

        assert_eq!(filter.match_by_key, vec![1, 0, 1]);
        assert_eq!(filter.group_code_by_key, None);
        assert!(filter.labels.is_empty());
    }

    #[test]
    fn resident_star_grouped_labels_are_sorted_and_key_bounded() {
        let rows = vec![
            DimensionRow {
                key: -1,
                selected: true,
                label: "aardvark",
            },
            DimensionRow {
                key: 2,
                selected: true,
                label: "beta",
            },
            DimensionRow {
                key: 0,
                selected: true,
                label: "alpha",
            },
            DimensionRow {
                key: 1,
                selected: false,
                label: "ignored",
            },
            DimensionRow {
                key: 9,
                selected: true,
                label: "zeta",
            },
        ];

        let filter = ResidentStarDimensionHostFilter::grouped_by_label(
            &rows,
            3,
            |row| row.key,
            |row| row.selected,
            |row| row.label,
            "empty labels",
            "missing label",
        )
        .expect("grouped dimension should build");

        assert_eq!(filter.labels, vec!["aardvark", "alpha", "beta", "zeta"]);
        assert_eq!(filter.match_by_key, vec![1, 0, 1]);
        assert_eq!(filter.group_code_by_key.as_deref(), Some(&[1, -1, 2][..]));
    }

    #[test]
    fn resident_star_fixed_code_preserves_zero_group_for_matches() {
        let rows = vec![
            DimensionRow {
                key: 0,
                selected: true,
                label: "a",
            },
            DimensionRow {
                key: 1,
                selected: false,
                label: "b",
            },
            DimensionRow {
                key: 2,
                selected: true,
                label: "c",
            },
            DimensionRow {
                key: 9,
                selected: true,
                label: "ignored",
            },
        ];

        let filter = ResidentStarDimensionHostFilter::grouped_by_fixed_code(
            &rows,
            3,
            vec![String::new()],
            0,
            |row| row.key,
            |row| row.selected,
        );

        assert_eq!(filter.labels, vec![String::new()]);
        assert_eq!(filter.match_by_key, vec![1, 0, 1]);
        assert_eq!(filter.group_code_by_key.as_deref(), Some(&[0, -1, 0][..]));
    }

    #[test]
    fn ssbm_q3_group_label_source_follows_variant_level() {
        let customers = vec![
            SsbmCustomerRow {
                custkey: 0,
                city: "TOKYO".to_owned(),
                nation: "JAPAN".to_owned(),
                region: "ASIA".to_owned(),
            },
            SsbmCustomerRow {
                custkey: 1,
                city: "SEOUL".to_owned(),
                nation: "KOREA".to_owned(),
                region: "ASIA".to_owned(),
            },
            SsbmCustomerRow {
                custkey: 2,
                city: "BOSTON".to_owned(),
                nation: "UNITED STATES".to_owned(),
                region: "AMERICA".to_owned(),
            },
        ];
        let q31 = ResidentStarDimensionHostFilter::grouped_by_label(
            &customers,
            3,
            |row| row.custkey,
            |row| SsbmQ3Variant::Q3_1.customer_matches(row),
            |row| {
                if SsbmQ3Variant::Q3_1.uses_nation_labels() {
                    row.nation.as_str()
                } else {
                    row.city.as_str()
                }
            },
            "empty labels",
            "missing label",
        )
        .expect("Q3.1 labels should build");

        let q32 = ResidentStarDimensionHostFilter::grouped_by_label(
            &customers,
            3,
            |row| row.custkey,
            |row| SsbmQ3Variant::Q3_2.customer_matches(row),
            |row| {
                if SsbmQ3Variant::Q3_2.uses_nation_labels() {
                    row.nation.as_str()
                } else {
                    row.city.as_str()
                }
            },
            "empty labels",
            "missing label",
        )
        .expect("Q3.2 labels should build");

        assert_eq!(q31.labels, vec!["JAPAN", "KOREA"]);
        assert_eq!(q32.labels, vec!["BOSTON"]);
    }

    #[test]
    fn ssbm_q4_fixed_sides_preserve_synthetic_codes() {
        let customers = vec![
            SsbmCustomerRow {
                custkey: 0,
                city: "NEW YORK".to_owned(),
                nation: "UNITED STATES".to_owned(),
                region: "AMERICA".to_owned(),
            },
            SsbmCustomerRow {
                custkey: 1,
                city: "TOKYO".to_owned(),
                nation: "JAPAN".to_owned(),
                region: "ASIA".to_owned(),
            },
        ];
        let parts = vec![
            SsbmPartRow {
                partkey: 0,
                mfgr: "MFGR#1".to_owned(),
                category: "MFGR#11".to_owned(),
                brand1: "MFGR#1111".to_owned(),
            },
            SsbmPartRow {
                partkey: 1,
                mfgr: "MFGR#9".to_owned(),
                category: "MFGR#99".to_owned(),
                brand1: "MFGR#9999".to_owned(),
            },
        ];

        let fixed_customer = ResidentStarDimensionHostFilter::grouped_by_fixed_code(
            &customers,
            2,
            Vec::new(),
            0,
            |row| row.custkey,
            |row| SsbmQ4Variant::Q4_2.customer_matches(row),
        );
        let synthetic_part = ResidentStarDimensionHostFilter::grouped_by_fixed_code(
            &parts,
            2,
            vec![String::new()],
            0,
            |row| row.partkey,
            |row| SsbmQ4Variant::Q4_1.part_matches(row),
        );

        assert!(fixed_customer.labels.is_empty());
        assert_eq!(
            fixed_customer.group_code_by_key.as_deref(),
            Some(&[0, -1][..])
        );
        assert_eq!(synthetic_part.labels, vec![String::new()]);
        assert_eq!(
            synthetic_part.group_code_by_key.as_deref(),
            Some(&[0, -1][..])
        );
    }
}
