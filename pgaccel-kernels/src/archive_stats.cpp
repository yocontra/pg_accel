// archive_stats.cpp — observability for the AdaptiveCpp Metal binary
// archive cache. Pure host code (no SYCL kernel inside), but compiled
// into pgaccel_kernels so its FFI lives next to the rest of the kernel
// surface that depends on the AdaptiveCpp runtime layout.
//
// This supports the Metal pipeline-state and binary-archive evidence gates.

#include <cerrno>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <string>
#include <system_error>
#include <unordered_set>

#include "pgaccel_ffi.h"
#include "pgaccel_error.h"
#include "archive_cache_path.hpp"

namespace {

/// Resolve the current process's pinned runtime path without initializing its
/// persistent storage. Metal uses apps/global; Linux uses an executable/hash
/// subdirectory. Tool-only explicit path overrides do not affect the runtime.
std::string resolve_jit_cache_dir() {
  return pgaccel::archive_cache::resolve();
}

}  // namespace

extern "C" pgaccel_status pgaccel_archive_stats_snapshot(pgaccel_archive_snapshot* out) try {
  if (!out) {
    return PGACCEL_ERROR;
  }
  std::memset(out, 0, sizeof(*out));

  const std::string cache_dir = resolve_jit_cache_dir();
  if (cache_dir.empty()) {
    return PGACCEL_ERROR;
  }

  std::error_code ec;
  if (!std::filesystem::is_directory(cache_dir, ec) || ec) {
    if (ec && ec != std::errc::no_such_file_or_directory)
      return PGACCEL_ERROR;
    ec.clear();
    const bool exists = std::filesystem::exists(cache_dir, ec);
    if (ec || exists)
      return PGACCEL_ERROR;
    // Cache directory does not exist yet (e.g. first-ever run before any
    // dispatch happened). All counters legitimately stay zero, but the
    // status is OK so callers can distinguish "cache empty" from
    // "snapshot itself failed".
    return PGACCEL_OK;
  }

  // First pass: count metallibs / metalars / jits and remember the
  // metallib stem set so we can detect orphans (metallib with no
  // matching metalar) without a second directory walk.
  std::unordered_set<std::string> metallib_stems;
  std::unordered_set<std::string> metalar_stems;

  for (auto it = std::filesystem::directory_iterator(cache_dir, ec);
       !ec && it != std::filesystem::directory_iterator(); it.increment(ec)) {
    const auto& entry = *it;
    const auto& path = entry.path();
    const std::string ext = path.extension().string();
    const std::string stem = path.stem().string();
    if (ext == ".metallib") {
      ++out->metallib_files;
      metallib_stems.insert(stem);
    } else if (ext == ".metalar") {
      ++out->metalar_files;
      metalar_stems.insert(stem);
    } else if (ext == ".jit") {
      ++out->jit_files;
    }
  }

  if (ec) {
    // Directory walk ran into an I/O error after we already started —
    // surface it to the caller rather than silently truncating the
    // snapshot.
    std::memset(out, 0, sizeof(*out));
    return PGACCEL_ERROR;
  }

  for (const auto& stem : metallib_stems) {
    if (metalar_stems.find(stem) == metalar_stems.end()) {
      ++out->orphan_metallib;
    }
  }

  return PGACCEL_OK;
} catch (const std::exception& e) {
  if (out)
    std::memset(out, 0, sizeof(*out));
  return pgaccel_kernel_failure("pgaccel_archive_stats_snapshot", &e);
} catch (...) {
  if (out)
    std::memset(out, 0, sizeof(*out));
  return pgaccel_kernel_failure("pgaccel_archive_stats_snapshot", nullptr);
}

extern "C" pgaccel_status pgaccel_archive_jit_cache_dir(char* buf, size_t buf_len) try {
  if (!buf || buf_len == 0) {
    return PGACCEL_ERROR;
  }
  buf[0] = '\0';
  const std::string cache_dir = resolve_jit_cache_dir();
  if (cache_dir.empty()) {
    return PGACCEL_ERROR;
  }
  if (cache_dir.size() + 1 > buf_len) {
    // Truncating would mislead the caller about which path was actually
    // scanned. Refuse instead of producing a partial path.
    return PGACCEL_ERROR;
  }
  std::memcpy(buf, cache_dir.c_str(), cache_dir.size() + 1);
  return PGACCEL_OK;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_archive_jit_cache_dir", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_archive_jit_cache_dir", nullptr);
}
