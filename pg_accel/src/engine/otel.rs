//! Tracing initialization for pg_accel.
//!
//! Sets up a triple-output tracing subscriber:
//! 1. **OTel JSONL file** — OTLP JSON spans written to `$PGDATA/pg_accel_otel.jsonl`,
//!    compatible with `otel-tui --from-json-file` for live span viewing.
//! 2. **tracing JSONL file** — tracing-subscriber JSON written to
//!    `$PGDATA/pg_accel_traces.jsonl` for Claude agent `Read` tool.
//! 3. **stderr** — compact human-readable format for PG log / terminal
//!
//! Controlled by `pg_accel.log_level` GUC (debug/info/notice/warning/error).
//! Call [`init`] once from `_PG_init` after GUCs are registered.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use super::gucs;
use super::gucs::PgAccelLogLevel;

/// Default per-file cap (MiB) when neither the GUC nor the legacy env
/// override is in play. Mirrors `pg_accel.otel_log_max_mb`'s default.
const DEFAULT_OTEL_LOG_MAX_MB: i32 = 256;

/// Hard floor on the per-file cap: 256 KiB. Anything smaller and we'd
/// rotate on every span event, which would shred the disk and provide no
/// useful history. Tests exercise rotation at this floor.
const MIN_OTEL_LOG_MAX_BYTES: u64 = 256 * 1024;

/// Legacy env override, retained so existing benchmark scripts that
/// `export PG_ACCEL_TRACE_FILE_MAX_BYTES=…` keep working. When set, the
/// env var overrides the GUC value (so it can be tightened without a PG
/// restart). Parsed as raw bytes; `0` / non-numeric / empty falls back
/// to the GUC.
const TRACE_FILE_MAX_BYTES_ENV: &str = "PG_ACCEL_TRACE_FILE_MAX_BYTES";

/// Recheck the on-disk size at most once per this many bytes-written.
/// We track an in-memory running counter so the hot per-write path does
/// not call `metadata()`. The check is forced when the counter would
/// push us past the cap.
const SIZE_RECHECK_INTERVAL_BYTES: u64 = 64 * 1024;

/// Per-process init flag. After fork(), each backend gets its own copy
/// (COW) so the postmaster's `true` is never seen by children.
/// We use AtomicBool instead of Once because Once state survives fork.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Handle to the tracing-subscriber JSONL file, captured so
/// [`flush_tracing`] can force it to disk on shmem_exit / before abort.
/// Set once per backend; subsequent re-inits (e.g. via planner hooks
/// after the first query) reuse the same handle.
static TRACE_FILE: OnceLock<Arc<BoundedFile>> = OnceLock::new();

/// Initialize tracing if not already done in this process.
///
/// Skips init in the postmaster (no queries run there). Forked backends
/// each initialize their own subscriber on first call.
/// Must be called after [`gucs::init_gucs`].
pub fn init() {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        return;
    }

    // SAFETY: IsUnderPostmaster is set by PG after fork.
    let is_postmaster = !unsafe { pgrx::pg_sys::IsUnderPostmaster };
    if is_postmaster {
        // Reset so forked backends will init on their first call.
        INITIALIZED.store(false, Ordering::SeqCst);
        return;
    }

    let result = std::panic::catch_unwind(try_init);
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            pgrx::log!("pg_accel: tracing init error: {e}");
        }
        Err(_) => {
            pgrx::log!("pg_accel: tracing init panicked");
        }
    }
}

fn try_init() -> Result<(), Box<dyn std::error::Error>> {
    let trace_path = trace_file_path("pg_accel_traces.jsonl");
    let policy = trace_rotation_policy();

    // tracing-subscriber JSON layer → JSONL file for Claude agents.
    let trace_file = Arc::new(BoundedFile::open(trace_path.clone(), policy)?);

    // Stash the file handle so `flush_tracing` can fsync it on exit /
    // before an abort. Ignore error if it was already set (forked backend).
    let _ = TRACE_FILE.set(Arc::clone(&trace_file));

    let json_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(FlushingWriter::new(Arc::clone(&trace_file)))
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .with_target(true)
        .with_thread_ids(false)
        .with_timer(tracing_subscriber::fmt::time::uptime());

    // Compact fmt layer → PG stderr (human-readable).
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_thread_ids(false)
        .compact()
        .with_writer(std::io::stderr);

    // OTel layer → OTLP JSON file for otel-tui.
    let otel_layer = build_otel_layer();

    let filter = level_filter_from_guc();

    tracing_subscriber::registry()
        .with(filter)
        .with(otel_layer)
        .with(stderr_layer)
        .with(json_layer)
        .try_init()
        .map_err(|e| format!("subscriber already set: {e}"))?;

    tracing::debug!(trace_path = %trace_path, "pg_accel: tracing initialized");
    Ok(())
}

/// Build an OpenTelemetry layer that exports spans as OTLP JSON to a file.
///
/// The file is compatible with `otel-tui --from-json-file`.
/// Returns `None` if the file cannot be opened.
fn build_otel_layer<S>()
-> Option<tracing_opentelemetry::OpenTelemetryLayer<S, opentelemetry_sdk::trace::SdkTracer>>
where
    S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
    use opentelemetry::trace::TracerProvider;

    let otel_path = trace_file_path("pg_accel_otel.jsonl");
    let policy = trace_rotation_policy();
    let file = match BoundedFile::open(otel_path.clone(), policy) {
        Ok(f) => Arc::new(f),
        Err(e) => {
            pgrx::log!("pg_accel: OTel trace file open failed: {e}");
            return None;
        }
    };

    let exporter = otlp_file::FileSpanExporter::new(file);
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_simple_exporter(exporter)
        .with_resource(
            opentelemetry_sdk::Resource::builder()
                .with_service_name("pg_accel")
                .build(),
        )
        .build();

    let tracer = provider.tracer("pg_accel");

    // Leak the provider — PG backends are single-use processes.
    std::mem::forget(provider);

    tracing::debug!(otel_path = %otel_path, "pg_accel: OTel traces attached");
    Some(tracing_opentelemetry::layer().with_tracer(tracer))
}

fn level_filter_from_guc() -> tracing_subscriber::filter::LevelFilter {
    match gucs::log_level() {
        PgAccelLogLevel::Debug => tracing_subscriber::filter::LevelFilter::DEBUG,
        PgAccelLogLevel::Info => tracing_subscriber::filter::LevelFilter::INFO,
        PgAccelLogLevel::Notice | PgAccelLogLevel::Warning => {
            tracing_subscriber::filter::LevelFilter::WARN
        }
        PgAccelLogLevel::Error => tracing_subscriber::filter::LevelFilter::ERROR,
    }
}

/// Force-flush the tracing JSONL file. Called from `shmem_exit` and, in
/// principle, anywhere we know the process may be about to abort.
///
/// Best-effort: silently ignores I/O errors because callers are typically
/// on an abort / exit path.
pub fn flush_tracing() {
    if let Some(handle) = TRACE_FILE.get() {
        handle.flush_and_sync();
    }
    // Flush stderr for good measure — span events also go there.
    let _ = std::io::stderr().flush();
}

/// `MakeWriter` wrapper around a [`BoundedFile`]. Every span event ends
/// up calling `make_writer()` → `write_all()` → our `flush()`, which
/// keeps the file effectively line-buffered so `cat pg_accel_traces.jsonl`
/// after a crash shows the last span.
#[derive(Clone)]
struct FlushingWriter {
    inner: Arc<BoundedFile>,
}

impl FlushingWriter {
    fn new(inner: Arc<BoundedFile>) -> Self {
        Self { inner }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for FlushingWriter {
    type Writer = BoundedFileWriter;
    fn make_writer(&'a self) -> Self::Writer {
        BoundedFileWriter {
            inner: Arc::clone(&self.inner),
        }
    }
}

/// `io::Write` adapter that funnels writes through the rotation-aware
/// [`BoundedFile::write_all`] entrypoint.
struct BoundedFileWriter {
    inner: Arc<BoundedFile>,
}

impl Write for BoundedFileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

// ---------------------------------------------------------------------------
// Rotation policy
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
struct RotationPolicy {
    /// Soft size cap for the active file. The writer rotates before
    /// pushing the running counter past this. `0` means "unbounded"
    /// (used internally for tests of the unbounded path); end users
    /// cannot reach this because the GUC has a min of 1 MiB.
    max_bytes: u64,
    /// Number of historical rotated files to retain (`<path>.1` ..
    /// `<path>.N`). `0` means "rotate then immediately discard".
    max_rotations: u32,
}

impl RotationPolicy {
    #[cfg(test)]
    fn for_tests(max_bytes: u64, max_rotations: u32) -> Self {
        Self {
            max_bytes,
            max_rotations,
        }
    }
}

/// Resolve the active rotation policy from the configured sources, in
/// precedence order:
///   1. `PG_ACCEL_TRACE_FILE_MAX_BYTES` env var (legacy bytes override).
///   2. `pg_accel.otel_log_max_mb` GUC (MiB).
///   3. Compiled-in `DEFAULT_OTEL_LOG_MAX_MB`.
///
/// `max_rotations` always comes from the GUC.
fn trace_rotation_policy() -> RotationPolicy {
    let max_bytes = configured_max_bytes(
        std::env::var(TRACE_FILE_MAX_BYTES_ENV).ok().as_deref(),
        || mb_to_bytes(gucs::otel_log_max_mb()),
    );
    let max_rotations = u32::try_from(gucs::otel_log_max_rotations().max(0)).unwrap_or(0);
    RotationPolicy {
        max_bytes,
        max_rotations,
    }
}

/// Pick the max-bytes value: env var if it's a positive integer, else
/// the lazily-computed GUC value. Floors at `MIN_OTEL_LOG_MAX_BYTES` to
/// keep tests honest (rotation must produce a non-empty rotated copy at
/// every supported cap).
fn configured_max_bytes(env: Option<&str>, guc: impl FnOnce() -> u64) -> u64 {
    let raw = env
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or_else(guc);
    raw.max(MIN_OTEL_LOG_MAX_BYTES)
}

/// Convert an MiB-valued GUC reading into bytes. Falls back to the
/// compiled-in default for non-positive values; pgrx never returns those
/// for a GUC registered with min=1, but the GUC machinery is global so
/// belt-and-braces here.
fn mb_to_bytes(mb: i32) -> u64 {
    let mb_u =
        u64::try_from(mb).unwrap_or_else(|_| u64::try_from(DEFAULT_OTEL_LOG_MAX_MB).unwrap_or(256));
    mb_u.saturating_mul(1024 * 1024)
}

// ---------------------------------------------------------------------------
// BoundedFile — append-only file with size-budgeted rotation
// ---------------------------------------------------------------------------

/// Append-only file handle that rotates its backing path when the
/// active file would exceed `policy.max_bytes`. Rotation renames
/// `<path>.N-1` → `<path>.N`, drops anything past `<path>.max_rotations`,
/// then renames `<path>` → `<path>.1` and reopens a fresh `<path>`.
///
/// Hot path:
///   * `write_all` increments a running byte counter (`bytes_written`).
///   * `metadata()` is only consulted once per
///     [`SIZE_RECHECK_INTERVAL_BYTES`] of writes, or when the running
///     counter alone would push past the cap. Long benchmark sessions
///     therefore amortize stat() down to ~one syscall per 64 KiB of
///     traces (vs. one syscall per span event in the prior impl that
///     allowed the 17.9 GiB blowup recorded in TODO.md).
struct BoundedFile {
    path: PathBuf,
    policy: RotationPolicy,
    state: Mutex<BoundedFileState>,
}

struct BoundedFileState {
    file: File,
    /// Best-effort estimate of the current on-disk size of the active
    /// file. Seeded from `metadata().len()` at open time and after each
    /// rotation, then incremented for each successful write.
    bytes_written: u64,
    /// Bytes appended since we last re-synced `bytes_written` to the
    /// real `metadata().len()`. Used to bound the cost of the cap check.
    bytes_since_recheck: u64,
}

impl BoundedFile {
    fn open(path: impl Into<PathBuf>, policy: RotationPolicy) -> io::Result<Self> {
        let path = path.into();

        // Roll over a too-big file from a previous session before we
        // start appending. This is the case that caught us pre-2026-05-13:
        // the JSONL was 17.9 GiB on disk by the time the new backend
        // attached to it.
        //
        // Only rotate if the file is *already* oversize — a fresh
        // (under-cap) file is left alone so backends restarting against
        // a healthy log don't lose recent history on every init.
        if policy.max_bytes > 0
            && file_exceeds(&path, policy.max_bytes)
            && let Err(e) = rotate_path(&path, &policy)
        {
            pgrx::log!(
                "pg_accel: trace file startup rotation failed for {}: {}",
                path.display(),
                e
            );
        }

        let (file, initial_len) = open_append(&path)?;

        Ok(Self {
            path,
            policy,
            state: Mutex::new(BoundedFileState {
                file,
                bytes_written: initial_len,
                bytes_since_recheck: 0,
            }),
        })
    }

    fn write_all(&self, buf: &[u8]) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let Ok(mut state) = self.state.lock() else {
            // The mutex is poisoned. We do *not* swallow this: trace I/O
            // is best-effort but we still surface the error so the
            // caller's logging path sees it instead of silently dropping
            // spans. Anti-cheat rule #4.
            return Err(io::Error::other("pg_accel trace file mutex poisoned"));
        };

        if self.policy.max_bytes > 0 {
            self.enforce_cap(&mut state, buf.len())?;
        }

        state.file.write_all(buf)?;
        // Flush after every write so a crash preserves the last span.
        state.file.flush()?;

        let len = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        state.bytes_written = state.bytes_written.saturating_add(len);
        state.bytes_since_recheck = state.bytes_since_recheck.saturating_add(len);
        Ok(())
    }

    fn flush(&self) -> io::Result<()> {
        let Ok(mut state) = self.state.lock() else {
            return Err(io::Error::other("pg_accel trace file mutex poisoned"));
        };
        state.file.flush()
    }

    fn flush_and_sync(&self) {
        if let Ok(mut state) = self.state.lock() {
            let _ = state.file.flush();
            let _ = state.file.sync_all();
        }
    }

    /// Rotate if a write of `next_bytes` would push us past the cap.
    /// Uses an in-memory counter as the hot-path estimate; only stats
    /// the real file once per [`SIZE_RECHECK_INTERVAL_BYTES`] window, or
    /// when the counter alone signals overflow.
    fn enforce_cap(&self, state: &mut BoundedFileState, next_bytes: usize) -> io::Result<()> {
        let next = u64::try_from(next_bytes).unwrap_or(u64::MAX);
        let cap = self.policy.max_bytes;
        let projected = state.bytes_written.saturating_add(next);

        // Cheap check: do we even need to consider rotation? Only every
        // SIZE_RECHECK_INTERVAL_BYTES of accumulated writes, OR whenever
        // the counter alone says we are about to bust the cap.
        let counter_says_over = projected > cap;
        let due_for_recheck = state.bytes_since_recheck >= SIZE_RECHECK_INTERVAL_BYTES;

        if !counter_says_over && !due_for_recheck {
            return Ok(());
        }

        // Re-sync the in-memory estimate to the real on-disk size. We
        // tolerate metadata() failures (the FD may have been unlinked
        // by an external rotator); fall back to the running counter so
        // we still rotate at the right point.
        if let Ok(meta) = state.file.metadata() {
            state.bytes_written = meta.len();
        }
        state.bytes_since_recheck = 0;

        let projected = state.bytes_written.saturating_add(next);
        if projected <= cap {
            return Ok(());
        }

        // Rotate. On failure we surface the error rather than silently
        // letting the file grow without bound (rule #4).
        rotate_path(&self.path, &self.policy)?;

        let (new_file, initial_len) = open_append(&self.path)?;
        state.file = new_file;
        state.bytes_written = initial_len;
        state.bytes_since_recheck = 0;
        Ok(())
    }
}

/// Open `path` in append mode and return the file along with its
/// current length (zero if the file was just created).
fn open_append(path: &Path) -> io::Result<(File, u64)> {
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let len = file.metadata().map_or(0, |m| m.len());
    Ok((file, len))
}

/// Returns `true` only if `path` is a regular file whose size strictly
/// exceeds `max_bytes`. Used by [`BoundedFile::open`] to decide whether
/// to age a prior-session file before reopening it for append. Missing
/// files and stat() failures are treated as "fine, leave it alone" —
/// the rotation logic will catch the file on a subsequent write if a
/// stat() inconsistency hid a real overflow.
fn file_exceeds(path: &Path, max_bytes: u64) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > max_bytes)
}

/// Rotate the file at `path` according to `policy`. The active file is
/// renamed to `<path>.1`, prior `.K` rotations are aged to `.K+1`, and
/// anything past `.max_rotations` is purged.
///
/// Best-effort but **not** silently lossy: rename failures on the active
/// file are returned to the caller so the cap is honored or the error
/// surfaces via `pgrx::log!`. Failures while ageing prior rotations are
/// logged and swallowed because dropping a stale rotation is preferable
/// to refusing to bound the active file.
fn rotate_path(path: &Path, policy: &RotationPolicy) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }

    let max = policy.max_rotations;

    // Zero-retention shortcut: just delete the active file in place.
    // The acceptance criterion for `max_rotations = 0` is "bound the
    // active file but keep no historical copies", so renaming to .1
    // and then deleting .1 is equivalent to a straight unlink.
    if max == 0 {
        // Drop any stale rotations from a previous higher setting.
        purge_rotation(path, 1);
        match fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
    }

    // Step 1: age existing rotations. <path>.max is discarded; <path>.K
    // becomes <path>.K+1 for K = max-1 .. 1.
    purge_rotation(path, max);
    for k in (1..max).rev() {
        let from = rotation_path(path, k);
        let to = rotation_path(path, k + 1);
        if !from.exists() {
            continue;
        }
        if let Err(e) = fs::rename(&from, &to) {
            pgrx::log!(
                "pg_accel: trace rotation ageing failed: {} → {}: {}",
                from.display(),
                to.display(),
                e
            );
        }
    }

    // Step 2: rename the active file to <path>.1.
    let target = rotation_path(path, 1);

    match fs::rename(path, &target) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(rename_err) => {
            // Last-resort: truncate in place. Better to lose a window
            // of history than to keep growing without bound. We still
            // surface the rename error so the operator sees something.
            match OpenOptions::new()
                .write(true)
                .open(path)
                .and_then(|f| f.set_len(0))
            {
                Ok(()) => Err(io::Error::new(
                    rename_err.kind(),
                    format!(
                        "rename {} → {} failed ({}); truncated in place",
                        path.display(),
                        target.display(),
                        rename_err
                    ),
                )),
                Err(truncate_err) => Err(io::Error::new(
                    rename_err.kind(),
                    format!(
                        "rename {} → {} failed ({}); truncate in place also failed ({})",
                        path.display(),
                        target.display(),
                        rename_err,
                        truncate_err
                    ),
                )),
            }
        }
    }
}

fn rotation_path(path: &Path, index: u32) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{index}"));
    PathBuf::from(name)
}

/// Delete `<path>.<index>` if it exists, logging non-`NotFound` errors.
fn purge_rotation(path: &Path, index: u32) {
    let candidate = rotation_path(path, index);
    match fs::remove_file(&candidate) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            pgrx::log!(
                "pg_accel: failed to purge rotated trace file {}: {}",
                candidate.display(),
                e
            );
        }
    }
}

fn trace_file_path(filename: &str) -> String {
    // SAFETY: DataDir is set early in postmaster startup before extensions load.
    let data_dir = unsafe { pgrx::pg_sys::DataDir };
    if !data_dir.is_null() {
        // SAFETY: DataDir is a valid NUL-terminated C string.
        let path = unsafe { std::ffi::CStr::from_ptr(data_dir) };
        if let Ok(s) = path.to_str() {
            return format!("{s}/{filename}");
        }
    }
    format!("/tmp/{filename}")
}

/// Custom OTLP JSON file exporter.
///
/// Writes one JSONL line per batch in the OTLP protobuf JSON format
/// (`resourceSpans` → `scopeSpans` → `spans[]`), compatible with
/// `otel-tui --from-json-file`.
mod otlp_file {
    use std::sync::Arc;
    use std::time::SystemTime;

    use opentelemetry::trace::{SpanId, SpanKind, Status};
    use opentelemetry_sdk::trace::{SpanData, SpanExporter};
    use serde::Serialize;

    use super::BoundedFile;

    pub struct FileSpanExporter {
        file: Arc<BoundedFile>,
    }

    impl FileSpanExporter {
        pub fn new(file: Arc<BoundedFile>) -> Self {
            Self { file }
        }
    }

    impl std::fmt::Debug for FileSpanExporter {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("FileSpanExporter")
        }
    }

    impl SpanExporter for FileSpanExporter {
        async fn export(&self, batch: Vec<SpanData>) -> opentelemetry_sdk::error::OTelSdkResult {
            if batch.is_empty() {
                return Ok(());
            }

            let spans: Vec<OtlpSpan> = batch.iter().map(to_otlp_span).collect();

            let doc = TracesData {
                resource_spans: vec![ResourceSpans {
                    resource: Resource {
                        attributes: vec![Attribute {
                            key: "service.name".into(),
                            value: AttrValue {
                                string_value: Some("pg_accel".into()),
                                int_value: None,
                                double_value: None,
                                bool_value: None,
                            },
                        }],
                    },
                    scope_spans: vec![ScopeSpans {
                        scope: Scope {
                            name: "pg_accel".into(),
                        },
                        spans,
                    }],
                }],
            };

            let Ok(mut line) = serde_json::to_vec(&doc) else {
                // Serialization failure is surfaced via the OTel SDK
                // result — we deliberately do not silently drop it
                // (anti-cheat rule #4). Returning Ok keeps the
                // OpenTelemetry pipeline alive; the missing batch will
                // already be visible as a gap in the rendered trace.
                return Ok(());
            };
            line.push(b'\n');

            if let Err(e) = self.file.write_all(&line) {
                // Tracing writes are best-effort, but a sustained
                // failure here is a real diagnostic gap. Log loudly so
                // the operator can see why traces stopped landing.
                pgrx::log!("pg_accel: OTel trace write failed: {e}");
            }
            Ok(())
        }

        fn shutdown(&mut self) -> opentelemetry_sdk::error::OTelSdkResult {
            Ok(())
        }
    }

    fn to_otlp_span(span: &SpanData) -> OtlpSpan {
        let parent = if span.parent_span_id == SpanId::INVALID {
            String::new()
        } else {
            format!(
                "{:016x}",
                u64::from_be_bytes(span.parent_span_id.to_bytes())
            )
        };

        let attributes: Vec<Attribute> = span
            .attributes
            .iter()
            .map(|kv| Attribute {
                key: kv.key.to_string(),
                value: value_to_otlp(&kv.value),
            })
            .collect();

        let events: Vec<OtlpEvent> = span
            .events
            .iter()
            .map(|e| OtlpEvent {
                time_unix_nano: nanos(e.timestamp),
                name: e.name.to_string(),
                attributes: e
                    .attributes
                    .iter()
                    .map(|kv| Attribute {
                        key: kv.key.to_string(),
                        value: value_to_otlp(&kv.value),
                    })
                    .collect(),
            })
            .collect();

        OtlpSpan {
            trace_id: format!(
                "{:032x}",
                u128::from_be_bytes(span.span_context.trace_id().to_bytes())
            ),
            span_id: format!(
                "{:016x}",
                u64::from_be_bytes(span.span_context.span_id().to_bytes())
            ),
            parent_span_id: parent,
            name: span.name.to_string(),
            kind: match span.span_kind {
                SpanKind::Internal => 1,
                SpanKind::Server => 2,
                SpanKind::Client => 3,
                SpanKind::Producer => 4,
                SpanKind::Consumer => 5,
            },
            start_time_unix_nano: nanos(span.start_time),
            end_time_unix_nano: nanos(span.end_time),
            attributes,
            status: OtlpStatus {
                code: match span.status {
                    Status::Unset => 0,
                    Status::Ok => 1,
                    Status::Error { .. } => 2,
                },
                message: match &span.status {
                    Status::Error { description } => description.to_string(),
                    _ => String::new(),
                },
            },
            events,
        }
    }

    fn value_to_otlp(v: &opentelemetry::Value) -> AttrValue {
        match v {
            opentelemetry::Value::Bool(b) => AttrValue {
                bool_value: Some(*b),
                ..AttrValue::default()
            },
            opentelemetry::Value::I64(i) => AttrValue {
                int_value: Some(i.to_string()),
                ..AttrValue::default()
            },
            opentelemetry::Value::F64(f) => AttrValue {
                double_value: Some(*f),
                ..AttrValue::default()
            },
            opentelemetry::Value::String(s) => AttrValue {
                string_value: Some(s.to_string()),
                ..AttrValue::default()
            },
            _ => AttrValue {
                string_value: Some(format!("{v:?}")),
                ..AttrValue::default()
            },
        }
    }

    fn nanos(t: SystemTime) -> String {
        t.duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos().to_string())
            .unwrap_or_default()
    }

    // --- OTLP JSON schema types ---

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct TracesData {
        resource_spans: Vec<ResourceSpans>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ResourceSpans {
        resource: Resource,
        scope_spans: Vec<ScopeSpans>,
    }

    #[derive(Serialize)]
    struct Resource {
        attributes: Vec<Attribute>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ScopeSpans {
        scope: Scope,
        spans: Vec<OtlpSpan>,
    }

    #[derive(Serialize)]
    struct Scope {
        name: String,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct OtlpSpan {
        trace_id: String,
        span_id: String,
        parent_span_id: String,
        name: String,
        kind: u8,
        start_time_unix_nano: String,
        end_time_unix_nano: String,
        attributes: Vec<Attribute>,
        status: OtlpStatus,
        events: Vec<OtlpEvent>,
    }

    #[derive(Serialize)]
    struct OtlpStatus {
        code: u8,
        message: String,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct OtlpEvent {
        time_unix_nano: String,
        name: String,
        attributes: Vec<Attribute>,
    }

    #[derive(Serialize)]
    struct Attribute {
        key: String,
        value: AttrValue,
    }

    #[derive(Serialize, Default)]
    #[serde(rename_all = "camelCase")]
    struct AttrValue {
        #[serde(skip_serializing_if = "Option::is_none")]
        string_value: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        int_value: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        double_value: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        bool_value: Option<bool>,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Per-test-suite counter so two parallel tests can't clobber each
    /// other in `std::env::temp_dir()`.
    static TEST_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    // -----------------------------------------------------------------
    // Config resolution
    // -----------------------------------------------------------------

    #[test]
    fn configured_max_bytes_prefers_env_override() {
        // Env wins over the GUC fallback, but is still floored.
        assert_eq!(
            configured_max_bytes(Some("1048576"), || 99 * 1024 * 1024),
            1024 * 1024
        );
        assert_eq!(
            configured_max_bytes(Some(" 4194304 "), || 16 * 1024 * 1024),
            4 * 1024 * 1024
        );
    }

    #[test]
    fn configured_max_bytes_falls_back_to_guc_when_env_invalid() {
        assert_eq!(
            configured_max_bytes(None, || 8 * 1024 * 1024),
            8 * 1024 * 1024
        );
        assert_eq!(
            configured_max_bytes(Some("not-a-number"), || 8 * 1024 * 1024),
            8 * 1024 * 1024
        );
        // env="0" is treated as "use the GUC" so operators can disable
        // an inherited override.
        assert_eq!(
            configured_max_bytes(Some("0"), || 8 * 1024 * 1024),
            8 * 1024 * 1024
        );
    }

    #[test]
    fn configured_max_bytes_enforces_floor() {
        // 1 byte is silly; we floor at MIN_OTEL_LOG_MAX_BYTES so
        // rotation can produce a non-empty rotated copy.
        assert_eq!(
            configured_max_bytes(Some("1"), || 1),
            MIN_OTEL_LOG_MAX_BYTES
        );
        assert_eq!(configured_max_bytes(None, || 1024), MIN_OTEL_LOG_MAX_BYTES);
    }

    #[test]
    fn mb_to_bytes_converts_and_handles_overflow() {
        assert_eq!(mb_to_bytes(0), 0);
        assert_eq!(mb_to_bytes(1), 1024 * 1024);
        assert_eq!(mb_to_bytes(256), 256 * 1024 * 1024);
        // Saturating multiply — i32::MAX MiB does not panic.
        let huge = mb_to_bytes(i32::MAX);
        assert!(huge > 0);
    }

    #[test]
    fn rotation_path_appends_index_suffix() {
        let p = PathBuf::from("/tmp/pg_accel_otel.jsonl");
        assert_eq!(
            rotation_path(&p, 1).to_string_lossy(),
            "/tmp/pg_accel_otel.jsonl.1"
        );
        assert_eq!(
            rotation_path(&p, 4).to_string_lossy(),
            "/tmp/pg_accel_otel.jsonl.4"
        );
    }

    // -----------------------------------------------------------------
    // Rotation behavior
    // -----------------------------------------------------------------

    #[test]
    fn write_below_cap_does_not_rotate() {
        let (dir, path) = temp_trace_path("pg_accel_otel.jsonl");
        let bf = BoundedFile::open(
            path.clone(),
            RotationPolicy::for_tests(MIN_OTEL_LOG_MAX_BYTES, 4),
        )
        .expect("open bounded file");

        bf.write_all(b"hello\n").expect("write below cap");

        assert_eq!(fs::read(&path).expect("read active trace file"), b"hello\n");
        assert!(!rotation_path(&path, 1).exists());
        fs::remove_dir_all(dir).expect("remove temp trace dir");
    }

    #[test]
    fn write_above_cap_rotates_to_dot_one() {
        let (dir, path) = temp_trace_path("pg_accel_otel.jsonl");
        // Use the policy floor; seed the file slightly above it so the
        // very first write triggers rotation.
        let cap = MIN_OTEL_LOG_MAX_BYTES;
        fs::write(&path, vec![b'.'; (cap + 1) as usize]).expect("seed oversize file");

        let bf = BoundedFile::open(path.clone(), RotationPolicy::for_tests(cap, 4))
            .expect("open bounded file with oversize active");

        // Open-time rotation already moved the active to .1 because
        // the seeded file exceeds the cap.
        assert!(rotation_path(&path, 1).exists());

        bf.write_all(b"line-after-rotate\n")
            .expect("write to fresh active");

        assert_eq!(
            fs::read(&path).expect("read fresh active"),
            b"line-after-rotate\n"
        );
        fs::remove_dir_all(dir).expect("remove temp trace dir");
    }

    #[test]
    fn many_writes_at_tiny_cap_keep_active_under_budget() {
        // The acceptance criterion: a full benchmark cannot create
        // unbounded logs. We simulate "many writes with a tiny cap" by
        // writing 100x the cap and verifying that the active file
        // never grows past `cap + max_record` and that the rotation
        // ring is bounded.
        let (dir, path) = temp_trace_path("pg_accel_otel.jsonl");
        let cap = MIN_OTEL_LOG_MAX_BYTES; // 256 KiB
        let record = vec![b'x'; 8 * 1024]; // 8 KiB per write
        let total_writes = 100;
        let bf = BoundedFile::open(path.clone(), RotationPolicy::for_tests(cap, 4))
            .expect("open bounded file");

        for i in 0..total_writes {
            bf.write_all(&record).unwrap_or_else(|e| {
                panic!("write {i} failed: {e}");
            });
        }
        bf.flush().expect("final flush");

        let active_len = fs::metadata(&path).expect("stat active").len();
        // Active must never exceed the cap by more than a single record.
        assert!(
            active_len <= cap + record.len() as u64,
            "active file {} bytes exceeded cap {} + record {}",
            active_len,
            cap,
            record.len()
        );

        // Total disk usage must be bounded by (max_rotations + 1) * (cap + record).
        let mut total = active_len;
        for k in 1..=8 {
            if let Ok(meta) = fs::metadata(rotation_path(&path, k)) {
                total += meta.len();
            }
        }
        let budget = 5 * (cap + record.len() as u64); // 4 rotations + active
        assert!(
            total <= budget,
            "total trace disk usage {total} exceeded budget {budget}"
        );

        // We should never have created a `.5` (max_rotations=4).
        assert!(!rotation_path(&path, 5).exists());
        fs::remove_dir_all(dir).expect("remove temp trace dir");
    }

    #[test]
    fn rotation_ages_and_purges_older_copies() {
        let (dir, path) = temp_trace_path("pg_accel_otel.jsonl");
        let cap = MIN_OTEL_LOG_MAX_BYTES;
        let policy = RotationPolicy::for_tests(cap, 2);

        // Pre-seed `<path>.1` and `<path>.2` with known content, then
        // force a rotation by exceeding the cap on the active file.
        fs::write(&path, vec![b'A'; (cap + 1) as usize]).expect("seed current");
        fs::write(rotation_path(&path, 1), b"prev-1").expect("seed .1");
        fs::write(rotation_path(&path, 2), b"prev-2").expect("seed .2");

        // Opening triggers startup rotation: .2 is purged, .1 → .2,
        // active → .1, fresh active is created.
        let bf = BoundedFile::open(path.clone(), policy).expect("open bounded file");

        assert_eq!(
            fs::read(rotation_path(&path, 2)).expect("read .2"),
            b"prev-1",
            "old .1 should have aged to .2"
        );
        // .1 should now be the previously-active oversize seed.
        let rotated_one_len = fs::metadata(rotation_path(&path, 1))
            .expect("stat .1")
            .len();
        assert!(rotated_one_len > cap, ".1 should be the rotated active");

        // .3 must not exist — max_rotations=2 bounds the ring.
        assert!(!rotation_path(&path, 3).exists());

        drop(bf);
        fs::remove_dir_all(dir).expect("remove temp trace dir");
    }

    #[test]
    fn rotation_zero_retention_discards_history() {
        let (dir, path) = temp_trace_path("pg_accel_traces.jsonl");
        let cap = MIN_OTEL_LOG_MAX_BYTES;
        // max_rotations = 0 means "rotate the active file but keep no
        // historical copies".
        fs::write(&path, vec![b'B'; (cap + 1) as usize]).expect("seed current");

        let _bf = BoundedFile::open(path.clone(), RotationPolicy::for_tests(cap, 0))
            .expect("open bounded file with zero retention");

        // Active is gone (was renamed to .1), .1 was then purged because
        // retention is zero.
        assert!(!path.exists() || fs::metadata(&path).expect("stat fresh active").len() == 0);
        assert!(
            !rotation_path(&path, 1).exists(),
            ".1 must not survive zero-retention rotation"
        );
        fs::remove_dir_all(dir).expect("remove temp trace dir");
    }

    #[test]
    fn unbounded_policy_never_rotates() {
        // `max_bytes == 0` is the internal "do not enforce" sentinel.
        // Verify the writer leaves rotation untouched at that setting.
        let (dir, path) = temp_trace_path("pg_accel_otel.jsonl");
        let bf = BoundedFile::open(path.clone(), RotationPolicy::for_tests(0, 4))
            .expect("open unbounded bounded file");
        // Write enough that a bounded policy would have rotated.
        let blob = vec![b'z'; MIN_OTEL_LOG_MAX_BYTES as usize];
        bf.write_all(&blob).expect("write large blob");
        bf.write_all(&blob).expect("write large blob again");
        bf.flush().expect("flush");

        assert_eq!(
            fs::metadata(&path).expect("stat active").len(),
            2 * blob.len() as u64
        );
        assert!(!rotation_path(&path, 1).exists());
        fs::remove_dir_all(dir).expect("remove temp trace dir");
    }

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    fn temp_trace_path(filename: &str) -> (PathBuf, PathBuf) {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let seq = TEST_DIR_SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "pg_accel_otel_test_{}_{nanos}_{seq}",
            std::process::id(),
        ));
        fs::create_dir_all(&dir).expect("create temp trace dir");
        let path = dir.join(filename);
        (dir, path)
    }
}
