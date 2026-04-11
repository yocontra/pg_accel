//! GPU Background Worker coordinator.
//!
//! PostgreSQL backends are fork()'d from the postmaster. After fork(), the
//! Metal/SYCL GPU driver connection is broken (IOKit Mach ports become stale).
//! This module runs a Background Worker process that owns the GPU and processes
//! work requests from forked backends via shared memory.
//!
//! # Architecture
//!
//! ```text
//! Backend (forked)              BGW (fresh process, owns GPU)
//! ─────────────────             ──────────────────────────────
//! 1. Create DSM segment         (sleeping on latch)
//! 2. Write input data to DSM
//! 3. Write GpuRequest to shmem
//! 4. SetLatch(bgw_latch)  ───→  5. Wake, read GpuRequest
//!                                6. Attach DSM, run GPU kernel
//!                                7. Write GpuResponse to shmem
//!    8. Wake (latch set)   ←───  8. SetLatch(backend_latch)
//! 9. Read result from DSM
//! 10. Detach DSM
//! ```

use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

use pgrx::lwlock::PgLwLock;
use pgrx::prelude::*;
use pgrx::shmem::PGRXSharedMemory;

// ---------------------------------------------------------------------------
// GPU operation codes
// ---------------------------------------------------------------------------

/// Identifies the GPU kernel to execute.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuOp {
    Noop = 0,
    ReduceSumF32 = 1,
    ReduceMinF32 = 2,
    ReduceMaxF32 = 3,
    ReduceSumF64 = 4,
    ReduceMinF64 = 5,
    ReduceMaxF64 = 6,
    ReduceSumI64 = 7,
    ReduceCount = 8,
    SortF32 = 10,
    SortF64 = 11,
    SortI32 = 12,
    SortI64 = 13,
    SortKvF32 = 14,
    SortKvF64 = 15,
    SortKvI32 = 16,
    SortKvI64 = 17,
    FusedFilterReduceF32 = 20,
    FusedFilterMultiReduceF32 = 21,
    FusedFilterCountF32 = 22,
    WindowRowNumber = 30,
    WindowLag = 31,
    WindowLead = 32,
}

// ---------------------------------------------------------------------------
// Shared memory request/response structures
// ---------------------------------------------------------------------------

/// Maximum number of extra parameters packed into a request.
const MAX_PARAMS: usize = 16;

/// A GPU work request written by a backend into shared memory.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GpuRequest {
    /// Which kernel to run.
    pub op: u32,
    /// DSM handle for input data (created by the requesting backend).
    pub dsm_handle: u32,
    /// Number of rows / elements.
    pub n_rows: u64,
    /// Byte offset into DSM where input data starts.
    pub input_offset: u64,
    /// Byte length of the input data.
    pub input_len: u64,
    /// Byte offset for secondary input (e.g. indices for sort_kv).
    pub input2_offset: u64,
    /// Byte length of secondary input.
    pub input2_len: u64,
    /// Generic parameter slots (interpretation depends on op).
    /// [0]: cmp_op for fused, offset for window lag/lead, etc.
    /// [1]: filter_val as f32 bits, default_val for window, etc.
    /// [2]: agg_op for fused, etc.
    /// [3]: num_aggs for multi-reduce, etc.
    pub params: [u64; MAX_PARAMS],
}

impl Default for GpuRequest {
    fn default() -> Self {
        Self {
            op: 0,
            dsm_handle: 0,
            n_rows: 0,
            input_offset: 0,
            input_len: 0,
            input2_offset: 0,
            input2_len: 0,
            params: [0; MAX_PARAMS],
        }
    }
}

/// Response from the BGW after processing a GPU request.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GpuResponse {
    /// 0 = success (PGACCEL_OK), negative = error.
    pub status: i32,
    /// For scalar results (reduce sum/min/max), the result value.
    pub scalar_f64: f64,
    /// For integer scalar results (reduce count).
    pub scalar_i64: i64,
    /// Byte offset in DSM where output data was written (for sort, window).
    pub output_offset: u64,
    /// Byte length of output data.
    pub output_len: u64,
}

impl Default for GpuResponse {
    fn default() -> Self {
        Self {
            status: 0,
            scalar_f64: 0.0,
            scalar_i64: 0,
            output_offset: 0,
            output_len: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Shared memory coordination state
// ---------------------------------------------------------------------------

/// States for the request slot. Transitions:
///   IDLE → PENDING (backend writes request)
///   PENDING → PROCESSING (BGW picks up)
///   PROCESSING → DONE (BGW writes response)
///   DONE → IDLE (backend reads response)
pub const STATE_IDLE: u32 = 0;
pub const STATE_PENDING: u32 = 1;
pub const STATE_PROCESSING: u32 = 2;
pub const STATE_DONE: u32 = 3;

/// Shared memory structure for GPU BGW coordination.
///
/// Only one request can be in-flight at a time (Metal has an in-order queue
/// anyway, so concurrent GPU submission doesn't help).
#[repr(C)]
pub struct GpuBgwState {
    /// Atomic state machine for the request slot.
    pub state: AtomicU32,
    /// PID of the BGW process (set by BGW on startup, read by backends).
    pub bgw_pid: AtomicI32,
    /// PID of the backend currently holding the request slot.
    pub requester_pid: AtomicI32,
    /// The current request (valid when state >= PENDING).
    pub request: GpuRequest,
    /// The current response (valid when state == DONE).
    pub response: GpuResponse,
    // -- Device info published by the BGW for backends to read --
    /// Number of GPU compute units (0 = no GPU available).
    pub device_compute_units: AtomicU32,
    /// Whether the GPU supports fp64 (0 = no, 1 = yes).
    pub device_has_fp64: AtomicU32,
    /// Whether the GPU uses unified memory (0 = no, 1 = yes).
    pub device_is_unified: AtomicU32,
    /// Maximum single allocation size on the GPU (bytes).
    pub device_max_alloc: std::sync::atomic::AtomicU64,
    /// Device name (null-terminated UTF-8, 128 bytes).
    pub device_name: [u8; 128],
}

impl Default for GpuBgwState {
    fn default() -> Self {
        Self {
            state: AtomicU32::new(STATE_IDLE),
            bgw_pid: AtomicI32::new(0),
            requester_pid: AtomicI32::new(0),
            request: GpuRequest::default(),
            response: GpuResponse::default(),
            device_compute_units: AtomicU32::new(0),
            device_has_fp64: AtomicU32::new(0),
            device_is_unified: AtomicU32::new(0),
            device_max_alloc: std::sync::atomic::AtomicU64::new(0),
            device_name: [0u8; 128],
        }
    }
}

impl std::fmt::Debug for GpuBgwState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuBgwState")
            .field("state", &self.state.load(Ordering::Relaxed))
            .field("bgw_pid", &self.bgw_pid.load(Ordering::Relaxed))
            .finish()
    }
}

// SAFETY: GpuBgwState contains only atomics and Copy types. All mutable
// access is coordinated by the atomic state machine (no PgLwLock needed
// for the hot path — the state machine provides mutual exclusion).
unsafe impl PGRXSharedMemory for GpuBgwState {}

/// LwLock-protected shared state. The LwLock is used only for the
/// initialization handshake; the hot path uses lock-free atomics.
pub static GPU_BGW: PgLwLock<GpuBgwState> =
    // SAFETY: Initialised in `init_shmem()` which is called from `_PG_init`.
    unsafe { PgLwLock::new(c"pg_accel_gpu_bgw") };

// ---------------------------------------------------------------------------
// Shared memory registration
// ---------------------------------------------------------------------------

/// Register the shared-memory segment. **Must be called from `_PG_init`.**
#[cfg(not(test))]
pub fn init_shmem() {
    pgrx::pg_shmem_init!(GPU_BGW);
}

// ---------------------------------------------------------------------------
// BGW registration (called from _PG_init)
// ---------------------------------------------------------------------------

/// Register the GPU coordinator background worker.
/// Must be called from `_PG_init` during postmaster startup.
#[cfg(not(test))]
pub fn register_bgw() {
    use pgrx::bgworkers::{BackgroundWorkerBuilder, BgWorkerStartTime};

    BackgroundWorkerBuilder::new("pg_accel GPU Coordinator")
        .set_function("gpu_bgw_main")
        .set_library("pg_accel")
        .enable_shmem_access(None)
        .set_start_time(BgWorkerStartTime::RecoveryFinished)
        .set_restart_time(Some(std::time::Duration::from_secs(1)))
        .load();
}

// ---------------------------------------------------------------------------
// BGW main function
// ---------------------------------------------------------------------------

/// Path to the GPU worker binary, set by build.rs at compile time.
const GPU_WORKER_PATH: &str = env!("PGACCEL_GPU_WORKER_PATH");

/// Persistent GPU worker child process handle.
struct GpuWorkerProcess {
    child: std::process::Child,
}

impl GpuWorkerProcess {
    /// Spawn the GPU worker binary via fork+exec.
    /// The exec() resets inherited Mach ports, giving the child a fresh
    /// MTLCompilerService XPC connection for Metal shader JIT compilation.
    fn spawn() -> Option<Self> {
        // Try compile-time path first, then search PATH.
        let worker_path = if std::path::Path::new(GPU_WORKER_PATH).exists() {
            GPU_WORKER_PATH.to_string()
        } else {
            // Fallback: look in PG's bindir.
            "pgaccel_gpu_worker".to_string()
        };

        let child = match std::process::Command::new(&worker_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit()) // worker diagnostics → PG log
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                pgrx::warning!(
                    "pg_accel BGW: failed to spawn GPU worker at {}: {}",
                    worker_path,
                    e
                );
                return None;
            }
        };

        Some(Self { child })
    }

    /// Wait for the worker to signal readiness (1 byte on stdout),
    /// then read device info sent by the worker.
    fn wait_ready(&mut self) -> Option<WorkerDeviceInfo> {
        use std::io::Read;
        let stdout = self.child.stdout.as_mut().expect("stdout pipe");

        // Read the ready byte.
        let mut buf = [0u8; 1];
        if stdout.read_exact(&mut buf).is_err() || buf[0] != 1 {
            return None;
        }

        // Read device info: [compute_units:u32][has_fp64:u8][is_unified:u8][max_alloc:u64][name:128]
        let mut cu_buf = [0u8; 4];
        let mut fp64_buf = [0u8; 1];
        let mut unified_buf = [0u8; 1];
        let mut alloc_buf = [0u8; 8];
        let mut name_buf = [0u8; 128];

        if stdout.read_exact(&mut cu_buf).is_err()
            || stdout.read_exact(&mut fp64_buf).is_err()
            || stdout.read_exact(&mut unified_buf).is_err()
            || stdout.read_exact(&mut alloc_buf).is_err()
            || stdout.read_exact(&mut name_buf).is_err()
        {
            return None;
        }

        Some(WorkerDeviceInfo {
            compute_units: u32::from_le_bytes(cu_buf),
            has_fp64: fp64_buf[0] != 0,
            is_unified: unified_buf[0] != 0,
            max_alloc: u64::from_le_bytes(alloc_buf),
            name: name_buf,
        })
    }

    /// Send a request to the worker via POSIX shared memory.
    ///
    /// Bulk data is placed in a POSIX shm segment; only a tiny control
    /// message goes through the pipe. The worker mmaps the shm, operates
    /// in-place, and sends back only scalar results via the pipe.
    fn call(
        &mut self,
        op: u32,
        n_rows: u64,
        shm_name: &str,
        data_len: u64,
    ) -> Option<WorkerResponse> {
        use std::io::{Read, Write};

        let stdin = self.child.stdin.as_mut()?;
        let stdout = self.child.stdout.as_mut()?;

        // Write control message: [op:u32][n_rows:u64][shm_name_len:u32][shm_name][data_len:u64]
        let name_bytes = shm_name.as_bytes();
        let name_len = name_bytes.len() as u32;
        if stdin.write_all(&op.to_le_bytes()).is_err()
            || stdin.write_all(&n_rows.to_le_bytes()).is_err()
            || stdin.write_all(&name_len.to_le_bytes()).is_err()
            || stdin.write_all(name_bytes).is_err()
            || stdin.write_all(&data_len.to_le_bytes()).is_err()
            || stdin.flush().is_err()
        {
            return None;
        }

        // Read response: status(i32) + scalar_f64(f64) + scalar_i64(i64) — 20 bytes.
        let mut hdr = [0u8; 4 + 8 + 8]; // 20 bytes
        if stdout.read_exact(&mut hdr).is_err() {
            return None;
        }

        let status = i32::from_le_bytes(hdr[0..4].try_into().ok()?);
        let scalar_f64 = f64::from_le_bytes(hdr[4..12].try_into().ok()?);
        let scalar_i64 = i64::from_le_bytes(hdr[12..20].try_into().ok()?);

        Some(WorkerResponse {
            status,
            scalar_f64,
            scalar_i64,
        })
    }
}

impl Drop for GpuWorkerProcess {
    fn drop(&mut self) {
        // Close stdin to signal the worker to exit, then wait.
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

/// Response from the GPU worker process.
/// Bulk data (for sort ops) is in the POSIX shm segment, not here.
struct WorkerResponse {
    status: i32,
    scalar_f64: f64,
    scalar_i64: i64,
}

/// Device info received from the GPU worker at startup.
struct WorkerDeviceInfo {
    compute_units: u32,
    has_fp64: bool,
    is_unified: bool,
    max_alloc: u64,
    name: [u8; 128],
}

/// Entry point for the GPU coordinator background worker.
///
/// The BGW fork+exec's a standalone GPU worker binary. The exec() call resets
/// all inherited Mach ports, giving the worker a fresh MTLCompilerService XPC
/// connection — required for Metal shader JIT on macOS.
#[pgrx::pg_guard]
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn gpu_bgw_main(_arg: pgrx::pg_sys::Datum) {
    use pgrx::bgworkers::{BackgroundWorker, SignalWakeFlags};

    BackgroundWorker::attach_signal_handlers(SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM);

    pgrx::log!("pg_accel BGW: spawning GPU worker process");

    let mut worker = if let Some(w) = GpuWorkerProcess::spawn() {
        w
    } else {
        pgrx::warning!("pg_accel BGW: cannot start GPU worker — exiting");
        return;
    };

    let device_info = if let Some(info) = worker.wait_ready() {
        info
    } else {
        pgrx::warning!("pg_accel BGW: GPU worker failed to initialize — exiting");
        return;
    };

    let name_str = std::str::from_utf8(&device_info.name)
        .unwrap_or("unknown")
        .trim_end_matches('\0');
    pgrx::log!(
        "pg_accel BGW: GPU worker ready — {} ({} CUs, fp64={}, unified={})",
        name_str,
        device_info.compute_units,
        device_info.has_fp64,
        device_info.is_unified,
    );

    // Publish our PID and device info so backends can read it from shmem.
    {
        let mut state = GPU_BGW.exclusive();
        state
            .device_compute_units
            .store(device_info.compute_units, Ordering::Release);
        state
            .device_has_fp64
            .store(u32::from(device_info.has_fp64), Ordering::Release);
        state
            .device_is_unified
            .store(u32::from(device_info.is_unified), Ordering::Release);
        state
            .device_max_alloc
            .store(device_info.max_alloc, Ordering::Release);
        // SAFETY: device_name is [u8; 128] in both source and dest.
        state.device_name.copy_from_slice(&device_info.name);
        state.bgw_pid.store(
            // SAFETY: MyProcPid is always valid in a backend process.
            unsafe { pgrx::pg_sys::MyProcPid },
            Ordering::Release,
        );
    }

    pgrx::log!("pg_accel BGW: entering main loop");

    while BackgroundWorker::wait_latch(Some(std::time::Duration::from_millis(100))) {
        process_pending_request_via_worker(&mut worker);
    }

    process_pending_request_via_worker(&mut worker);

    // Worker is cleaned up on drop (stdin closed → worker exits).
    drop(worker);
    pgrx::log!("pg_accel BGW: shutdown complete");
}

/// Check for and process a pending GPU request via the worker process.
fn process_pending_request_via_worker(worker: &mut GpuWorkerProcess) {
    // Try to transition from PENDING → PROCESSING.
    let state = GPU_BGW.exclusive();

    if state
        .state
        .compare_exchange(
            STATE_PENDING,
            STATE_PROCESSING,
            Ordering::AcqRel,
            Ordering::Relaxed,
        )
        .is_err()
    {
        return; // No pending request.
    }

    let request = state.request;
    let requester_pid = state.requester_pid.load(Ordering::Acquire);

    pgrx::log!(
        "pg_accel BGW: processing request op={} from pid={}, n_rows={}",
        request.op,
        requester_pid,
        request.n_rows
    );

    drop(state);

    // Execute via the worker process (fork+exec'd, clean GPU context).
    let response = execute_via_worker(worker, &request);

    pgrx::log!(
        "pg_accel BGW: request op={} completed, status={}",
        request.op,
        response.status
    );

    // Write the response and transition to DONE.
    {
        let mut state = GPU_BGW.exclusive();
        state.response = response;
        state.state.store(STATE_DONE, Ordering::Release);
    }

    // Wake the requesting backend.
    if requester_pid > 0 {
        // SAFETY: SetLatch on another backend's latch is safe if that
        // backend is alive. If it died, the signal is harmlessly lost.
        unsafe {
            let proc = pgrx::pg_sys::BackendPidGetProc(requester_pid);
            if proc.is_null() {
                libc::kill(requester_pid, libc::SIGUSR1);
            } else {
                pgrx::pg_sys::SetLatch(&raw mut (*proc).procLatch);
            }
        }
    }
}

/// Execute a GPU kernel request via POSIX shared memory.
///
/// Flow: DSM → memcpy → POSIX shm → worker operates in-place → memcpy → DSM.
/// Only tiny control messages go through the pipe (~20 bytes each way).
fn execute_via_worker(worker: &mut GpuWorkerProcess, req: &GpuRequest) -> GpuResponse {
    let mut resp = GpuResponse::default();

    // Attach the DSM segment.
    let dsm_seg = if req.dsm_handle != 0 {
        // SAFETY: dsm_attach maps a shared memory segment created by the
        // requesting backend.
        let seg = unsafe { pgrx::pg_sys::dsm_attach(req.dsm_handle) };
        if seg.is_null() {
            resp.status = -1;
            return resp;
        }
        Some(seg)
    } else {
        None
    };

    let dsm_base = dsm_seg.map(|seg| {
        // SAFETY: seg is a valid DSM segment we just attached.
        unsafe { pgrx::pg_sys::dsm_segment_address(seg).cast::<u8>() }
    });

    let op = req.op;
    let n = req.n_rows;

    let Some(base) = dsm_base else {
        resp.status = -1;
        return resp;
    };

    // Compute total shm size needed.
    let shm_size = compute_shm_size(req);
    if shm_size == 0 {
        resp.status = -1;
        return resp;
    }

    // Create POSIX shared memory segment.
    let shm_name = format!(
        "/pgaccel_{}",
        // SAFETY: MyProcPid is valid in a BGW process.
        unsafe { pgrx::pg_sys::MyProcPid }
    );

    let shm_ptr = if let Some(ptr) = posix_shm_create(&shm_name, shm_size) {
        ptr
    } else {
        pgrx::warning!("pg_accel BGW: failed to create POSIX shm {shm_name}");
        resp.status = -1;
        return resp;
    };

    // Copy data from DSM into POSIX shm.
    copy_dsm_to_shm(req, base, shm_ptr);

    // Call the worker — only control message through pipe.
    if let Some(wr) = worker.call(op, n, &shm_name, shm_size as u64) {
        resp.status = wr.status;
        resp.scalar_f64 = wr.scalar_f64;
        resp.scalar_i64 = wr.scalar_i64;

        // For sort ops, copy sorted data back from shm to DSM.
        if is_sort_op(op) && wr.status == 0 {
            copy_shm_to_dsm_sort(req, base, shm_ptr);
            resp.output_offset = req.input_offset;
            resp.output_len = req.input_len;
        }
    } else {
        pgrx::warning!("pg_accel BGW: worker communication failed for op={op}");
        resp.status = -1;
    }

    // Cleanup POSIX shm.
    posix_shm_destroy(&shm_name, shm_ptr, shm_size);

    // Detach DSM segment.
    if let Some(seg) = dsm_seg {
        // SAFETY: seg is a valid DSM segment we attached.
        unsafe {
            pgrx::pg_sys::dsm_detach(seg);
        }
    }

    resp
}

fn is_sort_op(op: u32) -> bool {
    op == GpuOp::SortKvF32 as u32 || op == GpuOp::SortKvF64 as u32
}

// ---------------------------------------------------------------------------
// POSIX shared memory helpers
// ---------------------------------------------------------------------------

/// Create a POSIX shared memory segment of `size` bytes. Returns mmap'd pointer.
fn posix_shm_create(name: &str, size: usize) -> Option<*mut u8> {
    use std::ffi::CString;
    let c_name = CString::new(name).ok()?;

    // SAFETY: shm_open/ftruncate/mmap are standard POSIX APIs.
    unsafe {
        let fd = libc::shm_open(c_name.as_ptr(), libc::O_CREAT | libc::O_RDWR, 0o600);
        if fd < 0 {
            return None;
        }
        if libc::ftruncate(fd, size as libc::off_t) != 0 {
            libc::close(fd);
            libc::shm_unlink(c_name.as_ptr());
            return None;
        }
        let ptr = libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        );
        libc::close(fd);
        if ptr == libc::MAP_FAILED {
            libc::shm_unlink(c_name.as_ptr());
            return None;
        }
        Some(ptr.cast::<u8>())
    }
}

/// Unmap and unlink a POSIX shared memory segment.
fn posix_shm_destroy(name: &str, ptr: *mut u8, size: usize) {
    use std::ffi::CString;
    // SAFETY: ptr was returned by posix_shm_create with matching size.
    unsafe {
        libc::munmap(ptr.cast(), size);
        if let Ok(c_name) = CString::new(name) {
            libc::shm_unlink(c_name.as_ptr());
        }
    }
}

/// Compute total POSIX shm size needed for a request.
fn compute_shm_size(req: &GpuRequest) -> usize {
    let n = req.n_rows as usize;
    let op = req.op;

    match op {
        x if x == GpuOp::ReduceSumF32 as u32
            || x == GpuOp::ReduceMinF32 as u32
            || x == GpuOp::ReduceMaxF32 as u32 =>
        {
            n * std::mem::size_of::<f32>()
        }
        x if x == GpuOp::ReduceSumF64 as u32
            || x == GpuOp::ReduceMinF64 as u32
            || x == GpuOp::ReduceMaxF64 as u32 =>
        {
            n * std::mem::size_of::<f64>()
        }
        x if x == GpuOp::ReduceSumI64 as u32 => n * std::mem::size_of::<i64>(),
        x if x == GpuOp::SortKvF32 as u32 => {
            n * std::mem::size_of::<f32>() + n * std::mem::size_of::<u32>()
        }
        x if x == GpuOp::SortKvF64 as u32 => {
            n * std::mem::size_of::<f64>() + n * std::mem::size_of::<u32>()
        }
        _ => 0,
    }
}

/// Copy data from DSM into the POSIX shm segment.
fn copy_dsm_to_shm(req: &GpuRequest, dsm_base: *mut u8, shm_ptr: *mut u8) {
    let n = req.n_rows as usize;
    let op = req.op;

    // SAFETY: dsm_base and shm_ptr are valid mapped regions of sufficient size.
    unsafe {
        if is_sort_op(op) {
            // Sort: copy [keys][indices] contiguously into shm.
            let key_size = if op == GpuOp::SortKvF64 as u32 { 8 } else { 4 };
            let keys_len = n * key_size;
            let idx_len = n * std::mem::size_of::<u32>();
            std::ptr::copy_nonoverlapping(
                dsm_base.add(req.input_offset as usize),
                shm_ptr,
                keys_len,
            );
            std::ptr::copy_nonoverlapping(
                dsm_base.add(req.input2_offset as usize),
                shm_ptr.add(keys_len),
                idx_len,
            );
        } else {
            // Reduce: copy input data contiguously.
            let len = req.input_len as usize;
            std::ptr::copy_nonoverlapping(dsm_base.add(req.input_offset as usize), shm_ptr, len);
        }
    }
}

/// Copy sorted data back from shm to DSM (keys and indices to their offsets).
fn copy_shm_to_dsm_sort(req: &GpuRequest, dsm_base: *mut u8, shm_ptr: *mut u8) {
    let n = req.n_rows as usize;
    let key_size = if req.op == GpuOp::SortKvF64 as u32 {
        8
    } else {
        4
    };
    let keys_len = n * key_size;
    let idx_len = n * std::mem::size_of::<u32>();

    // SAFETY: shm contains [keys][indices], DSM has space at the original offsets.
    unsafe {
        std::ptr::copy_nonoverlapping(shm_ptr, dsm_base.add(req.input_offset as usize), keys_len);
        std::ptr::copy_nonoverlapping(
            shm_ptr.add(keys_len),
            dsm_base.add(req.input2_offset as usize),
            idx_len,
        );
    }
}

// ---------------------------------------------------------------------------
// Public device-info query (for backends — reads from shared memory, no SYCL)
// ---------------------------------------------------------------------------

/// Device info read from BGW shared memory. No SYCL initialization needed.
#[derive(Debug, Clone)]
pub struct BgwDeviceInfo {
    pub compute_units: u32,
    pub has_fp64: bool,
    pub is_unified: bool,
    pub max_alloc: u64,
    pub device_name: [u8; 128],
}

/// Read GPU device info from BGW shared memory.
/// Returns `None` if the BGW hasn't started yet.
#[cfg(not(test))]
pub fn bgw_device_info() -> Option<BgwDeviceInfo> {
    let state = GPU_BGW.share();
    let pid = state.bgw_pid.load(Ordering::Acquire);
    if pid <= 0 {
        return None;
    }
    let cu = state.device_compute_units.load(Ordering::Acquire);
    if cu == 0 {
        return None;
    }
    Some(BgwDeviceInfo {
        compute_units: cu,
        has_fp64: state.device_has_fp64.load(Ordering::Acquire) != 0,
        is_unified: state.device_is_unified.load(Ordering::Acquire) != 0,
        max_alloc: state.device_max_alloc.load(Ordering::Acquire),
        device_name: state.device_name,
    })
}
