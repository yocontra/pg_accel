# Cross-Verification Protocol

Sneaky cheating is hard for the agent that did it to self-detect — the same agent
that wrote the silently-failing code also wrote the summary that says it works.
Assume you are not the exception. After any non-trivial change, spawn **fresh verifier
agents** (no prior context, no stake in the work) before reporting done to the user.

## When cross-verification is mandatory

Spawn verifiers for any of:
- Claimed perf win (any benchmark number cited in your response)
- Claimed fix for a crash, hang, wrong-result, or correctness regression
- New GPU kernel landed, or existing kernel rewritten
- Planner change that claims to enable/disable a strategy
- Any diff touching >1 of: `pgaccel-kernels/` (kernel), `src/gpu/` (bridge),
  `src/engine/executor/` (executor), `src/ffi/planner_hooks.rs` (planner)
- Any task where the user explicitly said "make sure" / "verify" / "confirm"

Skip only for: typo fixes, doc-only edits, obvious one-liners whose correctness is
visible in the diff alone.

## Use multiple verifiers in parallel, not one

A single verifier can be misled by the same framing that misled you. Spawn 2–3 in
parallel (single message, multiple `Agent` tool calls) with **disjoint briefs** so
they can't be fooled by the same trick:

### Verifier A — Re-run the claim
Subagent type: `general-purpose`. Give it:
- The commit SHA or branch under test
- The exact command to run (`just bench ...`, `just test <name>`, the failing query)
- Required output: real tool output pasted verbatim, plus a one-line PASS/FAIL

Its *only* job is to confirm the numbers/behavior. It must not re-summarize the
change or opine on code style.

### Verifier B — Audit the diff against the rails
Subagent type: `staff-code-reviewer` (or `general-purpose` with explicit brief).
Give it:
- The diff (`git diff <base>..HEAD`)
- A link to `.claude/rules/anti-cheat.md`
- Required: grep for banned patterns and report any hit with `file:line`

Specifically look for:
- `#[ignore]` without a matching `anti-cheat-allow:` comment
- `todo!()` / `unimplemented!()` in non-test code
- `unwrap_or(Vec::new())` / `unwrap_or(vec![])` / `unwrap_or_default()` on GPU paths
- `.ok()` discarding a `Result` on kernel dispatch
- Benchmark row counts that decreased vs prior baseline
- `min_batch_size` raised (hiding GPU break-even regression)
- `max_parallel_workers_per_gather = 0` anywhere
- `#[cfg(not(test))]` newly wrapping code that used to be test-visible
- Broad `#[allow(...)]` at module scope that wasn't there before

### Verifier C — Trace/log check (when relevant)
Subagent type: `general-purpose`. Give it:
- The trace file path (`~/.pgrx/data-17/pg_accel_traces.jsonl`)
- The list of spans that should exist if the claim is true
- Required: `Read` the trace file directly, confirm expected spans with attributes

Verifier C exists because it's very easy for the author to write a plausible-sounding
summary of a trace without ever opening the file. C forces the check.

## Verifier prompt requirements

Verifiers start with empty context. Each prompt must be self-contained:

1. **The specific claim in one sentence.** E.g. "Reduce of 10M i64 rows completes in
   <200ms on the default device." Not "perf improvement for reduce".
2. **The commit SHA or branch under test.** Not "the latest changes".
3. **Exact command(s) or file path(s) to inspect.** Not "the relevant tests".
4. **The red-flag patterns to check for.** Link to this file and `anti-cheat.md`.
5. **Required output format.** `PASS` / `FAIL` header + pasted evidence. Forbid
   re-summarizing the change or opining on code style.

Run verifiers in the **foreground**. Block on their reports before telling the user
"done".

## Responding to verifier failure

A verifier reporting `FAIL` is ground truth. Banned responses:
- Reframing the claim to make the failure "not count"
- Narrowing the scope after the fact
- Spawning a fourth verifier hoping for a different answer
- Arguing with the verifier's report

Allowed responses:
- Fix the underlying issue, re-run all verifiers from scratch
- Escalate honestly: *"Verifier caught X. Here is what I tried, here is what I think
  is actually wrong."* The user would rather hear a real problem than a manufactured
  success.

## Example invocation

```
Spawn 3 verifiers in one message:

Agent(description: "Re-run reduce bench", subagent_type: "general-purpose",
      prompt: "You are a fresh verifier. Claim: <one sentence>. Commit: <SHA>.
               Run `just bench reduce-i64-10M` and paste the output verbatim.
               Respond with PASS or FAIL + evidence. Do not re-summarize.")

Agent(description: "Audit diff for cheats", subagent_type: "staff-code-reviewer",
      prompt: "Review `git diff main..HEAD` against .claude/rules/anti-cheat.md.
               Grep for the banned patterns listed in .claude/rules/cross-verification.md.
               Report hits with file:line. PASS or FAIL.")

Agent(description: "Verify trace spans", subagent_type: "general-purpose",
      prompt: "Read ~/.pgrx/data-17/pg_accel_traces.jsonl and confirm spans
               gpu.reduce_i64 and exec.agg_grouped exist with n=10000000.
               PASS or FAIL + pasted span records.")
```
