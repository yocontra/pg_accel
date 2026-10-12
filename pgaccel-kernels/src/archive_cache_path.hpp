// Observe the pinned AdaptiveCpp path contract without persistent_storage::get().
// The settings accessor may cache read-only configuration, but does not create
// directories, open the app database or initialize a device.
#pragma once

#include <pwd.h>
#include <unistd.h>

#include <cstdlib>
#include <filesystem>
#include <string>

#include "hipSYCL/common/filesystem.hpp"
#include "hipSYCL/common/settings.hpp"
#include "hipSYCL/common/stable_running_hash.hpp"

namespace pgaccel::archive_cache {

inline std::filesystem::path default_base() {
  // Defined empty XDG/HOME values are meaningful to the pinned runtime.
  if (const char* xdg = std::getenv("XDG_DATA_HOME"))
    return std::filesystem::path{xdg} / "acpp";
  if (const char* home = std::getenv("HOME"))
    return std::filesystem::path{home} / ".acpp";
  if (const passwd* account = ::getpwuid(::getuid()); account && account->pw_dir)
    return std::filesystem::path{account->pw_dir} / ".acpp";
  return std::filesystem::current_path() / ".acpp";
}

inline std::filesystem::path path_for(const std::filesystem::path& base,
                                      const std::string& executable) {
  std::string application = "global";
  if (!executable.empty()) {
    hipsycl::common::stable_running_hash hash;
    hash(executable.data(), executable.size());
    application = std::filesystem::path{executable}.filename().string() + "-" +
                  std::to_string(hash.get_current_hash());
  }
  return base / "apps" / application / "jit-cache";
}

inline std::string resolve() {
  std::string configured;
  const auto base = hipsycl::common::settings::try_retrieve_settings_variable("appdb_dir", configured)
                        ? std::filesystem::path{configured}
                        : default_base();
  return path_for(base, hipsycl::common::filesystem::get_this_executable_path()).string();
}

}  // namespace pgaccel::archive_cache
