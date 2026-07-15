# Security Policy

## Supported versions

pg_accel has not made a public release. Security fixes currently target the
latest commit on the repository's active development branch. A supported
release-version table will be added when the first release is published.

## Reporting a vulnerability

Use [GitHub private vulnerability
reporting](https://github.com/yocontra/pg_accel/security/advisories/new). Include
the affected commit, PostgreSQL version, device/backend, reproduction steps,
and the smallest useful logs or SQL input. Do not include credentials or
production data.

Do not open a public issue, discussion, or pull request containing vulnerability
details. Private vulnerability reporting must be enabled and verified before a
public release; if the link above is unavailable, the repository does not yet
publish a separate source-backed private security address. That missing channel
is a release blocker, not permission to disclose the issue publicly.

## Security gates

Release candidates keep `Cargo.lock` committed and must pass
`cargo metadata --locked`, `just deny`, and `just audit`. Any ignored advisory
must identify the transitive dependency owner, why the affected path is not
reachable or accepted temporarily, and a concrete revisit condition.

Unsafe Rust is concentrated around PostgreSQL FFI, shared-memory lifecycle, and
the C++/SYCL bridge. Each boundary must document its invariants and remain under
the repository's strict lint and source-audit gates; this policy does not treat
the mere presence of a `SAFETY` comment as proof.

PostgreSQL APIs are called on the backend main thread. Current resident
execution submits synchronous device work from that thread; PostgreSQL parallel
workers are separate processes. The AdaptiveCpp runtime may manage internal
threads, but those threads must not call PostgreSQL APIs.

The extension registers PostgreSQL shared-memory state for the cluster thread
budget and resident-byte ledger. Access is serialized by PostgreSQL LWLocks,
and backend-local resource owners, GPU state, thread allocations, and tracing
are cleaned up through ordered `before_shmem_exit` callbacks.
