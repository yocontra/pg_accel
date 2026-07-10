// Synchronize forked children immediately before cold GPU initialization and
// immediately before their first GPU dispatch. The parent deliberately never
// calls a pgaccel API, matching a PostgreSQL postmaster that loads the extension
// but leaves Metal/SYCL initialization to its backends.

#include <fcntl.h>
#include <limits.h>
#include <poll.h>
#include <signal.h>
#include <sys/wait.h>
#include <unistd.h>

#include <algorithm>
#include <array>
#include <cerrno>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <exception>
#include <type_traits>
#include <vector>

#include "pgaccel_ffi.h"

namespace {

constexpr uint32_t kMessageMagic = 0x50474246;  // "PGBF"
constexpr size_t kReduceCount = 100'000;
constexpr float kExpectedSum = static_cast<float>(kReduceCount);
constexpr float kSumTolerance = 1.0f;

enum class Stage : uint32_t {
  kSpawned = 0,
  kReadyInit = 1,
  kInitStarted = 2,
  kInitSucceeded = 3,
  kReadyDispatch = 4,
  kDispatchStarted = 5,
  kDispatchSucceeded = 6,
  kShutdownStarted = 7,
  kComplete = 8,
  kFailure = 255,
};

enum class Failure : int32_t {
  kNone = 0,
  kHostSetup = 1,
  kInitGateClosed = 2,
  kInitFailed = 3,
  kNoGpuDevice = 4,
  kDispatchGateClosed = 5,
  kDispatchFailed = 6,
  kWrongResult = 7,
  kNoGpuExecution = 8,
  kShutdownFailed = 9,
  kUnexpectedException = 10,
};

struct ChildMessage {
  uint32_t magic = kMessageMagic;
  uint32_t stage = static_cast<uint32_t>(Stage::kSpawned);
  uint32_t failure_at = static_cast<uint32_t>(Stage::kSpawned);
  int32_t failure = static_cast<int32_t>(Failure::kNone);
  int32_t worker_index = -1;
  int32_t round_index = -1;
  int32_t pid = -1;
  int32_t init_status = PGACCEL_ERROR;
  int32_t dispatch_status = PGACCEL_ERROR;
  int32_t shutdown_status = PGACCEL_ERROR;
  uint32_t compute_units = 0;
  uint32_t reserved = 0;
  uint64_t init_us = 0;
  uint64_t dispatch_us = 0;
  uint64_t gpu_before = 0;
  uint64_t gpu_after = 0;
  float sum = 0.0f;
  float expected_sum = kExpectedSum;
};

static_assert(std::is_trivially_copyable<ChildMessage>::value, "child reports must be pipe-safe");
static_assert(sizeof(ChildMessage) <= PIPE_BUF, "child reports must be atomic pipe writes");

struct WorkerPipes {
  int report[2] = {-1, -1};
  int init_gate[2] = {-1, -1};
  int dispatch_gate[2] = {-1, -1};
};

struct ChildState {
  pid_t pid = -1;
  int worker_index = -1;
  int round_index = -1;
  int report_fd = -1;
  int init_gate_fd = -1;
  int dispatch_gate_fd = -1;
  Stage last_stage = Stage::kSpawned;
  ChildMessage last_message{};
  ChildMessage failure_message{};
  std::array<unsigned char, sizeof(ChildMessage)> pending{};
  size_t pending_size = 0;
  bool report_eof = false;
  bool failed = false;
  bool protocol_error = false;
  bool exited = false;
  int wait_status = 0;
};

int parse_env_int(const char* name, int fallback, int max_value) {
  const char* value = std::getenv(name);
  if (value == nullptr || value[0] == '\0')
    return fallback;

  errno = 0;
  char* end = nullptr;
  const long parsed = std::strtol(value, &end, 10);
  if (errno != 0 || end == value || *end != '\0' || parsed < 1 || parsed > max_value) {
    std::fprintf(stderr, "%s=%s is invalid; using %d\n", name, value, fallback);
    return fallback;
  }
  return static_cast<int>(parsed);
}

const char* stage_name(Stage stage) {
  switch (stage) {
    case Stage::kSpawned:
      return "spawned";
    case Stage::kReadyInit:
      return "ready-init";
    case Stage::kInitStarted:
      return "init-started";
    case Stage::kInitSucceeded:
      return "init-succeeded";
    case Stage::kReadyDispatch:
      return "ready-dispatch";
    case Stage::kDispatchStarted:
      return "dispatch-started";
    case Stage::kDispatchSucceeded:
      return "dispatch-succeeded";
    case Stage::kShutdownStarted:
      return "shutdown-started";
    case Stage::kComplete:
      return "complete";
    case Stage::kFailure:
      return "failure";
  }
  return "invalid";
}

const char* failure_name(Failure failure) {
  switch (failure) {
    case Failure::kNone:
      return "none";
    case Failure::kHostSetup:
      return "host-setup";
    case Failure::kInitGateClosed:
      return "init-gate-closed";
    case Failure::kInitFailed:
      return "init-failed";
    case Failure::kNoGpuDevice:
      return "no-gpu-device";
    case Failure::kDispatchGateClosed:
      return "dispatch-gate-closed";
    case Failure::kDispatchFailed:
      return "dispatch-failed";
    case Failure::kWrongResult:
      return "wrong-result";
    case Failure::kNoGpuExecution:
      return "no-gpu-execution";
    case Failure::kShutdownFailed:
      return "shutdown-failed";
    case Failure::kUnexpectedException:
      return "unexpected-exception";
  }
  return "invalid";
}

uint64_t elapsed_us(std::chrono::steady_clock::time_point start,
                    std::chrono::steady_clock::time_point end) {
  return static_cast<uint64_t>(
      std::chrono::duration_cast<std::chrono::microseconds>(end - start).count());
}

void close_fd(int& fd) {
  if (fd >= 0) {
    (void)close(fd);
    fd = -1;
  }
}

void close_pipe_set(WorkerPipes& pipes) {
  close_fd(pipes.report[0]);
  close_fd(pipes.report[1]);
  close_fd(pipes.init_gate[0]);
  close_fd(pipes.init_gate[1]);
  close_fd(pipes.dispatch_gate[0]);
  close_fd(pipes.dispatch_gate[1]);
}

void close_pipe_sets(std::vector<WorkerPipes>& pipes) {
  for (auto& worker : pipes)
    close_pipe_set(worker);
}

bool write_all(int fd, const void* data, size_t size) {
  const auto* bytes = static_cast<const unsigned char*>(data);
  size_t written = 0;
  while (written < size) {
    const ssize_t result = write(fd, bytes + written, size - written);
    if (result > 0) {
      written += static_cast<size_t>(result);
      continue;
    }
    if (result < 0 && errno == EINTR)
      continue;
    return false;
  }
  return true;
}

bool read_gate_token(int fd) {
  unsigned char token = 0;
  for (;;) {
    const ssize_t result = read(fd, &token, sizeof(token));
    if (result == sizeof(token))
      return token == 1;
    if (result < 0 && errno == EINTR)
      continue;
    return false;
  }
}

bool emit_message(int fd, ChildMessage& message, Stage stage) {
  message.stage = static_cast<uint32_t>(stage);
  return write_all(fd, &message, sizeof(message));
}

[[noreturn]] void child_failure(int report_fd, ChildMessage& message, Stage failure_at,
                                Failure failure, int exit_code) {
  message.failure_at = static_cast<uint32_t>(failure_at);
  message.failure = static_cast<int32_t>(failure);
  (void)emit_message(report_fd, message, Stage::kFailure);
  close_fd(report_fd);
  _exit(exit_code);
}

void close_unrelated_child_fds(std::vector<WorkerPipes>& pipes, int worker_index) {
  for (int i = 0; i < static_cast<int>(pipes.size()); ++i) {
    if (i == worker_index) {
      close_fd(pipes[i].report[0]);
      close_fd(pipes[i].init_gate[1]);
      close_fd(pipes[i].dispatch_gate[1]);
    } else {
      close_pipe_set(pipes[i]);
    }
  }
}

[[noreturn]] void run_child(int worker_index, int round_index, int report_fd, int init_gate_fd,
                            int dispatch_gate_fd) {
  ChildMessage message;
  message.worker_index = worker_index;
  message.round_index = round_index;
  message.pid = static_cast<int32_t>(getpid());

  try {
    // Complete all host-only setup before announcing the first barrier. After
    // the gate opens, the next pgaccel operation is pgaccel_init().
    std::vector<float> input(kReduceCount, 1.0f);

    if (!emit_message(report_fd, message, Stage::kReadyInit))
      _exit(70);
    if (!read_gate_token(init_gate_fd)) {
      child_failure(report_fd, message, Stage::kReadyInit, Failure::kInitGateClosed, 71);
    }
    close_fd(init_gate_fd);

    if (!emit_message(report_fd, message, Stage::kInitStarted))
      _exit(70);
    const auto init_start = std::chrono::steady_clock::now();
    const pgaccel_status init_status = pgaccel_init();
    const auto init_end = std::chrono::steady_clock::now();
    message.init_us = elapsed_us(init_start, init_end);
    message.init_status = static_cast<int32_t>(init_status);
    if (init_status != PGACCEL_OK) {
      child_failure(report_fd, message, Stage::kInitStarted, Failure::kInitFailed, 72);
    }

    const pgaccel_device_info device = pgaccel_get_device_info();
    message.compute_units = device.compute_units;
    if (device.compute_units == 0) {
      child_failure(report_fd, message, Stage::kInitSucceeded, Failure::kNoGpuDevice, 73);
    }
    if (!emit_message(report_fd, message, Stage::kInitSucceeded))
      _exit(70);

    pgaccel_reset_gpu_exec_count();
    message.gpu_before = pgaccel_gpu_exec_count();

    // No GPU command is submitted between this barrier and the reduction.
    if (!emit_message(report_fd, message, Stage::kReadyDispatch))
      _exit(70);
    if (!read_gate_token(dispatch_gate_fd)) {
      child_failure(report_fd, message, Stage::kReadyDispatch, Failure::kDispatchGateClosed, 74);
    }
    close_fd(dispatch_gate_fd);

    if (!emit_message(report_fd, message, Stage::kDispatchStarted))
      _exit(70);
    const auto dispatch_start = std::chrono::steady_clock::now();
    const pgaccel_status dispatch_status =
        pgaccel_reduce_sum_f32(input.data(), input.size(), &message.sum);
    const auto dispatch_end = std::chrono::steady_clock::now();
    message.dispatch_us = elapsed_us(dispatch_start, dispatch_end);
    message.dispatch_status = static_cast<int32_t>(dispatch_status);
    message.gpu_after = pgaccel_gpu_exec_count();

    if (dispatch_status != PGACCEL_OK) {
      child_failure(report_fd, message, Stage::kDispatchStarted, Failure::kDispatchFailed, 75);
    }
    if (std::fabs(message.sum - message.expected_sum) > kSumTolerance) {
      child_failure(report_fd, message, Stage::kDispatchStarted, Failure::kWrongResult, 76);
    }
    if (message.gpu_after <= message.gpu_before) {
      child_failure(report_fd, message, Stage::kDispatchStarted, Failure::kNoGpuExecution, 77);
    }
    if (!emit_message(report_fd, message, Stage::kDispatchSucceeded))
      _exit(70);

    if (!emit_message(report_fd, message, Stage::kShutdownStarted))
      _exit(70);
    const pgaccel_status shutdown_status = pgaccel_shutdown();
    message.shutdown_status = static_cast<int32_t>(shutdown_status);
    if (shutdown_status != PGACCEL_OK) {
      child_failure(report_fd, message, Stage::kShutdownStarted, Failure::kShutdownFailed, 78);
    }
    if (!emit_message(report_fd, message, Stage::kComplete))
      _exit(70);

    close_fd(report_fd);
    _exit(0);
  } catch (const std::exception& error) {
    std::fprintf(stderr, "fork-boundary child %d: unexpected exception: %s\n", worker_index,
                 error.what());
    const auto failure_at = static_cast<Stage>(message.stage);
    const Failure failure =
        failure_at == Stage::kSpawned ? Failure::kHostSetup : Failure::kUnexpectedException;
    child_failure(report_fd, message, failure_at, failure, 79);
  } catch (...) {
    std::fprintf(stderr, "fork-boundary child %d: unexpected non-standard exception\n",
                 worker_index);
    const auto failure_at = static_cast<Stage>(message.stage);
    const Failure failure =
        failure_at == Stage::kSpawned ? Failure::kHostSetup : Failure::kUnexpectedException;
    child_failure(report_fd, message, failure_at, failure, 79);
  }
}

bool set_nonblocking(int fd) {
  const int flags = fcntl(fd, F_GETFL, 0);
  return flags >= 0 && fcntl(fd, F_SETFL, flags | O_NONBLOCK) == 0;
}

bool valid_progress_stage(uint32_t raw_stage) {
  return raw_stage >= static_cast<uint32_t>(Stage::kReadyInit) &&
         raw_stage <= static_cast<uint32_t>(Stage::kComplete);
}

void accept_message(ChildState& child, const ChildMessage& message) {
  if (message.magic != kMessageMagic || message.worker_index != child.worker_index ||
      message.round_index != child.round_index || message.pid != static_cast<int32_t>(child.pid)) {
    child.protocol_error = true;
    return;
  }

  if (message.stage == static_cast<uint32_t>(Stage::kFailure)) {
    child.failed = true;
    child.failure_message = message;
    return;
  }

  if (!valid_progress_stage(message.stage) ||
      message.stage != static_cast<uint32_t>(child.last_stage) + 1) {
    child.protocol_error = true;
    return;
  }

  child.last_stage = static_cast<Stage>(message.stage);
  child.last_message = message;
}

void drain_report(ChildState& child) {
  if (child.report_fd < 0)
    return;

  for (;;) {
    const size_t remaining = child.pending.size() - child.pending_size;
    const ssize_t result =
        read(child.report_fd, child.pending.data() + child.pending_size, remaining);
    if (result > 0) {
      child.pending_size += static_cast<size_t>(result);
      if (child.pending_size == child.pending.size()) {
        ChildMessage message;
        std::memcpy(&message, child.pending.data(), sizeof(message));
        child.pending_size = 0;
        accept_message(child, message);
      }
      continue;
    }
    if (result == 0) {
      if (child.pending_size != 0)
        child.protocol_error = true;
      child.report_eof = true;
      close_fd(child.report_fd);
      return;
    }
    if (errno == EINTR)
      continue;
    if (errno == EAGAIN || errno == EWOULDBLOCK)
      return;
    child.protocol_error = true;
    close_fd(child.report_fd);
    return;
  }
}

void drain_reports(std::vector<ChildState>& children) {
  for (auto& child : children)
    drain_report(child);
}

void reap_nonblocking(std::vector<ChildState>& children) {
  for (auto& child : children) {
    if (child.exited || child.pid <= 0)
      continue;

    int status = 0;
    pid_t result;
    do {
      result = waitpid(child.pid, &status, WNOHANG);
    } while (result < 0 && errno == EINTR);

    if (result == child.pid) {
      child.exited = true;
      child.wait_status = status;
    } else if (result < 0) {
      child.protocol_error = true;
    }
  }
}

bool poll_reports(std::vector<ChildState>& children, int timeout_ms) {
  std::vector<pollfd> descriptors;
  std::vector<size_t> child_indexes;
  descriptors.reserve(children.size());
  child_indexes.reserve(children.size());

  for (size_t i = 0; i < children.size(); ++i) {
    if (children[i].report_fd >= 0) {
      descriptors.push_back({children[i].report_fd, POLLIN | POLLHUP, 0});
      child_indexes.push_back(i);
    }
  }

  int result;
  do {
    result =
        poll(descriptors.empty() ? nullptr : descriptors.data(), descriptors.size(), timeout_ms);
  } while (result < 0 && errno == EINTR);
  if (result < 0) {
    std::perror("poll");
    return false;
  }

  for (size_t i = 0; i < descriptors.size(); ++i) {
    if ((descriptors[i].revents & (POLLIN | POLLHUP)) != 0)
      drain_report(children[child_indexes[i]]);
    if ((descriptors[i].revents & (POLLERR | POLLNVAL)) != 0)
      children[child_indexes[i]].protocol_error = true;
  }
  return true;
}

bool stage_reached(const ChildState& child, Stage target) {
  return !child.failed && !child.protocol_error &&
         static_cast<uint32_t>(child.last_stage) >= static_cast<uint32_t>(target);
}

bool all_reached(const std::vector<ChildState>& children, Stage target) {
  return std::all_of(children.begin(), children.end(),
                     [target](const ChildState& child) { return stage_reached(child, target); });
}

bool wait_for_stage(std::vector<ChildState>& children, Stage target, int timeout_seconds,
                    const char* label) {
  const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(timeout_seconds);

  for (;;) {
    drain_reports(children);
    reap_nonblocking(children);

    if (all_reached(children, target))
      return true;

    for (const auto& child : children) {
      if (child.failed || child.protocol_error || (child.exited && !stage_reached(child, target))) {
        std::fprintf(stderr, "%s failed before all children reached %s\n", label,
                     stage_name(target));
        return false;
      }
    }

    const auto now = std::chrono::steady_clock::now();
    if (now >= deadline) {
      std::fprintf(stderr, "%s timed out after %d seconds waiting for %s\n", label, timeout_seconds,
                   stage_name(target));
      return false;
    }

    const auto remaining_ms =
        std::chrono::duration_cast<std::chrono::milliseconds>(deadline - now).count();
    const int poll_ms = static_cast<int>(std::min<int64_t>(remaining_ms, 100));
    if (!poll_reports(children, std::max(poll_ms, 1)))
      return false;
  }
}

bool all_exited(const std::vector<ChildState>& children) {
  return std::all_of(children.begin(), children.end(),
                     [](const ChildState& child) { return child.exited; });
}

bool wait_for_exit(std::vector<ChildState>& children, int timeout_seconds, const char* label) {
  const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(timeout_seconds);
  while (!all_exited(children)) {
    if (!poll_reports(children, 20))
      return false;
    reap_nonblocking(children);
    if (std::chrono::steady_clock::now() >= deadline) {
      std::fprintf(stderr, "%s timed out after %d seconds waiting for child exit\n", label,
                   timeout_seconds);
      return false;
    }
  }
  drain_reports(children);
  return true;
}

bool release_gate(std::vector<ChildState>& children, bool init_gate) {
  const unsigned char token = 1;
  bool ok = true;
  for (auto& child : children) {
    int& fd = init_gate ? child.init_gate_fd : child.dispatch_gate_fd;
    if (fd < 0) {
      std::fprintf(stderr, "worker %d: %s gate was already closed\n", child.worker_index,
                   init_gate ? "init" : "dispatch");
      ok = false;
    } else if (!write_all(fd, &token, sizeof(token))) {
      std::fprintf(stderr, "worker %d: failed to release %s gate: %s\n", child.worker_index,
                   init_gate ? "init" : "dispatch", std::strerror(errno));
      ok = false;
    }
    close_fd(fd);
  }
  return ok;
}

void print_child_diagnostic(const ChildState& child) {
  std::fprintf(stderr, "  worker=%d pid=%d last_stage=%s", child.worker_index,
               static_cast<int>(child.pid), stage_name(child.last_stage));
  if (child.failed) {
    const auto failure = static_cast<Failure>(child.failure_message.failure);
    const auto failure_at = static_cast<Stage>(child.failure_message.failure_at);
    std::fprintf(stderr, " failure=%s failure_at=%s init=%d dispatch=%d sum=%.1f gpu=%llu->%llu",
                 failure_name(failure), stage_name(failure_at), child.failure_message.init_status,
                 child.failure_message.dispatch_status, child.failure_message.sum,
                 static_cast<unsigned long long>(child.failure_message.gpu_before),
                 static_cast<unsigned long long>(child.failure_message.gpu_after));
  }
  if (child.protocol_error)
    std::fprintf(stderr, " protocol_error=1");
  if (!child.exited) {
    std::fprintf(stderr, " exit=running");
  } else if (WIFSIGNALED(child.wait_status)) {
    std::fprintf(stderr, " exit=signal(%d)", WTERMSIG(child.wait_status));
  } else if (WIFEXITED(child.wait_status)) {
    std::fprintf(stderr, " exit=code(%d)", WEXITSTATUS(child.wait_status));
  } else {
    std::fprintf(stderr, " exit=unknown(0x%x)", child.wait_status);
  }
  std::fprintf(stderr, "\n");
}

void print_round_diagnostics(const std::vector<ChildState>& children) {
  for (const auto& child : children)
    print_child_diagnostic(child);
}

void close_parent_fds(std::vector<ChildState>& children) {
  for (auto& child : children) {
    close_fd(child.report_fd);
    close_fd(child.init_gate_fd);
    close_fd(child.dispatch_gate_fd);
  }
}

void abort_children(std::vector<ChildState>& children) {
  for (auto& child : children) {
    if (!child.exited && child.pid > 0)
      (void)kill(child.pid, SIGTERM);
    close_fd(child.init_gate_fd);
    close_fd(child.dispatch_gate_fd);
  }

  const auto term_deadline = std::chrono::steady_clock::now() + std::chrono::seconds(2);
  while (!all_exited(children) && std::chrono::steady_clock::now() < term_deadline) {
    (void)poll_reports(children, 20);
    reap_nonblocking(children);
  }

  for (auto& child : children) {
    if (!child.exited && child.pid > 0)
      (void)kill(child.pid, SIGKILL);
  }
  for (auto& child : children) {
    if (child.exited || child.pid <= 0)
      continue;
    int status = 0;
    pid_t result;
    do {
      result = waitpid(child.pid, &status, 0);
    } while (result < 0 && errno == EINTR);
    if (result == child.pid) {
      child.exited = true;
      child.wait_status = status;
    }
  }
  drain_reports(children);
  close_parent_fds(children);
}

bool validate_round(const std::vector<ChildState>& children) {
  bool ok = true;
  for (const auto& child : children) {
    const ChildMessage& result = child.last_message;
    const bool exited_cleanly =
        child.exited && WIFEXITED(child.wait_status) && WEXITSTATUS(child.wait_status) == 0;
    const bool correct = child.last_stage == Stage::kComplete && !child.failed &&
                         !child.protocol_error && exited_cleanly &&
                         result.init_status == PGACCEL_OK && result.dispatch_status == PGACCEL_OK &&
                         result.shutdown_status == PGACCEL_OK && result.compute_units > 0 &&
                         std::fabs(result.sum - result.expected_sum) <= kSumTolerance &&
                         result.gpu_after > result.gpu_before;
    if (!correct)
      ok = false;

    const uint64_t gpu_delta =
        result.gpu_after >= result.gpu_before ? result.gpu_after - result.gpu_before : 0;
    std::printf("worker=%d pid=%d init_ms=%.2f dispatch_ms=%.2f sum=%.1f "
                "gpu_delta=%llu exit=%s\n",
                child.worker_index, static_cast<int>(child.pid),
                static_cast<double>(result.init_us) / 1000.0,
                static_cast<double>(result.dispatch_us) / 1000.0, result.sum,
                static_cast<unsigned long long>(gpu_delta), correct ? "ok" : "failed");
  }
  return ok;
}

bool spawn_children(int worker_count, int round_index, std::vector<ChildState>& children) {
  std::vector<WorkerPipes> pipes(static_cast<size_t>(worker_count));
  for (int i = 0; i < worker_count; ++i) {
    if (pipe(pipes[i].report) != 0 || pipe(pipes[i].init_gate) != 0 ||
        pipe(pipes[i].dispatch_gate) != 0) {
      std::perror("pipe");
      close_pipe_sets(pipes);
      return false;
    }
  }

  children.assign(static_cast<size_t>(worker_count), ChildState{});
  std::fflush(nullptr);
  for (int i = 0; i < worker_count; ++i) {
    const pid_t pid = fork();
    if (pid < 0) {
      std::perror("fork");
      for (auto& child : children) {
        if (child.pid > 0)
          (void)kill(child.pid, SIGTERM);
      }
      close_pipe_sets(pipes);
      for (auto& child : children) {
        if (child.pid <= 0)
          continue;
        int status = 0;
        while (waitpid(child.pid, &status, 0) < 0 && errno == EINTR) {}
      }
      children.clear();
      return false;
    }

    if (pid == 0) {
      close_unrelated_child_fds(pipes, i);
      run_child(i, round_index, pipes[i].report[1], pipes[i].init_gate[0],
                pipes[i].dispatch_gate[0]);
    }

    children[i].pid = pid;
    children[i].worker_index = i;
    children[i].round_index = round_index;
  }

  for (int i = 0; i < worker_count; ++i) {
    close_fd(pipes[i].report[1]);
    close_fd(pipes[i].init_gate[0]);
    close_fd(pipes[i].dispatch_gate[0]);

    children[i].report_fd = pipes[i].report[0];
    pipes[i].report[0] = -1;
    children[i].init_gate_fd = pipes[i].init_gate[1];
    pipes[i].init_gate[1] = -1;
    children[i].dispatch_gate_fd = pipes[i].dispatch_gate[1];
    pipes[i].dispatch_gate[1] = -1;

    if (!set_nonblocking(children[i].report_fd)) {
      std::perror("fcntl(O_NONBLOCK)");
      close_pipe_sets(pipes);
      abort_children(children);
      return false;
    }
  }
  close_pipe_sets(pipes);
  return true;
}

bool run_round(int worker_count, int round_index, int stage_timeout_seconds) {
  std::vector<ChildState> children;
  if (!spawn_children(worker_count, round_index, children))
    return false;

  const auto fail_round = [&children]() {
    abort_children(children);
    print_round_diagnostics(children);
    return false;
  };

  if (!wait_for_stage(children, Stage::kReadyInit, stage_timeout_seconds, "ready-init"))
    return fail_round();
  std::printf("round=%d releasing %d children into pgaccel_init\n", round_index + 1, worker_count);
  std::fflush(stdout);
  if (!release_gate(children, true))
    return fail_round();

  if (!wait_for_stage(children, Stage::kReadyDispatch, stage_timeout_seconds, "init"))
    return fail_round();
  std::printf("round=%d releasing %d children into first GPU dispatch\n", round_index + 1,
              worker_count);
  std::fflush(stdout);
  if (!release_gate(children, false))
    return fail_round();

  if (!wait_for_stage(children, Stage::kDispatchSucceeded, stage_timeout_seconds, "dispatch"))
    return fail_round();
  if (!wait_for_stage(children, Stage::kComplete, stage_timeout_seconds, "shutdown"))
    return fail_round();
  if (!wait_for_exit(children, stage_timeout_seconds, "exit"))
    return fail_round();

  const bool valid = validate_round(children);
  if (!valid)
    print_round_diagnostics(children);
  close_parent_fds(children);
  return valid;
}

}  // namespace

int main() {
  const int worker_count = parse_env_int("PGACCEL_FORK_BOUNDARY_WORKERS", 2, 32);
  const int round_count = parse_env_int("PGACCEL_FORK_BOUNDARY_ROUNDS", 1, 1000);
  const int stage_timeout_seconds =
      parse_env_int("PGACCEL_FORK_BOUNDARY_STAGE_TIMEOUT_S", 180, 1800);

  struct sigaction ignore_sigpipe = {};
  ignore_sigpipe.sa_handler = SIG_IGN;
  sigemptyset(&ignore_sigpipe.sa_mask);
  if (sigaction(SIGPIPE, &ignore_sigpipe, nullptr) != 0) {
    std::perror("sigaction(SIGPIPE)");
    return 2;
  }

  std::printf("=== Synchronized cold-fork GPU boundary test ===\n");
  std::printf("parent_pid=%d workers=%d rounds=%d stage_timeout_s=%d\n", getpid(), worker_count,
              round_count, stage_timeout_seconds);
  std::printf("parent will not call pgaccel_init or any other pgaccel API\n");

  for (int round = 0; round < round_count; ++round) {
    if (!run_round(worker_count, round, stage_timeout_seconds)) {
      std::fprintf(stderr, "RESULT: FAIL at round %d/%d\n", round + 1, round_count);
      return 1;
    }
  }

  std::printf("RESULT: PASS - %d synchronized children x %d rounds\n", worker_count, round_count);
  return 0;
}
