// These exports operate only on bounded descriptors and pointer-address spans.
// Kept separate from SYCL execution so their entire implementation can be built
// and tested without linking a GPU runtime.
#include "join_rows_layout.hpp"

using namespace pgaccel_join_detail;

extern "C" pgaccel_status pgaccel_join_rows_workspace_bytes(
    const pgaccel_join_rows_request* request, uint64_t* bytes) {
  if (request == nullptr || bytes == nullptr || !aligned(request, alignof(pgaccel_join_rows_request)) ||
      !aligned(bytes, alignof(uint64_t)))
    return PGACCEL_INVALID_ARGUMENT;
  Span input, output;
  if (!span(request, sizeof(*request), &input) || !span(bytes, sizeof(*bytes), &output) || overlaps(input, output))
    return PGACCEL_INVALID_ARGUMENT;
  Layout layout;
  const auto status = layout_for(*request, &layout);
  if (status != PGACCEL_OK)
    return status;
  for (size_t key = 0; key < PGACCEL_JOIN_ROWS_MAX_KEYS; ++key) {
    if (key >= request->key_count)
      break;
    if (!query_key_aliases(request->outer_keys[key], output) ||
        !query_key_aliases(request->inner_keys[key], output))
      return PGACCEL_INVALID_ARGUMENT;
  }
  *bytes = layout.bytes;
  return PGACCEL_OK;
}

extern "C" pgaccel_status pgaccel_join_rows_controlled_workspace_bytes(
    const pgaccel_join_rows_request* request, const pgaccel_join_control* control,
    uint64_t* bytes) {
  if (request == nullptr || control == nullptr || bytes == nullptr ||
      !aligned(request, alignof(pgaccel_join_rows_request)) ||
      !aligned(control, alignof(pgaccel_join_control)) || !aligned(bytes, alignof(uint64_t)))
    return PGACCEL_INVALID_ARGUMENT;
  Layout layout;
  const auto status = controlled_layout(*request, *control, &layout);
  if (status != PGACCEL_OK)
    return status;
  if (!controlled_query_aliases(request, control, bytes))
    return PGACCEL_INVALID_ARGUMENT;
  *bytes = layout.bytes;
  return PGACCEL_OK;
}
