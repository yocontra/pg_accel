typedef unsigned long long uint64_t;
typedef enum { PGACCEL_OK = 0, PGACCEL_ERROR = 1 } pgaccel_status;
struct pgaccel_join_rows_request { uint64_t rows; const uint64_t* values; };
struct pgaccel_join_control { uint64_t limit; };
extern "C" pgaccel_status pgaccel_join_rows_workspace_bytes(
    const pgaccel_join_rows_request* r, uint64_t* out) {
  *out = r->rows * 8; return PGACCEL_OK;
}
extern "C" pgaccel_status pgaccel_join_rows_controlled_workspace_bytes(
    const pgaccel_join_rows_request* r, const pgaccel_join_control* c, uint64_t* out) {
  *out = r->rows * 8 + c->limit; return PGACCEL_OK;
}
