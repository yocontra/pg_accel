#pragma clang attribute push(__attribute__((no_profile_instrument_function)), \
                             apply_to = function)
#include <sycl/sycl.hpp>

#include <cstdint>
#include <iostream>

extern "C" void profile_step(const char*, std::uint64_t, std::uint32_t,
                             std::uint32_t, std::uint64_t)
    asm("llvm.instrprof.increment.step");

static const char overflow_profile_name[] = "metal_overflow_only_probe";

SYCL_EXTERNAL __attribute__((noinline)) void overflow_profile_step() {
  profile_step(overflow_profile_name, 0x12345678u, 1u, 0u,
               UINT64_C(0x100000000));
}

struct OverflowOnlyProfileKernel {
  void operator()() const { overflow_profile_step(); }
};

int main() {
  sycl::queue queue{sycl::default_selector_v};
  queue.single_task(OverflowOnlyProfileKernel{}).wait();
  std::cout << queue.get_device().get_info<sycl::info::device::name>() << '\n';
  return 0;
}
#pragma clang attribute pop
