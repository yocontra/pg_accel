## Summary

Describe the change and the user-visible behavior it affects.

## Validation

- [ ] `cargo fmt -- --check`
- [ ] `cargo metadata --locked --format-version 1 --no-deps`
- [ ] `cargo deny check`
- [ ] `cargo audit`, when available for the active Rust toolchain
- [ ] `cargo test -p pg_accel_bench --locked`
- [ ] `cargo check -p pg_accel --no-default-features --features pg18 --lib`
- [ ] SQL integration tests, when the change touches SQL-visible behavior
- [ ] Benchmark or release-gate evidence, when the change affects planner selection or performance claims

## Risk

Call out unsafe-code changes, PostgreSQL FFI changes, GPU kernel changes, benchmark methodology changes, and any known follow-up work.
