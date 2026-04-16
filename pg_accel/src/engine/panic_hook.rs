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
//! The hook WRAPS the existing default hook (chain, never replace), and is
//! allocation-light so it works under panic pressure.
//!
//! Call [`install`] exactly once at the top of `_PG_init`.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

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
        // Never let the hook itself panic — wrap the body in catch_unwind.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            write_panic_record(info);
        }));

        // Chain to the previous hook so default formatting still runs.
        prev(info);
    }));
}

/// Where the panic log goes. Mirrors `otel::trace_file_path`.
fn panic_log_path() -> String {
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
        .map(|d| d.as_nanos())
        .unwrap_or(0);

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
