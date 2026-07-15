#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <limits>
#include <utility>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"

namespace {

int passed = 0;
int failed = 0;

#define CHECK(label, condition)                                              \
  do {                                                                       \
    if (condition) {                                                         \
      ++passed;                                                              \
    } else {                                                                 \
      std::fprintf(stderr, "FAIL: %s (%s:%d)\n", label, __FILE__, __LINE__); \
      ++failed;                                                              \
    }                                                                        \
  } while (0)

template <typename T>
class DeviceBuffer {
 public:
  DeviceBuffer() = default;
  explicit DeviceBuffer(const std::vector<T>& values) { copy(values); }
  explicit DeviceBuffer(size_t count) { allocate(count); }
  DeviceBuffer(const DeviceBuffer&) = delete;
  DeviceBuffer& operator=(const DeviceBuffer&) = delete;
  DeviceBuffer(DeviceBuffer&& other) noexcept : pointer_(other.pointer_), count_(other.count_) {
    other.pointer_ = nullptr;
    other.count_ = 0;
  }
  DeviceBuffer& operator=(DeviceBuffer&& other) noexcept {
    if (this != &other) {
      reset();
      pointer_ = other.pointer_;
      count_ = other.count_;
      other.pointer_ = nullptr;
      other.count_ = 0;
    }
    return *this;
  }
  ~DeviceBuffer() { reset(); }

  void copy(const std::vector<T>& values) {
    reset();
    count_ = values.size();
    if (values.empty())
      return;
    void* raw = nullptr;
    if (pgaccel_expr_device_alloc_copy(values.data(), values.size() * sizeof(T), &raw) !=
            PGACCEL_OK ||
        raw == nullptr) {
      std::fprintf(stderr, "device copy allocation failed\n");
      std::exit(2);
    }
    pointer_ = static_cast<T*>(raw);
  }

  void allocate(size_t count) {
    reset();
    count_ = count;
    if (count == 0)
      return;
    void* raw = nullptr;
    if (pgaccel_expr_device_alloc(count * sizeof(T), &raw) != PGACCEL_OK || raw == nullptr) {
      std::fprintf(stderr, "device output allocation failed\n");
      std::exit(2);
    }
    pointer_ = static_cast<T*>(raw);
  }

  std::vector<T> to_host() const {
    std::vector<T> result(count_);
    if (count_ != 0 && pgaccel_expr_device_copy_to_host(result.data(), pointer_,
                                                        count_ * sizeof(T)) != PGACCEL_OK) {
      std::fprintf(stderr, "device output copy failed\n");
      std::exit(2);
    }
    return result;
  }

  T* get() const { return pointer_; }
  size_t size() const { return count_; }

 private:
  void reset() {
    if (pointer_ != nullptr)
      pgaccel_expr_device_free(pointer_);
    pointer_ = nullptr;
    count_ = 0;
  }

  T* pointer_ = nullptr;
  size_t count_ = 0;
};

struct HostGeometry {
  uint32_t type;
  std::vector<double> coordinates;
  std::vector<uint64_t> rings;
  int32_t srid = 4326;
  bool is_null = false;
};

HostGeometry point(double x, double y, int32_t srid = 4326) {
  return {PGACCEL_RESIDENT_GEOMETRY_POINT, {x, y}, {}, srid, false};
}

HostGeometry line(std::initializer_list<double> coordinates, int32_t srid = 4326) {
  return {PGACCEL_RESIDENT_GEOMETRY_LINESTRING, coordinates, {}, srid, false};
}

HostGeometry polygon(std::initializer_list<double> coordinates,
                     std::initializer_list<uint64_t> rings = {0}, int32_t srid = 4326) {
  return {PGACCEL_RESIDENT_GEOMETRY_POLYGON, coordinates, rings, srid, false};
}

HostGeometry empty_point() {
  return {PGACCEL_RESIDENT_GEOMETRY_POINT, {}, {}, 4326, false};
}

HostGeometry null_geometry() {
  return {0, {}, {}, 0, true};
}

struct DeviceLane {
  DeviceBuffer<double> coordinates;
  DeviceBuffer<double> bboxes;
  DeviceBuffer<uint64_t> geometry_offsets;
  DeviceBuffer<uint64_t> ring_offsets;
  DeviceBuffer<pgaccel_resident_geometry_row> rows;
  DeviceBuffer<uint8_t> nulls;
  size_t row_count = 0;
  size_t coordinate_pair_count = 0;
  size_t ring_count = 0;

  pgaccel_resident_geometry_view view() const {
    return {PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION,
            0,
            coordinates.get(),
            bboxes.get(),
            geometry_offsets.get(),
            ring_offsets.get(),
            rows.get(),
            nulls.get(),
            coordinates.size() * sizeof(double),
            bboxes.size() * sizeof(double),
            geometry_offsets.size() * sizeof(uint64_t),
            ring_offsets.size() * sizeof(uint64_t),
            rows.size() * sizeof(pgaccel_resident_geometry_row),
            nulls.size() * sizeof(uint8_t),
            row_count,
            coordinate_pair_count,
            ring_count};
  }
};

struct DeviceWorkspace {
  DeviceBuffer<uint8_t> control{PGACCEL_SPATIAL_CONTROL_BYTES};
  DeviceBuffer<uint32_t> failure_flags{1};

  pgaccel_spatial_workspace view() const {
    return {PGACCEL_SPATIAL_WORKSPACE_ABI_VERSION,
            0,
            control.get(),
            control.size() * sizeof(uint8_t),
            failure_flags.get(),
            failure_flags.size() * sizeof(uint32_t)};
  }
};

pgaccel_status run_resident_request(const pgaccel_spatial_resident_request* request,
                                    int32_t* detail) {
  DeviceWorkspace owned;
  const pgaccel_spatial_workspace workspace = owned.view();
  pgaccel_status status = pgaccel_spatial_eval_resident_launch(request, &workspace, detail);
  if (status == PGACCEL_OK && request != nullptr && request->count != 0)
    status = pgaccel_spatial_workspace_finish(&workspace, detail);
  return status;
}

DeviceLane make_lane(const std::vector<HostGeometry>& geometries) {
  std::vector<double> coordinates;
  std::vector<double> bboxes;
  std::vector<uint64_t> geometry_offsets{0};
  std::vector<uint64_t> ring_offsets;
  std::vector<pgaccel_resident_geometry_row> rows;
  std::vector<uint8_t> nulls;
  for (const HostGeometry& geometry : geometries) {
    const uint64_t pair_begin = coordinates.size() / 2;
    if (!geometry.is_null)
      coordinates.insert(coordinates.end(), geometry.coordinates.begin(),
                         geometry.coordinates.end());
    const uint64_t pair_end = coordinates.size() / 2;
    geometry_offsets.push_back(pair_end);
    nulls.push_back(geometry.is_null ? 1 : 0);
    if (geometry.is_null) {
      rows.push_back({});
      bboxes.insert(bboxes.end(), {0.0, 0.0, 0.0, 0.0});
      continue;
    }
    const uint64_t first_ring = ring_offsets.size();
    for (uint64_t local : geometry.rings)
      ring_offsets.push_back(pair_begin + local);
    const uint32_t flags = pair_begin == pair_end ? 0 : PGACCEL_RESIDENT_GEOMETRY_BBOX_VALID;
    rows.push_back({geometry.type, geometry.srid, first_ring,
                    static_cast<uint32_t>(geometry.rings.size()), flags});
    if (pair_begin == pair_end) {
      bboxes.insert(bboxes.end(), {0.0, 0.0, 0.0, 0.0});
      continue;
    }
    double min_x = std::numeric_limits<double>::max();
    double min_y = std::numeric_limits<double>::max();
    double max_x = std::numeric_limits<double>::lowest();
    double max_y = std::numeric_limits<double>::lowest();
    for (size_t index = 0; index < geometry.coordinates.size(); index += 2) {
      min_x = std::fmin(min_x, geometry.coordinates[index]);
      min_y = std::fmin(min_y, geometry.coordinates[index + 1]);
      max_x = std::fmax(max_x, geometry.coordinates[index]);
      max_y = std::fmax(max_y, geometry.coordinates[index + 1]);
    }
    bboxes.insert(bboxes.end(), {min_x, min_y, max_x, max_y});
  }
  DeviceLane lane;
  lane.row_count = geometries.size();
  lane.coordinate_pair_count = coordinates.size() / 2;
  lane.ring_count = ring_offsets.size();
  lane.coordinates.copy(coordinates);
  lane.bboxes.copy(bboxes);
  lane.geometry_offsets.copy(geometry_offsets);
  lane.ring_offsets.copy(ring_offsets);
  lane.rows.copy(rows);
  lane.nulls.copy(nulls);
  return lane;
}

struct PredicateRun {
  pgaccel_status status;
  int32_t detail;
  std::vector<int8_t> results;
};

PredicateRun run_predicate(const DeviceLane& left, bool left_constant, const DeviceLane& right,
                           bool right_constant, size_t count, pgaccel_spatial_predicate predicate,
                           double threshold = 0.0, size_t byte_budget = 256 * 1024 * 1024) {
  DeviceBuffer<int8_t> output(count);
  pgaccel_spatial_resident_request request{};
  request.abi_version = PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION;
  request.predicate = predicate;
  request.distance_threshold = threshold;
  request.count = count;
  request.max_referenced_bytes = byte_budget;
  request.left = {left.view(), 0, left_constant ? 0u : 1u};
  request.right = {right.view(), 0, right_constant ? 0u : 1u};
  request.predicate_results = output.get();
  request.predicate_results_bytes = output.size() * sizeof(int8_t);
  request.output_capacity = count;
  int32_t detail = -1;
  const pgaccel_status status = run_resident_request(&request, &detail);
  return {status, detail, status == PGACCEL_OK ? output.to_host() : std::vector<int8_t>{}};
}

void check_results(const char* label, const PredicateRun& run,
                   std::initializer_list<int8_t> expected) {
  CHECK(label, run.status == PGACCEL_OK && run.detail == PGACCEL_SPATIAL_DETAIL_NONE &&
                   run.results == std::vector<int8_t>(expected));
}

struct DistanceRun {
  pgaccel_status status;
  int32_t detail;
  std::vector<double> distances;
  std::vector<uint8_t> uncertain;
};

DistanceRun run_distance(const DeviceLane& left, bool left_constant, const DeviceLane& right,
                         bool right_constant, size_t count) {
  DeviceBuffer<double> distances(count);
  DeviceBuffer<uint8_t> uncertain(count);
  pgaccel_spatial_resident_request request{};
  request.abi_version = PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION;
  request.predicate = PGACCEL_SPATIAL_PREDICATE_DISTANCE;
  request.count = count;
  request.max_referenced_bytes = 256 * 1024 * 1024;
  request.left = {left.view(), 0, left_constant ? 0u : 1u};
  request.right = {right.view(), 0, right_constant ? 0u : 1u};
  request.distances = distances.get();
  request.distances_bytes = distances.size() * sizeof(double);
  request.distance_uncertain = uncertain.get();
  request.distance_uncertain_bytes = uncertain.size() * sizeof(uint8_t);
  request.output_capacity = count;
  int32_t detail = -1;
  const pgaccel_status status = run_resident_request(&request, &detail);
  if (status != PGACCEL_OK)
    return {status, detail, {}, {}};
  return {status, detail, distances.to_host(), uncertain.to_host()};
}

void test_intersects_pair_matrix() {
  const DeviceLane square = make_lane({polygon({0, 0, 2, 0, 2, 2, 0, 2, 0, 0})});
  const DeviceLane points = make_lane({point(1, 1), point(0, 1), point(3, 3)});
  check_results("Point/Polygon",
                run_predicate(points, false, square, true, 3, PGACCEL_SPATIAL_PREDICATE_INTERSECTS),
                {1, 0, -1});
  check_results("Polygon/Point",
                run_predicate(square, true, points, false, 3, PGACCEL_SPATIAL_PREDICATE_INTERSECTS),
                {1, 0, -1});

  const DeviceLane point_pairs_left = make_lane({point(1, 1), point(1, 1)});
  const DeviceLane point_pairs_right = make_lane({point(1, 1), point(2, 2)});
  check_results("Point/Point",
                run_predicate(point_pairs_left, false, point_pairs_right, false, 2,
                              PGACCEL_SPATIAL_PREDICATE_INTERSECTS),
                {1, -1});

  const DeviceLane vertex_points = make_lane({point(1, 1), point(5, 5)});
  const DeviceLane point_lines = make_lane({line({0, 0, 1, 1, 2, 0}), line({0, 0, 1, 1, 2, 0})});
  check_results("Point/Line",
                run_predicate(vertex_points, false, point_lines, false, 2,
                              PGACCEL_SPATIAL_PREDICATE_INTERSECTS),
                {1, -1});
  check_results("Line/Point",
                run_predicate(point_lines, false, vertex_points, false, 2,
                              PGACCEL_SPATIAL_PREDICATE_INTERSECTS),
                {1, -1});

  const DeviceLane lines = make_lane({line({-1, 1, 3, 1}), line({3, 0, 3, 2}), line({1, 1, 1, 1})});
  check_results("Line/Polygon",
                run_predicate(lines, false, square, true, 3, PGACCEL_SPATIAL_PREDICATE_INTERSECTS),
                {1, -1, 1});
  check_results("Polygon/Line",
                run_predicate(square, true, lines, false, 3, PGACCEL_SPATIAL_PREDICATE_INTERSECTS),
                {1, -1, 1});

  const DeviceLane line_left =
      make_lane({line({0, 0, 2, 2}), line({0, 0, 2, 0}), line({0, 0, 1, 0})});
  const DeviceLane line_right =
      make_lane({line({0, 2, 2, 0}), line({0, 2, 2, 2}), line({1, 0, 2, 0})});
  check_results(
      "Line/Line",
      run_predicate(line_left, false, line_right, false, 3, PGACCEL_SPATIAL_PREDICATE_INTERSECTS),
      {1, -1, 0});

  const DeviceLane polygons =
      make_lane({polygon({1, 1, 3, 1, 3, 3, 1, 3, 1, 1}), polygon({3, 3, 4, 3, 4, 4, 3, 4, 3, 3}),
                 polygon({0, 0, 2, 0, 2, 2, 0, 2, 0, 0})});
  check_results(
      "Polygon/Polygon",
      run_predicate(polygons, false, square, true, 3, PGACCEL_SPATIAL_PREDICATE_INTERSECTS),
      {1, -1, 0});
  check_results(
      "Polygon/Polygon reverse",
      run_predicate(square, true, polygons, false, 3, PGACCEL_SPATIAL_PREDICATE_INTERSECTS),
      {1, -1, 0});

  const DeviceLane mixed_left =
      make_lane({point(1, 1), point(0, 0), point(1, 1), line({0, 0, 2, 0}), line({0, 0, 2, 2}),
                 line({-1, 1, 3, 1}), polygon({0, 0, 2, 0, 2, 2, 0, 2, 0, 0}),
                 polygon({0, 0, 2, 0, 2, 2, 0, 2, 0, 0}), polygon({0, 0, 2, 0, 2, 2, 0, 2, 0, 0})});
  const DeviceLane mixed_right =
      make_lane({point(1, 1), line({0, 0, 2, 0}), polygon({0, 0, 2, 0, 2, 2, 0, 2, 0, 0}),
                 point(0, 0), line({0, 2, 2, 0}), polygon({0, 0, 2, 0, 2, 2, 0, 2, 0, 0}),
                 point(1, 1), line({-1, 1, 3, 1}), polygon({1, 1, 3, 1, 3, 3, 1, 3, 1, 1})});
  pgaccel_reset_gpu_exec_count();
  const PredicateRun mixed =
      run_predicate(mixed_left, false, mixed_right, false, 9, PGACCEL_SPATIAL_PREDICATE_INTERSECTS);
  check_results("mixed ordered geometry-pair batch", mixed, {1, 1, 1, 1, 1, 1, 1, 1, 1});
  CHECK("mixed geometry-pair batch records one aggregate execution", pgaccel_gpu_exec_count() == 1);
}

void test_holes_boundaries_and_predicates() {
  const DeviceLane holed =
      make_lane({polygon({0, 0, 4, 0, 4, 4, 0, 4, 0, 0, 1, 1, 3, 1, 3, 3, 1, 3, 1, 1}, {0, 5})});
  const DeviceLane points = make_lane({point(0.5, 0.5), point(2, 2), point(1, 2)});
  check_results("holes and boundaries",
                run_predicate(points, false, holed, true, 3, PGACCEL_SPATIAL_PREDICATE_INTERSECTS),
                {1, -1, 0});

  const DeviceLane square = make_lane({polygon({0, 0, 2, 0, 2, 2, 0, 2, 0, 0})});
  const DeviceLane contain_points = make_lane({point(1, 1), point(0, 1), point(3, 3)});
  check_results(
      "Contains Point",
      run_predicate(square, true, contain_points, false, 3, PGACCEL_SPATIAL_PREDICATE_CONTAINS),
      {1, 0, -1});
  check_results(
      "Within Point",
      run_predicate(contain_points, false, square, true, 3, PGACCEL_SPATIAL_PREDICATE_WITHIN),
      {1, 0, -1});

  const DeviceLane contain_lines = make_lane({line({0.5, 0.5, 1.5, 1.5}), line({-1, 1, 3, 1})});
  check_results(
      "Contains Line",
      run_predicate(square, true, contain_lines, false, 2, PGACCEL_SPATIAL_PREDICATE_CONTAINS),
      {1, -1});

  const DeviceLane concave =
      make_lane({polygon({0, 0, 4, 0, 4, 4, 3, 4, 3, 1, 1, 1, 1, 4, 0, 4, 0, 0})});
  const DeviceLane exits_concavity = make_lane({line({0.5, 3.5, 3.5, 3.5})});
  check_results(
      "Contains rejects edge leaving concavity",
      run_predicate(concave, true, exits_concavity, true, 1, PGACCEL_SPATIAL_PREDICATE_CONTAINS),
      {-1});

  const DeviceLane extreme_line_a = make_lane({line({-1e200, -1e200, 1e200, 1e200})});
  const DeviceLane extreme_line_b =
      make_lane({line({-1e200, -1e200 + 1e185, 1e200, 1e200 - 1e185})});
  check_results("overflowing orientation is uncertain",
                run_predicate(extreme_line_a, true, extreme_line_b, true, 1,
                              PGACCEL_SPATIAL_PREDICATE_INTERSECTS),
                {0});

  const DeviceLane large_square = make_lane({polygon({0, 0, 10, 0, 10, 10, 0, 10, 0, 0})});
  DeviceLane non_tight_point = make_lane({point(5, 5)});
  const double covering_bbox[4] = {-1, -1, 11, 11};
  CHECK("non-tight bbox setup",
        pgaccel_expr_device_copy_from_host(non_tight_point.bboxes.get(), covering_bbox,
                                           sizeof(covering_bbox)) == PGACCEL_OK);
  check_results("Contains does not trust non-tight inner bbox",
                run_predicate(large_square, true, non_tight_point, true, 1,
                              PGACCEL_SPATIAL_PREDICATE_CONTAINS),
                {1});

  const DeviceLane origins = make_lane({point(0, 0), point(0, 0), point(0, 0)});
  const DeviceLane targets = make_lane({point(1, 0), point(3, 0), point(2, 0)});
  check_results(
      "DWithin",
      run_predicate(origins, false, targets, false, 3, PGACCEL_SPATIAL_PREDICATE_DWITHIN, 2.0),
      {1, -1, 0});

  const DeviceLane null_empty = make_lane({null_geometry(), empty_point()});
  check_results(
      "NULL and EMPTY filters",
      run_predicate(null_empty, false, square, true, 2, PGACCEL_SPATIAL_PREDICATE_INTERSECTS),
      {-1, -1});
}

void test_distance() {
  const DeviceLane origins = make_lane({point(0, 0), point(0, 0)});
  const DeviceLane targets = make_lane({point(3, 4), point(0, 0)});
  const DistanceRun run = run_distance(origins, false, targets, false, 2);
  CHECK("Distance status", run.status == PGACCEL_OK && run.detail == PGACCEL_SPATIAL_DETAIL_NONE);
  CHECK("Distance values", run.distances.size() == 2 && std::fabs(run.distances[0] - 5.0) < 1e-12 &&
                               run.distances[1] == 0.0);
  CHECK("Distance uncertainty sidecar", run.uncertain == std::vector<uint8_t>({0, 0}));

  const DeviceLane extreme_origin = make_lane({point(0, 0)});
  const DeviceLane extreme_target = make_lane({point(1e200, 0)});
  const DistanceRun extreme = run_distance(extreme_origin, true, extreme_target, true, 1);
  CHECK("finite 1e200 point distance remains finite",
        extreme.status == PGACCEL_OK && extreme.distances.size() == 1 &&
            std::isfinite(extreme.distances[0]) &&
            std::fabs(extreme.distances[0] / 1e200 - 1.0) < 1e-12 &&
            extreme.uncertain == std::vector<uint8_t>({0}));

  const DeviceLane projection_point = make_lane({point(0, 2)});
  const DeviceLane overflowing_segment = make_lane({line({-1e308, 0, 1e308, 0})});
  const DistanceRun projection = run_distance(projection_point, true, overflowing_segment, true, 1);
  CHECK("overflowing projection is uncertain",
        projection.status == PGACCEL_OK && projection.distances == std::vector<double>({0.0}) &&
            projection.uncertain == std::vector<uint8_t>({1}));
  check_results("DWithin uses bbox lower bound after projection uncertainty",
                run_predicate(projection_point, true, overflowing_segment, true, 1,
                              PGACCEL_SPATIAL_PREDICATE_DWITHIN, 1.0),
                {-1});

  const DeviceLane mixed_left =
      make_lane({point(0, 0), point(0, 0), point(0, 0), line({0, 0, 0, 1}), line({0, 0, 0, 1}),
                 line({0, 0, 0, 1}), polygon({0, 0, 1, 0, 1, 1, 0, 1, 0, 0}),
                 polygon({0, 0, 1, 0, 1, 1, 0, 1, 0, 0}), polygon({0, 0, 1, 0, 1, 1, 0, 1, 0, 0})});
  const DeviceLane mixed_right =
      make_lane({point(3, 0), line({3, 0, 3, 1}), polygon({3, 0, 4, 0, 4, 1, 3, 1, 3, 0}),
                 point(3, 0), line({3, 0, 3, 1}), polygon({3, 0, 4, 0, 4, 1, 3, 1, 3, 0}),
                 point(3, 0), line({3, 0, 3, 1}), polygon({3, 0, 4, 0, 4, 1, 3, 1, 3, 0})});
  pgaccel_reset_gpu_exec_count();
  const DistanceRun mixed_distance = run_distance(mixed_left, false, mixed_right, false, 9);
  const std::vector<double> expected_distance = {3, 3, 3, 3, 3, 3, 2, 2, 2};
  bool mixed_distance_matches = mixed_distance.status == PGACCEL_OK &&
                                mixed_distance.distances.size() == expected_distance.size() &&
                                mixed_distance.uncertain == std::vector<uint8_t>(9, 0);
  for (size_t index = 0; mixed_distance_matches && index < expected_distance.size(); ++index)
    mixed_distance_matches =
        std::fabs(mixed_distance.distances[index] - expected_distance[index]) < 1e-12;
  CHECK("mixed ordered geometry-pair distance batch", mixed_distance_matches);
  CHECK("mixed distance batch records one aggregate execution", pgaccel_gpu_exec_count() == 1);

  pgaccel_reset_gpu_exec_count();
  check_results("mixed ordered geometry-pair DWithin batch",
                run_predicate(mixed_left, false, mixed_right, false, 9,
                              PGACCEL_SPATIAL_PREDICATE_DWITHIN, 2.5),
                {-1, -1, -1, -1, -1, -1, 1, 1, 1});
  CHECK("mixed DWithin batch records one aggregate execution", pgaccel_gpu_exec_count() == 1);
}

void test_hard_failures() {
  const DeviceLane left = make_lane({point(0, 0)});
  const DeviceLane wrong_srid = make_lane({point(0, 0, 3857)});
  PredicateRun srid =
      run_predicate(left, true, wrong_srid, true, 1, PGACCEL_SPATIAL_PREDICATE_INTERSECTS);
  CHECK("SRID mismatch is hard", srid.status == PGACCEL_INVALID_ARGUMENT &&
                                     srid.detail == PGACCEL_SPATIAL_DETAIL_SRID_MISMATCH &&
                                     srid.results.empty());

  const DeviceLane right = make_lane({point(1, 1)});
  DeviceBuffer<int8_t> legacy_output(std::vector<int8_t>{77});
  pgaccel_spatial_resident_request legacy_request{};
  legacy_request.abi_version = PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION;
  legacy_request.predicate = PGACCEL_SPATIAL_PREDICATE_INTERSECTS;
  legacy_request.count = 1;
  legacy_request.max_referenced_bytes = 1 << 20;
  legacy_request.left = {left.view(), 0, 0};
  legacy_request.right = {right.view(), 0, 0};
  legacy_request.predicate_results = legacy_output.get();
  legacy_request.predicate_results_bytes = sizeof(int8_t);
  legacy_request.output_capacity = 1;
  pgaccel_reset_gpu_exec_count();
  int32_t legacy_detail = -1;
  CHECK("legacy non-empty resident ABI declines without writes",
        pgaccel_spatial_eval_resident_ex(&legacy_request, &legacy_detail) == PGACCEL_UNSUPPORTED &&
            legacy_detail == PGACCEL_SPATIAL_DETAIL_CONTRACT &&
            legacy_output.to_host() == std::vector<int8_t>({77}) && pgaccel_gpu_exec_count() == 0);

  pgaccel_spatial_resident_request legacy_empty{};
  legacy_empty.abi_version = PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION;
  legacy_empty.predicate = PGACCEL_SPATIAL_PREDICATE_INTERSECTS;
  legacy_detail = -1;
  CHECK("legacy empty resident ABI remains valid",
        pgaccel_spatial_eval_resident_ex(&legacy_empty, &legacy_detail) == PGACCEL_OK &&
            legacy_detail == PGACCEL_SPATIAL_DETAIL_NONE);

  PredicateRun budget =
      run_predicate(left, true, right, true, 1, PGACCEL_SPATIAL_PREDICATE_INTERSECTS, 0.0, 1);
  CHECK("byte budget is hard", budget.status == PGACCEL_INVALID_ARGUMENT &&
                                   budget.detail == PGACCEL_SPATIAL_DETAIL_BYTE_BUDGET);

  PredicateRun zero_budget =
      run_predicate(left, true, right, true, 1, PGACCEL_SPATIAL_PREDICATE_INTERSECTS, 0.0, 0);
  CHECK("zero byte budget is hard", zero_budget.status == PGACCEL_INVALID_ARGUMENT &&
                                        zero_budget.detail == PGACCEL_SPATIAL_DETAIL_BYTE_BUDGET);

  const DeviceLane square = make_lane({polygon({0, 0, 2, 0, 2, 2, 0, 2, 0, 0})});
  PredicateRun lopsided =
      run_predicate(square, true, left, true, 1, PGACCEL_SPATIAL_PREDICATE_INTERSECTS, 0.0, 300);
  CHECK("lopsided budget conservatively rejects",
        lopsided.status == PGACCEL_INVALID_ARGUMENT &&
            lopsided.detail == PGACCEL_SPATIAL_DETAIL_BYTE_BUDGET);

  DeviceBuffer<int8_t> output(1);
  pgaccel_spatial_resident_request request{};
  request.abi_version = PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION;
  request.predicate = PGACCEL_SPATIAL_PREDICATE_INTERSECTS;
  request.count = 1;
  request.max_referenced_bytes = 1 << 20;
  request.left = {left.view(), 0, 0};
  request.right = {right.view(), 0, 0};
  request.predicate_results = output.get();
  request.predicate_results_bytes = output.size() * sizeof(int8_t);
  request.output_capacity = 0;
  int32_t detail = -1;
  CHECK("output capacity is hard",
        run_resident_request(&request, &detail) == PGACCEL_INVALID_ARGUMENT &&
            detail == PGACCEL_SPATIAL_DETAIL_CONTRACT);

  DeviceBuffer<int8_t> invalid_predicate_output(std::vector<int8_t>{77});
  request.predicate = static_cast<pgaccel_spatial_predicate>(
      static_cast<int>(PGACCEL_SPATIAL_PREDICATE_DISTANCE) + 1);
  request.predicate_results = invalid_predicate_output.get();
  request.predicate_results_bytes = sizeof(int8_t);
  request.output_capacity = 1;
  pgaccel_reset_gpu_exec_count();
  detail = -1;
  CHECK("unsupported predicate is hard before dispatch",
        run_resident_request(&request, &detail) == PGACCEL_INVALID_ARGUMENT &&
            detail == PGACCEL_SPATIAL_DETAIL_CONTRACT &&
            invalid_predicate_output.to_host() == std::vector<int8_t>({77}) &&
            pgaccel_gpu_exec_count() == 0);

  int8_t host_output = 99;
  request.predicate = PGACCEL_SPATIAL_PREDICATE_INTERSECTS;
  request.output_capacity = 1;
  request.predicate_results = &host_output;
  detail = -1;
  CHECK("host output pointer is hard",
        run_resident_request(&request, &detail) == PGACCEL_INVALID_ARGUMENT &&
            detail == PGACCEL_SPATIAL_DETAIL_CONTRACT && host_output == 99);

  request.predicate_results = output.get();
  request.distance_threshold = std::numeric_limits<double>::quiet_NaN();
  request.predicate = PGACCEL_SPATIAL_PREDICATE_DWITHIN;
  detail = -1;
  CHECK("NaN DWithin threshold is hard",
        run_resident_request(&request, &detail) == PGACCEL_INVALID_ARGUMENT &&
            detail == PGACCEL_SPATIAL_DETAIL_CONTRACT);

  request.predicate = PGACCEL_SPATIAL_PREDICATE_INTERSECTS;
  request.distance_threshold = 0.0;
  request.count = std::numeric_limits<size_t>::max();
  request.output_capacity = std::numeric_limits<size_t>::max();
  detail = -1;
  CHECK("output span overflow is hard",
        run_resident_request(&request, &detail) == PGACCEL_INVALID_ARGUMENT &&
            detail == PGACCEL_SPATIAL_DETAIL_CONTRACT);

  DeviceBuffer<int8_t> short_output(2);
  pgaccel_spatial_resident_request short_output_request{};
  short_output_request.abi_version = PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION;
  short_output_request.predicate = PGACCEL_SPATIAL_PREDICATE_INTERSECTS;
  short_output_request.count = 2;
  short_output_request.max_referenced_bytes = 1 << 20;
  short_output_request.left = {left.view(), 0, 0};
  short_output_request.right = {right.view(), 0, 0};
  short_output_request.predicate_results = short_output.get();
  short_output_request.predicate_results_bytes = 1;
  short_output_request.output_capacity = 2;
  detail = -1;
  CHECK("short output allocation is hard",
        run_resident_request(&short_output_request, &detail) == PGACCEL_INVALID_ARGUMENT &&
            detail == PGACCEL_SPATIAL_DETAIL_CONTRACT);

  pgaccel_resident_geometry_view oversized_view = left.view();
  oversized_view.row_count = 2;
  pgaccel_spatial_resident_request oversized_request = short_output_request;
  oversized_request.left = {oversized_view, 0, 1};
  oversized_request.predicate_results_bytes = short_output.size() * sizeof(int8_t);
  pgaccel_reset_gpu_exec_count();
  detail = -1;
  CHECK("oversized logical lane count is hard before dispatch",
        run_resident_request(&oversized_request, &detail) == PGACCEL_INVALID_ARGUMENT &&
            detail == PGACCEL_SPATIAL_DETAIL_CONTRACT && pgaccel_gpu_exec_count() == 0);

  DeviceLane invalid_null = make_lane({point(0, 0)});
  const uint8_t invalid_null_byte = 2;
  CHECK("invalid NULL sidecar setup",
        pgaccel_expr_device_copy_from_host(invalid_null.nulls.get(), &invalid_null_byte, 1) ==
            PGACCEL_OK);
  PredicateRun invalid_null_run =
      run_predicate(invalid_null, true, right, true, 1, PGACCEL_SPATIAL_PREDICATE_INTERSECTS);
  CHECK("invalid NULL sidecar is hard",
        invalid_null_run.status == PGACCEL_INVALID_ARGUMENT &&
            invalid_null_run.detail == PGACCEL_SPATIAL_DETAIL_GEOMETRY);

  DeviceLane invalid_coordinate = make_lane({point(0, 0)});
  const double nan_coordinate = std::numeric_limits<double>::quiet_NaN();
  CHECK("NaN coordinate setup",
        pgaccel_expr_device_copy_from_host(invalid_coordinate.coordinates.get(), &nan_coordinate,
                                           sizeof(nan_coordinate)) == PGACCEL_OK);
  const PredicateRun invalid_coordinate_run =
      run_predicate(invalid_coordinate, true, right, true, 1, PGACCEL_SPATIAL_PREDICATE_INTERSECTS);
  CHECK("NaN coordinate is hard",
        invalid_coordinate_run.status == PGACCEL_INVALID_ARGUMENT &&
            invalid_coordinate_run.detail == PGACCEL_SPATIAL_DETAIL_GEOMETRY);

  DeviceLane invalid_empty = make_lane({empty_point()});
  const double negative_zero = -0.0;
  CHECK("negative-zero empty bbox setup",
        pgaccel_expr_device_copy_from_host(invalid_empty.bboxes.get(), &negative_zero,
                                           sizeof(negative_zero)) == PGACCEL_OK);
  const PredicateRun invalid_empty_run =
      run_predicate(invalid_empty, true, right, true, 1, PGACCEL_SPATIAL_PREDICATE_INTERSECTS);
  CHECK("negative-zero empty bbox is hard",
        invalid_empty_run.status == PGACCEL_INVALID_ARGUMENT &&
            invalid_empty_run.detail == PGACCEL_SPATIAL_DETAIL_GEOMETRY);
}

void test_recheck_helpers() {
  const auto make_eval_request = [](const DeviceLane& left, bool left_constant,
                                    const DeviceLane& right, bool right_constant, size_t count,
                                    DeviceBuffer<int8_t>& output) {
    pgaccel_spatial_resident_request request{};
    request.abi_version = PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION;
    request.predicate = PGACCEL_SPATIAL_PREDICATE_INTERSECTS;
    request.count = count;
    request.max_referenced_bytes = 32 * 1024 * 1024;
    request.left = {left.view(), 0, left_constant ? 0u : 1u};
    request.right = {right.view(), 0, right_constant ? 0u : 1u};
    request.predicate_results = output.get();
    request.predicate_results_bytes = output.size() * sizeof(int8_t);
    request.output_capacity = count;
    return request;
  };

  const DeviceLane square = make_lane({polygon({0, 0, 2, 0, 2, 2, 0, 2, 0, 0})});
  const DeviceLane points = make_lane({point(1, 1), point(0, 1), point(3, 3)});
  DeviceBuffer<int8_t> tri_state(3);
  DeviceBuffer<int8_t> final_mask(std::vector<int8_t>{55, 55, 55});
  DeviceBuffer<uint64_t> uncertain_indices(std::vector<uint64_t>{99, 99, 99});
  DeviceBuffer<uint64_t> uncertain_count(std::vector<uint64_t>{99});
  const pgaccel_spatial_resident_request eval =
      make_eval_request(points, false, square, true, 3, tri_state);
  pgaccel_spatial_recheck_compact_request compact{};
  compact.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  compact.tri_state = tri_state.get();
  compact.tri_state_bytes = tri_state.size() * sizeof(int8_t);
  compact.final_mask = final_mask.get();
  compact.final_mask_bytes = final_mask.size() * sizeof(int8_t);
  compact.uncertain_indices = uncertain_indices.get();
  compact.uncertain_indices_bytes = uncertain_indices.size() * sizeof(uint64_t);
  compact.uncertain_count = uncertain_count.get();
  compact.uncertain_count_bytes = uncertain_count.size() * sizeof(uint64_t);
  compact.row_count = 3;
  compact.uncertain_capacity = 3;
  DeviceWorkspace compact_workspace;
  const pgaccel_spatial_workspace compact_workspace_view = compact_workspace.view();
  int32_t detail = -1;
  const pgaccel_status eval_status =
      pgaccel_spatial_eval_resident_launch(&eval, &compact_workspace_view, &detail);
  const pgaccel_status compact_status =
      eval_status == PGACCEL_OK
          ? pgaccel_spatial_recheck_compact_launch(&compact, &compact_workspace_view, &detail)
          : eval_status;
  const pgaccel_status finish_status =
      compact_status == PGACCEL_OK
          ? pgaccel_spatial_workspace_finish(&compact_workspace_view, &detail)
          : compact_status;
  CHECK("ordered tri-state compaction status",
        finish_status == PGACCEL_OK && detail == PGACCEL_SPATIAL_DETAIL_NONE);
  CHECK("ordered tri-state compaction mask",
        final_mask.to_host() == std::vector<int8_t>({1, -1, -1}));
  CHECK("ordered tri-state compaction indices",
        uncertain_count.to_host() == std::vector<uint64_t>({1}) &&
            uncertain_indices.to_host() == std::vector<uint64_t>({1, 99, 99}));

  DeviceLane invalid_lane = make_lane({point(0, 0)});
  const uint8_t invalid_null = 2;
  CHECK("sticky failure setup",
        pgaccel_expr_device_copy_from_host(invalid_lane.nulls.get(), &invalid_null,
                                           sizeof(invalid_null)) == PGACCEL_OK);
  const DeviceLane valid_lane = make_lane({point(1, 1)});
  DeviceBuffer<int8_t> invalid_tri_state(std::vector<int8_t>{77});
  DeviceBuffer<int8_t> sticky_mask(std::vector<int8_t>{55});
  DeviceBuffer<uint64_t> sticky_indices(std::vector<uint64_t>{91});
  DeviceBuffer<uint64_t> sticky_count(std::vector<uint64_t>{88});
  const pgaccel_spatial_resident_request invalid_eval =
      make_eval_request(invalid_lane, true, valid_lane, true, 1, invalid_tri_state);
  pgaccel_spatial_recheck_compact_request sticky_compact{};
  sticky_compact.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  sticky_compact.tri_state = invalid_tri_state.get();
  sticky_compact.tri_state_bytes = sizeof(int8_t);
  sticky_compact.final_mask = sticky_mask.get();
  sticky_compact.final_mask_bytes = sizeof(int8_t);
  sticky_compact.uncertain_indices = sticky_indices.get();
  sticky_compact.uncertain_indices_bytes = sizeof(uint64_t);
  sticky_compact.uncertain_count = sticky_count.get();
  sticky_compact.uncertain_count_bytes = sizeof(uint64_t);
  sticky_compact.row_count = 1;
  sticky_compact.uncertain_capacity = 1;
  DeviceWorkspace sticky_workspace;
  const pgaccel_spatial_workspace sticky_workspace_view = sticky_workspace.view();
  detail = -1;
  const pgaccel_status invalid_eval_status =
      pgaccel_spatial_eval_resident_launch(&invalid_eval, &sticky_workspace_view, &detail);
  const pgaccel_status sticky_compact_status =
      invalid_eval_status == PGACCEL_OK
          ? pgaccel_spatial_recheck_compact_launch(&sticky_compact, &sticky_workspace_view, &detail)
          : invalid_eval_status;
  const pgaccel_status sticky_finish_status =
      sticky_compact_status == PGACCEL_OK
          ? pgaccel_spatial_workspace_finish(&sticky_workspace_view, &detail)
          : sticky_compact_status;
  CHECK("evaluation failure remains sticky through compaction",
        sticky_finish_status == PGACCEL_INVALID_ARGUMENT &&
            detail == PGACCEL_SPATIAL_DETAIL_GEOMETRY);
  CHECK("sticky evaluation failure suppresses every compaction write",
        invalid_tri_state.to_host() == std::vector<int8_t>({77}) &&
            sticky_mask.to_host() == std::vector<int8_t>({55}) &&
            sticky_indices.to_host() == std::vector<uint64_t>({91}) &&
            sticky_count.to_host() == std::vector<uint64_t>({88}));

  DeviceBuffer<int8_t> malformed_tri_state(1);
  DeviceBuffer<int8_t> malformed_mask(std::vector<int8_t>{41});
  DeviceBuffer<uint64_t> malformed_indices(std::vector<uint64_t>{42});
  DeviceBuffer<uint64_t> malformed_count(std::vector<uint64_t>{43});
  const pgaccel_spatial_resident_request valid_eval =
      make_eval_request(valid_lane, true, valid_lane, true, 1, malformed_tri_state);
  pgaccel_spatial_recheck_compact_request malformed_compact{};
  malformed_compact.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  malformed_compact.tri_state = malformed_tri_state.get();
  malformed_compact.tri_state_bytes = sizeof(int8_t);
  malformed_compact.final_mask = malformed_mask.get();
  malformed_compact.final_mask_bytes = sizeof(int8_t);
  malformed_compact.uncertain_indices = malformed_indices.get();
  malformed_compact.uncertain_indices_bytes = sizeof(uint64_t);
  malformed_compact.uncertain_count = malformed_count.get();
  malformed_compact.uncertain_count_bytes = sizeof(uint64_t);
  malformed_compact.row_count = 1;
  malformed_compact.uncertain_capacity = 1;
  DeviceWorkspace malformed_workspace;
  const pgaccel_spatial_workspace malformed_workspace_view = malformed_workspace.view();
  detail = -1;
  const pgaccel_status valid_eval_status =
      pgaccel_spatial_eval_resident_launch(&valid_eval, &malformed_workspace_view, &detail);
  const int8_t invalid_tri_state_value = 2;
  CHECK("invalid tri-state setup",
        valid_eval_status == PGACCEL_OK &&
            pgaccel_expr_device_copy_from_host(malformed_tri_state.get(), &invalid_tri_state_value,
                                               sizeof(invalid_tri_state_value)) == PGACCEL_OK);
  const pgaccel_status malformed_compact_status = pgaccel_spatial_recheck_compact_launch(
      &malformed_compact, &malformed_workspace_view, &detail);
  const pgaccel_status malformed_finish_status =
      malformed_compact_status == PGACCEL_OK
          ? pgaccel_spatial_workspace_finish(&malformed_workspace_view, &detail)
          : malformed_compact_status;
  CHECK("invalid tri-state is a typed hard failure",
        malformed_finish_status == PGACCEL_INVALID_ARGUMENT &&
            detail == PGACCEL_SPATIAL_DETAIL_TRISTATE);
  CHECK("invalid tri-state suppresses every compaction write",
        malformed_mask.to_host() == std::vector<int8_t>({41}) &&
            malformed_indices.to_host() == std::vector<uint64_t>({42}) &&
            malformed_count.to_host() == std::vector<uint64_t>({43}));

  std::vector<HostGeometry> boundary_points;
  boundary_points.reserve(PGACCEL_SPATIAL_MAX_CHUNK_ROWS);
  for (size_t row = 0; row < PGACCEL_SPATIAL_MAX_CHUNK_ROWS; ++row)
    boundary_points.push_back(point(0, 1));
  const DeviceLane boundary_lane = make_lane(boundary_points);
  DeviceBuffer<int8_t> boundary_tri_state(PGACCEL_SPATIAL_MAX_CHUNK_ROWS);
  DeviceBuffer<int8_t> boundary_mask(PGACCEL_SPATIAL_MAX_CHUNK_ROWS);
  DeviceBuffer<uint64_t> boundary_indices(PGACCEL_SPATIAL_MAX_CHUNK_ROWS);
  DeviceBuffer<uint64_t> boundary_count(1);
  const pgaccel_spatial_resident_request boundary_eval = make_eval_request(
      boundary_lane, false, square, true, PGACCEL_SPATIAL_MAX_CHUNK_ROWS, boundary_tri_state);
  pgaccel_spatial_recheck_compact_request boundary_compact{};
  boundary_compact.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  boundary_compact.tri_state = boundary_tri_state.get();
  boundary_compact.tri_state_bytes = boundary_tri_state.size() * sizeof(int8_t);
  boundary_compact.final_mask = boundary_mask.get();
  boundary_compact.final_mask_bytes = boundary_mask.size() * sizeof(int8_t);
  boundary_compact.uncertain_indices = boundary_indices.get();
  boundary_compact.uncertain_indices_bytes = boundary_indices.size() * sizeof(uint64_t);
  boundary_compact.uncertain_count = boundary_count.get();
  boundary_compact.uncertain_count_bytes = sizeof(uint64_t);
  boundary_compact.row_count = PGACCEL_SPATIAL_MAX_CHUNK_ROWS;
  boundary_compact.uncertain_capacity = PGACCEL_SPATIAL_MAX_CHUNK_ROWS;
  DeviceWorkspace boundary_workspace;
  const pgaccel_spatial_workspace boundary_workspace_view = boundary_workspace.view();
  detail = -1;
  const pgaccel_status boundary_eval_status =
      pgaccel_spatial_eval_resident_launch(&boundary_eval, &boundary_workspace_view, &detail);
  const pgaccel_status boundary_compact_status =
      boundary_eval_status == PGACCEL_OK ? pgaccel_spatial_recheck_compact_launch(
                                               &boundary_compact, &boundary_workspace_view, &detail)
                                         : boundary_eval_status;
  const pgaccel_status boundary_finish_status =
      boundary_compact_status == PGACCEL_OK
          ? pgaccel_spatial_workspace_finish(&boundary_workspace_view, &detail)
          : boundary_compact_status;
  const std::vector<uint64_t> boundary_indices_host = boundary_indices.to_host();
  bool ordered_boundary = boundary_indices_host.size() == PGACCEL_SPATIAL_MAX_CHUNK_ROWS;
  for (size_t row = 0; ordered_boundary && row < boundary_indices_host.size(); ++row)
    ordered_boundary = boundary_indices_host[row] == row;
  CHECK("all-uncertain exact-boundary compaction status",
        boundary_finish_status == PGACCEL_OK && detail == PGACCEL_SPATIAL_DETAIL_NONE &&
            boundary_count.to_host() == std::vector<uint64_t>({PGACCEL_SPATIAL_MAX_CHUNK_ROWS}));
  CHECK("all-uncertain exact-boundary indices are strictly ordered", ordered_boundary);

  pgaccel_spatial_recheck_compact_request overlimit_compact = boundary_compact;
  overlimit_compact.row_count = PGACCEL_SPATIAL_MAX_CHUNK_ROWS + 1;
  overlimit_compact.uncertain_capacity = PGACCEL_SPATIAL_MAX_CHUNK_ROWS + 1;
  overlimit_compact.tri_state_bytes = overlimit_compact.row_count;
  overlimit_compact.final_mask_bytes = overlimit_compact.row_count;
  overlimit_compact.uncertain_indices_bytes =
      overlimit_compact.uncertain_capacity * sizeof(uint64_t);
  detail = -1;
  CHECK("compaction rejects an over-limit row count before dispatch",
        pgaccel_spatial_recheck_compact_launch(&overlimit_compact, &boundary_workspace_view,
                                               &detail) == PGACCEL_INVALID_ARGUMENT &&
            detail == PGACCEL_SPATIAL_DETAIL_CONTRACT);

  pgaccel_spatial_recheck_compact_request inexact_capacity = compact;
  inexact_capacity.uncertain_capacity = 2;
  inexact_capacity.uncertain_indices_bytes = 2 * sizeof(uint64_t);
  detail = -1;
  CHECK("compaction requires exact worst-case uncertainty capacity",
        pgaccel_spatial_recheck_compact_launch(&inexact_capacity, &compact_workspace_view,
                                               &detail) == PGACCEL_INVALID_ARGUMENT &&
            detail == PGACCEL_SPATIAL_DETAIL_CONTRACT);

  pgaccel_spatial_recheck_compact_request short_span = compact;
  short_span.tri_state_bytes = 2;
  detail = -1;
  CHECK("compaction rejects a short declared span",
        pgaccel_spatial_recheck_compact_launch(&short_span, &compact_workspace_view, &detail) ==
                PGACCEL_INVALID_ARGUMENT &&
            detail == PGACCEL_SPATIAL_DETAIL_CONTRACT);

  pgaccel_spatial_recheck_compact_request aliased_compact = compact;
  aliased_compact.final_mask = tri_state.get();
  detail = -1;
  CHECK("compaction rejects aliased input and output spans",
        pgaccel_spatial_recheck_compact_launch(&aliased_compact, &compact_workspace_view,
                                               &detail) == PGACCEL_INVALID_ARGUMENT &&
            detail == PGACCEL_SPATIAL_DETAIL_CONTRACT);

  DeviceBuffer<uint64_t> patch_indices(std::vector<uint64_t>{1, 3});
  DeviceBuffer<int8_t> patch_results(std::vector<int8_t>{1, -1});
  DeviceBuffer<int8_t> patch_mask(std::vector<int8_t>{-1, -1, 1, 1});
  pgaccel_spatial_recheck_patch_request patch{};
  patch.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  patch.indices = patch_indices.get();
  patch.indices_bytes = patch_indices.size() * sizeof(uint64_t);
  patch.results = patch_results.get();
  patch.results_bytes = patch_results.size() * sizeof(int8_t);
  patch.final_mask = patch_mask.get();
  patch.final_mask_bytes = patch_mask.size() * sizeof(int8_t);
  patch.row_count = 4;
  patch.patch_count = 2;
  DeviceWorkspace patch_workspace;
  const pgaccel_spatial_workspace patch_workspace_view = patch_workspace.view();
  detail = -1;
  const pgaccel_status patch_launch_status =
      pgaccel_spatial_recheck_patch_launch(&patch, &patch_workspace_view, &detail);
  const pgaccel_status patch_finish_status =
      patch_launch_status == PGACCEL_OK
          ? pgaccel_spatial_workspace_finish(&patch_workspace_view, &detail)
          : patch_launch_status;
  CHECK("ordered exact-result patch status",
        patch_finish_status == PGACCEL_OK && detail == PGACCEL_SPATIAL_DETAIL_NONE);
  CHECK("ordered exact-result patch mask",
        patch_mask.to_host() == std::vector<int8_t>({-1, 1, 1, -1}));

  DeviceBuffer<uint64_t> duplicate_indices(std::vector<uint64_t>{1, 1});
  DeviceBuffer<int8_t> duplicate_results(std::vector<int8_t>{1, -1});
  DeviceBuffer<int8_t> duplicate_mask(std::vector<int8_t>{4, 4, 4});
  pgaccel_spatial_recheck_patch_request duplicate_patch{};
  duplicate_patch.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  duplicate_patch.indices = duplicate_indices.get();
  duplicate_patch.indices_bytes = duplicate_indices.size() * sizeof(uint64_t);
  duplicate_patch.results = duplicate_results.get();
  duplicate_patch.results_bytes = duplicate_results.size() * sizeof(int8_t);
  duplicate_patch.final_mask = duplicate_mask.get();
  duplicate_patch.final_mask_bytes = duplicate_mask.size() * sizeof(int8_t);
  duplicate_patch.row_count = 3;
  duplicate_patch.patch_count = 2;
  DeviceWorkspace duplicate_workspace;
  const pgaccel_spatial_workspace duplicate_workspace_view = duplicate_workspace.view();
  detail = -1;
  const pgaccel_status duplicate_launch_status =
      pgaccel_spatial_recheck_patch_launch(&duplicate_patch, &duplicate_workspace_view, &detail);
  const pgaccel_status duplicate_finish_status =
      duplicate_launch_status == PGACCEL_OK
          ? pgaccel_spatial_workspace_finish(&duplicate_workspace_view, &detail)
          : duplicate_launch_status;
  CHECK("duplicate patch index is a typed hard failure",
        duplicate_finish_status == PGACCEL_INVALID_ARGUMENT &&
            detail == PGACCEL_SPATIAL_DETAIL_RECHECK_INDEX &&
            duplicate_mask.to_host() == std::vector<int8_t>({4, 4, 4}));

  DeviceBuffer<uint64_t> invalid_result_indices(std::vector<uint64_t>{1});
  DeviceBuffer<int8_t> invalid_results(std::vector<int8_t>{0});
  DeviceBuffer<int8_t> invalid_result_mask(std::vector<int8_t>{7, 7});
  pgaccel_spatial_recheck_patch_request invalid_result_patch{};
  invalid_result_patch.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  invalid_result_patch.indices = invalid_result_indices.get();
  invalid_result_patch.indices_bytes = sizeof(uint64_t);
  invalid_result_patch.results = invalid_results.get();
  invalid_result_patch.results_bytes = sizeof(int8_t);
  invalid_result_patch.final_mask = invalid_result_mask.get();
  invalid_result_patch.final_mask_bytes = 2 * sizeof(int8_t);
  invalid_result_patch.row_count = 2;
  invalid_result_patch.patch_count = 1;
  DeviceWorkspace invalid_result_workspace;
  const pgaccel_spatial_workspace invalid_result_workspace_view = invalid_result_workspace.view();
  detail = -1;
  const pgaccel_status invalid_result_launch_status = pgaccel_spatial_recheck_patch_launch(
      &invalid_result_patch, &invalid_result_workspace_view, &detail);
  const pgaccel_status invalid_result_finish_status =
      invalid_result_launch_status == PGACCEL_OK
          ? pgaccel_spatial_workspace_finish(&invalid_result_workspace_view, &detail)
          : invalid_result_launch_status;
  CHECK("invalid exact-result patch is a typed hard failure",
        invalid_result_finish_status == PGACCEL_INVALID_ARGUMENT &&
            detail == PGACCEL_SPATIAL_DETAIL_RECHECK_PATCH &&
            invalid_result_mask.to_host() == std::vector<int8_t>({7, 7}));

  pgaccel_spatial_recheck_patch_request overlimit_patch = patch;
  overlimit_patch.row_count = PGACCEL_SPATIAL_MAX_CHUNK_ROWS + 1;
  detail = -1;
  CHECK("patch rejects an over-limit row count before dispatch",
        pgaccel_spatial_recheck_patch_launch(&overlimit_patch, &patch_workspace_view, &detail) ==
                PGACCEL_INVALID_ARGUMENT &&
            detail == PGACCEL_SPATIAL_DETAIL_CONTRACT);

  pgaccel_spatial_recheck_patch_request short_patch = patch;
  short_patch.indices_bytes = sizeof(uint64_t);
  detail = -1;
  CHECK("patch rejects a short declared span",
        pgaccel_spatial_recheck_patch_launch(&short_patch, &patch_workspace_view, &detail) ==
                PGACCEL_INVALID_ARGUMENT &&
            detail == PGACCEL_SPATIAL_DETAIL_CONTRACT);

  pgaccel_spatial_recheck_patch_request aliased_patch = patch;
  aliased_patch.results = reinterpret_cast<const int8_t*>(patch_indices.get());
  detail = -1;
  CHECK("patch rejects aliased input spans",
        pgaccel_spatial_recheck_patch_launch(&aliased_patch, &patch_workspace_view, &detail) ==
                PGACCEL_INVALID_ARGUMENT &&
            detail == PGACCEL_SPATIAL_DETAIL_CONTRACT);
}

void test_large_rows_and_vertices() {
  constexpr size_t row_count = PGACCEL_SPATIAL_MAX_CHUNK_ROWS;
  std::vector<HostGeometry> points;
  points.reserve(row_count);
  for (size_t index = 0; index < row_count; ++index)
    points.push_back((index & 1) == 0 ? point(0.5, 0.5) : point(2.0, 2.0));
  const DeviceLane point_lane = make_lane(points);
  const DeviceLane square = make_lane({polygon({0, 0, 1, 0, 1, 1, 0, 1, 0, 0})});
  pgaccel_reset_gpu_exec_count();
  const PredicateRun run =
      run_predicate(point_lane, false, square, true, row_count,
                    PGACCEL_SPATIAL_PREDICATE_INTERSECTS, 0.0, 32 * 1024 * 1024);
  CHECK("maximum chunk row status", run.status == PGACCEL_OK && run.results.size() == row_count);
  bool correct = run.results.size() == row_count;
  for (size_t index = 0; correct && index < run.results.size(); ++index)
    correct = run.results[index] == ((index & 1) == 0 ? 1 : -1);
  CHECK("maximum chunk row classification", correct);
  CHECK("maximum chunk row call records one dispatch", pgaccel_gpu_exec_count() == 1);

  DeviceBuffer<int8_t> overlimit_output(1);
  pgaccel_spatial_resident_request overlimit{};
  overlimit.abi_version = PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION;
  overlimit.predicate = PGACCEL_SPATIAL_PREDICATE_INTERSECTS;
  overlimit.count = PGACCEL_SPATIAL_MAX_CHUNK_ROWS + 1;
  overlimit.max_referenced_bytes = 1 << 20;
  overlimit.left = {square.view(), 0, 0};
  overlimit.right = {square.view(), 0, 0};
  overlimit.predicate_results = overlimit_output.get();
  overlimit.predicate_results_bytes = overlimit.count;
  overlimit.output_capacity = overlimit.count;
  int32_t overlimit_detail = -1;
  pgaccel_reset_gpu_exec_count();
  CHECK("over-limit row count is rejected before dispatch",
        run_resident_request(&overlimit, &overlimit_detail) == PGACCEL_INVALID_ARGUMENT &&
            overlimit_detail == PGACCEL_SPATIAL_DETAIL_CONTRACT && pgaccel_gpu_exec_count() == 0);

  constexpr size_t vertices = 4096;
  std::vector<double> ring((vertices + 1) * 2);
  constexpr double pi = 3.1415926535897932384626433832795;
  for (size_t index = 0; index < vertices; ++index) {
    const double angle = 2.0 * pi * static_cast<double>(index) / vertices;
    ring[index * 2] = std::cos(angle);
    ring[index * 2 + 1] = std::sin(angle);
  }
  ring[vertices * 2] = ring[0];
  ring[vertices * 2 + 1] = ring[1];
  const DeviceLane large_polygon =
      make_lane({{PGACCEL_RESIDENT_GEOMETRY_POLYGON, std::move(ring), {0}, 4326, false}});
  const DeviceLane probes = make_lane({point(0, 0), point(2, 2)});
  check_results(
      "4096 vertex polygon",
      run_predicate(probes, false, large_polygon, true, 2, PGACCEL_SPATIAL_PREDICATE_INTERSECTS),
      {1, -1});
}

}  // namespace

int main() {
  if (pgaccel_init() != PGACCEL_OK) {
    std::fprintf(stderr, "pgaccel_init failed\n");
    return 1;
  }
  test_intersects_pair_matrix();
  test_holes_boundaries_and_predicates();
  test_distance();
  test_hard_failures();
  test_recheck_helpers();
  test_large_rows_and_vertices();
  CHECK("shutdown", pgaccel_shutdown() == PGACCEL_OK);
  std::printf("test_spatial_resident: %d passed, %d failed\n", passed, failed);
  return failed == 0 ? 0 : 1;
}
