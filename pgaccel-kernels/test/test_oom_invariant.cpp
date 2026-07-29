// test_oom_invariant.cpp - OOM-never invariant for live kernel families.
//
// Each live family (reduce / expression VM / grouped aggregation / spatial /
// H3) processes min(2 * max_alloc, 2 GiB) logical input. The 2 GiB cap is
// greater than 2 GB and keeps the release gate feasible on large-memory
// devices. Bounded-lifecycle families generate that logical input from chunks;
// APIs that accept host spans must stream internally. Each family must:
//
//   (a) complete without OOM, bad_alloc, SIGSEGV, SIGKILL
//   (b) return a correct result (cross-check against a small-input
//       reference extended by multiplication, e.g. sum of 2N uniform
//       values = 2× sum of N)
//   (c) peak RSS stay below 3 × caps.max_alloc_bytes — proving the
//       kernel streams rather than buffering the full input
//
// If any kernel fails these invariants, the test reports FAIL and
// returns a specific exit code so the dispatcher can route the
// contingency work (streaming/chunking fix) — DO NOT make the test
// pass by relaxing the ceiling; that masks a real streaming bug.
//
#include <sys/resource.h>
#if defined(__APPLE__)
#include <mach/mach.h>
#include <mach/mach_init.h>
#include <mach/task.h>
#else
#include <unistd.h>
#endif

#include <algorithm>
#include <cctype>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <random>
#include <string>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"
#include "pgaccel_olap.h"

static size_t peak_rss_bytes() {
  // task_info() gives current resident; getrusage() gives peak.
  struct rusage ru;
  if (getrusage(RUSAGE_SELF, &ru) == 0) {
    // ru_maxrss is bytes on macOS (KB on Linux).
#if defined(__APPLE__)
    return static_cast<size_t>(ru.ru_maxrss);
#else
    return static_cast<size_t>(ru.ru_maxrss) * 1024ULL;
#endif
  }
  return 0;
}

static size_t current_rss_bytes() {
#if defined(__APPLE__)
  mach_task_basic_info_data_t info;
  mach_msg_type_number_t count = MACH_TASK_BASIC_INFO_COUNT;
  kern_return_t kr = task_info(mach_task_self(), MACH_TASK_BASIC_INFO,
                               reinterpret_cast<task_info_t>(&info), &count);
  if (kr != KERN_SUCCESS)
    return 0;
  return info.resident_size;
#else
  FILE* f = std::fopen("/proc/self/statm", "r");
  if (!f)
    return 0;
  long resident_pages = 0;
  int matched = std::fscanf(f, "%*s %ld", &resident_pages);
  std::fclose(f);
  if (matched != 1 || resident_pages < 0)
    return 0;
  long page_size = sysconf(_SC_PAGESIZE);
  if (page_size <= 0)
    return 0;
  return static_cast<size_t>(resident_pages) * static_cast<size_t>(page_size);
#endif
}

struct FamilyResult {
  const char* name;
  bool status_ok;
  bool correct;
  size_t peak_rss_bytes;
  size_t rss_ceiling_bytes;
  bool under_ceiling;
  std::string note;
  size_t rss_baseline_bytes = 0;
  size_t rss_delta_bytes = 0;
  uint64_t gpu_dispatches = 0;
};

constexpr size_t kLifecycleChunkBytes = size_t{32} * 1024 * 1024;

static FamilyResult run_reduce_family(size_t N, size_t rss_ceiling) {
  FamilyResult r = {"reduce_f64", false, false, 0, rss_ceiling, false, ""};
  printf("\n-- reduce_f64 @ N=%zu (%.2f GB raw input) --\n", N,
         static_cast<double>(N * sizeof(double)) / (1024.0 * 1024.0 * 1024.0));

  // Use uniform value 1.0 so correctness check is trivial: sum == N.
  // Allocating the full vector on CPU side is the whole point — we
  // want to test that the kernel streams the input through the device
  // rather than copying it whole.
  std::vector<double> v;
  try {
    v.assign(N, 1.0);
  } catch (const std::bad_alloc&) {
    r.note = "CPU-side vector allocation failed (host cannot hold input)";
    return r;
  }
  const size_t rss_before = current_rss_bytes();
  double got = 0.0;
  pgaccel_reset_gpu_exec_count();
  pgaccel_status st = pgaccel_reduce_sum_f64(v.data(), N, &got);
  r.gpu_dispatches = pgaccel_gpu_exec_count();
  const size_t rss_after = peak_rss_bytes();
  r.status_ok = (st == PGACCEL_OK && r.gpu_dispatches > 0);
  r.peak_rss_bytes = rss_after;
  r.rss_baseline_bytes = rss_before;
  // Correctness: sum of N 1.0s is N exactly in fp64.
  r.correct = r.status_ok && got == static_cast<double>(N);
  // RSS ceiling — delta from before-call vs peak.
  const size_t rss_delta = rss_after > rss_before ? rss_after - rss_before : 0;
  r.rss_delta_bytes = rss_delta;
  r.under_ceiling = rss_delta <= rss_ceiling;
  printf("   status=%d (OK=%d) got=%.0f expected=%.0f correct=%d  "
         "dispatches=%llu rss_before=%.2fGB peak=%.2fGB delta=%.2fGB ceiling=%.2fGB under=%d\n",
         (int)st, r.status_ok, got, (double)N, r.correct,
         static_cast<unsigned long long>(r.gpu_dispatches), rss_before / 1e9, rss_after / 1e9,
         rss_delta / 1e9, rss_ceiling / 1e9, r.under_ceiling);
  if (!r.status_ok)
    r.note = "kernel returned non-OK status on device-exceeding input";
  else if (!r.correct)
    r.note = "result differs from expected (sum of N 1.0s)";
  return r;
}

static FamilyResult run_expr_vm_family(size_t N, size_t rss_ceiling) {
  FamilyResult r = {"expr_vm_f64", false, false, 0, rss_ceiling, false, ""};
  // Eight physical columns cut per-dispatch USM allocation churn by 8x without
  // multiplying row/result processing by the 64x cost of a one-column shape.
  // Chunking still makes the device execute LOAD_COL for all N float64 values.
  constexpr size_t kColumnCount = 8;
  const size_t num_cols = std::min(kColumnCount, N);
  if (num_cols == 0) {
    r.note = "expr-VM logical input is empty";
    return r;
  }

  const size_t full_rows = N / num_cols;
  const size_t tail_values = N % num_cols;
  // Bound each command buffer as well as each allocation. Long-running Metal
  // command buffers can be terminated by the OS even when memory is bounded.
  const size_t rows_per_chunk = kLifecycleChunkBytes / (num_cols * sizeof(double));
  const size_t capacity = std::min(full_rows, std::max<size_t>(1, rows_per_chunk));

  printf("\n-- expr_vm_f64 @ values=%zu (%.2f GB logical raw input, %zu cols x %zu rows) --\n", N,
         static_cast<double>(N * sizeof(double)) / (1024.0 * 1024.0 * 1024.0), num_cols, full_rows);

  std::vector<double> input;
  std::vector<int8_t> output;
  std::vector<void*> columns(num_cols);
  std::vector<uint8_t*> nulls(num_cols, nullptr);
  std::vector<pgaccel_val_tag> types(num_cols, PGACCEL_VAL_FLOAT64);
  try {
    input.assign(capacity * num_cols, 1.5);
    output.resize(capacity);
  } catch (const std::bad_alloc&) {
    r.note = "bounded expr-VM chunk allocation failed";
    return r;
  }
  for (size_t col = 0; col < num_cols; ++col)
    columns[col] = input.data() + col * capacity;

  auto build_wide_load_program = [](size_t column_count) {
    std::vector<pgaccel_expr_instruction> instructions;
    instructions.reserve(column_count * 3 - 1);
    for (size_t col = 0; col < column_count; ++col) {
      pgaccel_expr_instruction load = {};
      load.opcode = PGACCEL_EXPR_OP_LOAD_COL;
      load.arg = static_cast<uint32_t>(col);
      instructions.push_back(load);
      pgaccel_expr_instruction is_not_null = {};
      is_not_null.opcode = PGACCEL_EXPR_OP_IS_NOT_NULL;
      instructions.push_back(is_not_null);
      if (col > 0) {
        pgaccel_expr_instruction and_op = {};
        and_op.opcode = PGACCEL_EXPR_OP_AND;
        instructions.push_back(and_op);
      }
    }
    return instructions;
  };

  // The predicate semantically ANDs an IS_NOT_NULL result for every column, so
  // every physical lane must be read to produce the final value.
  std::vector<pgaccel_expr_instruction> instructions = build_wide_load_program(num_cols);
  pgaccel_expr_program program = {};
  program.instructions = instructions.data();
  program.inst_count = instructions.size();
  program.max_stack = std::min<size_t>(2, num_cols);
  program.num_cols = num_cols;

  pgaccel_batch batch = {};
  batch.num_cols = num_cols;
  batch.col_data = columns.data();
  batch.col_nulls = nulls.data();
  batch.col_types = types.data();

  const size_t rss_before = current_rss_bytes();
  pgaccel_reset_gpu_exec_count();
  pgaccel_status st = PGACCEL_OK;
  size_t processed_rows = 0;
  size_t processed_values = 0;
  bool values_ok = true;
  while (processed_rows < full_rows && st == PGACCEL_OK && values_ok) {
    const size_t rows = std::min(capacity, full_rows - processed_rows);
    batch.num_rows = rows;
    std::fill(output.begin(), output.begin() + static_cast<std::ptrdiff_t>(rows), int8_t{99});
    st = pgaccel_expr_eval_predicate(&program, &batch, output.data());
    for (size_t row = 0; row < rows && st == PGACCEL_OK; ++row) {
      if (output[row] != PGACCEL_EXPR_TRUE) {
        values_ok = false;
        break;
      }
    }
    if (st == PGACCEL_OK && values_ok) {
      processed_rows += rows;
      processed_values += rows * num_cols;
    }
  }

  // N is normally exactly divisible by 64. Preserve exact logical-byte
  // accounting for unusual device caps with one final narrower row.
  if (tail_values != 0 && st == PGACCEL_OK && values_ok) {
    std::vector<pgaccel_expr_instruction> tail_instructions = build_wide_load_program(tail_values);
    program.instructions = tail_instructions.data();
    program.inst_count = tail_instructions.size();
    program.max_stack = std::min<size_t>(2, tail_values);
    program.num_cols = tail_values;
    batch.num_cols = tail_values;
    batch.num_rows = 1;
    output[0] = int8_t{99};
    st = pgaccel_expr_eval_predicate(&program, &batch, output.data());
    values_ok = st == PGACCEL_OK && output[0] == PGACCEL_EXPR_TRUE;
    if (values_ok)
      processed_values += tail_values;
  }
  r.gpu_dispatches = pgaccel_gpu_exec_count();
  const size_t rss_after = peak_rss_bytes();
  r.status_ok = (st == PGACCEL_OK && processed_values == N && r.gpu_dispatches > 0);
  r.peak_rss_bytes = rss_after;
  r.rss_baseline_bytes = rss_before;
  r.correct = r.status_ok && values_ok;
  const size_t rss_delta = rss_after > rss_before ? rss_after - rss_before : 0;
  r.rss_delta_bytes = rss_delta;
  r.under_ceiling = rss_delta <= rss_ceiling;
  printf("   status=%d (OK=%d) values_ok=%d processed_values=%zu logical_bytes=%zu "
         "rss_before=%.2fGB peak=%.2fGB delta=%.2fGB "
         "ceiling=%.2fGB dispatches=%llu under=%d\n",
         (int)st, r.status_ok, values_ok, processed_values, processed_values * sizeof(double),
         rss_before / 1e9, rss_after / 1e9, rss_delta / 1e9, rss_ceiling / 1e9,
         static_cast<unsigned long long>(r.gpu_dispatches), r.under_ceiling);
  if (!r.status_ok)
    r.note = "bounded expr-VM predicate did not process the logical input";
  else if (!r.correct)
    r.note = "bounded expr-VM predicate produced incorrect output";
  return r;
}

static FamilyResult run_grouped_agg_family(size_t N, size_t rss_ceiling) {
  FamilyResult r = {"grouped_agg_i32_mul", false, false, 0, rss_ceiling, false, ""};
  constexpr size_t kKeyCount = 3;
  constexpr size_t kDimCount = 3;
  constexpr size_t kMeasureInputCount = 2;
  constexpr size_t kMeasureCount = 2;
  constexpr size_t kKeyCardinality = 256;
  constexpr size_t kGroupCount = kKeyCardinality * kKeyCardinality;
  constexpr size_t kPhysicalColumnCount = kKeyCount + kDimCount + kMeasureInputCount;
  constexpr size_t kBytesPerRow = kPhysicalColumnCount * sizeof(int32_t);
  constexpr size_t kFullCyclesPerChunk = 15;
  constexpr size_t kRowsPerChunk = kFullCyclesPerChunk * kGroupCount;
  static_assert(kRowsPerChunk < 1'000'000);
  static_assert(kRowsPerChunk * kBytesPerRow <= kLifecycleChunkBytes);
  const size_t logical_input_bytes = N * sizeof(double);
  if (logical_input_bytes % kBytesPerRow != 0 ||
      (logical_input_bytes / kBytesPerRow) % kGroupCount != 0) {
    r.note = "grouped-aggregate logical input is not an exact whole-row payload";
    return r;
  }
  const size_t row_count = logical_input_bytes / kBytesPerRow;
  const size_t capacity = std::min(row_count, kRowsPerChunk);
  const size_t expected_lifecycle_calls = (row_count + capacity - 1) / capacity;

  printf("\n-- grouped_agg_i32_mul @ rows=%zu (%.2f GB logical input, %zu B/row: "
         "%zu int32 keys + %zu int32 dimension keys + %zu int32 MUL inputs, %zu groups) --\n",
         row_count, static_cast<double>(logical_input_bytes) / (1024.0 * 1024.0 * 1024.0),
         kBytesPerRow, kKeyCount, kDimCount, kMeasureInputCount, kGroupCount);

  void* input_allocations[kPhysicalColumnCount] = {};
  auto free_inputs = [&]() {
    for (void* allocation : input_allocations)
      pgaccel_expr_shared_free(allocation);
  };

  pgaccel_status status = PGACCEL_OK;
  for (size_t input = 0; input < kPhysicalColumnCount && status == PGACCEL_OK; ++input)
    status = pgaccel_expr_shared_alloc(capacity * sizeof(int32_t), &input_allocations[input]);
  bool all_inputs_allocated = status == PGACCEL_OK;
  for (const void* allocation : input_allocations)
    all_inputs_allocated = all_inputs_allocated && allocation != nullptr;
  if (!all_inputs_allocated) {
    free_inputs();
    r.note = "bounded grouped-aggregate input allocation failed";
    return r;
  }

  auto* key0 = static_cast<int32_t*>(input_allocations[0]);
  auto* key1 = static_cast<int32_t*>(input_allocations[1]);
  auto* key2 = static_cast<int32_t*>(input_allocations[2]);
  for (size_t row = 0; row < capacity; ++row) {
    key0[row] = static_cast<int32_t>((row / kKeyCardinality) % kKeyCardinality);
    key1[row] = static_cast<int32_t>(row % kKeyCardinality);
    key2[row] = 0;
  }
  for (size_t dim = 0; dim < kDimCount; ++dim)
    std::fill_n(static_cast<int32_t*>(input_allocations[kKeyCount + dim]), capacity, int32_t{0});
  std::fill_n(static_cast<int32_t*>(input_allocations[kKeyCount + kDimCount]), capacity,
              int32_t{1});
  std::fill_n(static_cast<int32_t*>(input_allocations[kKeyCount + kDimCount + 1]), capacity,
              int32_t{2});

  pgaccel_grouped_agg_desc desc = {};
  desc.abi_version = PGACCEL_OLAP_ABI_VERSION;
  desc.size_bytes = sizeof(desc);
  desc.row_count = capacity;
  desc.grouping_mode = PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX;
  desc.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_DENSE;
  desc.key_count = kKeyCount;
  desc.group_capacity = kGroupCount;
  const uint32_t key_cardinalities[kKeyCount] = {kKeyCardinality, kKeyCardinality, 1};
  for (size_t key = 0; key < kKeyCount; ++key) {
    desc.keys[key].values.values = input_allocations[key];
    desc.keys[key].values.type = PGACCEL_VAL_INT32;
    desc.keys[key].source = PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT;
    desc.keys[key].code_min = 0;
    desc.keys[key].cardinality = key_cardinalities[key];
    desc.keys[key].null_code = PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE;
  }
  desc.dim_count = kDimCount;
  for (size_t dim = 0; dim < kDimCount; ++dim) {
    desc.dims[dim].fact_key.values = input_allocations[kKeyCount + dim];
    desc.dims[dim].fact_key.type = PGACCEL_VAL_INT32;
    desc.dims[dim].key_min = 0;
    desc.dims[dim].key_count = 1;
  }
  desc.measure_count = kMeasureCount;
  desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
  desc.measures[0].value.values = input_allocations[kKeyCount + kDimCount];
  desc.measures[0].value.physical_type = PGACCEL_GROUPED_AGG_PHYSICAL_INT32;
  desc.measures[0].value.element_bytes = sizeof(int32_t);
  desc.measures[0].rhs.values = input_allocations[kKeyCount + kDimCount + 1];
  desc.measures[0].rhs.physical_type = PGACCEL_GROUPED_AGG_PHYSICAL_INT32;
  desc.measures[0].rhs.element_bytes = sizeof(int32_t);
  desc.measures[0].op = PGACCEL_GROUPED_AGG_MEASURE_MUL;
  desc.measures[0].agg_mask = PGACCEL_GROUPED_AGG_LANE_SUM;
  desc.measures[0].accumulator_kind = PGACCEL_GROUPED_AGG_ACCUM_I64;
  desc.measures[0].state_bytes = sizeof(int64_t);
  desc.measures[1].op = PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR;
  desc.measures[1].agg_mask = PGACCEL_GROUPED_AGG_LANE_COUNT;
  desc.measures[1].accumulator_kind = PGACCEL_GROUPED_AGG_ACCUM_I64;
  desc.measures[1].state_bytes = sizeof(int64_t);
  desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_NONE;
  desc.where_filter.value_cmp_opcode = PGACCEL_EXPR_OP_ALWAYS_TRUE;
  for (auto& filter : desc.measure_filters) {
    filter.kind = PGACCEL_GROUPED_AGG_FILTER_NONE;
    filter.value_cmp_opcode = PGACCEL_EXPR_OP_ALWAYS_TRUE;
  }

  pgaccel_grouped_agg_workspace_req req = {};
  req.abi_version = PGACCEL_OLAP_ABI_VERSION;
  req.size_bytes = sizeof(req);
  status = pgaccel_grouped_agg_workspace_requirements(&desc, &req);
  void* workspace = nullptr;
  if (status == PGACCEL_OK) {
    status = pgaccel_grouped_agg_workspace_alloc(req.bytes, req.alignment,
                                                 PGACCEL_MEM_SPACE_SHARED_USM, &workspace);
  }
  if (status != PGACCEL_OK || (req.bytes != 0 && workspace == nullptr)) {
    free_inputs();
    pgaccel_grouped_agg_workspace_free(workspace);
    r.note = "grouped-aggregate workspace allocation failed";
    return r;
  }
  desc.scratch = workspace;
  desc.scratch_bytes = req.bytes;
  desc.scratch_space = PGACCEL_MEM_SPACE_SHARED_USM;
  desc.scratch_alignment = static_cast<uint32_t>(req.alignment);

  const size_t rss_before = current_rss_bytes();
  pgaccel_reset_gpu_exec_count();
  size_t processed = 0;
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  std::vector<uint8_t> active(kGroupCount);
  std::vector<int64_t> sums(kGroupCount);
  std::vector<uint64_t> nonnull(kGroupCount);
  std::vector<uint64_t> counts(kGroupCount);
  std::vector<int64_t> total_sums(kGroupCount);
  std::vector<uint64_t> total_nonnull(kGroupCount);
  std::vector<uint64_t> total_counts(kGroupCount);
  pgaccel_grouped_agg_out output = {};
  output.abi_version = PGACCEL_OLAP_ABI_VERSION;
  output.size_bytes = sizeof(output);
  output.group_capacity = kGroupCount;
  output.output_space = PGACCEL_MEM_SPACE_HOST;
  output.active_groups = active.data();
  output.measures[0].sum = sums.data();
  output.measures[0].nonnull_count = nonnull.data();
  output.measures[1].count = counts.data();
  bool chunk_values_ok = true;
  size_t lifecycle_calls = 0;
  while (processed < row_count && status == PGACCEL_OK && chunk_values_ok) {
    desc.row_count = std::min(capacity, row_count - processed);
    desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
    output.emitted_group_count = 0;
    output.selected_count = 0;
    output.uncertain_count = 0;
    status = pgaccel_grouped_agg_execute_ex(&desc, &output, &detail);
    ++lifecycle_calls;
    if (status != PGACCEL_OK)
      break;

    const uint64_t chunk_count = desc.row_count / kGroupCount;
    chunk_values_ok = desc.row_count % kGroupCount == 0 &&
                      output.emitted_group_count == kGroupCount &&
                      output.selected_count == desc.row_count && output.uncertain_count == 0;
    for (size_t group = 0; group < kGroupCount && chunk_values_ok; ++group) {
      chunk_values_ok = active[group] == 1 &&
                        sums[group] == static_cast<int64_t>(chunk_count * 2) &&
                        nonnull[group] == chunk_count && counts[group] == chunk_count;
      total_sums[group] += sums[group];
      total_nonnull[group] += nonnull[group];
      total_counts[group] += counts[group];
    }
    if (chunk_values_ok)
      processed += desc.row_count;
  }

  r.gpu_dispatches = pgaccel_gpu_exec_count();
  const size_t rss_after = peak_rss_bytes();
  r.status_ok = status == PGACCEL_OK && detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE &&
                processed == row_count && lifecycle_calls == expected_lifecycle_calls &&
                r.gpu_dispatches == lifecycle_calls;
  r.peak_rss_bytes = rss_after;
  r.rss_baseline_bytes = rss_before;
  bool values_ok = r.status_ok && chunk_values_ok;
  const uint64_t expected_count = row_count / kGroupCount;
  for (size_t group = 0; group < kGroupCount && values_ok; ++group) {
    values_ok = total_sums[group] == static_cast<int64_t>(expected_count * 2) &&
                total_nonnull[group] == expected_count && total_counts[group] == expected_count;
  }
  r.correct = values_ok;
  pgaccel_grouped_agg_workspace_free(workspace);
  free_inputs();

  const size_t rss_delta = rss_after > rss_before ? rss_after - rss_before : 0;
  r.rss_delta_bytes = rss_delta;
  r.under_ceiling = rss_delta <= rss_ceiling;
  printf("   status=%d detail=%d status_ok=%d correct=%d processed_rows=%zu lifecycle_calls=%zu "
         "logical_bytes=%zu rss_before=%.2fGB peak=%.2fGB delta=%.2fGB "
         "ceiling=%.2fGB dispatches=%llu under=%d\n",
         static_cast<int>(status), detail, r.status_ok, r.correct, processed, lifecycle_calls,
         processed * kBytesPerRow, rss_before / 1e9, rss_after / 1e9, rss_delta / 1e9,
         rss_ceiling / 1e9, static_cast<unsigned long long>(r.gpu_dispatches), r.under_ceiling);
  if (!r.status_ok)
    r.note = "grouped-aggregate bounded one-shot lifecycle did not process the logical input";
  else if (!r.correct)
    r.note = "grouped-aggregate bounded one-shot lifecycle produced incorrect MUL/SUM state";
  return r;
}

static FamilyResult run_spatial_family(size_t N, size_t rss_ceiling) {
  FamilyResult r = {"spatial_f64", false, false, 0, rss_ceiling, false, ""};
  printf("\n-- spatial_f64 (PIP) @ N=%zu points (%.2f GB raw input) --\n", N,
         static_cast<double>(N * 2 * sizeof(double)) / (1024.0 * 1024.0 * 1024.0));

  // Unit square ring.
  static const double ring[] = {0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 0.0, 0.0};
  std::vector<double> pts;
  std::vector<int8_t> results;
  try {
    pts.resize(N * 2);
    results.assign(N, 99);
  } catch (const std::bad_alloc&) {
    r.note = "CPU-side alloc failed";
    return r;
  }
  // Every point at (0.5, 0.5) — definitely inside.
  for (size_t i = 0; i < N; ++i) {
    pts[2 * i] = 0.5;
    pts[2 * i + 1] = 0.5;
  }
  const size_t rss_before = current_rss_bytes();
  pgaccel_reset_gpu_exec_count();
  pgaccel_status st =
      pgaccel_point_in_ring_bulk(pts.data(), N, ring, 5, /*use_fp64=*/true, results.data());
  r.gpu_dispatches = pgaccel_gpu_exec_count();
  const size_t rss_after = peak_rss_bytes();
  r.status_ok = (st == PGACCEL_OK && r.gpu_dispatches > 0);
  r.peak_rss_bytes = rss_after;
  r.rss_baseline_bytes = rss_before;
  // Correctness: every result should be 1 (inside).
  bool all_inside = r.status_ok;
  if (r.status_ok) {
    for (size_t i = 0; i < N; ++i) {
      if (results[i] != 1) {
        all_inside = false;
        break;
      }
    }
  }
  r.correct = all_inside;
  const size_t rss_delta = rss_after > rss_before ? rss_after - rss_before : 0;
  r.rss_delta_bytes = rss_delta;
  r.under_ceiling = rss_delta <= rss_ceiling;
  printf("   status=%d (OK=%d) all_inside=%d  rss_before=%.2fGB peak=%.2fGB delta=%.2fGB "
         "ceiling=%.2fGB dispatches=%llu under=%d\n",
         (int)st, r.status_ok, all_inside, rss_before / 1e9, rss_after / 1e9, rss_delta / 1e9,
         rss_ceiling / 1e9, static_cast<unsigned long long>(r.gpu_dispatches), r.under_ceiling);
  if (!r.status_ok)
    r.note = "spatial_f64 PIP returned non-OK on device-exceeding input";
  else if (!r.correct)
    r.note = "spatial_f64 PIP misclassified points";
  return r;
}

static FamilyResult run_h3_family(size_t N, size_t rss_ceiling) {
  FamilyResult r = {"h3_f64", false, false, 0, rss_ceiling, false, ""};
  printf("\n-- h3_f64 @ N=%zu (%.2f GB raw lat+lng input) --\n", N,
         static_cast<double>(N * 2 * sizeof(double)) / (1024.0 * 1024.0 * 1024.0));
  std::vector<double> lats, lngs;
  std::vector<uint64_t> cells;
  std::vector<uint8_t> valids;
  try {
    lats.assign(N, 37.7749);
    lngs.assign(N, -122.4194);
    cells.resize(N);
    valids.resize(N);
  } catch (const std::bad_alloc&) {
    r.note = "CPU-side alloc failed";
    return r;
  }
  const size_t rss_before = current_rss_bytes();
  pgaccel_reset_gpu_exec_count();
  pgaccel_status st = pgaccel_h3_lat_lng_to_cell_bulk(lats.data(), lngs.data(), N, 7,
                                                      /*use_fp64=*/1, cells.data(), valids.data());
  r.gpu_dispatches = pgaccel_gpu_exec_count();
  const size_t rss_after = peak_rss_bytes();
  r.status_ok = (st == PGACCEL_OK && r.gpu_dispatches > 0);
  r.peak_rss_bytes = rss_after;
  r.rss_baseline_bytes = rss_before;
  bool all_valid = r.status_ok;
  uint64_t first = 0;
  if (r.status_ok) {
    first = cells[0];
    for (size_t i = 0; i < N; ++i) {
      if (!valids[i] || cells[i] != first) {
        all_valid = false;
        break;
      }
    }
  }
  r.correct = all_valid;
  const size_t rss_delta = rss_after > rss_before ? rss_after - rss_before : 0;
  r.rss_delta_bytes = rss_delta;
  r.under_ceiling = rss_delta <= rss_ceiling;
  printf("   status=%d (OK=%d) all_same_valid=%d  rss_before=%.2fGB peak=%.2fGB delta=%.2fGB "
         "ceiling=%.2fGB dispatches=%llu under=%d\n",
         (int)st, r.status_ok, all_valid, rss_before / 1e9, rss_after / 1e9, rss_delta / 1e9,
         rss_ceiling / 1e9, static_cast<unsigned long long>(r.gpu_dispatches), r.under_ceiling);
  if (!r.status_ok)
    r.note = "h3_f64 returned non-OK on device-exceeding input";
  else if (!r.correct)
    r.note = "h3_f64 produced inconsistent cells for same lat/lng";
  return r;
}

int main() {
  if (pgaccel_init() != PGACCEL_OK) {
    fprintf(stderr, "pgaccel_init failed\n");
    return 1;
  }

  pgaccel_platform_caps caps = pgaccel_get_caps();
  pgaccel_device_info info = pgaccel_get_device_info();
  printf("Device: %s backend=%s has_native_fp64=%d max_alloc_bytes=%zu\n", info.device_name,
         info.backend_name, info.has_native_fp64, caps.max_alloc_bytes);
  std::string backend = info.backend_name;
  std::transform(backend.begin(), backend.end(), backend.begin(),
                 [](unsigned char value) { return static_cast<char>(std::tolower(value)); });
  const bool accelerator_backend =
      backend == "metal" || backend == "cuda" || backend == "hip" || backend == "level_zero";
  if (info.device_name[0] == '\0' || info.backend_name[0] == '\0' || info.compute_units == 0 ||
      caps.max_alloc_bytes == 0 || !accelerator_backend) {
    fprintf(stderr,
            "FAIL: OOM invariant requires a real accelerator device with nonzero capacity\n");
    pgaccel_shutdown();
    return 2;
  }
  printf("PGACCEL_DEVICE_PROOF device=\"%s\" backend=\"%s\" compute_units=%u "
         "max_alloc_bytes=%zu real_device=1\n",
         info.device_name, info.backend_name, info.compute_units, caps.max_alloc_bytes);

  // Input size: 2 × max_alloc_bytes / sizeof(double).
  const size_t max_alloc = caps.max_alloc_bytes;
  const size_t N = (2 * max_alloc) / sizeof(double);
  // Cap N at 256 Mi (2 GiB of doubles) to keep the test feasible on
  // constrained hosts — still exceeds any typical max_alloc on M-series.
  const size_t N_capped = std::min<size_t>(N, size_t{256} * 1024 * 1024);
  // RSS ceiling: 3 × max_alloc. If max_alloc is small, add slack so
  // that the harness's baseline RSS isn't what flags the test.
  const size_t rss_ceiling = 3 * max_alloc;

  printf("\nPlan:\n");
  printf("  N (doubles) = 2 * max_alloc / sizeof(double) = %zu\n", N);
  printf("  N_capped    = %zu (%.2f GB per-family input)\n", N_capped,
         static_cast<double>(N_capped * sizeof(double)) / 1e9);
  printf("  RSS ceiling = 3 * max_alloc = %.2f GB\n", rss_ceiling / 1e9);

  std::vector<FamilyResult> results;
  results.push_back(run_reduce_family(N_capped, rss_ceiling));
  results.push_back(run_expr_vm_family(N_capped, rss_ceiling));
  results.push_back(run_grouped_agg_family(N_capped, rss_ceiling));
  // Spatial PIP: 2 doubles per point — use N_capped/2 points.
  results.push_back(run_spatial_family(N_capped / 2, rss_ceiling));
  // h3: same shape as spatial.
  results.push_back(run_h3_family(N_capped / 2, rss_ceiling));

  pgaccel_shutdown();

  printf("\n=== OOM-never invariant summary ===\n");
  int fails = 0;
  for (const auto& r : results) {
    const bool pass = r.status_ok && r.correct && r.under_ceiling && r.gpu_dispatches > 0;
    printf("  %-14s %s  peak_rss=%.2fGB ceiling=%.2fGB status_ok=%d correct=%d under_ceiling=%d "
           "note=\"%s\"\n",
           r.name, pass ? "PASS" : "FAIL", r.peak_rss_bytes / 1e9, r.rss_ceiling_bytes / 1e9,
           r.status_ok, r.correct, r.under_ceiling, r.note.c_str());
    printf("PGACCEL_OOM_FAMILY family=%s result=%s dispatches=%llu "
           "peak_rss_bytes=%zu rss_baseline_bytes=%zu rss_delta_bytes=%zu "
           "rss_limit_bytes=%zu\n",
           r.name, pass ? "PASS" : "FAIL", static_cast<unsigned long long>(r.gpu_dispatches),
           r.peak_rss_bytes, r.rss_baseline_bytes, r.rss_delta_bytes, r.rss_ceiling_bytes);
    if (!pass)
      fails++;
  }
  if (fails) {
    fprintf(stderr,
            "\nFAIL: %d kernel family/families violate OOM-never invariant. "
            "This is a streaming/chunking regression — do NOT relax the "
            "RSS ceiling to make it pass.\n",
            fails);
    return 1;
  }
  printf("PGACCEL_OOM_INVARIANT result=PASS families=%zu max_alloc_bytes=%zu "
         "input_doubles=%zu rss_limit_bytes=%zu\n",
         results.size(), max_alloc, N_capped, rss_ceiling);
  printf("\nPASS — all live kernel families honor OOM-never invariant.\n");
  return 0;
}
