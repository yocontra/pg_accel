#include <sycl/sycl.hpp>

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>
#include <new>
#include <vector>

#include "ooo_overlap_support.h"
#include "pgaccel_queue.h"

namespace {

using clk = std::chrono::steady_clock;

struct run_report {
  uint64_t wall_ns = 0;
  uint64_t sort_start_ns = 0;
  uint64_t sort_end_ns = 0;
  uint64_t window_start_ns = 0;
  uint64_t window_end_ns = 0;
  uint64_t final_start_ns = 0;
  uint64_t final_end_ns = 0;
  uint64_t sort_kernel_count = 0;
};

static size_t next_power_of_two(size_t n) {
  if (n <= 1)
    return 2;
  --n;
  n |= n >> 1;
  n |= n >> 2;
  n |= n >> 4;
  n |= n >> 8;
  n |= n >> 16;
  if constexpr (sizeof(size_t) == 8)
    n |= n >> 32;
  return n + 1;
}

static uint64_t ns_since_epoch(const sycl::event& e,
                               sycl::info::event_profiling::command_start) {
  return static_cast<uint64_t>(
      e.get_profiling_info<sycl::info::event_profiling::command_start>());
}

static uint64_t ns_since_epoch(const sycl::event& e,
                               sycl::info::event_profiling::command_end) {
  return static_cast<uint64_t>(
      e.get_profiling_info<sycl::info::event_profiling::command_end>());
}

static sycl::event submit_sort_step(sycl::queue& q, int32_t* keys, uint32_t* indices,
                                    uint32_t* scratch, size_t n, size_t k, size_t j,
                                    uint32_t spin_iters,
                                    const std::vector<sycl::event>& deps,
                                    size_t lane) {
  return q.submit(sycl::property_list{
                      sycl::property::command_group::AdaptiveCpp_prefer_execution_lane{lane}},
                  [&](sycl::handler& h) {
                    if (!deps.empty())
                      h.depends_on(deps);
                    h.parallel_for(sycl::range<1>(n), [=](sycl::id<1> id) {
                      const size_t i = id[0];
                      const size_t partner = i ^ j;

                      uint32_t mix = static_cast<uint32_t>(i ^ partner ^ k ^ j);
                      for (uint32_t s = 0; s < spin_iters; ++s) {
                        mix = mix * 1664525u + 1013904223u;
                      }
                      scratch[i] = mix;

                      if (partner > i && partner < n) {
                        const bool ascending = ((i & k) == 0);
                        const int32_t vi = keys[i];
                        const int32_t vp = keys[partner];
                        const uint32_t ii = indices[i];
                        const uint32_t ip = indices[partner];

                        bool should_swap = ascending ? (vp < vi || (vp == vi && ip < ii))
                                                     : (vi < vp || (vp == vi && ii < ip));
                        if (should_swap) {
                          keys[i] = vp;
                          keys[partner] = vi;
                          indices[i] = ip;
                          indices[partner] = ii;
                        }
                      }
                    });
                  });
}

static sycl::event submit_window_row_number(sycl::queue& q, const size_t* part_start,
                                            int64_t* results, uint32_t* scratch, size_t n,
                                            uint32_t spin_iters,
                                            const std::vector<sycl::event>& deps,
                                            size_t lane) {
  return q.submit(sycl::property_list{
                      sycl::property::command_group::AdaptiveCpp_prefer_execution_lane{lane}},
                  [&](sycl::handler& h) {
                    if (!deps.empty())
                      h.depends_on(deps);
                    h.parallel_for(sycl::range<1>(n), [=](sycl::id<1> id) {
                      const size_t i = id[0];
                      uint32_t mix = static_cast<uint32_t>(i);
                      for (uint32_t s = 0; s < spin_iters; ++s) {
                        mix = mix * 22695477u + 1u;
                      }
                      scratch[i] = mix;
                      results[i] = static_cast<int64_t>(i - part_start[i] + 1);
                    });
                  });
}

static sycl::event submit_final_marker(sycl::queue& q, const uint32_t* sort_scratch,
                                       const uint32_t* window_scratch, uint32_t* marker,
                                       const std::vector<sycl::event>& deps, size_t lane) {
  return q.submit(sycl::property_list{
                      sycl::property::command_group::AdaptiveCpp_prefer_execution_lane{lane}},
                  [&](sycl::handler& h) {
                    if (!deps.empty())
                      h.depends_on(deps);
                    h.single_task([=]() { marker[0] = sort_scratch[0] ^ window_scratch[0]; });
                  });
}

static size_t bitonic_step_count(size_t n) {
  size_t steps = 0;
  for (size_t k = 2; k <= n; k *= 2) {
    for (size_t j = k / 2; j > 0; j /= 2)
      ++steps;
  }
  return steps;
}

static pgaccel_status run_probe_once(sycl::queue& q, bool overlap, size_t n,
                                     uint32_t spin_iters_per_sort_step,
                                     const std::vector<int32_t>& input_keys,
                                     const std::vector<uint32_t>& input_indices,
                                     const std::vector<size_t>& input_part_start,
                                     int32_t* d_keys, uint32_t* d_indices,
                                     size_t* d_part_start, int64_t* d_window_results,
                                     uint32_t* d_sort_scratch,
                                     uint32_t* d_window_scratch, uint32_t* d_marker,
                                     run_report* report) {
  q.memcpy(d_keys, input_keys.data(), n * sizeof(int32_t));
  q.memcpy(d_indices, input_indices.data(), n * sizeof(uint32_t));
  q.memcpy(d_part_start, input_part_start.data(), n * sizeof(size_t));
  q.memset(d_window_results, 0, n * sizeof(int64_t));
  q.memset(d_sort_scratch, 0, n * sizeof(uint32_t));
  q.memset(d_window_scratch, 0, n * sizeof(uint32_t));
  q.memset(d_marker, 0, sizeof(uint32_t));
  q.wait_and_throw();

  const uint32_t window_spin_iters = static_cast<uint32_t>(
      std::min<uint64_t>(std::numeric_limits<uint32_t>::max(),
                         static_cast<uint64_t>(spin_iters_per_sort_step) * bitonic_step_count(n) *
                             2));

  sycl::event first_sort_event;
  sycl::event last_sort_event;
  sycl::event window_event;
  sycl::event final_event;
  uint64_t sort_kernels = 0;

  const auto wall_start = clk::now();
  if (overlap) {
    window_event = submit_window_row_number(q, d_part_start, d_window_results, d_window_scratch, n,
                                            window_spin_iters, {}, 1);
  }

  std::vector<sycl::event> deps;
  for (size_t k = 2; k <= n; k *= 2) {
    for (size_t j = k / 2; j > 0; j /= 2) {
      sycl::event e = submit_sort_step(q, d_keys, d_indices, d_sort_scratch, n, k, j,
                                       spin_iters_per_sort_step, deps, 0);
      if (sort_kernels == 0)
        first_sort_event = e;
      last_sort_event = e;
      deps = {last_sort_event};
      ++sort_kernels;
    }
  }

  std::vector<sycl::event> window_deps;
  if (!overlap) {
    window_deps = {last_sort_event};
    window_event = submit_window_row_number(q, d_part_start, d_window_results, d_window_scratch, n,
                                            window_spin_iters, window_deps, 1);
  }

  final_event = submit_final_marker(q, d_sort_scratch, d_window_scratch, d_marker,
                                    {last_sort_event, window_event}, 2);
  final_event.wait_and_throw();
  const auto wall_end = clk::now();

  report->wall_ns =
      static_cast<uint64_t>(std::chrono::duration_cast<std::chrono::nanoseconds>(wall_end -
                                                                                 wall_start)
                                .count());
  report->sort_start_ns =
      ns_since_epoch(first_sort_event, sycl::info::event_profiling::command_start{});
  report->sort_end_ns = ns_since_epoch(last_sort_event, sycl::info::event_profiling::command_end{});
  report->window_start_ns =
      ns_since_epoch(window_event, sycl::info::event_profiling::command_start{});
  report->window_end_ns =
      ns_since_epoch(window_event, sycl::info::event_profiling::command_end{});
  report->final_start_ns =
      ns_since_epoch(final_event, sycl::info::event_profiling::command_start{});
  report->final_end_ns = ns_since_epoch(final_event, sycl::info::event_profiling::command_end{});
  report->sort_kernel_count = sort_kernels;

  return PGACCEL_OK;
}

}  // namespace

extern "C" pgaccel_status pgaccel_sort_window_overlap_probe(
    size_t count, uint32_t spin_iters_per_sort_step, pgaccel_ooo_overlap_report* out) try {
  if (out == nullptr)
    return PGACCEL_ERROR;
  std::memset(out, 0, sizeof(*out));
  sycl::queue* ooo = pgaccel_get_ooo_queue();
  if (ooo == nullptr || ooo->is_in_order())
    return PGACCEL_UNSUPPORTED;

  size_t n = next_power_of_two(count);
  if (n < 1024)
    n = 1024;
  if (n > 65536)
    return PGACCEL_ERROR;
  if (spin_iters_per_sort_step == 0)
    spin_iters_per_sort_step = 1;

  sycl::queue& q = *ooo;

  std::vector<int32_t> input_keys(n);
  std::vector<uint32_t> input_indices(n);
  std::vector<size_t> input_part_start(n, 0);
  for (size_t i = 0; i < n; ++i) {
    input_keys[i] = static_cast<int32_t>(n - i);
    input_indices[i] = static_cast<uint32_t>(i);
  }

  int32_t* d_keys = nullptr;
  uint32_t* d_indices = nullptr;
  size_t* d_part_start = nullptr;
  int64_t* d_window_results = nullptr;
  uint32_t* d_sort_scratch = nullptr;
  uint32_t* d_window_scratch = nullptr;
  uint32_t* d_marker = nullptr;
  pgaccel_status status = PGACCEL_OK;

  try {
    d_keys = sycl::malloc_device<int32_t>(n, q);
    d_indices = sycl::malloc_device<uint32_t>(n, q);
    d_part_start = sycl::malloc_device<size_t>(n, q);
    d_window_results = sycl::malloc_device<int64_t>(n, q);
    d_sort_scratch = sycl::malloc_device<uint32_t>(n, q);
    d_window_scratch = sycl::malloc_device<uint32_t>(n, q);
    d_marker = sycl::malloc_device<uint32_t>(1, q);
    if (!d_keys || !d_indices || !d_part_start || !d_window_results || !d_sort_scratch ||
        !d_window_scratch || !d_marker) {
      throw std::bad_alloc();
    }

    run_report warmup;
    status = run_probe_once(q, true, n, 1, input_keys, input_indices, input_part_start, d_keys,
                            d_indices, d_part_start, d_window_results, d_sort_scratch,
                            d_window_scratch, d_marker, &warmup);
    run_report serial;
    run_report overlap;

    if (status == PGACCEL_OK) {
      status = run_probe_once(q, false, n, spin_iters_per_sort_step, input_keys, input_indices,
                              input_part_start, d_keys, d_indices, d_part_start, d_window_results,
                              d_sort_scratch, d_window_scratch, d_marker, &serial);
    }
    if (status == PGACCEL_OK) {
      status = run_probe_once(q, true, n, spin_iters_per_sort_step, input_keys, input_indices,
                              input_part_start, d_keys, d_indices, d_part_start, d_window_results,
                              d_sort_scratch, d_window_scratch, d_marker, &overlap);
    }

    std::vector<int32_t> sorted_keys(n);
    std::vector<int64_t> window_results(n);
    if (status == PGACCEL_OK) {
      q.memcpy(sorted_keys.data(), d_keys, n * sizeof(int32_t)).wait_and_throw();
      q.memcpy(window_results.data(), d_window_results, n * sizeof(int64_t)).wait_and_throw();

      for (size_t i = 1; i < n; ++i) {
        if (sorted_keys[i] < sorted_keys[i - 1]) {
          status = PGACCEL_ERROR;
          break;
        }
      }
    }
    if (status == PGACCEL_OK) {
      for (size_t i = 0; i < n; ++i) {
        if (window_results[i] != static_cast<int64_t>(i + 1)) {
          status = PGACCEL_ERROR;
          break;
        }
      }
    }

    if (status == PGACCEL_OK) {
      out->serial_wall_ns = serial.wall_ns;
      out->overlap_wall_ns = overlap.wall_ns;
      out->sort_start_ns = overlap.sort_start_ns;
      out->sort_end_ns = overlap.sort_end_ns;
      out->window_start_ns = overlap.window_start_ns;
      out->window_end_ns = overlap.window_end_ns;
      out->final_start_ns = overlap.final_start_ns;
      out->final_end_ns = overlap.final_end_ns;
      out->sort_kernel_count = overlap.sort_kernel_count;
      out->spans_overlap = overlap.window_start_ns < overlap.sort_end_ns &&
                           overlap.sort_start_ns < overlap.window_end_ns;
      out->wall_time_improved = overlap.wall_ns < serial.wall_ns;

      pgaccel_record_gpu_exec();
    }
  } catch (const sycl::exception& e) {
    fprintf(stderr, "pgaccel: OOO overlap probe failed: %s\n", e.what());
    status = PGACCEL_ERROR;
  } catch (const std::bad_alloc&) {
    status = PGACCEL_OOM;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: OOO overlap probe failed: %s\n", e.what());
    status = PGACCEL_ERROR;
  }

  if (d_marker)
    sycl::free(d_marker, q);
  if (d_window_scratch)
    sycl::free(d_window_scratch, q);
  if (d_sort_scratch)
    sycl::free(d_sort_scratch, q);
  if (d_window_results)
    sycl::free(d_window_results, q);
  if (d_part_start)
    sycl::free(d_part_start, q);
  if (d_indices)
    sycl::free(d_indices, q);
  if (d_keys)
    sycl::free(d_keys, q);

  return status;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_sort_window_overlap_probe", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_sort_window_overlap_probe", nullptr);
}
