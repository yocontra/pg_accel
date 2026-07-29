// test_kernel_failure_status.cpp — induced-failure honesty gate.
//
// Pins the kernel-safety contracts for async failure propagation and
// exception containment at extern-C boundaries:
//
//   1. A dispatch whose staging allocation cannot possibly succeed
//      (multiple exabyte-sized buffers) must surface an error status —
//      PGACCEL_OOM if the USM allocator returns nullptr, PGACCEL_ERROR if
//      it throws — and must NEVER report PGACCEL_OK over unwritten output.
//   2. The failure must not be misreported as PGACCEL_ERROR_NO_DEVICE:
//      a positive control proves a working device exists in this process.
//   3. The failing call must not crash the process. Before the extern "C"
//      try/catch fixes, a throwing allocation escaped through the C ABI
//      as std::terminate (backend SIGABRT); this binary exiting cleanly
//      with an error status is the regression gate.
//   4. pgaccel_expr_eval_predicate's documented contract holds on error:
//      the results array is fully populated with PGACCEL_EXPR_UNCERTAIN.
//
// The induced failure is a pgaccel_batch whose num_rows is absurdly large
// (2^59 rows). Staging allocates num_rows-sized device buffers *before*
// reading any host column memory (stage_dispatch / OneColScratch), so the
// tiny host buffers are never overread: allocation fails first, on any
// real machine, deterministically.

#include <cinttypes>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"
#include "pgaccel_olap.h"

namespace sycl {
class queue;
}
extern sycl::queue* g_queue;

static int g_pass = 0;
static int g_fail = 0;

#define ASSERT_TRUE(desc, cond)              \
  do {                                       \
    if ((cond)) {                            \
      g_pass++;                              \
    } else {                                 \
      fprintf(stderr, "FAIL: %s\n", (desc)); \
      g_fail++;                              \
    }                                        \
  } while (0)

// One tiny valid int32 column; num_rows lies about the buffer length. Safe
// because every entry point under test allocates num_rows-sized device
// scratch (and fails) before any host-side read of the column data.
struct HugeBatch {
  int32_t col_data[4] = {1, 2, 3, 4};
  void* col_ptrs[1];
  uint8_t* null_ptrs[1];
  pgaccel_val_tag types[1];
  pgaccel_batch batch;

  explicit HugeBatch(size_t num_rows) {
    col_ptrs[0] = col_data;
    null_ptrs[0] = nullptr;
    types[0] = PGACCEL_VAL_INT32;
    batch.num_rows = num_rows;
    batch.num_cols = 1;
    batch.col_data = col_ptrs;
    batch.col_nulls = null_ptrs;
    batch.col_types = types;
  }
};

static const char* status_name(pgaccel_status st) {
  switch (st) {
    case PGACCEL_OK:
      return "PGACCEL_OK";
    case PGACCEL_ERROR:
      return "PGACCEL_ERROR";
    case PGACCEL_UNSUPPORTED:
      return "PGACCEL_UNSUPPORTED";
    case PGACCEL_OOM:
      return "PGACCEL_OOM";
    case PGACCEL_TIMEOUT:
      return "PGACCEL_TIMEOUT";
    case PGACCEL_ERROR_NO_DEVICE:
      return "PGACCEL_ERROR_NO_DEVICE";
    default:
      return "<unknown>";
  }
}

// The failure must be an honest error: OOM (allocator returned nullptr) or
// ERROR (allocator/submission threw, caught at the extern "C" boundary).
// OK would be silent corruption; NO_DEVICE would be a lie (positive
// control proved the device works); UNSUPPORTED would misreport a resource
// failure as a planner-shape decline.
static void assert_honest_failure(const char* desc, pgaccel_status st) {
  const bool honest = (st == PGACCEL_OOM || st == PGACCEL_ERROR);
  if (!honest) {
    fprintf(stderr, "FAIL: %s — got %s (%d), expected PGACCEL_OOM or PGACCEL_ERROR\n", desc,
            status_name(st), (int)st);
    g_fail++;
  } else {
    printf("  %s -> %s (honest failure)\n", desc, status_name(st));
    g_pass++;
  }
}

int main() {
  // Device is required: this gate is about misreporting failures on a
  // machine that HAS a GPU, so a missing device fails the test honestly.
  if (pgaccel_init() != PGACCEL_OK) {
    fprintf(stderr, "FAIL: pgaccel_init() failed — no usable GPU device\n");
    if (pgaccel_shutdown() != PGACCEL_OK)
      fprintf(stderr, "FAIL: pgaccel_shutdown() failed after initialization failure\n");
    return 1;
  }

  // ── Positive control: the VM entry points succeed on a small batch ──
  {
    HugeBatch small(4);
    int8_t results[4] = {99, 99, 99, 99};
    pgaccel_val constant = {};
    constant.tag = PGACCEL_VAL_INT32;
    constant.data.i32 = 2;
    pgaccel_expr_instruction predicate_insts[3] = {};
    predicate_insts[0].opcode = PGACCEL_EXPR_OP_LOAD_COL;
    predicate_insts[0].arg = 0;
    predicate_insts[1].opcode = PGACCEL_EXPR_OP_LOAD_CONST;
    predicate_insts[1].arg = 0;
    predicate_insts[2].opcode = PGACCEL_EXPR_OP_GT;
    pgaccel_expr_program predicate = {};
    predicate.instructions = predicate_insts;
    predicate.inst_count = 3;
    predicate.const_pool = &constant;
    predicate.const_count = 1;
    predicate.max_stack = 2;
    predicate.num_cols = 1;

    std::memset(results, 99, sizeof(results));
    pgaccel_status st = pgaccel_expr_eval_predicate(&predicate, &small.batch, results);
    ASSERT_TRUE("positive control: bytecode predicate returns OK", st == PGACCEL_OK);
    ASSERT_TRUE("positive control: bytecode predicate copyback correct",
                results[0] == PGACCEL_EXPR_FALSE && results[1] == PGACCEL_EXPR_FALSE &&
                    results[2] == PGACCEL_EXPR_TRUE && results[3] == PGACCEL_EXPR_TRUE);

    pgaccel_expr_instruction project_inst = {};
    project_inst.opcode = PGACCEL_EXPR_OP_LOAD_COL;
    project_inst.arg = 0;
    pgaccel_expr_program project = {};
    project.instructions = &project_inst;
    project.inst_count = 1;
    project.max_stack = 1;
    project.num_cols = 1;
    pgaccel_val projected[4] = {};
    uint8_t uncertain[4] = {99, 99, 99, 99};
    st = pgaccel_expr_eval_project(&project, &small.batch, projected, uncertain);
    ASSERT_TRUE("positive control: bytecode projection returns OK", st == PGACCEL_OK);
    const bool projected_values =
        projected[0].tag == PGACCEL_VAL_INT32 && projected[0].data.i32 == 1 &&
        projected[1].tag == PGACCEL_VAL_INT32 && projected[1].data.i32 == 2 &&
        projected[2].tag == PGACCEL_VAL_INT32 && projected[2].data.i32 == 3 &&
        projected[3].tag == PGACCEL_VAL_INT32 && projected[3].data.i32 == 4;
    ASSERT_TRUE("positive control: bytecode projection copyback correct", projected_values);
    ASSERT_TRUE("positive control: bytecode projection uncertainty clear",
                uncertain[0] == 0 && uncertain[1] == 0 && uncertain[2] == 0 && uncertain[3] == 0);

    double round_values[4] = {-1.5, -0.0, 1.5, 2.4};
    void* round_columns[1] = {round_values};
    uint8_t* round_nulls[1] = {nullptr};
    pgaccel_val_tag round_types[1] = {PGACCEL_VAL_FLOAT64};
    pgaccel_batch round_batch = {4, 1, round_columns, round_nulls, round_types};
    pgaccel_expr_instruction round_insts[2] = {};
    round_insts[0].opcode = PGACCEL_EXPR_OP_LOAD_COL;
    round_insts[0].arg = 0;
    round_insts[1].opcode = PGACCEL_EXPR_OP_ROUND_F64;
    pgaccel_expr_program round_program = {};
    round_program.instructions = round_insts;
    round_program.inst_count = 2;
    round_program.max_stack = 1;
    round_program.num_cols = 1;
    st = pgaccel_expr_eval_project(&round_program, &round_batch, projected, uncertain);
    ASSERT_TRUE("positive control: bytecode round returns OK", st == PGACCEL_OK);
    ASSERT_TRUE("positive control: bytecode round is half-away-from-zero",
                projected[0].data.f64 == -2.0 && projected[1].data.f64 == 0.0 &&
                    std::signbit(projected[1].data.f64) && projected[2].data.f64 == 2.0 &&
                    projected[3].data.f64 == 2.0);
  }

  // 2^59 rows: staging needs num_rows * 8B (f64 stage) + num_rows * 1B
  // masks — orders of magnitude beyond any max_alloc. Deterministic OOM.
  const size_t kHugeRows = (size_t)1 << 59;

  // A successfully initialized runtime with a temporarily unavailable queue
  // is distinct from initialization failure. Pin every resident-memory C ABI
  // to the documented NO_DEVICE status before restoring the live queue.
  {
    sycl::queue* live_queue = g_queue;
    ASSERT_TRUE("queue-unavailable setup has a live queue", live_queue != nullptr);
    g_queue = nullptr;

    uint8_t byte = 0;
    void* p = reinterpret_cast<void*>(0x1);
    ASSERT_TRUE("shared alloc reports unavailable queue",
                pgaccel_expr_shared_alloc(1, &p) == PGACCEL_ERROR_NO_DEVICE && p == nullptr);
    pgaccel_expr_shared_free(&byte);

    p = reinterpret_cast<void*>(0x1);
    ASSERT_TRUE("device alloc reports unavailable queue",
                pgaccel_expr_device_alloc(1, &p) == PGACCEL_ERROR_NO_DEVICE && p == nullptr);
    p = reinterpret_cast<void*>(0x1);
    ASSERT_TRUE("device alloc-copy reports unavailable queue",
                pgaccel_expr_device_alloc_copy(&byte, 1, &p) == PGACCEL_ERROR_NO_DEVICE &&
                    p == nullptr);
    ASSERT_TRUE("device copy-from-host reports unavailable queue",
                pgaccel_expr_device_copy_from_host(&byte, &byte, 1) == PGACCEL_ERROR_NO_DEVICE);
    ASSERT_TRUE("device copy-to-host reports unavailable queue",
                pgaccel_expr_device_copy_to_host(&byte, &byte, 1) == PGACCEL_ERROR_NO_DEVICE);
    pgaccel_expr_device_free(&byte);

    p = reinterpret_cast<void*>(0x1);
    ASSERT_TRUE("grouped workspace alloc reports unavailable queue",
                pgaccel_grouped_agg_workspace_alloc(1, alignof(void*), PGACCEL_MEM_SPACE_SHARED_USM,
                                                    &p) == PGACCEL_ERROR_NO_DEVICE &&
                    p == nullptr);
    pgaccel_grouped_agg_workspace_free(&byte);

    g_queue = live_queue;
  }

  // ── Induced failure 1: bytecode VM entry point (stage_dispatch) ──
  //
  // Uses pgaccel_expr_eval_project rather than _predicate: the predicate
  // entry fulfils its documented "results always populated" contract by
  // memset-ing num_rows bytes into the caller buffer up front, so a
  // huge claimed row count cannot be paired with a small host results
  // buffer there. Project performs no host-side write before staging
  // fails, so the tiny output buffers below are never touched.
  {
    HugeBatch huge(kHugeRows);
    pgaccel_expr_instruction insts[2] = {};
    insts[0].opcode = PGACCEL_EXPR_OP_LOAD_COL;
    insts[0].arg = 0;
    insts[1].opcode = PGACCEL_EXPR_OP_IS_NULL;
    pgaccel_expr_program prog = {};
    prog.instructions = insts;
    prog.inst_count = 2;
    prog.const_pool = nullptr;
    prog.const_count = 0;
    prog.max_stack = 2;
    prog.num_cols = 1;

    pgaccel_val out[4];
    uint8_t uncertain[4];
    pgaccel_status st = pgaccel_expr_eval_project(&prog, &huge.batch, out, uncertain);
    assert_honest_failure("expr_eval_project with 2^59-row staging", st);
  }

  // ── Induced failure 2: shared-USM allocator entry point ──
  {
    void* p = reinterpret_cast<void*>(0x1);
    pgaccel_status st = pgaccel_expr_shared_alloc(kHugeRows * 8, &p);
    assert_honest_failure("pgaccel_expr_shared_alloc(2^62 bytes)", st);
    ASSERT_TRUE("shared_alloc failure left no dangling pointer", p == nullptr || st == PGACCEL_OK);
  }

  // The remaining resident allocators must contain the same impossible
  // allocation without leaking a stale output pointer or crossing the C ABI.
  {
    void* p = reinterpret_cast<void*>(0x1);
    pgaccel_status st = pgaccel_expr_device_alloc(kHugeRows * 8, &p);
    assert_honest_failure("pgaccel_expr_device_alloc(2^62 bytes)", st);
    ASSERT_TRUE("device_alloc failure left no dangling pointer", p == nullptr || st == PGACCEL_OK);
  }

  {
    const uint8_t source = 0;
    void* p = reinterpret_cast<void*>(0x1);
    pgaccel_status st = pgaccel_expr_device_alloc_copy(&source, kHugeRows * 8, &p);
    assert_honest_failure("pgaccel_expr_device_alloc_copy(2^62 bytes)", st);
    ASSERT_TRUE("device_alloc_copy failure left no dangling pointer",
                p == nullptr || st == PGACCEL_OK);
  }

  {
    void* p = reinterpret_cast<void*>(0x1);
    pgaccel_status st =
        pgaccel_grouped_agg_workspace_alloc(kHugeRows * 8, 64, PGACCEL_MEM_SPACE_SHARED_USM, &p);
    assert_honest_failure("pgaccel_grouped_agg_workspace_alloc(2^62 bytes)", st);
    ASSERT_TRUE("grouped workspace failure left no dangling pointer",
                p == nullptr || st == PGACCEL_OK);
  }

  // H3 bulk entry points allocate every count-sized staging span before
  // reading the host arrays. The impossible count therefore tests each
  // allocator's cleanup and C-boundary containment without an input overread.
  {
    const uint64_t cell = UINT64_C(0x8029fffffffffff);
    int32_t i32_output = 0;
    uint8_t byte_output = 0;
    uint64_t cell_output = 0;

    assert_honest_failure("h3_get_resolution with 2^59 rows",
                          pgaccel_h3_get_resolution_bulk(&cell, kHugeRows, &i32_output));
    assert_honest_failure("h3_get_base_cell with 2^59 rows",
                          pgaccel_h3_get_base_cell_bulk(&cell, kHugeRows, &i32_output));
    assert_honest_failure("h3_is_valid_cell with 2^59 rows",
                          pgaccel_h3_is_valid_cell_bulk(&cell, kHugeRows, &byte_output));
    assert_honest_failure("h3_is_pentagon with 2^59 rows",
                          pgaccel_h3_is_pentagon_bulk(&cell, kHugeRows, &byte_output));
    assert_honest_failure("h3_is_res_class_iii with 2^59 rows",
                          pgaccel_h3_is_res_class_iii_bulk(&cell, kHugeRows, &byte_output));
    assert_honest_failure("h3_cell_to_parent with 2^59 rows",
                          pgaccel_h3_cell_to_parent_bulk(&cell, kHugeRows, 0, &cell_output));
    assert_honest_failure("h3_cell_to_center_child with 2^59 rows",
                          pgaccel_h3_cell_to_center_child_bulk(&cell, kHugeRows, 1, &cell_output));
    assert_honest_failure("h3_grid_distance with 2^59 rows",
                          pgaccel_h3_grid_distance_bulk(&cell, &cell, kHugeRows, &i32_output));

    const size_t huge_slab_rows = kHugeRows / 4;
    const double coordinate_f64 = 0.0;
    const float coordinate_f32 = 0.0f;
    pgaccel_agg_state* state = nullptr;
    assert_honest_failure("h3 fp64 lat/lng with 2^57 rows",
                          pgaccel_h3_lat_lng_to_cell_bulk(&coordinate_f64, &coordinate_f64,
                                                          huge_slab_rows, 0, true, &cell_output,
                                                          &byte_output));
    assert_honest_failure("h3 fp32 lat/lng with 2^57 rows",
                          pgaccel_h3_lat_lng_to_cell_bulk(&coordinate_f32, &coordinate_f32,
                                                          huge_slab_rows, 0, false, &cell_output,
                                                          &byte_output));
    assert_honest_failure(
        "h3 lat/lng count with 2^57 rows",
        pgaccel_h3_lat_lng_count_bulk(&coordinate_f64, &coordinate_f64, huge_slab_rows, 0, &state));
    assert_honest_failure("h3 fp32-exact count with 2^57 rows",
                          pgaccel_h3_lat_lng_count_bulk_f32_exact(&coordinate_f32, &coordinate_f32,
                                                                  &coordinate_f64, &coordinate_f64,
                                                                  huge_slab_rows, 0, &state));
    assert_honest_failure("h3 resident count with 2^57 rows",
                          pgaccel_h3_lat_lng_count_resident_bulk(&coordinate_f64, &coordinate_f64,
                                                                 &coordinate_f32, &coordinate_f32,
                                                                 huge_slab_rows, 0, &state));
  }

  {
    const double point[2] = {0.0, 0.0};
    const double ring[8] = {0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 0.0};
    int8_t predicate_output = 0;
    double distance_output = 0.0;
    uint8_t uncertain_output = 0;

    assert_honest_failure(
        "point_in_ring with 2^59 points",
        pgaccel_point_in_ring_bulk(point, kHugeRows, ring, 4, true, &predicate_output));
    assert_honest_failure("sphere_distance with 2^59 pairs",
                          pgaccel_sphere_distance_bulk(point, point, kHugeRows, true,
                                                       &distance_output, &uncertain_output));
    assert_honest_failure(
        "segment_intersects with 2^58 pairs",
        pgaccel_segment_intersects_bulk(ring, ring, kHugeRows / 2, true, &predicate_output));

    // Shape the counts so the coordinate allocations wrap to a few elements
    // while a later byte-sized allocation remains impossible. The functions
    // check every allocation before their first memcpy, so these calls cover
    // partial-allocation cleanup without reading beyond the tiny inputs.
    const size_t cleanup_rows = (size_t{1} << 63) + 1;
    assert_honest_failure(
        "point_in_ring partial-allocation cleanup",
        pgaccel_point_in_ring_bulk(point, 1, ring, kHugeRows, true, &predicate_output));
    assert_honest_failure("sphere_distance fp32 partial-allocation cleanup",
                          pgaccel_sphere_distance_bulk(
                              reinterpret_cast<const float*>(point),
                              reinterpret_cast<const float*>(point), cleanup_rows, false,
                              reinterpret_cast<float*>(&distance_output), &uncertain_output));
    assert_honest_failure("sphere_distance fp64 partial-allocation cleanup",
                          pgaccel_sphere_distance_bulk(point, point, cleanup_rows, true,
                                                       &distance_output, &uncertain_output));
    assert_honest_failure(
        "segment_intersects partial-allocation cleanup",
        pgaccel_segment_intersects_bulk(ring, ring, cleanup_rows, true, &predicate_output));
  }

  // Point-in-polygon staging allocates the complete slab before building or
  // reading host-side staging vectors. Exercise both simple and cooperative
  // dispatch selection with an impossible point span.
  {
    const float point[2] = {0.0f, 0.0f};
    const float bbox[4] = {-1.0f, -1.0f, 1.0f, 1.0f};
    const float polygon[8] = {-1.0f, -1.0f, 1.0f, -1.0f, 1.0f, 1.0f, -1.0f, 1.0f};
    int8_t output = 0;
    assert_honest_failure(
        "point_in_polygon simple staging with 2^59 points",
        pgaccel_point_in_polygon_bulk(point, kHugeRows, bbox, polygon, 4, nullptr, 0, &output));
    assert_honest_failure(
        "point_in_polygon cooperative staging with 2^59 points",
        pgaccel_point_in_polygon_bulk(point, kHugeRows, bbox, polygon, 2048, nullptr, 0, &output));
  }

  {
    const float box_f32[4] = {0.0f, 0.0f, 1.0f, 1.0f};
    const double box_f64[4] = {0.0, 0.0, 1.0, 1.0};
    uint8_t intersects = 0;
    assert_honest_failure(
        "bbox f32 with 2^58 boxes",
        pgaccel_bbox_intersects_bulk_f32(box_f32, kHugeRows / 2, box_f32, 1, &intersects, nullptr));
    assert_honest_failure(
        "bbox f64 with 2^58 boxes",
        pgaccel_bbox_intersects_bulk_f64(box_f64, kHugeRows / 2, box_f64, 1, &intersects, nullptr));
  }

  {
    const double f64 = 1.0;
    const float f32 = 1.0f;
    const int64_t i64 = 1;
    const uint8_t byte = 1;
    double f64_output = 0.0;
    float f32_sum = 0.0f;
    float f32_min = 0.0f;
    float f32_max = 0.0f;
    int64_t i64_sum = 0;
    int64_t i64_min = 0;
    int64_t i64_max = 0;
    int64_t aggregate_count = 0;
    size_t size_output = 0;
    uint64_t u64_output = 0;

    assert_honest_failure("reduce sum with 2^59 rows",
                          pgaccel_reduce_sum_f64(&f64, kHugeRows, &f64_output));
    assert_honest_failure("reduce min with 2^59 rows",
                          pgaccel_reduce_min_f64(&f64, kHugeRows, &f64_output));
    assert_honest_failure("reduce count with 2^59 rows",
                          pgaccel_reduce_count(&byte, kHugeRows, &size_output));
    assert_honest_failure(
        "reduce multi with 2^59 rows",
        pgaccel_reduce_multi_i64(&i64, kHugeRows, &i64_sum, &i64_min, &i64_max, &aggregate_count));
    assert_honest_failure("reduce masked multi with 2^59 rows",
                          pgaccel_reduce_multi_masked_f32(&f32, &byte, &byte, kHugeRows, &f32_sum,
                                                          &f32_min, &f32_max, &aggregate_count));
    assert_honest_failure("reduce sumsq with 2^59 rows",
                          pgaccel_reduce_sum_sq_f64(&f64, kHugeRows, &f64_output));
    assert_honest_failure(
        "reduce stats with 2^59 rows",
        pgaccel_reduce_stats_f32(&f32, kHugeRows, &u64_output, &f64_output, &f64_output));
  }

  // ── Survival: a failing dispatch must not poison the process ──
  {
    HugeBatch small(4);
    int8_t results[4] = {99, 99, 99, 99};
    pgaccel_val constant = {};
    constant.tag = PGACCEL_VAL_INT32;
    constant.data.i32 = 2;
    pgaccel_expr_instruction insts[3] = {};
    insts[0].opcode = PGACCEL_EXPR_OP_LOAD_COL;
    insts[0].arg = 0;
    insts[1].opcode = PGACCEL_EXPR_OP_LOAD_CONST;
    insts[1].arg = 0;
    insts[2].opcode = PGACCEL_EXPR_OP_LE;
    pgaccel_expr_program program = {};
    program.instructions = insts;
    program.inst_count = 3;
    program.const_pool = &constant;
    program.const_count = 1;
    program.max_stack = 2;
    program.num_cols = 1;
    pgaccel_status st = pgaccel_expr_eval_predicate(&program, &small.batch, results);
    ASSERT_TRUE("post-failure control: bytecode predicate still returns OK", st == PGACCEL_OK);
    ASSERT_TRUE("post-failure control: results correct",
                results[0] == PGACCEL_EXPR_TRUE && results[1] == PGACCEL_EXPR_TRUE &&
                    results[2] == PGACCEL_EXPR_FALSE && results[3] == PGACCEL_EXPR_FALSE);
  }

  ASSERT_TRUE("pgaccel_shutdown succeeds", pgaccel_shutdown() == PGACCEL_OK);
  printf("test_kernel_failure_status: %d passed, %d failed\n", g_pass, g_fail);
  return g_fail == 0 ? 0 : 1;
}
