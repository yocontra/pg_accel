#!/usr/bin/env python3
"""Post-qualification, single-backend NUMERIC latency probe; never clears cache.

Use libpq PGHOST/PGPORT/PGUSER/PGDATABASE to select the owned scratch server.
Run only after the existing qualification runner is terminal and GPU is free.
This records native-call HOST wall time, not GPU duration or proved JIT cost.
Requires psycopg2, already used by the corpus importer live tests.
"""

import argparse
import json
import os
import pathlib
import secrets
import stat
import time

import psycopg2
import psycopg2.extras


def emit(label, value):
    print(json.dumps({"event": label, "value": value}, default=str), flush=True)


def snapshot(directory):
    if not directory.is_dir():
        return {"directory_exists": False, "files": {}}
    files = {}
    for path in directory.iterdir():
        if path.suffix not in {".metalar", ".metallib", ".jit"}:
            continue
        try:
            stat = path.stat()
        except FileNotFoundError:
            continue  # Concurrent cache publication remains visible in later snapshots.
        files[path.name] = {"bytes": stat.st_size, "mtime_ns": stat.st_mtime_ns}
    return {"directory_exists": True, "files": files}


def trace_gate(backend_pid, ready, go, timeout_seconds):
    """Publish an exclusive nonce/PID signal and wait for its matching release."""
    if not 0 < timeout_seconds <= 300:
        raise ValueError("trace timeout must be greater than zero and at most 300s")
    paths = []
    for path in (ready, go):
        if not path.is_absolute():
            raise ValueError("trace signals must use absolute owned task paths")
        parent = path.parent.resolve(strict=True)
        parent_stat = parent.stat()
        if parent_stat.st_uid != os.getuid() or parent_stat.st_mode & 0o022:
            raise ValueError("trace signal directory must be owned and not group/world writable")
        resolved = parent / path.name
        if os.path.lexists(resolved):
            raise FileExistsError(f"refusing existing trace signal: {resolved}")
        paths.append(resolved)
    ready, go = paths
    if ready == go:
        raise ValueError("trace ready and go paths must differ")
    signal = {"backend_pid": backend_pid, "nonce": secrets.token_hex(16)}
    with ready.open("x", encoding="utf-8") as stream:
        os.chmod(ready, 0o600)
        json.dump(signal, stream)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    emit("trace_ready", {**signal, "ready_file": str(ready), "go_file": str(go)})
    deadline = time.monotonic() + timeout_seconds
    while time.monotonic() < deadline:
        try:
            descriptor = os.open(go, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        except FileNotFoundError:
            time.sleep(min(0.1, max(0, deadline - time.monotonic())))
            continue
        with os.fdopen(descriptor, "r", encoding="utf-8") as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_size > 4096:
                raise ValueError("trace go signal must be a small owned regular file")
            if json.loads(stream.read(4097)) != signal:
                raise ValueError("trace go signal does not match this backend and nonce")
        emit("trace_released", signal)
        return
    raise TimeoutError("trace attach/release deadline expired; no measured query executed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache-dir", required=True, type=pathlib.Path)
    parser.add_argument("--qualification-finished", action="store_true")
    parser.add_argument("--trace-ready-file", type=pathlib.Path)
    parser.add_argument("--trace-go-file", type=pathlib.Path)
    parser.add_argument("--trace-timeout-seconds", type=float, default=60)
    args = parser.parse_args()
    if not args.qualification_finished:
        parser.error("coordinate with the verifier; run only after qualification finishes")
    if bool(args.trace_ready_file) != bool(args.trace_go_file):
        parser.error("trace ready and go files must be supplied together")
    if not 0 < args.trace_timeout_seconds <= 300:
        parser.error("trace timeout must be greater than zero and at most 300s")

    connection = psycopg2.connect("")
    connection.autocommit = True
    warnings = []
    try:
        with connection.cursor(cursor_factory=psycopg2.extras.RealDictCursor) as cur:
            cur.execute("SELECT pg_backend_pid() pid, version() version")
            backend = cur.fetchone()
            emit("backend", backend)
            cur.execute("SELECT name, setting, unit FROM pg_settings WHERE name LIKE 'pg_accel.%' ORDER BY name")
            emit("settings_before", cur.fetchall())
            cur.execute("SELECT setting, unit FROM pg_settings WHERE name='pg_accel.kernel_timeout_ms'")
            timeout = cur.fetchone()
            if timeout is None or timeout["unit"] != "ms" or int(timeout["setting"]) != 5000:
                raise RuntimeError("refusing probe: warning threshold must remain 5000ms")
            cur.execute("SELECT * FROM pg_accel_device_limits()")
            limits = cur.fetchall()
            emit("device_limits", limits)
            by_name = {row["name"]: row["value"] for row in limits}
            for name in ("gpu_reduce_max_chunk", "gpu_grouped_agg_one_shot_max_rows"):
                if int(by_name[name]) != 1_000_000:
                    raise RuntimeError(f"refusing unmatched device limit: {name}")
            cur.execute("SET pg_accel.auto_load=off; SET pg_accel.gpu_enabled=on; SET pg_accel.execution_profiling=on; SET pg_accel.enabled=off")
            cur.execute("""
                CREATE TEMP TABLE _numeric_agg AS
                SELECT i id,
                  CASE WHEN i%97=0 THEN NULL ELSE i%5 END g,
                  CASE WHEN i%101=0 THEN NULL ELSE date '2026-01-01'+i%7 END h,
                  (CASE WHEN i%103=0 THEN NULL ELSE 'r-'||i%3 END) COLLATE "C" k,
                  CASE WHEN i%11=0 THEN NULL ELSE (i%10001-5000)::numeric/100 END v,
                  CASE WHEN i%13=0 THEN NULL ELSE (i%7919-3959)::numeric/1000 END w,
                  CASE WHEN i%17=0 THEN NULL ELSE (i%60001-30000)::int2 END qty,
                  CASE WHEN i%29=0 THEN NULL ELSE i%2=0 END b
                FROM generate_series(1,1000017) s(i)
            """)
            cur.execute("ANALYZE _numeric_agg")
            cur.execute("SELECT sum(v) s FROM _numeric_agg")
            expected = cur.fetchone()["s"]
            emit("stock_sum", expected)
            cur.execute("SELECT pg_accel_pin('_numeric_agg'::regclass, ARRAY['id','g','h','k','v','w','qty','b'])")
            emit("pin", cur.fetchall())
            cur.execute("SET pg_accel.enabled=on")
            # Planning only: do not pre-execute or accidentally warm the measured query.
            cur.execute("EXPLAIN (COSTS OFF) SELECT sum(v) s FROM _numeric_agg")
            emit("plan_before", cur.fetchall())
            emit("setup_notices", list(connection.notices))
            connection.notices.clear()
            if args.trace_ready_file:
                trace_gate(backend["pid"], args.trace_ready_file, args.trace_go_file,
                           args.trace_timeout_seconds)
            for attempt in range(1, 4):
                cur.execute("SELECT pg_accel_reset_stats()")
                # Native execution counter is monotonic, not reset by reset_stats().
                cur.execute("SELECT pg_accel_kernel_executions() executions")
                executions_before = cur.fetchone()["executions"]
                emit(f"query_{attempt}_kernel_executions_before", executions_before)
                emit(f"cache_before_{attempt}", snapshot(args.cache_dir))
                started = time.perf_counter_ns()
                cur.execute("CREATE TEMP TABLE _numeric_actual AS SELECT sum(v) s FROM _numeric_agg")
                elapsed_us = (time.perf_counter_ns() - started) // 1000
                emit(f"query_{attempt}_host_wall_us", elapsed_us)
                notices = list(connection.notices)
                connection.notices.clear()
                warnings.extend(notice for notice in notices if "WARNING:" in notice)
                emit(f"query_{attempt}_notices", notices)
                # Read stats immediately; result validation and DROP are outside timing.
                cur.execute("SELECT * FROM pg_accel_descriptor_stage_stats()")
                emit(f"query_{attempt}_descriptor_stages", cur.fetchall())
                cur.execute("SELECT * FROM pg_accel_stats()")
                stats = cur.fetchall()
                emit(f"query_{attempt}_stats", stats)
                emit(f"cache_after_{attempt}", snapshot(args.cache_dir))
                cur.execute("SELECT s FROM _numeric_actual")
                actual = cur.fetchone()["s"]
                emit(f"query_{attempt}_sum", actual)
                if actual != expected:
                    raise RuntimeError("NUMERIC result differs from stock PostgreSQL")
                if stats[0]["gpu_kernel_executions"] <= executions_before:
                    raise RuntimeError("measured query did not execute a GPU kernel")
                cur.execute("DROP TABLE _numeric_actual")
            # This is a fourth execution, identified separately; records actual mode/batches.
            cur.execute("EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) SELECT sum(v) s FROM _numeric_agg")
            emit("post_measurement_explain_analyze", cur.fetchall())
            final_notices = list(connection.notices)
            emit("post_measurement_notices", final_notices)
            warnings.extend(notice for notice in final_notices if "WARNING:" in notice)
            emit("warning_count", len(warnings))
            emit("interpretation", "host wall/native-call timings only; cache deltas and first-repeat differences do not directly measure GPU time or isolate JIT causality")
            return 1 if warnings else 0
    finally:
        connection.close()  # Drops only this backend's temporary fixtures.


if __name__ == "__main__":
    raise SystemExit(main())
