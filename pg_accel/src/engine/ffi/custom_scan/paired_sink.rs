//! Bounded, append-only test evidence, independent of disposable PGDATA.
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub const FILE_LIMIT: u64 = 4 * 1024 * 1024;
pub const TOTAL_LIMIT: u64 = 512 * 1024 * 1024;
pub const RECORD_LIMIT: u64 = 512;
pub const ROW_LIMIT: usize = 32_768;
pub type SharedSink = std::rc::Rc<std::cell::RefCell<Sink>>;

thread_local! {
    static ACTIVE: std::cell::RefCell<Option<SharedSink>> = const { std::cell::RefCell::new(None) };
}
pub fn with_sink<T>(sink: SharedSink, body: impl FnOnce() -> T) -> T {
    struct Restore(Option<SharedSink>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ACTIVE.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(ACTIVE.with(|slot| slot.borrow_mut().replace(sink)));
    body()
}
pub fn retain_row_bound<T: serde::Serialize>(rows: &T, next: &T) {
    let active = ACTIVE.with(|slot| slot.borrow().clone());
    if let Some(sink) = active {
        let result = sink.borrow_mut().record(
            "row_bound_failure",
            &serde_json::json!({"prefix":rows,"rejected_next":next}),
        );
        if let Err(error) = result {
            let _ = writeln!(
                io::stderr(),
                "paired row-bound secondary receipt error: {error}"
            );
        }
    }
}

pub struct Sink {
    directory: File,
    path: PathBuf,
    device: u64,
    inode: u64,
    ordinal: u64,
    bytes: u64,
    failed: bool,
}

impl Sink {
    pub fn open_owned(path: &Path) -> Result<Self, String> {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or("repository parent missing")?
            .join(".codex/scratch/hash-count-where-rust-20261030");
        if !path.is_absolute()
            || !path.starts_with(&base)
            || path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
            || std::fs::canonicalize(path).map_err(|e| e.to_string())? != path
        {
            return Err(
                "sink path is outside exact owned scratch tree or aliases another path".into(),
            );
        }
        let before = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        // SAFETY: geteuid reads this process's identity and has no PG effects.
        let uid = unsafe { libc::geteuid() };
        if !before.is_dir()
            || before.uid() != uid
            || before.mode() & 0o777 != 0o700
            || std::fs::read_dir(path)
                .map_err(|e| e.to_string())?
                .next()
                .is_some()
        {
            return Err("sink must be fresh, empty, owned0700 directory".into());
        }
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|e| e.to_string())?;
        let opened = directory.metadata().map_err(|e| e.to_string())?;
        if (opened.dev(), opened.ino()) != (before.dev(), before.ino()) {
            return Err("sink identity changed while opening".into());
        }
        Ok(Self {
            directory,
            path: path.to_owned(),
            device: opened.dev(),
            inode: opened.ino(),
            ordinal: 0,
            bytes: 0,
            failed: false,
        })
    }

    pub fn record<T: serde::Serialize>(&mut self, kind: &str, payload: &T) -> Result<(), String> {
        let result = self.record_inner(kind, payload);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn record_inner<T: serde::Serialize>(&mut self, kind: &str, payload: &T) -> Result<(), String> {
        if self.failed || self.ordinal >= RECORD_LIMIT || self.bytes >= TOTAL_LIMIT {
            return Err("sink already failed or hard inventory/byte bound reached".into());
        }
        let current = std::fs::symlink_metadata(&self.path).map_err(|e| e.to_string())?;
        if !current.is_dir() || (current.dev(), current.ino()) != (self.device, self.inode) {
            return Err("sink directory replaced".into());
        }
        let ordinal = self.ordinal;
        // Consume ordinal before opening. A partial file or failed create can
        // never be overwritten/reused by another record or best-effort error.
        self.ordinal += 1;
        let name = std::ffi::CString::new(format!("record-{ordinal:06}.json"))
            .map_err(|e| e.to_string())?;
        // SAFETY: owned directory FD and fixed NUL-terminated basename remain
        // live. O_EXCL/O_NOFOLLOW prohibit alias/overwrite; returned FD is owned.
        let fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error().to_string());
        }
        // SAFETY: openat returned a fresh FD now transferred exactly once.
        let mut file = unsafe { File::from_raw_fd(fd) };
        #[derive(serde::Serialize)]
        struct Envelope<'a, T> {
            ordinal: u64,
            kind: &'a str,
            payload: &'a T,
        }
        let record = Envelope {
            ordinal,
            kind,
            payload,
        };
        let mut limited = LimitedWriter {
            file: &mut file,
            total: &mut self.bytes,
            written: 0,
        };
        // Streaming serializer cannot allocate a whole over-bound escaped row
        // record before enforcing byte limits. Payloads themselves are bounded.
        serde_json::to_writer(&mut limited, &record).map_err(|e| e.to_string())?;
        limited.write_all(b"\n").map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        self.directory.sync_all().map_err(|e| e.to_string())?;
        Ok(())
    }
}

struct LimitedWriter<'a> {
    file: &'a mut File,
    total: &'a mut u64,
    written: u64,
}
impl Write for LimitedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let size =
            u64::try_from(bytes.len()).map_err(|_| io::Error::other("write length overflow"))?;
        if size > FILE_LIMIT - self.written || size > TOTAL_LIMIT - *self.total {
            return Err(io::Error::other("durable evidence byte bound reached"));
        }
        let written = self.file.write(bytes)?;
        let count =
            u64::try_from(written).map_err(|_| io::Error::other("write result overflow"))?;
        self.written += count;
        *self.total += count;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Preserve primary PostgreSQL ERROR even if its secondary receipt fails.
pub fn error_before_rethrow(sink: &SharedSink, context: &str) {
    let result = sink
        .try_borrow_mut()
        .map_err(|_| "sink borrow unavailable".to_owned())
        .and_then(|mut sink| sink.record("pg_error", &serde_json::json!({"context":context})));
    if let Err(error) = result {
        let _ = writeln!(
            io::stderr(),
            "paired primary PGERROR retained; secondary receipt failure: {error}"
        );
    }
}

pub fn active_error_before_rethrow(context: &str) {
    // Catch closures capture only immutable context strings, never a sink
    // RefCell that might have been borrowed across a PostgreSQL error.
    let sink = ACTIVE.with(|slot| slot.borrow().clone());
    if let Some(sink) = sink {
        error_before_rethrow(&sink, context);
    } else {
        let _ = writeln!(
            io::stderr(),
            "paired primary PGERROR; active receipt sink missing: {context}"
        );
    }
}

pub(super) fn owned_negative_sink(label: &str) -> Result<SharedSink, String> {
    use std::os::unix::fs::DirBuilderExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    if !label
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("invalid fixed negative-test label".into());
    }
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("missing repository root")?;
    if std::fs::canonicalize(repository).map_err(|error| error.to_string())? != repository {
        return Err("repository alias is not an owned test location".into());
    }
    let mut root = repository.to_owned();
    for component in [".codex", "scratch", "hash-count-where-rust-20261030"] {
        root.push(component);
        match std::fs::symlink_metadata(&root) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err("test directory component is not a real directory".into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&root)
                    .map_err(|error| error.to_string())?;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    let path = root.join(format!(
        "paired-sink-test-{}-{}-{label}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    // Fresh deterministic owner+ordinal, no retry if stale artifacts collide.
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .map_err(|error| error.to_string())?;
    Ok(std::rc::Rc::new(std::cell::RefCell::new(Sink::open_owned(
        &path,
    )?)))
}

pub(super) fn with_failed_sink<T>(body: impl FnOnce() -> T) -> T {
    let sink = owned_negative_sink("primary-error").expect("owned failure sink");
    sink.borrow_mut().failed = true;
    with_sink(sink, body)
}

#[cfg(test)]
mod tests {
    use super::{FILE_LIMIT, RECORD_LIMIT, Sink, TOTAL_LIMIT, owned_negative_sink};
    #[test]
    fn bounded_sink_retains_exact_records_and_rejects_reopen_aliases() {
        let sink = owned_negative_sink("exact").expect("fresh sink");
        let path = sink.borrow().path.clone();
        sink.borrow_mut()
            .record("one", &vec![(Some(-17_i64), 0_i64), (None, 9)])
            .expect("durable record");
        let bytes = std::fs::read(path.join("record-000000.json")).expect("record");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        assert_eq!(parsed["ordinal"], 0);
        assert_eq!(parsed["payload"], serde_json::json!([[-17, 0], [null, 9]]));
        assert!(
            Sink::open_owned(&path).is_err(),
            "nonempty directory must reject"
        );
        let alias = path.with_extension("alias");
        std::os::unix::fs::symlink(&path, &alias).expect("owned alias fixture");
        assert!(Sink::open_owned(&alias).is_err());
        assert_eq!(sink.borrow().bytes, bytes.len() as u64);
    }
    #[test]
    fn bounded_sink_charges_partial_bytes_and_never_retries_after_limit() {
        let sink = owned_negative_sink("partial").expect("fresh sink");
        let path = sink.borrow().path.clone();
        let oversized = "x".repeat(usize::try_from(FILE_LIMIT).expect("bounded size"));
        assert!(sink.borrow_mut().record("oversized", &oversized).is_err());
        let bytes = std::fs::metadata(path.join("record-000000.json"))
            .expect("partial retained")
            .len();
        assert!(bytes > 0 && bytes <= FILE_LIMIT);
        assert_eq!(sink.borrow().bytes, bytes);
        assert!(sink.borrow_mut().record("never", &0).is_err());
        assert!(!path.join("record-000001.json").exists());
    }
    #[test]
    fn bounded_sink_rejects_collisions_total_limit_and_record_limit() {
        let sink = owned_negative_sink("collision").expect("fresh sink");
        let path = sink.borrow().path.join("record-000000.json");
        std::fs::write(&path, b"retained sentinel").expect("owned collision");
        assert!(sink.borrow_mut().record("collision", &0).is_err());
        assert_eq!(
            std::fs::read(&path).expect("sentinel"),
            b"retained sentinel"
        );
        let total = owned_negative_sink("total").expect("fresh sink");
        total.borrow_mut().bytes = TOTAL_LIMIT - 4;
        assert!(total.borrow_mut().record("bound", &0).is_err());
        assert!(total.borrow().bytes <= TOTAL_LIMIT);
        let records = owned_negative_sink("records").expect("fresh sink");
        records.borrow_mut().ordinal = RECORD_LIMIT;
        assert!(records.borrow_mut().record("bound", &0).is_err());
    }
    #[test]
    fn bounded_sink_rejects_directory_replacement() {
        use std::os::unix::fs::DirBuilderExt;
        let sink = owned_negative_sink("replacement").expect("fresh sink");
        let path = sink.borrow().path.clone();
        let retained = path.with_extension("retained");
        std::fs::rename(&path, &retained).expect("retain original directory");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .expect("replacement");
        assert!(sink.borrow_mut().record("reject", &0).is_err());
        assert_eq!(std::fs::read_dir(&path).expect("replacement").count(), 0);
        assert_eq!(std::fs::read_dir(&retained).expect("original").count(), 0);
    }
}
