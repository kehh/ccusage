use std::{collections::BTreeSet, path::Path, process::Command};

use ccusage_test_support::{Fixture, fs_fixture};
use serde_json::Value;

const PROJECT_A: &str = "/tmp/junie-project-a";
const PROJECT_B: &str = "/tmp/junie-project-b";

/// Session A alone: two events, the first carrying two models.
const PROJECT_A_TOTALS: &[(&str, u64)] = &[
    ("inputTokens", 3921),
    ("outputTokens", 293),
    ("cacheCreationTokens", 150),
    ("cacheReadTokens", 400),
    ("totalTokens", 4764),
];

/// Both fixture sessions.
const COMBINED_TOTALS: &[(&str, u64)] = &[
    ("inputTokens", 4176),
    ("outputTokens", 344),
    ("cacheCreationTokens", 180),
    ("cacheReadTokens", 450),
    ("totalTokens", 5150),
];

#[test]
fn snapshots_junie_focused_json_and_table_reports_for_all_kinds() {
    let fixture = junie_fixture();

    for kind in ["daily", "weekly", "monthly", "session"] {
        let json_output = run_kind(&fixture, kind, &[], true);
        let report: Value =
            serde_json::from_str(&json_output).expect("Junie JSON output should be valid");
        assert_fixture_report(&report, kind);
        insta::assert_json_snapshot!(format!("junie_cli_{kind}_json"), report);

        let table_output = run_kind(&fixture, kind, &[], false);
        insta::assert_snapshot!(format!("junie_cli_{kind}_table"), table_output);
    }
}

#[test]
fn legacy_event_reports_under_index_created_at() {
    let fixture = legacy_fixture();
    let output = run_cli(
        &fixture,
        &[
            "junie",
            "daily",
            "--since",
            "20260501",
            "--until",
            "20260601",
            "--offline",
            "--no-color",
            "--timezone",
            "UTC",
            "--json",
        ],
    );
    let report: Value = serde_json::from_str(&output).expect("legacy JSON output should be valid");

    assert_eq!(report["daily"][0]["date"], "2026-05-20");
    assert_eq!(report["totals"]["inputTokens"].as_u64(), Some(1121));
    assert_eq!(report["totals"]["outputTokens"].as_u64(), Some(1));
    assert_eq!(report["totals"]["totalTokens"].as_u64(), Some(1122));
    assert!((report["totals"]["totalCost"].as_f64().unwrap() - 0.00225).abs() < 1e-12);
}

#[test]
fn junie_project_filter_reports_only_matching_project() {
    let fixture = junie_fixture();

    let daily = run_kind(&fixture, "daily", &["--project", PROJECT_A], true);
    let report: Value = serde_json::from_str(&daily).expect("filtered JSON output should be valid");
    assert_totals(&report, PROJECT_A_TOTALS);
    assert!(
        !daily.contains(PROJECT_B),
        "project B must not leak into a project-A run: {daily}"
    );
    assert!(
        !daily.contains("claude-sonnet-4-20250514"),
        "project B's model must not leak into a project-A run: {daily}"
    );

    // The filter is entry selection, so it also narrows weekly reports.
    let weekly = run_kind(&fixture, "weekly", &["--project", PROJECT_A], true);
    let report: Value =
        serde_json::from_str(&weekly).expect("filtered weekly JSON should be valid");
    assert_totals(&report, PROJECT_A_TOTALS);
    assert!(!weekly.contains(PROJECT_B));
}

#[test]
fn junie_instances_json_groups_by_project() {
    let fixture = junie_fixture();
    let output = run_kind(&fixture, "daily", &["--instances"], true);
    let report: Value = serde_json::from_str(&output).expect("instances JSON should be valid");

    let top_level = report
        .as_object()
        .expect("instances output should be an object")
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        top_level,
        BTreeSet::from(["projects".to_string(), "totals".to_string()])
    );

    let projects = report["projects"]
        .as_object()
        .expect("projects should be an object");
    assert_eq!(
        projects.keys().collect::<Vec<_>>(),
        [PROJECT_A, PROJECT_B],
        "both fixture projects should be present"
    );

    let project_a_tokens: u64 = projects[PROJECT_A]
        .as_array()
        .expect("per-project rows should be an array")
        .iter()
        .map(|row| row["totalTokens"].as_u64().expect("row totalTokens"))
        .sum();
    assert_eq!(
        project_a_tokens, 4764,
        "project A must match session A alone"
    );
    assert_totals(&report, COMBINED_TOTALS);
}

#[test]
fn junie_instances_json_groups_unindexed_sessions_under_unknown() {
    let fixture = fs_fixture!({
        "sessions/session-261007-120000-aaaa/events.jsonl": include_str!(
            "../../../adapters/junie/tests/fixtures/current/events.jsonl"
        ),
        "sessions/session-261008-130000-bbbb/events.jsonl": include_str!(
            "../../../adapters/junie/tests/fixtures/current/events-b.jsonl"
        ),
    });

    let output = run_kind(&fixture, "daily", &["--instances"], true);
    let report: Value = serde_json::from_str(&output).expect("instances JSON should be valid");
    let projects = report["projects"]
        .as_object()
        .expect("projects should be an object");

    assert_eq!(
        projects.keys().collect::<Vec<_>>(),
        ["unknown"],
        "sessions missing from index.jsonl group under the unknown project"
    );
    assert_totals(&report, COMBINED_TOTALS);
}

#[test]
fn snapshots_junie_instances_and_project_alias_tables() {
    let fixture = junie_fixture();

    let grouped = run_kind(&fixture, "daily", &["--instances"], false);
    insta::assert_snapshot!("junie_cli_daily_instances_table", grouped);

    let aliased = run_kind(
        &fixture,
        "daily",
        &[
            "--instances",
            "--project-aliases",
            "/tmp/junie-project-a=Project A,/tmp/junie-project-b=Project B",
        ],
        false,
    );
    insta::assert_snapshot!("junie_cli_daily_instances_aliases_table", aliased);
}

#[test]
fn junie_project_config_matches_the_project_flag() {
    let fixture = junie_fixture();
    let config_path = fixture.write_file(
        "ccusage.json",
        r#"{"junie":{"defaults":{"project":"/tmp/junie-project-a"}}}"#,
    );
    let config_path = config_path.to_string_lossy().into_owned();

    let configured = run_kind(&fixture, "daily", &["--config", &config_path], true);
    let flagged = run_kind(&fixture, "daily", &["--project", PROJECT_A], true);
    let configured: Value = serde_json::from_str(&configured).expect("config JSON should be valid");
    let flagged: Value = serde_json::from_str(&flagged).expect("flag JSON should be valid");

    assert_eq!(configured, flagged);
}

#[test]
#[ignore = "reads the developer's real ~/.junie corpus"]
fn real_junie_home_reports_usage() {
    let Some(home) = std::env::var_os("HOME") else {
        eprintln!("skipping: HOME is not set");
        return;
    };
    if !Path::new(&home).join(".junie").join("sessions").is_dir() {
        eprintln!("skipping: ~/.junie/sessions is absent");
        return;
    }

    let output = Command::new(env!("CARGO_BIN_EXE_ccusage"))
        .args(["junie", "daily", "--json", "--offline", "--no-color"])
        .env("LOG_LEVEL", "0")
        .env("NO_COLOR", "1")
        .output()
        .expect("ccusage should run");
    assert!(
        output.status.success(),
        "ccusage junie daily failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value =
        serde_json::from_slice(&output.stdout).expect("real-home JSON should be valid");
    let total_tokens = report["totals"]["totalTokens"].as_u64().unwrap_or(0);
    assert!(total_tokens > 0, "expected real Junie usage, got {report}");
}

fn assert_fixture_report(report: &Value, kind: &str) {
    assert_totals(report, COMBINED_TOTALS);
    assert!((report["totals"]["totalCost"].as_f64().unwrap() - 0.00885).abs() < 1e-12);

    let (rows_key, period_key, expected_periods): (&str, &str, &[&str]) = match kind {
        "daily" => ("daily", "date", &["2026-10-07", "2026-10-08"]),
        "weekly" => ("weekly", "week", &["2026-10-05"]),
        "monthly" => ("monthly", "month", &["2026-10"]),
        "session" => (
            "sessions",
            "sessionId",
            &["session-261007-120000-aaaa", "session-261008-130000-bbbb"],
        ),
        _ => unreachable!("test iterates over supported Junie reports"),
    };
    let rows = report[rows_key]
        .as_array()
        .expect("report rows should be an array");
    let mut periods = rows
        .iter()
        .map(|row| row[period_key].as_str().expect("period should be a string"))
        .collect::<Vec<_>>();
    periods.sort_unstable();
    assert_eq!(periods, expected_periods.to_vec());

    let models = rows
        .iter()
        .flat_map(|row| {
            row["modelsUsed"]
                .as_array()
                .expect("modelsUsed should be an array")
        })
        .map(|model| model.as_str().expect("model should be a string"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        models,
        BTreeSet::from([
            "gpt-4.1-2025-04-14",
            "Qwen/Qwen3.6-27B-FP8",
            "claude-sonnet-4-20250514",
        ])
    );
}

fn assert_totals(report: &Value, expected: &[(&str, u64)]) {
    let totals = &report["totals"];
    for (key, value) in expected {
        assert_eq!(totals[key].as_u64(), Some(*value), "{key} totals");
    }
    assert!(
        totals.get("unpricedModels").is_none(),
        "auto mode keeps recorded costs, so no model should be reported unpriced"
    );
}

fn junie_fixture() -> Fixture {
    fs_fixture!({
        "sessions/session-261007-120000-aaaa/events.jsonl": include_str!(
            "../../../adapters/junie/tests/fixtures/current/events.jsonl"
        ),
        "sessions/session-261008-130000-bbbb/events.jsonl": include_str!(
            "../../../adapters/junie/tests/fixtures/current/events-b.jsonl"
        ),
        "sessions/index.jsonl": include_str!(
            "../../../adapters/junie/tests/fixtures/current/sessions/index.jsonl"
        ),
    })
}

fn legacy_fixture() -> Fixture {
    fs_fixture!({
        "sessions/session-260520-093639-1r02/events.jsonl": include_str!(
            "../../../adapters/junie/tests/fixtures/legacy/events.jsonl"
        ),
        "sessions/index.jsonl": include_str!(
            "../../../adapters/junie/tests/fixtures/legacy/sessions/index.jsonl"
        ),
    })
}

fn run_kind(fixture: &Fixture, kind: &str, extra: &[&str], json: bool) -> String {
    let mut args = vec![
        "junie",
        kind,
        "--since",
        "20261001",
        "--until",
        "20261101",
        "--offline",
        "--no-color",
        "--timezone",
        "UTC",
    ];
    args.extend_from_slice(extra);
    if json {
        args.push("--json");
    }
    run_cli(fixture, &args)
}

fn run_cli(fixture: &Fixture, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_ccusage"))
        .env_clear()
        .env("JUNIE_HOME", fixture.root())
        .env("HOME", fixture.path("home"))
        .env("USERPROFILE", fixture.path("userprofile"))
        .env("XDG_CONFIG_HOME", fixture.path("xdg-config"))
        .env("XDG_DATA_HOME", fixture.path("xdg-data"))
        .env("COLUMNS", "120")
        .env("LOG_LEVEL", "0")
        .env("NO_COLOR", "1")
        .args(args)
        .output()
        .expect("ccusage should run");
    assert!(
        output.status.success(),
        "ccusage {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("CLI stderr should be UTF-8");
    if args.contains(&"--json") {
        assert!(stderr.is_empty(), "JSON report should not write to stderr");
        stdout
    } else {
        format!("stdout:\n{stdout}\nstderr:\n{stderr}")
    }
}
