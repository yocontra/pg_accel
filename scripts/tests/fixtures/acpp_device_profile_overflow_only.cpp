#pragma clang attribute push(__attribute__((no_profile_instrument_function)), \
                             apply_to = function)
#include <sycl/sycl.hpp>

#include <cstdint>
#include <iostream>
#include <string_view>

extern "C" void profile_step(const char*, std::uint64_t, std::uint32_t,
                             std::uint32_t, std::uint64_t)
    asm("llvm.instrprof.increment.step");

static const char overflow_profile_name[] = "metal_overflow_only_probe";
static const char ordinary_profile_name[] = "metal_profile_flush_probe";

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

int main(int argc, char** argv) {
  if (argc != 2) return 2;
  sycl::queue queue{sycl::default_selector_v};
  const std::string_view mode{argv[1]};
  if (mode == "overflow") {
    queue.single_task(OverflowOnlyProfileKernel{}).wait();
  } else if (mode == "ordinary") {
    queue.single_task(OrdinaryProfileKernel{}).wait();
  } else {
    return 2;
  }
  std::cout << queue.get_device().get_info<sycl::info::device::name>() << '\n';
  return 0;
}
#pragma clang attribute pop
