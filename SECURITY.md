# Security Policy

## Reporting security issues

Report security vulnerabilities by email to **eric@contra.io**. Please include
a description of the issue, steps to reproduce, and any relevant logs or stack
traces. You will receive a response within 72 hours.

Do not open public GitHub issues for security vulnerabilities.

## Unsafe code

pg_accel uses `unsafe` in two areas:

1. **PostgreSQL FFI.** The pgrx framework requires unsafe calls to interact with
   PostgreSQL internals (SPI, Custom Scan vtables, shared memory, LWLocks).
   Every unsafe block has a `// SAFETY:` comment explaining why the invariants
   hold.

2. **GPU kernel bridge.** The C++/SYCL kernel interface uses FFI to pass
   buffers between Rust and the GPU runtime. Buffer lifetimes are managed by
   the dispatch layer and are scoped to a single batch execution.

All unsafe usage is auditable via `cargo clippy` and `grep -r "unsafe"`.

## Thread safety model

PostgreSQL backends are single-threaded processes. pg_accel spawns worker
threads only for GPU kernel orchestration, sort-key extraction, and top-k
merge -- never for calling PostgreSQL C functions. A shared-memory LWLock
enforces a cluster-wide thread budget so backends do not oversubscribe the
system.

Worker threads are blocked from receiving PostgreSQL signals via signal
masking. `CHECK_FOR_INTERRUPTS()` is called between batches on the main
backend thread.

## Shared memory

pg_accel allocates a small shared memory segment during `_PG_init` to hold
the global thread budget counter. Access is protected by a PostgreSQL LWLock.
The lock is always released in a `before_shmem_exit` callback to prevent
deadlocks on backend crash.
