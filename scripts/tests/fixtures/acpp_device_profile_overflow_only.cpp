#pragma clang attribute push(__attribute__((no_profile_instrument_function)), \
                             apply_to = function)
#include <sycl/sycl.hpp>

#include <csignal>
#include <cstdint>
#include <iostream>
#include <string_view>
#include <sys/resource.h>

extern "C" void profile_step(const char*, std::uint64_t, std::uint32_t,
                             std::uint32_t, std::uint64_t)
    asm("llvm.instrprof.increment.step");

static const char overflow_profile_name[] = "metal_overflow_only_probe";
static const char ordinary_profile_name[] = "metal_profile_flush_probe";
static const char short_write_profile_name[] =
    "metal_short_write_probe_"
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";

SYCL_EXTERNAL __attribute__((noinline)) void overflow_profile_step() {
  profile_step(overflow_profile_name, 0x12345678u, 1u, 0u,
               UINT64_C(0x100000000));
}

struct OverflowOnlyProfileKernel {
  void operator()() const { overflow_profile_step(); }
};

SYCL_EXTERNAL __attribute__((noinline)) void ordinary_profile_step() {
  profile_step(ordinary_profile_name, 0x87654321u, 1u, 0u, UINT64_C(1));
}

struct OrdinaryProfileKernel {
  void operator()() const { ordinary_profile_step(); }
};

SYCL_EXTERNAL __attribute__((noinline)) void short_write_profile_step() {
  profile_step(short_write_profile_name, 0xabcdef01u, 1u, 0u, UINT64_C(1));
}

struct ShortWriteProfileKernel {
  void operator()() const { short_write_profile_step(); }
};

int main(int argc, char** argv) {
  if (argc != 2) return 2;
  sycl::queue queue{sycl::default_selector_v};
  const std::string_view mode{argv[1]};
  if (mode == "overflow") {
    queue.single_task(OverflowOnlyProfileKernel{}).wait();
  } else if (mode == "ordinary") {
    queue.single_task(OrdinaryProfileKernel{}).wait();
  } else if (mode == "short-write") {
    queue.single_task(ShortWriteProfileKernel{}).wait();
  } else {
    return 2;
  }
  std::cout << queue.get_device().get_info<sycl::info::device::name>()
            << std::endl;
  if (mode == "short-write") {
    if (std::signal(SIGXFSZ, SIG_IGN) == SIG_ERR) return 3;
    const rlimit limit{512, 512};
    if (setrlimit(RLIMIT_FSIZE, &limit) != 0) return 4;
  }
  return 0;
}
#pragma clang attribute pop
