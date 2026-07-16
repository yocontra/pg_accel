//! EXPLAIN-plan-shape integration assertions.
//!
//! Ensures admitted shapes emit the intended `CustomScan(pg_accel)` plans, and
//! roadmap or crash-gated shapes stay on native PostgreSQL plans with explicit
//! decline evidence. Without these checks, planner regressions can silently
//! select the wrong execution path and only show up as unexpected benchmark
//! movement.
//!
//! Gated behind `#[cfg(feature = "integration_tests")]` so the default
//! `cargo test -p pg_accel_bench` invocation stays hermetic.

#![allow(dead_code)]

use std::{fmt::Write as _, path::Path};

use postgres::{Client, NoTls};

#[cfg(all(test, feature = "integration_tests"))]
use crate::integration_connection::live_pg_test_lock;
use crate::integration_connection::test_connection;
use crate::workloads::parallel_stress::bench_f32_10m_setup_sql;

const DIRECT_COST_OVERRIDE_SOURCE: &str = "pg_accel/src/tests/phase2_cache.rs";
const DIRECT_COST_OVERRIDE_ALLOW_MARKER: &str =
    "// admission-audit-allow: direct cost_multiplier executor contract test";
const DIRECT_COST_DEFAULT_PROOF_LINE: &str =
    "fn assert_default_cost_decline_matches_native(query: &str) {";
const DIRECT_MIN_BATCH_SOURCE: &str = "pg_accel/src/tests/mod.rs";
const DIRECT_MIN_BATCH_ALLOW_MARKER: &str =
    "// admission-audit-allow: direct min_batch_size GUC contract test";

fn numeric_cost_source_line(value: &str) -> String {
    [
        "Spi::run(\"SET LOCAL pg_accel.",
        "cost_multiplier = ",
        value,
        "\")",
    ]
    .concat()
}

fn is_sql_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn ascii_keyword_end(source: &[u8], start: usize, keyword: &[u8]) -> Option<usize> {
    let end = start.checked_add(keyword.len())?;
    let candidate = source.get(start..end)?;
    if (start > 0 && is_sql_identifier_byte(source[start - 1]))
        || source
            .get(end)
            .is_some_and(|byte| is_sql_identifier_byte(*byte))
        || !candidate.eq_ignore_ascii_case(keyword)
    {
        return None;
    }
    Some(end)
}

fn fragmented_keyword_match(
    source: &[u8],
    start: usize,
    keyword: &[u8],
) -> Option<(usize, Vec<usize>)> {
    if start > 0 && is_sql_identifier_byte(source[start - 1]) {
        return None;
    }
    let mut cursor = start;
    let mut positions = Vec::with_capacity(keyword.len());
    for expected in keyword {
        cursor = skip_source_construction_separators(source, cursor);
        if !source
            .get(cursor)
            .is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
        {
            return None;
        }
        positions.push(cursor);
        cursor += 1;
    }
    Some((cursor, positions))
}

fn fragmented_keyword_end(source: &[u8], start: usize, keyword: &[u8]) -> Option<usize> {
    fragmented_keyword_match(source, start, keyword).map(|(end, _positions)| end)
}

fn enclosing_rust_string_start(source: &str, target: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut cursor = 0;
    let mut string_start = None;
    while cursor < target {
        if bytes[cursor] == b'\\' && string_start.is_some() {
            cursor = (cursor + 2).min(target);
            continue;
        }
        if bytes[cursor] == b'"' {
            string_start = if string_start.is_some() {
                None
            } else {
                Some(cursor)
            };
        }
        cursor += 1;
    }
    string_start
}

fn utf8_suffix_with_byte_limit(source: &str, max_bytes: usize) -> &str {
    let minimum = source.len().saturating_sub(max_bytes);
    let start = (minimum..=source.len())
        .find(|index| source.is_char_boundary(*index))
        .unwrap_or(source.len());
    source.get(start..).unwrap_or_default()
}

fn diagnostic_rust_string(source: &str, string_start: usize) -> bool {
    let Some(prefix) = source.get(..string_start) else {
        return false;
    };
    let prefix = utf8_suffix_with_byte_limit(prefix, 96);
    let compact = prefix.split_whitespace().collect::<String>();
    compact.ends_with("panic!(")
        || compact.ends_with(".expect(")
        || compact.ends_with("assert!(")
        || compact.ends_with("assert_eq!(")
}

fn is_rust_source(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
}

fn skip_source_construction_separators(source: &[u8], mut cursor: usize) -> usize {
    loop {
        while source.get(cursor).is_some_and(|byte| {
            byte.is_ascii_whitespace()
                || matches!(
                    byte,
                    b'"' | b'\'' | b',' | b'(' | b')' | b'[' | b']' | b'\\' | b'#'
                )
        }) {
            cursor += 1;
        }
        if source.get(cursor..cursor + 2) == Some(b"/*") {
            cursor += 2;
            while cursor + 1 < source.len() && source.get(cursor..cursor + 2) != Some(b"*/") {
                cursor += 1;
            }
            cursor = (cursor + 2).min(source.len());
            continue;
        }
        if source.get(cursor..cursor + 2) == Some(b"--") {
            cursor += 2;
            while source.get(cursor).is_some_and(|byte| *byte != b'\n') {
                cursor += 1;
            }
            continue;
        }
        if source.get(cursor..cursor + 2) == Some(b"//") {
            cursor += 2;
            while source.get(cursor).is_some_and(|byte| *byte != b'\n') {
                cursor += 1;
            }
            continue;
        }
        break;
    }
    cursor
}

fn set_config_argument_prefix(source: &[u8], mut cursor: usize, target: usize) -> bool {
    let mut projected = Vec::new();
    while cursor < target {
        let next = skip_source_construction_separators(source, cursor);
        if next != cursor {
            cursor = next;
            continue;
        }
        let Some(byte) = source.get(cursor).copied() else {
            return false;
        };
        if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'(') {
            projected.push(byte.to_ascii_lowercase());
        } else if !matches!(byte, b'&' | b'+') {
            return false;
        }
        cursor += 1;
    }
    projected.is_empty() || projected == b"pg_accel."
}

fn fragmented_set_config_call_before(name: &str, source: &str, target: usize) -> bool {
    let bytes = source.as_bytes();
    for start in 0..target {
        let Some((end, positions)) = fragmented_keyword_match(bytes, start, b"set_config") else {
            continue;
        };
        if end > target || !set_config_argument_prefix(bytes, end, target) {
            continue;
        }
        if is_rust_source(name)
            && positions
                .iter()
                .any(|position| enclosing_rust_string_start(source, *position).is_none())
        {
            continue;
        }
        return true;
    }
    false
}

fn starts_numeric_literal(source: &[u8], cursor: usize) -> bool {
    match source.get(cursor) {
        Some(byte) if byte.is_ascii_digit() => true,
        Some(b'.') => source.get(cursor + 1).is_some_and(u8::is_ascii_digit),
        Some(b'-' | b'+') => source.get(cursor + 1).is_some_and(|byte| {
            byte.is_ascii_digit()
                || (*byte == b'.' && source.get(cursor + 2).is_some_and(u8::is_ascii_digit))
        }),
        _ => false,
    }
}

/// Return the source-line index of every non-default assignment to a protected
/// planner setting. This intentionally audits source construction, not only
/// rendered SQL: it sees `=`, `TO`, `set_config(..., value, ...)`, adjacent
/// Rust string fragments, and dynamic `format!` placeholders. Any assignment
/// value other than the literal `DEFAULT` is a pin and must be explicitly
/// approved by the narrow contract-test allowlist.
fn nondefault_setting_pin_lines(name: &str, source: &str, setting: &str) -> Vec<usize> {
    let bytes = source.as_bytes();
    let mut matches = Vec::new();

    for start in 0..bytes.len() {
        let Some(mut cursor) = fragmented_keyword_end(bytes, start, setting.as_bytes()) else {
            continue;
        };
        let rust_string = enclosing_rust_string_start(source, start);
        if is_rust_source(name)
            && rust_string.is_none_or(|opening| diagnostic_rust_string(source, opening))
        {
            continue;
        }
        cursor = skip_source_construction_separators(bytes, cursor);
        let has_assignment_operator;
        if bytes.get(cursor) == Some(&b'=') {
            cursor += 1;
            has_assignment_operator = true;
        } else if let Some(end) = ascii_keyword_end(bytes, cursor, b"to") {
            cursor = end;
            has_assignment_operator = true;
        } else if let Some(end) = ascii_keyword_end(bytes, cursor, b"from") {
            cursor = end;
            has_assignment_operator = true;
        } else {
            has_assignment_operator = false;
        }
        cursor = skip_source_construction_separators(bytes, cursor);

        let set_config_assignment = fragmented_set_config_call_before(name, source, start);
        if !has_assignment_operator && !set_config_assignment {
            continue;
        }
        if ascii_keyword_end(bytes, cursor, b"default").is_some() {
            continue;
        }
        matches.push(
            source
                .get(..start)
                .unwrap_or_default()
                .matches('\n')
                .count(),
        );
    }

    matches.sort_unstable();
    matches.dedup();
    matches
}

fn numeric_cost_pin_lines(source: &str) -> Vec<usize> {
    nondefault_setting_pin_lines("fixture.sql", source, "cost_multiplier")
}

fn direct_min_batch_override_allowed(
    name: &str,
    lines: &[&str],
    line_index: usize,
    line: &str,
) -> bool {
    let allowed_lines = [
        [
            "Spi::run(\"SET pg_accel.",
            "min_batch_size = 512\").expect(\"SET min_batch_size should succeed\");",
        ]
        .concat(),
        ["Spi::run(\"SET pg_accel.", "min_batch_size = 256\")"].concat(),
        [
            "Spi::run(&format!(\"SET pg_accel.",
            "min_batch_size = {bs}\"))",
        ]
        .concat(),
    ];
    name == DIRECT_MIN_BATCH_SOURCE
        && allowed_lines.iter().any(|allowed| line.trim() == allowed)
        && line_index
            .checked_sub(1)
            .and_then(|previous| lines.get(previous))
            .is_some_and(|previous| previous.trim() == DIRECT_MIN_BATCH_ALLOW_MARKER)
}

fn direct_cost_override_allowed(name: &str, lines: &[&str], line_index: usize, line: &str) -> bool {
    name == DIRECT_COST_OVERRIDE_SOURCE
        && line.trim() == numeric_cost_source_line("0.1")
        && line_index
            .checked_sub(1)
            .and_then(|previous| lines.get(previous))
            .is_some_and(|previous| previous.trim() == DIRECT_COST_OVERRIDE_ALLOW_MARKER)
        && lines
            .iter()
            .any(|candidate| candidate.trim() == DIRECT_COST_DEFAULT_PROOF_LINE)
}

#[derive(Debug, PartialEq, Eq)]
struct NumericCostPinAudit {
    approved_lines: Vec<usize>,
    rejected_lines: Vec<usize>,
}

impl NumericCostPinAudit {
    fn passes_for_source(&self, name: &str) -> bool {
        self.rejected_lines.is_empty()
            && if name == DIRECT_COST_OVERRIDE_SOURCE {
                self.approved_lines.len() == 1
            } else {
                self.approved_lines.is_empty()
            }
    }
}

fn audit_numeric_cost_pins(name: &str, source: &str) -> NumericCostPinAudit {
    let lines = source.lines().collect::<Vec<_>>();
    let mut approved_lines = Vec::new();
    let mut rejected_lines = Vec::new();
    for line_index in nondefault_setting_pin_lines(name, source, "cost_multiplier") {
        let line = lines.get(line_index).copied().unwrap_or_default();
        if direct_cost_override_allowed(name, &lines, line_index, line) {
            approved_lines.push(line_index);
        } else {
            rejected_lines.push(line_index);
        }
    }
    NumericCostPinAudit {
        approved_lines,
        rejected_lines,
    }
}

#[test]
fn released_planner_evidence_uses_default_admission_settings() {
    const EVIDENCE_SOURCES: &[(&str, &str)] = &[
        ("plan_shape_test.rs", include_str!("plan_shape_test.rs")),
        ("explain_audit.rs", include_str!("explain_audit.rs")),
        (
            "parallel_stress.rs",
            include_str!("workloads/parallel_stress.rs"),
        ),
        (
            "parallel_hashjoin_rebuild_decline.rs",
            include_str!("workloads/parallel_hashjoin_rebuild_decline.rs"),
        ),
        (
            "spatial_selectivity_sweep.rs",
            include_str!("workloads/spatial_selectivity_sweep.rs"),
        ),
        (
            "h3_protection_test.rs",
            include_str!("h3_protection_test.rs"),
        ),
        (
            "parallel_stress_test.rs",
            include_str!("parallel_stress_test.rs"),
        ),
        (
            "resident_concurrency_test.rs",
            include_str!("resident_concurrency_test.rs"),
        ),
        (
            "pg_accel/src/tests/phase2_cache.rs",
            include_str!("../../pg_accel/src/tests/phase2_cache.rs"),
        ),
        (
            "pg_accel/src/tests/mod.rs",
            include_str!("../../pg_accel/src/tests/mod.rs"),
        ),
        (
            "sql/tests/07_regression_small.sql",
            include_str!("../../sql/tests/07_regression_small.sql"),
        ),
        (
            "sql/tests/09_explain_plans.sql",
            include_str!("../../sql/tests/09_explain_plans.sql"),
        ),
        (
            "sql/tests/50_hash_agg_groupby.sql",
            include_str!("../../sql/tests/50_hash_agg_groupby.sql"),
        ),
        (
            "sql/tests/88_concurrent_features.sql",
            include_str!("../../sql/tests/88_concurrent_features.sql"),
        ),
    ];
    let parallel_settings = [
        "max_parallel_workers_per_gather",
        "min_parallel_table_scan_size",
        "min_parallel_index_scan_size",
        "parallel_setup_cost",
        "parallel_tuple_cost",
    ];

    let mut approved_cost_overrides = 0;
    let mut approved_min_batch_overrides = 0;
    for (name, source) in EVIDENCE_SOURCES {
        // The first section of this file defines and adversarially tests this
        // audit. Its released plan evidence begins at `connect`; all other
        // evidence sources are audited in full.
        let auditable_source = if *name == "plan_shape_test.rs" {
            let evidence_start = ["fn con", "nect() -> Client {"].concat();
            source
                .split_once(&evidence_start)
                .map_or(*source, |(_, body)| body)
        } else {
            source
        };
        let numeric_cost_audit = audit_numeric_cost_pins(name, auditable_source);
        assert!(
            numeric_cost_audit.rejected_lines.is_empty(),
            "released planner evidence `{name}` line {} pins a numeric cost multiplier without the exact direct-contract allow marker",
            numeric_cost_audit
                .rejected_lines
                .first()
                .copied()
                .unwrap_or_default()
                + 1
        );
        assert!(
            numeric_cost_audit.passes_for_source(name),
            "released planner evidence `{name}` has {} approved numeric cost override(s); expected {}",
            numeric_cost_audit.approved_lines.len(),
            usize::from(*name == DIRECT_COST_OVERRIDE_SOURCE)
        );
        approved_cost_overrides += numeric_cost_audit.approved_lines.len();
        let lines = auditable_source.lines().collect::<Vec<_>>();
        for line_index in nondefault_setting_pin_lines(name, auditable_source, "min_batch_size") {
            let line = lines.get(line_index).copied().unwrap_or_default();
            assert!(
                direct_min_batch_override_allowed(name, &lines, line_index, line),
                "released planner evidence `{name}` line {} pins minimum batch size outside the exact direct GUC contract",
                line_index + 1
            );
            approved_min_batch_overrides += 1;
        }
        for setting in parallel_settings {
            let pins = nondefault_setting_pin_lines(name, auditable_source, setting);
            if let Some(line_index) = pins.first() {
                panic!(
                    "released planner evidence `{name}` line {} pins protected planner setting `{setting}`",
                    line_index + 1
                );
            }
        }
    }
    assert_eq!(
        approved_cost_overrides, 1,
        "released planner evidence must contain exactly one narrowly approved numeric cost override"
    );
    assert_eq!(
        approved_min_batch_overrides, 3,
        "released planner evidence must contain only the three exact direct min_batch_size GUC checks"
    );
}

#[test]
fn admission_audit_rejects_unmarked_copied_and_changed_numeric_cost_pins() {
    let direct_line = numeric_cost_source_line("0.1");
    assert_eq!(
        audit_numeric_cost_pins(DIRECT_COST_OVERRIDE_SOURCE, &direct_line).rejected_lines,
        vec![0],
    );
    let session_pin = ["Spi::run(\"SET pg_accel.", "cost_multiplier = ", "0.1\")"].concat();
    assert_eq!(
        audit_numeric_cost_pins("plan_shape_test.rs", &session_pin).rejected_lines,
        vec![0]
    );

    let copied_elsewhere = format!(
        "{DIRECT_COST_DEFAULT_PROOF_LINE}\n{DIRECT_COST_OVERRIDE_ALLOW_MARKER}\n{direct_line}"
    );
    assert_eq!(
        audit_numeric_cost_pins("pg_accel/src/tests/not_phase2_cache.rs", &copied_elsewhere)
            .rejected_lines,
        vec![2],
    );

    for value in ["0.2", "-0.2", "+0.2", ".2", "'0.2'"] {
        let changed_value = format!(
            "{DIRECT_COST_DEFAULT_PROOF_LINE}\n{DIRECT_COST_OVERRIDE_ALLOW_MARKER}\n{}",
            numeric_cost_source_line(value)
        );
        assert_eq!(
            audit_numeric_cost_pins(DIRECT_COST_OVERRIDE_SOURCE, &changed_value).rejected_lines,
            vec![2],
            "changed numeric literal {value} must not inherit the exception"
        );
    }

    let alternate_spellings = [
        [
            "Spi::run(\"",
            "set",
            " local pg_accel.cost_multiplier to 0.1\")",
        ]
        .concat(),
        [
            "Spi::run(\"",
            "SeT",
            " SESSION pg_accel.cost_multiplier = 0.1\")",
        ]
        .concat(),
        [
            "Spi::run(\"",
            "SET",
            "\tLOCAL\tpg_accel . cost_multiplier\tTO\t0.1\")",
        ]
        .concat(),
        [
            "Spi::run(\"",
            "SET",
            " LOCAL pg_accel.cost_multiplier\nTO 0.1\")",
        ]
        .concat(),
        [
            "Spi::run(\"SELECT ",
            "SeT_ConFig",
            "( 'pg_accel.cost_multiplier' , '0.1' , true)\")",
        ]
        .concat(),
        [
            "[\"SET LOCAL pg_accel.\", \"",
            "cost_multiplier TO 0.1\"].concat()",
        ]
        .concat(),
        [
            "format!(\"SET LOCAL pg_accel.",
            "cost_multiplier = {}\", 0.1)",
        ]
        .concat(),
    ];
    for spelling in alternate_spellings {
        assert_eq!(
            audit_numeric_cost_pins("plan_shape_test.rs", &spelling).rejected_lines,
            vec![0],
            "valid PostgreSQL SET spelling must not bypass the numeric cost audit: {spelling:?}"
        );
    }
}

#[test]
fn admission_audit_rejects_alternate_min_batch_and_parallel_setting_pins() {
    let protected_settings = [
        "min_batch_size",
        "max_parallel_workers_per_gather",
        "min_parallel_table_scan_size",
        "min_parallel_index_scan_size",
        "parallel_setup_cost",
        "parallel_tuple_cost",
    ];
    for setting in protected_settings {
        let sql_setting = if setting == "min_batch_size" {
            format!("pg_accel.{setting}")
        } else {
            setting.to_owned()
        };
        let set_to = format!("Spi::run(\"sEt {sql_setting}\nTo 0\")");
        assert_eq!(
            nondefault_setting_pin_lines("fixture.sql", &set_to, setting),
            vec![0],
            "lowercase/multiline SET TO must be rejected for {setting}"
        );

        let set_config = format!("SELECT set_config( '{sql_setting}' , '0.1' , true)");
        assert_eq!(
            nondefault_setting_pin_lines("fixture.sql", &set_config, setting),
            vec![0],
            "set_config must be rejected for {setting}"
        );

        let default = format!("SET {sql_setting} TO DEFAULT");
        assert!(
            nondefault_setting_pin_lines("fixture.sql", &default, setting).is_empty(),
            "literal DEFAULT must remain admissible for {setting}"
        );
    }
}

#[test]
fn admission_audit_is_sql_scoped_and_rejects_fragmented_or_dynamic_pins() {
    assert!(
        nondefault_setting_pin_lines(
            "fixture.rs",
            "let cost_multiplier = settings.cost_multiplier;",
            "cost_multiplier"
        )
        .is_empty()
    );
    assert!(
        nondefault_setting_pin_lines(
            "fixture.RS",
            "let cost_multiplier = settings.cost_multiplier;",
            "cost_multiplier"
        )
        .is_empty(),
        "Rust source detection must be case-insensitive"
    );
    for unrelated in [
        "Spi::run(\"SET pg_accel.soft_fp64_cost_multiplier = 0.1\")",
        "Spi::run(\"SET pg_accel.cost_multiplier_extra = 0.1\")",
    ] {
        assert!(
            nondefault_setting_pin_lines("fixture.rs", unrelated, "cost_multiplier").is_empty(),
            "identifier prefixes/suffixes are not the protected GUC: {unrelated}"
        );
    }
    assert!(
        nondefault_setting_pin_lines(
            "fixture.rs",
            "panic!(\"query with cost_multiplier={value} should not crash\")",
            "cost_multiplier"
        )
        .is_empty()
    );

    let unicode_diagnostic = format!(
        "\u{e9}{}panic!(\"query with cost_multiplier={{value}} should not crash\")",
        "x".repeat(88)
    );
    assert!(
        nondefault_setting_pin_lines("fixture.rs", &unicode_diagnostic, "cost_multiplier")
            .is_empty(),
        "a multibyte character at the diagnostic suffix boundary must remain safe"
    );

    let unicode_prefixed_pin = "\u{3c0}\nSpi::run(\"SET pg_accel.cost_multiplier = 0.1\")";
    assert_eq!(
        nondefault_setting_pin_lines("fixture.RS", unicode_prefixed_pin, "cost_multiplier"),
        vec![1],
        "multibyte prefixes and uppercase Rust extensions must not hide SQL pins"
    );

    let adversarial = [
        "[\"SET LOCAL pg_accel.cost_\", \"multiplier = 0.1\"].concat()",
        "Spi::run(\"SET pg_accel.cost_multiplier /* split */ TO 0.1\")",
        "Spi::run(&format!(\"SET pg_accel.cost_multiplier = {value}\"))",
        "Spi::run(\"SET pg_accel.cost_multiplier FROM CURRENT\")",
        "Spi::run(&format!(\"SELECT set_config('pg_accel.cost_multiplier', '{value}', true)\"))",
        "Spi::run(&format!(\"SELECT set_config(\n'pg_accel.cost_multiplier',\n'{value}', true)\"))",
    ];
    for source in adversarial {
        assert!(
            !nondefault_setting_pin_lines("fixture.rs", source, "cost_multiplier").is_empty(),
            "protected SQL construction must fail closed: {source}"
        );
    }

    let separated_comment = format!("/*{}*/", " separated ".repeat(80));
    let distant_set_config = format!(
        "Spi::run(\"SELECT set_config({separated_comment}'pg_accel.cost_multiplier', '0.1', true)\")"
    );
    assert!(distant_set_config.len() > 512);
    assert_eq!(
        nondefault_setting_pin_lines("fixture.rs", &distant_set_config, "cost_multiplier"),
        vec![0],
        "set_config must remain bound to its argument across arbitrarily long comments"
    );

    let fragmented_set_config =
        r#"Spi::run(&["SELECT set_","config('pg_accel.cost_multiplier', '0.1', true)"].concat())"#;
    assert_eq!(
        nondefault_setting_pin_lines("fixture.rs", fragmented_set_config, "cost_multiplier"),
        vec![0],
        "fragmented set_config constructors must not bypass admission auditing"
    );

    for ordinary_rust in [
        "let set_config = callback; let cost_multiplier = settings.cost_multiplier;",
        r#"let prefix = "set_"; let suffix = "config("; let label = "cost_multiplier";"#,
        r#"let words = ["set_", "config"]; let metric = "cost_multiplier";"#,
    ] {
        assert!(
            nondefault_setting_pin_lines("fixture.rs", ordinary_rust, "cost_multiplier").is_empty(),
            "ordinary Rust identifiers and disconnected strings are not SQL assignments: {ordinary_rust}"
        );
    }
}

#[test]
fn admission_audit_cost_allow_marker_must_be_exact_adjacent_and_paired_with_default_proof() {
    let direct_line = numeric_cost_source_line("0.1");
    let exact = format!(
        "{DIRECT_COST_DEFAULT_PROOF_LINE}\n{DIRECT_COST_OVERRIDE_ALLOW_MARKER}\n{direct_line}"
    );
    assert!(
        audit_numeric_cost_pins(DIRECT_COST_OVERRIDE_SOURCE, &exact)
            .passes_for_source(DIRECT_COST_OVERRIDE_SOURCE)
    );

    let suffixed_marker = format!(
        "{DIRECT_COST_DEFAULT_PROOF_LINE}\n{DIRECT_COST_OVERRIDE_ALLOW_MARKER} (copied)\n{direct_line}"
    );
    assert_eq!(
        audit_numeric_cost_pins(DIRECT_COST_OVERRIDE_SOURCE, &suffixed_marker).rejected_lines,
        vec![2]
    );

    let nonadjacent_marker = format!(
        "{DIRECT_COST_DEFAULT_PROOF_LINE}\n{DIRECT_COST_OVERRIDE_ALLOW_MARKER}\n// unrelated\n{direct_line}"
    );
    assert_eq!(
        audit_numeric_cost_pins(DIRECT_COST_OVERRIDE_SOURCE, &nonadjacent_marker).rejected_lines,
        vec![3]
    );

    let missing_default_proof = format!("{DIRECT_COST_OVERRIDE_ALLOW_MARKER}\n{direct_line}");
    assert_eq!(
        audit_numeric_cost_pins(DIRECT_COST_OVERRIDE_SOURCE, &missing_default_proof).rejected_lines,
        vec![1]
    );
}

#[test]
fn admission_audit_rejects_duplicate_exact_cost_overrides_in_the_allowed_source() {
    let direct_line = numeric_cost_source_line("0.1");
    let duplicate = format!(
        "{DIRECT_COST_DEFAULT_PROOF_LINE}\n\
         {DIRECT_COST_OVERRIDE_ALLOW_MARKER}\n{direct_line}\n\
         {DIRECT_COST_OVERRIDE_ALLOW_MARKER}\n{direct_line}"
    );
    let audit = audit_numeric_cost_pins(DIRECT_COST_OVERRIDE_SOURCE, &duplicate);
    assert_eq!(audit.approved_lines, vec![2, 4]);
    assert!(audit.rejected_lines.is_empty());
    assert!(!audit.passes_for_source(DIRECT_COST_OVERRIDE_SOURCE));
}

#[test]
fn plan_shape_tests_do_not_expect_superseded_generic_rejection_codes() {
    let retired = [
        ["partial_agg_no_gpu_", "producing_child"].concat(),
        ["parallel_fused_count_", "unstable"].concat(),
        ["parallel_fused_count_", "disabled"].concat(),
        ["standalone_gpuexpr_", "no_gpu_pipeline"].concat(),
        ["no_gpu_resident_", "pipeline"].concat(),
        ["nlj_between_host_boundary_", "unsafe"].concat(),
        ["nestloop_scalar_", "no_gpu_kernel"].concat(),
    ];
    for retired in retired {
        assert!(
            !include_str!("plan_shape_test.rs").contains(&retired),
            "plan-shape suite still expects retired runtime rejection `{retired}`"
        );
    }
}

fn connect() -> Client {
    let connection = test_connection();
    let mut client = Client::connect(&connection, NoTls).expect("connect to bench PG");
    client
        .simple_query("SELECT 1 FROM pg_accel_stats() LIMIT 1")
        .expect("load pg_accel extension in backend");
    client
}

/// Restore the documented PostgreSQL planner defaults before plan evidence.
fn use_default_parallel_planner(c: &mut Client) {
    for stmt in [
        "SET pg_accel.enabled = on",
        "SET max_parallel_workers_per_gather = DEFAULT",
        "RESET min_parallel_table_scan_size",
        "RESET parallel_setup_cost",
        "RESET parallel_tuple_cost",
        "RESET enable_nestloop",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

/// Collect the full `EXPLAIN` output as one big lowercase string so
/// substring assertions are robust to per-row formatting.
fn explain_query(c: &mut Client, prefix: &str, sql: &str) -> String {
    let rows = c
        .simple_query(&format!("{prefix} {sql}"))
        .unwrap_or_else(|e| panic!("`{prefix} {sql}` failed: {e}"));
    let mut out = String::new();
    for m in rows {
        if let postgres::SimpleQueryMessage::Row(r) = m
            && let Some(s) = r.get(0)
        {
            out.push_str(s);
            out.push('\n');
        }
    }
    out.to_lowercase()
}

fn explain(c: &mut Client, sql: &str) -> String {
    explain_query(c, "EXPLAIN", sql)
}

fn explain_analyze(c: &mut Client, sql: &str) -> String {
    explain_query(
        c,
        "EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)",
        sql,
    )
}

fn kernel_executions(c: &mut Client) -> i64 {
    c.query_one("SELECT pg_accel_kernel_executions()", &[])
        .expect("pg_accel_kernel_executions()")
        .get::<_, i64>(0)
}

fn pg_accel_stat_i64(c: &mut Client, column: &str) -> i64 {
    c.query_one(&format!("SELECT {column} FROM pg_accel_stats()"), &[])
        .unwrap_or_else(|e| panic!("pg_accel_stats column `{column}` failed: {e}"))
        .get::<_, i64>(0)
}

fn device_limit_i64(c: &mut Client, name: &str) -> i64 {
    c.query_one(
        "SELECT value::bigint FROM pg_accel_device_limits() WHERE name = $1",
        &[&name],
    )
    .unwrap_or_else(|e| panic!("pg_accel device limit `{name}` failed: {e}"))
    .get::<_, i64>(0)
}

fn descriptor_groupagg_fixture_rows(c: &mut Client) -> i64 {
    let minimum = device_limit_i64(c, "gpu_hash_agg_min_rows");
    minimum
        .saturating_add((minimum / 4).max(1_024))
        .max(250_000)
}

fn explain_metric_i64(plan: &str, metric: &str) -> Option<i64> {
    let needle = format!("{}:", metric.to_lowercase());
    plan.lines().find_map(|line| {
        let pos = line.find(&needle)?;
        let digits: String = line
            .get(pos + needle.len()..)?
            .chars()
            .skip_while(char::is_ascii_whitespace)
            .take_while(char::is_ascii_digit)
            .collect();
        (!digits.is_empty()).then(|| digits.parse::<i64>().expect("metric digits parse"))
    })
}

#[derive(Debug, Clone, Copy)]
struct FusedCountExpectation {
    count: i64,
    rows_dispatched: i64,
    batches: i64,
    requires_gpuexpr_child: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct FusedFloat4ReduceRow {
    sum: Option<f64>,
    min: Option<f64>,
    max: Option<f64>,
    count: i64,
}

#[derive(Debug, Clone, PartialEq)]
struct FusedInt8ReduceRow {
    min: Option<i64>,
    max: Option<i64>,
    count: i64,
}

fn last_planner_rejection_reason(c: &mut Client) -> Option<String> {
    c.query_one("SELECT pg_accel_last_planner_rejection_reason()", &[])
        .ok()
        .and_then(|row| row.get::<_, Option<String>>(0))
}

fn planner_rejection_count(c: &mut Client, reason: &str) -> i64 {
    c.query_one("SELECT pg_accel_planner_rejection_count($1)", &[&reason])
        .unwrap_or_else(|e| panic!("pg_accel_planner_rejection_count({reason}) failed: {e}"))
        .get::<_, i64>(0)
}

fn assert_rejection_reason_observed(c: &mut Client, expected: &[&str], context: &str) {
    let actual_last = last_planner_rejection_reason(c);
    let observed = expected.iter().any(|reason| {
        planner_rejection_count(c, reason) > 0 || actual_last.as_deref() == Some(*reason)
    });
    assert!(
        observed,
        "{context}: expected planner rejection reason to be one of {expected:?}, \
         got last={actual_last:?}"
    );
}

fn scalar_i64(c: &mut Client, sql: &str) -> i64 {
    c.query_one(sql, &[])
        .unwrap_or_else(|e| panic!("query `{sql}` failed: {e}"))
        .get::<_, i64>(0)
}

fn scalar_float4_reduce(c: &mut Client, sql: &str) -> FusedFloat4ReduceRow {
    let row = c
        .query_one(sql, &[])
        .unwrap_or_else(|e| panic!("query `{sql}` failed: {e}"));
    FusedFloat4ReduceRow {
        sum: row.get::<_, Option<f64>>(0),
        min: row.get::<_, Option<f64>>(1),
        max: row.get::<_, Option<f64>>(2),
        count: row.get::<_, i64>(3),
    }
}

fn scalar_int8_reduce(c: &mut Client, sql: &str) -> FusedInt8ReduceRow {
    let row = c
        .query_one(sql, &[])
        .unwrap_or_else(|e| panic!("query `{sql}` failed: {e}"));
    FusedInt8ReduceRow {
        min: row.get::<_, Option<i64>>(0),
        max: row.get::<_, Option<i64>>(1),
        count: row.get::<_, i64>(2),
    }
}

fn assert_float4_reduce_rows_close(
    label: &str,
    native: &FusedFloat4ReduceRow,
    accelerated: &FusedFloat4ReduceRow,
) {
    assert_eq!(
        accelerated.count, native.count,
        "{label} count differs from native PostgreSQL"
    );
    for (name, got, expected, rel_tol) in [
        ("sum", accelerated.sum, native.sum, 1.0e-3),
        ("min", accelerated.min, native.min, 1.0e-5),
        ("max", accelerated.max, native.max, 1.0e-5),
    ] {
        match (got, expected) {
            (Some(got), Some(expected)) => assert!(
                (got - expected).abs() <= expected.abs().max(1.0) * rel_tol,
                "{label} {name} differs from native PostgreSQL: got {got}, expected {expected}"
            ),
            _ => assert_eq!(
                got, expected,
                "{label} {name} NULL-ness differs from native PostgreSQL"
            ),
        }
    }
}

fn assert_int8_reduce_rows_equal(
    label: &str,
    native: &FusedInt8ReduceRow,
    accelerated: &FusedInt8ReduceRow,
) {
    assert_eq!(
        accelerated, native,
        "{label} bigint filtered reduce differs from native PostgreSQL"
    );
}

/// Assert that `EXPLAIN sql` contains every substring in `needles`.
fn assert_plan_contains(c: &mut Client, sql: &str, needles: &[&str]) {
    let plan = explain(c, sql);
    for n in needles {
        assert!(
            plan.contains(&n.to_lowercase()),
            "plan for `{sql}` is missing `{n}`:\n{plan}"
        );
    }
}

/// Assert that `EXPLAIN sql` contains none of the substrings in `needles`.
fn assert_plan_lacks(c: &mut Client, sql: &str, needles: &[&str]) {
    let plan = explain(c, sql);
    for n in needles {
        assert!(
            !plan.contains(&n.to_lowercase()),
            "plan for `{sql}` unexpectedly contains `{n}`:\n{plan}"
        );
    }
}

/// Setup the bench fixture and the tiny fact/dim pair used by the hash-join
/// assertion. Idempotent — each `CREATE TABLE` is `IF NOT EXISTS`.
fn ensure_fixtures(c: &mut Client) {
    for stmt in bench_f32_10m_setup_sql() {
        c.simple_query(&stmt).expect("f32_10m setup");
    }

    for stmt in [
        "CREATE UNLOGGED TABLE IF NOT EXISTS bench_fact (id bigint, payload real)",
        "INSERT INTO bench_fact (id, payload) \
         SELECT g, random()::real FROM generate_series(1, 1000000) g \
         ON CONFLICT DO NOTHING",
        "CREATE UNLOGGED TABLE IF NOT EXISTS bench_dim (id bigint, name text)",
        "INSERT INTO bench_dim (id, name) \
         SELECT g, 'd' || g::text FROM generate_series(1, 1000) g \
         ON CONFLICT DO NOTHING",
        "ANALYZE bench_fact",
        "ANALYZE bench_dim",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

/// Setup the grouped HashAgg quarantine fixture. The 100K scale is the first
/// quarantined threshold after the observed grouped GPU HashAgg crash, so this
/// query must stay native until a real safe implementation lands.
fn ensure_hashagg_gate_fixture(c: &mut Client) {
    for stmt in [
        "CREATE UNLOGGED TABLE IF NOT EXISTS bench_hashagg_gate \
         (id bigint, grp int4, val double precision)",
        "TRUNCATE bench_hashagg_gate",
        "INSERT INTO bench_hashagg_gate (id, grp, val) \
         SELECT g, (g % 10000)::int4, random() \
         FROM generate_series(1, 100000) g",
        "ANALYZE bench_hashagg_gate",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

fn polygon_wkt(vertices: usize, cx: f64, cy: f64, radius: f64) -> String {
    let mut out = String::from("POLYGON((");
    for i in 0..=vertices {
        if i > 0 {
            out.push(',');
        }
        let j = i % vertices;
        let angle = std::f64::consts::TAU * (j as f64) / (vertices as f64);
        let x = radius.mul_add(angle.cos(), cx);
        let y = radius.mul_add(angle.sin(), cy);
        write!(&mut out, "{x:.8} {y:.8}").expect("write polygon coordinate");
    }
    out.push_str("))");
    out
}

fn ensure_postgis_point_polygon_fixture(c: &mut Client) {
    for stmt in [
        "CREATE EXTENSION IF NOT EXISTS postgis CASCADE",
        "CREATE UNLOGGED TABLE IF NOT EXISTS bench_postgis_pip_gate \
         (id int4, geom geometry(Point, 4326) NOT NULL)",
        "TRUNCATE bench_postgis_pip_gate",
        "INSERT INTO bench_postgis_pip_gate (id, geom) \
         SELECT g, ST_SetSRID(\
                    ST_MakePoint((g % 1000)::float8 / 10.0, \
                                 ((g / 1000) % 250)::float8 / 10.0), \
                    4326)::geometry(Point, 4326) \
         FROM generate_series(1, 250000) AS g",
        "INSERT INTO bench_postgis_pip_gate (id, geom) VALUES \
           (250001, ST_SetSRID(ST_MakePoint(58.0, 12.5), 4326)::geometry(Point, 4326)), \
           (250002, ST_SetSRID(ST_MakePoint(50.0, 20.5), 4326)::geometry(Point, 4326)), \
           (250003, ST_SetSRID(ST_MakePoint(42.0, 12.5), 4326)::geometry(Point, 4326)), \
           (250004, ST_SetSRID(ST_MakePoint(50.0, 4.5), 4326)::geometry(Point, 4326))",
        "ANALYZE bench_postgis_pip_gate",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

fn ensure_gpuexpr_direct_fixture(c: &mut Client) {
    for stmt in [
        "CREATE UNLOGGED TABLE IF NOT EXISTS bench_gpuexpr_direct_gate ( \
            id int4 NOT NULL, \
            val float4 NOT NULL, \
            category int4 NOT NULL \
         )",
        "TRUNCATE bench_gpuexpr_direct_gate",
        "INSERT INTO bench_gpuexpr_direct_gate \
         SELECT \
           g::int4, \
           (g % 1000)::float4, \
           (g % 100)::int4 \
         FROM generate_series(1, 1000000) AS g",
        "ANALYZE bench_gpuexpr_direct_gate",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

fn ensure_gpuexpr_fused_count_isolated_fixture(c: &mut Client) -> String {
    ensure_gpuexpr_count_isolated_fixture(c, "fused_count", 2_000_000)
}

fn ensure_gpuexpr_direct_isolated_fixture(c: &mut Client, table_suffix: &str) -> String {
    ensure_gpuexpr_count_isolated_fixture(c, table_suffix, 1_000_000)
}

fn ensure_gpuexpr_count_isolated_fixture(c: &mut Client, table_suffix: &str, rows: i64) -> String {
    assert!(rows > 0, "isolated GpuExpr fixture rows must be positive");
    let backend_pid: i32 = c
        .query_one("SELECT pg_backend_pid()", &[])
        .expect("pg_backend_pid")
        .get(0);
    let table = format!("bench_gpuexpr_{table_suffix}_pg18_{backend_pid}");
    let stmts = [
        format!("DROP TABLE IF EXISTS {table}"),
        format!(
            "CREATE UNLOGGED TABLE {table} ( \
                id int4 NOT NULL, \
                val float4 NOT NULL, \
                category int4 NOT NULL \
             )"
        ),
        format!(
            "INSERT INTO {table} \
             SELECT \
               g::int4, \
               (g % 1000)::float4, \
               (g % 100)::int4 \
             FROM generate_series(1, {rows}) AS g"
        ),
        format!("ANALYZE {table}"),
    ];

    for stmt in stmts {
        c.simple_query(&stmt).unwrap_or_else(|e| {
            panic!("isolated GpuExpr fused-count fixture statement failed `{stmt}`: {e}")
        });
    }
    table
}

fn ensure_gpuexpr_bigint_count_isolated_fixture(c: &mut Client, table_suffix: &str) -> String {
    let backend_pid: i32 = c
        .query_one("SELECT pg_backend_pid()", &[])
        .expect("pg_backend_pid")
        .get(0);
    let table = format!("bench_gpuexpr_{table_suffix}_pg18_{backend_pid}");
    let stmts = [
        format!("DROP TABLE IF EXISTS {table}"),
        format!(
            "CREATE UNLOGGED TABLE {table} ( \
                id int4 NOT NULL, \
                bigval int8 NOT NULL, \
                category int4 NOT NULL \
             )"
        ),
        format!(
            "INSERT INTO {table} \
             SELECT \
               g::int4, \
               g::int8, \
               (g % 100)::int4 \
             FROM generate_series(1, 1000000) AS g"
        ),
        format!("ANALYZE {table}"),
    ];

    for stmt in stmts {
        c.simple_query(&stmt).unwrap_or_else(|e| {
            panic!("isolated GpuExpr bigint fused-count fixture statement failed `{stmt}`: {e}")
        });
    }
    table
}

fn set_parallel_count_common_settings(c: &mut Client) {
    for stmt in [
        "SET jit = off",
        "SET max_parallel_workers_per_gather = DEFAULT",
        "RESET min_parallel_table_scan_size",
        "RESET parallel_setup_cost",
        "RESET parallel_tuple_cost",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

fn set_parallel_count_native_mode(c: &mut Client) {
    set_parallel_count_common_settings(c);
    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
}

fn set_parallel_count_accel_mode(c: &mut Client) {
    set_parallel_count_common_settings(c);
    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.parallel_fused_count = on",
        "SET pg_accel.cost_multiplier = DEFAULT",
        "SET pg_accel.min_batch_size = DEFAULT",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

fn ensure_gpuexpr_nullable_prefix_fixture(c: &mut Client) {
    for stmt in [
        "DROP TABLE IF EXISTS bench_gpuexpr_nullable_prefix_gate",
        "CREATE UNLOGGED TABLE bench_gpuexpr_nullable_prefix_gate ( \
            pad int4 NULL, \
            val float4 NULL, \
            category int4 NOT NULL \
         )",
        "INSERT INTO bench_gpuexpr_nullable_prefix_gate \
         SELECT \
           CASE WHEN g % 3 = 0 THEN NULL ELSE g::int4 END, \
           CASE WHEN g % 10 = 0 THEN NULL ELSE (g % 1000)::float4 END, \
           (g % 100)::int4 \
         FROM generate_series(1, 1000000) AS g",
        "ANALYZE bench_gpuexpr_nullable_prefix_gate",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

fn ensure_gpuexpr_multibatch_mask_fixture(c: &mut Client) {
    for stmt in [
        "DROP TABLE IF EXISTS bench_gpuexpr_multibatch_mask_gate",
        "CREATE UNLOGGED TABLE bench_gpuexpr_multibatch_mask_gate ( \
            id int4 NOT NULL, \
            val float4 NULL, \
            category int4 NOT NULL \
         )",
        "INSERT INTO bench_gpuexpr_multibatch_mask_gate \
         SELECT \
           g::int4, \
           CASE \
             WHEN g > 4194304 AND g % 10 = 0 THEN NULL \
             ELSE (g % 1000)::float4 \
           END, \
           (g % 100)::int4 \
         FROM generate_series(1, 4300000) AS g",
        "ANALYZE bench_gpuexpr_multibatch_mask_gate",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

fn ensure_gpuexpr_nan_fixture(c: &mut Client) {
    for stmt in [
        "CREATE UNLOGGED TABLE IF NOT EXISTS bench_gpuexpr_nan_gate ( \
            val float4 NOT NULL, \
            category int4 NOT NULL \
         )",
        "TRUNCATE bench_gpuexpr_nan_gate",
        "INSERT INTO bench_gpuexpr_nan_gate \
         SELECT \
           'NaN'::float4, \
           (g % 100)::int4 \
         FROM generate_series(1, 1000000) AS g",
        "ANALYZE bench_gpuexpr_nan_gate",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

fn assert_fused_gpuexpr_count_matches_native(
    c: &mut Client,
    label: &str,
    sql: &str,
    expected: FusedCountExpectation,
) {
    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native_count = scalar_i64(c, sql);
    assert_eq!(
        native_count, expected.count,
        "{label} native count changed; fixture/query expectation is stale"
    );

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.cost_multiplier = DEFAULT",
        "SET pg_accel.min_batch_size = DEFAULT",
        "SET max_parallel_workers_per_gather = DEFAULT",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset stats");
    let plan = explain(c, sql);
    assert!(
        !plan.contains("custom scan") && !plan.contains("gpuagg"),
        "{label} must stay native until pg_accel has a GPU-resident count pipeline:\n{plan}"
    );
    assert!(
        plan.contains("aggregate") && plan.contains("seq scan"),
        "{label} should stay on PostgreSQL native aggregate/scan:\n{plan}"
    );
    assert_rejection_reason_observed(
        c,
        &["shape_unsupported_predicate"],
        &format!("{label} fused count"),
    );
    let before = kernel_executions(c);
    let accel_count = scalar_i64(c, sql);
    let after = kernel_executions(c);
    assert_eq!(
        after, before,
        "{label} native execution must not dispatch pg_accel kernels"
    );
    assert_eq!(
        pg_accel_stat_i64(c, "gpu_uncertain_count"),
        0,
        "{label} native execution must not produce pg_accel uncertain rows"
    );
    assert_eq!(
        pg_accel_stat_i64(c, "stock_exec_count"),
        0,
        "{label} native execution must not enter pg_accel stock fallback"
    );
    assert_eq!(
        accel_count, native_count,
        "{label} native count changed under pg_accel hooks"
    );
}

fn assert_fused_gpuexpr_float4_reduce_matches_native(
    c: &mut Client,
    label: &str,
    sql: &str,
    _rows_dispatched: i64,
    _batches: i64,
) {
    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native = scalar_float4_reduce(c, sql);
    assert!(
        native.count > 0,
        "{label} fixture/query should select at least one non-null value"
    );

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.cost_multiplier = DEFAULT",
        "SET pg_accel.min_batch_size = DEFAULT",
        "SET max_parallel_workers_per_gather = DEFAULT",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset stats");
    let plan = explain(c, sql);
    assert!(
        !plan.contains("custom scan") && !plan.contains("gpuagg"),
        "{label} must stay native until pg_accel has a GPU-resident f32 reduce pipeline:\n{plan}"
    );
    assert!(
        plan.contains("aggregate") && plan.contains("seq scan"),
        "{label} should stay on PostgreSQL native aggregate/scan:\n{plan}"
    );
    assert_rejection_reason_observed(
        c,
        &["shape_unsupported_predicate"],
        &format!("{label} filtered f32 reduce"),
    );
    let before = kernel_executions(c);
    let accelerated = scalar_float4_reduce(c, sql);
    let after = kernel_executions(c);
    assert_eq!(
        after, before,
        "{label} native execution must not dispatch pg_accel kernels"
    );
    assert_eq!(
        pg_accel_stat_i64(c, "gpu_uncertain_count"),
        0,
        "{label} native execution must not produce pg_accel uncertain rows"
    );
    assert_eq!(
        pg_accel_stat_i64(c, "stock_exec_count"),
        0,
        "{label} native execution must not enter pg_accel stock fallback"
    );
    assert_float4_reduce_rows_close(label, &native, &accelerated);
}

fn assert_fused_gpuexpr_int8_reduce_matches_native(
    c: &mut Client,
    label: &str,
    sql: &str,
    _rows_dispatched: i64,
    _batches: i64,
) {
    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native = scalar_int8_reduce(c, sql);
    assert!(
        native.count > 0,
        "{label} fixture/query should select at least one non-null value"
    );

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.cost_multiplier = DEFAULT",
        "SET pg_accel.min_batch_size = DEFAULT",
        "SET max_parallel_workers_per_gather = DEFAULT",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset stats");
    let plan = explain(c, sql);
    assert!(
        !plan.contains("custom scan") && !plan.contains("gpuagg"),
        "{label} must stay native until pg_accel has a GPU-resident i64 reduce pipeline:\n{plan}"
    );
    assert!(
        plan.contains("aggregate") && plan.contains("seq scan"),
        "{label} should stay on PostgreSQL native aggregate/scan:\n{plan}"
    );
    assert_rejection_reason_observed(
        c,
        &["shape_unsupported_predicate"],
        &format!("{label} filtered i64 reduce"),
    );
    let before = kernel_executions(c);
    let accelerated = scalar_int8_reduce(c, sql);
    let after = kernel_executions(c);
    assert_eq!(
        after, before,
        "{label} native execution must not dispatch pg_accel kernels"
    );
    assert_eq!(
        pg_accel_stat_i64(c, "gpu_uncertain_count"),
        0,
        "{label} native execution must not produce pg_accel uncertain rows"
    );
    assert_eq!(
        pg_accel_stat_i64(c, "stock_exec_count"),
        0,
        "{label} native execution must not enter pg_accel stock fallback"
    );
    assert_int8_reduce_rows_equal(label, &native, &accelerated);
}

fn ensure_nlj_between_fixture(c: &mut Client) {
    let event_rows = device_limit_i64(c, "gpu_min_rows")
        .saturating_mul(2)
        .max(100_000);
    for stmt in [
        "CREATE UNLOGGED TABLE IF NOT EXISTS bench_nlj_events \
         (id int4 NOT NULL, ts int8 NOT NULL)",
        "CREATE UNLOGGED TABLE IF NOT EXISTS bench_nlj_windows \
         (id int4 NOT NULL, lo int8 NOT NULL, hi int8 NOT NULL)",
        "TRUNCATE bench_nlj_events",
        "TRUNCATE bench_nlj_windows",
        "INSERT INTO bench_nlj_windows \
         SELECT i::int4, (i * 1000)::int8, (i * 1000 + 999)::int8 \
         FROM generate_series(0, 999) AS i",
        "ANALYZE bench_nlj_windows",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
    c.simple_query(&format!(
        "INSERT INTO bench_nlj_events \
         SELECT g::int4, \
                ((((g - 1) % 1000) * 1000) + ((g - 1) / 1000))::int8 \
         FROM generate_series(1, {event_rows}) AS g"
    ))
    .expect("populate NLJ BETWEEN event fixture");
    c.simple_query("ANALYZE bench_nlj_events")
        .expect("analyze NLJ BETWEEN event fixture");
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupIntStatsRow {
    group_key: i32,
    sum: i64,
    min: i32,
    max: i32,
    count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupIntSumCountRow {
    group_key: i32,
    sum: Option<i64>,
    count: i64,
}

fn ensure_descriptor_groupagg_renamed_direct_fixture(c: &mut Client) {
    let rows = descriptor_groupagg_fixture_rows(c);
    for stmt in [
        "DROP TABLE IF EXISTS pg_accel_rg_rename_direct",
        "DROP TABLE IF EXISTS bench_employees_agg",
        "CREATE UNLOGGED TABLE bench_employees_agg (             id serial PRIMARY KEY,             dept int4 NOT NULL,             salary int4 NOT NULL          )",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
    c.simple_query(&format!(
        "INSERT INTO bench_employees_agg (dept, salary)          SELECT (g % 64)::int4, (1000 + (g % 1000))::int4          FROM generate_series(1, {rows}) AS g"
    ))
    .expect("populate direct descriptor aggregate fixture");
    c.simple_query("ANALYZE bench_employees_agg")
        .expect("analyze direct descriptor aggregate fixture");
    c.simple_query("SELECT pg_accel_pin('bench_employees_agg'::regclass, ARRAY['dept', 'salary'])")
        .expect("pin grouped aggregate inputs");
    for stmt in [
        "ALTER TABLE bench_employees_agg RENAME TO pg_accel_rg_rename_direct",
        "ALTER TABLE pg_accel_rg_rename_direct RENAME COLUMN dept TO grp",
        "ALTER TABLE pg_accel_rg_rename_direct RENAME COLUMN salary TO measure",
        "ANALYZE pg_accel_rg_rename_direct",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

fn ensure_descriptor_groupagg_renamed_expression_fixture(c: &mut Client) {
    let rows = descriptor_groupagg_fixture_rows(c);
    for stmt in [
        "DROP TABLE IF EXISTS pg_accel_rg_rename_expr",
        "DROP TABLE IF EXISTS bench_expression_sales",
        "CREATE UNLOGGED TABLE bench_expression_sales (             id serial PRIMARY KEY,             product_id int4 NOT NULL,             price int4 NOT NULL,             discount int4 NOT NULL          )",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
    c.simple_query(&format!(
        "INSERT INTO bench_expression_sales (product_id, price, discount)          SELECT (g % 128)::int4,                 (1 + (g % 997))::int4,                 (1 + (g % 49))::int4          FROM generate_series(1, {rows}) AS g"
    ))
    .expect("populate expression descriptor aggregate fixture");
    c.simple_query("ANALYZE bench_expression_sales")
        .expect("analyze expression descriptor aggregate fixture");
    c.simple_query(
        "SELECT pg_accel_pin(
            'bench_expression_sales'::regclass,
            ARRAY['product_id', 'price', 'discount']
        )",
    )
    .expect("pin expression aggregate inputs");
    for stmt in [
        "ALTER TABLE bench_expression_sales RENAME TO pg_accel_rg_rename_expr",
        "ALTER TABLE pg_accel_rg_rename_expr RENAME COLUMN product_id TO grp",
        "ALTER TABLE pg_accel_rg_rename_expr RENAME COLUMN price TO lhs",
        "ALTER TABLE pg_accel_rg_rename_expr RENAME COLUMN discount TO rhs",
        "ANALYZE pg_accel_rg_rename_expr",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

fn ensure_aggregate_filter_decline_fixture(c: &mut Client) {
    let rows = descriptor_groupagg_fixture_rows(c);
    for stmt in [
        "DROP TABLE IF EXISTS bench_rg_filter_decline",
        "CREATE UNLOGGED TABLE bench_rg_filter_decline (             id serial PRIMARY KEY,             product_id int4 NOT NULL,             price int4 NOT NULL,             discount int4 NOT NULL,             active boolean NOT NULL          )",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
    c.simple_query(&format!(
        "INSERT INTO bench_rg_filter_decline (product_id, price, discount, active) \
         SELECT (g % 64)::int4, \
                (1 + (g % 997))::int4, \
                (1 + (g % 49))::int4, \
                (g % 3) <> 0 \
         FROM generate_series(1, {rows}) AS g"
    ))
    .expect("populate aggregate FILTER decline fixture");
    c.simple_query("ANALYZE bench_rg_filter_decline")
        .expect("analyze aggregate FILTER decline fixture");
    c.simple_query(
        "SELECT pg_accel_pin(
            'bench_rg_filter_decline'::regclass,
            ARRAY['product_id', 'price', 'discount', 'active']
        )",
    )
    .expect("pin aggregate FILTER decline inputs");
}

fn direct_groupagg_rows(c: &mut Client, sql: &str) -> Vec<GroupIntStatsRow> {
    c.query(sql, &[])
        .unwrap_or_else(|e| panic!("query `{sql}` failed: {e}"))
        .into_iter()
        .map(|row| GroupIntStatsRow {
            group_key: row.get::<_, i32>(0),
            sum: row.get::<_, i64>(1),
            min: row.get::<_, i32>(2),
            max: row.get::<_, i32>(3),
            count: row.get::<_, i64>(4),
        })
        .collect()
}

fn expression_groupagg_rows(c: &mut Client, sql: &str) -> Vec<GroupIntSumCountRow> {
    c.query(sql, &[])
        .unwrap_or_else(|e| panic!("query `{sql}` failed: {e}"))
        .into_iter()
        .map(|row| GroupIntSumCountRow {
            group_key: row.get::<_, i32>(0),
            sum: row.get::<_, Option<i64>>(1),
            count: row.get::<_, i64>(2),
        })
        .collect()
}

fn assert_descriptor_groupagg_plan(plan: &str, context: &str) {
    for needle in [
        "custom scan (gpuaccelagg)",
        "strategy: gpuagg",
        "gpu descriptor strategy: descriptor_grouped_aggregate",
    ] {
        assert!(
            plan.contains(needle),
            "{context}: descriptor plan missing `{needle}`:\n{plan}"
        );
    }
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_descriptor_groupagg_survives_table_and_column_rename() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_descriptor_groupagg_renamed_direct_fixture(&mut c);

    let sql = "SELECT grp, SUM(measure), MIN(measure), MAX(measure), COUNT(*)                FROM pg_accel_rg_rename_direct                GROUP BY grp                ORDER BY grp";

    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native = direct_groupagg_rows(&mut c, sql);

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.auto_load = on",
        "SET pg_accel.cost_multiplier = DEFAULT",
        "SET pg_accel.min_batch_size = DEFAULT",
        "SET max_parallel_workers_per_gather = DEFAULT",
        "SELECT pg_accel_reset_stats()",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    let plan = explain_query(&mut c, "EXPLAIN (VERBOSE, COSTS OFF)", sql);
    assert_descriptor_groupagg_plan(&plan, "renamed direct integer groupagg");

    let before = kernel_executions(&mut c);
    let accelerated = direct_groupagg_rows(&mut c, sql);
    let after = kernel_executions(&mut c);
    assert!(
        after > before,
        "renamed descriptor groupagg should dispatch a GPU kernel"
    );
    assert_eq!(pg_accel_stat_i64(&mut c, "stock_exec_count"), 0);
    assert_eq!(accelerated, native);
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_descriptor_expression_groupagg_survives_rename() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_descriptor_groupagg_renamed_expression_fixture(&mut c);

    let sql = "SELECT grp, SUM(lhs * rhs), COUNT(*)                FROM pg_accel_rg_rename_expr                GROUP BY grp                ORDER BY grp";

    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native = expression_groupagg_rows(&mut c, sql);

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.auto_load = on",
        "SET pg_accel.cost_multiplier = DEFAULT",
        "SET pg_accel.min_batch_size = DEFAULT",
        "SET max_parallel_workers_per_gather = DEFAULT",
        "SELECT pg_accel_reset_stats()",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    let plan = explain_query(&mut c, "EXPLAIN (VERBOSE, COSTS OFF)", sql);
    assert_descriptor_groupagg_plan(&plan, "renamed int4 expression groupagg");

    let before = kernel_executions(&mut c);
    let accelerated = expression_groupagg_rows(&mut c, sql);
    let after = kernel_executions(&mut c);
    assert!(
        after > before,
        "renamed descriptor expression groupagg should dispatch a GPU kernel"
    );
    assert_eq!(pg_accel_stat_i64(&mut c, "stock_exec_count"), 0);
    assert_eq!(accelerated, native);
}

#[cfg(feature = "integration_tests")]
#[test]
fn resident_descriptor_backend_exit_releases_ledger_without_postmaster_restart() {
    let _live_pg_guard = live_pg_test_lock();
    let mut monitor = connect();
    monitor
        .batch_execute("SET pg_accel.gpu_enabled = off")
        .expect("keep monitor backend GPU-idle");
    monitor
        .batch_execute("DROP TABLE IF EXISTS pg_accel_backend_exit_lifecycle")
        .expect("remove stale backend-exit fixture");
    let postmaster_started_at = monitor
        .query_one("SELECT pg_postmaster_start_time()::text", &[])
        .expect("read postmaster start time")
        .get::<_, String>(0);
    let mut worker = connect();
    let rows = descriptor_groupagg_fixture_rows(&mut worker);
    worker
        .batch_execute(
            "CREATE UNLOGGED TABLE pg_accel_backend_exit_lifecycle (
                 id serial PRIMARY KEY,
                 grp int4 NOT NULL,
                 measure int4 NOT NULL
             )",
        )
        .expect("create backend-exit fixture");
    worker
        .simple_query(&format!(
            "INSERT INTO pg_accel_backend_exit_lifecycle (grp, measure)
             SELECT (g % 64)::int4, (1000 + (g % 1000))::int4
             FROM generate_series(1, {rows}) AS g"
        ))
        .expect("populate backend-exit fixture");
    worker
        .batch_execute("ANALYZE pg_accel_backend_exit_lifecycle")
        .expect("analyze backend-exit fixture");

    let sql = "SELECT grp, SUM(measure), MIN(measure), MAX(measure), COUNT(*)
               FROM pg_accel_backend_exit_lifecycle
               GROUP BY grp
               ORDER BY grp";
    worker
        .batch_execute("SET pg_accel.enabled = off")
        .expect("disable pg_accel for native reference");
    let native = direct_groupagg_rows(&mut worker, sql);

    worker
        .batch_execute(
            "SELECT pg_accel_pin(
                 'pg_accel_backend_exit_lifecycle'::regclass,
                 ARRAY['grp', 'measure']
             );
             SET pg_accel.enabled = on;
             SET pg_accel.auto_load = on;
             SET pg_accel.cost_multiplier = DEFAULT;
             SET pg_accel.min_batch_size = DEFAULT;
             SET max_parallel_workers_per_gather = DEFAULT;
             SELECT pg_accel_reset_stats();",
        )
        .expect("pin and configure backend-exit fixture");
    let plan = explain_query(&mut worker, "EXPLAIN (VERBOSE, COSTS OFF)", sql);
    assert_descriptor_groupagg_plan(&plan, "backend-exit descriptor groupagg");

    let before = kernel_executions(&mut worker);
    let accelerated = direct_groupagg_rows(&mut worker, sql);
    let after = kernel_executions(&mut worker);
    assert!(
        after > before,
        "backend-exit fixture must dispatch a GPU kernel"
    );
    assert_eq!(accelerated, native);

    let status = worker
        .query_one(
            "SELECT COUNT(*)::bigint,
                    COALESCE(SUM(raw_bytes), 0)::bigint,
                    COALESCE(SUM(derived_bytes), 0)::bigint
             FROM pg_accel_resident_status()",
            &[],
        )
        .expect("read worker resident status");
    let status_rows = status.get::<_, i64>(0);
    let raw_bytes = status.get::<_, i64>(1);
    let derived_bytes = status.get::<_, i64>(2);
    let worker_bytes = raw_bytes
        .checked_add(derived_bytes)
        .expect("worker resident bytes fit bigint");
    assert_eq!(status_rows, 1);
    assert!(raw_bytes > 0, "pinned fixture must own raw resident bytes");
    assert!(
        derived_bytes > 0,
        "executed descriptor must own a derived resident artifact"
    );

    let cluster_live = worker
        .query_one("SELECT pg_accel_resident_live_bytes()", &[])
        .expect("read live resident ledger")
        .get::<_, i64>(0);
    let other_backend_bytes = cluster_live
        .checked_sub(worker_bytes)
        .expect("worker status cannot exceed cluster ledger");

    // Closing libpq makes PostgreSQL run before_shmem_exit. The monitor is a
    // separate backend: a postmaster crash would terminate it, while a missed
    // cleanup would leave worker_bytes in the shared ledger.
    drop(worker);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let state = monitor.query_one(
            "SELECT pg_postmaster_start_time()::text,
                    pg_is_in_recovery(),
                    pg_accel_resident_live_bytes(),
                    EXISTS (SELECT 1 FROM pg_accel_resident_status())",
            &[],
        );
        let row = state.unwrap_or_else(|error| {
            panic!("monitor backend was lost while resident worker exited: {error}")
        });
        assert_eq!(row.get::<_, String>(0), postmaster_started_at);
        assert!(!row.get::<_, bool>(1), "server entered crash recovery");
        let live = row.get::<_, i64>(2);
        assert!(!row.get::<_, bool>(3), "monitor has no local residency");
        if live == other_backend_bytes {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "exited backend retained {live} ledger bytes; expected {other_backend_bytes}"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    monitor
        .batch_execute("DROP TABLE pg_accel_backend_exit_lifecycle")
        .expect("drop backend-exit fixture");

    // Observe the monitor's own exit too. Its GPU is disabled so it remains a
    // pure watchdog; this catches teardown faults after the final SQL command
    // has already returned successfully to libpq.
    let monitor_pid = monitor
        .query_one("SELECT pg_backend_pid()", &[])
        .expect("read monitor backend PID")
        .get::<_, i32>(0);
    let mut watchdog = connect();
    watchdog
        .batch_execute("SET pg_accel.gpu_enabled = off")
        .expect("keep watchdog backend GPU-idle");
    drop(monitor);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let row = watchdog
            .query_one(
                "SELECT pg_postmaster_start_time()::text,
                        pg_is_in_recovery(),
                        EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid = $1)",
                &[&monitor_pid],
            )
            .unwrap_or_else(|error| panic!("watchdog was lost while monitor exited: {error}"));
        assert_eq!(row.get::<_, String>(0), postmaster_started_at);
        assert!(!row.get::<_, bool>(1), "server entered crash recovery");
        if !row.get::<_, bool>(2) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "monitor backend {monitor_pid} did not exit"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    for _ in 0..4 {
        std::thread::sleep(std::time::Duration::from_millis(25));
        let row = watchdog
            .query_one(
                "SELECT pg_postmaster_start_time()::text, pg_is_in_recovery()",
                &[],
            )
            .expect("watchdog remains connected after monitor teardown");
        assert_eq!(row.get::<_, String>(0), postmaster_started_at);
        assert!(!row.get::<_, bool>(1), "server entered crash recovery");
    }
}

#[cfg(feature = "integration_tests")]
#[test]
fn planner_only_gpu_init_backend_exit_does_not_restart_postmaster() {
    let _live_pg_guard = live_pg_test_lock();
    let mut monitor = connect();
    monitor
        .batch_execute("SET pg_accel.gpu_enabled = off")
        .expect("keep monitor backend GPU-idle");
    let postmaster_started_at = monitor
        .query_one("SELECT pg_postmaster_start_time()::text", &[])
        .expect("read postmaster start time")
        .get::<_, String>(0);
    let resident_baseline = monitor
        .query_one("SELECT pg_accel_resident_live_bytes()", &[])
        .expect("read cluster resident ledger baseline")
        .get::<_, i64>(0);

    let mut worker = connect();
    worker
        .batch_execute(
            "SET pg_accel.enabled = on;
             SET pg_accel.gpu_enabled = on;
             SELECT pg_accel_reset_stats();",
        )
        .expect("configure planner-only GPU probe");
    let worker_pid = worker
        .query_one("SELECT pg_backend_pid()", &[])
        .expect("read worker backend PID")
        .get::<_, i32>(0);
    worker
        .query_one(
            "SELECT gpu_device_name FROM pg_accel_device_info() LIMIT 1",
            &[],
        )
        .expect("initialize GPU runtime without dispatch");
    assert_eq!(
        kernel_executions(&mut worker),
        0,
        "device capability probe must not dispatch a kernel"
    );
    assert_eq!(
        worker
            .query_one("SELECT pg_accel_resident_live_bytes()", &[])
            .expect("read resident ledger after planner-only initialization")
            .get::<_, i64>(0),
        resident_baseline,
        "planner-only runtime initialization changed the resident ledger"
    );

    drop(worker);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let row = monitor
            .query_one(
                "SELECT pg_postmaster_start_time()::text,
                        pg_is_in_recovery(),
                        pg_accel_resident_live_bytes(),
                        EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid = $1)",
                &[&worker_pid],
            )
            .unwrap_or_else(|error| {
                panic!("monitor was lost while planner-only GPU backend exited: {error}")
            });
        assert_eq!(row.get::<_, String>(0), postmaster_started_at);
        assert!(!row.get::<_, bool>(1), "server entered crash recovery");
        assert_eq!(
            row.get::<_, i64>(2),
            resident_baseline,
            "planner probe changed the resident ledger"
        );
        if !row.get::<_, bool>(3) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "planner-only backend {worker_pid} did not exit"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    for _ in 0..4 {
        std::thread::sleep(std::time::Duration::from_millis(25));
        let row = monitor
            .query_one(
                "SELECT pg_postmaster_start_time()::text, pg_is_in_recovery()",
                &[],
            )
            .expect("monitor remains connected after planner-only teardown");
        assert_eq!(row.get::<_, String>(0), postmaster_started_at);
        assert!(!row.get::<_, bool>(1), "server entered crash recovery");
    }
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_aggregate_filter_has_precise_structural_decline() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_aggregate_filter_decline_fixture(&mut c);

    let sql = "SELECT product_id,                       SUM(price * discount) FILTER (WHERE active),                       COUNT(*)                FROM bench_rg_filter_decline                GROUP BY product_id                ORDER BY product_id";

    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native = expression_groupagg_rows(&mut c, sql);

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.auto_load = on",
        "SET pg_accel.cost_multiplier = DEFAULT",
        "SET pg_accel.min_batch_size = DEFAULT",
        "SELECT pg_accel_reset_stats()",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    let plan = explain_query(&mut c, "EXPLAIN (VERBOSE, COSTS OFF)", sql);
    assert!(
        !plan.contains("custom scan"),
        "aggregate FILTER must stay native until descriptor FILTER execution exists:\n{plan}"
    );
    assert_rejection_reason_observed(
        &mut c,
        &["shape_aggregate_modifier"],
        "aggregate FILTER descriptor decline",
    );

    let before = kernel_executions(&mut c);
    let enabled = expression_groupagg_rows(&mut c, sql);
    let after = kernel_executions(&mut c);
    assert_eq!(
        after, before,
        "structural decline must not dispatch a kernel"
    );
    assert_eq!(enabled, native);
    assert_eq!(pg_accel_stat_i64(&mut c, "stock_exec_count"), 0);
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_parallel_unsupported_float_agg_stays_native() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_fixtures(&mut c);
    use_default_parallel_planner(&mut c);

    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset stats");
    let sql = "SELECT SUM(v) FROM bench_f32_10m";
    let plan = explain(&mut c, sql);
    for needle in ["gather", "partial aggregate", "parallel seq scan"] {
        assert!(
            plan.contains(needle),
            "parallel aggregate native plan missing `{needle}`:\n{plan}"
        );
    }
    for needle in ["customscan(pg_accel)", "gpuagg", "gpureduce"] {
        assert!(
            !plan.contains(needle),
            "parallel aggregate should stay native without a GPU-producing child; got:\n{plan}"
        );
    }
    assert_rejection_reason_observed(
        &mut c,
        &["shape_unsupported_measure_type"],
        "parallel float aggregate should expose the generic measure-type decline",
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_parallel_full_sort_stays_native() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_fixtures(&mut c);
    use_default_parallel_planner(&mut c);

    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset stats");
    let plan = explain(&mut c, "SELECT * FROM bench_f32_10m ORDER BY v");
    assert!(
        plan.contains("gather"),
        "sort plan missing any Gather variant:\n{plan}"
    );
    assert!(
        plan.contains("sort") && plan.contains("parallel seq scan"),
        "full-output sort should stay on PostgreSQL's native parallel sort path:\n{plan}"
    );
    assert!(
        !plan.contains("customscan(pg_accel)") && !plan.contains("gpusort"),
        "full-output sort should not select GpuSort until output stays GPU-resident:\n{plan}"
    );
    assert_rejection_reason_observed(
        &mut c,
        &["sort_heap_full_output"],
        "full-output sort should expose the heap-output native-decline gate",
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_parallel_hashjoin() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_fixtures(&mut c);
    use_default_parallel_planner(&mut c);

    let sql = "SELECT f.*, d.name FROM bench_fact f JOIN bench_dim d USING(id)";
    assert_plan_contains(&mut c, sql, &["Hash Join"]);
    assert_plan_lacks(
        &mut c,
        sql,
        &["CustomScan(pg_accel)", "GpuHashJoin", "GpuAccelHashJoin"],
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_grouped_hashagg_100k_declines_gpu() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_hashagg_gate_fixture(&mut c);
    use_default_parallel_planner(&mut c);

    let sql = "SELECT grp, SUM(val), COUNT(*) FROM bench_hashagg_gate GROUP BY grp";
    assert_plan_contains(&mut c, sql, &["HashAggregate"]);
    assert_plan_lacks(
        &mut c,
        sql,
        &["CustomScan(pg_accel)", "GpuAgg", "GpuAccelAgg"],
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_nlj_between_non_equality_join_stays_native() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_nlj_between_fixture(&mut c);
    let sql = "SELECT count(*) \
               FROM bench_nlj_events e \
               JOIN bench_nlj_windows w \
                 ON e.ts >= w.lo AND e.ts <= w.hi";

    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native_count = scalar_i64(&mut c, sql);

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.cost_multiplier = DEFAULT",
        "SET pg_accel.min_batch_size = DEFAULT",
        "RESET enable_nestloop",
        "SELECT pg_accel_reset_stats()",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    let plan = explain(&mut c, sql);
    assert!(
        plan.contains("nested loop"),
        "BETWEEN fixture should remain on PostgreSQL's native nested-loop plan:\n{plan}"
    );
    for needle in ["custom scan", "gpuacceljoin", "gpunestedloopineq"] {
        assert!(
            !plan.contains(needle),
            "crash-gated NLJ BETWEEN must not select `{needle}`:\n{plan}"
        );
    }
    assert_rejection_reason_observed(
        &mut c,
        &["shape_non_equality_join"],
        &format!("NLJ BETWEEN should expose its non-equality join decline; plan:\n{plan}"),
    );

    let before = kernel_executions(&mut c);
    let accel_count = scalar_i64(&mut c, sql);
    let after = kernel_executions(&mut c);

    assert_eq!(
        accel_count, native_count,
        "crash-gated NLJ BETWEEN count differs from native PostgreSQL"
    );
    assert_eq!(
        after, before,
        "native-declined NLJ BETWEEN query must not dispatch GPU work \
         (before={before}, after={after})"
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_postgis_point_polygon_intersects_declines_until_exact_semantics() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_postgis_point_polygon_fixture(&mut c);
    let polygon = polygon_wkt(2200, 50.0, 12.5, 8.0);
    let polygon_4326 = format!("SRID=4326;{polygon}");

    for (label, sql) in [
        (
            "Point column × constant Polygon",
            format!(
                "SELECT count(*) FROM bench_postgis_pip_gate \
                 WHERE ST_Intersects(geom, '{polygon_4326}'::geometry)"
            ),
        ),
        (
            "constant Polygon × Point column",
            format!(
                "SELECT count(*) FROM bench_postgis_pip_gate \
                 WHERE ST_Intersects('{polygon_4326}'::geometry, geom)"
            ),
        ),
    ] {
        for stmt in [
            "SET pg_accel.enabled = on",
            "SET pg_accel.cost_multiplier = DEFAULT",
            "SELECT pg_accel_reset_stats()",
        ] {
            c.simple_query(stmt).expect(stmt);
        }

        let plan = explain(&mut c, &sql);
        assert!(
            !plan.contains("custom scan") && !plan.contains("gpuspatial"),
            "{label} ST_Intersects must stay native until exact fp64 PostGIS semantics land:\n{plan}"
        );
        assert_rejection_reason_observed(
            &mut c,
            &["postgis_intersects_unsupported_shape"],
            &format!("{label} ST_Intersects should expose a native-decline gate; plan:\n{plan}"),
        );
    }
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_postgis_intersects_unsupported_shape_stays_native() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    for stmt in [
        "CREATE EXTENSION IF NOT EXISTS postgis CASCADE",
        "CREATE UNLOGGED TABLE IF NOT EXISTS bench_postgis_intersects_unsupported_gate \
         (id int4, geom geometry NOT NULL)",
        "TRUNCATE bench_postgis_intersects_unsupported_gate",
        "INSERT INTO bench_postgis_intersects_unsupported_gate (id, geom) \
         SELECT g, ST_SetSRID(\
                    ST_MakePoint((g % 1000)::float8 / 10.0, \
                                 ((g / 1000) % 250)::float8 / 10.0), \
                    4326)::geometry \
         FROM generate_series(1, 250000) AS g",
        "ANALYZE bench_postgis_intersects_unsupported_gate",
        "SET pg_accel.enabled = on",
        "SET pg_accel.cost_multiplier = DEFAULT",
        "SELECT pg_accel_reset_stats()",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    let polygon = polygon_wkt(2200, 50.0, 12.5, 8.0);
    let polygon_4326 = format!("SRID=4326;{polygon}");
    let sql = format!(
        "SELECT count(*) FROM bench_postgis_intersects_unsupported_gate \
         WHERE ST_Intersects(geom, '{polygon_4326}'::geometry)"
    );
    let plan = explain(&mut c, &sql);
    assert!(
        !plan.contains("custom scan") && !plan.contains("gpuspatial"),
        "generic geometry ST_Intersects must stay native behind the shape gate:\n{plan}"
    );
    assert_rejection_reason_observed(
        &mut c,
        &["postgis_intersects_unsupported_shape"],
        &format!(
            "generic geometry ST_Intersects should expose a native-decline gate; plan:\n{plan}"
        ),
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_direct_gpuexpr_template_scan_stays_native_until_resident() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let table = ensure_gpuexpr_direct_isolated_fixture(&mut c, "direct_template");
    let sql = format!("SELECT count(*) FROM {table} WHERE val > 500.0::float4");

    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native_count = scalar_i64(&mut c, &sql);

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.cost_multiplier = DEFAULT",
        "SET pg_accel.min_batch_size = DEFAULT",
        "SET max_parallel_workers_per_gather = DEFAULT",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset stats");
    let plan = explain(&mut c, &sql);
    assert!(
        !plan.contains("custom scan"),
        "template-safe numeric WHERE must stay native until GpuExpr scan is GPU-resident:\n{plan}"
    );
    assert!(
        plan.contains("aggregate") && plan.contains("seq scan"),
        "template-safe numeric WHERE should use PostgreSQL native aggregate/scan:\n{plan}"
    );
    assert_rejection_reason_observed(
        &mut c,
        &["shape_unsupported_predicate"],
        "direct GpuExpr template scan",
    );

    let before = kernel_executions(&mut c);
    let accel_count = scalar_i64(&mut c, &sql);
    let after = kernel_executions(&mut c);

    assert_eq!(
        accel_count, native_count,
        "native direct-template count changed under pg_accel hooks"
    );
    assert_eq!(
        after, before,
        "native direct-template query must not dispatch pg_accel kernels"
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_fused_gpuexpr_count_stays_native_until_resident() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let table = ensure_gpuexpr_direct_isolated_fixture(&mut c, "fused_count_core");

    let cases = [
        (
            "single predicate",
            format!("SELECT count(*) FROM {table} WHERE val > 500.0::float4"),
            FusedCountExpectation {
                count: 499_000,
                rows_dispatched: 1_000_000,
                batches: 1,
                requires_gpuexpr_child: true,
            },
        ),
        (
            "two predicates",
            format!(
                "SELECT count(*) FROM {table} \
                 WHERE val > 500.0::float4 AND category < 50"
            ),
            FusedCountExpectation {
                count: 249_000,
                rows_dispatched: 1_000_000,
                batches: 1,
                requires_gpuexpr_child: true,
            },
        ),
    ];

    for (label, sql, expected) in cases {
        assert_fused_gpuexpr_count_matches_native(&mut c, label, &sql, expected);
    }
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_gpu_only_declines_host_staged_fused_reduce() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let table = ensure_gpuexpr_direct_isolated_fixture(&mut c, "gpu_only_fused_reduce");
    let sql = format!(
        "SELECT sum_val::float8, min_val::float8, max_val::float8, count_val \
         FROM ( \
             SELECT sum(val) AS sum_val, \
                    min(val) AS min_val, \
                    max(val) AS max_val, \
                    count(val) AS count_val \
             FROM {table} \
             WHERE val > 500.0::float4 AND category < 50 \
         ) AS fused_reduce"
    );

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.cost_multiplier = DEFAULT",
        "SET pg_accel.min_batch_size = DEFAULT",
        "SET max_parallel_workers_per_gather = DEFAULT",
        "SELECT pg_accel_reset_stats()",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    let plan = explain(&mut c, &sql);
    assert!(
        !plan.contains("custom scan") && !plan.contains("gpuagg"),
        "pg_accel must decline host-staged fused reduce:\n{plan}"
    );
    assert!(
        plan.contains("aggregate") && plan.contains("seq scan"),
        "pg_accel should leave the query on PostgreSQL native aggregate/scan:\n{plan}"
    );
    assert_rejection_reason_observed(
        &mut c,
        &["shape_unsupported_predicate"],
        "GPU-only fused reduce",
    );

    let before = kernel_executions(&mut c);
    let native = scalar_float4_reduce(&mut c, &sql);
    let after = kernel_executions(&mut c);
    assert!(
        native.count > 0,
        "GPU-only fixture/query should select at least one non-null value"
    );
    assert_eq!(
        after, before,
        "GPU-only native execution must not dispatch pg_accel kernels"
    );
    assert_eq!(
        pg_accel_stat_i64(&mut c, "stock_exec_count"),
        0,
        "GPU-only native execution must not enter pg_accel stock fallback"
    );

    c.simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .expect("drop GPU-only fused-reduce fixture");
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_fused_gpuexpr_float4_reduce_stays_native_until_resident() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let table = ensure_gpuexpr_direct_isolated_fixture(&mut c, "fused_reduce_core");
    let sql = format!(
        "SELECT sum_val::float8, min_val::float8, max_val::float8, count_val \
         FROM ( \
             SELECT sum(val) AS sum_val, \
                    min(val) AS min_val, \
                    max(val) AS max_val, \
                    count(val) AS count_val \
             FROM {table} \
             WHERE val > 500.0::float4 AND category < 50 \
         ) AS fused_reduce"
    );

    assert_fused_gpuexpr_float4_reduce_matches_native(
        &mut c,
        "two-predicate float4 filtered reduce",
        &sql,
        1_000_000,
        1,
    );

    c.simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .expect("drop isolated fused-reduce fixture");
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_fused_gpuexpr_int8_reduce_stays_native_until_resident() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let table = ensure_gpuexpr_bigint_count_isolated_fixture(&mut c, "fused_reduce_i64");
    let sql = format!(
        "SELECT min_val, max_val, count_val \
         FROM ( \
             SELECT min(bigval) AS min_val, \
                    max(bigval) AS max_val, \
                    count(*) AS count_val \
             FROM {table} \
             WHERE bigval > 500000::bigint AND category < 50 \
         ) AS fused_reduce"
    );

    assert_fused_gpuexpr_int8_reduce_matches_native(
        &mut c,
        "two-predicate bigint filtered reduce",
        &sql,
        1_000_000,
        1,
    );

    c.simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .expect("drop isolated fused-reduce i64 fixture");
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_fused_gpuexpr_count_bigint_exact_constants_stay_native_until_resident() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let table = ensure_gpuexpr_bigint_count_isolated_fixture(&mut c, "fused_count_bigint");

    let sql = format!(
        "SELECT count(*) FROM {table} \
         WHERE bigval > 500000::bigint AND category < 50"
    );
    assert_fused_gpuexpr_count_matches_native(
        &mut c,
        "bigint exact-constant two-predicate fused count",
        &sql,
        FusedCountExpectation {
            count: 250_000,
            rows_dispatched: 1_000_000,
            batches: 1,
            requires_gpuexpr_child: false,
        },
    );

    c.simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .expect("drop isolated bigint fused-count fixture");
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_parallel_filtered_count_guc_on_stays_native_at_generic_predicate_gate() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let table = ensure_gpuexpr_direct_isolated_fixture(&mut c, "parallel_fused_count");

    let sql = format!("SELECT count(*) FROM {table} WHERE val > 500.0::float4");
    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native_count = scalar_i64(&mut c, &sql);
    assert_eq!(native_count, 499_000);

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.parallel_fused_count = on",
        "SET pg_accel.cost_multiplier = DEFAULT",
        "SET pg_accel.min_batch_size = DEFAULT",
        "SET max_parallel_workers_per_gather = DEFAULT",
        "RESET min_parallel_table_scan_size",
        "RESET parallel_setup_cost",
        "RESET parallel_tuple_cost",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    let plan = explain(&mut c, &sql);
    for needle in ["gather", "partial aggregate", "parallel seq scan"] {
        assert!(
            plan.contains(needle),
            "generic predicate gate should keep the filtered count on PostgreSQL `{needle}` path:\n{plan}"
        );
    }
    assert!(
        !plan.contains("custom scan"),
        "the legacy GUC must not bypass generic descriptor predicate validation:\n{plan}"
    );
    assert_rejection_reason_observed(
        &mut c,
        &["shape_unsupported_predicate"],
        "parallel filtered count GUC-on generic predicate gate",
    );

    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset stats");
    let before = kernel_executions(&mut c);
    let analyzed = explain_analyze(&mut c, &sql);
    let after = kernel_executions(&mut c);
    assert!(
        !analyzed.contains("custom scan"),
        "EXPLAIN ANALYZE must stay native after generic predicate rejection:\n{analyzed}"
    );
    assert_eq!(
        after, before,
        "generic-predicate GUC-on native fallback must not dispatch GPU kernels \
         (before={before}, after={after})"
    );
    assert_eq!(pg_accel_stat_i64(&mut c, "stock_exec_count"), 0);
    assert_eq!(scalar_i64(&mut c, &sql), native_count);
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_parallel_filtered_count_guc_off_stays_native_at_generic_predicate_gate() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let table = ensure_gpuexpr_direct_isolated_fixture(&mut c, "parallel_fused_count_default");

    let sql = format!("SELECT count(*) FROM {table} WHERE val > 500.0::float4");
    let default_parallel_fused_count = c
        .query_one("SHOW pg_accel.parallel_fused_count", &[])
        .expect("SHOW pg_accel.parallel_fused_count")
        .get::<_, String>(0);
    assert_eq!(
        default_parallel_fused_count, "off",
        "parallel fused count must stay production-default off"
    );

    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native_count = scalar_i64(&mut c, &sql);
    assert_eq!(native_count, 499_000);

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.cost_multiplier = DEFAULT",
        "SET pg_accel.min_batch_size = DEFAULT",
        "SET max_parallel_workers_per_gather = DEFAULT",
        "RESET min_parallel_table_scan_size",
        "RESET parallel_setup_cost",
        "RESET parallel_tuple_cost",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    let plan = explain(&mut c, &sql);
    for needle in ["gather", "partial aggregate", "parallel seq scan"] {
        assert!(
            plan.contains(needle),
            "generic predicate gate should keep the GUC-off filtered count on PostgreSQL `{needle}` path:\n{plan}"
        );
    }
    assert!(
        !plan.contains("custom scan"),
        "GUC-off filtered count must not bypass generic descriptor predicate validation:\n{plan}"
    );
    assert_rejection_reason_observed(
        &mut c,
        &["shape_unsupported_predicate"],
        "parallel filtered count GUC-off generic predicate gate",
    );

    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset stats");
    let before = kernel_executions(&mut c);
    let analyzed = explain_analyze(&mut c, &sql);
    let after = kernel_executions(&mut c);
    assert!(
        !analyzed.contains("custom scan"),
        "EXPLAIN ANALYZE must stay on PostgreSQL native execution when the GUC is off:\n{analyzed}"
    );
    assert_eq!(
        after, before,
        "generic-predicate GUC-off native fallback must not dispatch GPU kernels \
         (before={before}, after={after})"
    );
    assert_eq!(pg_accel_stat_i64(&mut c, "stock_exec_count"), 0);
    assert_eq!(scalar_i64(&mut c, &sql), native_count);
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_parallel_filtered_count_bounded_stays_native_at_generic_predicate_gate() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let minimum = device_limit_i64(&mut c, "gpu_expr_min_rows");
    let rows = minimum
        .saturating_add((minimum / 4).max(1_024))
        .max(1_000_000);
    let table = ensure_gpuexpr_count_isolated_fixture(&mut c, "parallel_fused_bounded", rows);
    let sql = format!("SELECT count(*) FROM {table} WHERE val > 500.0::float4");

    set_parallel_count_native_mode(&mut c);
    let native_count = scalar_i64(&mut c, &sql);
    let native_plan = explain(&mut c, &sql);
    for needle in ["gather", "partial aggregate", "parallel seq scan"] {
        assert!(
            native_plan.contains(needle),
            "bounded native parallel baseline should select `{needle}`:\n{native_plan}"
        );
    }
    assert!(
        !native_plan.contains("custom scan"),
        "bounded native parallel baseline must not select pg_accel:\n{native_plan}"
    );

    set_parallel_count_accel_mode(&mut c);
    let accel_plan = explain(&mut c, &sql);
    for needle in ["gather", "partial aggregate", "parallel seq scan"] {
        assert!(
            accel_plan.contains(needle),
            "bounded generic-predicate decline should stay on `{needle}`:\n{accel_plan}"
        );
    }
    assert!(
        !accel_plan.contains("custom scan"),
        "bounded GUC-on parallel count must not bypass generic predicate validation:\n{accel_plan}"
    );
    assert_rejection_reason_observed(
        &mut c,
        &["shape_unsupported_predicate"],
        "bounded parallel filtered count generic predicate gate",
    );

    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset stats before bounded generic-predicate execution");
    let before = kernel_executions(&mut c);
    let analyzed = explain_analyze(&mut c, &sql);
    let after = kernel_executions(&mut c);
    assert!(
        !analyzed.contains("custom scan"),
        "bounded generic-predicate execution must stay native:\n{analyzed}"
    );
    assert_eq!(
        after, before,
        "bounded generic-predicate decline must not dispatch a GPU kernel"
    );
    assert_eq!(pg_accel_stat_i64(&mut c, "stock_exec_count"), 0);
    assert_eq!(scalar_i64(&mut c, &sql), native_count);

    c.simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .expect("drop bounded parallel fused-count fixture");
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_fused_gpuexpr_count_two_predicate_stays_native_until_resident() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let table = ensure_gpuexpr_fused_count_isolated_fixture(&mut c);
    let sql = format!(
        "SELECT count(*) FROM {table} \
         WHERE val > 500.0::float4 AND category < 50"
    );

    assert_fused_gpuexpr_count_matches_native(
        &mut c,
        "isolated PG18 two-predicate direct-USM fused count",
        &sql,
        FusedCountExpectation {
            count: 498_000,
            rows_dispatched: 2_000_000,
            batches: 1,
            requires_gpuexpr_child: true,
        },
    );

    c.simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .expect("drop isolated fused-count fixture");
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_fused_gpuexpr_count_nullable_prefix_stays_native_until_resident() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_gpuexpr_nullable_prefix_fixture(&mut c);

    assert_fused_gpuexpr_count_matches_native(
        &mut c,
        "nullable-prefix two predicates",
        "SELECT count(*) FROM bench_gpuexpr_nullable_prefix_gate \
         WHERE val > 500.0::float4 AND category < 50",
        FusedCountExpectation {
            count: 225_000,
            rows_dispatched: 1_000_000,
            batches: 1,
            requires_gpuexpr_child: true,
        },
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_fused_gpuexpr_count_multibatch_null_mask_stays_native_until_resident() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_gpuexpr_multibatch_mask_fixture(&mut c);

    assert_fused_gpuexpr_count_matches_native(
        &mut c,
        "multi-batch nullable two predicates",
        "SELECT count(*) FROM bench_gpuexpr_multibatch_mask_gate \
         WHERE val > 500.0::float4 AND category < 50",
        FusedCountExpectation {
            count: 1_068_156,
            rows_dispatched: 4_300_000,
            batches: 2,
            requires_gpuexpr_child: true,
        },
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_fused_gpuexpr_count_pg_nan_equality_stays_native_until_resident() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_gpuexpr_nan_fixture(&mut c);

    assert_fused_gpuexpr_count_matches_native(
        &mut c,
        "float4 NaN equality",
        "SELECT count(*) FROM bench_gpuexpr_nan_gate \
         WHERE val = 'NaN'::float4",
        FusedCountExpectation {
            count: 1_000_000,
            rows_dispatched: 1_000_000,
            batches: 1,
            requires_gpuexpr_child: true,
        },
    );
}
