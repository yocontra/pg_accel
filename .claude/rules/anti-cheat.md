# Anti-Cheat Rails

Loaded via progressive disclosure — read in full any time you are about to mark work
done, edit code under `src/` / `pgaccel-kernels/src/`, or cite benchmark numbers.

This is a hard problem space (GPU kernels, PG planner, fork-safe Metal, parallel agg)
and agents have a documented tendency to cheat when stuck. The patterns below are bans,
not suggestions. Deterministic hooks in `.claude/hooks/` block the most egregious ones
at Edit/Write/Bash time; the rest are reviewer/verifier-enforced.

## The 10 bans

1. **No fake success.** If you didn't run `just ci` / `just bench` / the failing command
   in this conversation and observe its output, do not claim it passed. "Should work",
   "expected to pass", "this looks correct" are not evidence.
2. **No weakening tests.** Banned: `#[ignore]`, commented-out assertions, loosened
   tolerances, shrunk inputs, stub fixtures, `assert_eq!` → range check to sidestep a
   failure. If a test catches a bug, fix the bug — not the test. The Edit/Write hook
   blocks `#[ignore]` in `.rs` files; bypass with `// anti-cheat-allow: <reason>`.
3. **No hiding regressions.** Don't shrink benchmark row counts, lower `min_batch_size`
   to skip the GPU path, cherry-pick configurations that avoid the failing branch, or
   disable features for green numbers. Benchmarks compare like-for-like against the
   prior baseline. Never compare against PG single-threaded — see CLAUDE.md Benchmark
   Rule #11. The hook blocks `max_parallel_workers_per_gather = 0`.
4. **No silent error swallowing on GPU paths.** `unwrap_or(Vec::new())`, `.ok()` on
   kernel dispatch, empty-catch blocks, "return empty on failure" — all banned. Surface
   errors via `?`, `tracing::error!`, or panic. An empty result from a failed kernel
   silently corrupts query output; a crash at least tells the user something is wrong.
5. **No fabricated evidence.** Every trace span, benchmark number, log line, or crash
   report you cite must come from a tool call whose output is in this conversation. No
   invented numbers like "10M rows in 47ms". No paraphrasing traces you didn't open.
6. **No guessed APIs.** Grep or Read the source before writing against it. Hallucinated
   pgrx macros, AdaptiveCpp APIs, and FFI struct layouts have caused real backend
   crashes in this project. "I think pgrx has a `foo()` helper" is not a plan.
7. **No stubs masquerading as done.** `todo!()`, `unimplemented!()`, `Ok(vec![])`,
   returning `0` to get past a compile error — not implemented. The hook blocks
   `todo!()` / `unimplemented!()` in `.rs` files outside of tests.
8. **No bypassing the build.** No `--no-verify`, no broad `#[allow(dead_code)]` to
   silence real warnings, no `#[cfg(not(test))]` to hide code from tests, no
   file-wide clippy lint disables. The Bash hook blocks `--no-verify`.
9. **When stuck, say so.** See feedback memory `feedback_dont_disable_gpu.md`. If a
   kernel genuinely doesn't work and you can't figure out why, the correct output is:
   *"I tried X, Y, Z; here is what the trace/crash report showed; I don't have a fix."*
   Not a CPU fallback, not a disabled test, not a raised threshold to skip the failing
   size, not a declaration of victory on an adjacent easier problem.
10. **Cite `file:line` for code claims.** "The planner skips partial aggs when X"
    requires `src/ffi/planner_hooks.rs:NNN`. Every factual claim about this repo must
    be one-click verifiable.

## Bypass mechanism (hook-enforced rules only)

If you have a genuine reason to land a pattern that would otherwise be hook-blocked
(e.g. a test that must be `#[ignore]`-d while a platform regression upstream is
resolved), add an inline justification comment on the same line or the line
immediately above:

```rust
// anti-cheat-allow: upstream AdaptiveCpp bug #123 blocks this on x86 macOS; tracked in pg_accel issue #45
#[ignore]
fn test_foo() { ... }
```

The reason must be specific and auditable (issue number, upstream bug, concrete
external blocker). "TODO fix later" is not a valid reason.

## If you catch yourself reaching for any of these

Stop. Escalate to the user. An honest "I'm stuck, here's what I tried" is infinitely
more useful than a green build that hides the bug until the next benchmark run.
