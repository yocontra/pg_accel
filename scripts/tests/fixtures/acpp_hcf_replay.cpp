// Compiler-only regression. Never link or load this object with its replay.
#ifdef PGACCEL_REPLAY_WIDTH_ONLY
// Header-free cross-target input distinguishes the pass guard from missing SDKs.
extern "C" int replay_width_only() { return 42; }
#else
#include <sycl/sycl.hpp>

// Twenty distinct annotated functions also match the name discovery path.
// This spills the old SmallPtrSet's 16-element inline storage and exercises
// duplicate discovery of each function, not just many calls to one helper.
#define REPLAY_EXTRACTOR(N)                                                     \
  extern "C" void replay_kernel_##N();                                          \
  extern "C" const char replay_name_##N[];                                      \
  extern "C" __attribute__((annotate("acpp_sscp_extract_kernel_name"))) void     \
      __acpp_sscp_extract_kernel_name_##N(void (*)(), const char *);             \
  extern "C" const char *replay_extract_##N() {                                 \
    __acpp_sscp_extract_kernel_name_##N(replay_kernel_##N, replay_name_##N);      \
    return replay_name_##N;                                                    \
  }

REPLAY_EXTRACTOR(00)
REPLAY_EXTRACTOR(01)
REPLAY_EXTRACTOR(02)
REPLAY_EXTRACTOR(03)
REPLAY_EXTRACTOR(04)
REPLAY_EXTRACTOR(05)
REPLAY_EXTRACTOR(06)
REPLAY_EXTRACTOR(07)
REPLAY_EXTRACTOR(08)
REPLAY_EXTRACTOR(09)
REPLAY_EXTRACTOR(10)
REPLAY_EXTRACTOR(11)
REPLAY_EXTRACTOR(12)
REPLAY_EXTRACTOR(13)
REPLAY_EXTRACTOR(14)
REPLAY_EXTRACTOR(15)
REPLAY_EXTRACTOR(16)
REPLAY_EXTRACTOR(17)
REPLAY_EXTRACTOR(18)
REPLAY_EXTRACTOR(19)

void replay_device_computation(sycl::queue &queue, int *output) {
  queue.single_task([=]() { *output = 42; });
}
#endif
