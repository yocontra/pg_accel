#include <algorithm>
#include <array>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_join_rows.h"

namespace {
int passed = 0;
int failed = 0;
void check(bool condition, const char* message) {
  if (condition) ++passed;
  else { ++failed; std::fprintf(stderr, "FAIL: %s\n", message); }
}

struct Buffer {
  void* pointer = nullptr;
  Buffer() = default;
  Buffer(const Buffer&) = delete;
  Buffer& operator=(const Buffer&) = delete;
  ~Buffer() { pgaccel_expr_device_free(pointer); }
  bool allocate(size_t bytes) {
    return pgaccel_expr_device_alloc(bytes, &pointer) == PGACCEL_OK;
  }
  template <typename T> bool upload(const std::vector<T>& values) {
    return pgaccel_expr_device_alloc_copy(values.data(), values.size() * sizeof(T), &pointer) == PGACCEL_OK;
  }
  template <typename T> std::vector<T> download(size_t count) const {
    std::vector<T> values(count);
    if (count != 0)
      check(pgaccel_expr_device_copy_to_host(values.data(), pointer, count * sizeof(T)) == PGACCEL_OK,
            "device output copy");
    return values;
  }
};

struct HostKey {
  uint32_t type = PGACCEL_JOIN_ROWS_INT32;
  std::vector<int64_t> values;
  std::vector<uint8_t> nulls;
};

struct Fixture {
  pgaccel_join_rows_request request{};
  std::array<Buffer, 4> outer_values, outer_nulls, inner_values, inner_nulls;
  std::vector<HostKey> outer, inner;

  Fixture(std::vector<HostKey> left, std::vector<HostKey> right)
      : outer(std::move(left)), inner(std::move(right)) {
    request.version = PGACCEL_JOIN_ROWS_VERSION;
    request.key_count = static_cast<uint32_t>(outer.size());
    request.outer_rows = outer[0].values.size();
    request.inner_rows = inner[0].values.size();
    for (size_t i = 0; i < outer.size(); ++i) {
      request.outer_keys[i] = upload(outer[i], outer_values[i], outer_nulls[i]);
      request.inner_keys[i] = upload(inner[i], inner_values[i], inner_nulls[i]);
    }
  }

  static pgaccel_join_rows_key upload(const HostKey& key, Buffer& values, Buffer& nulls) {
    if (key.type == PGACCEL_JOIN_ROWS_INT32) {
      const std::vector<int32_t> narrow(key.values.begin(), key.values.end());
      check(values.upload(narrow), "INT32 keys uploaded");
    } else {
      check(values.upload(key.values), "INT64 keys uploaded");
    }
    check(nulls.upload(key.nulls), "null sidecar uploaded");
    return {values.pointer, static_cast<const uint8_t*>(nulls.pointer),
            key.values.size() * (key.type == PGACCEL_JOIN_ROWS_INT32 ? 4u : 8u),
            key.nulls.size(), key.type, 0};
  }

  bool equal(size_t left, size_t right) const {
    for (size_t k = 0; k < outer.size(); ++k) {
      if ((!outer[k].nulls.empty() && outer[k].nulls[left] != 0) ||
          (!inner[k].nulls.empty() && inner[k].nulls[right] != 0) ||
          outer[k].values[left] != inner[k].values[right])
        return false;
    }
    return true;
  }

  std::vector<pgaccel_join_row_pair> oracle(uint32_t kind) const {
    std::vector<pgaccel_join_row_pair> output;
    for (size_t left = 0; left < request.outer_rows; ++left) {
      bool matched = false;
      for (size_t right = 0; right < request.inner_rows; ++right) {
        if (!equal(left, right)) continue;
        matched = true;
        if (kind == PGACCEL_JOIN_ROWS_INNER || kind == PGACCEL_JOIN_ROWS_LEFT)
          output.push_back({left, right});
      }
      if ((!matched && (kind == PGACCEL_JOIN_ROWS_LEFT || kind == PGACCEL_JOIN_ROWS_ANTI)) ||
          (matched && kind == PGACCEL_JOIN_ROWS_SEMI))
        output.push_back({left, PGACCEL_JOIN_ROWS_NULL_ROW});
    }
    return output;
  }
};

bool pair_less(const pgaccel_join_row_pair& a, const pgaccel_join_row_pair& b) {
  return a.outer_row < b.outer_row || (a.outer_row == b.outer_row && a.inner_row < b.inner_row);
}

struct CancelState {
  uint64_t checks = 0;
  uint64_t cancel_at = UINT64_MAX;
};

void compare(std::vector<pgaccel_join_row_pair> actual, std::vector<pgaccel_join_row_pair> expected);

int cancel_check(void* context) {
  auto& state = *static_cast<CancelState*>(context);
  return ++state.checks == state.cancel_at ? 1 : 0;
}

void run_controlled(Fixture& fixture, uint32_t kind, uint64_t items, uint64_t steps,
                    bool every_cancel_boundary) {
  fixture.request.kind = kind;
  const auto expected = fixture.oracle(kind);
  CancelState cancellation;
  pgaccel_join_control control{PGACCEL_JOIN_CONTROL_VERSION, 0, items, steps,
                                cancel_check, &cancellation};
  uint64_t bytes = 0;
  const auto before_query = pgaccel_gpu_exec_count();
  check(pgaccel_join_rows_controlled_workspace_bytes(&fixture.request, &control, &bytes) == PGACCEL_OK,
        "bounded layout query");
  check(cancellation.checks == 0 && pgaccel_gpu_exec_count() == before_query,
        "bounded query never invokes callbacks or device work");
  Buffer workspace, output;
  check(workspace.allocate(bytes), "bounded cursor workspace");
  const pgaccel_join_row_pair poison{UINT64_MAX - 1, UINT64_MAX - 2};
  check(output.upload(std::vector<pgaccel_join_row_pair>(expected.size() + 1, poison)), "bounded output poison");
  auto* pairs = static_cast<pgaccel_join_row_pair*>(output.pointer);
  pgaccel_join_controlled_result result{};
  const auto before = pgaccel_gpu_exec_count();
  check(pgaccel_join_rows_execute_controlled(&fixture.request, &control, workspace.pointer, bytes,
                                            pairs, expected.size() + 1, &result) == PGACCEL_OK,
        "bounded join success");
  check(result.cancelled == 0 && result.reserved == 0 && result.rows.emitted_rows == expected.size() &&
        result.rows.required_rows == expected.size(), "bounded exact successful cardinality");
  check(result.cancel_checks == cancellation.checks && result.cancel_checks == result.rows.kernel_launches + 1 &&
        result.rows.kernel_launches == pgaccel_gpu_exec_count() - before,
        "cancellation boundary before work and after every completed kernel");
  const auto checks = result.cancel_checks;
  auto actual = output.download<pgaccel_join_row_pair>(expected.size() + 1);
  check(std::memcmp(&actual.back(), &poison, sizeof(poison)) == 0, "bounded output tail untouched");
  actual.pop_back();
  compare(actual, expected);
  cancellation = {};
  pgaccel_join_controlled_result counted{};
  const auto before_count = pgaccel_gpu_exec_count();
  check(pgaccel_join_rows_count_controlled(&fixture.request, &control, workspace.pointer, bytes, &counted) == PGACCEL_OK,
        "count-only returns success without output storage or capacity exhaustion");
  check(counted.rows.required_rows == expected.size() && counted.rows.emitted_rows == 0 &&
        counted.cancelled == 0 && counted.reserved == 0 &&
        counted.cancel_checks == counted.rows.kernel_launches + 1 &&
        counted.rows.kernel_launches == pgaccel_gpu_exec_count() - before_count,
        "count-only exact device multiplicity and dispatch accounting");
  if (!expected.empty() && (kind == PGACCEL_JOIN_ROWS_INNER || kind == PGACCEL_JOIN_ROWS_LEFT))
    check(counted.rows.kernel_launches < result.rows.kernel_launches,
          "count-only skips offset propagation and emission");
  const auto count_checks = counted.cancel_checks;
  if (every_cancel_boundary) {
    for (uint64_t stop = 1; stop <= count_checks; ++stop) {
      cancellation = {0, stop};
      counted = {};
      check(pgaccel_join_rows_count_controlled(&fixture.request, &control, workspace.pointer, bytes, &counted) == PGACCEL_TIMEOUT &&
            counted.cancelled == 1 && counted.rows.emitted_rows == 0 && counted.rows.required_rows == 0 &&
            counted.cancel_checks == stop && counted.rows.kernel_launches == stop - 1,
            "count-only cancellation publishes no partial cardinality");
      cancellation = {};
      check(pgaccel_join_rows_count_controlled(&fixture.request, &control, workspace.pointer, bytes, &counted) == PGACCEL_OK &&
            counted.rows.required_rows == expected.size() && counted.rows.emitted_rows == 0,
            "count-only retries reset all progress after cancellation");
    }
  }
  if (!expected.empty()) {
    cancellation = {};
    result = {};
    check(pgaccel_join_rows_execute_controlled(&fixture.request, &control, workspace.pointer, bytes,
              expected.size() == 1 ? nullptr : pairs, expected.size() - 1, &result) == PGACCEL_OOM,
          "bounded capacity reports need without truncation");
    check(result.rows.required_rows == expected.size() && result.rows.emitted_rows == 0 && result.cancelled == 0,
          "bounded capacity metadata");
    auto unchanged = output.download<pgaccel_join_row_pair>(expected.size() + 1);
    unchanged.pop_back();
    check(std::memcmp(unchanged.data(), actual.data(), actual.size() * sizeof(actual[0])) == 0,
          "insufficient bounded capacity writes no output bytes");
    compare(unchanged, expected);
  }
  if (every_cancel_boundary) {
    for (uint64_t stop = 1; stop <= checks; ++stop) {
      cancellation = {0, stop};
      result = {};
      const auto before_cancel = pgaccel_gpu_exec_count();
      check(pgaccel_join_rows_execute_controlled(&fixture.request, &control, workspace.pointer, bytes,
                                                pairs, expected.size() + 1, &result) == PGACCEL_TIMEOUT,
            "callback cancellation returns a distinct incomplete result");
      check(result.cancelled == 1 && result.rows.emitted_rows == 0 && result.cancel_checks == stop &&
            cancellation.checks == stop && result.rows.kernel_launches == stop - 1 &&
            pgaccel_gpu_exec_count() - before_cancel == stop - 1,
            "no launches after cancellation and no partial publication");
      cancellation = {};
      check(pgaccel_join_rows_execute_controlled(&fixture.request, &control, workspace.pointer, bytes,
                                                pairs, expected.size() + 1, &result) == PGACCEL_OK,
            "fresh call resets cancelled count/emit cursors");
      compare(output.download<pgaccel_join_row_pair>(expected.size()), expected);
    }
  }
  control.cancel_check = nullptr;
  control.context = nullptr;
  check(pgaccel_join_rows_count_controlled(&fixture.request, &control, workspace.pointer, bytes, &counted) == PGACCEL_OK &&
        counted.rows.required_rows == expected.size() && counted.cancel_checks == 0 && counted.cancelled == 0,
        "count-only permits no callback without unbounding device work");
  check(pgaccel_join_rows_execute_controlled(&fixture.request, &control, workspace.pointer, bytes,
                                            pairs, expected.size() + 1, &result) == PGACCEL_OK &&
        result.cancel_checks == 0 && result.cancelled == 0, "optional callback retains bounded work");

  for (int mutation = 0; mutation < 14; ++mutation) {
    auto invalid = control;
    void* scratch = workspace.pointer;
    uint64_t span_bytes = bytes;
    auto* destination = &result;
    switch (mutation) {
      case 0: invalid.version++; break;
      case 1: invalid.reserved = 1; break;
      case 2: invalid.work_items = 0; break;
      case 3: invalid.chain_steps = 0; break;
      case 4: invalid.work_items = UINT64_MAX; break;
      case 5: invalid.chain_steps = UINT64_MAX; break;
      case 6: invalid.context = &cancellation; break;
      case 7: span_bytes--; break;
      case 8: scratch = &invalid; break;
      case 9: destination = reinterpret_cast<pgaccel_join_controlled_result*>(&invalid); break;
      case 10: destination = reinterpret_cast<pgaccel_join_controlled_result*>(workspace.pointer); break;
      case 11: scratch = static_cast<uint8_t*>(workspace.pointer) + 1; break;
      case 12: destination = nullptr; break;
      case 13: scratch = nullptr; break;
    }
    const auto before_invalid = pgaccel_gpu_exec_count();
    const auto count_status = pgaccel_join_rows_count_controlled(&fixture.request, &invalid, scratch, span_bytes, destination);
    check((count_status == PGACCEL_INVALID_ARGUMENT || count_status == PGACCEL_UNSUPPORTED) &&
          pgaccel_gpu_exec_count() == before_invalid,
          "count-only rejects invalid control, spans and aliases before dispatch");
    const auto status = pgaccel_join_rows_execute_controlled(&fixture.request, &invalid, scratch,
                                                           span_bytes, pairs, expected.size() + 1, destination);
    check(status == (mutation == 0 ? PGACCEL_UNSUPPORTED : PGACCEL_INVALID_ARGUMENT),
          "bounded malformed contract rejects");
    check(pgaccel_gpu_exec_count() == before_invalid, "bounded malformed contract cannot dispatch");
  }
  check(pgaccel_join_rows_controlled_workspace_bytes(&fixture.request, &control,
          reinterpret_cast<uint64_t*>(&control)) == PGACCEL_INVALID_ARGUMENT,
        "bounded query output cannot overwrite control");
  check(pgaccel_join_rows_controlled_workspace_bytes(nullptr, &control, &bytes) == PGACCEL_INVALID_ARGUMENT,
        "bounded query rejects null request");
  check(pgaccel_join_rows_controlled_workspace_bytes(&fixture.request, nullptr, &bytes) == PGACCEL_INVALID_ARGUMENT,
        "bounded query rejects null control");
}

void compare(std::vector<pgaccel_join_row_pair> actual, std::vector<pgaccel_join_row_pair> expected) {
  check(std::is_sorted(actual.begin(), actual.end(), [](auto a, auto b) { return a.outer_row < b.outer_row; }),
        "outer row order is deterministic");
  std::sort(actual.begin(), actual.end(), pair_less);
  std::sort(expected.begin(), expected.end(), pair_less);
  check(actual.size() == expected.size(), "oracle row count");
  check(actual.size() == expected.size() &&
        (actual.empty() || std::memcmp(actual.data(), expected.data(), actual.size() * sizeof(actual[0])) == 0),
        "exact pair multiset matches independent SQL-equality oracle");
}

void project(Fixture& fixture, const pgaccel_join_row_pair* pairs,
             const std::vector<pgaccel_join_row_pair>& mappings) {
  const size_t rows = mappings.size();
  pgaccel_join_project_request request{};
  request.version = PGACCEL_JOIN_PROJECT_VERSION;
  request.column_count = 3;
  request.outer_rows = fixture.request.outer_rows;
  request.inner_rows = fixture.request.inner_rows;
  request.row_count = rows;
  request.pairs = rows == 0 ? nullptr : pairs;
  request.pair_bytes = rows * sizeof(*pairs);
  std::array<Buffer, 3> values, nulls;
  Buffer error, bytes;
  check(error.allocate(PGACCEL_JOIN_PROJECT_WORKSPACE_BYTES), "projection workspace");
  const std::vector<uint8_t> byte_values(fixture.request.outer_rows, 0xa5);
  check(bytes.upload(byte_values), "bit-preserving byte payload");
  for (size_t index = 0; index < request.column_count; ++index) {
    const auto key = index == 1 ? fixture.request.inner_keys[0] : fixture.request.outer_keys[0];
    const uint32_t width = index == 2 ? 1 : (key.type == PGACCEL_JOIN_ROWS_INT32 ? 4 : 8);
    check(values[index].upload(std::vector<uint8_t>(rows * width, 0xcd)),
          "projection output poison");
    check(nulls[index].upload(std::vector<uint8_t>(rows, 0xcd)), "projection null poison");
    request.columns[index] = {index == 1 ? PGACCEL_JOIN_PROJECT_INNER : PGACCEL_JOIN_PROJECT_OUTER,
                              width,
                              index == 2 ? bytes.pointer : key.values,
                              key.nulls,
                              index == 2 ? byte_values.size() : key.value_bytes,
                              key.null_bytes,
                              values[index].pointer,
                              static_cast<uint8_t*>(nulls[index].pointer),
                              rows * width,
                              rows};
  }
  for (bool controlled : {false, true}) {
    if (rows != 0) {
      for (size_t index = 0; index < request.column_count; ++index) {
        const std::vector<uint8_t> poison(rows * request.columns[index].width, 0xcd);
        check(pgaccel_expr_device_copy_from_host(values[index].pointer, poison.data(),
                                                 poison.size()) == PGACCEL_OK,
              "reset projection value poison");
        check(pgaccel_expr_device_copy_from_host(nulls[index].pointer, poison.data(), rows) ==
                  PGACCEL_OK,
              "reset projection null poison");
      }
    }
    pgaccel_join_project_result result{};
    const auto before = pgaccel_gpu_exec_count();
    if (controlled) {
      CancelState cancellation;
      const uint64_t items = rows > 1000 ? 4096 : 3;
      pgaccel_join_control control{
          PGACCEL_JOIN_CONTROL_VERSION, 0, items, 1, cancel_check, &cancellation};
      pgaccel_join_controlled_result progress{};
      check(pgaccel_join_rows_project_controlled(&request, &control, error.pointer,
                                                 PGACCEL_JOIN_PROJECT_WORKSPACE_BYTES,
                                                 &progress) == PGACCEL_OK,
            "controlled projection succeeds");
      check(progress.cancelled == 0 && progress.reserved == 0 &&
                progress.rows.required_rows == rows &&
                progress.cancel_checks == cancellation.checks &&
                progress.cancel_checks == progress.rows.kernel_launches + 1 &&
                progress.rows.kernel_launches ==
                    (rows == 0 ? 0 : 1 + 2 * ((rows + items - 1) / items)),
            "bounded gather launch counts and callback boundaries");
      result = {progress.rows.emitted_rows, progress.rows.kernel_launches};
    } else {
      check(pgaccel_join_rows_project(&request, error.pointer, PGACCEL_JOIN_PROJECT_WORKSPACE_BYTES,
                                      &result) == PGACCEL_OK,
            "resident row mappings project to device columns");
    }
    check(result.emitted_rows == rows &&
              result.kernel_launches == pgaccel_gpu_exec_count() - before,
          "projection cardinality and launch accounting");
    for (size_t index = 0; index < request.column_count; ++index) {
      const auto& key = index == 1 ? fixture.inner[0] : fixture.outer[0];
      const auto actual_nulls = nulls[index].download<uint8_t>(rows);
      const auto actual_bytes =
          values[index].download<uint8_t>(rows * request.columns[index].width);
      bool correct = true;
      for (size_t row = 0; row < rows; ++row) {
        const auto source = index == 1 ? mappings[row].inner_row : mappings[row].outer_row;
        const bool is_null =
            source == PGACCEL_JOIN_ROWS_NULL_ROW || (!key.nulls.empty() && key.nulls[source] != 0);
        const uint64_t expected =
            is_null ? 0 : (index == 2 ? 0xa5 : static_cast<uint64_t>(key.values[source]));
        uint64_t actual = 0;
        const uint32_t width = request.columns[index].width;
        std::memcpy(&actual, &actual_bytes[row * width], width);
        const uint64_t mask = width == 8 ? UINT64_MAX : ((UINT64_C(1) << (width * 8)) - 1);
        correct &=
            actual_nulls[row] == static_cast<uint8_t>(is_null) && actual == (expected & mask);
      }
      check(correct, "resident projection preserves bits and canonical SQL NULLs");
    }
  }
}

void run(Fixture& fixture) {
  for (uint32_t kind = PGACCEL_JOIN_ROWS_INNER; kind <= PGACCEL_JOIN_ROWS_ANTI; ++kind) {
    fixture.request.kind = kind;
    const auto expected = fixture.oracle(kind);
    uint64_t workspace_bytes = 0;
    const auto before_query = pgaccel_gpu_exec_count();
    check(pgaccel_join_rows_workspace_bytes(&fixture.request, &workspace_bytes) == PGACCEL_OK,
          "valid pure layout query");
    if (fixture.request.outer_rows != 0)
      check(workspace_bytes >= sizeof(pgaccel_join_rows_request) + 64,
            "nonempty workspace includes charged device metadata");
    check(pgaccel_gpu_exec_count() == before_query, "layout query does not dispatch");
    Buffer workspace, output;
    check(workspace.allocate(workspace_bytes), "workspace allocated");
    const pgaccel_join_row_pair poison{UINT64_MAX - 1, UINT64_MAX - 2};
    const size_t capacity = expected.size() + 3;
    check(output.upload(std::vector<pgaccel_join_row_pair>(capacity, poison)), "output poison uploaded");
    if (!workspace.pointer || !output.pointer) return;
    auto* pairs = static_cast<pgaccel_join_row_pair*>(output.pointer);
    if (!expected.empty()) {
      pgaccel_join_rows_result insufficient{};
      check(pgaccel_join_rows_execute(&fixture.request, workspace.pointer, workspace_bytes,
                                      expected.size() == 1 ? nullptr : pairs, expected.size() - 1, &insufficient) == PGACCEL_OOM,
            "capacity exhaustion fails explicitly");
      check(insufficient.required_rows == expected.size() && insufficient.emitted_rows == 0,
            "capacity failure returns exact required cardinality");
      const auto unchanged = output.download<pgaccel_join_row_pair>(capacity);
      check(std::all_of(unchanged.begin(), unchanged.end(), [&](auto p) {
        return p.outer_row == poison.outer_row && p.inner_row == poison.inner_row;
      }), "capacity exhaustion never writes partial output");
    }
    pgaccel_join_rows_result result{};
    const auto before = pgaccel_gpu_exec_count();
    check(pgaccel_join_rows_execute(&fixture.request, workspace.pointer, workspace_bytes,
                                    pairs, capacity, &result) == PGACCEL_OK, "resident join executes");
    check(result.required_rows == expected.size() && result.emitted_rows == expected.size(),
          "successful count publication");
    check(pgaccel_gpu_exec_count() - before == result.kernel_launches, "exact dispatch accounting");
    check(fixture.request.outer_rows == 0 || result.kernel_launches > 0, "nonempty probe uses GPU");
    auto actual = output.download<pgaccel_join_row_pair>(capacity);
    check(std::all_of(actual.begin() + expected.size(), actual.end(), [&](auto p) {
      return p.outer_row == poison.outer_row && p.inner_row == poison.inner_row;
    }), "output tail is untouched");
    actual.resize(expected.size());
    project(fixture, pairs, actual);
    compare(actual, expected);
    // Reusing workspace resets every bucket/count/offset, not just the result.
    result = {};
    check(pgaccel_join_rows_execute(&fixture.request, workspace.pointer, workspace_bytes,
                                    pairs, capacity, &result) == PGACCEL_OK && result.emitted_rows == expected.size(),
          "repeat execution resets workspace");
    compare(output.download<pgaccel_join_row_pair>(expected.size()), expected);
  }
}

void invalid_projection_contracts() {
  Buffer pairs, source, destination, nulls, error;
  check(pairs.upload(std::vector<pgaccel_join_row_pair>{{0, 0}, {1, 1}}), "projection contract pairs");
  check(source.upload(std::vector<int32_t>{7, 11}), "projection contract input");
  check(destination.upload(std::vector<int32_t>{-47, -47}), "projection contract poison");
  check(nulls.upload(std::vector<uint8_t>{0xcd, 0xcd}), "projection contract null poison");
  check(error.allocate(PGACCEL_JOIN_PROJECT_WORKSPACE_BYTES), "projection contract workspace");
  pgaccel_join_project_request request{};
  request.version = PGACCEL_JOIN_PROJECT_VERSION;
  request.column_count = 1;
  request.outer_rows = 2;
  request.inner_rows = 2;
  request.row_count = 2;
  request.pairs = static_cast<const pgaccel_join_row_pair*>(pairs.pointer);
  request.pair_bytes = 32;
  request.columns[0] = {0, 4, source.pointer, nullptr, 8, 0, destination.pointer,
                        static_cast<uint8_t*>(nulls.pointer), 8, 2};
  int32_t host_values[] = {7, 11};
  for (size_t mutation = 0; mutation < 19; ++mutation) {
    auto invalid = request;
    auto* status_word = static_cast<uint32_t*>(error.pointer);
    uint64_t workspace_bytes = PGACCEL_JOIN_PROJECT_WORKSPACE_BYTES;
    switch (mutation) {
      case 0: invalid.version++; break;
      case 1: invalid.column_count = 0; break;
      case 2: invalid.column_count = 17; break;
      case 3: invalid.reserved = 1; break;
      case 4: invalid.pair_bytes = 16; break;
      case 5: invalid.columns[0].width = 2; break;
      case 6: invalid.columns[0].side = 2; break;
      case 7: invalid.columns[0].output_value_bytes = 4; break;
      case 8: invalid.columns[0].output_null_bytes = 1; break;
      case 9: invalid.columns[0].value_bytes = 4; break;
      case 10: invalid.columns[1] = invalid.columns[0]; break;
      case 11: invalid.columns[0].output_values = source.pointer; break;
      case 12: status_word = static_cast<uint32_t*>(destination.pointer); break;
      case 13: invalid.columns[0].output_nulls = static_cast<uint8_t*>(destination.pointer); break;
      case 14: invalid.row_count = UINT64_MAX; break;
      case 15: workspace_bytes--; break;
      case 16: status_word = reinterpret_cast<uint32_t*>(static_cast<uint8_t*>(error.pointer) + 4); break;
      case 17: status_word = nullptr; break;
      case 18: invalid.columns[0].values = host_values; break;
    }
    const pgaccel_join_project_result poison{53, 59};
    auto result = poison;
    const auto before = pgaccel_gpu_exec_count();
    const auto status = pgaccel_join_rows_project(&invalid, status_word, workspace_bytes, &result);
    check(status == PGACCEL_INVALID_ARGUMENT || status == PGACCEL_UNSUPPORTED, "invalid projection contract rejected");
    check(std::memcmp(&result, &poison, sizeof(result)) == 0, "invalid projection leaves metadata unchanged");
    check(pgaccel_gpu_exec_count() == before, "invalid projection contract never dispatches");
    pgaccel_join_control control{PGACCEL_JOIN_CONTROL_VERSION, 0, 1, 1, nullptr, nullptr};
    const pgaccel_join_controlled_result controlled_poison{{47, 53, 59}, 61, 67, 71};
    auto progress = controlled_poison;
    const auto controlled_status = pgaccel_join_rows_project_controlled(&invalid, &control, status_word,
                                                                        workspace_bytes, &progress);
    check(controlled_status == PGACCEL_INVALID_ARGUMENT || controlled_status == PGACCEL_UNSUPPORTED,
          "controlled projection rejects malformed request");
    check(std::memcmp(&progress, &controlled_poison, sizeof(progress)) == 0 && pgaccel_gpu_exec_count() == before,
          "controlled malformed request never dispatches or changes result");
  }
  for (auto invalid_pair : {pgaccel_join_row_pair{2, 0}, pgaccel_join_row_pair{0, 2},
                            pgaccel_join_row_pair{UINT64_MAX, 0}}) {
    const pgaccel_join_row_pair invalid_pairs[]{{0, 0}, invalid_pair};
    check(pgaccel_expr_device_copy_from_host(pairs.pointer, invalid_pairs, sizeof(invalid_pairs)) == PGACCEL_OK,
          "invalid row index fixture");
    pgaccel_join_project_result result{53, 59};
    check(pgaccel_join_rows_project(&request, error.pointer, PGACCEL_JOIN_PROJECT_WORKSPACE_BYTES, &result) == PGACCEL_INVALID_ARGUMENT,
          "device row bounds fail before projection");
    check(result.emitted_rows == 53 && result.kernel_launches == 59, "invalid index never publishes success");
    check(destination.download<int32_t>(2) == std::vector<int32_t>({-47, -47}), "invalid index leaves values untouched");
    check(nulls.download<uint8_t>(2) == std::vector<uint8_t>({0xcd, 0xcd}), "invalid index leaves nulls untouched");
    pgaccel_join_control control{PGACCEL_JOIN_CONTROL_VERSION, 0, 1, 1, nullptr, nullptr};
    pgaccel_join_controlled_result progress{};
    check(pgaccel_join_rows_project_controlled(&request, &control, error.pointer,
              PGACCEL_JOIN_PROJECT_WORKSPACE_BYTES, &progress) == PGACCEL_INVALID_ARGUMENT,
          "bad index in a later validation chunk prevents every gather write");
    check(destination.download<int32_t>(2) == std::vector<int32_t>({-47, -47}) &&
          nulls.download<uint8_t>(2) == std::vector<uint8_t>({0xcd, 0xcd}),
          "chunked validation is complete before any projected output");
  }
  const pgaccel_join_row_pair valid_pairs[]{{1, 0}, {0, PGACCEL_JOIN_ROWS_NULL_ROW}};
  check(pgaccel_expr_device_copy_from_host(pairs.pointer, valid_pairs, sizeof(valid_pairs)) == PGACCEL_OK,
        "retry row mappings");
  pgaccel_join_project_result result{};
  check(pgaccel_join_rows_project(&request, error.pointer, PGACCEL_JOIN_PROJECT_WORKSPACE_BYTES, &result) == PGACCEL_OK,
        "projection resets poisoned error word");
  check(destination.download<int32_t>(2) == std::vector<int32_t>({11, 7}), "retry returns correct values");
  // Maximum-width regression: pass column metadata through resident workspace,
  // not a pointer-rich descriptor expanded into Metal argument bindings.
  std::array<Buffer, PGACCEL_JOIN_PROJECT_MAX_COLUMNS> wide_values, wide_nulls;
  request.column_count = PGACCEL_JOIN_PROJECT_MAX_COLUMNS;
  for (size_t index = 0; index < request.column_count; ++index) {
    check(wide_values[index].allocate(8), "wide projection value allocation");
    check(wide_nulls[index].allocate(2), "wide projection null allocation");
    request.columns[index] = {static_cast<uint32_t>(index % 2), 4, source.pointer, nullptr, 8, 0,
                              wide_values[index].pointer, static_cast<uint8_t*>(wide_nulls[index].pointer), 8, 2};
  }
  check(pgaccel_join_rows_project(&request, error.pointer, PGACCEL_JOIN_PROJECT_WORKSPACE_BYTES, &result) == PGACCEL_OK,
        "all sixteen resident columns project safely");
  CancelState cancellation;
  pgaccel_join_control control{PGACCEL_JOIN_CONTROL_VERSION, 0, 1, 1, cancel_check, &cancellation};
  for (uint64_t stop = 1; stop <= 6; ++stop) {
    const int32_t poison_values[] = {-47, -47};
    const uint8_t poison_nulls[] = {0xcd, 0xcd};
    for (size_t index = 0; index < request.column_count; ++index) {
      check(pgaccel_expr_device_copy_from_host(wide_values[index].pointer, poison_values, sizeof(poison_values)) == PGACCEL_OK &&
            pgaccel_expr_device_copy_from_host(wide_nulls[index].pointer, poison_nulls, sizeof(poison_nulls)) == PGACCEL_OK,
            "poison all columns before each cancellation boundary");
    }
    cancellation = {0, stop};
    pgaccel_join_controlled_result progress{};
    const auto before = pgaccel_gpu_exec_count();
    check(pgaccel_join_rows_project_controlled(&request, &control, error.pointer,
              PGACCEL_JOIN_PROJECT_WORKSPACE_BYTES, &progress) == PGACCEL_TIMEOUT,
          "wide projection cancels at every fill/validate/gather boundary");
    check(progress.cancelled == 1 && progress.rows.emitted_rows == 0 && progress.cancel_checks == stop &&
          progress.rows.kernel_launches == stop - 1 && pgaccel_gpu_exec_count() - before == stop - 1,
          "cancelled projection never publishes partial columns or dispatches further");
    for (size_t index = 0; index < request.column_count; ++index) {
      const auto values = wide_values[index].download<int32_t>(2);
      const auto null_bytes = wide_nulls[index].download<uint8_t>(2);
      check(values[0] == (stop <= 4 ? -47 : (index % 2 == 0 ? 11 : 7)) &&
            values[1] == (stop <= 5 ? -47 : (index % 2 == 0 ? 7 : 0)) &&
            null_bytes[0] == (stop <= 4 ? 0xcd : 0) &&
            null_bytes[1] == (stop <= 5 ? 0xcd : (index % 2 == 0 ? 0 : 1)),
            "only completed gather chunks may touch poisoned outputs, never validation chunks");
    }
    cancellation = {};
    check(pgaccel_join_rows_project_controlled(&request, &control, error.pointer,
              PGACCEL_JOIN_PROJECT_WORKSPACE_BYTES, &progress) == PGACCEL_OK && progress.rows.emitted_rows == 2,
          "projection retry resets workspace after every cancellation boundary");
  }
  for (size_t mutation = 0; mutation < 10; ++mutation) {
    auto invalid = control;
    auto modified = request;
    pgaccel_join_controlled_result progress{};
    auto* result_pointer = &progress;
    switch (mutation) {
      case 0: invalid.version++; break;
      case 1: invalid.reserved = 1; break;
      case 2: invalid.work_items = 0; break;
      case 3: invalid.chain_steps = 0; break;
      case 4: invalid.work_items = UINT64_MAX; break;
      case 5: invalid.chain_steps = UINT64_MAX; break;
      case 6: invalid.cancel_check = nullptr; break;
      case 7: result_pointer = reinterpret_cast<pgaccel_join_controlled_result*>(&invalid); break;
      case 8: modified.columns[0].output_values = &invalid; break;
      case 9: modified.columns[0].output_nulls = reinterpret_cast<uint8_t*>(&invalid); break;
    }
    const auto before = pgaccel_gpu_exec_count();
    const auto status = pgaccel_join_rows_project_controlled(&modified, &invalid, error.pointer,
                                                            PGACCEL_JOIN_PROJECT_WORKSPACE_BYTES, result_pointer);
    check((status == PGACCEL_INVALID_ARGUMENT || status == PGACCEL_UNSUPPORTED) && pgaccel_gpu_exec_count() == before,
          "controlled projection validates limits and rejects writable control aliases before dispatch");
  }
  for (size_t index = 0; index < request.column_count; ++index) {
    check(wide_values[index].download<int32_t>(2) ==
              (index % 2 == 0 ? std::vector<int32_t>{11, 7} : std::vector<int32_t>{7, 0}),
          "wide projection keeps exact side values");
    check(wide_nulls[index].download<uint8_t>(2) ==
              (index % 2 == 0 ? std::vector<uint8_t>{0, 0} : std::vector<uint8_t>{0, 1}),
          "wide projection preserves null padding");
  }
}

void invalid_contracts(Fixture& fixture) {
  uint64_t bytes = 0;
  check(pgaccel_join_rows_workspace_bytes(&fixture.request, &bytes) == PGACCEL_OK, "contract fixture layout");
  Buffer workspace, output;
  check(workspace.allocate(bytes), "contract workspace");
  check(output.allocate(4096), "contract output");
  auto* pairs = static_cast<pgaccel_join_row_pair*>(output.pointer);
  for (int mutation = 0; mutation < 19; ++mutation) {
    auto request = fixture.request;
    uint64_t declared = bytes;
    uint64_t capacity = 8;
    void* scratch = workspace.pointer;
    auto* destination = pairs;
    switch (mutation) {
      case 0: request.version++; break;
      case 1: request.kind = 4; break;
      case 2: request.key_count = 0; break;
      case 3: request.key_count = 5; break;
      case 4: request.reserved = 1; break;
      case 5: request.outer_keys[0].reserved = 1; break;
      case 6: request.inner_keys[0].type = request.outer_keys[0].type == PGACCEL_JOIN_ROWS_INT32
                  ? PGACCEL_JOIN_ROWS_INT64 : PGACCEL_JOIN_ROWS_INT32; break;
      case 7: request.outer_keys[0].value_bytes = 1; break;
      case 8: request.inner_rows = UINT64_MAX; break;
      case 9: request.outer_keys[3].value_bytes = 1; break;
      case 10: declared = bytes - 1; break;
      case 11: scratch = output.pointer; destination = pairs; break;
      case 12: destination = reinterpret_cast<pgaccel_join_row_pair*>(const_cast<void*>(request.outer_keys[0].values)); break;
      case 13: capacity = UINT64_MAX; break;
      case 14: request.outer_keys[0].null_bytes = 1; break;
      case 15: request.outer_keys[0].type = 99; break;
      case 16: scratch = static_cast<uint8_t*>(scratch) + 1; break;
      case 17: request.outer_keys[0].values = static_cast<const uint8_t*>(request.outer_keys[0].values) + 1; break;
      case 18: scratch = &request; break;
    }
    const pgaccel_join_rows_result poison{47, 53, 59};
    auto result = poison;
    const auto before = pgaccel_gpu_exec_count();
    const auto status = pgaccel_join_rows_execute(&request, scratch, declared, destination, capacity, &result);
    check(status == PGACCEL_INVALID_ARGUMENT || status == PGACCEL_UNSUPPORTED, "invalid contract rejected");
    check(std::memcmp(&result, &poison, sizeof(result)) == 0, "invalid contract leaves result untouched");
    check(pgaccel_gpu_exec_count() == before, "invalid contract does not dispatch");
  }
  auto host_request = fixture.request;
  std::vector<int32_t> host_keys(host_request.outer_rows, 1);
  host_request.outer_keys[0].values = host_keys.data();
  pgaccel_join_rows_result result{};
  check(pgaccel_join_rows_execute(&host_request, workspace.pointer, bytes, pairs, 8, &result) == PGACCEL_INVALID_ARGUMENT,
        "ordinary host input is not device input");
  check(pgaccel_join_rows_workspace_bytes(nullptr, &bytes) == PGACCEL_INVALID_ARGUMENT, "null metadata input");
  check(pgaccel_join_rows_workspace_bytes(&fixture.request, nullptr) == PGACCEL_INVALID_ARGUMENT, "null metadata output");
  check(pgaccel_join_rows_workspace_bytes(&fixture.request,
          reinterpret_cast<uint64_t*>(const_cast<void*>(fixture.request.outer_keys[0].values))) == PGACCEL_INVALID_ARGUMENT,
        "layout output cannot alias resident input");
  check(pgaccel_join_rows_execute(&fixture.request, workspace.pointer, bytes, pairs, 8,
          reinterpret_cast<pgaccel_join_rows_result*>(workspace.pointer)) == PGACCEL_INVALID_ARGUMENT,
        "result cannot alias workspace");
}
} // namespace

int main() {
  // Metadata-only validation must work before runtime initialization.
  pgaccel_join_rows_request empty_request{};
  empty_request.version = PGACCEL_JOIN_ROWS_VERSION;
  empty_request.key_count = 1;
  empty_request.outer_keys[0].type = PGACCEL_JOIN_ROWS_INT32;
  empty_request.inner_keys[0].type = PGACCEL_JOIN_ROWS_INT32;
  uint64_t empty_bytes = 0;
  check(pgaccel_join_rows_workspace_bytes(&empty_request, &empty_bytes) == PGACCEL_OK && empty_bytes == 64,
        "layout query works without initializing GPU");
  check(pgaccel_gpu_exec_count() == 0, "layout query has no dispatch side effect");
  if (pgaccel_init() != PGACCEL_OK) {
    std::fprintf(stderr, "No GPU for resident join-row tests\n");
    return 77;
  }
  invalid_projection_contracts();
  for (uint32_t type : {PGACCEL_JOIN_ROWS_INT32, PGACCEL_JOIN_ROWS_INT64}) {
    Fixture base({HostKey{type, {1, 2, 2, 3, -1, 0, 7}, {0, 0, 0, 0, 0, 0, 255}}},
                 {HostKey{type, {2, 2, -1, 8, 0, 1}, {0, 0, 0, 0, 1, 0}}});
    run(base);
    invalid_contracts(base);
    Fixture self({HostKey{type, {1, 1, -1, 3}, {0, 0, 0, 1}}},
                 {HostKey{type, {1, 1, -1, 3}, {0, 0, 0, 1}}});
    self.request.inner_keys[0] = self.request.outer_keys[0];
    run(self);
    Fixture empty({HostKey{type, {}, {}}}, {HostKey{type, {}, {}}});
    run(empty);
    Fixture no_inner({HostKey{type, {1, 2, 3}, {0, 1, 0}}}, {HostKey{type, {}, {}}});
    run(no_inner);
    Fixture no_outer({HostKey{type, {}, {}}}, {HostKey{type, {1, 2}, {}}});
    run(no_outer);
    Fixture all_null({HostKey{type, {1, 1, 1}, {1, 2, 255}}}, {HostKey{type, {1, 1}, {1, 255}}});
    run(all_null);
  }
  for (size_t key_count : {size_t{2}, size_t{3}, size_t{4}}) {
    std::vector<HostKey> outer, inner;
    for (size_t key = 0; key < key_count; ++key) {
      const uint32_t type = key % 2 == 0 ? PGACCEL_JOIN_ROWS_INT32 : PGACCEL_JOIN_ROWS_INT64;
      outer.push_back({type, {1, 1, 2, -1, 4, 5}, {0, 0, 0, 0, 0, static_cast<uint8_t>(key == 1)}});
      inner.push_back({type, {1, 1, 1, -1, 9, 5}, {0, 0, 0, 0, 0, 0}});
      inner.back().values[2] += static_cast<int64_t>(key);
    }
    Fixture composite(std::move(outer), std::move(inner));
    run(composite);
  }
  Fixture extremes({HostKey{PGACCEL_JOIN_ROWS_INT64, {INT64_MIN, INT64_MAX, -1, 0, INT64_MIN}, {}}},
                   {HostKey{PGACCEL_JOIN_ROWS_INT64, {INT64_MAX, INT64_MIN, -1, INT64_MIN}, {}}});
  run(extremes);
  // Real residency omits NULL sidecars for nonnullable columns. Exercise
  // asymmetric optional pointers as well as fully present/absent metadata,
  // through all four join kinds and every supported composite-key count.
  for (size_t key_count = 1; key_count <= PGACCEL_JOIN_ROWS_MAX_KEYS; ++key_count) {
    for (unsigned mask = 0; mask < 4; ++mask) {
      std::vector<HostKey> outer, inner;
      for (size_t key = 0; key < key_count; ++key) {
        const uint32_t type = key % 2 == 0 ? PGACCEL_JOIN_ROWS_INT32 : PGACCEL_JOIN_ROWS_INT64;
        outer.push_back({type, {1, 1, 2, 3, 4}, mask & 1 ? std::vector<uint8_t>{0, 0, 0, 1, 0} : std::vector<uint8_t>{}});
        inner.push_back({type, {1, 1, 3}, mask & 2 ? std::vector<uint8_t>{0, 1, 0} : std::vector<uint8_t>{}});
      }
      Fixture optional(std::move(outer), std::move(inner));
      run(optional);
    }
  }
  Fixture skew({HostKey{PGACCEL_JOIN_ROWS_INT32, std::vector<int64_t>(65, -7), {}}},
               {HostKey{PGACCEL_JOIN_ROWS_INT32, std::vector<int64_t>(1025, -7), {}}});
  run(skew);
  for (size_t keys = 1; keys <= 4; ++keys) {
    std::vector<HostKey> outer, inner;
    for (size_t key = 0; key < keys; ++key) {
      const uint32_t type = key % 2 == 0 ? PGACCEL_JOIN_ROWS_INT32 : PGACCEL_JOIN_ROWS_INT64;
      outer.push_back({type, {1, 1, 2, 8, 0}, {0, 0, 0, 0, 1}});
      inner.push_back({type, {1, 1, 1, 2, 0}, {}});
    }
    Fixture bounded(std::move(outer), std::move(inner));
    // INNER/LEFT visit full chains, so their launch counts are independent of
    // atomic insertion order. Existence joins can stop early; exhaust their
    // cancellation boundaries on homogeneous keys below instead.
    for (uint32_t kind = 0; kind <= PGACCEL_JOIN_ROWS_ANTI; ++kind)
      run_controlled(bounded, kind, 3, 2, keys == 1 && kind <= PGACCEL_JOIN_ROWS_LEFT);
  }
  Fixture bounded_existence({HostKey{PGACCEL_JOIN_ROWS_INT32, {-7, -7, 8, 0}, {0, 0, 0, 1}}},
                            {HostKey{PGACCEL_JOIN_ROWS_INT32, std::vector<int64_t>(7, -7), {}}});
  for (uint32_t kind : {PGACCEL_JOIN_ROWS_SEMI, PGACCEL_JOIN_ROWS_ANTI})
    run_controlled(bounded_existence, kind, 2, 1, true);
  Fixture bounded_empty({HostKey{PGACCEL_JOIN_ROWS_INT32, {}, {}}},
                        {HostKey{PGACCEL_JOIN_ROWS_INT32, {}, {}}});
  run_controlled(bounded_empty, PGACCEL_JOIN_ROWS_INNER, 1, 1, true);
  Fixture bounded_left_empty({HostKey{PGACCEL_JOIN_ROWS_INT32, {1, 2}, {}}},
                             {HostKey{PGACCEL_JOIN_ROWS_INT32, {}, {}}});
  for (uint32_t kind = 0; kind <= PGACCEL_JOIN_ROWS_ANTI; ++kind)
    run_controlled(bounded_left_empty, kind, 1, 1, false);
  // Cross two prefix levels and exercise skew/duplicates/collisions, with an
  // independent host oracle. Only the test copies row-proportional results.
  for (size_t rows : {size_t{255}, size_t{256}, size_t{257}, size_t{65537}}) {
    HostKey outer{PGACCEL_JOIN_ROWS_INT32, {}, {}}, inner{PGACCEL_JOIN_ROWS_INT32, {}, {}};
    for (size_t row = 0; row < rows; ++row) {
      outer.values.push_back(static_cast<int64_t>(row % 23));
      outer.nulls.push_back(row % 17 == 0 ? 1 : 0);
    }
    for (size_t row = 0; row < 67; ++row) {
      inner.values.push_back(static_cast<int64_t>(row % 19));
      inner.nulls.push_back(row % 13 == 0 ? 1 : 0);
    }
    Fixture boundary({std::move(outer)}, {std::move(inner)});
    run(boundary);
    if (rows == 65537)
      run_controlled(boundary, PGACCEL_JOIN_ROWS_LEFT, 4096, 3, false);
  }
  std::printf("resident join rows: %d passed, %d failed\n", passed, failed);
  return failed == 0 ? 0 : 1;
}
