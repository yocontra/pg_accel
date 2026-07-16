#include <sycl/sycl.hpp>

struct DeviceProfileDormancyKernel {
  void operator()(int* value) const { *value = 7; }
};

int main() {
  sycl::queue queue{sycl::default_selector_v};
  int* value = sycl::malloc_shared<int>(1, queue);
  if (!value) return 2;
  *value = 0;
  queue.single_task([=]() { DeviceProfileDormancyKernel{}(value); }).wait();
  const int status = *value == 7 ? 0 : 3;
  sycl::free(value, queue);
  return status;
}
