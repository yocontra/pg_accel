/* Versioned resident row-mapping producer. This is not SQL planner admission. */
#ifndef PGACCEL_JOIN_ROWS_H
#define PGACCEL_JOIN_ROWS_H

#include "pgaccel_ffi.h"

#ifdef __cplusplus
extern "C" {
#endif

#define PGACCEL_JOIN_ROWS_VERSION 1u
#define PGACCEL_JOIN_ROWS_MAX_KEYS 4u
#define PGACCEL_JOIN_ROWS_NULL_ROW UINT64_MAX

typedef enum {
  PGACCEL_JOIN_ROWS_INNER = 0,
  PGACCEL_JOIN_ROWS_LEFT = 1,
  PGACCEL_JOIN_ROWS_SEMI = 2,
  PGACCEL_JOIN_ROWS_ANTI = 3,
} pgaccel_join_rows_kind;

typedef enum {
  PGACCEL_JOIN_ROWS_INT32 = 1,
  PGACCEL_JOIN_ROWS_INT64 = 2,
} pgaccel_join_rows_key_type;

typedef struct {
  const void* values;
  const uint8_t* nulls; /* Any nonzero byte is SQL NULL. */
  uint64_t value_bytes;
  uint64_t null_bytes;
  uint32_t type;
  uint32_t reserved;
} pgaccel_join_rows_key;

typedef struct {
  uint32_t version;
  uint32_t kind;
  uint32_t key_count;
  uint32_t reserved;
  uint64_t outer_rows;
  uint64_t inner_rows;
  pgaccel_join_rows_key outer_keys[PGACCEL_JOIN_ROWS_MAX_KEYS];
  pgaccel_join_rows_key inner_keys[PGACCEL_JOIN_ROWS_MAX_KEYS];
} pgaccel_join_rows_request;

typedef struct {
  uint64_t outer_row;
  /* NULL_ROW for unmatched LEFT and all SEMI/ANTI outputs. */
  uint64_t inner_row;
} pgaccel_join_row_pair;

typedef struct {
  uint64_t required_rows;
  uint64_t emitted_rows;
  /* Completed device fills/kernels; scalar copyback is not a launch. */
  uint64_t kernel_launches;
} pgaccel_join_rows_result;

/* Pure layout query: no device discovery/allocation or row-value reads.
 * Each input side is bounded by INT32_MAX rows in v1. Output indices,
 * cardinalities, offsets and byte forecasts are 64-bit. Inactive keys must
 * be zero; corresponding active key types must match exactly. Empty sides
 * have NULL pointers/zero byte lengths but retain the active key types.
 * The host bytes output must not alias the request or any declared input.
 * Nonempty outer inputs include a fixed-size device request snapshot in the
 * queried workspace. Callers must query, not hard-code, workspace size. */
pgaccel_status pgaccel_join_rows_workspace_bytes(
    const pgaccel_join_rows_request* request, uint64_t* bytes);

/* All keys/NULL sidecars remain immutable device/shared USM for this call.
 * Declared byte spans must refer to real live allocations on the current
 * device. Input-input aliasing is legal, including self joins. Workspace
 * (8-byte aligned) and output must be disjoint from each other, every input,
 * request and host result. The caller owns and budgets all large buffers.
 * Zero output capacity requires a NULL output pointer.
 *
 * Hash build, SQL equality/NULL handling, counts, hierarchical prefix sums
 * and row emission run on the GPU. Only a scalar cardinality crosses back.
 * Output is ordered by outer row; matched inner row order is unspecified.
 * Duplicate matches retain their full multiplicity. SEMI/ANTI are EXISTS /
 * NOT EXISTS, not SQL NOT IN (a NULL inner key never matches).
 *
 * Capacity exhaustion returns PGACCEL_OOM with required_rows set and
 * emitted_rows=0, without writing any output pair. Invalid contracts leave
 * all outputs untouched. Other failures may modify workspace/output and
 * must be treated as poisoned; no successful result is published. Each
 * invocation resets workspace, so explicit retry/reuse is safe. No hidden
 * allocation of row-proportional storage or CPU join fallback is permitted.
 */
pgaccel_status pgaccel_join_rows_execute(
    const pgaccel_join_rows_request* request, void* workspace, uint64_t workspace_bytes,
    pgaccel_join_row_pair* output, uint64_t output_capacity,
    pgaccel_join_rows_result* result);

#define PGACCEL_JOIN_CONTROL_VERSION 1u

/* Called synchronously on the calling host thread, before dispatch and after
 * each completed device fill/kernel. Zero continues; nonzero cancels. Must
 * not throw, longjmp, call PostgreSQL APIs or mutate any input/workspace.
 * It may read an interrupt flag. The context remains caller-owned. */
typedef int (*pgaccel_join_cancel_check)(void* context);

typedef struct {
  uint32_t version;
  uint32_t reserved;
  /* Maximum work-items in one launch. Prefix-scan items touch at most 256
   * rows each. Probe/emit items visit at most chain_steps links per round. */
  uint64_t work_items;
  uint64_t chain_steps;
  pgaccel_join_cancel_check cancel_check;
  void* context;
} pgaccel_join_control;

typedef struct {
  pgaccel_join_rows_result rows;
  uint32_t cancelled;
  uint32_t reserved;
  uint64_t cancel_checks;
} pgaccel_join_controlled_result;

/* Pure bounded-driver layout query. Both limits must be in 1..INT32_MAX.
 * Null callback requires null context. Output must not alias control,
 * request or any input. Includes all device cursor/progress/status storage;
 * there is no hidden row-proportional host or device allocation. */
pgaccel_status pgaccel_join_rows_controlled_workspace_bytes(
    const pgaccel_join_rows_request* request, const pgaccel_join_control* control,
    uint64_t* bytes);

/* Same equality/output contract as execute, with bounded build/probe/scan/emit
 * launches. Probe cursors/counts and emit positions remain resident between
 * rounds; only scalar progress and final cardinality return to the host.
 * Workspace/output/result must additionally be disjoint from control.
 * Callback cancellation returns TIMEOUT with cancelled=1 and emitted_rows=0.
 * Partial output, if any, is poisoned and must not be consumed. A running
 * kernel is synchronous: cancellation is observed only at its next boundary.
 * A fresh invocation resets the complete state, including after cancellation.
 * Limits are caller admission parameters, not a wall-clock deadline. */
pgaccel_status pgaccel_join_rows_execute_controlled(
    const pgaccel_join_rows_request* request, const pgaccel_join_control* control,
    void* workspace, uint64_t workspace_bytes, pgaccel_join_row_pair* output,
    uint64_t output_capacity, pgaccel_join_controlled_result* result);

/* Bounded device cardinality only: same workspace, validation and cancellation
 * contract as execute_controlled, without output allocation or pair emission.
 * Success is OK with rows.required_rows equal to exact join multiplicity and
 * rows.emitted_rows == 0. Count-only does not return OOM for nonzero cardinality;
 * OOM remains a real execution/allocation error. Global offset propagation and
 * emit traversal are skipped. No rows or partial counts are published on cancel. */
pgaccel_status pgaccel_join_rows_count_controlled(
    const pgaccel_join_rows_request* request, const pgaccel_join_control* control,
    void* workspace, uint64_t workspace_bytes, pgaccel_join_controlled_result* result);

#define PGACCEL_JOIN_PROJECT_VERSION 1u
#define PGACCEL_JOIN_PROJECT_MAX_COLUMNS 16u
#define PGACCEL_JOIN_PROJECT_OUTER 0u
#define PGACCEL_JOIN_PROJECT_INNER 1u

/* Bit-preserving fixed-width projection, not a cast or expression evaluator.
 * Width is 1, 4 or 8 bytes. SQL NULL (including an unmatched inner row) emits
 * zero value bits and a canonical 1 null byte; non-NULL emits null byte 0. */
typedef struct {
  uint32_t side;
  uint32_t width;
  const void* values;
  const uint8_t* nulls;
  uint64_t value_bytes;
  uint64_t null_bytes;
  void* output_values;
  uint8_t* output_nulls;
  uint64_t output_value_bytes;
  uint64_t output_null_bytes;
} pgaccel_join_project_column;

/* Error word, alignment padding and bounded device metadata. */
#define PGACCEL_JOIN_PROJECT_WORKSPACE_BYTES 1160u

typedef struct {
  uint32_t version;
  uint32_t column_count;
  uint64_t reserved;
  uint64_t outer_rows;
  uint64_t inner_rows;
  uint64_t row_count;
  const pgaccel_join_row_pair* pairs;
  uint64_t pair_bytes;
  pgaccel_join_project_column columns[PGACCEL_JOIN_PROJECT_MAX_COLUMNS];
} pgaccel_join_project_request;

typedef struct {
  uint64_t emitted_rows;
  uint64_t kernel_launches;
} pgaccel_join_project_result;

/* Resident join -> column-consumer boundary. Inputs may alias other inputs.
 * All writable spans (including the caller-owned device workspace and
 * the host result) must be disjoint from all inputs, request and each other.
 * Every device pointer must describe the declared live allocation. Source
 * columns have exactly their side's row domain; the pair prefix determines
 * output order/cardinality. Empty spans use NULL pointers. Inactive columns
 * are zero. Only the initialized pair prefix is read; unused capacity is not.
 *
 * Pair-index validation runs on the GPU before projection. Invalid indices
 * return INVALID_ARGUMENT without touching output columns or host result.
 * Other execution errors may poison device outputs; no result is published.
 * Only the scalar validation status crosses to the host. All row work and
 * output columns stay resident; the caller budgets and owns every buffer.
 * Workspace is 8-byte aligned and at least WORKSPACE_BYTES bytes. Fixed-size
 * column metadata is copied into it; kernels capture only device pointers,
 * not an expanded pointer-rich host descriptor. No row values are staged.
 */
pgaccel_status pgaccel_join_rows_project(
    const pgaccel_join_project_request* request, void* workspace, uint64_t workspace_bytes,
    pgaccel_join_project_result* result);

/* Same projection contract and workspace as project, with at most work_items
 * rows per validation/gather kernel (each row has at most MAX_COLUMNS lanes).
 * Uses the control callback/limits contract above; chain_steps is validated
 * but unused. All pair indices are checked before any output column is written.
 * Control must be disjoint from every writable span. Cancellation publishes
 * TIMEOUT/cancelled=1/emitted_rows=0; discard all partially written columns.
 * On success required_rows == emitted_rows == request.row_count. */
pgaccel_status pgaccel_join_rows_project_controlled(
    const pgaccel_join_project_request* request, const pgaccel_join_control* control,
    void* workspace, uint64_t workspace_bytes, pgaccel_join_controlled_result* result);

#ifdef __cplusplus
}
#endif
#endif
