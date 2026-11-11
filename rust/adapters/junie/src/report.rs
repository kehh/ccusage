use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::{
    BucketKind, LoadedEntry, Result, SessionAccumulator, UsageSummary,
    cli::{AgentReportKind, WeekDay},
    summarize_by_key, summarize_summaries_by_bucket, totals_json,
};

pub fn report_from_rows(rows: &[UsageSummary], kind: AgentReportKind) -> Value {
    let rows_json = rows
        .iter()
        .map(|row| ccusage_core::agent_summary_json(row, kind, kind == AgentReportKind::Session))
        .collect::<Vec<_>>();
    json!({
        rows_key(kind): rows_json,
        "totals": totals_json(rows),
    })
}

pub fn summarize_entries(
    entries: &[LoadedEntry],
    kind: AgentReportKind,
) -> Result<Vec<UsageSummary>> {
    match kind {
        AgentReportKind::Daily => summarize_by_key(
            entries,
            |entry| entry.date.clone(),
            |date| (date.to_string(), None),
        ),
        AgentReportKind::Monthly => {
            let daily = summarize_entries(entries, AgentReportKind::Daily)?;
            Ok(summarize_summaries_by_bucket(
                &daily,
                BucketKind::Monthly,
                WeekDay::Sunday,
            ))
        }
        AgentReportKind::Session => {
            let mut groups = BTreeMap::<String, SessionAccumulator>::new();
            for entry in entries {
                groups
                    .entry(entry.session_id.to_string())
                    .or_default()
                    .add_entry(entry);
            }
            groups
                .into_values()
                .map(SessionAccumulator::into_summary)
                .collect()
        }
        AgentReportKind::Weekly => {
            let daily = summarize_entries(entries, AgentReportKind::Daily)?;
            Ok(summarize_summaries_by_bucket(
                &daily,
                BucketKind::Weekly,
                WeekDay::Monday,
            ))
        }
    }
}

/// Daily rows keyed by (date, project), `UsageSummary.project` set — the
/// `--instances` view. The composite key splits on the first `|`, so a
/// project path containing `|` stays intact (dates never contain one).
pub fn summarize_daily_grouped_by_project(entries: &[LoadedEntry]) -> Result<Vec<UsageSummary>> {
    summarize_by_key(
        entries,
        |entry| format!("{}|{}", entry.date, entry.project),
        |key| {
            let (date, project) = key.split_once('|').unwrap_or((key, "unknown"));
            (date.to_string(), Some(project.to_string()))
        },
    )
}

fn rows_key(kind: AgentReportKind) -> &'static str {
    match kind {
        AgentReportKind::Daily => "daily",
        AgentReportKind::Weekly => "weekly",
        AgentReportKind::Monthly => "monthly",
        AgentReportKind::Session => "sessions",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{TokenUsageRaw, UsageEntry, UsageMessage};

    #[test]
    fn snapshots_daily_weekly_monthly_and_session_json_reports() {
        let entries = snapshot_entries();
        let daily = summarize_entries(&entries, AgentReportKind::Daily).unwrap();
        let weekly = summarize_entries(&entries, AgentReportKind::Weekly).unwrap();
        let monthly = summarize_entries(&entries, AgentReportKind::Monthly).unwrap();
        let sessions = summarize_entries(&entries, AgentReportKind::Session).unwrap();
        assert_eq!(
            weekly
                .iter()
                .map(|row| row.week.as_deref())
                .collect::<Vec<_>>(),
            [Some("2026-10-26"), Some("2026-11-02")]
        );

        insta::assert_json_snapshot!(
            "junie_daily_json",
            report_from_rows(&daily, AgentReportKind::Daily)
        );
        insta::assert_json_snapshot!(
            "junie_weekly_json",
            report_from_rows(&weekly, AgentReportKind::Weekly)
        );
        insta::assert_json_snapshot!(
            "junie_monthly_json",
            report_from_rows(&monthly, AgentReportKind::Monthly)
        );
        insta::assert_json_snapshot!(
            "junie_sessions_json",
            report_from_rows(&sessions, AgentReportKind::Session)
        );
    }

    #[test]
    fn weekly_rows_land_in_monday_starting_buckets() {
        // 2026-10-31 is a Saturday, 2026-11-01 a Sunday, 2026-11-02 a Monday:
        // the fixture spans a Sunday-to-Monday boundary, so only Monday-start
        // weeks produce exactly these two buckets.
        let weekly = summarize_entries(&snapshot_entries(), AgentReportKind::Weekly).unwrap();

        assert_eq!(
            weekly
                .iter()
                .map(|row| row.week.as_deref())
                .collect::<Vec<_>>(),
            [Some("2026-10-26"), Some("2026-11-02")]
        );
    }

    #[test]
    fn session_rows_carry_session_id_and_project() {
        let sessions = summarize_entries(&snapshot_entries(), AgentReportKind::Session).unwrap();
        let report = report_from_rows(&sessions, AgentReportKind::Session);

        let session_a = &report["sessions"][0];
        assert_eq!(session_a["sessionId"], "session-a");
        assert_eq!(session_a["projectPath"], "/tmp/proj-a");
        let session_b = &report["sessions"][1];
        assert_eq!(session_b["sessionId"], "session-b");
        assert_eq!(session_b["projectPath"], "/tmp/proj-b");
    }

    #[test]
    fn grouped_by_project_yields_one_row_per_date_and_project() {
        let rows = summarize_daily_grouped_by_project(&snapshot_entries()).unwrap();

        assert_eq!(
            rows.iter()
                .map(|row| (
                    row.date.as_deref().unwrap(),
                    row.project.as_deref().unwrap()
                ))
                .collect::<Vec<_>>(),
            [
                ("2026-10-31", "/tmp/proj-a"),
                ("2026-11-01", "/tmp/proj-a"),
                ("2026-11-01", "/tmp/proj-b"),
                ("2026-11-02", "/tmp/proj-b"),
            ]
        );
        assert!(rows.iter().all(|row| row.project.is_some()));
    }

    fn snapshot_entries() -> Vec<LoadedEntry> {
        [
            entry(
                "session-a",
                "/tmp/proj-a",
                "2026-10-31T12:00:00Z",
                "gpt-4.1-2025-04-14",
                TokenUsageRaw {
                    input_tokens: 100,
                    output_tokens: 25,
                    cache_creation_input_tokens: 15,
                    cache_read_input_tokens: 10,
                    ..TokenUsageRaw::default()
                },
                0.002,
                Some(0.002),
                None,
            ),
            entry(
                "session-a",
                "/tmp/proj-a",
                "2026-11-01T12:00:00Z",
                "Qwen/Qwen3.6-27B-FP8",
                TokenUsageRaw {
                    input_tokens: 40,
                    output_tokens: 12,
                    cache_creation_input_tokens: 5,
                    cache_read_input_tokens: 5,
                    ..TokenUsageRaw::default()
                },
                0.001,
                Some(0.001),
                None,
            ),
            entry(
                "session-b",
                "/tmp/proj-b",
                "2026-11-01T18:00:00Z",
                "gpt-4.1-2025-04-14",
                TokenUsageRaw {
                    input_tokens: 12,
                    output_tokens: 3,
                    ..TokenUsageRaw::default()
                },
                0.0,
                None,
                None,
            ),
            entry(
                "session-b",
                "/tmp/proj-b",
                "2026-11-02T09:30:00Z",
                "vendor/custom-model",
                TokenUsageRaw {
                    input_tokens: 60,
                    output_tokens: 7,
                    ..TokenUsageRaw::default()
                },
                0.0,
                None,
                Some("vendor/custom-model"),
            ),
        ]
        .into()
    }

    fn entry(
        session_id: &str,
        project: &str,
        timestamp: &str,
        model: &str,
        usage: TokenUsageRaw,
        cost: f64,
        stored_cost: Option<f64>,
        missing_pricing_model: Option<&str>,
    ) -> LoadedEntry {
        let timestamp = crate::parse_ts_timestamp(timestamp).expect("valid snapshot timestamp");
        let timestamp_text = crate::format_rfc3339_millis(timestamp);
        // The fixture dates assume UTC; format with an explicit zone so the
        // local timezone cannot shift the 18:00Z entry across a day boundary.
        let tz = crate::parse_tz(Some("UTC"));
        let date = crate::format_date_tz(timestamp, tz.as_ref());
        LoadedEntry {
            data: UsageEntry {
                session_id: Some(session_id.to_string()),
                timestamp: timestamp_text,
                version: None,
                message: UsageMessage {
                    usage,
                    model: Some(model.to_string()),
                    id: Some(format!("request-{date}")),
                },
                cost_usd: stored_cost,
                request_id: None,
                is_api_error_message: None,
                is_sidechain: None,
            },
            timestamp,
            date,
            project: Arc::from(project),
            session_id: Arc::from(session_id),
            project_path: Arc::from(project),
            cost,
            extra_total_tokens: 0,
            credits: None,
            message_count: None,
            model: Some(model.to_string()),
            usage_limit_reset_time: None,
            missing_pricing_model: missing_pricing_model.map(str::to_string),
            stored_cost,
        }
    }
}
