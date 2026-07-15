//! Durable panic hook for pg_accel.
//!
//! When a Rust panic occurs across a C FFI boundary (e.g., inside
//! `cee_scape::call_with_sigsetjmp`), the panic cannot unwind → the runtime
//! invokes `panic_cannot_unwind` → `abort()`, generating SIGABRT. The default
//! panic message is printed to stderr, but PG's log_collector may not capture
//! it and trace subscribers may not flush before the abort.
//!
//! This module installs a panic hook that writes a JSONL record to
//! `$PGDATA/pg_accel_panic.log` (or `/tmp/pg_accel_panic.log` if `DataDir`
//! is not set), then flushes + syncs the file so the record hits disk
//! before the process aborts. It also prints a `PGACCEL PANIC:` line to
//! stderr so it's visible in terminal runs.
//!
//! The hook wraps the existing hook for genuine Rust panics. Ordinary pgrx
//! and PostgreSQL ERROR payloads are control flow: they are neither recorded
//! nor chained, so a caught SQL error cannot contaminate release panic logs.
//!
//! Call [`install`] exactly once at the top of `_PG_init`.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "pg_test")]
use std::cell::RefCell;

/// Per-process install flag. AtomicBool (not Once) so forked backends that
/// inherit the installed hook via COW don't re-install a second one — and
/// so a postmaster install is effectively inherited by all children.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Install the pg_accel panic hook.
///
/// Safe to call multiple times; only the first call installs the hook.
/// Chains onto the previous hook rather than replacing it so Rust's
/// default formatting and any other registered hook still run.
pub fn install() {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }

    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if is_postgres_error_control_flow(info.payload()) {
            return;
        }

        // Never let the hook itself panic — wrap the body in catch_unwind.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            write_panic_record(info);
        }));

        // Chain to the previous hook so default formatting still runs.
        prev(info);
    }));
}

fn is_postgres_error_control_flow(payload: &(dyn std::any::Any + Send)) -> bool {
    use pgrx::pg_sys::panic::{CaughtError, ErrorReport, ErrorReportWithLevel};

    if payload.is::<ErrorReport>() || payload.is::<ErrorReportWithLevel>() {
        return true;
    }
    matches!(
        payload.downcast_ref::<CaughtError>(),
        Some(CaughtError::PostgresError(_) | CaughtError::ErrorReport(_))
    )
}

#[cfg(feature = "pg_test")]
thread_local! {
    static TEST_PANIC_LOG_PATH: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Where the panic log goes. Mirrors `otel::trace_file_path`.
fn panic_log_path() -> String {
    #[cfg(feature = "pg_test")]
    if let Some(path) = TEST_PANIC_LOG_PATH.with(|slot| slot.borrow().clone()) {
        return path;
    }

    default_panic_log_path()
}

fn default_panic_log_path() -> String {
    // SAFETY: DataDir is set early in postmaster startup before extensions load.
    let data_dir = unsafe { pgrx::pg_sys::DataDir };
    if !data_dir.is_null() {
        // SAFETY: DataDir is a valid NUL-terminated C string.
        let path = unsafe { std::ffi::CStr::from_ptr(data_dir) };
        if let Ok(s) = path.to_str() {
            return format!("{s}/pg_accel_panic.log");
        }
    }
    "/tmp/pg_accel_panic.log".to_string()
}

/// Test-only unique panic artifact. The path override is backend-thread local,
/// so this never truncates or hides an existing release log. Dropping the
/// guard restores the previous path and deliberately preserves the artifact.
#[cfg(feature = "pg_test")]
pub(crate) struct PanicLogTestArtifact {
    path: String,
    previous: Option<String>,
}

#[cfg(feature = "pg_test")]
impl PanicLogTestArtifact {
    pub(crate) fn fresh() -> std::io::Result<Self> {
        let default_path = std::path::PathBuf::from(default_panic_log_path());
        let parent = default_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("/tmp"));
        let unique = format!(
            "pg_accel_panic.cancel.{}.{}.log",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos())
        );
        let path = parent.join(unique).to_string_lossy().into_owned();
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.sync_all()?;
        let previous = TEST_PANIC_LOG_PATH.with(|slot| slot.replace(Some(path.clone())));
        Ok(Self { path, previous })
    }

    pub(crate) fn contents(&self) -> std::io::Result<String> {
        std::fs::read_to_string(&self.path)
    }

    #[must_use]
    pub(crate) fn path(&self) -> &str {
        &self.path
    }
}

#[cfg(feature = "pg_test")]
impl Drop for PanicLogTestArtifact {
    fn drop(&mut self) {
        TEST_PANIC_LOG_PATH.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}

fn write_panic_record(info: &std::panic::PanicHookInfo<'_>) {
    // Extract payload (best-effort: Display via downcast to &str / String).
    let payload: &str = info
        .payload()
        .downcast_ref::<&'static str>()
        .copied()
        .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic payload>");

    let (file, line, col) = info
        .location()
        .map_or(("<unknown>", 0, 0), |l| (l.file(), l.line(), l.column()));

    let ts_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());

    // SAFETY: getpid is always safe.
    let pid = unsafe { libc::getpid() };

    let thread = std::thread::current();
    let thread_name = thread.name().unwrap_or("<unnamed>");

    // Capture a backtrace. `capture()` respects RUST_BACKTRACE; if disabled
    // this is essentially free. Format to string lazily.
    let bt = std::backtrace::Backtrace::capture();
    let bt_string = format!("{bt}");

    // Build a JSONL record. Manual JSON keeps us allocation-light and
    // avoids a serde_json dep in the hook path.
    let record = format!(
        "{{\"ts_unix_nanos\":{ts},\"pid\":{pid},\"thread\":{thread},\"panic\":{panic},\"location\":{loc},\"backtrace\":{bt}}}\n",
        ts = ts_nanos,
        pid = pid,
        thread = JsonStr(thread_name),
        panic = JsonStr(payload),
        loc = JsonStr(&format!("{file}:{line}:{col}")),
        bt = JsonStr(&bt_string),
    );

    // Also write a distinctive line to stderr (always, even if file open fails).
    let stderr_line = format!(
        "PGACCEL PANIC: pid={pid} thread={thread_name} at {file}:{line}:{col}: {payload}\n"
    );
    let _ = std::io::stderr().write_all(stderr_line.as_bytes());
    let _ = std::io::stderr().flush();

    // Open the log file in append mode and write + flush + sync.
    // If anything fails, we've already emitted to stderr — silently continue.
    let path = panic_log_path();
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(record.as_bytes());
        let _ = f.flush();
        // fsync to durably commit before the impending abort.
        let _ = f.sync_all();
    }
}

/// Minimal JSON string escaper — enough for panic messages.
/// Escapes `"`, `\`, newlines, tabs, carriage returns, and control chars.
struct JsonStr<'a>(&'a str);

impl std::fmt::Display for JsonStr<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use std::fmt::Write as _;
        f.write_str("\"")?;
        for c in self.0.chars() {
            match c {
                '"' => f.write_str("\\\"")?,
                '\\' => f.write_str("\\\\")?,
                '\n' => f.write_str("\\n")?,
                '\r' => f.write_str("\\r")?,
                '\t' => f.write_str("\\t")?,
                c if (c as u32) < 0x20 => {
                    write!(f, "\\u{:04x}", c as u32)?;
                }
                c => f.write_char(c)?,
            }
        }
        f.write_str("\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pgrx::PgLogLevel;
    use pgrx::pg_sys::panic::{CaughtError, ErrorReport, ErrorReportWithLevel};
    use pgrx::prelude::PgSqlErrorCode;

    fn error_report_with_level_payload() -> Box<dyn std::any::Any + Send> {
        std::panic::catch_unwind(|| {
            ErrorReport::new(
                PgSqlErrorCode::ERRCODE_QUERY_CANCELED,
                "test cancellation",
                "panic_hook_test",
            )
            .report(PgLogLevel::ERROR);
        })
        .expect_err("ERROR report should use pgrx panic control flow")
    }

    fn error_report_with_level() -> ErrorReportWithLevel {
        *error_report_with_level_payload()
            .downcast::<ErrorReportWithLevel>()
            .expect("pgrx ERROR payload should retain its typed report")
    }

    #[test]
    fn postgres_error_payloads_are_control_flow() {
        let bare = ErrorReport::new(
            PgSqlErrorCode::ERRCODE_QUERY_CANCELED,
            "bare cancellation",
            "panic_hook_test",
        );
        assert!(is_postgres_error_control_flow(&bare));

        let report = error_report_with_level_payload();
        assert!(is_postgres_error_control_flow(report.as_ref()));

        let postgres = CaughtError::PostgresError(error_report_with_level());
        assert!(is_postgres_error_control_flow(&postgres));

        let rust_report = CaughtError::ErrorReport(error_report_with_level());
        assert!(is_postgres_error_control_flow(&rust_report));
    }

    #[test]
    fn genuine_rust_panics_remain_recordable() {
        assert!(!is_postgres_error_control_flow(&"rust panic"));
        assert!(!is_postgres_error_control_flow(&String::from("rust panic")));

        let caught = CaughtError::RustPanic {
            ereport: error_report_with_level(),
            payload: Box::new(String::from("rust panic")),
        };
        assert!(!is_postgres_error_control_flow(&caught));
    }
}
