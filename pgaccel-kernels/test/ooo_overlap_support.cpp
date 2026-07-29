#include "ooo_overlap_support.h"

// Representative backend serialization diagnostic only. These reduce-like and
// count-like kernels deliberately avoid production APIs so their event
// dependencies are fully controlled. Results may establish backend queue
// serialization, never production-kernel correctness, coverage, or overlap.

#include <sycl/sycl.hpp>

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>
#include <new>
#include <vector>

sycl::queue* pgaccel_get_ooo_queue();

namespace {

using Clock = std::chrono::steady_clock;
constexpr size_t kGroupCount = 64;

pgaccel_status report_probe_failure(const std::exception* error) {
  std::fprintf(stderr, "pgaccel: pgaccel_resident_reduce_overlap_probe: GPU kernel failure: %s\n",
               error != nullptr ? error->what() : "unknown C++ exception");
  return PGACCEL_ERROR;
}

struct RunReport {
  uint64_t wall_ns = 0;
  uint64_t reduce_start_ns = 0;
  uint64_t reduce_end_ns = 0;
  uint64_t resident_start_ns = 0;
  uint64_t resident_end_ns = 0;
  uint64_t final_start_ns = 0;
  uint64_t final_end_ns = 0;
};

uint64_t event_start(const sycl::event& event) {
  return static_cast<uint64_t>(
      event.get_profiling_info<sycl::info::event_profiling::command_start>());
}

uint64_t event_end(const sycl::event& event) {
  return static_cast<uint64_t>(
      event.get_profiling_info<sycl::info::event_profiling::command_end>());
}

sycl::event submit_reduce(sycl::queue& queue, const uint32_t* values, uint32_t* sum,
                          uint32_t* scratch, size_t count, uint32_t spin_iters,
                          const std::vector<sycl::event>& dependencies, size_t lane) {
  return queue.submit(
      sycl::property_list{sycl::property::command_group::AdaptiveCpp_prefer_execution_lane{lane}},
      [&](sycl::handler& handler) {
        if (!dependencies.empty())
          handler.depends_on(dependencies);
        handler.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
          const size_t row = id[0];
          uint32_t mix = static_cast<uint32_t>(row);
          for (uint32_t step = 0; step < spin_iters; ++step)
            mix = mix * 1664525u + 1013904223u;
          scratch[row] = mix;
          sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                           sycl::access::address_space::global_space>
              sum_ref(*sum);
          sum_ref.fetch_add(values[row]);
        });
      });
}

sycl::event submit_resident_count(sycl::queue& queue, const uint32_t* keys, uint32_t* counts,
                                  uint32_t* scratch, size_t count, uint32_t spin_iters,
                                  const std::vector<sycl::event>& dependencies, size_t lane) {
  return queue.submit(
      sycl::property_list{sycl::property::command_group::AdaptiveCpp_prefer_execution_lane{lane}},
      [&](sycl::handler& handler) {
        if (!dependencies.empty())
          handler.depends_on(dependencies);
        handler.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
          const size_t row = id[0];
          uint32_t mix = static_cast<uint32_t>(row ^ keys[row]);
          for (uint32_t step = 0; step < spin_iters; ++step)
            mix = mix * 22695477u + 1u;
          scratch[row] = mix;
          const size_t group = static_cast<size_t>(keys[row] & (kGroupCount - 1));
          sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                           sycl::access::address_space::global_space>
              count_ref(counts[group]);
          count_ref.fetch_add(1u);
        });
      });
}

sycl::event submit_final_marker(sycl::queue& queue, const uint32_t* reduce_scratch,
                                const uint32_t* resident_scratch, uint32_t* marker,
                                const std::vector<sycl::event>& dependencies, size_t lane) {
  return queue.submit(
      sycl::property_list{sycl::property::command_group::AdaptiveCpp_prefer_execution_lane{lane}},
      [&](sycl::handler& handler) {
        handler.depends_on(dependencies);
        handler.single_task([=]() { marker[0] = reduce_scratch[0] ^ resident_scratch[0]; });
      });
}

pgaccel_status run_once(sycl::queue& queue, bool overlap, const uint32_t* device_values,
                        const uint32_t* device_keys, uint32_t* device_sum, uint32_t* device_counts,
                        uint32_t* reduce_scratch, uint32_t* resident_scratch, uint32_t* marker,
                        size_t count, uint32_t spin_iters, RunReport* report) {
  queue.memset(device_sum, 0, sizeof(*device_sum));
  queue.memset(device_counts, 0, kGroupCount * sizeof(*device_counts));
  queue.memset(reduce_scratch, 0, count * sizeof(*reduce_scratch));
  queue.memset(resident_scratch, 0, count * sizeof(*resident_scratch));
  queue.memset(marker, 0, sizeof(*marker));
  queue.wait_and_throw();

  const auto wall_start = Clock::now();
  const sycl::event reduce =
      submit_reduce(queue, device_values, device_sum, reduce_scratch, count, spin_iters, {}, 0);
  const std::vector<sycl::event> resident_dependencies =
      overlap ? std::vector<sycl::event>{} : std::vector<sycl::event>{reduce};
  const sycl::event resident =
      submit_resident_count(queue, device_keys, device_counts, resident_scratch, count, spin_iters,
                            resident_dependencies, 1);
  sycl::event final =
      submit_final_marker(queue, reduce_scratch, resident_scratch, marker, {reduce, resident}, 2);
  final.wait_and_throw();
  const auto wall_end = Clock::now();

  report->wall_ns = static_cast<uint64_t>(
      std::chrono::duration_cast<std::chrono::nanoseconds>(wall_end - wall_start).count());
  report->reduce_start_ns = event_start(reduce);
  report->reduce_end_ns = event_end(reduce);
  report->resident_start_ns = event_start(resident);
  report->resident_end_ns = event_end(resident);
  report->final_start_ns = event_start(final);
  report->final_end_ns = event_end(final);
  return PGACCEL_OK;
}

}  // namespace

extern "C" pgaccel_status
pgaccel_resident_reduce_overlap_probe(size_t count, uint32_t spin_iters,
                                      pgaccel_ooo_overlap_report* out) try {
  if (out == nullptr)
    return PGACCEL_ERROR;
  std::memset(out, 0, sizeof(*out));
  sycl::queue* queue = pgaccel_get_ooo_queue();
  if (queue == nullptr || queue->is_in_order())
    return PGACCEL_UNSUPPORTED;

  const size_t rows = std::clamp(count, size_t{1024}, size_t{65536});
  if (spin_iters == 0)
    spin_iters = 1;

  std::vector<uint32_t> values(rows);
  std::vector<uint32_t> keys(rows);
  uint32_t expected_sum = 0;
  uint32_t expected_counts[kGroupCount] = {};
  for (size_t row = 0; row < rows; ++row) {
    values[row] = static_cast<uint32_t>(row % 17);
    keys[row] = static_cast<uint32_t>((row * 37 + 11) % kGroupCount);
    expected_sum += values[row];
    ++expected_counts[keys[row]];
  }

  uint32_t* device_values = nullptr;
  uint32_t* device_keys = nullptr;
  uint32_t* device_sum = nullptr;
  uint32_t* device_counts = nullptr;
  uint32_t* reduce_scratch = nullptr;
  uint32_t* resident_scratch = nullptr;
  uint32_t* marker = nullptr;
  pgaccel_status status = PGACCEL_OK;
  try {
    device_values = sycl::malloc_device<uint32_t>(rows, *queue);
    device_keys = sycl::malloc_device<uint32_t>(rows, *queue);
    device_sum = sycl::malloc_device<uint32_t>(1, *queue);
    device_counts = sycl::malloc_device<uint32_t>(kGroupCount, *queue);
    reduce_scratch = sycl::malloc_device<uint32_t>(rows, *queue);
    resident_scratch = sycl::malloc_device<uint32_t>(rows, *queue);
    marker = sycl::malloc_device<uint32_t>(1, *queue);
    if (device_values == nullptr || device_keys == nullptr || device_sum == nullptr ||
        device_counts == nullptr || reduce_scratch == nullptr || resident_scratch == nullptr ||
        marker == nullptr) {
      throw std::bad_alloc();
    }
    queue->memcpy(device_values, values.data(), rows * sizeof(values[0]));
    queue->memcpy(device_keys, keys.data(), rows * sizeof(keys[0]));
    queue->wait_and_throw();

    RunReport warmup;
    RunReport serial;
    RunReport overlap;
    status = run_once(*queue, true, device_values, device_keys, device_sum, device_counts,
                      reduce_scratch, resident_scratch, marker, rows, 1, &warmup);
    if (status == PGACCEL_OK) {
      status = run_once(*queue, false, device_values, device_keys, device_sum, device_counts,
                        reduce_scratch, resident_scratch, marker, rows, spin_iters, &serial);
    }
    if (status == PGACCEL_OK) {
      status = run_once(*queue, true, device_values, device_keys, device_sum, device_counts,
                        reduce_scratch, resident_scratch, marker, rows, spin_iters, &overlap);
    }

    uint32_t observed_sum = 0;
    uint32_t observed_counts[kGroupCount] = {};
    if (status == PGACCEL_OK) {
      queue->memcpy(&observed_sum, device_sum, sizeof(observed_sum));
      queue->memcpy(observed_counts, device_counts, sizeof(observed_counts));
      queue->wait_and_throw();
      if (observed_sum != expected_sum ||
          !std::equal(std::begin(observed_counts), std::end(observed_counts),
                      std::begin(expected_counts))) {
        status = PGACCEL_ERROR;
      }
    }

    if (status == PGACCEL_OK) {
      out->serial_wall_ns = serial.wall_ns;
      out->overlap_wall_ns = overlap.wall_ns;
      out->reduce_start_ns = overlap.reduce_start_ns;
      out->reduce_end_ns = overlap.reduce_end_ns;
      out->resident_start_ns = overlap.resident_start_ns;
      out->resident_end_ns = overlap.resident_end_ns;
      out->final_start_ns = overlap.final_start_ns;
      out->final_end_ns = overlap.final_end_ns;
      out->spans_overlap = overlap.resident_start_ns < overlap.reduce_end_ns &&
                           overlap.reduce_start_ns < overlap.resident_end_ns;
      out->wall_time_improved = overlap.wall_ns < serial.wall_ns;
      pgaccel_record_gpu_exec();
    }
  } catch (const sycl::exception& error) {
    std::fprintf(stderr, "pgaccel: resident/reduce OOO probe failed: %s\n", error.what());
    status = PGACCEL_ERROR;
  } catch (const std::bad_alloc&) {
    status = PGACCEL_OOM;
  } catch (const std::exception& error) {
    std::fprintf(stderr, "pgaccel: resident/reduce OOO probe failed: %s\n", error.what());
    status = PGACCEL_ERROR;
  }

  if (marker != nullptr)
    sycl::free(marker, *queue);
  if (resident_scratch != nullptr)
    sycl::free(resident_scratch, *queue);
  if (reduce_scratch != nullptr)
    sycl::free(reduce_scratch, *queue);
  if (device_counts != nullptr)
    sycl::free(device_counts, *queue);
  if (device_sum != nullptr)
    sycl::free(device_sum, *queue);
  if (device_keys != nullptr)
    sycl::free(device_keys, *queue);
  if (device_values != nullptr)
    sycl::free(device_values, *queue);
  return status;
} catch (const std::exception& error) {
  return report_probe_failure(&error);
} catch (...) {
  return report_probe_failure(nullptr);
}
