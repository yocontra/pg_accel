//! Independent-backend residency concurrency proof.
//!
//! The live test is opt-in through `integration_tests`. It owns eight libpq
//! connections at once, so PostgreSQL executes every lane in a distinct backend
//! process with a backend-local GPU residency store. Hermetic tests below cover
//! the exact budget arithmetic and bounded log-delta audit without opening a
//! database connection.

#[cfg(feature = "integration_tests")]
use std::collections::BTreeSet;
#[cfg(feature = "integration_tests")]
use std::fs;
#[cfg(feature = "integration_tests")]
use std::path::{Path, PathBuf};
#[cfg(feature = "integration_tests")]
use std::sync::{Arc, mpsc};
#[cfg(feature = "integration_tests")]
use std::thread;
#[cfg(feature = "integration_tests")]
use std::time::{Duration, Instant};

#[cfg(feature = "integration_tests")]
use postgres::fallible_iterator::FallibleIterator;
#[cfg(feature = "integration_tests")]
use postgres::{CancelToken, Client, NoTls};
#[cfg(feature = "integration_tests")]
use serde::Serialize;

#[cfg(feature = "integration_tests")]
use crate::artifacts::{
    ArtifactWriter, append_pgdata_log_candidates, default_log_candidates, default_run_dir,
};
#[cfg(feature = "integration_tests")]
use crate::integration_connection::{live_pg_test_lock, test_connection};

#[cfg(any(test, feature = "integration_tests"))]
const MIB: u64 = 1024 * 1024;
#[cfg(any(test, feature = "integration_tests"))]
const FATAL_LOG_PATTERNS: &[&str] = &[
    "pgaccel panic",
    "panicked at",
    "segmentation fault",
    "terminated by signal",
    "server closed the connection unexpectedly",
    "terminating connection due to crash",
    "resource leak",
    "leaked resource",
    "kernel failure",
    "metal command buffer failed",
    "mtlcompilerservice",
    "cuda error",
    "cudaerror",
];
#[cfg(feature = "integration_tests")]
const BACKEND_COUNT: usize = 8;
#[cfg(feature = "integration_tests")]
const SOAK_ITERATIONS: usize = 20;
#[cfg(feature = "integration_tests")]
const FIXTURE: &str = "pg_accel_resident_concurrency";
#[cfg(feature = "integration_tests")]
const CANCEL_FIXTURE: &str = "pg_accel_resident_user_cancel";
#[cfg(feature = "integration_tests")]
const APPLICATION_PREFIX: &str = "pg_accel_resident_concurrency_";
#[cfg(feature = "integration_tests")]
const QUERY_TAG: &str = "pg_accel_resident_soak";
#[cfg(feature = "integration_tests")]
const QUERY: &str = "/* pg_accel_resident_soak */ \
                     SELECT grp, SUM(measure), MIN(measure), MAX(measure), COUNT(*) \
                     FROM pg_accel_resident_concurrency \
                     GROUP BY grp \
                     ORDER BY grp";
#[cfg(feature = "integration_tests")]
const CANCEL_BASE_QUERY: &str = "SELECT grp, SUM(measure), MIN(measure), MAX(measure), COUNT(*) \
                                FROM pg_accel_resident_user_cancel \
                                GROUP BY grp \
                                ORDER BY grp";
#[cfg(feature = "integration_tests")]
const CANCEL_QUERY_TAG: &str = "pg_accel_real_user_cancel";
#[cfg(feature = "integration_tests")]
// The 256-branch control hit the 10-minute statement timeout at 605.27s on Metal;
// 64 retains a multi-minute deterministic cancel window while keeping the control bounded.
const CANCEL_QUERY_BRANCHES: usize = 64;
#[cfg(feature = "integration_tests")]
const SOAK_BARRIER_KEY: i64 = 0x5047_4131_305F_5632;
#[cfg(feature = "integration_tests")]
const OPERATION_TIMEOUT: Duration = Duration::from_mins(10);
#[cfg(feature = "integration_tests")]
const EXIT_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(feature = "integration_tests")]
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(feature = "integration_tests")]
const GRACEFUL_JOIN_TIMEOUT: Duration = Duration::from_secs(1);
#[cfg(feature = "integration_tests")]
const CANCEL_JOIN_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(feature = "integration_tests")]
const MONITOR_CANCEL_GRACE: Duration = Duration::from_secs(2);

#[cfg(any(test, feature = "integration_tests"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "integration_tests", derive(Serialize))]
struct MonotonicInterval {
    start_us: u64,
    end_us: u64,
}

#[cfg(any(test, feature = "integration_tests"))]
fn strict_common_overlap(intervals: &[MonotonicInterval]) -> Option<MonotonicInterval> {
    let start_us = intervals.iter().map(|interval| interval.start_us).max()?;
    let end_us = intervals.iter().map(|interval| interval.end_us).min()?;
    (start_us < end_us).then_some(MonotonicInterval { start_us, end_us })
}

#[cfg(any(test, feature = "integration_tests"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CancellationClass {
    QueryCanceled,
    OtherSqlState,
    NotDatabaseError,
}

#[cfg(any(test, feature = "integration_tests"))]
fn classify_cancellation_code(code: Option<&str>) -> CancellationClass {
    match code {
        Some("57014") => CancellationClass::QueryCanceled,
        Some(_) => CancellationClass::OtherSqlState,
        None => CancellationClass::NotDatabaseError,
    }
}

#[cfg(any(test, feature = "integration_tests"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExactBudget {
    mib: i32,
    bytes: u64,
    required_bytes: u64,
    spare_bytes: u64,
}

/// Convert an exact N-backend resident footprint to the integer-MiB GUC while
/// retaining a strict proof that another backend's raw copy cannot fit.
#[cfg(any(test, feature = "integration_tests"))]
fn exact_backend_budget(
    baseline_bytes: u64,
    backend_bytes: u64,
    backend_raw_bytes: u64,
    backend_count: usize,
) -> Result<ExactBudget, String> {
    if backend_count == 0 {
        return Err("resident concurrency budget needs at least one backend".to_owned());
    }
    if backend_bytes == 0 || backend_raw_bytes == 0 || backend_raw_bytes > backend_bytes {
        return Err(format!(
            "invalid calibrated resident charge: raw={backend_raw_bytes}, total={backend_bytes}"
        ));
    }
    let backend_count = u64::try_from(backend_count)
        .map_err(|error| format!("resident backend count does not fit u64: {error}"))?;
    let workers = backend_bytes
        .checked_mul(backend_count)
        .ok_or_else(|| "resident worker charge overflow".to_owned())?;
    let required_bytes = baseline_bytes
        .checked_add(workers)
        .ok_or_else(|| "resident cluster charge overflow".to_owned())?;
    let rounded = required_bytes
        .checked_add(MIB - 1)
        .ok_or_else(|| "resident MiB rounding overflow".to_owned())?;
    let mib_u64 = rounded / MIB;
    if mib_u64 == 0 || mib_u64 > 1_048_576 {
        return Err(format!(
            "resident concurrency budget {mib_u64} MiB is outside the GUC range"
        ));
    }
    let bytes = mib_u64
        .checked_mul(MIB)
        .ok_or_else(|| "resident rounded budget overflow".to_owned())?;
    let spare_bytes = bytes
        .checked_sub(required_bytes)
        .ok_or_else(|| "rounded resident budget is below the required charge".to_owned())?;
    if spare_bytes >= backend_raw_bytes {
        return Err(format!(
            "fixture raw charge {backend_raw_bytes} does not exceed MiB rounding slack \
             {spare_bytes}; a ninth backend would not prove budget enforcement"
        ));
    }
    let mib = i32::try_from(mib_u64).map_err(|error| {
        format!("resident concurrency budget does not fit the GUC type: {error}")
    })?;
    Ok(ExactBudget {
        mib,
        bytes,
        required_bytes,
        spare_bytes,
    })
}

#[cfg(any(test, feature = "integration_tests"))]
fn artifact_delta_body(contents: &str) -> Result<&str, String> {
    contents
        .split_once("---\n")
        .map(|(_, body)| body)
        .ok_or_else(|| "log delta artifact has no header delimiter".to_owned())
}

#[cfg(any(test, feature = "integration_tests"))]
fn log_delta_failure(source: &str, body: &str) -> Option<String> {
    let source_lower = source.to_ascii_lowercase();
    if source_lower.contains("pg_accel_panic") && !body.trim().is_empty() {
        return Some(format!("pg_accel panic log gained bytes: {source}"));
    }

    let body_lower = body.to_ascii_lowercase();
    FATAL_LOG_PATTERNS.iter().find_map(|pattern| {
        body_lower
            .contains(pattern)
            .then(|| format!("log delta `{source}` contains fatal pattern `{pattern}`"))
    })
}

#[cfg(feature = "integration_tests")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct GroupRow {
    group_key: i32,
    sum: i64,
    min: i32,
    max: i32,
    count: i64,
}

#[cfg(any(test, feature = "integration_tests"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "integration_tests", derive(Serialize))]
struct AccelCounters {
    kernels: i64,
    accelerated: i64,
    stock: i64,
}

#[cfg(feature = "integration_tests")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct LocalStatus {
    rows: i64,
    raw_bytes: i64,
    derived_bytes: i64,
    all_pinned: bool,
}

#[cfg(feature = "integration_tests")]
impl LocalStatus {
    fn total_bytes(self) -> Result<i64, String> {
        self.raw_bytes
            .checked_add(self.derived_bytes)
            .ok_or_else(|| "backend-local resident byte total overflow".to_owned())
    }
}

#[cfg(feature = "integration_tests")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ResidentFingerprint {
    relid: i64,
    columns: Vec<i32>,
    raw_bytes: i64,
    derived_bytes: i64,
    pinned: bool,
    generation: i64,
}

#[cfg(feature = "integration_tests")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DeviceArtifactRecord {
    path: String,
    bytes: u64,
    fnv1a64: u64,
}

#[cfg(feature = "integration_tests")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DeviceArtifactSnapshot {
    root: String,
    records: Vec<DeviceArtifactRecord>,
}

#[cfg(feature = "integration_tests")]
#[derive(Debug, Serialize)]
struct CancellationEvidence {
    postmaster_started_at: String,
    backend_pid: i32,
    fixture_rows: i64,
    repeated_branches: usize,
    full_result_rows: usize,
    full_kernel_dispatches: i64,
    canceled_rows_received: usize,
    canceled_kernel_dispatches: i64,
    canceled_queries_accelerated: i64,
    sqlstate: String,
    activity_observation: ServerActivityObservation,
    plan_fingerprint: u64,
    resident_fingerprint: ResidentFingerprint,
    cluster_bytes: i64,
    device_artifacts: DeviceArtifactSnapshot,
    recovery_kernel_dispatches: i64,
}

#[cfg(feature = "integration_tests")]
struct CancelWorkerContext {
    query: String,
    plan: String,
    expected: Vec<GroupRow>,
    postmaster_started_at: String,
    backend_pid: i32,
    fixture_rows: i64,
    full_result_rows: usize,
    full_kernel_dispatches: i64,
    counters_before: AccelCounters,
    resident_before: ResidentFingerprint,
    cluster_before: i64,
    artifacts_before: DeviceArtifactSnapshot,
}

#[cfg(feature = "integration_tests")]
struct CancelReady {
    first_row: GroupRow,
}

#[cfg(feature = "integration_tests")]
#[derive(Debug, Clone, Serialize)]
struct WorkerSnapshot {
    slot: usize,
    pid: i32,
    raw_bytes: i64,
    derived_bytes: i64,
    local_bytes: i64,
    observed_cluster_bytes: i64,
    kernel_executions: i64,
    queries_accelerated: i64,
    stock_exec_count: i64,
    phase_interval: Option<MonotonicInterval>,
}

#[cfg(feature = "integration_tests")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
enum WorkerPhase {
    Ready,
    Soak,
    PostRelease,
}

#[cfg(feature = "integration_tests")]
enum WorkerCommand {
    Run {
        phase: WorkerPhase,
        iterations: usize,
        start_at: Instant,
        barrier_key: Option<i64>,
    },
    Exit,
}

#[cfg(feature = "integration_tests")]
enum WorkerReport {
    Completed {
        phase: WorkerPhase,
        snapshot: WorkerSnapshot,
    },
    Failed {
        slot: usize,
        detail: String,
    },
}

#[cfg(feature = "integration_tests")]
struct WorkerPool {
    controls: Vec<Option<mpsc::Sender<WorkerCommand>>>,
    handles: Vec<Option<thread::JoinHandle<Result<(), String>>>>,
    cancel_tokens: Vec<Option<CancelToken>>,
    pids: Vec<i32>,
    reports: mpsc::Receiver<WorkerReport>,
    clock_origin: Instant,
}

#[cfg(feature = "integration_tests")]
struct WorkerSpec {
    slot: usize,
    table: String,
    fixture_rows: i64,
    expected: Arc<Vec<GroupRow>>,
    budget_mib: i32,
    pid: i32,
    clock_origin: Instant,
}

#[cfg(feature = "integration_tests")]
#[derive(Debug, Clone, Serialize)]
struct ServerActivityObservation {
    observed_us: u64,
    pids: Vec<i32>,
}

#[cfg(feature = "integration_tests")]
#[derive(Debug, Clone, Serialize)]
struct SoakOverlapEvidence {
    barrier_key: i64,
    barrier_wait: ServerActivityObservation,
    aggregate_active: ServerActivityObservation,
    strict_common_interval: MonotonicInterval,
}

#[cfg(feature = "integration_tests")]
impl WorkerPool {
    fn spawn(
        connection: &str,
        table: &str,
        fixture_rows: i64,
        expected: &[GroupRow],
        budget_mib: i32,
    ) -> Result<Self, String> {
        let (report_tx, reports) = mpsc::channel();
        let expected = Arc::new(expected.to_vec());
        let clock_origin = Instant::now();
        let mut pool = Self {
            controls: Vec::with_capacity(BACKEND_COUNT),
            handles: Vec::with_capacity(BACKEND_COUNT),
            cancel_tokens: Vec::with_capacity(BACKEND_COUNT),
            pids: Vec::with_capacity(BACKEND_COUNT),
            reports,
            clock_origin,
        };
        for slot in 0..BACKEND_COUNT {
            let (control_tx, control_rx) = mpsc::channel();
            let application_name = format!("{APPLICATION_PREFIX}{slot}");
            let mut client = open_named_client(connection, &application_name)?;
            let pid = backend_pid(&mut client)?;
            let cancel_token = client.cancel_token();
            let spec = WorkerSpec {
                slot,
                table: table.to_owned(),
                fixture_rows,
                expected: Arc::clone(&expected),
                budget_mib,
                pid,
                clock_origin,
            };
            let worker_reports = report_tx.clone();
            let handle = thread::Builder::new()
                .name(format!("resident-backend-{slot}"))
                .spawn(move || worker_entry(client, &spec, &control_rx, &worker_reports))
                .map_err(|error| format!("spawn resident backend worker {slot}: {error}"))?;
            pool.controls.push(Some(control_tx));
            pool.handles.push(Some(handle));
            pool.cancel_tokens.push(Some(cancel_token));
            pool.pids.push(pid);
        }
        drop(report_tx);
        Ok(pool)
    }

    fn collect_phase(
        &self,
        expected_phase: WorkerPhase,
        expected_reports: usize,
    ) -> Result<Vec<WorkerSnapshot>, String> {
        let deadline = Instant::now() + OPERATION_TIMEOUT;
        let mut snapshots = Vec::with_capacity(expected_reports);
        while snapshots.len() < expected_reports {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "timed out waiting for {expected_reports} {expected_phase:?} reports; got {}",
                    snapshots.len()
                ));
            }
            let report = self.reports.recv_timeout(remaining).map_err(|error| {
                format!("resident worker report channel ended during {expected_phase:?}: {error}")
            })?;
            match report {
                WorkerReport::Completed { phase, snapshot } if phase == expected_phase => {
                    snapshots.push(snapshot);
                }
                WorkerReport::Completed { phase, snapshot } => {
                    return Err(format!(
                        "worker {} reported phase {phase:?} while {expected_phase:?} was expected",
                        snapshot.slot
                    ));
                }
                WorkerReport::Failed { slot, detail } => {
                    return Err(format!("resident backend worker {slot} failed: {detail}"));
                }
            }
        }
        Ok(snapshots)
    }

    fn run_active(
        &self,
        phase: WorkerPhase,
        iterations: usize,
    ) -> Result<Vec<WorkerSnapshot>, String> {
        let start_at = Instant::now() + Duration::from_millis(100);
        let mut sent = 0;
        for (slot, control) in self.controls.iter().enumerate() {
            let Some(control) = control else {
                continue;
            };
            control
                .send(WorkerCommand::Run {
                    phase,
                    iterations,
                    start_at,
                    barrier_key: None,
                })
                .map_err(|error| format!("send {phase:?} command to worker {slot}: {error}"))?;
            sent += 1;
        }
        self.collect_phase(phase, sent)
    }

    fn run_synchronized_soak(
        &self,
        monitor: &mut Client,
        iterations: usize,
    ) -> Result<(Vec<WorkerSnapshot>, SoakOverlapEvidence), String> {
        let active_slots = self
            .controls
            .iter()
            .enumerate()
            .filter_map(|(slot, control)| control.as_ref().map(|_| slot))
            .collect::<Vec<_>>();
        if active_slots.len() != BACKEND_COUNT {
            return Err(format!(
                "synchronized soak needs {BACKEND_COUNT} live workers, found {}",
                active_slots.len()
            ));
        }
        let pids = active_slots
            .iter()
            .map(|&slot| self.pids[slot])
            .collect::<Vec<_>>();

        monitor
            .query_one("SELECT pg_advisory_lock($1)", &[&SOAK_BARRIER_KEY])
            .map_err(|error| format!("acquire resident soak monitor barrier: {error}"))?;
        let start_at = Instant::now() + Duration::from_millis(100);
        for &slot in &active_slots {
            if let Err(error) = self.controls[slot]
                .as_ref()
                .expect("active worker slot has a control channel")
                .send(WorkerCommand::Run {
                    phase: WorkerPhase::Soak,
                    iterations,
                    start_at,
                    barrier_key: Some(SOAK_BARRIER_KEY),
                })
            {
                let _ = release_soak_barrier(monitor);
                return Err(format!(
                    "send synchronized soak command to worker {slot}: {error}"
                ));
            }
        }

        let barrier_wait = match wait_for_activity_set(
            monitor,
            &pids,
            "pg_advisory_xact_lock_shared",
            true,
            self.clock_origin,
        ) {
            Ok(observation) => observation,
            Err(error) => {
                return match release_soak_barrier(monitor) {
                    Ok(()) => Err(error),
                    Err(unlock) => Err(format!("{error}; barrier release also failed: {unlock}")),
                };
            }
        };
        release_soak_barrier(monitor)?;

        let aggregate_active =
            wait_for_activity_set(monitor, &pids, QUERY_TAG, false, self.clock_origin)?;
        let snapshots = self.collect_phase(WorkerPhase::Soak, active_slots.len())?;
        let intervals = snapshots
            .iter()
            .map(|snapshot| {
                snapshot.phase_interval.ok_or_else(|| {
                    format!(
                        "worker {} omitted its synchronized soak interval",
                        snapshot.slot
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let strict_common_interval = strict_common_overlap(&intervals).ok_or_else(|| {
            format!("eight worker soak intervals do not strictly overlap: {intervals:?}")
        })?;

        Ok((
            snapshots,
            SoakOverlapEvidence {
                barrier_key: SOAK_BARRIER_KEY,
                barrier_wait,
                aggregate_active,
                strict_common_interval,
            },
        ))
    }

    fn stop(&mut self, slot: usize) -> Result<(), String> {
        let control = self
            .controls
            .get_mut(slot)
            .ok_or_else(|| format!("resident worker slot {slot} is out of range"))?
            .take()
            .ok_or_else(|| format!("resident worker slot {slot} is already stopped"))?;
        let send_error = control.send(WorkerCommand::Exit).err();
        let handle = self
            .handles
            .get_mut(slot)
            .and_then(Option::take)
            .ok_or_else(|| format!("resident worker slot {slot} has no join handle"))?;
        let cancel_token = self
            .cancel_tokens
            .get_mut(slot)
            .and_then(Option::take)
            .ok_or_else(|| format!("resident worker slot {slot} has no cancel token"))?;
        let mut cancel_detail = None;
        if !wait_for_worker_finish(&handle, GRACEFUL_JOIN_TIMEOUT) {
            if let Err(error) = cancel_token.cancel_query(NoTls) {
                cancel_detail = Some(format!("cancel request failed: {error}"));
            }
            if !wait_for_worker_finish(&handle, CANCEL_JOIN_TIMEOUT) {
                let pid = self.pids.get(slot).copied().map_or(-1, |pid| pid);
                drop(handle);
                return Err(format!(
                    "resident worker {slot} PID {pid} did not stop within {:?} after cancellation{}",
                    CANCEL_JOIN_TIMEOUT,
                    cancel_detail
                        .as_deref()
                        .map_or_else(String::new, |detail| format!(" ({detail})"))
                ));
            }
        }
        match handle.join() {
            Ok(Ok(())) if cancel_detail.is_none() => send_error.map_or(Ok(()), |error| {
                Err(format!(
                    "resident worker {slot} exited before its stop command: {error}"
                ))
            }),
            Ok(Ok(())) => Err(format!(
                "resident worker {slot} stopped after a failed cancel request: {}",
                cancel_detail.unwrap_or_else(|| "unknown cancel failure".to_owned())
            )),
            Ok(Err(detail)) => Err(format!(
                "resident worker {slot} returned an error: {detail}"
            )),
            Err(payload) => Err(format!(
                "resident worker {slot} panicked: {}",
                panic_payload_text(&payload)
            )),
        }
    }

    fn shutdown_all(&mut self) -> Result<(), String> {
        let mut failures = Vec::new();
        for slot in 0..self.controls.len() {
            if self.controls[slot].is_some()
                && let Err(error) = self.stop(slot)
            {
                failures.push(error);
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

#[cfg(feature = "integration_tests")]
fn release_soak_barrier(monitor: &mut Client) -> Result<(), String> {
    let unlocked = monitor
        .query_one("SELECT pg_advisory_unlock($1)", &[&SOAK_BARRIER_KEY])
        .map_err(|error| format!("release resident soak monitor barrier: {error}"))?
        .try_get::<_, bool>(0)
        .map_err(|error| format!("decode resident soak barrier release: {error}"))?;
    if !unlocked {
        return Err("resident soak monitor did not own the advisory barrier".to_owned());
    }
    Ok(())
}

#[cfg(feature = "integration_tests")]
fn matching_activity_pids(
    monitor: &mut Client,
    pids: &[i32],
    query_needle: &str,
    require_lock_wait: bool,
) -> Result<BTreeSet<i32>, String> {
    if pids.is_empty() {
        return Ok(BTreeSet::new());
    }
    let pid_list = pids
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let lock_clause = if require_lock_wait {
        "AND wait_event_type = 'Lock'"
    } else {
        ""
    };
    monitor
        .query(
            &format!(
                "SELECT pid
                 FROM pg_stat_activity
                 WHERE pid IN ({pid_list})
                   AND state = 'active'
                   AND strpos(query, $1) > 0
                   {lock_clause}
                 ORDER BY pid"
            ),
            &[&query_needle],
        )
        .map_err(|error| format!("read synchronized worker activity: {error}"))?
        .into_iter()
        .map(|row| {
            row.try_get(0)
                .map_err(|error| format!("decode synchronized worker PID: {error}"))
        })
        .collect()
}

#[cfg(feature = "integration_tests")]
fn wait_for_activity_set(
    monitor: &mut Client,
    pids: &[i32],
    query_needle: &str,
    require_lock_wait: bool,
    clock_origin: Instant,
) -> Result<ServerActivityObservation, String> {
    let expected = pids.iter().copied().collect::<BTreeSet<_>>();
    if expected.len() != pids.len() {
        return Err(format!(
            "activity proof has duplicate backend PIDs: {pids:?}"
        ));
    }
    let deadline = Instant::now() + EXIT_TIMEOUT;
    let mut best = BTreeSet::new();
    loop {
        let observed = matching_activity_pids(monitor, pids, query_needle, require_lock_wait)?;
        if observed.len() > best.len() {
            best.clone_from(&observed);
        }
        if observed == expected {
            return Ok(ServerActivityObservation {
                observed_us: clock_offset_us(clock_origin)?,
                pids: observed.into_iter().collect(),
            });
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "never observed all {} backends active for `{query_needle}`; best={best:?}, expected={expected:?}",
                pids.len()
            ));
        }
        thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(feature = "integration_tests")]
impl Drop for WorkerPool {
    fn drop(&mut self) {
        for control in self.controls.iter_mut().filter_map(Option::take) {
            let _ = control.send(WorkerCommand::Exit);
        }
        wait_for_worker_set(&self.handles, GRACEFUL_JOIN_TIMEOUT);
        for (slot, handle) in self.handles.iter().enumerate() {
            if handle.as_ref().is_some_and(|handle| !handle.is_finished())
                && let Some(cancel_token) = self.cancel_tokens.get_mut(slot).and_then(Option::take)
            {
                let _ = cancel_token.cancel_query(NoTls);
            }
        }
        wait_for_worker_set(&self.handles, CANCEL_JOIN_TIMEOUT);
        for handle in self.handles.iter_mut().filter_map(Option::take) {
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                drop(handle);
            }
        }
    }
}

#[cfg(feature = "integration_tests")]
fn wait_for_worker_finish<T>(
    handle: &thread::JoinHandle<Result<T, String>>,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while !handle.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    handle.is_finished()
}

#[cfg(feature = "integration_tests")]
fn wait_for_worker_set(
    handles: &[Option<thread::JoinHandle<Result<(), String>>>],
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    while handles.iter().flatten().any(|handle| !handle.is_finished()) && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(feature = "integration_tests")]
fn panic_payload_text(payload: &Box<dyn std::any::Any + Send>) -> &str {
    payload
        .downcast_ref::<&'static str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

#[cfg(feature = "integration_tests")]
fn clock_offset_us(clock_origin: Instant) -> Result<u64, String> {
    u64::try_from(clock_origin.elapsed().as_micros())
        .map_err(|error| format!("monotonic evidence timestamp does not fit u64: {error}"))
}

#[cfg(feature = "integration_tests")]
fn run_worker_phase(
    client: &mut Client,
    spec: &WorkerSpec,
    phase: WorkerPhase,
    iterations: usize,
    barrier_key: Option<i64>,
) -> Result<MonotonicInterval, String> {
    let run = |client: &mut Client| {
        let start_us = clock_offset_us(spec.clock_origin)?;
        run_exact_iterations(client, QUERY, &spec.expected, iterations, spec.slot, phase)?;
        let end_us = clock_offset_us(spec.clock_origin)?;
        if start_us >= end_us {
            return Err(format!(
                "worker {} {phase:?} has non-positive monotonic interval {start_us}..{end_us}",
                spec.slot
            ));
        }
        Ok(MonotonicInterval { start_us, end_us })
    };

    let Some(barrier_key) = barrier_key else {
        return run(client);
    };

    client
        .batch_execute("BEGIN")
        .map_err(|error| format!("begin synchronized {phase:?} worker {}: {error}", spec.slot))?;
    let result = (|| {
        client
            .query_one("SELECT pg_advisory_xact_lock_shared($1)", &[&barrier_key])
            .map_err(|error| {
                format!(
                    "worker {} wait on synchronized {phase:?} barrier: {error}",
                    spec.slot
                )
            })?;
        run(client)
    })();
    match result {
        Ok(interval) => {
            client.batch_execute("COMMIT").map_err(|error| {
                format!(
                    "commit synchronized {phase:?} worker {}: {error}",
                    spec.slot
                )
            })?;
            Ok(interval)
        }
        Err(error) => match client.batch_execute("ROLLBACK") {
            Ok(()) => Err(error),
            Err(rollback) => Err(format!("{error}; rollback also failed: {rollback}")),
        },
    }
}

#[cfg(feature = "integration_tests")]
fn worker_entry(
    client: Client,
    spec: &WorkerSpec,
    controls: &mpsc::Receiver<WorkerCommand>,
    reports: &mpsc::Sender<WorkerReport>,
) -> Result<(), String> {
    let result = worker_loop(client, spec, controls, reports);
    if let Err(detail) = &result {
        let _ = reports.send(WorkerReport::Failed {
            slot: spec.slot,
            detail: detail.clone(),
        });
    }
    result
}

#[cfg(feature = "integration_tests")]
fn worker_loop(
    mut client: Client,
    spec: &WorkerSpec,
    controls: &mpsc::Receiver<WorkerCommand>,
    reports: &mpsc::Sender<WorkerReport>,
) -> Result<(), String> {
    let slot = spec.slot;
    configure_accel_backend(&mut client, slot, spec.budget_mib)?;
    let pid = spec.pid;
    let observed_pid = backend_pid(&mut client)?;
    if observed_pid != pid {
        return Err(format!(
            "worker {slot} connection changed backend PID from {pid} to {observed_pid}"
        ));
    }
    pin_fixture(&mut client, &spec.table, spec.fixture_rows)?;
    require_descriptor_plan(&mut client, QUERY)?;
    run_exact_iterations(
        &mut client,
        QUERY,
        &spec.expected,
        1,
        slot,
        WorkerPhase::Ready,
    )?;
    reports
        .send(WorkerReport::Completed {
            phase: WorkerPhase::Ready,
            snapshot: worker_snapshot(&mut client, slot, pid, None)?,
        })
        .map_err(|error| format!("send ready report from worker {slot}: {error}"))?;

    loop {
        match controls
            .recv()
            .map_err(|error| format!("worker {slot} control channel ended: {error}"))?
        {
            WorkerCommand::Run {
                phase,
                iterations,
                start_at,
                barrier_key,
            } => {
                let delay = start_at.saturating_duration_since(Instant::now());
                if !delay.is_zero() {
                    thread::sleep(delay);
                }
                let phase_interval =
                    run_worker_phase(&mut client, spec, phase, iterations, barrier_key)?;
                reports
                    .send(WorkerReport::Completed {
                        phase,
                        snapshot: worker_snapshot(&mut client, slot, pid, Some(phase_interval))?,
                    })
                    .map_err(|error| {
                        format!("send {phase:?} report from worker {slot}: {error}")
                    })?;
            }
            WorkerCommand::Exit => {
                drop(client);
                return Ok(());
            }
        }
    }
}

#[cfg(feature = "integration_tests")]
fn open_client(connection: &str) -> Result<Client, String> {
    open_client_with_application(connection, None)
}

#[cfg(feature = "integration_tests")]
fn open_named_client(connection: &str, application_name: &str) -> Result<Client, String> {
    open_client_with_application(connection, Some(application_name))
}

#[cfg(feature = "integration_tests")]
fn open_client_with_application(
    connection: &str,
    application_name: Option<&str>,
) -> Result<Client, String> {
    let mut config = connection
        .parse::<postgres::Config>()
        .map_err(|error| format!("parse resident backend connection: {error}"))?;
    config.connect_timeout(CONNECT_TIMEOUT);
    if let Some(application_name) = application_name {
        config.application_name(application_name);
    }
    config
        .connect(NoTls)
        .map_err(|error| format!("connect independent resident backend: {error}"))
}

#[cfg(feature = "integration_tests")]
fn configure_accel_backend(
    client: &mut Client,
    slot: usize,
    budget_mib: i32,
) -> Result<(), String> {
    client
        .batch_execute(&format!(
            "SET statement_timeout = '10min';
             SET lock_timeout = '30s';
             SET pg_accel.enabled = on;
             SET pg_accel.gpu_enabled = on;
             SET pg_accel.auto_load = on;
             SET pg_accel.resident_memory_budget_mb = {budget_mib};
             SELECT pg_accel_reset_stats();"
        ))
        .map_err(|error| format!("configure resident backend {slot}: {error}"))?;
    let settings = client
        .query_one(
            "SELECT current_setting('application_name'),
                    current_setting('pg_accel.cost_multiplier')::float8",
            &[],
        )
        .map_err(|error| format!("read worker {slot} session settings: {error}"))?;
    let observed_application = settings
        .try_get::<_, String>(0)
        .map_err(|error| format!("decode worker {slot} application name: {error}"))?;
    let expected_application = format!("{APPLICATION_PREFIX}{slot}");
    if observed_application != expected_application {
        return Err(format!(
            "worker {slot} has application_name `{observed_application}`, expected \
             `{expected_application}`"
        ));
    }
    let cost_multiplier = settings
        .try_get::<_, f64>(1)
        .map_err(|error| format!("decode worker {slot} cost multiplier: {error}"))?;
    if (cost_multiplier - 1.0).abs() > f64::EPSILON {
        return Err(format!(
            "worker {slot} must use the documented cost multiplier 1.0, observed {cost_multiplier}"
        ));
    }
    Ok(())
}

#[cfg(feature = "integration_tests")]
fn backend_pid(client: &mut Client) -> Result<i32, String> {
    client
        .query_one("SELECT pg_backend_pid()", &[])
        .map_err(|error| format!("read backend PID: {error}"))?
        .try_get(0)
        .map_err(|error| format!("decode backend PID: {error}"))
}

#[cfg(feature = "integration_tests")]
fn pin_fixture(client: &mut Client, table: &str, fixture_rows: i64) -> Result<(), String> {
    let row = client
        .query_one(
            "SELECT pg_accel_pin($1::text::regclass, ARRAY['grp', 'measure'])",
            &[&table],
        )
        .map_err(|error| format!("pin resident concurrency fixture: {error}"))?;
    let pinned_rows = row
        .try_get::<_, i64>(0)
        .map_err(|error| format!("decode pg_accel_pin row count: {error}"))?;
    if pinned_rows != fixture_rows {
        return Err(format!(
            "pg_accel_pin loaded {pinned_rows} rows, expected {fixture_rows}"
        ));
    }
    Ok(())
}

#[cfg(feature = "integration_tests")]
fn explain_text(client: &mut Client, sql: &str) -> Result<String, String> {
    let rows = client
        .query(&format!("EXPLAIN (VERBOSE, COSTS OFF) {sql}"), &[])
        .map_err(|error| format!("explain resident grouped aggregate: {error}"))?;
    let mut plan = String::new();
    for row in rows {
        let line = row
            .try_get::<_, String>(0)
            .map_err(|error| format!("decode EXPLAIN row: {error}"))?;
        plan.push_str(&line);
        plan.push('\n');
    }
    Ok(plan.to_ascii_lowercase())
}

#[cfg(feature = "integration_tests")]
fn require_descriptor_plan(client: &mut Client, sql: &str) -> Result<(), String> {
    let plan = explain_text(client, sql)?;
    for needle in [
        "custom scan (gpuaccelagg)",
        "strategy: gpuagg",
        "gpu descriptor strategy: descriptor_grouped_aggregate",
    ] {
        if !plan.contains(needle) {
            return Err(format!(
                "resident concurrency plan is missing `{needle}`; fallback is forbidden:\n{plan}"
            ));
        }
    }
    Ok(())
}

#[cfg(feature = "integration_tests")]
fn grouped_rows(client: &mut Client, sql: &str) -> Result<Vec<GroupRow>, String> {
    client
        .query(sql, &[])
        .map_err(|error| format!("execute resident grouped aggregate: {error}"))?
        .into_iter()
        .map(|row| {
            Ok(GroupRow {
                group_key: row
                    .try_get(0)
                    .map_err(|error| format!("decode group key: {error}"))?,
                sum: row
                    .try_get(1)
                    .map_err(|error| format!("decode group sum: {error}"))?,
                min: row
                    .try_get(2)
                    .map_err(|error| format!("decode group min: {error}"))?,
                max: row
                    .try_get(3)
                    .map_err(|error| format!("decode group max: {error}"))?,
                count: row
                    .try_get(4)
                    .map_err(|error| format!("decode group count: {error}"))?,
            })
        })
        .collect()
}

#[cfg(feature = "integration_tests")]
fn group_row_at(row: &postgres::Row, offset: usize) -> Result<GroupRow, String> {
    Ok(GroupRow {
        group_key: row
            .try_get(offset)
            .map_err(|error| format!("decode group key: {error}"))?,
        sum: row
            .try_get(offset + 1)
            .map_err(|error| format!("decode group sum: {error}"))?,
        min: row
            .try_get(offset + 2)
            .map_err(|error| format!("decode group min: {error}"))?,
        max: row
            .try_get(offset + 3)
            .map_err(|error| format!("decode group max: {error}"))?,
        count: row
            .try_get(offset + 4)
            .map_err(|error| format!("decode group count: {error}"))?,
    })
}

#[cfg(any(test, feature = "integration_tests"))]
fn monotonic_delta(after: i64, before: i64, label: &str) -> Result<i64, String> {
    if after < before {
        return Err(format!(
            "{label} counter moved backwards: before={before}, after={after}"
        ));
    }
    after
        .checked_sub(before)
        .ok_or_else(|| format!("{label} counter delta overflowed: before={before}, after={after}"))
}

#[cfg(any(test, feature = "integration_tests"))]
fn counter_delta(after: AccelCounters, before: AccelCounters) -> Result<AccelCounters, String> {
    Ok(AccelCounters {
        kernels: monotonic_delta(after.kernels, before.kernels, "kernel execution")?,
        accelerated: monotonic_delta(after.accelerated, before.accelerated, "accelerated query")?,
        stock: monotonic_delta(after.stock, before.stock, "stock execution")?,
    })
}

#[cfg(feature = "integration_tests")]
fn resident_fingerprint(client: &mut Client, table: &str) -> Result<ResidentFingerprint, String> {
    let row = client
        .query_one(
            "SELECT relid::bigint, columns, raw_bytes, derived_bytes, pinned, generation
             FROM pg_accel_resident_status()
             WHERE relid = $1::text::regclass::oid",
            &[&table],
        )
        .map_err(|error| format!("read resident cancellation fingerprint: {error}"))?;
    Ok(ResidentFingerprint {
        relid: row
            .try_get(0)
            .map_err(|error| format!("decode resident fingerprint relid: {error}"))?,
        columns: row
            .try_get(1)
            .map_err(|error| format!("decode resident fingerprint columns: {error}"))?,
        raw_bytes: row
            .try_get(2)
            .map_err(|error| format!("decode resident fingerprint raw bytes: {error}"))?,
        derived_bytes: row
            .try_get(3)
            .map_err(|error| format!("decode resident fingerprint derived bytes: {error}"))?,
        pinned: row
            .try_get(4)
            .map_err(|error| format!("decode resident fingerprint pin state: {error}"))?,
        generation: row
            .try_get(5)
            .map_err(|error| format!("decode resident fingerprint generation: {error}"))?,
    })
}

#[cfg(feature = "integration_tests")]
fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(feature = "integration_tests")]
fn collect_device_artifacts(
    root: &Path,
    directory: &Path,
    records: &mut Vec<DeviceArtifactRecord>,
) -> Result<(), String> {
    let entries = fs::read_dir(directory).map_err(|error| {
        format!(
            "read AdaptiveCpp artifact directory {}: {error}",
            directory.display()
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "read AdaptiveCpp artifact entry under {}: {error}",
                directory.display()
            )
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("read artifact type for {}: {error}", path.display()))?;
        if file_type.is_dir() {
            collect_device_artifacts(root, &path, records)?;
            continue;
        }
        if !file_type.is_file()
            || !matches!(
                path.extension().and_then(std::ffi::OsStr::to_str),
                Some("jit" | "metallib" | "metalar")
            )
        {
            continue;
        }
        let contents = fs::read(&path)
            .map_err(|error| format!("read device artifact {}: {error}", path.display()))?;
        let relative = path
            .strip_prefix(root)
            .map_err(|error| format!("relativize device artifact {}: {error}", path.display()))?;
        records.push(DeviceArtifactRecord {
            path: relative.to_string_lossy().into_owned(),
            bytes: u64::try_from(contents.len())
                .map_err(|error| format!("device artifact length does not fit u64: {error}"))?,
            fnv1a64: fnv1a64(&contents),
        });
    }
    Ok(())
}

#[cfg(feature = "integration_tests")]
fn device_artifact_snapshot() -> Result<DeviceArtifactSnapshot, String> {
    let root = std::env::var_os("ACPP_APPDB_DIR").map_or_else(
        || {
            std::env::var_os("HOME").map(PathBuf::from).map(|home| {
                home.join(".acpp")
                    .join("apps")
                    .join("global")
                    .join("jit-cache")
            })
        },
        |root| Some(PathBuf::from(root)),
    );
    let root = root.ok_or_else(|| {
        "device artifact proof needs ACPP_APPDB_DIR or HOME to resolve the JIT cache".to_owned()
    })?;
    let mut records = Vec::new();
    if root.is_dir() {
        collect_device_artifacts(&root, &root, &mut records)?;
    }
    records.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(DeviceArtifactSnapshot {
        root: root.display().to_string(),
        records,
    })
}

#[cfg(feature = "integration_tests")]
fn stable_device_artifact_snapshot() -> Result<DeviceArtifactSnapshot, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut previous = device_artifact_snapshot()?;
    loop {
        thread::sleep(Duration::from_millis(25));
        let current = device_artifact_snapshot()?;
        if current == previous {
            return Ok(current);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "AdaptiveCpp device artifacts did not settle within 5s: previous={previous:?}, \
                 current={current:?}"
            ));
        }
        previous = current;
    }
}

#[cfg(feature = "integration_tests")]
fn repeated_cancel_query(branches: usize) -> Result<String, String> {
    if branches < 2 {
        return Err("real cancellation query needs at least two GPU branches".to_owned());
    }
    let branches = (0..branches)
        .map(|branch| {
            format!(
                "SELECT {branch}::int4 AS branch_id, grouped.grp, grouped.sum, grouped.min, \
                 grouped.max, grouped.count \
                 FROM ({CANCEL_BASE_QUERY}) AS grouped(grp, sum, min, max, count)"
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    Ok(format!("/* {CANCEL_QUERY_TAG} */ {branches}"))
}

#[cfg(feature = "integration_tests")]
fn require_repeated_descriptor_plan(plan: &str, branches: usize) -> Result<(), String> {
    for (needle, label) in [
        ("custom scan (gpuaccelagg)", "CustomScan"),
        ("strategy: gpuagg", "GPU strategy"),
        (
            "gpu descriptor strategy: descriptor_grouped_aggregate",
            "descriptor strategy",
        ),
    ] {
        let observed = plan.matches(needle).count();
        if observed != branches {
            return Err(format!(
                "real cancellation plan has {observed} {label} nodes, expected {branches}; \
                 fallback is forbidden:\n{plan}"
            ));
        }
    }
    Ok(())
}

#[cfg(feature = "integration_tests")]
fn validate_repeated_rows(
    rows: &[postgres::Row],
    expected: &[GroupRow],
    branches: usize,
) -> Result<(), String> {
    let expected_len = expected
        .len()
        .checked_mul(branches)
        .ok_or_else(|| "repeated cancellation result row count overflow".to_owned())?;
    if rows.len() != expected_len {
        return Err(format!(
            "full cancellation control returned {} rows, expected {expected_len}",
            rows.len()
        ));
    }
    let mut by_branch = vec![Vec::with_capacity(expected.len()); branches];
    for row in rows {
        let branch = row
            .try_get::<_, i32>(0)
            .map_err(|error| format!("decode cancellation branch id: {error}"))?;
        let branch = usize::try_from(branch)
            .map_err(|error| format!("cancellation branch id is negative: {error}"))?;
        let bucket = by_branch
            .get_mut(branch)
            .ok_or_else(|| format!("cancellation result returned out-of-range branch {branch}"))?;
        bucket.push(group_row_at(row, 1)?);
    }
    for (branch, actual) in by_branch.iter_mut().enumerate() {
        actual.sort_by_key(|row| row.group_key);
        if actual != expected {
            return Err(format!(
                "full cancellation control branch {branch} differs from native PostgreSQL: \
                 expected={expected:?}, actual={actual:?}"
            ));
        }
    }
    Ok(())
}

#[cfg(feature = "integration_tests")]
fn accel_counters(client: &mut Client) -> Result<AccelCounters, String> {
    let row = client
        .query_one(
            "SELECT pg_accel_kernel_executions(), queries_accelerated, stock_exec_count \
             FROM pg_accel_stats()",
            &[],
        )
        .map_err(|error| format!("read resident worker acceleration counters: {error}"))?;
    Ok(AccelCounters {
        kernels: row
            .try_get(0)
            .map_err(|error| format!("decode kernel counter: {error}"))?,
        accelerated: row
            .try_get(1)
            .map_err(|error| format!("decode accelerated-query counter: {error}"))?,
        stock: row
            .try_get(2)
            .map_err(|error| format!("decode stock-exec counter: {error}"))?,
    })
}

#[cfg(feature = "integration_tests")]
fn run_exact_iterations(
    client: &mut Client,
    sql: &str,
    expected: &[GroupRow],
    iterations: usize,
    slot: usize,
    phase: WorkerPhase,
) -> Result<(), String> {
    if iterations == 0 {
        return Err(format!("worker {slot} {phase:?} iteration count is zero"));
    }
    for iteration in 0..iterations {
        let before = accel_counters(client)?;
        let actual = grouped_rows(client, sql)?;
        let after = accel_counters(client)?;
        if actual != expected {
            return Err(format!(
                "worker {slot} {phase:?} iteration {iteration} returned wrong rows: \
                 expected={expected:?}, actual={actual:?}"
            ));
        }
        if after.kernels <= before.kernels {
            return Err(format!(
                "worker {slot} {phase:?} iteration {iteration} did not dispatch a GPU kernel: \
                 before={}, after={}",
                before.kernels, after.kernels
            ));
        }
        if after.accelerated <= before.accelerated {
            return Err(format!(
                "worker {slot} {phase:?} iteration {iteration} did not increment \
                 queries_accelerated: before={}, after={}",
                before.accelerated, after.accelerated
            ));
        }
        if after.stock != 0 {
            return Err(format!(
                "worker {slot} {phase:?} iteration {iteration} used stock execution {} times",
                after.stock
            ));
        }
    }
    Ok(())
}

#[cfg(feature = "integration_tests")]
fn local_status(client: &mut Client) -> Result<LocalStatus, String> {
    let row = client
        .query_one(
            "SELECT COUNT(*)::bigint,
                    COALESCE(SUM(raw_bytes), 0)::bigint,
                    COALESCE(SUM(derived_bytes), 0)::bigint,
                    COALESCE(BOOL_AND(pinned), false)
             FROM pg_accel_resident_status()",
            &[],
        )
        .map_err(|error| format!("read backend-local resident status: {error}"))?;
    Ok(LocalStatus {
        rows: row
            .try_get(0)
            .map_err(|error| format!("decode resident status row count: {error}"))?,
        raw_bytes: row
            .try_get(1)
            .map_err(|error| format!("decode resident raw bytes: {error}"))?,
        derived_bytes: row
            .try_get(2)
            .map_err(|error| format!("decode resident derived bytes: {error}"))?,
        all_pinned: row
            .try_get(3)
            .map_err(|error| format!("decode resident pin state: {error}"))?,
    })
}

#[cfg(feature = "integration_tests")]
fn cluster_bytes(client: &mut Client) -> Result<i64, String> {
    client
        .query_one("SELECT pg_accel_resident_live_bytes()", &[])
        .map_err(|error| format!("read cluster resident byte ledger: {error}"))?
        .try_get(0)
        .map_err(|error| format!("decode cluster resident byte ledger: {error}"))
}

#[cfg(feature = "integration_tests")]
fn worker_snapshot(
    client: &mut Client,
    slot: usize,
    pid: i32,
    phase_interval: Option<MonotonicInterval>,
) -> Result<WorkerSnapshot, String> {
    let status = local_status(client)?;
    if status.rows != 1 || !status.all_pinned || status.raw_bytes <= 0 || status.derived_bytes <= 0
    {
        return Err(format!(
            "worker {slot} PID {pid} has invalid backend-local residency: {status:?}"
        ));
    }
    let counters = accel_counters(client)?;
    if counters.stock != 0 || counters.kernels <= 0 || counters.accelerated <= 0 {
        return Err(format!(
            "worker {slot} PID {pid} has invalid acceleration counters: {counters:?}"
        ));
    }
    Ok(WorkerSnapshot {
        slot,
        pid,
        raw_bytes: status.raw_bytes,
        derived_bytes: status.derived_bytes,
        local_bytes: status.total_bytes()?,
        observed_cluster_bytes: cluster_bytes(client)?,
        kernel_executions: counters.kernels,
        queries_accelerated: counters.accelerated,
        stock_exec_count: counters.stock,
        phase_interval,
    })
}

#[cfg(feature = "integration_tests")]
#[derive(Debug, Clone)]
struct MonitorState {
    postmaster_started_at: String,
    in_recovery: bool,
    cluster_bytes: i64,
    local_status_rows: i64,
}

#[cfg(feature = "integration_tests")]
fn monitor_state(monitor: &mut Client) -> Result<MonitorState, String> {
    let row = monitor
        .query_one(
            "SELECT pg_postmaster_start_time()::text,
                    pg_is_in_recovery(),
                    pg_accel_resident_live_bytes(),
                    (SELECT COUNT(*)::bigint FROM pg_accel_resident_status())",
            &[],
        )
        .map_err(|error| format!("read resident concurrency monitor state: {error}"))?;
    Ok(MonitorState {
        postmaster_started_at: row
            .try_get(0)
            .map_err(|error| format!("decode postmaster start time: {error}"))?,
        in_recovery: row
            .try_get(1)
            .map_err(|error| format!("decode recovery state: {error}"))?,
        cluster_bytes: row
            .try_get(2)
            .map_err(|error| format!("decode monitored cluster bytes: {error}"))?,
        local_status_rows: row
            .try_get(3)
            .map_err(|error| format!("decode monitor local status count: {error}"))?,
    })
}

#[cfg(feature = "integration_tests")]
fn require_monitor_state(
    state: &MonitorState,
    postmaster_started_at: &str,
    expected_cluster_bytes: i64,
) -> Result<(), String> {
    if state.postmaster_started_at != postmaster_started_at {
        return Err(format!(
            "postmaster restarted during resident concurrency proof: before={postmaster_started_at}, \
             after={}",
            state.postmaster_started_at
        ));
    }
    if state.in_recovery {
        return Err(
            "PostgreSQL entered crash recovery during resident concurrency proof".to_owned(),
        );
    }
    if state.local_status_rows != 0 {
        return Err(format!(
            "GPU-idle monitor unexpectedly owns {} resident entries",
            state.local_status_rows
        ));
    }
    if state.cluster_bytes != expected_cluster_bytes {
        return Err(format!(
            "cluster ledger is {} bytes, expected {expected_cluster_bytes}",
            state.cluster_bytes
        ));
    }
    Ok(())
}

#[cfg(feature = "integration_tests")]
fn active_pid_count(monitor: &mut Client, pids: &[i32]) -> Result<i64, String> {
    if pids.is_empty() {
        return Ok(0);
    }
    let pid_list = pids
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    monitor
        .query_one(
            &format!("SELECT COUNT(*)::bigint FROM pg_stat_activity WHERE pid IN ({pid_list})"),
            &[],
        )
        .map_err(|error| format!("read resident worker pg_stat_activity state: {error}"))?
        .try_get(0)
        .map_err(|error| format!("decode resident worker activity count: {error}"))
}

#[cfg(feature = "integration_tests")]
fn tagged_backend_pids(monitor: &mut Client) -> Result<Vec<i32>, String> {
    monitor
        .query(
            "SELECT pid
             FROM pg_stat_activity
             WHERE strpos(application_name, $1) = 1
               AND pid <> pg_backend_pid()
             ORDER BY pid",
            &[&APPLICATION_PREFIX],
        )
        .map_err(|error| format!("read tagged resident test backends: {error}"))?
        .into_iter()
        .map(|row| {
            row.try_get(0)
                .map_err(|error| format!("decode tagged resident backend PID: {error}"))
        })
        .collect()
}

#[cfg(feature = "integration_tests")]
fn signal_tagged_backends(monitor: &mut Client, terminate: bool) -> Result<(), String> {
    let function = if terminate {
        "pg_terminate_backend"
    } else {
        "pg_cancel_backend"
    };
    monitor
        .query(
            &format!(
                "SELECT {function}(pid)
                 FROM pg_stat_activity
                 WHERE strpos(application_name, $1) = 1
                   AND pid <> pg_backend_pid()"
            ),
            &[&APPLICATION_PREFIX],
        )
        .map_err(|error| format!("invoke {function} for resident test backends: {error}"))?;
    Ok(())
}

#[cfg(feature = "integration_tests")]
fn wait_for_tagged_cleanup(
    monitor: &mut Client,
    postmaster_started_at: &str,
    timeout: Duration,
) -> Result<bool, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let state = monitor_state(monitor)?;
        if state.postmaster_started_at != postmaster_started_at || state.in_recovery {
            require_monitor_state(&state, postmaster_started_at, 0)?;
        }
        let pids = tagged_backend_pids(monitor)?;
        if pids.is_empty() && state.cluster_bytes == 0 {
            require_monitor_state(&state, postmaster_started_at, 0)?;
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(feature = "integration_tests")]
fn cleanup_tagged_backends(
    monitor: &mut Client,
    postmaster_started_at: &str,
) -> Result<(), String> {
    if wait_for_tagged_cleanup(monitor, postmaster_started_at, Duration::ZERO)? {
        return Ok(());
    }
    signal_tagged_backends(monitor, false)?;
    if wait_for_tagged_cleanup(monitor, postmaster_started_at, MONITOR_CANCEL_GRACE)? {
        return Ok(());
    }
    signal_tagged_backends(monitor, true)?;
    if wait_for_tagged_cleanup(monitor, postmaster_started_at, EXIT_TIMEOUT)? {
        return Ok(());
    }
    let pids = tagged_backend_pids(monitor)?;
    let live = cluster_bytes(monitor)?;
    Err(format!(
        "tagged resident backends did not clean up after terminate: pids={pids:?}, \
         cluster_bytes={live}"
    ))
}

#[cfg(feature = "integration_tests")]
fn wait_for_backend_exit(
    monitor: &mut Client,
    postmaster_started_at: &str,
    exited_pids: &[i32],
    expected_cluster_bytes: i64,
) -> Result<(), String> {
    let deadline = Instant::now() + EXIT_TIMEOUT;
    loop {
        let state = monitor_state(monitor)?;
        if state.postmaster_started_at != postmaster_started_at || state.in_recovery {
            return require_monitor_state(&state, postmaster_started_at, expected_cluster_bytes);
        }
        let active = active_pid_count(monitor, exited_pids)?;
        if active == 0 && state.cluster_bytes == expected_cluster_bytes {
            return require_monitor_state(&state, postmaster_started_at, expected_cluster_bytes);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "backend cleanup timed out: active={active}, cluster_bytes={}, expected_bytes={}, \
                 exited_pids={exited_pids:?}",
                state.cluster_bytes, expected_cluster_bytes
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(feature = "integration_tests")]
fn create_named_fixture(monitor: &mut Client, fixture: &str) -> Result<i64, String> {
    let minimum_rows = monitor
        .query_one(
            "SELECT value::bigint FROM pg_accel_device_limits() \
             WHERE name = 'gpu_hash_agg_min_rows'",
            &[],
        )
        .map_err(|error| format!("read grouped aggregate minimum rows: {error}"))?
        .try_get::<_, i64>(0)
        .map_err(|error| format!("decode grouped aggregate minimum rows: {error}"))?;
    let fixture_rows = minimum_rows
        .saturating_add((minimum_rows / 4).max(1_024))
        .max(250_000);
    monitor
        .batch_execute(&format!(
            "DROP TABLE IF EXISTS {fixture};
             CREATE UNLOGGED TABLE {fixture} (
                 id int8 PRIMARY KEY,
                 grp int4 NOT NULL,
                 measure int4 NOT NULL
             );"
        ))
        .map_err(|error| format!("create resident concurrency fixture: {error}"))?;
    monitor
        .execute(
            &format!(
                "INSERT INTO {fixture} (id, grp, measure)
                 SELECT g, (g % 64)::int4, (1000 + (g % 1000))::int4
                 FROM generate_series(1::bigint, $1::bigint) AS g"
            ),
            &[&fixture_rows],
        )
        .map_err(|error| format!("populate resident concurrency fixture: {error}"))?;
    monitor
        .batch_execute(&format!("ANALYZE {fixture}"))
        .map_err(|error| format!("analyze resident concurrency fixture: {error}"))?;
    Ok(fixture_rows)
}

#[cfg(feature = "integration_tests")]
fn create_fixture(monitor: &mut Client) -> Result<i64, String> {
    create_named_fixture(monitor, FIXTURE)
}

#[cfg(feature = "integration_tests")]
fn calibrate_backend(
    connection: &str,
    fixture_rows: i64,
    expected: &[GroupRow],
) -> Result<(WorkerSnapshot, Client), String> {
    let application_name = format!("{APPLICATION_PREFIX}{BACKEND_COUNT}");
    let mut client = open_named_client(connection, &application_name)?;
    configure_accel_backend(&mut client, BACKEND_COUNT, -1)?;
    let pid = backend_pid(&mut client)?;
    pin_fixture(&mut client, FIXTURE, fixture_rows)?;
    require_descriptor_plan(&mut client, QUERY)?;
    run_exact_iterations(
        &mut client,
        QUERY,
        expected,
        1,
        BACKEND_COUNT,
        WorkerPhase::Ready,
    )?;
    let snapshot = worker_snapshot(&mut client, BACKEND_COUNT, pid, None)?;
    Ok((snapshot, client))
}

#[cfg(feature = "integration_tests")]
fn validate_worker_snapshots(
    snapshots: &[WorkerSnapshot],
    expected_slots: &BTreeSet<usize>,
    calibration: &WorkerSnapshot,
) -> Result<i64, String> {
    if snapshots.len() != expected_slots.len() {
        return Err(format!(
            "resident worker snapshot count is {}, expected {}",
            snapshots.len(),
            expected_slots.len()
        ));
    }
    let slots = snapshots
        .iter()
        .map(|snapshot| snapshot.slot)
        .collect::<BTreeSet<_>>();
    if &slots != expected_slots {
        return Err(format!(
            "resident worker slots are {slots:?}, expected {expected_slots:?}"
        ));
    }
    let pids = snapshots
        .iter()
        .map(|snapshot| snapshot.pid)
        .collect::<BTreeSet<_>>();
    if pids.len() != snapshots.len() {
        return Err(format!("resident workers reused backend PIDs: {pids:?}"));
    }
    let mut total = 0_i64;
    for snapshot in snapshots {
        if snapshot.raw_bytes != calibration.raw_bytes
            || snapshot.derived_bytes != calibration.derived_bytes
            || snapshot.local_bytes != calibration.local_bytes
        {
            return Err(format!(
                "worker {} PID {} owns raw/derived/total {}/{}/{}, calibrated {}/{}/{}",
                snapshot.slot,
                snapshot.pid,
                snapshot.raw_bytes,
                snapshot.derived_bytes,
                snapshot.local_bytes,
                calibration.raw_bytes,
                calibration.derived_bytes,
                calibration.local_bytes
            ));
        }
        if snapshot.kernel_executions <= 0
            || snapshot.queries_accelerated <= 0
            || snapshot.stock_exec_count != 0
        {
            return Err(format!(
                "worker {} PID {} has invalid dispatch counters: kernels={}, accelerated={}, stock={}",
                snapshot.slot,
                snapshot.pid,
                snapshot.kernel_executions,
                snapshot.queries_accelerated,
                snapshot.stock_exec_count
            ));
        }
        total = total
            .checked_add(snapshot.local_bytes)
            .ok_or_else(|| "sum of backend-local resident bytes overflowed".to_owned())?;
    }
    Ok(total)
}

#[cfg(feature = "integration_tests")]
fn budget_rejection_text(error: &postgres::Error) -> String {
    let Some(db_error) = error.as_db_error() else {
        return error.to_string();
    };
    let mut text = format!("{} [{}]", db_error.message(), db_error.code().code());
    if let Some(detail) = db_error.detail() {
        text.push_str(" detail=");
        text.push_str(detail);
    }
    if let Some(hint) = db_error.hint() {
        text.push_str(" hint=");
        text.push_str(hint);
    }
    text
}

#[cfg(feature = "integration_tests")]
fn reject_ninth_backend(
    monitor: &mut Client,
    connection: &str,
    budget: ExactBudget,
    live_worker_pids: &BTreeSet<i32>,
    postmaster_started_at: &str,
    expected_cluster_bytes: i64,
) -> Result<String, String> {
    let slot = BACKEND_COUNT + 1;
    let application_name = format!("{APPLICATION_PREFIX}{slot}");
    let mut ninth = open_named_client(connection, &application_name)?;
    configure_accel_backend(&mut ninth, slot, budget.mib)?;
    let ninth_pid = backend_pid(&mut ninth)?;
    if live_worker_pids.contains(&ninth_pid) {
        return Err(format!("ninth backend reused live worker PID {ninth_pid}"));
    }
    let error = ninth
        .query_one(
            "SELECT pg_accel_pin($1::text::regclass, ARRAY['grp', 'measure'])",
            &[&FIXTURE],
        )
        .err()
        .ok_or_else(|| {
            format!(
                "ninth backend PID {ninth_pid} bypassed the {} MiB cluster residency budget",
                budget.mib
            )
        })?;
    let rejection = budget_rejection_text(&error);
    if !rejection
        .to_ascii_lowercase()
        .contains("exceeds cluster budget")
    {
        return Err(format!(
            "ninth backend failed for the wrong reason: {rejection}"
        ));
    }
    let status = local_status(&mut ninth)?;
    if status.rows != 0 || status.raw_bytes != 0 || status.derived_bytes != 0 {
        return Err(format!(
            "budget-rejected ninth backend retained local residency: {status:?}"
        ));
    }
    let counters = accel_counters(&mut ninth)?;
    if counters.kernels != 0 || counters.accelerated != 0 || counters.stock != 0 {
        return Err(format!(
            "budget-rejected ninth backend executed work: {counters:?}"
        ));
    }
    ninth
        .query_one("SELECT 1", &[])
        .map_err(|error| format!("ninth backend was unusable after budget ERROR: {error}"))?;
    let state = monitor_state(monitor)?;
    require_monitor_state(&state, postmaster_started_at, expected_cluster_bytes)?;
    drop(ninth);
    wait_for_backend_exit(
        monitor,
        postmaster_started_at,
        &[ninth_pid],
        expected_cluster_bytes,
    )?;
    Ok(rejection)
}

#[cfg(feature = "integration_tests")]
#[derive(Debug, Serialize)]
struct ConcurrencyEvidence {
    postmaster_started_at: String,
    fixture_rows: i64,
    backend_count: usize,
    soak_iterations_per_backend: usize,
    native_expected_rows: Vec<GroupRow>,
    budget_mib: i32,
    budget_bytes: u64,
    required_bytes: u64,
    rounding_spare_bytes: u64,
    calibration: WorkerSnapshot,
    ready: Vec<WorkerSnapshot>,
    soak: Vec<WorkerSnapshot>,
    soak_overlap: SoakOverlapEvidence,
    released_slot: usize,
    released_pid: i32,
    post_release_cluster_bytes: i64,
    post_release: Vec<WorkerSnapshot>,
    ninth_backend_rejection: String,
    final_cluster_bytes: i64,
}

#[cfg(feature = "integration_tests")]
fn run_concurrency_proof(
    monitor: &mut Client,
    connection: &str,
) -> Result<ConcurrencyEvidence, String> {
    monitor
        .batch_execute(
            "SET statement_timeout = '10min';
             SET lock_timeout = '30s';
             SET pg_accel.gpu_enabled = off;
             SELECT 1 FROM pg_accel_stats() LIMIT 1;",
        )
        .map_err(|error| format!("configure GPU-idle concurrency monitor: {error}"))?;
    let initial = monitor_state(monitor)?;
    if initial.in_recovery {
        return Err("PostgreSQL is already in recovery before concurrency proof".to_owned());
    }
    if initial.local_status_rows != 0 || initial.cluster_bytes != 0 {
        return Err(format!(
            "resident concurrency proof requires a clean ledger: monitor_local={}, cluster_bytes={}",
            initial.local_status_rows, initial.cluster_bytes
        ));
    }
    let stale_pids = tagged_backend_pids(monitor)?;
    if !stale_pids.is_empty() {
        return Err(format!(
            "resident concurrency proof found stale tagged backends: {stale_pids:?}"
        ));
    }
    let postmaster_started_at = initial.postmaster_started_at;
    let fixture_rows = create_fixture(monitor)?;
    monitor
        .batch_execute("SET pg_accel.enabled = off")
        .map_err(|error| format!("disable pg_accel for native reference: {error}"))?;
    let expected = grouped_rows(monitor, QUERY)?;
    if expected.len() != 64 {
        return Err(format!(
            "native grouped reference returned {} rows, expected 64",
            expected.len()
        ));
    }

    let (calibration, calibrator) = calibrate_backend(connection, fixture_rows, &expected)?;
    let calibration_state = monitor_state(monitor)?;
    require_monitor_state(
        &calibration_state,
        &postmaster_started_at,
        calibration.local_bytes,
    )?;
    let calibration_pid = calibration.pid;
    drop(calibrator);
    wait_for_backend_exit(monitor, &postmaster_started_at, &[calibration_pid], 0)?;

    let raw_bytes = u64::try_from(calibration.raw_bytes)
        .map_err(|error| format!("calibrated raw resident bytes are negative: {error}"))?;
    let backend_bytes = u64::try_from(calibration.local_bytes)
        .map_err(|error| format!("calibrated total resident bytes are negative: {error}"))?;
    let budget = exact_backend_budget(0, backend_bytes, raw_bytes, BACKEND_COUNT)?;

    let mut pool = WorkerPool::spawn(connection, FIXTURE, fixture_rows, &expected, budget.mib)?;
    let mut ready = pool.collect_phase(WorkerPhase::Ready, BACKEND_COUNT)?;
    ready.sort_by_key(|snapshot| snapshot.slot);
    let all_slots = (0..BACKEND_COUNT).collect::<BTreeSet<_>>();
    let ready_total = validate_worker_snapshots(&ready, &all_slots, &calibration)?;
    let ready_pids = ready
        .iter()
        .map(|snapshot| snapshot.pid)
        .collect::<BTreeSet<_>>();
    let expected_backend_count = i64::try_from(BACKEND_COUNT)
        .map_err(|error| format!("resident backend count does not fit i64: {error}"))?;
    if active_pid_count(monitor, &ready_pids.iter().copied().collect::<Vec<_>>())?
        != expected_backend_count
    {
        return Err("one or more ready resident backend PIDs are not active".to_owned());
    }
    let ready_state = monitor_state(monitor)?;
    require_monitor_state(&ready_state, &postmaster_started_at, ready_total)?;
    if u64::try_from(ready_total)
        .map_err(|error| format!("ready resident total is negative: {error}"))?
        != budget.required_bytes
    {
        return Err(format!(
            "eight-backend resident total {ready_total} differs from budget proof requirement {}",
            budget.required_bytes
        ));
    }

    let ninth_backend_rejection = reject_ninth_backend(
        monitor,
        connection,
        budget,
        &ready_pids,
        &postmaster_started_at,
        ready_total,
    )?;

    let (mut soak, soak_overlap) = pool.run_synchronized_soak(monitor, SOAK_ITERATIONS)?;
    soak.sort_by_key(|snapshot| snapshot.slot);
    let soak_total = validate_worker_snapshots(&soak, &all_slots, &calibration)?;
    if soak_total != ready_total {
        return Err(format!(
            "resident byte total changed during soak: ready={ready_total}, soak={soak_total}"
        ));
    }
    require_monitor_state(&monitor_state(monitor)?, &postmaster_started_at, soak_total)?;

    let released = ready
        .first()
        .ok_or_else(|| "resident worker set is empty".to_owned())?
        .clone();
    pool.stop(released.slot)?;
    let post_release_cluster_bytes = ready_total
        .checked_sub(released.local_bytes)
        .ok_or_else(|| "post-release resident total underflow".to_owned())?;
    wait_for_backend_exit(
        monitor,
        &postmaster_started_at,
        &[released.pid],
        post_release_cluster_bytes,
    )?;

    let remaining_slots = all_slots
        .iter()
        .copied()
        .filter(|slot| *slot != released.slot)
        .collect::<BTreeSet<_>>();
    let mut post_release = pool.run_active(WorkerPhase::PostRelease, 1)?;
    post_release.sort_by_key(|snapshot| snapshot.slot);
    let post_release_total =
        validate_worker_snapshots(&post_release, &remaining_slots, &calibration)?;
    if post_release_total != post_release_cluster_bytes {
        return Err(format!(
            "surviving backends own {post_release_total} bytes after one release, \
             expected {post_release_cluster_bytes}"
        ));
    }
    let surviving_pids = post_release
        .iter()
        .map(|snapshot| snapshot.pid)
        .collect::<BTreeSet<_>>();
    let expected_surviving_pids = ready_pids
        .iter()
        .copied()
        .filter(|pid| *pid != released.pid)
        .collect::<BTreeSet<_>>();
    if surviving_pids != expected_surviving_pids {
        return Err(format!(
            "backend PID ownership changed after one release: survivors={surviving_pids:?}, \
             expected={expected_surviving_pids:?}"
        ));
    }
    require_monitor_state(
        &monitor_state(monitor)?,
        &postmaster_started_at,
        post_release_cluster_bytes,
    )?;

    let final_pids = surviving_pids.iter().copied().collect::<Vec<_>>();
    pool.shutdown_all()?;
    wait_for_backend_exit(monitor, &postmaster_started_at, &final_pids, 0)?;
    let final_state = monitor_state(monitor)?;
    require_monitor_state(&final_state, &postmaster_started_at, 0)?;

    Ok(ConcurrencyEvidence {
        postmaster_started_at,
        fixture_rows,
        backend_count: BACKEND_COUNT,
        soak_iterations_per_backend: SOAK_ITERATIONS,
        native_expected_rows: expected,
        budget_mib: budget.mib,
        budget_bytes: budget.bytes,
        required_bytes: budget.required_bytes,
        rounding_spare_bytes: budget.spare_bytes,
        calibration,
        ready,
        soak,
        soak_overlap,
        released_slot: released.slot,
        released_pid: released.pid,
        post_release_cluster_bytes,
        post_release,
        ninth_backend_rejection,
        final_cluster_bytes: final_state.cluster_bytes,
    })
}

#[cfg(feature = "integration_tests")]
fn run_cancel_worker(
    mut client: Client,
    context: CancelWorkerContext,
    ready: &mpsc::SyncSender<CancelReady>,
    resume: &mpsc::Receiver<ServerActivityObservation>,
) -> Result<CancellationEvidence, String> {
    let mut rows = client
        .query_raw(
            &context.query,
            std::iter::empty::<&(dyn postgres::types::ToSql + Sync)>(),
        )
        .map_err(|error| format!("start real user-cancel query: {error}"))?;
    let first = rows
        .next()
        .map_err(|error| format!("real user-cancel query failed before its first row: {error}"))?
        .ok_or_else(|| "real user-cancel query completed without rows".to_owned())?;
    let first_branch = first
        .try_get::<_, i32>(0)
        .map_err(|error| format!("decode first cancellation branch id: {error}"))?;
    if !(0..i32::try_from(CANCEL_QUERY_BRANCHES)
        .map_err(|error| format!("cancel branch count does not fit i32: {error}"))?)
        .contains(&first_branch)
    {
        return Err(format!(
            "first cancellation row has out-of-range branch {first_branch}"
        ));
    }
    let first_row = group_row_at(&first, 1)?;
    if !context.expected.contains(&first_row) {
        return Err(format!(
            "first streamed GPU row differs from native PostgreSQL: {first_row:?}"
        ));
    }
    ready
        .send(CancelReady { first_row })
        .map_err(|error| format!("publish real GPU dispatch readiness: {error}"))?;
    let activity_observation = resume
        .recv_timeout(EXIT_TIMEOUT)
        .map_err(|error| format!("wait for real CancelToken request: {error}"))?;

    let mut canceled_rows_received = 1_usize;
    let terminal_error = loop {
        match rows.next() {
            Ok(Some(_)) => {
                canceled_rows_received = canceled_rows_received
                    .checked_add(1)
                    .ok_or_else(|| "canceled row count overflow".to_owned())?;
            }
            Ok(None) => {
                return Err(format!(
                    "real user-cancel query completed all {canceled_rows_received} rows without SQLSTATE 57014"
                ));
            }
            Err(error) => break error,
        }
    };
    drop(rows);

    let sqlstate = terminal_error.code().map(postgres::error::SqlState::code);
    if classify_cancellation_code(sqlstate) != CancellationClass::QueryCanceled {
        return Err(format!(
            "real CancelToken produced {sqlstate:?}, expected exact SQLSTATE 57014: {terminal_error}"
        ));
    }
    let sqlstate = sqlstate
        .ok_or_else(|| "query-canceled error omitted its SQLSTATE".to_owned())?
        .to_owned();

    let after_cancel = accel_counters(&mut client)?;
    let cancel_delta = counter_delta(after_cancel, context.counters_before)?;
    if cancel_delta.kernels <= 0 || cancel_delta.kernels >= context.full_kernel_dispatches {
        return Err(format!(
            "real cancel completed {} GPU dispatches; expected >0 and < full control {}",
            cancel_delta.kernels, context.full_kernel_dispatches
        ));
    }
    if cancel_delta.accelerated <= 0 {
        return Err("real cancel did not increment queries_accelerated".to_owned());
    }
    if after_cancel.stock != 0 || cancel_delta.stock != 0 {
        return Err(format!(
            "real cancel used native stock execution: after={after_cancel:?}, delta={cancel_delta:?}"
        ));
    }
    let resident_after_cancel = resident_fingerprint(&mut client, CANCEL_FIXTURE)?;
    if resident_after_cancel != context.resident_before {
        return Err(format!(
            "real cancel changed resident fingerprint: before={:?}, after={resident_after_cancel:?}",
            context.resident_before
        ));
    }
    let cluster_after_cancel = cluster_bytes(&mut client)?;
    if cluster_after_cancel != context.cluster_before {
        return Err(format!(
            "real cancel changed cluster ledger: before={}, after={cluster_after_cancel}",
            context.cluster_before
        ));
    }
    let plan_after_cancel = explain_text(&mut client, &context.query)?;
    if plan_after_cancel != context.plan {
        return Err("real cancel changed the repeated descriptor plan fingerprint".to_owned());
    }
    let artifacts_after_cancel = stable_device_artifact_snapshot()?;
    if artifacts_after_cancel != context.artifacts_before {
        return Err(format!(
            "real cancel changed AdaptiveCpp device artifacts: before={:?}, after={artifacts_after_cancel:?}",
            context.artifacts_before
        ));
    }

    let probe = client
        .query_one("SELECT 42::int4", &[])
        .map_err(|error| format!("backend probe after real cancel: {error}"))?
        .try_get::<_, i32>(0)
        .map_err(|error| format!("decode backend probe after real cancel: {error}"))?;
    if probe != 42 {
        return Err(format!("backend probe after real cancel returned {probe}"));
    }
    let recovery_before = accel_counters(&mut client)?;
    let recovered = grouped_rows(&mut client, CANCEL_BASE_QUERY)?;
    if recovered != context.expected {
        return Err(format!(
            "GPU recovery query differs from native PostgreSQL: expected={:?}, actual={recovered:?}",
            context.expected
        ));
    }
    let recovery_after = accel_counters(&mut client)?;
    let recovery_delta = counter_delta(recovery_after, recovery_before)?;
    if recovery_delta.kernels <= 0 || recovery_delta.accelerated <= 0 || recovery_after.stock != 0 {
        return Err(format!(
            "backend did not recover on GPU after real cancel: after={recovery_after:?}, delta={recovery_delta:?}"
        ));
    }
    if resident_fingerprint(&mut client, CANCEL_FIXTURE)? != context.resident_before {
        return Err("GPU recovery changed the resident fingerprint".to_owned());
    }
    if cluster_bytes(&mut client)? != context.cluster_before {
        return Err("GPU recovery changed the cluster resident ledger".to_owned());
    }
    if stable_device_artifact_snapshot()? != context.artifacts_before {
        return Err("GPU recovery changed the warmed AdaptiveCpp device artifacts".to_owned());
    }

    Ok(CancellationEvidence {
        postmaster_started_at: context.postmaster_started_at,
        backend_pid: context.backend_pid,
        fixture_rows: context.fixture_rows,
        repeated_branches: CANCEL_QUERY_BRANCHES,
        full_result_rows: context.full_result_rows,
        full_kernel_dispatches: context.full_kernel_dispatches,
        canceled_rows_received,
        canceled_kernel_dispatches: cancel_delta.kernels,
        canceled_queries_accelerated: cancel_delta.accelerated,
        sqlstate,
        activity_observation,
        plan_fingerprint: fnv1a64(context.plan.as_bytes()),
        resident_fingerprint: context.resident_before,
        cluster_bytes: context.cluster_before,
        device_artifacts: context.artifacts_before,
        recovery_kernel_dispatches: recovery_delta.kernels,
    })
}

#[cfg(feature = "integration_tests")]
fn join_cancel_worker(
    handle: thread::JoinHandle<Result<CancellationEvidence, String>>,
) -> Result<CancellationEvidence, String> {
    match handle.join() {
        Ok(result) => result,
        Err(payload) => Err(format!(
            "real user-cancel worker panicked: {}",
            panic_payload_text(&payload)
        )),
    }
}

#[cfg(feature = "integration_tests")]
fn run_user_cancel_proof(
    monitor: &mut Client,
    connection: &str,
) -> Result<CancellationEvidence, String> {
    monitor
        .batch_execute(
            "SET statement_timeout = '10min';
             SET lock_timeout = '30s';
             SET pg_accel.gpu_enabled = off;
             SELECT 1 FROM pg_accel_stats() LIMIT 1;",
        )
        .map_err(|error| format!("configure GPU-idle cancellation monitor: {error}"))?;
    let initial = monitor_state(monitor)?;
    require_monitor_state(&initial, &initial.postmaster_started_at, 0)?;
    let stale_pids = tagged_backend_pids(monitor)?;
    if !stale_pids.is_empty() {
        return Err(format!(
            "real cancellation proof found stale tagged backends: {stale_pids:?}"
        ));
    }
    let postmaster_started_at = initial.postmaster_started_at;
    let fixture_rows = create_named_fixture(monitor, CANCEL_FIXTURE)?;
    monitor
        .batch_execute("SET pg_accel.enabled = off")
        .map_err(|error| format!("disable pg_accel for cancellation native reference: {error}"))?;
    let expected = grouped_rows(monitor, CANCEL_BASE_QUERY)?;
    if expected.len() != 64 {
        return Err(format!(
            "cancellation native grouped reference returned {} rows, expected 64",
            expected.len()
        ));
    }

    let slot = BACKEND_COUNT + 2;
    let application_name = format!("{APPLICATION_PREFIX}{slot}");
    let mut client = open_named_client(connection, &application_name)?;
    configure_accel_backend(&mut client, slot, -1)?;
    let backend_pid = backend_pid(&mut client)?;
    pin_fixture(&mut client, CANCEL_FIXTURE, fixture_rows)?;
    require_descriptor_plan(&mut client, CANCEL_BASE_QUERY)?;
    run_exact_iterations(
        &mut client,
        CANCEL_BASE_QUERY,
        &expected,
        1,
        slot,
        WorkerPhase::Ready,
    )?;

    let query = repeated_cancel_query(CANCEL_QUERY_BRANCHES)?;
    let plan = explain_text(&mut client, &query)?;
    require_repeated_descriptor_plan(&plan, CANCEL_QUERY_BRANCHES)?;
    client
        .batch_execute("SELECT pg_accel_reset_stats()")
        .map_err(|error| format!("reset stats before full cancellation control: {error}"))?;
    let full_before = accel_counters(&mut client)?;
    let full_rows = client
        .query(&query, &[])
        .map_err(|error| format!("run full cancellation control query: {error}"))?;
    validate_repeated_rows(&full_rows, &expected, CANCEL_QUERY_BRANCHES)?;
    let full_after = accel_counters(&mut client)?;
    let full_delta = counter_delta(full_after, full_before)?;
    if full_delta.kernels <= 0 || full_delta.accelerated <= 0 || full_after.stock != 0 {
        return Err(format!(
            "full cancellation control did not use only GPU execution: after={full_after:?}, delta={full_delta:?}"
        ));
    }
    let full_result_rows = full_rows.len();

    client
        .batch_execute("SELECT pg_accel_reset_stats()")
        .map_err(|error| format!("reset stats before real user cancel: {error}"))?;
    let counters_before = accel_counters(&mut client)?;
    if counters_before.kernels <= 0 {
        return Err(format!(
            "full control left no monotonic kernel baseline: {counters_before:?}"
        ));
    }
    if counters_before.accelerated != 0 || counters_before.stock != 0 {
        return Err(format!(
            "real cancel stats reset left resettable counters nonzero: {counters_before:?}"
        ));
    }
    let resident_before = resident_fingerprint(&mut client, CANCEL_FIXTURE)?;
    let cluster_before = cluster_bytes(&mut client)?;
    if cluster_before <= 0 {
        return Err(format!(
            "real cancel worker owns no cluster resident bytes: {cluster_before}"
        ));
    }
    let artifacts_before = stable_device_artifact_snapshot()?;
    for extension in ["jit", "metallib", "metalar"] {
        if !artifacts_before.records.iter().any(|record| {
            Path::new(&record.path)
                .extension()
                .and_then(std::ffi::OsStr::to_str)
                == Some(extension)
        }) {
            return Err(format!(
                "full GPU control did not warm any .{extension} AdaptiveCpp device artifact"
            ));
        }
    }
    let cancel_token = client.cancel_token();
    let context = CancelWorkerContext {
        query,
        plan,
        expected,
        postmaster_started_at: postmaster_started_at.clone(),
        backend_pid,
        fixture_rows,
        full_result_rows,
        full_kernel_dispatches: full_delta.kernels,
        counters_before,
        resident_before,
        cluster_before,
        artifacts_before,
    };
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = mpsc::sync_channel(1);
    let handle = thread::Builder::new()
        .name("resident-real-user-cancel".to_owned())
        .spawn(move || run_cancel_worker(client, context, &ready_tx, &resume_rx))
        .map_err(|error| format!("spawn real user-cancel worker: {error}"))?;

    let controller_result = (|| {
        let ready = ready_rx
            .recv_timeout(OPERATION_TIMEOUT)
            .map_err(|error| format!("wait for first streamed GPU result: {error}"))?;
        if !ready.first_row.count.is_positive() {
            return Err(format!(
                "first streamed GPU result has non-positive count: {:?}",
                ready.first_row
            ));
        }
        let clock_origin = Instant::now();
        let activity = wait_for_activity_set(
            monitor,
            &[backend_pid],
            CANCEL_QUERY_TAG,
            false,
            clock_origin,
        )?;
        cancel_token
            .cancel_query(NoTls)
            .map_err(|error| format!("send real PostgreSQL CancelRequest: {error}"))?;
        resume_tx
            .send(activity)
            .map_err(|error| format!("release canceled query row iterator: {error}"))?;
        Ok(())
    })();

    if let Err(controller_error) = controller_result {
        let _ = cancel_token.cancel_query(NoTls);
        drop(resume_tx);
        if !wait_for_worker_finish(&handle, CANCEL_JOIN_TIMEOUT) {
            drop(handle);
            return Err(format!(
                "{controller_error}; real user-cancel worker did not stop within {CANCEL_JOIN_TIMEOUT:?}"
            ));
        }
        let worker_error = join_cancel_worker(handle).err();
        return match worker_error {
            None => Err(controller_error),
            Some(worker_error) => Err(format!(
                "{controller_error}; worker also failed: {worker_error}"
            )),
        };
    }

    if !wait_for_worker_finish(&handle, CANCEL_JOIN_TIMEOUT) {
        let second_cancel = cancel_token.cancel_query(NoTls).err();
        if !wait_for_worker_finish(&handle, CANCEL_JOIN_TIMEOUT) {
            drop(handle);
            return Err(format!(
                "real user-cancel worker did not stop after two {CANCEL_JOIN_TIMEOUT:?} windows{}",
                second_cancel.map_or_else(String::new, |error| format!(
                    "; second cancel failed: {error}"
                ))
            ));
        }
    }
    let evidence = join_cancel_worker(handle)?;
    wait_for_backend_exit(monitor, &postmaster_started_at, &[backend_pid], 0)?;
    require_monitor_state(&monitor_state(monitor)?, &postmaster_started_at, 0)?;
    Ok(evidence)
}

#[cfg(feature = "integration_tests")]
fn resident_test_artifacts(
    monitor: &mut Client,
    artifact_env: &str,
    default_label: &str,
) -> Result<ArtifactWriter, String> {
    let root = std::env::var_os(artifact_env)
        .map_or_else(|| default_run_dir(default_label), PathBuf::from);
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("PG_ACCEL_TEST_POSTGRES_LOG") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let version_num = monitor
            .query_one("SHOW server_version_num", &[])
            .map_err(|error| format!("read PostgreSQL server version for log audit: {error}"))?
            .try_get::<_, String>(0)
            .map_err(|error| format!("decode PostgreSQL server version: {error}"))?
            .parse::<u32>()
            .map_err(|error| format!("parse PostgreSQL server version: {error}"))?;
        let pg_major = version_num / 10_000;
        candidates.push(
            PathBuf::from(home)
                .join(".pgrx")
                .join(format!("{pg_major}.log")),
        );
    }
    candidates.extend(default_log_candidates());
    let data_dir = monitor
        .query_one("SHOW data_directory", &[])
        .map_err(|error| format!("read PostgreSQL data_directory for log audit: {error}"))?
        .try_get::<_, String>(0)
        .map_err(|error| format!("decode PostgreSQL data_directory: {error}"))?;
    append_pgdata_log_candidates(&mut candidates, Path::new(&data_dir));
    ArtifactWriter::new(root, candidates)
        .map_err(|error| format!("create {default_label} artifacts: {error}"))
}

#[cfg(feature = "integration_tests")]
fn concurrency_artifacts(monitor: &mut Client) -> Result<ArtifactWriter, String> {
    resident_test_artifacts(
        monitor,
        "PG_ACCEL_RESIDENT_CONCURRENCY_ARTIFACT_DIR",
        "resident-concurrency",
    )
}

#[cfg(feature = "integration_tests")]
fn cancellation_artifacts(monitor: &mut Client) -> Result<ArtifactWriter, String> {
    resident_test_artifacts(
        monitor,
        "PG_ACCEL_RESIDENT_CANCEL_ARTIFACT_DIR",
        "resident-user-cancel",
    )
}

#[cfg(feature = "integration_tests")]
fn audit_complete_log_deltas(writer: &ArtifactWriter) -> Result<(), String> {
    let deltas = writer
        .complete_log_deltas()
        .map_err(|error| format!("read complete concurrency log deltas: {error}"))?;
    for (source, body) in deltas {
        if let Some(failure) = log_delta_failure(&source.display().to_string(), &body) {
            return Err(failure);
        }
    }
    Ok(())
}

#[cfg(feature = "integration_tests")]
fn append_failure(failures: &mut Vec<String>, context: &str, error: impl std::fmt::Display) {
    failures.push(format!("{context}: {error}"));
}

#[cfg(feature = "integration_tests")]
#[test]
fn resident_real_user_cancel_is_bounded_exact_and_recoverable() {
    let _live_pg_guard = live_pg_test_lock();
    let connection = test_connection();
    let mut monitor = open_client(&connection).expect("connect resident cancellation monitor");
    let writer = cancellation_artifacts(&mut monitor).expect("prepare cancellation artifacts");
    let postmaster_started_at = monitor
        .query_one("SELECT pg_postmaster_start_time()::text", &[])
        .expect("read cancellation postmaster start time")
        .get::<_, String>(0);
    let mut failures = Vec::new();

    match run_user_cancel_proof(&mut monitor, &connection) {
        Ok(evidence) => match serde_json::to_string_pretty(&evidence) {
            Ok(json) => {
                if let Err(error) = fs::write(
                    writer.root().join("resident-user-cancel-proof.json"),
                    format!("{json}\n"),
                ) {
                    append_failure(&mut failures, "write cancellation proof", error);
                }
            }
            Err(error) => append_failure(&mut failures, "serialize cancellation proof", error),
        },
        Err(error) => append_failure(&mut failures, "real resident user cancel", error),
    }

    if let Err(error) = cleanup_tagged_backends(&mut monitor, &postmaster_started_at) {
        append_failure(&mut failures, "bounded cancellation backend cleanup", error);
    }
    if let Err(error) = monitor.batch_execute(&format!("DROP TABLE IF EXISTS {CANCEL_FIXTURE}")) {
        append_failure(&mut failures, "drop cancellation fixture", error);
    }
    if !failures.is_empty() {
        let _ = writer.write_failure("resident-user-cancel", &failures.join("\n"));
    }
    match writer.capture_log_tails("resident-user-cancel") {
        Ok(_) => {}
        Err(error) => append_failure(&mut failures, "capture cancellation log deltas", error),
    }
    if let Err(error) = audit_complete_log_deltas(&writer) {
        append_failure(
            &mut failures,
            "complete cancellation log delta audit",
            error,
        );
    }
    if !failures.is_empty() {
        let _ = writer.write_failure("resident-user-cancel", &failures.join("\n"));
        panic!(
            "resident user-cancel gate failed; artifacts={}:\n{}",
            writer.root().display(),
            failures.join("\n")
        );
    }
}

#[cfg(feature = "integration_tests")]
#[test]
fn resident_eight_backend_grouped_soak_enforces_budget_and_ownership() {
    let _live_pg_guard = live_pg_test_lock();
    let connection = test_connection();
    let mut monitor = open_client(&connection).expect("connect resident concurrency monitor");
    let writer = concurrency_artifacts(&mut monitor).expect("prepare concurrency artifacts");
    let postmaster_started_at = monitor
        .query_one("SELECT pg_postmaster_start_time()::text", &[])
        .expect("read concurrency postmaster start time")
        .get::<_, String>(0);
    let mut failures = Vec::new();

    match run_concurrency_proof(&mut monitor, &connection) {
        Ok(evidence) => match serde_json::to_string_pretty(&evidence) {
            Ok(json) => {
                if let Err(error) = fs::write(
                    writer.root().join("resident-concurrency-proof.json"),
                    format!("{json}\n"),
                ) {
                    append_failure(&mut failures, "write concurrency proof", error);
                }
            }
            Err(error) => append_failure(&mut failures, "serialize concurrency proof", error),
        },
        Err(error) => append_failure(&mut failures, "resident concurrency proof", error),
    }

    if let Err(error) = cleanup_tagged_backends(&mut monitor, &postmaster_started_at) {
        append_failure(&mut failures, "bounded resident backend cleanup", error);
    }
    if let Err(error) = monitor.batch_execute(&format!("DROP TABLE IF EXISTS {FIXTURE}")) {
        append_failure(&mut failures, "drop concurrency fixture", error);
    }
    if !failures.is_empty() {
        let _ = writer.write_failure("resident-concurrency", &failures.join("\n"));
    }
    match writer.capture_log_tails("resident-concurrency") {
        Ok(_) => {}
        Err(error) => append_failure(&mut failures, "capture log deltas", error),
    }
    if let Err(error) = audit_complete_log_deltas(&writer) {
        append_failure(&mut failures, "complete log delta audit", error);
    }
    if !failures.is_empty() {
        let _ = writer.write_failure("resident-concurrency", &failures.join("\n"));
        panic!(
            "resident concurrency gate failed; artifacts={}:\n{}",
            writer.root().display(),
            failures.join("\n")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_budget_admits_eight_complete_backends_but_not_ninth_raw_copy() {
        let budget = exact_backend_budget(0, 3 * MIB + 17, 2 * MIB, 8)
            .expect("eight-backend budget should be representable");

        assert_eq!(budget.required_bytes, 24 * MIB + 136);
        assert_eq!(budget.mib, 25);
        assert_eq!(budget.bytes, 25 * MIB);
        assert_eq!(budget.spare_bytes, MIB - 136);
        assert!(budget.spare_bytes < 2 * MIB);
    }

    #[test]
    fn exact_budget_rejects_fixture_smaller_than_mib_rounding_slack() {
        let error = exact_backend_budget(0, MIB + 1, 1, 8)
            .expect_err("one-byte raw copy cannot exclude a ninth backend");

        assert!(error.contains("ninth backend"));
    }

    #[test]
    fn exact_budget_rejects_overflow_and_zero_charges() {
        assert!(exact_backend_budget(0, 0, 0, 8).is_err());
        assert!(exact_backend_budget(u64::MAX, 1, 1, 8).is_err());
        assert!(exact_backend_budget(0, u64::MAX, u64::MAX, 8).is_err());
    }

    #[test]
    fn log_delta_parser_ignores_header_and_preserves_new_bytes() {
        let artifact = "source: /tmp/postgres.log\nrun_start_offset_bytes: 12\n---\nnew line\n";

        assert_eq!(
            artifact_delta_body(artifact).expect("delta body"),
            "new line\n"
        );
    }

    #[test]
    fn log_delta_audit_rejects_panic_resource_and_disconnect_evidence() {
        assert!(log_delta_failure("/tmp/pg_accel_panic.log", "{}\n").is_some());
        assert!(log_delta_failure("postgres.log", "resource leak detected\n").is_some());
        assert!(
            log_delta_failure(
                "postgres.log",
                "server closed the connection unexpectedly\n"
            )
            .is_some()
        );
    }

    #[test]
    fn log_delta_audit_allows_expected_budget_error() {
        let body = "ERROR: pg_accel_pin: resident allocation exceeds cluster budget\n";

        assert_eq!(log_delta_failure("postgres.log", body), None);
        assert_eq!(log_delta_failure("/tmp/pg_accel_panic.log", ""), None);
    }

    #[test]
    fn strict_common_overlap_requires_one_positive_eight_way_interval() {
        let intervals = [
            MonotonicInterval {
                start_us: 10,
                end_us: 100,
            },
            MonotonicInterval {
                start_us: 20,
                end_us: 90,
            },
            MonotonicInterval {
                start_us: 30,
                end_us: 80,
            },
            MonotonicInterval {
                start_us: 40,
                end_us: 70,
            },
            MonotonicInterval {
                start_us: 41,
                end_us: 69,
            },
            MonotonicInterval {
                start_us: 42,
                end_us: 68,
            },
            MonotonicInterval {
                start_us: 43,
                end_us: 67,
            },
            MonotonicInterval {
                start_us: 44,
                end_us: 66,
            },
        ];

        assert_eq!(
            strict_common_overlap(&intervals),
            Some(MonotonicInterval {
                start_us: 44,
                end_us: 66,
            })
        );
    }

    #[test]
    fn strict_common_overlap_rejects_serial_touching_and_empty_intervals() {
        assert_eq!(strict_common_overlap(&[]), None);
        assert_eq!(
            strict_common_overlap(&[
                MonotonicInterval {
                    start_us: 0,
                    end_us: 10,
                },
                MonotonicInterval {
                    start_us: 10,
                    end_us: 20,
                },
            ]),
            None
        );
        assert_eq!(
            strict_common_overlap(&[
                MonotonicInterval {
                    start_us: 0,
                    end_us: 9,
                },
                MonotonicInterval {
                    start_us: 10,
                    end_us: 20,
                },
            ]),
            None
        );
    }

    #[test]
    fn cancellation_classification_requires_exact_query_canceled_sqlstate() {
        assert_eq!(
            classify_cancellation_code(Some("57014")),
            CancellationClass::QueryCanceled
        );
        assert_eq!(
            classify_cancellation_code(Some("57000")),
            CancellationClass::OtherSqlState
        );
        assert_eq!(
            classify_cancellation_code(None),
            CancellationClass::NotDatabaseError
        );
    }

    #[test]
    fn counter_delta_preserves_nonzero_monotonic_kernel_baseline() {
        let before = AccelCounters {
            kernels: 195,
            accelerated: 0,
            stock: 0,
        };
        let after = AccelCounters {
            kernels: 198,
            accelerated: 1,
            stock: 0,
        };

        assert_eq!(
            counter_delta(after, before).expect("monotonic counter delta"),
            AccelCounters {
                kernels: 3,
                accelerated: 1,
                stock: 0,
            }
        );
        assert_eq!(
            counter_delta(before, before).expect("equal monotonic counters"),
            AccelCounters {
                kernels: 0,
                accelerated: 0,
                stock: 0,
            }
        );
        assert!(counter_delta(before, after).is_err());
        assert!(monotonic_delta(i64::MAX, i64::MIN, "overflow test").is_err());
    }

    #[cfg(feature = "integration_tests")]
    #[test]
    fn worker_join_wait_respects_its_wall_clock_deadline() {
        let handle = thread::spawn(|| {
            thread::sleep(Duration::from_millis(50));
            Ok(())
        });

        assert!(!wait_for_worker_finish(&handle, Duration::from_millis(1)));
        assert!(wait_for_worker_finish(&handle, Duration::from_secs(1)));
        assert_eq!(handle.join().expect("join bounded worker"), Ok(()));
    }
}
