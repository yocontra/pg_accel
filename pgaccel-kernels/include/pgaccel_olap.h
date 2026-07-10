/*
 * pgaccel_olap.h - descriptor ABI for resident grouped aggregation.
 *
 * This is the contract shared by the grouped-aggregate kernel, Rust bridge,
 * and planner/residency/executor pipeline. It contains no admission thresholds
 * or launch geometry. Those remain DeviceLimits and kernel implementation
 * details. Exact semantics and evolution rules live in docs/olap-abi.md.
 */

#ifndef PGACCEL_OLAP_H
#define PGACCEL_OLAP_H

#include "pgaccel_expr.h"

#ifdef __cplusplus
extern "C" {
#endif

#define PGACCEL_OLAP_ABI_VERSION 1u

#define PGACCEL_GROUPED_AGG_MAX_KEYS 3u
#define PGACCEL_GROUPED_AGG_MAX_MEASURES 4u
#define PGACCEL_GROUPED_AGG_MAX_DIMS 4u
#define PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES 4u

#define PGACCEL_GROUPED_AGG_LANE_SUM (1u << 0)
#define PGACCEL_GROUPED_AGG_LANE_MIN (1u << 1)
#define PGACCEL_GROUPED_AGG_LANE_MAX (1u << 2)
#define PGACCEL_GROUPED_AGG_LANE_COUNT (1u << 3)
#define PGACCEL_GROUPED_AGG_LANE_SUMSQ (1u << 4)
#define PGACCEL_GROUPED_AGG_LANE_RHS_SUM (1u << 5)
#define PGACCEL_GROUPED_AGG_LANE_RHS_COUNT (1u << 6)
#define PGACCEL_GROUPED_AGG_LANE_ALL_KNOWN 0x7fu

/* Chunk lifecycle bits. One-shot execution sets all three. A reusable
 * workspace permits RESET|ACCUMULATE, zero or more ACCUMULATE calls, then
 * ACCUMULATE|FINALIZE or FINALIZE. */
#define PGACCEL_GROUPED_AGG_EXEC_RESET (1u << 0)
#define PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE (1u << 1)
#define PGACCEL_GROUPED_AGG_EXEC_FINALIZE (1u << 2)
#define PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN 0x7u

#define PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE INT32_MIN

typedef enum {
  PGACCEL_GROUPED_AGG_MEASURE_COLUMN = 0,
  PGACCEL_GROUPED_AGG_MEASURE_MUL = 1,
  PGACCEL_GROUPED_AGG_MEASURE_SUB = 2,
  PGACCEL_GROUPED_AGG_MEASURE_STATS_PAIR = 3,
  PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR = 4,
} pgaccel_grouped_agg_measure_op;

typedef enum {
  PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT = 0,
  PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM0 = 1,
  PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM1 = 2,
  PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM2 = 3,
  PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM3 = 4,
} pgaccel_grouped_agg_key_source;

/* SQL masks treat both FALSE (-1) and UNKNOWN (0) as rejected. RECHECK masks
 * treat 0 as unresolved and increment uncertain_count. Other mask bytes are
 * descriptor/runtime errors, never truthy-by-nonzero. */
typedef enum {
  PGACCEL_GROUPED_AGG_FILTER_NONE = 0,
  PGACCEL_GROUPED_AGG_FILTER_SQL = 1,
  PGACCEL_GROUPED_AGG_FILTER_RECHECK = 2,
} pgaccel_grouped_agg_filter_kind;

typedef enum {
  PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE = 0,
  PGACCEL_GROUPED_AGG_PRED_SOURCE_RHS = 1,
} pgaccel_grouped_agg_pred_source;

typedef enum {
  PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX = 0,
  PGACCEL_GROUPED_AGG_GROUPING_HASH = 1,
} pgaccel_grouped_agg_grouping_mode;

typedef enum {
  PGACCEL_GROUPED_AGG_OUTPUT_DENSE = 0,
  PGACCEL_GROUPED_AGG_OUTPUT_COMPACT = 1,
} pgaccel_grouped_agg_output_mode;

/* Dedicated accumulator-state kinds. NUMERIC and INTERVAL reserve their
 * Phase-9 state shapes now; implementations may return PGACCEL_UNSUPPORTED
 * until those kernels land without changing this layout. */
typedef enum {
  PGACCEL_GROUPED_AGG_ACCUM_I64 = 1,
  PGACCEL_GROUPED_AGG_ACCUM_F64 = 2,
  PGACCEL_GROUPED_AGG_ACCUM_NUMERIC = 3,
  PGACCEL_GROUPED_AGG_ACCUM_INTERVAL = 4,
} pgaccel_grouped_agg_accum_kind;

/* One logical grouping key. FACT reads values for the current row. Dense
 * radix mode requires an INT32 dictionary/dense code column. Hash mode also
 * accepts INT64, including H3 indexes. DIM sources ignore values and use the
 * key-owned lookup_by_key map, allowing multiple group attributes from the
 * same dimension.
 *
 * Dense codes lie in [code_min, code_min + cardinality). A FACT NULL maps to
 * the explicit in-range null_code and therefore forms one SQL NULL group.
 * An explicit null_code must not equal KEY_NO_NULL_CODE; that sentinel is valid
 * only when the producer proves the key non-null.
 * Dimension maps encode a nullable group attribute with the same null_code.
 * Keys compose in array order, key 0 most significant. Hash grouping ignores
 * code_min/cardinality but still groups NULL values together. */
typedef struct {
  pgaccel_expr_usm_col values;
  const int32_t* lookup_by_key;
  int32_t source;
  int32_t code_min;
  uint32_t cardinality;
  int32_t null_code;
  uint32_t flags;
  uint32_t _pad0;
} pgaccel_grouped_agg_key;

/* One expression/accumulator slot. value and rhs contain physical type tags.
 * ABI v1 supports FLOAT64 -> F64 and INT32/INT64 -> I64. I64 arithmetic is
 * checked; overflow returns PGACCEL_ERROR. COUNT_STAR has zeroed value/rhs
 * views. scale is zero for I64/F64, a decimal scale for NUMERIC, and reserved
 * interval fractional precision for INTERVAL. state_bytes is sizeof(int64_t)
 * or sizeof(double) for v1 and the fixed limb/state width for later kinds. */
typedef struct {
  pgaccel_expr_usm_col value;
  pgaccel_expr_usm_col rhs;
  int32_t op;
  uint32_t agg_mask;
  int32_t accumulator_kind;
  int32_t scale;
  uint32_t state_bytes;
  uint32_t flags;
} pgaccel_grouped_agg_measure;

/* A composable predicate. mask is an optional tri-state byte sidecar using
 * PGACCEL_EXPR_TRUE (+1), PGACCEL_EXPR_FALSE (-1), and
 * PGACCEL_EXPR_UNCERTAIN (0). Inclusive range pairs are ORed, then ANDed with
 * mask and the optional compare-constant term. Bounds and constants are
 * pgaccel_val so INT64 predicates never round through f64. Their tags must
 * match the referenced source column. ALWAYS_TRUE disables compare-constant.
 *
 * The descriptor has one global where_filter and one independent filter per
 * measure. Bytecode and spatial programs are producer stages: they write mask
 * sidecars and are never embedded as pointers in this ABI. */
typedef struct {
  int32_t kind;
  int32_t predicate_source;
  int32_t predicate_measure_slot;
  int32_t predicate_range_count;
  pgaccel_val predicate_lo[PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES];
  pgaccel_val predicate_hi[PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES];
  uint16_t value_cmp_opcode;
  uint16_t _pad0;
  uint32_t flags;
  pgaccel_val value_cmp_const;
  const int8_t* mask;
} pgaccel_grouped_agg_filter;

/* One inner equi-join dimension. Residency converts supported int4/int8/dict
 * join keys losslessly to a dense resident INT32 key domain before this ABI.
 * key_min/key_count address lookup arrays; implementations checked-add them
 * in widened arithmetic before indexing. match_by_key NULL means every
 * in-domain key matches. multiplicity_by_key NULL means one matching dim row;
 * otherwise it supplies the exact INNER JOIN row multiplicity. Per-row
 * multiplicities across dimensions are checked-multiplied as u64. A NULL or
 * out-of-domain fact join key rejects the row. A key that groups by this dim
 * requires multiplicity one because one lookup code cannot represent multiple
 * differently-grouped matches. */
typedef struct {
  pgaccel_expr_usm_col fact_key;
  const uint8_t* match_by_key;
  const uint64_t* multiplicity_by_key;
  int32_t key_min;
  uint32_t key_count;
  uint32_t flags;
  uint32_t _pad0;
} pgaccel_grouped_agg_dim;

/* Versioned execution descriptor. In dense mode, group_capacity equals the
 * checked product of key cardinalities, or one for an ungrouped aggregate. In
 * hash mode it is the maximum emitted groups; overflow is UNSUPPORTED, never
 * truncation. measure_filters[i] applies only to measures[i].
 *
 * scratch is either NULL with all scratch metadata zero (one-shot only), or a
 * caller-owned DEVICE/SHARED_USM allocation from the same AdaptiveCpp context
 * as all resident inputs. It must satisfy workspace_requirements, remain live
 * until the call returns, not alias inputs/outputs, and not be reused by
 * concurrent calls. A non-NULL sufficient workspace forbids hidden device
 * allocations, including sort/hash temporary storage. */
typedef struct {
  uint32_t abi_version;
  uint32_t size_bytes;
  size_t row_count;

  int32_t grouping_mode;
  int32_t output_mode;
  uint32_t key_count;
  uint32_t _pad0;
  size_t group_capacity;
  pgaccel_grouped_agg_key keys[PGACCEL_GROUPED_AGG_MAX_KEYS];

  uint32_t measure_count;
  uint32_t execution_flags;
  uint32_t flags;
  uint32_t _pad1;
  pgaccel_grouped_agg_measure measures[PGACCEL_GROUPED_AGG_MAX_MEASURES];

  pgaccel_grouped_agg_filter where_filter;
  pgaccel_grouped_agg_filter measure_filters[PGACCEL_GROUPED_AGG_MAX_MEASURES];

  uint32_t dim_count;
  uint32_t _pad2;
  pgaccel_grouped_agg_dim dims[PGACCEL_GROUPED_AGG_MAX_DIMS];

  void* scratch;
  size_t scratch_bytes;
  int32_t scratch_space;
  uint32_t scratch_alignment;
} pgaccel_grouped_agg_desc;

/* Workspace query result. alignment is a power of two. space is the minimum
 * pgaccel_mem_space accepted by execute (SHARED_USM or DEVICE). */
typedef struct {
  uint32_t abi_version;
  uint32_t size_bytes;
  size_t bytes;
  size_t alignment;
  int32_t space;
  uint32_t flags;
} pgaccel_grouped_agg_workspace_req;

/* Per-measure output lanes. Value-state pointers use the descriptor's
 * accumulator kind and state_bytes. count is required iff COUNT is projected.
 * nonnull_count is always required for SUM/MIN/MAX/SUMSQ, even when COUNT is
 * not projected, so an all-NULL group is distinguishable from a numeric zero.
 * STATS_PAIR uses independent rhs_count/rhs_nonnull_count. All counts are u64
 * and every weighted addition is checked. */
typedef struct {
  void* sum;
  void* min;
  void* max;
  void* sumsq;
  uint64_t* count;
  uint64_t* nonnull_count;
  void* rhs_sum;
  uint64_t* rhs_count;
  uint64_t* rhs_nonnull_count;
} pgaccel_grouped_agg_measure_out;

/* One logical key lane for compact output. */
typedef struct {
  void* values;
  uint8_t* nulls;
  int32_t type;
  uint32_t flags;
} pgaccel_grouped_agg_key_out;

/* Result buffers share output_space, which must be HOST or SHARED_USM. Dense
 * output is positional; active_groups is required and has group_capacity
 * bytes. It records group existence after key/dim/WHERE gating independently
 * of aggregate validity. Compact output requires typed key lanes. HASH always
 * uses compact output and requires group_codes == NULL because no stable dense
 * composite code exists.
 *
 * emitted_group_count is the active-group count in both modes. A zero-key
 * aggregate always has one active group, even on empty input. A keyed aggregate
 * on empty input has none. Inactive value lanes and MIN/MAX for all-NULL groups
 * are unspecified; count and nonnull_count lanes are always written as zero.
 * Buffers may not overlap one another, inputs, or scratch. */
typedef struct {
  uint32_t abi_version;
  uint32_t size_bytes;
  size_t group_capacity;
  int32_t output_space;
  uint32_t flags;
  size_t* group_codes;
  uint8_t* active_groups;
  pgaccel_grouped_agg_key_out keys[PGACCEL_GROUPED_AGG_MAX_KEYS];
  pgaccel_grouped_agg_measure_out measures[PGACCEL_GROUPED_AGG_MAX_MEASURES];
  size_t emitted_group_count;
  uint64_t selected_count;
  uint64_t uncertain_count;
} pgaccel_grouped_agg_out;

pgaccel_status pgaccel_grouped_agg_workspace_requirements(
    const pgaccel_grouped_agg_desc* desc, pgaccel_grouped_agg_workspace_req* out);

/* Phase 4B replaces the dark UNSUPPORTED implementation. Invalid descriptors,
 * runtime codes/masks, overflow, or device execution errors return ERROR and
 * never partial success. Well-formed capabilities not implemented in the
 * current phase return UNSUPPORTED. */
pgaccel_status pgaccel_grouped_agg_execute(const pgaccel_grouped_agg_desc* desc,
                                           pgaccel_grouped_agg_out* out);

/* LP64 layout pins. pgaccel_val and pgaccel_expr_usm_col are pinned by
 * pgaccel_expr.h. Every field in every new ABI struct is pinned below. */
PGACCEL_ABI_PIN(sizeof(pgaccel_grouped_agg_key) == 56, "grouped_agg_key size");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_key, values) == 0, "key.values");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_key, lookup_by_key) == 24, "key.lookup");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_key, source) == 32, "key.source");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_key, code_min) == 36, "key.code_min");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_key, cardinality) == 40, "key.cardinality");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_key, null_code) == 44, "key.null_code");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_key, flags) == 48, "key.flags");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_key, _pad0) == 52, "key._pad0");

PGACCEL_ABI_PIN(sizeof(pgaccel_grouped_agg_measure) == 72, "grouped_agg_measure size");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_measure, value) == 0, "measure.value");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_measure, rhs) == 24, "measure.rhs");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_measure, op) == 48, "measure.op");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_measure, agg_mask) == 52, "measure.mask");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_measure, accumulator_kind) == 56,
                "measure.accumulator_kind");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_measure, scale) == 60, "measure.scale");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_measure, state_bytes) == 64,
                "measure.state_bytes");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_measure, flags) == 68, "measure.flags");

PGACCEL_ABI_PIN(sizeof(pgaccel_grouped_agg_filter) == 176, "grouped_agg_filter size");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_filter, kind) == 0, "filter.kind");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_filter, predicate_source) == 4,
                "filter.source");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_filter, predicate_measure_slot) == 8,
                "filter.slot");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_filter, predicate_range_count) == 12,
                "filter.range_count");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_filter, predicate_lo) == 16, "filter.lo");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_filter, predicate_hi) == 80, "filter.hi");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_filter, value_cmp_opcode) == 144,
                "filter.opcode");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_filter, _pad0) == 146, "filter._pad0");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_filter, flags) == 148, "filter.flags");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_filter, value_cmp_const) == 152,
                "filter.constant");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_filter, mask) == 168, "filter.mask");

PGACCEL_ABI_PIN(sizeof(pgaccel_grouped_agg_dim) == 56, "grouped_agg_dim size");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_dim, fact_key) == 0, "dim.fact_key");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_dim, match_by_key) == 24, "dim.match");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_dim, multiplicity_by_key) == 32,
                "dim.multiplicity");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_dim, key_min) == 40, "dim.key_min");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_dim, key_count) == 44, "dim.key_count");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_dim, flags) == 48, "dim.flags");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_dim, _pad0) == 52, "dim._pad0");

PGACCEL_ABI_PIN(sizeof(pgaccel_grouped_agg_desc) == 1648, "grouped_agg_desc size");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, abi_version) == 0, "desc.version");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, size_bytes) == 4, "desc.size");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, row_count) == 8, "desc.rows");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, grouping_mode) == 16,
                "desc.grouping_mode");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, output_mode) == 20,
                "desc.output_mode");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, key_count) == 24, "desc.key_count");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, _pad0) == 28, "desc._pad0");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, group_capacity) == 32,
                "desc.group_capacity");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, keys) == 40, "desc.keys");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, measure_count) == 208,
                "desc.measure_count");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, execution_flags) == 212,
                "desc.execution_flags");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, flags) == 216, "desc.flags");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, _pad1) == 220, "desc._pad1");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, measures) == 224, "desc.measures");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, where_filter) == 512,
                "desc.where_filter");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, measure_filters) == 688,
                "desc.measure_filters");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, dim_count) == 1392,
                "desc.dim_count");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, _pad2) == 1396, "desc._pad2");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, dims) == 1400, "desc.dims");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, scratch) == 1624, "desc.scratch");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, scratch_bytes) == 1632,
                "desc.scratch_bytes");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, scratch_space) == 1640,
                "desc.scratch_space");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_desc, scratch_alignment) == 1644,
                "desc.scratch_alignment");

PGACCEL_ABI_PIN(sizeof(pgaccel_grouped_agg_workspace_req) == 32,
                "grouped_agg_workspace_req size");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_workspace_req, abi_version) == 0,
                "workspace.version");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_workspace_req, size_bytes) == 4,
                "workspace.size");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_workspace_req, bytes) == 8,
                "workspace.bytes");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_workspace_req, alignment) == 16,
                "workspace.alignment");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_workspace_req, space) == 24,
                "workspace.space");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_workspace_req, flags) == 28,
                "workspace.flags");

PGACCEL_ABI_PIN(sizeof(pgaccel_grouped_agg_measure_out) == 72,
                "grouped_agg_measure_out size");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_measure_out, sum) == 0, "measure_out.sum");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_measure_out, min) == 8, "measure_out.min");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_measure_out, max) == 16, "measure_out.max");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_measure_out, sumsq) == 24,
                "measure_out.sumsq");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_measure_out, count) == 32,
                "measure_out.count");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_measure_out, nonnull_count) == 40,
                "measure_out.nonnull_count");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_measure_out, rhs_sum) == 48,
                "measure_out.rhs_sum");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_measure_out, rhs_count) == 56,
                "measure_out.rhs_count");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_measure_out, rhs_nonnull_count) == 64,
                "measure_out.rhs_nonnull_count");

PGACCEL_ABI_PIN(sizeof(pgaccel_grouped_agg_key_out) == 24,
                "grouped_agg_key_out size");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_key_out, values) == 0, "key_out.values");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_key_out, nulls) == 8, "key_out.nulls");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_key_out, type) == 16, "key_out.type");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_key_out, flags) == 20, "key_out.flags");

PGACCEL_ABI_PIN(sizeof(pgaccel_grouped_agg_out) == 424, "grouped_agg_out size");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_out, abi_version) == 0, "out.version");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_out, size_bytes) == 4, "out.size");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_out, group_capacity) == 8,
                "out.group_capacity");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_out, output_space) == 16,
                "out.output_space");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_out, flags) == 20, "out.flags");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_out, group_codes) == 24,
                "out.group_codes");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_out, active_groups) == 32,
                "out.active_groups");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_out, keys) == 40, "out.keys");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_out, measures) == 112, "out.measures");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_out, emitted_group_count) == 400,
                "out.emitted_group_count");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_out, selected_count) == 408,
                "out.selected_count");
PGACCEL_ABI_PIN(offsetof(pgaccel_grouped_agg_out, uncertain_count) == 416,
                "out.uncertain_count");

#ifdef __cplusplus
}
#endif

#endif /* PGACCEL_OLAP_H */
