//! `EXPLAIN (VERBOSE)` audit harness — Phase 9 ship gate.
//!
//! Builds the fixed `EXPLAIN (VERBOSE)` query matrix and verifies, for each
//! query, that the parallel plan
//! shape includes pg_accel's `CustomScan` node underneath a `Gather` /
//! `Gather Merge`. Each row carries a [`RatchetExpectation`] telling the
//! harness whether the assertion is required to pass *today* or is gated
//! behind a future phase — gated rows print `[SKIP-gated-by-PhaseN]` and
//! do not fail the run, while `RequiredToday` rows that fail print
//! `[FAIL]` with a verbatim EXPLAIN snippet and force a non-zero exit.
//!
//! The harness is invoked via the bench CLI:
//!
//! ```text
//! cargo run -p pg_accel_bench --release -- explain-audit
//! ```
//!
//! Anti-cheat note: do **not** flip a `RequiredToday` row to
//! `RequiredAfterPhase(...)` to dodge a real failure. If a row fails today
//! that is a bug we want to surface, not paper over. See
//! `.claude/rules/anti-cheat.md` ban #2 ("No weakening tests").

use postgres::{Client, NoTls};

/// Whether a row in the audit matrix is required to pass today, or is
/// gated behind a future phase, or never required at all.
#[derive(Clone, Debug)]
pub enum RatchetExpectation {
    /// The row must pass on the current commit. A failure flips the audit
    /// to a non-zero exit and is reported as a regression.
    #[allow(dead_code)]
    RequiredToday,
    /// The row is expected to fail today; the listed phase is what unlocks
    /// it. Reported as `[SKIP-gated-by-...]` regardless of pass/fail so
    /// the harness can ratchet as the phase lands.
    RequiredAfterPhase(&'static str),
    /// The row tests an exotic/optional plan shape that is never required.
    /// Kept in the public type for future exotic rows; not currently
    /// constructed by [`build_matrix`] (the spec's nine rows are all
    /// `RequiredToday` or `RequiredAfterPhase`).
    #[allow(dead_code)]
    OptionalForever,
    /// The row is explicitly quarantined and must be visible in the audit
    /// report without executing EXPLAIN. This is an audit marker, not a
    /// production planner behavior change.
    Quarantined(&'static str),
}

/// One row of the audit matrix.
struct AuditRow {
    /// Short identifier printed in the report.
    name: &'static str,
    /// Plain-English description of the plan shape under test.
    description: &'static str,
    /// Idempotent SQL run before the audit query (fixture create + ANALYZE,
    /// session GUCs, etc.). Each statement is executed via
    /// `simple_query` and a failure of any one is fatal.
    setup: Vec<&'static str>,
    /// The audit query itself; the harness wraps it in `EXPLAIN (VERBOSE)`.
    query: &'static str,
    /// Ratchet level — see [`RatchetExpectation`].
    expectation: RatchetExpectation,
}

/// Result for a single row after running the audit.
#[derive(Clone, Debug)]
struct AuditOutcome {
    name: String,
    description: String,
    expectation: RatchetExpectation,
    /// `true` iff the EXPLAIN output shows a pg_accel `CustomScan` node
    /// inside a `Gather` / `Gather Merge` subtree.
    shape_matched: bool,
    /// Verbatim EXPLAIN text (for the report).
    explain: String,
    /// Resident-boundary audit results for every selected pg_accel Custom Scan
    /// found in `explain`.
    resident_audit: Vec<ResidentBoundaryFinding>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResidentBoundaryFinding {
    node: String,
    strategy: Option<String>,
    pipeline: Option<bool>,
    boundary_reason: Option<String>,
    status: ResidentBoundaryStatus,
    detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResidentBoundaryStatus {
    Pass,
    MissingStrategy,
    MissingPipeline,
    MissingProofEvidence,
    NonResidentPipeline,
}

/// Status code used for the per-row prefix in the printed report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowStatus {
    Pass,
    Fail,
    FailResidentBoundary,
    SkipGated,
    OptionalNotMet,
    Quarantined,
}

impl AuditOutcome {
    fn status(&self) -> RowStatus {
        if !self.resident_audit_passed() {
            return RowStatus::FailResidentBoundary;
        }
        match (&self.expectation, self.shape_matched) {
            (RatchetExpectation::RequiredToday | RatchetExpectation::OptionalForever, true) => {
                RowStatus::Pass
            }
            (RatchetExpectation::RequiredToday, false) => RowStatus::Fail,
            (RatchetExpectation::RequiredAfterPhase(_), _) => RowStatus::SkipGated,
            (RatchetExpectation::OptionalForever, false) => RowStatus::OptionalNotMet,
            (RatchetExpectation::Quarantined(_), _) => RowStatus::Quarantined,
        }
    }

    fn resident_audit_passed(&self) -> bool {
        self.resident_audit
            .iter()
            .all(|finding| finding.status == ResidentBoundaryStatus::Pass)
    }
}

/// Common fixtures shared across multiple rows. Idempotent: tables are
/// created if needed, then truncated before deterministic reload.
const COMMON_FIXTURES: &[&str] = &[
    "CREATE UNLOGGED TABLE IF NOT EXISTS bench_f32_10m \
       (id bigint, v real, dim int)",
    "TRUNCATE bench_f32_10m",
    "INSERT INTO bench_f32_10m (id, v, dim) \
     SELECT g, random()::real, (g % 16)::int \
     FROM generate_series(1, 10000000) g",
    "ANALYZE bench_f32_10m",
    "CREATE UNLOGGED TABLE IF NOT EXISTS bench_fact \
       (id bigint, payload real)",
    "TRUNCATE bench_fact",
    "INSERT INTO bench_fact (id, payload) \
     SELECT g, random()::real FROM generate_series(1, 1000000) g",
    "CREATE UNLOGGED TABLE IF NOT EXISTS bench_dim \
       (id bigint, name text)",
    "TRUNCATE bench_dim",
    "INSERT INTO bench_dim (id, name) \
     SELECT g, 'd' || g::text FROM generate_series(1, 1000) g",
    "ANALYZE bench_fact",
    "ANALYZE bench_dim",
    "CREATE UNLOGGED TABLE IF NOT EXISTS bench_isort \
       (id bigint, v real, dim int)",
    "CREATE INDEX IF NOT EXISTS bench_isort_dim_idx \
       ON bench_isort(dim)",
    "TRUNCATE bench_isort",
    "INSERT INTO bench_isort (id, v, dim) \
     SELECT g, random()::real, (g % 16)::int \
     FROM generate_series(1, 1000000) g",
    "ANALYZE bench_isort",
    "CREATE UNLOGGED TABLE IF NOT EXISTS bench_part \
       (id bigint, v real, dim int) \
       PARTITION BY HASH(id)",
    "CREATE UNLOGGED TABLE IF NOT EXISTS bench_part_p0 \
       PARTITION OF bench_part FOR VALUES WITH (modulus 4, remainder 0)",
    "CREATE UNLOGGED TABLE IF NOT EXISTS bench_part_p1 \
       PARTITION OF bench_part FOR VALUES WITH (modulus 4, remainder 1)",
    "CREATE UNLOGGED TABLE IF NOT EXISTS bench_part_p2 \
       PARTITION OF bench_part FOR VALUES WITH (modulus 4, remainder 2)",
    "CREATE UNLOGGED TABLE IF NOT EXISTS bench_part_p3 \
       PARTITION OF bench_part FOR VALUES WITH (modulus 4, remainder 3)",
    "TRUNCATE bench_part",
    "INSERT INTO bench_part (id, v, dim) \
     SELECT g, random()::real, (g % 16)::int \
     FROM generate_series(1, 1000000) g",
    "ANALYZE bench_part",
];

/// Documented planner defaults used by release-plan evidence.
const DEFAULT_PLANNER_SETTINGS: &[&str] = &[
    "SET pg_accel.enabled = on",
    "SET max_parallel_workers_per_gather = DEFAULT",
    "RESET min_parallel_table_scan_size",
    "RESET parallel_setup_cost",
    "RESET parallel_tuple_cost",
    "RESET enable_nestloop",
];

const SPATIAL_100K_QUARANTINE_REASON: &str = "legacy 100K spatial crash repro: chunked resident dispatch fix landed \
     (PGACCEL_SPATIAL_MAX_CHUNK_ROWS); temporarily quarantined while normal \
     spatial production admission remains dark; Phase 7 must flip this row \
     to executed evidence";

/// Build the full audit matrix. Order here is the order printed in the
/// report, so the order is an intentional stable contract.
fn build_matrix() -> Vec<AuditRow> {
    vec![
        AuditRow {
            name: "parallel_sum",
            description: "Plain SUM(v) — GPU-resident parallel reduce",
            setup: vec![],
            query: "SELECT SUM(v) FROM bench_f32_10m",
            expectation: RatchetExpectation::RequiredAfterPhase("GpuScan-fused partial aggregate"),
        },
        AuditRow {
            // Combined AVG+STDDEV used to be RequiredToday through a
            // `Finalize(Agg) -> Gather -> GpuAccel(partial)` path that
            // wrapped PostgreSQL's CPU partial scan. That shape violates the
            // GPU-resident admission rule: partial aggregate may re-enter
            // once the child is a real GpuScan/GpuJoin producer or the
            // aggregate owns the scan/reduce pipeline.
            name: "parallel_avg_stddev",
            description: "AVG(v), STDDEV(v) — GPU-resident parallel partial agg",
            setup: vec![],
            query: "SELECT AVG(v), STDDEV(v) FROM bench_f32_10m",
            expectation: RatchetExpectation::RequiredAfterPhase("GpuScan-fused partial aggregate"),
        },
        AuditRow {
            name: "parallel_groupby",
            description: "SELECT k, SUM(v) FROM t GROUP BY k — grouped HashAgg",
            setup: vec![],
            query: "SELECT dim, SUM(v) FROM bench_f32_10m GROUP BY dim",
            expectation: RatchetExpectation::RequiredAfterPhase("3a/3b grouped HashAgg"),
        },
        AuditRow {
            // Full ORDER BY with no LIMIT is not a release winning lane yet.
            // The 2026-05-13 benchmark showed no-limit GpuSort losing badly,
            // and the planner now intentionally gates it back to PostgreSQL
            // until the GPU full-sort algorithm/materialization path is fixed.
            name: "parallel_orderby",
            description: "ORDER BY v — full sort",
            setup: vec![],
            query: "SELECT * FROM bench_f32_10m ORDER BY v",
            expectation: RatchetExpectation::RequiredAfterPhase("full-sort GPU algorithm/costing"),
        },
        AuditRow {
            name: "parallel_window_partitioned",
            description: "ROW_NUMBER() OVER (PARTITION BY k ORDER BY v) — window",
            setup: vec![],
            query: "SELECT ROW_NUMBER() OVER (PARTITION BY dim ORDER BY v) \
                    FROM bench_f32_10m LIMIT 100",
            expectation: RatchetExpectation::RequiredAfterPhase("3c window partial path"),
        },
        AuditRow {
            // Plain JOIN is still a target lane, but the current cost model
            // correctly lets PostgreSQL's Parallel Hash Join win this 1M x
            // 1K fixture. Keep the row in the audit so it ratchets back to
            // RequiredToday only after the kernel/executor work proves a real
            // win instead of forcing an under-costed GpuHashJoin.
            name: "parallel_join",
            description: "Plain JOIN — parallel hash join",
            setup: vec![],
            query: "SELECT f.*, d.name FROM bench_fact f \
                    JOIN bench_dim d USING(id)",
            expectation: RatchetExpectation::RequiredAfterPhase("hashjoin cost/kernel calibration"),
        },
        AuditRow {
            name: "parallel_join_groupby",
            description: "JOIN + GROUP BY — grouped HashAgg over join",
            setup: vec![],
            query: "SELECT d.name, SUM(f.payload) FROM bench_fact f \
                    JOIN bench_dim d USING(id) GROUP BY d.name",
            expectation: RatchetExpectation::RequiredAfterPhase("3a/3b grouped HashAgg"),
        },
        AuditRow {
            name: "parallel_incremental_sort",
            description: "IncrementalSort — multi-key sort with presorted prefix",
            setup: vec![
                // Encourage the index path so PG picks IncrementalSort
                // rather than a full Sort.
                "SET enable_seqscan = off",
            ],
            query: "SELECT * FROM bench_isort ORDER BY dim, v",
            expectation: RatchetExpectation::RequiredAfterPhase("Post-1.0 cascaded multi-key sort"),
        },
        AuditRow {
            name: "parallel_append_partitioned",
            description: "Append over a partitioned table",
            setup: vec![],
            // SELECT ... LIMIT keeps the Append at the top of the plan
            // (no aggregate-induced CustomScan wrapping it). This is the
            // shape Lane 4's Append/MergeAppend injection has to fix.
            query: "SELECT * FROM bench_part WHERE v > 0.5 LIMIT 100",
            expectation: RatchetExpectation::RequiredAfterPhase("4 Append/MergeAppend injection"),
        },
        AuditRow {
            name: "spatial_100k_simple_regression_probe",
            description: "100K spatial repro, simple polygon — declined/quarantined",
            setup: vec![],
            query: "workload=spatial_sel_repro_simple_s90_b64k_w4_jiton rows=100000",
            expectation: RatchetExpectation::Quarantined(SPATIAL_100K_QUARANTINE_REASON),
        },
        AuditRow {
            name: "spatial_100k_coop1024_regression_probe",
            description: "100K spatial repro, cooperative 1024+v polygon — declined/quarantined",
            setup: vec![],
            query: "workload=spatial_sel_repro_coop1024_s90_b64k_w4_jiton rows=100000",
            expectation: RatchetExpectation::Quarantined(SPATIAL_100K_QUARANTINE_REASON),
        },
    ]
}

/// Fetch `EXPLAIN (VERBOSE) <query>` as a single newline-joined string.
fn fetch_explain(client: &mut Client, query: &str) -> Result<String, postgres::Error> {
    let stmt = format!("EXPLAIN (VERBOSE) {query}");
    let rows = client.query(&stmt, &[])?;
    let mut out = String::new();
    for row in &rows {
        let line: &str = row.get(0);
        out.push_str(line);
        out.push('\n');
    }
    Ok(out)
}

/// Detect a pg_accel CustomScan node inside a `Gather` / `Gather Merge`
/// subtree.
///
/// PostgreSQL's `EXPLAIN` output is indented by depth; a node is "inside
/// Gather" iff a `Gather` (or `Gather Merge`) line precedes it AND every
/// non-blank line between them is more deeply indented than the Gather
/// line. This is the simplest robust check that does not require a real
/// plan parser.
///
/// pg_accel's CustomScan nodes are always tagged `Custom Scan
/// (GpuAccel...)` per `pg_accel/src/engine/executor/...` — we match the
/// `Custom Scan (` prefix and require the substring `GpuAccel` so we do
/// not fire on third-party CustomScan providers.
fn shape_has_customscan_under_gather(explain: &str) -> bool {
    // Track every Gather/GatherMerge line and its indent depth. If we
    // later see a `Custom Scan (GpuAccel...)` deeper than any active
    // Gather depth, the shape matches.
    let mut gather_depths: Vec<usize> = Vec::new();
    for raw in explain.lines() {
        if raw.trim().is_empty() {
            continue;
        }
        let depth = leading_indent(raw);
        // Pop any Gather scopes whose subtree we have already exited.
        gather_depths.retain(|&g| depth > g);

        let trimmed = raw.trim_start_matches([' ', '-', '>']).trim_start();
        if trimmed.starts_with("Gather Merge")
            || trimmed.starts_with("Gather ")
            || trimmed == "Gather"
        {
            gather_depths.push(depth);
            continue;
        }
        if trimmed.starts_with("Custom Scan (")
            && trimmed.contains("GpuAccel")
            && !gather_depths.is_empty()
        {
            return true;
        }
    }
    false
}

/// Count the number of leading spaces on `line`, treating the EXPLAIN
/// `->` arrow as part of the indent.
fn leading_indent(line: &str) -> usize {
    let mut count = 0usize;
    for ch in line.chars() {
        // `->` is at depth N relative to its parent; we treat each of its
        // characters as one space so deeper sub-trees are strictly greater
        // than their parent's `->` line.
        if matches!(ch, ' ' | '-' | '>') {
            count += 1;
        } else {
            break;
        }
    }
    count
}

fn resident_boundary_audit(explain: &str) -> Vec<ResidentBoundaryFinding> {
    let lines: Vec<&str> = explain.lines().collect();
    let mut findings = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start_matches([' ', '-', '>']).trim_start();
        if !(trimmed.starts_with("Custom Scan (") && trimmed.contains("GpuAccel")) {
            continue;
        }
        let depth = leading_indent(line);
        let mut block = Vec::new();
        block.push(*line);
        for next in lines.iter().skip(idx + 1) {
            if next.trim().is_empty() {
                continue;
            }
            let next_depth = leading_indent(next);
            if next_depth <= depth {
                break;
            }
            block.push(*next);
        }
        findings.push(audit_resident_boundary_block(&block));
    }
    findings
}

fn audit_resident_boundary_block(block: &[&str]) -> ResidentBoundaryFinding {
    let node = block
        .first()
        .map(|line| line.trim().to_owned())
        .unwrap_or_default();
    let property_lines = direct_explain_property_lines(block);
    let strategy = explain_text_property(&property_lines, "Strategy");
    let pipeline = explain_bool_property(&property_lines, "GPU Resident Pipeline");
    let boundary_reason = explain_text_property(&property_lines, "GPU Resident Boundary");
    let proof_evidence = resident_proof_evidence_present(&property_lines);

    let (status, detail) = resident_boundary_status(strategy.as_deref(), pipeline, proof_evidence);
    ResidentBoundaryFinding {
        node,
        strategy,
        pipeline,
        boundary_reason,
        status,
        detail,
    }
}

fn direct_explain_property_lines<'a>(block: &[&'a str]) -> Vec<&'a str> {
    let Some(parent_depth) = block.first().map(|line| leading_indent(line)) else {
        return Vec::new();
    };
    let direct_depth = block
        .iter()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| leading_indent(line))
        .filter(|depth| *depth > parent_depth)
        .min();
    let Some(direct_depth) = direct_depth else {
        return Vec::new();
    };
    block
        .iter()
        .skip(1)
        .copied()
        .filter(|line| leading_indent(line) == direct_depth)
        .collect()
}

fn resident_boundary_status(
    strategy: Option<&str>,
    pipeline: Option<bool>,
    proof_evidence: bool,
) -> (ResidentBoundaryStatus, String) {
    let Some(strategy) = strategy else {
        return (
            ResidentBoundaryStatus::MissingStrategy,
            "selected pg_accel Custom Scan is missing `Strategy`".to_owned(),
        );
    };
    match pipeline {
        Some(true) if proof_evidence => (
            ResidentBoundaryStatus::Pass,
            format!("Strategy `{strategy}` reports a selected GPU-resident pipeline"),
        ),
        Some(true) => (
            ResidentBoundaryStatus::MissingProofEvidence,
            format!(
                "Strategy `{strategy}` reports `GPU Resident Pipeline: true` without \
                 resident proof version, stage-mask, and device-column evidence"
            ),
        ),
        Some(false) => (
            ResidentBoundaryStatus::NonResidentPipeline,
            format!(
                "Strategy `{strategy}` reports `GPU Resident Pipeline: false`; selected \
                 pg_accel Custom Scans must be GPU-resident"
            ),
        ),
        None => (
            ResidentBoundaryStatus::MissingPipeline,
            format!("Strategy `{strategy}` is missing `GPU Resident Pipeline: true`"),
        ),
    }
}

fn explain_text_property(block: &[&str], key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    block.iter().find_map(|line| {
        let trimmed = line.trim_start_matches([' ', '-', '>']).trim_start();
        trimmed
            .strip_prefix(&prefix)
            .map(|value| value.trim().trim_matches('"').to_owned())
            .filter(|value| !value.is_empty())
    })
}

fn explain_bool_property(block: &[&str], key: &str) -> Option<bool> {
    explain_text_property(block, key).and_then(|value| {
        if value.eq_ignore_ascii_case("true") {
            Some(true)
        } else if value.eq_ignore_ascii_case("false") {
            Some(false)
        } else {
            None
        }
    })
}

fn resident_proof_evidence_present(block: &[&str]) -> bool {
    explain_integer_property(block, "GPU Resident Proof Version")
        .is_some_and(|version| version >= 2)
        && explain_text_property(block, "GPU Resident Operator Class")
            .is_some_and(|class| !class.eq_ignore_ascii_case("unspecified"))
        && explain_integer_property(block, "GPU Resident Stage Mask").is_some_and(|mask| mask > 0)
        && explain_integer_property(block, "GPU Resident Device Columns")
            .is_some_and(|cols| cols > 0)
}

fn explain_integer_property(block: &[&str], key: &str) -> Option<i64> {
    explain_text_property(block, key).and_then(|value| value.parse().ok())
}

/// Run the full audit. Returns `Ok(true)` when every `RequiredToday` row
/// passed (the harness should exit 0); `Ok(false)` otherwise.
///
/// # Errors
///
/// Returns `Err` if the connection cannot be established, a fixture
/// statement fails, or any EXPLAIN query returns an error.
pub fn run_audit(connection: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let mut client = Client::connect(connection, NoTls)?;
    client.simple_query("SELECT 1 FROM pg_accel_stats() LIMIT 1")?;
    // 1. Idempotent fixture setup.
    for stmt in COMMON_FIXTURES {
        client
            .simple_query(stmt)
            .map_err(|e| format!("fixture `{stmt}` failed: {e}"))?;
    }
    // 2. Restore planner defaults once on the session.
    for stmt in DEFAULT_PLANNER_SETTINGS {
        client
            .simple_query(stmt)
            .map_err(|e| format!("session GUC `{stmt}` failed: {e}"))?;
    }

    let matrix = build_matrix();
    let mut outcomes: Vec<AuditOutcome> = Vec::with_capacity(matrix.len());

    for row in matrix {
        if let RatchetExpectation::Quarantined(reason) = &row.expectation {
            let reason = *reason;
            outcomes.push(AuditOutcome {
                name: row.name.to_owned(),
                description: row.description.to_owned(),
                expectation: RatchetExpectation::Quarantined(reason),
                shape_matched: false,
                resident_audit: Vec::new(),
                explain: format!(
                    "QUARANTINED: {reason}\n\
                     DECLINED: regression probe is temporarily quarantined while normal spatial production admission remains dark; Phase 7 must execute it.\n\
                     {query}\n",
                    query = row.query
                ),
            });
            continue;
        }

        // Per-row setup (e.g. enable_seqscan tweaks). Always wrap in a
        // savepoint-style reset by re-applying the global GUCs after.
        for stmt in &row.setup {
            client
                .simple_query(stmt)
                .map_err(|e| format!("row `{}` setup `{stmt}` failed: {e}", row.name))?;
        }
        let explain = fetch_explain(&mut client, row.query)?;
        let matched = shape_has_customscan_under_gather(&explain);
        // Reset row-local GUCs by re-applying the session defaults so the
        // next row starts from a known state.
        for stmt in DEFAULT_PLANNER_SETTINGS {
            client
                .simple_query(stmt)
                .map_err(|e| format!("session GUC reset `{stmt}` failed: {e}"))?;
        }
        // Reset enable_seqscan if any per-row setup turned it off.
        client
            .simple_query("SET enable_seqscan = on")
            .map_err(|e| format!("reset `enable_seqscan`: {e}"))?;
        outcomes.push(AuditOutcome {
            name: row.name.to_owned(),
            description: row.description.to_owned(),
            expectation: row.expectation,
            shape_matched: matched,
            resident_audit: resident_boundary_audit(&explain),
            explain,
        });
    }

    print_report(&outcomes);

    let any_required_failed = outcomes.iter().any(|o| {
        matches!(
            o.status(),
            RowStatus::Fail | RowStatus::FailResidentBoundary
        )
    });
    Ok(!any_required_failed)
}

/// Print a per-row report and a summary table to stdout.
fn print_report(outcomes: &[AuditOutcome]) {
    println!("=== EXPLAIN (VERBOSE) audit ===\n");
    for o in outcomes {
        let prefix = match o.status() {
            RowStatus::Pass => "[PASS]".to_owned(),
            RowStatus::Fail => "[FAIL]".to_owned(),
            RowStatus::FailResidentBoundary => "[FAIL-resident-boundary]".to_owned(),
            RowStatus::SkipGated => match &o.expectation {
                RatchetExpectation::RequiredAfterPhase(p) => {
                    format!("[SKIP-gated-by-{p}]")
                }
                _ => "[SKIP]".to_owned(),
            },
            RowStatus::OptionalNotMet => "[OPTIONAL-NOT-MET]".to_owned(),
            RowStatus::Quarantined => "[QUARANTINED-declined]".to_owned(),
        };
        println!("{prefix} {} — {}", o.name, o.description);
        // Indent the EXPLAIN output for readability.
        for line in o.explain.lines() {
            println!("    {line}");
        }
        print_resident_boundary_audit(o);
        match o.status() {
            RowStatus::Pass
            | RowStatus::SkipGated
            | RowStatus::OptionalNotMet
            | RowStatus::Quarantined => {}
            RowStatus::Fail => {
                println!(
                    "    !! RequiredToday row failed: no `Custom Scan (GpuAccel...)` \
                     found inside a Gather / Gather Merge subtree."
                );
            }
            RowStatus::FailResidentBoundary => {
                println!(
                    "    !! Resident-boundary audit failed: every selected pg_accel Custom \
                     Scan must report `GPU Resident Pipeline: true`."
                );
            }
        }
        println!();
    }

    println!("=== Ratchet summary ===");
    for o in outcomes {
        let tag = match &o.expectation {
            RatchetExpectation::RequiredToday => "required-today".to_owned(),
            RatchetExpectation::RequiredAfterPhase(p) => format!("required-after({p})"),
            RatchetExpectation::OptionalForever => "optional-forever".to_owned(),
            RatchetExpectation::Quarantined(p) => format!("quarantined({p})"),
        };
        let status = match o.status() {
            RowStatus::Pass => "PASS",
            RowStatus::Fail => "FAIL",
            RowStatus::FailResidentBoundary => "FAIL-resident-boundary",
            RowStatus::SkipGated => "skip",
            RowStatus::OptionalNotMet => "optional-not-met",
            RowStatus::Quarantined => "quarantined",
        };
        println!("  {:32}  {:32}  {status}", o.name, tag);
    }
}

fn print_resident_boundary_audit(outcome: &AuditOutcome) {
    if outcome.resident_audit.is_empty() {
        println!("    resident-boundary audit: no selected pg_accel Custom Scan");
        return;
    }
    for finding in &outcome.resident_audit {
        let status = match finding.status {
            ResidentBoundaryStatus::Pass => "PASS",
            ResidentBoundaryStatus::MissingStrategy => "FAIL-missing-strategy",
            ResidentBoundaryStatus::MissingPipeline => "FAIL-missing-pipeline",
            ResidentBoundaryStatus::MissingProofEvidence => "FAIL-missing-proof-evidence",
            ResidentBoundaryStatus::NonResidentPipeline => "FAIL-non-resident-pipeline",
        };
        let strategy = finding.strategy.as_deref().unwrap_or("-");
        let pipeline = finding
            .pipeline
            .map_or_else(|| "-".to_owned(), |value| value.to_string());
        let boundary = finding.boundary_reason.as_deref().unwrap_or("-");
        println!(
            "    resident-boundary audit: [{status}] node=`{node}` strategy=`{strategy}` \
             pipeline=`{pipeline}` boundary=`{boundary}`",
            node = finding.node,
        );
        println!("        {}", finding.detail);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Custom Scan (GpuAccelAgg)` directly under a `Gather` line is the
    /// happy path produced by the parallel SUM workload.
    const FIXTURE_SUM: &str = "\
 Finalize Aggregate  (cost=50968.73..50968.74 rows=1 width=4)
   Output: sum(v)
   ->  Gather  (cost=66560.22..50968.73 rows=1 width=4)
         Output: (PARTIAL sum(v))
         Workers Planned: 8
         ->  Custom Scan (GpuAccelAgg)  (cost=66560.22..50968.73 rows=1 width=4)
               Output: (PARTIAL sum(v))
               Strategy: GpuAgg
               ->  Parallel Seq Scan on public.bench_f32_10m  (cost=0.00..66555.22 rows=1250022 width=4)
                     Output: v
";

    /// A plain parallel partial-agg plan (no pg_accel injection).
    const FIXTURE_AVG_NO_ACCEL: &str = "\
 Finalize Aggregate  (cost=69680.31..69680.32 rows=1 width=16)
   Output: avg(v), stddev(v)
   ->  Gather  (cost=69680.27..69680.28 rows=8 width=64)
         Output: (PARTIAL avg(v)), (PARTIAL stddev(v))
         Workers Planned: 8
         ->  Partial Aggregate  (cost=69680.27..69680.28 rows=1 width=64)
               Output: PARTIAL avg(v), PARTIAL stddev(v)
               ->  Parallel Seq Scan on public.bench_f32_10m  (cost=0.00..66555.22 rows=1250022 width=4)
                     Output: id, v, dim
";

    /// A serial CustomScan plan (no Gather above the CustomScan node).
    /// Should NOT match — the spec requires the Gather wrapper.
    const FIXTURE_SERIAL_CUSTOMSCAN: &str = "\
 Custom Scan (GpuAccelScan)  (cost=5.00..219062.89 rows=10000175 width=16)
   Output: id, v, dim
   Strategy: GpuSort
   ->  Seq Scan on public.bench_f32_10m  (cost=0.00..154056.75 rows=10000175 width=16)
         Output: id, v, dim
";

    #[test]
    fn detects_customscan_inside_gather() {
        assert!(shape_has_customscan_under_gather(FIXTURE_SUM));
    }

    #[test]
    fn rejects_plan_without_pg_accel() {
        assert!(!shape_has_customscan_under_gather(FIXTURE_AVG_NO_ACCEL));
    }

    #[test]
    fn rejects_serial_customscan() {
        assert!(!shape_has_customscan_under_gather(
            FIXTURE_SERIAL_CUSTOMSCAN
        ));
    }

    #[test]
    fn ratchet_status_required_today_pass() {
        let o = AuditOutcome {
            name: "x".into(),
            description: "x".into(),
            expectation: RatchetExpectation::RequiredToday,
            shape_matched: true,
            explain: String::new(),
            resident_audit: Vec::new(),
        };
        assert_eq!(o.status(), RowStatus::Pass);
    }

    #[test]
    fn ratchet_status_required_today_fail() {
        let o = AuditOutcome {
            name: "x".into(),
            description: "x".into(),
            expectation: RatchetExpectation::RequiredToday,
            shape_matched: false,
            explain: String::new(),
            resident_audit: Vec::new(),
        };
        assert_eq!(o.status(), RowStatus::Fail);
    }

    #[test]
    fn ratchet_status_gated_skips_regardless() {
        let pass = AuditOutcome {
            name: "x".into(),
            description: "x".into(),
            expectation: RatchetExpectation::RequiredAfterPhase("phase X"),
            shape_matched: true,
            explain: String::new(),
            resident_audit: Vec::new(),
        };
        let fail = AuditOutcome {
            name: "x".into(),
            description: "x".into(),
            expectation: RatchetExpectation::RequiredAfterPhase("phase X"),
            shape_matched: false,
            explain: String::new(),
            resident_audit: Vec::new(),
        };
        assert_eq!(pass.status(), RowStatus::SkipGated);
        assert_eq!(fail.status(), RowStatus::SkipGated);
    }

    #[test]
    fn matrix_covers_expected_rows() {
        // Ship-gate matrix size: Phase 9 rows plus visible spatial crash gates.
        assert_eq!(build_matrix().len(), 11);
    }

    #[test]
    fn orderby_row_exercises_gated_full_sort_lane() {
        let matrix = build_matrix();
        let row = matrix
            .iter()
            .find(|row| row.name == "parallel_orderby")
            .expect("parallel_orderby row missing");
        assert_eq!(row.description, "ORDER BY v — full sort");
        assert_eq!(row.query, "SELECT * FROM bench_f32_10m ORDER BY v");
        assert!(matches!(
            row.expectation,
            RatchetExpectation::RequiredAfterPhase("full-sort GPU algorithm/costing")
        ));
    }

    #[test]
    fn join_row_is_gated_until_hashjoin_wins() {
        let matrix = build_matrix();
        let row = matrix
            .iter()
            .find(|row| row.name == "parallel_join")
            .expect("parallel_join row missing");
        assert_eq!(row.description, "Plain JOIN — parallel hash join");
        assert!(matches!(
            row.expectation,
            RatchetExpectation::RequiredAfterPhase("hashjoin cost/kernel calibration")
        ));
    }

    #[test]
    fn ratchet_status_optional_forever() {
        // Exercise the OptionalForever variant — present in the public
        // ratchet API for rows that are never required (ban #2 forbids
        // bypassing failures via this variant; we keep it so the type
        // surface is complete for future exotic plan shapes).
        let pass = AuditOutcome {
            name: "x".into(),
            description: "x".into(),
            expectation: RatchetExpectation::OptionalForever,
            shape_matched: true,
            explain: String::new(),
            resident_audit: Vec::new(),
        };
        let nope = AuditOutcome {
            name: "x".into(),
            description: "x".into(),
            expectation: RatchetExpectation::OptionalForever,
            shape_matched: false,
            explain: String::new(),
            resident_audit: Vec::new(),
        };
        assert_eq!(pass.status(), RowStatus::Pass);
        assert_eq!(nope.status(), RowStatus::OptionalNotMet);
    }

    #[test]
    fn ratchet_status_quarantined() {
        let o = AuditOutcome {
            name: "x".into(),
            description: "x".into(),
            expectation: RatchetExpectation::Quarantined("quarantined workload"),
            shape_matched: true,
            explain: String::new(),
            resident_audit: Vec::new(),
        };
        assert_eq!(o.status(), RowStatus::Quarantined);
    }

    #[test]
    fn resident_boundary_audit_accepts_resident_pipeline() {
        let plan = "\
Finalize Aggregate
  ->  Gather
        ->  Custom Scan (GpuAccelAgg)
              Strategy: GpuAgg
              GPU Resident Pipeline: true
              GPU Resident Proof Version: 2
              GPU Resident Operator Class: resident_groupagg
              GPU Resident Stage Mask: 5
              GPU Resident Device Columns: 2
              ->  Parallel Seq Scan on public.bench
";

        let findings = resident_boundary_audit(plan);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].status, ResidentBoundaryStatus::Pass);
        assert_eq!(findings[0].strategy.as_deref(), Some("GpuAgg"));
        assert_eq!(findings[0].pipeline, Some(true));
        assert_eq!(findings[0].boundary_reason.as_deref(), None);
    }

    #[test]
    fn resident_boundary_audit_rejects_true_without_proof_metadata() {
        let plan = "\
Finalize Aggregate
  ->  Gather
        ->  Custom Scan (GpuAccelAgg)
              Strategy: GpuAgg
              GPU Resident Pipeline: true
              ->  Parallel Seq Scan on public.bench
";

        let findings = resident_boundary_audit(plan);

        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].status,
            ResidentBoundaryStatus::MissingProofEvidence
        );
    }

    #[test]
    fn resident_boundary_audit_rejects_true_without_operator_class() {
        let plan = "\
Finalize Aggregate
  ->  Gather
        ->  Custom Scan (GpuAccelAgg)
              Strategy: GpuAgg
              GPU Resident Pipeline: true
              GPU Resident Proof Version: 2
              GPU Resident Stage Mask: 5
              GPU Resident Device Columns: 2
              ->  Parallel Seq Scan on public.bench
";

        let findings = resident_boundary_audit(plan);

        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].status,
            ResidentBoundaryStatus::MissingProofEvidence
        );
    }

    #[test]
    fn resident_boundary_audit_rejects_nonresident_pipeline() {
        let plan = "\
Finalize Aggregate
  ->  Gather
        ->  Custom Scan (GpuAccelAgg)
              Strategy: GpuAgg
              GPU Resident Pipeline: false
              ->  Parallel Seq Scan on public.bench
";

        let findings = resident_boundary_audit(plan);

        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].status,
            ResidentBoundaryStatus::NonResidentPipeline
        );
    }

    #[test]
    fn resident_boundary_audit_rejects_nonresident_pipeline_even_with_boundary() {
        let plan = "\
Finalize Aggregate
  ->  Gather
        ->  Custom Scan (GpuAccelAgg)
              Strategy: GpuAgg
              GPU Resident Pipeline: false
              GPU Resident Boundary: stale text
              ->  Parallel Seq Scan on public.bench
";

        let findings = resident_boundary_audit(plan);

        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].status,
            ResidentBoundaryStatus::NonResidentPipeline
        );
    }

    #[test]
    fn resident_boundary_audit_does_not_let_child_boundary_satisfy_parent() {
        let plan = "\
Custom Scan (GpuAccelJoin)
  Strategy: GpuJoin
  ->  Custom Scan (GpuAccelAgg)
        Strategy: GpuAgg
        GPU Resident Pipeline: true
        GPU Resident Proof Version: 2
        GPU Resident Operator Class: resident_groupagg
        GPU Resident Stage Mask: 5
        GPU Resident Device Columns: 2
        ->  Seq Scan on public.bench
";

        let findings = resident_boundary_audit(plan);

        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].strategy.as_deref(), Some("GpuJoin"));
        assert_eq!(
            findings[0].status,
            ResidentBoundaryStatus::MissingPipeline,
            "parent Custom Scan must not borrow nested child boundary properties"
        );
        assert_eq!(findings[1].strategy.as_deref(), Some("GpuAgg"));
        assert_eq!(findings[1].status, ResidentBoundaryStatus::Pass);
    }

    #[test]
    fn resident_boundary_failure_overrides_gated_shape_status() {
        let o = AuditOutcome {
            name: "x".into(),
            description: "x".into(),
            expectation: RatchetExpectation::RequiredAfterPhase("future phase"),
            shape_matched: false,
            explain: String::new(),
            resident_audit: vec![ResidentBoundaryFinding {
                node: "Custom Scan (GpuAccelAgg)".to_owned(),
                strategy: Some("GpuAgg".to_owned()),
                pipeline: Some(false),
                boundary_reason: None,
                status: ResidentBoundaryStatus::NonResidentPipeline,
                detail: "non-resident pipeline".to_owned(),
            }],
        };

        assert_eq!(o.status(), RowStatus::FailResidentBoundary);
    }

    #[test]
    fn spatial_100k_regression_probe_rows_are_quarantined() {
        let matrix = build_matrix();
        let rows: Vec<_> = matrix
            .iter()
            .filter(|row| row.name.starts_with("spatial_100k_"))
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.iter().map(|row| row.name).collect::<Vec<_>>(),
            [
                "spatial_100k_simple_regression_probe",
                "spatial_100k_coop1024_regression_probe",
            ]
        );
        assert!(SPATIAL_100K_QUARANTINE_REASON.contains("Phase 7 must flip this row"));
        for row in rows {
            assert!(row.description.contains("declined/quarantined"));
            assert!(row.query.contains("rows=100000"));
            assert!(matches!(
                row.expectation,
                RatchetExpectation::Quarantined(reason)
                    if reason == SPATIAL_100K_QUARANTINE_REASON
            ));
        }
    }

    fn finding(status: ResidentBoundaryStatus) -> ResidentBoundaryFinding {
        ResidentBoundaryFinding {
            node: "Custom Scan (GpuAccelAgg)".to_owned(),
            strategy: (status != ResidentBoundaryStatus::MissingStrategy)
                .then(|| "GpuAgg".to_owned()),
            pipeline: match status {
                ResidentBoundaryStatus::Pass | ResidentBoundaryStatus::MissingProofEvidence => {
                    Some(true)
                }
                ResidentBoundaryStatus::NonResidentPipeline => Some(false),
                ResidentBoundaryStatus::MissingStrategy
                | ResidentBoundaryStatus::MissingPipeline => None,
            },
            boundary_reason: Some("test boundary".to_owned()),
            status,
            detail: format!("detail for {status:?}"),
        }
    }

    #[test]
    fn property_parsers_and_boundary_statuses_fail_closed() {
        assert!(direct_explain_property_lines(&[]).is_empty());
        assert!(direct_explain_property_lines(&["Custom Scan (GpuAccelAgg)"]).is_empty());
        assert_eq!(
            explain_text_property(&["  Strategy: GpuAgg"], "Strategy"),
            Some("GpuAgg".to_owned())
        );
        assert_eq!(
            explain_text_property(&["  Strategy GpuAgg"], "Strategy"),
            None
        );
        assert_eq!(
            explain_bool_property(&["  Enabled: TRUE"], "Enabled"),
            Some(true)
        );
        assert_eq!(
            explain_bool_property(&["  Enabled: false"], "Enabled"),
            Some(false)
        );
        assert_eq!(
            explain_bool_property(&["  Enabled: maybe"], "Enabled"),
            None
        );
        assert_eq!(
            explain_integer_property(&["  Count: -7"], "Count"),
            Some(-7)
        );
        assert_eq!(explain_integer_property(&["  Count: seven"], "Count"), None);

        let cases = [
            (
                resident_boundary_status(None, Some(true), true),
                ResidentBoundaryStatus::MissingStrategy,
                "missing `Strategy`",
            ),
            (
                resident_boundary_status(Some("GpuAgg"), None, true),
                ResidentBoundaryStatus::MissingPipeline,
                "missing `GPU Resident Pipeline: true`",
            ),
            (
                resident_boundary_status(Some("GpuAgg"), Some(true), false),
                ResidentBoundaryStatus::MissingProofEvidence,
                "without resident proof",
            ),
            (
                resident_boundary_status(Some("GpuAgg"), Some(false), true),
                ResidentBoundaryStatus::NonResidentPipeline,
                "reports `GPU Resident Pipeline: false`",
            ),
            (
                resident_boundary_status(Some("GpuAgg"), Some(true), true),
                ResidentBoundaryStatus::Pass,
                "selected GPU-resident pipeline",
            ),
        ];
        for ((actual, detail), expected, fragment) in cases {
            assert_eq!(actual, expected);
            assert!(detail.contains(fragment));
        }
    }

    #[test]
    fn proof_evidence_requires_every_positive_resident_field() {
        let complete = [
            "GPU Resident Proof Version: 2",
            "GPU Resident Operator Class: resident_groupagg",
            "GPU Resident Stage Mask: 5",
            "GPU Resident Device Columns: 2",
        ];
        assert!(resident_proof_evidence_present(&complete));
        for index in 0..complete.len() {
            let incomplete = complete
                .iter()
                .enumerate()
                .filter_map(|(candidate, line)| (candidate != index).then_some(*line))
                .collect::<Vec<_>>();
            assert!(!resident_proof_evidence_present(&incomplete));
        }
        assert!(!resident_proof_evidence_present(&[
            "GPU Resident Proof Version: 0",
            "GPU Resident Operator Class: resident_groupagg",
            "GPU Resident Stage Mask: 5",
            "GPU Resident Device Columns: 2",
        ]));
    }

    #[test]
    fn shape_parser_handles_gather_merge_scope_exit_and_arrow_indentation() {
        assert_eq!(leading_indent("    ->  Node"), 8);
        assert_eq!(leading_indent("Node"), 0);
        assert!(shape_has_customscan_under_gather(
            "Gather Merge\n  ->  Sort\n        ->  Custom Scan (GpuAccelSort)\n"
        ));
        assert!(!shape_has_customscan_under_gather(
            "Gather\n  ->  Seq Scan on t\nCustom Scan (GpuAccelAgg)\n"
        ));
        assert!(!shape_has_customscan_under_gather(
            "Gather\n  ->  Custom Scan (OtherProvider)\n"
        ));
        assert!(!shape_has_customscan_under_gather("\n\nGather\n"));
    }

    #[test]
    fn report_renderer_handles_every_ratchet_and_boundary_status() {
        let outcomes = vec![
            AuditOutcome {
                name: "pass".to_owned(),
                description: "required success".to_owned(),
                expectation: RatchetExpectation::RequiredToday,
                shape_matched: true,
                explain: "Gather\n  -> Custom Scan (GpuAccelAgg)\n".to_owned(),
                resident_audit: vec![finding(ResidentBoundaryStatus::Pass)],
            },
            AuditOutcome {
                name: "fail".to_owned(),
                description: "required failure".to_owned(),
                expectation: RatchetExpectation::RequiredToday,
                shape_matched: false,
                explain: "Seq Scan on t\n".to_owned(),
                resident_audit: Vec::new(),
            },
            AuditOutcome {
                name: "boundary".to_owned(),
                description: "resident failures".to_owned(),
                expectation: RatchetExpectation::RequiredToday,
                shape_matched: true,
                explain: "Custom Scan (GpuAccelAgg)\n".to_owned(),
                resident_audit: vec![
                    finding(ResidentBoundaryStatus::MissingStrategy),
                    finding(ResidentBoundaryStatus::MissingPipeline),
                    finding(ResidentBoundaryStatus::MissingProofEvidence),
                    finding(ResidentBoundaryStatus::NonResidentPipeline),
                ],
            },
            AuditOutcome {
                name: "gated".to_owned(),
                description: "future lane".to_owned(),
                expectation: RatchetExpectation::RequiredAfterPhase("Phase X"),
                shape_matched: false,
                explain: String::new(),
                resident_audit: Vec::new(),
            },
            AuditOutcome {
                name: "optional".to_owned(),
                description: "optional lane".to_owned(),
                expectation: RatchetExpectation::OptionalForever,
                shape_matched: false,
                explain: String::new(),
                resident_audit: Vec::new(),
            },
            AuditOutcome {
                name: "quarantine".to_owned(),
                description: "quarantined lane".to_owned(),
                expectation: RatchetExpectation::Quarantined("known issue"),
                shape_matched: false,
                explain: "QUARANTINED\n".to_owned(),
                resident_audit: Vec::new(),
            },
        ];

        assert_eq!(outcomes[0].status(), RowStatus::Pass);
        assert_eq!(outcomes[1].status(), RowStatus::Fail);
        assert_eq!(outcomes[2].status(), RowStatus::FailResidentBoundary);
        assert_eq!(outcomes[3].status(), RowStatus::SkipGated);
        assert_eq!(outcomes[4].status(), RowStatus::OptionalNotMet);
        assert_eq!(outcomes[5].status(), RowStatus::Quarantined);
        print_report(&outcomes);
    }
}
