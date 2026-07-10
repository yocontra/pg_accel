// Child-process regression ladder for the linear pairwise spatial kernel.
// Each child initializes Metal cold, evaluates one row-count boundary, and
// exits independently so a driver/runtime abort is attributed to that cell.

#include <signal.h>
#include <sys/wait.h>
#include <unistd.h>

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <vector>

#include "pgaccel_ffi.h"

namespace {

std::vector<float> make_regular_ring(size_t unique_vertices) {
  constexpr double kPi = 3.14159265358979323846264338327950288;
  std::vector<float> ring((unique_vertices + 1) * 2);
  for (size_t i = 0; i < unique_vertices; ++i) {
    const double angle = 2.0 * kPi * static_cast<double>(i) / static_cast<double>(unique_vertices);
    ring[i * 2] = static_cast<float>(std::cos(angle));
    ring[i * 2 + 1] = static_cast<float>(std::sin(angle));
  }
  ring[unique_vertices * 2] = ring[0];
  ring[unique_vertices * 2 + 1] = ring[1];
  return ring;
}

[[noreturn]] void run_case(size_t row_count) {
  alarm(180);
  if (pgaccel_init() != PGACCEL_OK)
    _exit(10);

  pgaccel_reset_gpu_exec_count();
  if (row_count == 0) {
    const pgaccel_status status = pgaccel_spatial_intersects_pairwise(nullptr, nullptr, 0, nullptr);
    if (status != PGACCEL_OK || pgaccel_gpu_exec_count() != 0)
      _exit(11);
    if (pgaccel_shutdown() != PGACCEL_OK)
      _exit(12);
    _exit(0);
  }

  constexpr size_t kPolygonVertices = 100;
  std::vector<float> polygon_coords = make_regular_ring(kPolygonVertices);
  float polygon_bbox[] = {-1.0f, -1.0f, 1.0f, 1.0f};
  uint32_t polygon_rings[] = {0};
  const pgaccel_geometry polygon = {PGACCEL_GEOM_POLYGON, polygon_bbox,  polygon_coords.data(),
                                    kPolygonVertices + 1, polygon_rings, 1};

  std::vector<float> point_coords(row_count * 2);
  std::vector<float> point_bboxes(row_count * 4);
  std::vector<pgaccel_geometry> points(row_count);
  std::vector<pgaccel_geometry> polygons(row_count, polygon);
  std::vector<int8_t> results(row_count, 99);

  for (size_t i = 0; i < row_count; ++i) {
    const float coordinate = (i & 1) == 0 ? 0.0f : 2.0f;
    point_coords[i * 2] = coordinate;
    point_coords[i * 2 + 1] = coordinate;
    point_bboxes[i * 4] = coordinate;
    point_bboxes[i * 4 + 1] = coordinate;
    point_bboxes[i * 4 + 2] = coordinate;
    point_bboxes[i * 4 + 3] = coordinate;
    points[i] = {PGACCEL_GEOM_POINT,
                 point_bboxes.data() + i * 4,
                 point_coords.data() + i * 2,
                 1,
                 nullptr,
                 0};
  }

  const uint64_t before = pgaccel_gpu_exec_count();
  const pgaccel_status status = pgaccel_spatial_intersects_pairwise(points.data(), polygons.data(),
                                                                    row_count, results.data());
  const uint64_t after = pgaccel_gpu_exec_count();
  if (status != PGACCEL_OK)
    _exit(20);
  if (after != before + 1)
    _exit(21);

  for (size_t i = 0; i < row_count; ++i) {
    const int8_t expected = (i & 1) == 0 ? 1 : -1;
    if (results[i] != expected)
      _exit(results[i] == 99 ? 22 : 23);
  }

  if (pgaccel_shutdown() != PGACCEL_OK)
    _exit(24);
  _exit(0);
}

}  // namespace

int main() {
  // DeviceLimits clamps chunk rows to [256, 65,536]. The remaining cells
  // bracket the quarantined 80K..150K planner band without removing it.
  const size_t row_counts[] = {0,      1,      255,    256,     65'535,  65'536, 65'537,
                               79'999, 80'000, 99'999, 100'000, 150'000, 150'001};

  (void)setenv("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES", 1);
  for (size_t row_count : row_counts) {
    const pid_t pid = fork();
    if (pid < 0) {
      std::perror("fork");
      return 1;
    }
    if (pid == 0)
      run_case(row_count);

    int status = 0;
    if (waitpid(pid, &status, 0) != pid) {
      std::perror("waitpid");
      return 1;
    }
    if (WIFSIGNALED(status)) {
      std::fprintf(stderr, "FAIL rows=%zu child signal=%d\n", row_count, WTERMSIG(status));
      return 1;
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
      std::fprintf(stderr, "FAIL rows=%zu child exit=%d\n", row_count,
                   WIFEXITED(status) ? WEXITSTATUS(status) : -1);
      return 1;
    }
    std::printf("PASS rows=%zu\n", row_count);
  }
  return 0;
}
