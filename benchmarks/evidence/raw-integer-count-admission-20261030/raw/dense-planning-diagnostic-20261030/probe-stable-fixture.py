#!/usr/bin/env python3
"""One bounded diagnostic, not a replacement performance admission gate."""
import argparse
import ctypes as C
import json
from pathlib import Path
import time

QUERY = 'SELECT k, count(*) AS n FROM bench_raw_integer_count GROUP BY k'
ORDER = [True, False, False, True, False, True, True, False, True, False]
SETTINGS = {
    'work_mem': '512MB', 'effective_cache_size': '48GB',
    'max_parallel_workers_per_gather': '8', 'max_parallel_workers': '12',
    'maintenance_work_mem': '2GB', 'jit': 'off', 'jit_above_cost': '100000', 'random_page_cost': '1.1',
    'effective_io_concurrency': '256', 'track_io_timing': 'on',
    'parallel_leader_participation': 'on', 'statement_timeout': '5min',
    'lock_timeout': '10s', 'pg_accel.planner_profiling': 'on',
    'pg_accel.execution_profiling': 'on',
}


def emit(event, **value):
    print(json.dumps({'event': event, **value}, sort_keys=True), flush=True)


class DB:
    def __init__(self, pq, connection):
        self.pq = pq
        self.handle = pq.PQconnectdb(connection.encode())
        if not self.handle or pq.PQstatus(self.handle) != 0:
            if self.handle:
                pq.PQfinish(self.handle)
            raise RuntimeError('libpq connection failed')

    def query(self, sql):
        result = self.pq.PQexec(self.handle, sql.encode())
        if not result:
            raise RuntimeError('PQexec returned NULL')
        try:
            if self.pq.PQresultStatus(result) not in (1, 2):
                raise RuntimeError(self.pq.PQresultErrorMessage(result).decode())
            return [[None if self.pq.PQgetisnull(result, row, col) else
                     self.pq.PQgetvalue(result, row, col).decode()
                     for col in range(self.pq.PQnfields(result))]
                    for row in range(self.pq.PQntuples(result))]
        finally:
            self.pq.PQclear(result)

    def close(self):
        self.pq.PQfinish(self.handle)


def bind(path):
    pq = C.CDLL(path)
    for name, result, args in [
        ('PQconnectdb', C.c_void_p, [C.c_char_p]),
        ('PQstatus', C.c_int, [C.c_void_p]),
        ('PQexec', C.c_void_p, [C.c_void_p, C.c_char_p]),
        ('PQresultStatus', C.c_int, [C.c_void_p]),
        ('PQresultErrorMessage', C.c_char_p, [C.c_void_p]),
        ('PQntuples', C.c_int, [C.c_void_p]),
        ('PQnfields', C.c_int, [C.c_void_p]),
        ('PQgetisnull', C.c_int, [C.c_void_p, C.c_int, C.c_int]),
        ('PQgetvalue', C.c_char_p, [C.c_void_p, C.c_int, C.c_int]),
        ('PQclear', None, [C.c_void_p]), ('PQfinish', None, [C.c_void_p]),
    ]:
        fn = getattr(pq, name)
        fn.restype, fn.argtypes = result, args
    return pq


def canonical(rows):
    return sorted(rows, key=lambda row: (row[0] is None, row[0] or ''))


def snapshot(db):
    return json.loads(db.query('EXECUTE diagnostic_snapshot')[0][0])


def delta(before, after):
    if isinstance(before, dict):
        assert before.keys() == after.keys()
        return {key: delta(before[key], after[key]) for key in before}
    assert isinstance(before, int) and isinstance(after, int) and after >= before
    return after - before


def measure(db, prepared, expected, phase, index, sql_type):
    before = snapshot(db)
    started = time.perf_counter_ns()
    rows = db.query('EXECUTE dense_target' if prepared else QUERY)
    elapsed_ns = time.perf_counter_ns() - started
    after = snapshot(db)
    diff = delta(before, after)
    emit('sample', sql_type=sql_type, phase=phase, index=index,
         mode='prepared' if prepared else 'direct', elapsed_ns=elapsed_ns,
         before=before, after=after, delta=diff, rows=rows)
    assert canonical(rows) == expected and len(rows) == 65, 'full native parity'
    assert diff['stats']['gpu_kernel_executions'] == 4, 'four native lifecycle calls'
    assert diff['stats']['stock_exec_count'] == 0, 'no stock fallback'
    assert diff['modes']['parallel_dense_count'] == 3, 'three data-bearing dense chunks'
    assert sum(diff['modes'].values()) == 3, 'no alternate physical kernel mode'
    assert diff['artifact']['builds'] == diff['artifact']['rebuilds'] == 0
    assert diff['artifact']['raw_load_us'] == 0, 'warm resident artifact'
    assert diff['prepared']['custom'] == 0
    if prepared:
        assert diff['prepared']['generic'] == 1, 'cached generic execution'
        assert sum(v['calls'] for v in diff['planner'].values()) == 0, 'no replanning'
    else:
        assert diff['planner']['upper_group_agg']['calls'] > 0, 'direct query replanned'


def setup_snapshot(db):
    db.query("""PREPARE diagnostic_snapshot AS SELECT json_build_object(
      'stats', (SELECT row_to_json(s) FROM pg_accel_stats() s),
      'planner', (SELECT json_object_agg(stage, json_build_object('calls',calls,'us',elapsed_us)) FROM pg_accel_planner_stage_stats()),
      'descriptor', (SELECT json_object_agg(stage, json_build_object('calls',calls,'us',elapsed_us)) FROM pg_accel_descriptor_stage_stats()),
      'modes', (SELECT json_object_agg(mode,calls) FROM pg_accel_grouped_kernel_mode_stats()),
      'artifact', (SELECT row_to_json(s) FROM pg_accel_artifact_lifecycle_stats() s),
      'runtime', (SELECT row_to_json(s) FROM pg_accel_grouped_runtime_stats() s),
      'prepared', (SELECT json_build_object('generic',COALESCE(max(generic_plans),0),'custom',COALESCE(max(custom_plans),0)) FROM pg_prepared_statements WHERE name='dense_target'))""")
    snapshot(db)
    first = snapshot(db)
    second = snapshot(db)
    assert first == second, 'counter observer must be cached and nonperturbing'


def corpus_state(native):
    # Fixed-width INT/BOOL fixture has no TOAST relation. Flush setup-backend
    # counters before reading them; only this backend ever writes the fixture.
    native.query('SELECT pg_stat_force_next_flush()')
    native.query('SELECT pg_stat_clear_snapshot()')
    state = native.query("""SELECT c.oid,c.relfilenode,c.reltoastrelid,c.reloptions::text,
      s.n_tup_ins,s.n_tup_upd,s.n_tup_del,s.n_tup_hot_upd,s.autovacuum_count,s.autoanalyze_count
      FROM pg_class c JOIN pg_stat_all_tables s ON s.relid=c.oid
      WHERE c.oid='bench_raw_integer_count'::regclass""")
    assert len(state) == 1
    assert state[0][2] == '0', 'fixed-width fixture must have no TOAST relation'
    assert state[0][3] == '{autovacuum_enabled=false}'
    assert state[0][4:] == ['2000017','0','0','0','0','0'], 'no DML or autonomous maintenance'
    return state


def run_cell(pq, connection, sql_type):
    conns = []
    try:
        for _ in range(3):
            conns.append(DB(pq, connection))
        native, direct, prepared = conns
        for db in conns:
            for name, value in SETTINGS.items():
                db.query(f"SET {name} = '{value}'")
            for name, value in SETTINGS.items():
                assert db.query('SHOW ' + name)[0][0] == value, f'observed setting {name}'
        native.query('SET pg_accel.enabled = off')
        native.query(f'CREATE TABLE bench_raw_integer_count (k {sql_type}, value boolean, qualifies boolean) WITH (autovacuum_enabled=false)')
        native.query(f"""INSERT INTO bench_raw_integer_count
          SELECT CASE WHEN i % 97 = 0 THEN NULL::{sql_type} ELSE (g-32)::{sql_type} END,
          CASE WHEN i % 7 = 0 THEN NULL::boolean ELSE i % 2 = 0 END,
          CASE WHEN g = 0 THEN false WHEN i % 11 = 0 THEN NULL::boolean ELSE i % 3 <> 0 END
          FROM (SELECT i, CASE WHEN i > 64 AND i % 5 = 0 THEN 0 ELSE (i-1) % 64 END AS g
          FROM generate_series(1::bigint,2000017) AS s(i)) AS fixture""")
        native.query('ANALYZE bench_raw_integer_count')
        setup_snapshot(native)
        before = snapshot(native)
        expected = canonical(native.query(QUERY))
        diff = delta(before, snapshot(native))
        assert diff['stats']['gpu_kernel_executions'] == diff['stats']['stock_exec_count'] == 0
        assert len(expected) == 65 and sum(int(row[1]) for row in expected) == 2000017
        emit('native_oracle', sql_type=sql_type, rows=expected, delta=diff)
        for db, is_prepared in ((direct, False), (prepared, True)):
            settings = db.query("SELECT name,setting,unit FROM pg_settings WHERE name LIKE 'pg_accel.%' OR name IN ('shared_buffers','work_mem','effective_cache_size','max_parallel_workers_per_gather','max_worker_processes','max_parallel_workers','maintenance_work_mem','jit','jit_above_cost','random_page_cost','effective_io_concurrency','track_io_timing','parallel_leader_participation','plan_cache_mode') ORDER BY name")
            observed = {row[0]: row[1] for row in settings}
            assert observed['pg_accel.enabled'] == observed['pg_accel.gpu_enabled'] == 'on'
            assert observed['pg_accel.cost_multiplier'] == '1' and observed['pg_accel.min_batch_size'] == '65536'
            assert observed['plan_cache_mode'] == 'auto'
            assert observed['shared_buffers'] == '2097152' and observed['max_worker_processes'] == '16'
            emit('settings', sql_type=sql_type, prepared=is_prepared, rows=settings)
            assert db.query("SELECT pg_accel_pin('bench_raw_integer_count',ARRAY['k'])")[0][0] == '2000017'
            if is_prepared:
                db.query('PREPARE dense_target AS ' + QUERY)
            # Separate instrumented proof, outside every timed/counter sample.
            explain = db.query('EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF) ' + ('EXECUTE dense_target' if is_prepared else QUERY))
            text = '\n'.join(row[0] for row in explain)
            emit('plan', sql_type=sql_type, prepared=is_prepared, rows=explain)
            assert 'GPU Physical Kernel Mode: parallel_dense_count' in text and 'GPU Kernel Dispatched: true' in text
            setup_snapshot(db)
        corpus_before = corpus_state(native)
        emit('corpus_before', sql_type=sql_type, state=corpus_before)
        for phase, schedule in [('warmup', ORDER[:5]), ('measured', ORDER)]:
            for index, direct_first in enumerate(schedule):
                emit('pair', sql_type=sql_type, phase=phase, index=index, direct_first=direct_first)
                modes = [(direct, False), (prepared, True)]
                for db, is_prepared in modes if direct_first else reversed(modes):
                    measure(db, is_prepared, expected, phase, index, sql_type)
        corpus_after = corpus_state(native)
        emit('corpus_after', sql_type=sql_type, state=corpus_after)
        assert corpus_after == corpus_before, 'immutable fixture maintenance/DML state'
    finally:
        for db in conns:
            db.close()
    cleanup = DB(pq, connection)
    try:
        cleanup.query('DROP TABLE bench_raw_integer_count')
    finally:
        cleanup.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--connection', required=True)
    parser.add_argument('--libpq', required=True)
    parser.add_argument('--arm', choices=('old', 'new'), required=True)
    parser.add_argument('--reference-device-log', required=True)
    args = parser.parse_args()
    pq = bind(args.libpq)
    # Compare common physical limits, not identical full inventories: the new
    # module intentionally adds one declared reference-device coefficient.
    expected = {}
    for line in Path(args.reference_device_log).read_text().splitlines():
        columns = [part.strip() for part in line.split('|')]
        if len(columns) == 3 and columns[2] == 'hardware_derived':
            expected[columns[0]] = columns[1:]
    assert len(expected) == 76, 'frozen baseline physical-limit inventory'
    preflight = DB(pq, args.connection)
    try:
        actual = {row[0]: row[1:] for row in preflight.query('SELECT name,value,source FROM pg_accel_device_limits()')}
        coefficient = actual.pop('gpu_dense_count_serial_work_cost', None)
        assert actual == expected, 'common physical limits match original ABBA baseline'
        assert coefficient == (['0.06788047049882354', 'reference_device_calibrated'] if args.arm == 'new' else None)
        device = preflight.query('SELECT cpu_cores,configured_workers,gpu_available,gpu_device_name,memory_model,pg_version,pg_accel_version FROM pg_accel_device_info()')
        assert device == [['12', '0', 't', 'Apple M2 Max', 'explicit_device', '180004', '1.0.0-rc1']]
        emit('physical_preflight', common_limits=actual, allowed_new_coefficient=coefficient, device=device)
    finally:
        preflight.close()
    for sql_type in ('int2', 'int8'):
        run_cell(pq, args.connection, sql_type)
    emit('complete', status='PASS', interpretation='diagnostic host wall and stage timings, not device duration or gate requalification')


if __name__ == '__main__':
    main()
