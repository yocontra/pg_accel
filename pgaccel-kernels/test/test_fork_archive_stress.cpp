// test_fork_archive_stress: Phase 2 "Metal pipeline-state XPC edge case"
// instrumentation harness.
//
// Reproduces the PG postmaster→backend fork pattern at scale: parent
// process never touches Metal; spawns N children; each child runs M
// iterations of a representative kernel mix (reduce f32, h3 lat_lng,
// PIP-simple); per-child stderr is captured and scanned for the
// AdaptiveCpp runtime's archive failure markers; archive cache state is
// snapshotted before/after.
//
// Acceptance gate: an N=8 × M=20 stress run shows
// **zero** XPC errors. The harness exits non-zero if any child surfaced
// an `MTLCompilerService` / pipeline-state failure / archive load
// failure log line, or if the per-child dispatch matrix produced wrong
// results. It also prints per-fork first-dispatch timing and cache-id
// evidence so reviewers can distinguish unstable kernel hashes from the
// unavoidable per-process Metal library/archive/pipeline construction cost.
// The per-child report on stdout is the source of truth for reviewers —
// paste it verbatim in the task report.

#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <sys/wait.h>
#include <unistd.h>

#include <atomic>
#include <cerrno>
#include <chrono>
#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <set>
#include <sstream>
#include <string>
#include <vector>

#include "pgaccel_ffi.h"

// ──────────────────────────────────────────────────────────────────────
// Stress parameters. The acceptance contract pins N=8, M=20 as the
// matrix. Override via env for diagnostics / extended soak.
// ──────────────────────────────────────────────────────────────────────

static int parse_env_int(const char* name, int fallback, int max_value = 1024) {
  const char* v = std::getenv(name);
  if (!v || v[0] == '\0')
    return fallback;
  char* end = nullptr;
  long parsed = std::strtol(v, &end, 10);
  if (end == v || parsed < 1 || parsed > max_value)
    return fallback;
  return static_cast<int>(parsed);
}

static const int N_WORKERS = parse_env_int("PGACCEL_FORK_STRESS_WORKERS", 8);
static const int N_ITERATIONS = parse_env_int("PGACCEL_FORK_STRESS_ITERS", 20);
static const int FIRST_DISPATCH_BUDGET_US =
    parse_env_int("PGACCEL_FORK_FIRST_DISPATCH_BUDGET_US", 50'000, 60'000'000);

// Kernel input sizes — small enough that the iteration loop dominates,
// large enough that GPU dispatch is actually exercised (above
// gpu_min_rows).
static constexpr size_t REDUCE_N = 100'000;
static constexpr size_t H3_N = 100'000;
static constexpr size_t PIP_N = 100'000;

// The stress loop deterministically materializes five Metal code objects:
// two-pass reduce emits an ndrange partial reduction plus a single_task
// finalizer, fp64 H3 emits projection plus integer H3 assembly, and PIP emits
// one code object.
static constexpr size_t EXPECTED_EMPTY_CACHE_IDS = 5;

// ──────────────────────────────────────────────────────────────────────
// Archive-failure marker patterns. These are emitted by AdaptiveCpp's
// `metal_code_object.cpp` / `metal_queue.cpp` when HIPSYCL_DEBUG_LEVEL >= 2.
// We bump to level 3 in the child so the INFO-level "loaded binary
// archive from ..." line is also visible, but the error markers below
// fire at WARNING (level 2) regardless. Any hit is a fail.
// ──────────────────────────────────────────────────────────────────────

struct MarkerSet {
  std::vector<std::string> xpc_compiler_service;  // "MTLCompilerService" anywhere
  std::vector<std::string> pipeline_state_fail;   // "Failed to create compute pipeline state"
  std::vector<std::string> archive_load_fail;     // "newBinaryArchive(url) failed"
  std::vector<std::string> archive_build_fail;    // "acpp-metal-archive-build exited with status"
  std::vector<std::string> archive_skipped;       // "archive build skipped for"
  std::vector<std::string> posix_spawn_fail;      // "posix_spawn(...) failed"
};

static void scan_marker(MarkerSet& m, const std::string& line) {
  // Order matters: we want the most specific marker per line.
  if (line.find("MTLCompilerService") != std::string::npos) {
    m.xpc_compiler_service.push_back(line);
  }
  if (line.find("Failed to create compute pipeline state") != std::string::npos) {
    m.pipeline_state_fail.push_back(line);
  }
  if (line.find("newBinaryArchive(url) failed") != std::string::npos) {
    m.archive_load_fail.push_back(line);
  }
  if (line.find("acpp-metal-archive-build exited with status") != std::string::npos) {
    m.archive_build_fail.push_back(line);
  }
  if (line.find("archive build skipped for") != std::string::npos) {
    m.archive_skipped.push_back(line);
  }
  if (line.find("posix_spawn") != std::string::npos && line.find("failed") != std::string::npos) {
    m.posix_spawn_fail.push_back(line);
  }
}

static size_t total_failures(const MarkerSet& m) {
  return m.xpc_compiler_service.size() + m.pipeline_state_fail.size() + m.archive_load_fail.size() +
         m.archive_build_fail.size() + m.posix_spawn_fail.size();
}

static uint64_t elapsed_us(std::chrono::steady_clock::time_point start,
                           std::chrono::steady_clock::time_point end) {
  return static_cast<uint64_t>(std::chrono::duration_cast<std::chrono::microseconds>(end - start).count());
}

static double us_to_ms(uint64_t us) {
  return static_cast<double>(us) / 1000.0;
}

static std::vector<std::string> list_cache_ids(const char* cache_dir, const char* extension) {
  std::vector<std::string> ids;
  if (!cache_dir || cache_dir[0] == '\0')
    return ids;

  std::error_code ec;
  if (!std::filesystem::is_directory(cache_dir, ec) || ec)
    return ids;

  std::set<std::string> unique;
  for (const auto& entry : std::filesystem::directory_iterator(cache_dir, ec)) {
    if (ec)
      break;
    if (entry.path().extension() == extension)
      unique.insert(entry.path().stem().string());
  }
  ids.assign(unique.begin(), unique.end());
  return ids;
}

static std::string join_ids(const std::vector<std::string>& ids) {
  if (ids.empty())
    return "<none>";
  std::ostringstream out;
  for (size_t i = 0; i < ids.size(); ++i) {
    if (i != 0)
      out << ",";
    out << ids[i];
  }
  return out.str();
}

// ──────────────────────────────────────────────────────────────────────
// Per-child report passed parent ← child through a binary pipe. Kept
// trivially-copyable so a single write/read pair is sufficient.
// ──────────────────────────────────────────────────────────────────────

struct ChildReport {
  pid_t pid;
  int worker_index;
  int iterations_attempted;
  int iterations_passed;
  int pgaccel_init_status;
  int first_failure_iter;          // -1 if all passed
  int first_failure_kernel;        // 0=reduce 1=h3 2=pip; -1 if none
  int first_failure_status;        // pgaccel_status of the failing call
  uint64_t gpu_exec_count_before;  // after reset() in child, before dispatch loop
  uint64_t gpu_exec_count_after;
  uint64_t archive_metallib_before;
  uint64_t archive_metalar_before;
  uint64_t archive_orphan_before;
  uint64_t archive_metallib_after;
  uint64_t archive_metalar_after;
  uint64_t archive_orphan_after;
  uint32_t compute_units;
  uint32_t backend_is_metal;  // 1 if backend == "metal"
  uint64_t init_us;
  uint64_t first_iteration_us;
  uint64_t first_reduce_us;
  uint64_t first_h3_us;
  uint64_t first_pip_us;
  uint64_t warm_iterations;
  uint64_t warm_iteration_total_us;
  uint64_t warm_iteration_max_us;
  uint64_t warm_reduce_total_us;
  uint64_t warm_reduce_max_us;
  uint64_t warm_h3_total_us;
  uint64_t warm_h3_max_us;
  uint64_t warm_pip_total_us;
  uint64_t warm_pip_max_us;
  uint64_t wall_us;
};

// ──────────────────────────────────────────────────────────────────────
// Per-iteration kernel mix. Each kernel is small so the cost is
// dominated by per-dispatch overhead — exactly the path that exercises
// the pipeline-state code in `metal_queue.cpp::launch_kernel_from_library`.
// Return value: 0 = OK, otherwise (kernel_id << 16) | (pgaccel_status & 0xff).
// ──────────────────────────────────────────────────────────────────────

struct IterationTiming {
  uint64_t reduce_us = 0;
  uint64_t h3_us = 0;
  uint64_t pip_us = 0;
  uint64_t total_us = 0;
};

static int run_one_iteration(std::vector<float>& reduce_buf, std::vector<double>& h3_lat,
                             std::vector<double>& h3_lng, std::vector<uint64_t>& h3_cells,
                             std::vector<uint8_t>& h3_valid, std::vector<float>& pip_points,
                             std::vector<int8_t>& pip_results, const float* pip_bbox,
                             const float* pip_ring, IterationTiming* timing = nullptr) {
  const auto iter_t0 = std::chrono::steady_clock::now();

  // ── reduce_sum_f32 ──
  float sum = 0.0f;
  const auto reduce_t0 = std::chrono::steady_clock::now();
  pgaccel_status st = pgaccel_reduce_sum_f32(reduce_buf.data(), REDUCE_N, &sum);
  const auto reduce_t1 = std::chrono::steady_clock::now();
  if (timing)
    timing->reduce_us = elapsed_us(reduce_t0, reduce_t1);
  if (st != PGACCEL_OK) {
    return (0 << 16) | (st & 0xff);
  }
  // Cheap sanity: REDUCE_N entries each == 1.0f → sum within tolerance.
  if (std::fabs(sum - static_cast<float>(REDUCE_N)) > 1.0f) {
    return (0 << 16) | 0xfe;  // wrong-result sentinel
  }

  // ── h3_lat_lng_to_cell_bulk ──
  const auto h3_t0 = std::chrono::steady_clock::now();
  st = pgaccel_h3_lat_lng_to_cell_bulk(h3_lat.data(), h3_lng.data(), H3_N,
                                       /*resolution=*/7, /*use_fp64=*/1, h3_cells.data(),
                                       h3_valid.data());
  const auto h3_t1 = std::chrono::steady_clock::now();
  if (timing)
    timing->h3_us = elapsed_us(h3_t0, h3_t1);
  if (st != PGACCEL_OK) {
    return (1 << 16) | (st & 0xff);
  }
  if (h3_cells[0] == 0 || h3_valid[0] != 1) {
    return (1 << 16) | 0xfe;
  }

  // ── point_in_polygon_bulk (simple, no rings index) ──
  const auto pip_t0 = std::chrono::steady_clock::now();
  st = pgaccel_point_in_polygon_bulk(pip_points.data(), PIP_N, pip_bbox, pip_ring, 5, nullptr, 0,
                                     pip_results.data());
  const auto pip_t1 = std::chrono::steady_clock::now();
  if (timing) {
    timing->pip_us = elapsed_us(pip_t0, pip_t1);
    timing->total_us = elapsed_us(iter_t0, pip_t1);
  }
  if (st != PGACCEL_OK) {
    return (2 << 16) | (st & 0xff);
  }
  return 0;
}

// ──────────────────────────────────────────────────────────────────────
// Child entry point. fd_out is the write end of the report pipe; the
// child must NOT close it until after the final write. fd_err is the
// write end of the stderr pipe; we dup2 it onto fd 2 so AdaptiveCpp's
// HIPSYCL_DEBUG_* messages land in the parent for marker scanning.
// ──────────────────────────────────────────────────────────────────────

[[noreturn]] static void run_child(int worker_index, int fd_out, int fd_err) {
  // Redirect stderr → pipe so the parent can scan AdaptiveCpp diagnostics.
  if (dup2(fd_err, STDERR_FILENO) < 0) {
    // No way to log the error meaningfully — just exit.
    _exit(80);
  }
  // The pipe write-end is now stderr; close the duplicate.
  close(fd_err);

  // Make AdaptiveCpp emit info-level archive log lines so the marker
  // scanner can see them. Done after the dup2 so the env var is set in
  // the same process that loads libacpp-rt.
  setenv("HIPSYCL_DEBUG_LEVEL", "3", /*overwrite=*/1);

  ChildReport report{};
  report.pid = getpid();
  report.worker_index = worker_index;
  report.first_failure_iter = -1;
  report.first_failure_kernel = -1;
  report.first_failure_status = 0;

  // Archive snapshot before init (parent's pre-fork view as inherited).
  pgaccel_archive_snapshot snap_before{};
  pgaccel_archive_stats_snapshot(&snap_before);
  report.archive_metallib_before = snap_before.metallib_files;
  report.archive_metalar_before = snap_before.metalar_files;
  report.archive_orphan_before = snap_before.orphan_metallib;

  const auto init_t0 = std::chrono::steady_clock::now();
  pgaccel_status init_st = pgaccel_init();
  const auto init_t1 = std::chrono::steady_clock::now();
  report.init_us = elapsed_us(init_t0, init_t1);
  report.pgaccel_init_status = static_cast<int>(init_st);
  if (init_st != PGACCEL_OK) {
    write(fd_out, &report, sizeof(report));
    close(fd_out);
    _exit(81);
  }

  pgaccel_device_info info = pgaccel_get_device_info();
  report.compute_units = info.compute_units;
  report.backend_is_metal = (std::strcmp(info.backend_name, "metal") == 0) ? 1u : 0u;

  pgaccel_reset_gpu_exec_count();
  report.gpu_exec_count_before = pgaccel_gpu_exec_count();

  // Allocate per-iteration buffers once; the kernels are run in a loop
  // so the cost amortizes against fork/init.
  std::vector<float> reduce_buf(REDUCE_N, 1.0f);

  std::vector<double> h3_lat(H3_N);
  std::vector<double> h3_lng(H3_N);
  for (size_t i = 0; i < H3_N; ++i) {
    // Spread points around NYC so cells differ across the batch.
    h3_lat[i] = 40.7128 + static_cast<double>(i % 1000) * 1e-5;
    h3_lng[i] = -74.0060 + static_cast<double>(i % 1000) * 1e-5;
  }
  std::vector<uint64_t> h3_cells(H3_N, 0);
  std::vector<uint8_t> h3_valid(H3_N, 0);

  std::vector<float> pip_points(PIP_N * 2);
  for (size_t i = 0; i < PIP_N; ++i) {
    // Half inside, half outside a unit diamond bbox.
    if ((i & 1u) == 0u) {
      pip_points[i * 2] = 0.25f;
      pip_points[i * 2 + 1] = 0.10f;
    } else {
      pip_points[i * 2] = 2.0f;
      pip_points[i * 2 + 1] = 2.0f;
    }
  }
  std::vector<int8_t> pip_results(PIP_N, 99);
  const float pip_bbox[4] = {-1.0f, -1.0f, 1.0f, 1.0f};
  const float pip_ring[10] = {0.0f, 1.0f, 1.0f, 0.0f, 0.0f, -1.0f, -1.0f, 0.0f, 0.0f, 1.0f};

  const auto t0 = std::chrono::steady_clock::now();

  for (int iter = 0; iter < N_ITERATIONS; ++iter) {
    report.iterations_attempted = iter + 1;
    IterationTiming timing{};
    int rc = run_one_iteration(reduce_buf, h3_lat, h3_lng, h3_cells, h3_valid, pip_points,
                               pip_results, pip_bbox, pip_ring, &timing);
    if (iter == 0) {
      report.first_iteration_us = timing.total_us;
      report.first_reduce_us = timing.reduce_us;
      report.first_h3_us = timing.h3_us;
      report.first_pip_us = timing.pip_us;
    }
    if (rc != 0) {
      report.first_failure_iter = iter;
      report.first_failure_kernel = (rc >> 16) & 0xff;
      report.first_failure_status = rc & 0xff;
      break;
    }
    if (iter > 0) {
      ++report.warm_iterations;
      report.warm_iteration_total_us += timing.total_us;
      report.warm_iteration_max_us = std::max(report.warm_iteration_max_us, timing.total_us);
      report.warm_reduce_total_us += timing.reduce_us;
      report.warm_reduce_max_us = std::max(report.warm_reduce_max_us, timing.reduce_us);
      report.warm_h3_total_us += timing.h3_us;
      report.warm_h3_max_us = std::max(report.warm_h3_max_us, timing.h3_us);
      report.warm_pip_total_us += timing.pip_us;
      report.warm_pip_max_us = std::max(report.warm_pip_max_us, timing.pip_us);
    }
    report.iterations_passed = iter + 1;
  }

  const auto t1 = std::chrono::steady_clock::now();
  report.wall_us =
      static_cast<uint64_t>(std::chrono::duration_cast<std::chrono::microseconds>(t1 - t0).count());

  report.gpu_exec_count_after = pgaccel_gpu_exec_count();

  pgaccel_archive_snapshot snap_after{};
  pgaccel_archive_stats_snapshot(&snap_after);
  report.archive_metallib_after = snap_after.metallib_files;
  report.archive_metalar_after = snap_after.metalar_files;
  report.archive_orphan_after = snap_after.orphan_metallib;

  // Best-effort flush of stderr (the pipe) before we tear down.
  std::fflush(stderr);

  if (write(fd_out, &report, sizeof(report)) != sizeof(report)) {
    // Failed to send report — exit with a marker the parent can spot.
    close(fd_out);
    _exit(82);
  }
  close(fd_out);

  pgaccel_shutdown();
  _exit(report.first_failure_iter >= 0 ? 1 : 0);
}

// ──────────────────────────────────────────────────────────────────────
// Parent: drain `fd` non-blockingly into `into`, splitting on '\n'. Used
// to capture each child's stderr pipe in parallel without deadlocking
// if the child writes more than a single pipe buffer.
// ──────────────────────────────────────────────────────────────────────

static void set_nonblock(int fd) {
  int flags = fcntl(fd, F_GETFL, 0);
  if (flags >= 0)
    fcntl(fd, F_SETFL, flags | O_NONBLOCK);
}

struct ChildIO {
  pid_t pid;
  int worker_index;
  int fd_report;  // parent read end of report pipe
  int fd_stderr;  // parent read end of stderr pipe
  std::string stderr_buf;
  std::vector<std::string> stderr_lines;
  bool report_received = false;
  ChildReport report{};
};

static void drain_pipe(ChildIO& io) {
  char tmp[4096];
  for (;;) {
    ssize_t n = read(io.fd_stderr, tmp, sizeof(tmp));
    if (n > 0) {
      io.stderr_buf.append(tmp, static_cast<size_t>(n));
      size_t pos;
      while ((pos = io.stderr_buf.find('\n')) != std::string::npos) {
        io.stderr_lines.emplace_back(io.stderr_buf.substr(0, pos));
        io.stderr_buf.erase(0, pos + 1);
      }
    } else if (n == 0) {
      // EOF — child closed stderr.
      if (!io.stderr_buf.empty()) {
        io.stderr_lines.emplace_back(io.stderr_buf);
        io.stderr_buf.clear();
      }
      return;
    } else {
      if (errno == EAGAIN || errno == EWOULDBLOCK)
        return;
      if (errno == EINTR)
        continue;
      return;
    }
  }
}

static void try_read_report(ChildIO& io) {
  if (io.report_received)
    return;
  ssize_t n = read(io.fd_report, &io.report, sizeof(io.report));
  if (n == sizeof(io.report)) {
    io.report_received = true;
  }
}

int main() {
  std::printf("=== Metal MTLBinaryArchive fork stress test ===\n");
  std::printf("workers=%d iterations_per_worker=%d total_dispatches=%d\n", N_WORKERS, N_ITERATIONS,
              N_WORKERS * N_ITERATIONS * 3);

  char cache_buf[512] = {};
  pgaccel_status cd_st = pgaccel_archive_jit_cache_dir(cache_buf, sizeof(cache_buf));
  if (cd_st == PGACCEL_OK) {
    std::printf("jit_cache_dir=%s\n", cache_buf);
  } else {
    std::printf("jit_cache_dir=<unresolved>\n");
  }

  pgaccel_archive_snapshot snap_start{};
  pgaccel_archive_stats_snapshot(&snap_start);
  std::vector<std::string> pre_metalar_ids = list_cache_ids(cache_buf, ".metalar");
  std::vector<std::string> pre_metallib_ids = list_cache_ids(cache_buf, ".metallib");
  std::vector<std::string> pre_jit_ids = list_cache_ids(cache_buf, ".jit");
  std::printf(
      "pre-fork archive cache: metallib=%llu metalar=%llu jit=%llu orphan=%llu\n",
      (unsigned long long)snap_start.metallib_files, (unsigned long long)snap_start.metalar_files,
      (unsigned long long)snap_start.jit_files, (unsigned long long)snap_start.orphan_metallib);
  std::printf("pre-fork cache ids: metallib=%s\n", join_ids(pre_metallib_ids).c_str());
  std::printf("pre-fork cache ids: metalar=%s\n", join_ids(pre_metalar_ids).c_str());
  std::printf("pre-fork cache ids: jit=%s\n", join_ids(pre_jit_ids).c_str());
  std::fflush(stdout);

  // Important: do NOT call pgaccel_init() in the parent. We are
  // simulating the postmaster pattern (no Metal init pre-fork) so that
  // each child cold-starts via the archive cache.

  std::vector<ChildIO> kids(N_WORKERS);
  for (int i = 0; i < N_WORKERS; ++i) {
    int report_pipe[2];
    int stderr_pipe[2];
    if (pipe(report_pipe) < 0 || pipe(stderr_pipe) < 0) {
      std::perror("pipe");
      return 2;
    }

    pid_t pid = fork();
    if (pid < 0) {
      std::perror("fork");
      return 2;
    }
    if (pid == 0) {
      // Child: close read ends.
      close(report_pipe[0]);
      close(stderr_pipe[0]);
      run_child(i, report_pipe[1], stderr_pipe[1]);
      // run_child does not return.
    }
    // Parent: close write ends and remember the read ends.
    close(report_pipe[1]);
    close(stderr_pipe[1]);
    kids[i].pid = pid;
    kids[i].worker_index = i;
    kids[i].fd_report = report_pipe[0];
    kids[i].fd_stderr = stderr_pipe[0];
    set_nonblock(kids[i].fd_stderr);
  }

  // Drain all child stderr pipes concurrently and collect their reports.
  // Poll until every child has exited and its pipes are drained.
  std::vector<pollfd> pfds;
  pfds.reserve(N_WORKERS * 2);
  for (auto& k : kids) {
    pfds.push_back({k.fd_stderr, POLLIN, 0});
    pfds.push_back({k.fd_report, POLLIN, 0});
  }

  int kids_remaining = N_WORKERS;
  std::vector<bool> stderr_open(N_WORKERS, true);

  while (kids_remaining > 0) {
    int rc = poll(pfds.data(), pfds.size(), 30 * 1000);
    if (rc < 0) {
      if (errno == EINTR)
        continue;
      std::perror("poll");
      break;
    }
    if (rc == 0) {
      // 30 s with no activity — bail out, but try to reap pending children
      // so we don't leave zombies.
      std::fprintf(stderr, "test_fork_archive_stress: poll timeout, "
                           "draining and exiting\n");
      break;
    }
    for (int i = 0; i < N_WORKERS; ++i) {
      pollfd& pf_err = pfds[i * 2];
      pollfd& pf_rep = pfds[i * 2 + 1];
      if (pf_err.revents & (POLLIN | POLLHUP)) {
        drain_pipe(kids[i]);
        if (pf_err.revents & POLLHUP) {
          // Child closed its stderr fd; stop polling on this side.
          if (stderr_open[i]) {
            // One final drain after HUP catches buffered data.
            drain_pipe(kids[i]);
            stderr_open[i] = false;
            pf_err.fd = -1;
          }
        }
      }
      if (pf_rep.revents & POLLIN) {
        try_read_report(kids[i]);
        if (kids[i].report_received) {
          pf_rep.fd = -1;
        }
      }
    }
    // Check children that have all pipes done.
    int all_done = 0;
    for (int i = 0; i < N_WORKERS; ++i) {
      if (pfds[i * 2].fd < 0 && pfds[i * 2 + 1].fd < 0) {
        ++all_done;
      }
    }
    if (all_done == N_WORKERS)
      break;
  }

  // Reap all children.
  std::vector<int> exit_status(N_WORKERS, -1);
  std::vector<int> term_signal(N_WORKERS, 0);
  for (int i = 0; i < N_WORKERS; ++i) {
    int wstatus = 0;
    if (waitpid(kids[i].pid, &wstatus, 0) < 0) {
      std::perror("waitpid");
      continue;
    }
    if (WIFEXITED(wstatus)) {
      exit_status[i] = WEXITSTATUS(wstatus);
    } else if (WIFSIGNALED(wstatus)) {
      term_signal[i] = WTERMSIG(wstatus);
    }
    close(kids[i].fd_report);
    close(kids[i].fd_stderr);
  }
  (void)kids_remaining;

  pgaccel_archive_snapshot snap_end{};
  pgaccel_archive_stats_snapshot(&snap_end);
  std::vector<std::string> post_metalar_ids = list_cache_ids(cache_buf, ".metalar");
  std::vector<std::string> post_metallib_ids = list_cache_ids(cache_buf, ".metallib");
  std::vector<std::string> post_jit_ids = list_cache_ids(cache_buf, ".jit");
  const bool started_empty = snap_start.metallib_files == 0 && snap_start.metalar_files == 0 &&
                             snap_start.jit_files == 0;
  std::printf("\npost-fork archive cache: metallib=%llu metalar=%llu jit=%llu "
              "orphan=%llu (delta_metallib=%lld delta_metalar=%lld)\n",
              (unsigned long long)snap_end.metallib_files,
              (unsigned long long)snap_end.metalar_files, (unsigned long long)snap_end.jit_files,
              (unsigned long long)snap_end.orphan_metallib,
              (long long)snap_end.metallib_files - (long long)snap_start.metallib_files,
              (long long)snap_end.metalar_files - (long long)snap_start.metalar_files);
  std::printf("post-fork cache ids: metallib=%s\n", join_ids(post_metallib_ids).c_str());
  std::printf("post-fork cache ids: metalar=%s\n", join_ids(post_metalar_ids).c_str());
  std::printf("post-fork cache ids: jit=%s\n", join_ids(post_jit_ids).c_str());

  // ── Per-child report ──
  std::printf("\n%-3s %-7s %-4s %-4s %-4s %-12s %-12s %-12s %-12s\n", "idx", "pid", "init", "ok",
              "fail", "gpu_exec", "metallib", "metalar", "orphans");
  int total_ok = 0;
  int total_xpc = 0;
  int total_pipeline_fail = 0;
  int total_load_fail = 0;
  int total_build_fail = 0;
  int total_skipped = 0;
  int total_spawn_fail = 0;
  int crashed = 0;
  int reports_missing = 0;

  for (int i = 0; i < N_WORKERS; ++i) {
    auto& k = kids[i];
    if (!k.report_received)
      ++reports_missing;
    const ChildReport& r = k.report;
    std::printf(
        "%-3d %-7d %-4d %-4d %-4d %-12llu %llu→%llu     %llu→%llu     %llu→%llu\n", i, (int)k.pid,
        r.pgaccel_init_status, r.iterations_passed, r.iterations_attempted - r.iterations_passed,
        (unsigned long long)(r.gpu_exec_count_after - r.gpu_exec_count_before),
        (unsigned long long)r.archive_metallib_before, (unsigned long long)r.archive_metallib_after,
        (unsigned long long)r.archive_metalar_before, (unsigned long long)r.archive_metalar_after,
        (unsigned long long)r.archive_orphan_before, (unsigned long long)r.archive_orphan_after);
    if (k.report_received && r.iterations_passed == N_ITERATIONS)
      ++total_ok;
    if (term_signal[i] != 0)
      ++crashed;
  }

  std::printf("\nexit_status:");
  for (int i = 0; i < N_WORKERS; ++i)
    std::printf(" %d", exit_status[i]);
  std::printf("\nterm_signal:");
  for (int i = 0; i < N_WORKERS; ++i)
    std::printf(" %d", term_signal[i]);
  std::printf("\n");

  uint64_t max_first_reduce_us = 0;
  uint64_t max_first_iteration_us = 0;
  uint64_t max_steady_after_first_us = 0;
  uint64_t max_init_us = 0;
  std::printf("\n=== Per-child first-iteration timings (ms) ===\n");
  std::printf("%-3s %-8s %-10s %-10s %-10s %-10s %-10s %-10s\n", "idx", "init",
              "first_all", "first_sum", "first_h3", "first_pip", "steady_avg", "loop_wall");
  for (int i = 0; i < N_WORKERS; ++i) {
    const ChildReport& r = kids[i].report;
    max_init_us = std::max(max_init_us, r.init_us);
    max_first_reduce_us = std::max(max_first_reduce_us, r.first_reduce_us);
    max_first_iteration_us = std::max(max_first_iteration_us, r.first_iteration_us);
    uint64_t steady_after_first_us = 0;
    if (r.iterations_passed > 1 && r.wall_us > r.first_iteration_us) {
      steady_after_first_us =
          (r.wall_us - r.first_iteration_us) / static_cast<uint64_t>(r.iterations_passed - 1);
      max_steady_after_first_us = std::max(max_steady_after_first_us, steady_after_first_us);
    }
    std::printf("%-3d %-8.2f %-10.2f %-10.2f %-10.2f %-10.2f %-10.2f %-10.2f\n", i,
                us_to_ms(r.init_us), us_to_ms(r.first_iteration_us),
                us_to_ms(r.first_reduce_us), us_to_ms(r.first_h3_us),
                us_to_ms(r.first_pip_us), us_to_ms(steady_after_first_us),
                us_to_ms(r.wall_us));
    std::printf(
        "latency_record_us worker=%d init_us=%llu cold_iteration_us=%llu cold_reduce_us=%llu "
        "cold_h3_us=%llu cold_pip_us=%llu warm_iterations=%llu "
        "warm_iteration_total_us=%llu warm_iteration_max_us=%llu warm_reduce_total_us=%llu "
        "warm_reduce_max_us=%llu warm_h3_total_us=%llu warm_h3_max_us=%llu "
        "warm_pip_total_us=%llu warm_pip_max_us=%llu wall_us=%llu\n",
        i, (unsigned long long)r.init_us, (unsigned long long)r.first_iteration_us,
        (unsigned long long)r.first_reduce_us, (unsigned long long)r.first_h3_us,
        (unsigned long long)r.first_pip_us, (unsigned long long)r.warm_iterations,
        (unsigned long long)r.warm_iteration_total_us, (unsigned long long)r.warm_iteration_max_us,
        (unsigned long long)r.warm_reduce_total_us, (unsigned long long)r.warm_reduce_max_us,
        (unsigned long long)r.warm_h3_total_us, (unsigned long long)r.warm_h3_max_us,
        (unsigned long long)r.warm_pip_total_us, (unsigned long long)r.warm_pip_max_us,
        (unsigned long long)r.wall_us);
  }
  const char* first_dispatch_status =
      max_first_reduce_us <= static_cast<uint64_t>(FIRST_DISPATCH_BUDGET_US)
          ? "within_budget"
          : (started_empty ? "above_budget_cold_cache_compile"
                           : "above_budget_cached_code_object_pipeline");
  std::printf("first_dispatch_budget_us=%d max_first_sum_us=%llu status=%s\n",
              FIRST_DISPATCH_BUDGET_US, (unsigned long long)max_first_reduce_us,
              first_dispatch_status);
  std::printf("max_first_iteration_us=%llu max_steady_after_first_us=%llu max_init_us=%llu\n",
              (unsigned long long)max_first_iteration_us,
              (unsigned long long)max_steady_after_first_us, (unsigned long long)max_init_us);

  // ── Stderr marker scan ──
  std::printf("\n=== stderr marker scan (per-worker) ===\n");
  for (int i = 0; i < N_WORKERS; ++i) {
    MarkerSet m;
    for (const auto& line : kids[i].stderr_lines)
      scan_marker(m, line);
    total_xpc += static_cast<int>(m.xpc_compiler_service.size());
    total_pipeline_fail += static_cast<int>(m.pipeline_state_fail.size());
    total_load_fail += static_cast<int>(m.archive_load_fail.size());
    total_build_fail += static_cast<int>(m.archive_build_fail.size());
    total_skipped += static_cast<int>(m.archive_skipped.size());
    total_spawn_fail += static_cast<int>(m.posix_spawn_fail.size());

    if (total_failures(m) == 0 && m.archive_skipped.empty()) {
      std::printf("[worker %d] clean (no archive failure markers; %zu stderr lines)\n", i,
                  kids[i].stderr_lines.size());
    } else {
      std::printf("[worker %d] xpc=%zu pipeline_fail=%zu archive_load_fail=%zu "
                  "archive_build_fail=%zu skipped=%zu posix_spawn=%zu\n",
                  i, m.xpc_compiler_service.size(), m.pipeline_state_fail.size(),
                  m.archive_load_fail.size(), m.archive_build_fail.size(), m.archive_skipped.size(),
                  m.posix_spawn_fail.size());
      auto dump = [&](const char* tag, const std::vector<std::string>& v) {
        for (const auto& s : v)
          std::printf("    %s: %s\n", tag, s.c_str());
      };
      dump("XPC", m.xpc_compiler_service);
      dump("PIPELINE", m.pipeline_state_fail);
      dump("ARCHIVE_LOAD", m.archive_load_fail);
      dump("ARCHIVE_BUILD", m.archive_build_fail);
      dump("ARCHIVE_SKIP", m.archive_skipped);
      dump("SPAWN", m.posix_spawn_fail);

      // Dump the most recent stderr lines surrounding the failure so
      // reviewers can see what AdaptiveCpp emitted just before the XPC
      // error. Limit to 15 lines to keep the per-worker block readable.
      std::printf("  --- last %zu stderr lines from worker %d ---\n",
                  std::min<size_t>(kids[i].stderr_lines.size(), 15u), i);
      size_t start = (kids[i].stderr_lines.size() > 15u) ? kids[i].stderr_lines.size() - 15u : 0u;
      for (size_t j = start; j < kids[i].stderr_lines.size(); ++j) {
        std::printf("  | %s\n", kids[i].stderr_lines[j].c_str());
      }
    }
  }

  std::printf("\n=== Totals ===\n");
  std::printf("workers_succeeded=%d / %d\n", total_ok, N_WORKERS);
  std::printf("workers_crashed=%d\n", crashed);
  std::printf("reports_missing=%d\n", reports_missing);
  std::printf("xpc_compiler_service_hits=%d\n", total_xpc);
  std::printf("pipeline_state_failures=%d\n", total_pipeline_fail);
  std::printf("archive_load_failures=%d\n", total_load_fail);
  std::printf("archive_build_failures=%d\n", total_build_fail);
  std::printf("archive_skipped_intentional=%d (not a failure — large metallibs)\n", total_skipped);
  std::printf("posix_spawn_failures=%d\n", total_spawn_fail);

  const bool empty_cache_counts_exact =
      post_metallib_ids.size() == EXPECTED_EMPTY_CACHE_IDS &&
      post_metalar_ids.size() == EXPECTED_EMPTY_CACHE_IDS &&
      post_jit_ids.size() == EXPECTED_EMPTY_CACHE_IDS;
  const bool metallib_metalar_ids_equal = post_metallib_ids == post_metalar_ids;
  const bool metallib_jit_ids_equal = post_metallib_ids == post_jit_ids;
  const bool cache_id_sets_identical = metallib_metalar_ids_equal && metallib_jit_ids_equal;
  const int hash_instability_failures =
      started_empty && (!empty_cache_counts_exact || !cache_id_sets_identical) ? 1 : 0;
  const char* cache_hash_status =
      !started_empty ? "not_enforced"
                     : (hash_instability_failures == 0 ? "pass" : "fail");
  std::printf("cache_hash_unique_ids: metallib=%zu metalar=%zu jit=%zu "
              "expected_empty=%zu started_empty=%d counts_exact=%d\n",
              post_metallib_ids.size(), post_metalar_ids.size(), post_jit_ids.size(),
              EXPECTED_EMPTY_CACHE_IDS, started_empty ? 1 : 0,
              empty_cache_counts_exact ? 1 : 0);
  std::printf("cache_hash_id_sets: metallib_metalar_equal=%d metallib_jit_equal=%d all_equal=%d\n",
              metallib_metalar_ids_equal ? 1 : 0, metallib_jit_ids_equal ? 1 : 0,
              cache_id_sets_identical ? 1 : 0);
  std::printf("cache_hash_contract: mode=%s enforced=%d expected_ids_per_extension=%zu "
              "counts_exact=%d sets_identical=%d status=%s\n",
              started_empty ? "empty" : "nonempty_diagnostic", started_empty ? 1 : 0,
              EXPECTED_EMPTY_CACHE_IDS, empty_cache_counts_exact ? 1 : 0,
              cache_id_sets_identical ? 1 : 0, cache_hash_status);
  std::printf("cache_hash_ids_changed: metallib=%d metalar=%d jit=%d\n",
              pre_metallib_ids == post_metallib_ids ? 0 : 1,
              pre_metalar_ids == post_metalar_ids ? 0 : 1, pre_jit_ids == post_jit_ids ? 0 : 1);
  std::printf("cache_hash_instability_failures=%d\n", hash_instability_failures);

  // Acceptance gate: zero XPC errors AND every worker completed every
  // iteration AND no abnormal terminations.
  const int hard_failures =
      total_xpc + total_pipeline_fail + total_load_fail + total_build_fail + total_spawn_fail +
      hash_instability_failures;
  if (hard_failures == 0 && crashed == 0 && reports_missing == 0 && total_ok == N_WORKERS) {
    std::printf("\nRESULT: PASS — %d × %d fork stress with zero MTLCompilerService "
                "XPC errors.\n",
                N_WORKERS, N_ITERATIONS);
    return 0;
  }
  std::printf("\nRESULT: FAIL — see per-worker dump above for the trigger.\n");
  return 1;
}
