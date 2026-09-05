// pgaccel_error.h — allocation-free native error handoff.

#pragma once

#include <cstddef>
#include <cstdio>
#include <exception>

#include "pgaccel_ffi.h"

/// Record one caught native exception in the per-thread handoff buffer.
/// Defined out of line so every kernel translation unit writes the same TLS.
void pgaccel_set_last_error(const char* entry_point, const std::exception* e) noexcept;

/// Honest terminal catch handler for extern "C" kernel entry points: retain
/// the exception for structured callers, log it, and return PGACCEL_ERROR. A
/// swallowed failure must never masquerade as success or as a missing device.
inline pgaccel_status pgaccel_kernel_failure(const char* entry_point, const std::exception* e) {
  const char* safe_entry_point = entry_point != nullptr ? entry_point : "<unknown>";
  pgaccel_set_last_error(safe_entry_point, e);
  std::fprintf(stderr, "pgaccel: %s: GPU kernel failure: %s\n", safe_entry_point,
               e != nullptr ? e->what() : "unknown C++ exception");
  return PGACCEL_ERROR;
}
