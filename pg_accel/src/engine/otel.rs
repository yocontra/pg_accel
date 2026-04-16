//! Tracing initialization for pg_accel.
//!
//! Sets up a triple-output tracing subscriber:
//! 1. **OTel JSONL file** — OTLP JSON spans written to `$PGDATA/pg_accel_traces.jsonl`,
//!    compatible with `otel-tui --from-json-file` for live span viewing.
//! 2. **tracing JSONL file** — tracing-subscriber JSON for Claude agent `Read` tool.
//! 3. **stderr** — compact human-readable format for PG log / terminal
//!
//! Controlled by `pg_accel.log_level` GUC (debug/info/notice/warning/error).
//! Call [`init`] once from `_PG_init` after GUCs are registered.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use super::gucs;
use super::gucs::PgAccelLogLevel;

/// Per-process init flag. After fork(), each backend gets its own copy
/// (COW) so the postmaster's `true` is never seen by children.
/// We use AtomicBool instead of Once because Once state survives fork.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Handle to the tracing-subscriber JSONL file, captured so
/// [`flush_tracing`] can force it to disk on shmem_exit / before abort.
/// `OnceLock<Option<_>>` so we can tell "not yet initialized" from
/// "initialized but file open failed".
static TRACE_FILE: OnceLock<Mutex<File>> = OnceLock::new();

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

    // tracing-subscriber JSON layer → JSONL file for Claude agents.
    let trace_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&trace_path)?;

    // Stash the file handle so `flush_tracing` can fsync it on exit /
    // before an abort. Ignore error if it was already set (forked backend).
    let _ = TRACE_FILE.set(Mutex::new(trace_file.try_clone()?));

    let json_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(FlushingWriter::new(trace_file))
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
    let file = match OpenOptions::new()
        .create(true)
        .append(true)
        .open(&otel_path)
    {
        Ok(f) => f,
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
    if let Some(m) = TRACE_FILE.get()
        && let Ok(mut f) = m.lock()
    {
        let _ = f.flush();
        let _ = f.sync_all();
    }
    // Flush stderr for good measure — span events also go there.
    let _ = std::io::stderr().flush();
}

/// `MakeWriter` wrapper that calls `flush()` after every write so the
/// tracing JSONL file is durable line-by-line rather than block-buffered.
///
/// Each emitted event ends up calling `make_writer()` → `write_all()` →
/// our `flush()`. That turns the file into effectively line-buffered,
/// so `cat pg_accel_traces.jsonl` after a crash shows the last span.
#[derive(Clone)]
struct FlushingWriter {
    inner: std::sync::Arc<Mutex<File>>,
}

impl FlushingWriter {
    fn new(file: File) -> Self {
        Self {
            inner: std::sync::Arc::new(Mutex::new(file)),
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for FlushingWriter {
    type Writer = FlushingGuard<'a>;
    fn make_writer(&'a self) -> Self::Writer {
        FlushingGuard {
            guard: self.inner.lock().ok(),
        }
    }
}

struct FlushingGuard<'a> {
    guard: Option<std::sync::MutexGuard<'a, File>>,
}

impl Write for FlushingGuard<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(g) = self.guard.as_mut() {
            let n = g.write(buf)?;
            // Flush after every write so a crash preserves the line.
            g.flush()?;
            Ok(n)
        } else {
            Ok(buf.len())
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(g) = self.guard.as_mut() {
            g.flush()
        } else {
            Ok(())
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
    use std::fs::File;
    use std::io::Write;
    use std::sync::Mutex;
    use std::time::SystemTime;

    use opentelemetry::trace::{SpanId, SpanKind, Status};
    use opentelemetry_sdk::trace::{SpanData, SpanExporter};
    use serde::Serialize;

    pub struct FileSpanExporter {
        file: Mutex<File>,
    }

    impl FileSpanExporter {
        pub fn new(file: File) -> Self {
            Self {
                file: Mutex::new(file),
            }
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
                return Ok(());
            };
            line.push(b'\n');

            if let Ok(mut f) = self.file.lock() {
                let _ = f.write_all(&line);
                let _ = f.flush();
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
