/*
 * hash_join.cpp — hash join C API stub.
 *
 * GpuHashJoin is not planner-selectable until a real GPU-resident build/probe
 * implementation exists. The public symbols remain so Rust and tests can link,
 * but the API fails closed instead of exposing CPU or host-pointer debug paths.
 */

#include "pgaccel_hash_join.h"

struct pgaccel_hash_table {};

extern "C" {

pgaccel_hash_table* pgaccel_hash_join_build(const void* keys, const uint8_t* null_mask,
                                            const uint32_t* indices, size_t count,
                                            pgaccel_key_type key_type) {
  (void)keys;
  (void)null_mask;
  (void)indices;
  (void)count;
  (void)key_type;
  return nullptr;
}

void pgaccel_hash_join_free(pgaccel_hash_table* ht) {
  (void)ht;
}

pgaccel_status pgaccel_hash_join_probe(const pgaccel_hash_table* ht, const void* outer_keys,
                                       const uint8_t* outer_null_mask, size_t outer_count,
                                       uint32_t* match_pairs, size_t max_matches,
                                       size_t* match_count) {
  (void)ht;
  (void)outer_keys;
  (void)outer_null_mask;
  (void)outer_count;
  (void)match_pairs;
  (void)max_matches;
  if (match_count != nullptr) {
    *match_count = 0;
  }
  return PGACCEL_UNSUPPORTED;
}

}  // extern "C"
