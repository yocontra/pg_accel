/*
 * pgaccel_hash_join.h — hash join diagnostic types and API.
 *
 * Normal GpuHashJoin planning must wait for a real GPU build/probe
 * implementation or GPU-resident hash-table reuse. This API returns
 * unsupported/null when no GPU implementation is available; it does not run
 * a CPU hash join fallback.
 * NULL keys are excluded from the build side (SQL: NULL = NULL is not TRUE).
 *
 * Three-result model per probe:
 *   match_count > 0  — definite matches found
 *   match_count == 0 — no match (hash miss or all NULLs)
 *
 * The hash table is built from the inner relation and probed with
 * outer relation keys. Both build and probe operate on arrays of
 * pre-extracted join keys (int32, int64, or float64).
 */

#ifndef PGACCEL_HASH_JOIN_H
#define PGACCEL_HASH_JOIN_H

#include "pgaccel_ffi.h"

#ifdef __cplusplus
extern "C" {
#endif

/* ── Key type tags ──────────────────────────────────────────────── */

typedef enum {
  PGACCEL_KEY_INT32 = 0,
  PGACCEL_KEY_INT64 = 1,
  PGACCEL_KEY_FLOAT64 = 2,
  /* Slot 3 is reserved on the Rust planner side for CompositeInt4x2
   * (two int4 columns packed into one int8). The composite key is
   * unpacked to int8 by the executor before kernel dispatch, so the
   * kernel never sees key_type == 3. Keep slot 3 empty here so the
   * Rust↔C ABI alignment stays one-to-one for every key type the
   * kernel actually receives. */
  PGACCEL_KEY_UUID = 4, /* 16-byte UUID, host order */
  /* 24-byte canonical INET key:
   *   byte 0      = family (PGSQL_AF_INET=2 or PGSQL_AF_INET6=3)
   *   byte 1      = bits   (netmask, 0-128)
   *   bytes 2-17  = ipaddr (16 bytes; IPv4 zero-padded after the
   *                          first 4 address bytes)
   *   bytes 18-23 = zero padding (u64 alignment)
   * 24 bytes = 3 * uint64_t so read_key_u64 can hash via three
   * hash64() mixes XORed together. CIDR is the same payload
   * shape — registered separately on the Rust side via the
   * CIDROID classifier arm. */
  PGACCEL_KEY_INET = 5,
} pgaccel_key_type;

/* ── Hash table handle ──────────────────────────────────────────── */

/// Opaque handle to a GPU hash table.
typedef struct pgaccel_hash_table pgaccel_hash_table;

/* ── Build API ──────────────────────────────────────────────────── */

/// Build a hash table from inner relation keys.
///
/// Returns NULL when no real GPU build/probe implementation is available for
/// the requested shape.
///
/// `keys` points to an array of `count` values of the specified type.
/// `null_mask[i] == 1` means key[i] is NULL (excluded from table).
/// `indices` is an array of original row indices (0-based) — stored
/// alongside keys so probe results can map back to inner tuples.
///
/// Returns a hash table handle, or NULL on failure.
pgaccel_hash_table*
pgaccel_hash_join_build(const void* keys, const uint8_t* null_mask, /* [count] 1=null, 0=non-null */
                        const uint32_t* indices,                    /* [count] original row index */
                        size_t count, pgaccel_key_type key_type);

/// Free a hash table built by pgaccel_hash_join_build.
void pgaccel_hash_join_free(pgaccel_hash_table* ht);

/* ── Probe API ──────────────────────────────────────────────────── */

/// Probe the hash table with outer relation keys.
///
/// For each outer key, finds matching inner row indices.
/// Results are written as (outer_idx, inner_idx) pairs into
/// `match_pairs` (caller-allocated, capacity = `max_matches * 2`).
/// `max_matches * 2` must fit in size_t; impossible capacities are
/// rejected before any output writes.
///
/// `outer_null_mask[i] == 1` means outer key is NULL (no match).
///
/// Returns PGACCEL_OK on success. `match_count` receives the number
/// of matching pairs found.
pgaccel_status pgaccel_hash_join_probe(const pgaccel_hash_table* ht, const void* outer_keys,
                                       const uint8_t* outer_null_mask, size_t outer_count,
                                       uint32_t* match_pairs, /* [max_matches*2] output */
                                       size_t max_matches,
                                       size_t* match_count /* output: actual matches */
);

#ifdef __cplusplus
}
#endif

#endif /* PGACCEL_HASH_JOIN_H */
