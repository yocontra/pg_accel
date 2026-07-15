// archive_stats.cpp — observability for the AdaptiveCpp Metal binary
// archive cache. Pure host code (no SYCL kernel inside), but compiled
// into pgaccel_kernels so its FFI lives next to the rest of the kernel
// surface that depends on the AdaptiveCpp runtime layout.
//
// This supports the Metal pipeline-state and binary-archive evidence gates.

#include <cerrno>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <string>
#include <system_error>
#include <unordered_set>

#include "pgaccel_ffi.h"
#include "pgaccel_queue.h"

namespace {

/// Resolve the AdaptiveCpp JIT cache directory. Mirrors the path that
/// `hipsycl::common::filesystem::persistent_storage::get_jit_cache_dir()`
/// returns at runtime: `$HOME/.acpp/apps/global/jit-cache`. The runtime
/// will accept `ACPP_APPDB_DIR` to relocate this in CI environments;
/// honour it here so the snapshot matches the actual cache path the
/// AdaptiveCpp backend writes to.
///
/// Returns the empty string if HOME is unset and ACPP_APPDB_DIR is not
/// set — callers must treat that as a snapshot failure rather than
/// silently zeroing counters.
std::string resolve_jit_cache_dir() {
  if (const char* override_dir = std::getenv("ACPP_APPDB_DIR")) {
    if (override_dir[0] != '\0') {
      // ACPP_APPDB_DIR points at `<dir>/apps`. Append /global/jit-cache.
      std::filesystem::path p{override_dir};
      p /= "global";
      p /= "jit-cache";
      return p.string();
    }
  }
  const char* home = std::getenv("HOME");
  if (!home || home[0] == '\0') {
    return {};
  }
  std::filesystem::path p{home};
  p /= ".acpp";
  p /= "apps";
  p /= "global";
  p /= "jit-cache";
  return p.string();
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
    return PGACCEL_ERROR;
  }

  for (const auto& stem : metallib_stems) {
    if (metalar_stems.find(stem) == metalar_stems.end()) {
      ++out->orphan_metallib;
    }
  }

  return PGACCEL_OK;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_archive_stats_snapshot", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_archive_stats_snapshot", nullptr);
}

extern "C" pgaccel_status pgaccel_archive_jit_cache_dir(char* buf, size_t buf_len) try {
  if (!buf || buf_len == 0) {
    return PGACCEL_ERROR;
  }
  const std::string cache_dir = resolve_jit_cache_dir();
  if (cache_dir.empty()) {
    buf[0] = '\0';
    return PGACCEL_ERROR;
  }
  if (cache_dir.size() + 1 > buf_len) {
    // Truncating would mislead the caller about which path was actually
    // scanned. Refuse instead of producing a partial path.
    buf[0] = '\0';
    return PGACCEL_ERROR;
  }
  std::memcpy(buf, cache_dir.c_str(), cache_dir.size() + 1);
  return PGACCEL_OK;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_archive_jit_cache_dir", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_archive_jit_cache_dir", nullptr);
}
