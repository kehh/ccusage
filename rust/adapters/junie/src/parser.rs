use std::{collections::BTreeMap, fs, path::Path, sync::Arc};

use jiff::tz::TimeZone as JiffTimeZone;
use serde::Deserialize;

use crate::{
    LoadedEntry, PricingMap, TimestampMs, TokenUsageRaw, UsageEntry, UsageMessage,
    calculate_cost_for_usage_at, cli::CostMode, fast::LinePrefilter, format_date_tz,
    missing_pricing_model_for_usage, stored_cost_for_mode,
};
use ccusage_adapter_common::jsonl;

const LLM_RESPONSE_METADATA_EVENT: &str = "LlmResponseMetadataEvent";

/// Session metadata from `sessions/index.jsonl`, keyed by `sessionId`.
#[derive(Debug, Clone)]
pub(super) struct JunieSessionMeta {
    created_at: Option<i64>,
    pub(super) project_dir: Option<String>,
}

/// One `index.jsonl` line. Only the fields ccusage consumes are declared.
#[derive(Debug, Deserialize)]
struct JunieSessionIndexLine {
    #[serde(
        rename = "sessionId",
        default,
        deserialize_with = "jsonl::non_empty_string"
    )]
    session_id: Option<String>,
    #[serde(rename = "createdAt", default, deserialize_with = "jsonl::lenient_i64")]
    created_at: Option<i64>,
    #[serde(
        rename = "projectDir",
        default,
        deserialize_with = "jsonl::non_empty_string"
    )]
    project_dir: Option<String>,
}

/// A single `events.jsonl` line. Only the fields ccusage consumes are declared;
/// serde skips everything else. Legacy lines (Junie 1588.x) lack `timestampMs`.
#[derive(Debug, Deserialize)]
struct JunieWireLine {
    event: Option<JunieWireEvent>,
    #[serde(
        rename = "timestampMs",
        default,
        deserialize_with = "jsonl::lenient_i64"
    )]
    timestamp_ms: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct JunieWireEvent {
    #[serde(rename = "agentEvent")]
    agent_event: Option<JunieAgentEvent>,
}

#[derive(Debug, Deserialize)]
struct JunieAgentEvent {
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    kind: Option<String>,
    #[serde(rename = "modelUsage", default)]
    model_usage: Vec<JunieModelUsage>,
}

/// One per-model usage record inside an event's `modelUsage` array.
#[derive(Debug, Deserialize)]
struct JunieModelUsage {
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    model: Option<String>,
    #[serde(default, deserialize_with = "jsonl::lenient_f64")]
    cost: Option<f64>,
    #[serde(
        rename = "inputTokens",
        default,
        deserialize_with = "jsonl::lenient_u64"
    )]
    input_tokens: u64,
    #[serde(
        rename = "cacheInputTokens",
        default,
        deserialize_with = "jsonl::lenient_u64"
    )]
    cache_input_tokens: u64,
    #[serde(
        rename = "cacheCreateTokens",
        default,
        deserialize_with = "jsonl::lenient_u64"
    )]
    cache_create_tokens: u64,
    #[serde(
        rename = "outputTokens",
        default,
        deserialize_with = "jsonl::lenient_u64"
    )]
    output_tokens: u64,
}

/// One usage record: one `modelUsage` entry of one event, resolved to a
/// timestamp through the session fallback chain.
#[derive(Debug, Clone)]
pub(super) struct JunieUsageEntry {
    timestamp: TimestampMs,
    timestamp_text: String,
    pub(super) session_id: String,
    model: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_create_tokens: u64,
    cost_usd: Option<f64>,
}

/// Parse `sessions/index.jsonl` into session metadata, tolerating malformed
/// lines; a missing or unreadable index yields an empty map.
pub(super) fn read_session_index(path: &Path) -> BTreeMap<String, JunieSessionMeta> {
    let Ok(content) = fs::read(path) else {
        return BTreeMap::new();
    };
    jsonl::records::<JunieSessionIndexLine>(&content, None)
        .filter_map(|line| {
            let session_id = line.session_id?;
            Some((
                session_id,
                JunieSessionMeta {
                    created_at: line.created_at,
                    project_dir: line.project_dir,
                },
            ))
        })
        .collect()
}

/// Read one session's `events.jsonl` into usage entries. Malformed lines are
/// skipped; a record is dropped only when all four token counts are 0 and the
/// cost is missing or zero.
pub(super) fn read_events_file(
    path: &Path,
    index: &BTreeMap<String, JunieSessionMeta>,
) -> Vec<JunieUsageEntry> {
    let session_id = extract_session_id(path);
    let session_meta = index.get(&session_id);
    let fallback_timestamp = file_modified_timestamp(path);
    let Ok(content) = fs::read(path) else {
        return Vec::new();
    };
    // A real corpus is ~2.3 GB of events with only ~63k usage lines, so
    // prefilter on the byte marker before deserializing any line.
    let prefilter = LinePrefilter::any(&[b"LlmResponseMetadataEvent"]);
    jsonl::records::<JunieWireLine>(&content, Some(&prefilter))
        .flat_map(|line| line_to_entries(&line, &session_id, session_meta, fallback_timestamp))
        .collect()
}

fn line_to_entries(
    line: &JunieWireLine,
    session_id: &str,
    session_meta: Option<&JunieSessionMeta>,
    fallback_timestamp: TimestampMs,
) -> Vec<JunieUsageEntry> {
    let Some(agent_event) = line
        .event
        .as_ref()
        .and_then(|event| event.agent_event.as_ref())
    else {
        return Vec::new();
    };
    if agent_event.kind.as_deref() != Some(LLM_RESPONSE_METADATA_EVENT) {
        return Vec::new();
    }
    let timestamp = resolve_timestamp(line, session_meta, session_id, fallback_timestamp);
    let timestamp_text = crate::format_rfc3339_millis(timestamp);
    agent_event
        .model_usage
        .iter()
        .filter(|usage| !is_empty_usage(usage))
        .map(|usage| JunieUsageEntry {
            timestamp,
            timestamp_text: timestamp_text.clone(),
            session_id: session_id.to_string(),
            model: usage.model.clone().unwrap_or_else(|| "unknown".to_string()),
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_input_tokens,
            cache_create_tokens: usage.cache_create_tokens,
            cost_usd: usage.cost,
        })
        .collect()
}

/// A record is dropped only when it carries no usage at all: all four token
/// counts 0 and cost missing or zero.
fn is_empty_usage(usage: &JunieModelUsage) -> bool {
    usage.input_tokens == 0
        && usage.output_tokens == 0
        && usage.cache_input_tokens == 0
        && usage.cache_create_tokens == 0
        && usage.cost.unwrap_or(0.0) == 0.0
}

/// Timestamp resolution chain: the event's own `timestampMs`, then the
/// session's index `createdAt` (legacy events carry no timestamp), then the
/// `session-YYMMDD-HHMMSS-*` directory name, then the file's mtime.
fn resolve_timestamp(
    line: &JunieWireLine,
    session_meta: Option<&JunieSessionMeta>,
    session_id: &str,
    file_mtime: TimestampMs,
) -> TimestampMs {
    line.timestamp_ms
        .map(TimestampMs::from_millis)
        .or_else(|| {
            session_meta
                .and_then(|meta| meta.created_at)
                .map(TimestampMs::from_millis)
        })
        .or_else(|| parse_session_dir_timestamp(session_id))
        .unwrap_or(file_mtime)
}

/// Parse the `session-YYMMDD-HHMMSS-XXXX` directory name into a UTC
/// timestamp. Sessions absent from the index still bucket under their
/// creation date this way.
fn parse_session_dir_timestamp(session_id: &str) -> Option<TimestampMs> {
    let rest = session_id.strip_prefix("session-")?;
    let mut parts = rest.split('-');
    let date = parts.next()?;
    let time = parts.next()?;
    let digits = |value: &str| value.len() == 6 && value.bytes().all(|b| b.is_ascii_digit());
    if !digits(date) || !digits(time) {
        return None;
    }
    let year = 2000 + date.get(0..2)?.parse::<i16>().ok()?;
    let month = date.get(2..4)?.parse::<i8>().ok()?;
    let day = date.get(4..6)?.parse::<i8>().ok()?;
    let hour = time.get(0..2)?.parse::<i8>().ok()?;
    let minute = time.get(2..4)?.parse::<i8>().ok()?;
    let second = time.get(4..6)?.parse::<i8>().ok()?;
    let zoned = jiff::civil::date(year, month, day)
        .at(hour, minute, second, 0)
        .to_zoned(JiffTimeZone::UTC)
        .ok()?;
    Some(TimestampMs::from_millis(zoned.timestamp().as_millisecond()))
}

fn file_modified_timestamp(path: &Path) -> TimestampMs {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .map(TimestampMs::from_millis)
        .unwrap_or(TimestampMs::UNIX_EPOCH)
}

fn extract_session_id(file_path: &Path) -> String {
    file_path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("unknown")
        .to_string()
}

/// Composite dedupe key: two identical records (same session, time, model,
/// tokens, cost) are the same usage written twice. Tradeoff: identical
/// duplicate entries inside one event's `modelUsage` array would also
/// collapse — pathological in practice (a response round hitting the same
/// model twice with identical token counts and cost), accepted.
pub(super) fn junie_entry_key(entry: &JunieUsageEntry) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{}:{:?}",
        entry.session_id,
        entry.timestamp_text,
        entry.model,
        entry.input_tokens,
        entry.output_tokens,
        entry.cache_read_tokens,
        entry.cache_create_tokens,
        entry.cost_usd
    )
}

/// Convert a parsed entry into a `LoadedEntry`, wiring Junie's recorded cost
/// through the shared cost-mode machinery (copilot pattern): `auto` keeps the
/// recorded cost as `stored_cost`, `calculate` recomputes from pricing,
/// `display` shows the recorded cost.
pub(super) fn junie_entry_to_loaded(
    entry: JunieUsageEntry,
    project: &str,
    tz: Option<&JiffTimeZone>,
    mode: CostMode,
    pricing: &PricingMap,
) -> LoadedEntry {
    let usage = TokenUsageRaw {
        input_tokens: entry.input_tokens,
        output_tokens: entry.output_tokens,
        cache_creation_input_tokens: entry.cache_create_tokens,
        cache_read_input_tokens: entry.cache_read_tokens,
        speed: None,
        cache_creation: None,
    };
    let cost = calculate_cost_for_usage_at(
        Some(&entry.model),
        usage,
        entry.cost_usd,
        Some(entry.timestamp),
        mode,
        Some(pricing),
    );
    let missing_pricing_model = missing_pricing_model_for_usage(
        Some(&entry.model),
        usage,
        entry.cost_usd,
        mode,
        Some(pricing),
    );
    let stored_cost = stored_cost_for_mode(entry.cost_usd, mode);
    let data = UsageEntry {
        session_id: Some(entry.session_id.clone()),
        timestamp: entry.timestamp_text,
        version: None,
        message: UsageMessage {
            usage,
            model: Some(entry.model.clone()),
            id: None,
        },
        cost_usd: entry.cost_usd,
        request_id: None,
        is_api_error_message: None,
        is_sidechain: None,
    };
    LoadedEntry {
        date: format_date_tz(entry.timestamp, tz),
        timestamp: entry.timestamp,
        // Junie's projects are real paths, so unlike kimi/grok the project
        // field carries the session's projectDir, not a static agent name.
        project: Arc::from(project),
        session_id: Arc::from(entry.session_id),
        project_path: Arc::from(project),
        cost,
        extra_total_tokens: 0,
        credits: None,
        message_count: None,
        model: Some(entry.model),
        data,
        usage_limit_reset_time: None,
        missing_pricing_model,
        stored_cost,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PricingMap, TimestampMs, cli::CostMode, format_date_tz, format_rfc3339_millis};
    use ccusage_test_support::fs_fixture;
    use jiff::tz::TimeZone as JiffTimeZone;
    use std::collections::BTreeMap;

    fn utc() -> JiffTimeZone {
        JiffTimeZone::UTC
    }

    #[test]
    fn parses_current_format_event() {
        let fixture = fs_fixture!({
            "sessions/session-260731-115705-er3h/events.jsonl": [
                r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"LlmResponseMetadataEvent","agent":{"kind":"MainAgent","id":"main","name":"main","type":"LINEAR"},"modelUsage":[{"model":"gpt-4.1-2025-04-14","cost":0.00225,"inputTokens":1121,"cacheInputTokens":0,"cacheCreateTokens":0,"outputTokens":1,"time":0}]}},"taskId":"task-260731-115732-1owj","timestampMs":1785470253773}"#,
            ]
            .join("\n"),
        });
        let entries = read_events_file(
            &fixture.path("sessions/session-260731-115705-er3h/events.jsonl"),
            &BTreeMap::new(),
        );

        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.session_id, "session-260731-115705-er3h");
        assert_eq!(entry.timestamp.as_millis(), 1785470253773);
        assert_eq!(format_date_tz(entry.timestamp, Some(&utc())), "2026-07-31");
        assert_eq!(entry.model, "gpt-4.1-2025-04-14");
        assert_eq!(entry.input_tokens, 1121);
        assert_eq!(entry.output_tokens, 1);
        assert_eq!(entry.cache_read_tokens, 0);
        assert_eq!(entry.cache_create_tokens, 0);
        assert_eq!(entry.cost_usd, Some(0.00225));
    }

    #[test]
    fn expands_multi_model_usage_arrays_into_separate_entries() {
        let fixture = fs_fixture!({
            "sessions/session-260731-115705-er3h/events.jsonl": [
                r#"{"kind":"SessionA2uxEvent","event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"gpt-4.1-2025-04-14","cost":0.00225,"inputTokens":1121,"outputTokens":1},{"model":"Qwen/Qwen3.6-27B-FP8","cost":0.0004,"inputTokens":800,"cacheInputTokens":100,"cacheCreateTokens":50,"outputTokens":42}]}},"timestampMs":1785470253773}"#,
            ]
            .join("\n"),
        });
        let entries = read_events_file(
            &fixture.path("sessions/session-260731-115705-er3h/events.jsonl"),
            &BTreeMap::new(),
        );

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].model, "gpt-4.1-2025-04-14");
        assert_eq!(entries[1].model, "Qwen/Qwen3.6-27B-FP8");
        assert_eq!(entries[1].input_tokens, 800);
        assert_eq!(entries[1].cache_read_tokens, 100);
        assert_eq!(entries[1].cache_create_tokens, 50);
        assert_eq!(entries[1].output_tokens, 42);
        assert_eq!(entries[1].cost_usd, Some(0.0004));
    }

    #[test]
    fn legacy_event_buckets_under_index_created_at() {
        let fixture = fs_fixture!({
            "sessions/index.jsonl": [
                r#"{"sessionId":"session-260520-093639-1r02","createdAt":1748000000000,"projectDir":"/tmp/proj-a"}"#,
            ]
            .join("\n"),
            "sessions/session-260520-093639-1r02/events.jsonl": [
                r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"gpt-4.1-2025-04-14","cost":0.00225,"inputTokens":1121,"outputTokens":1}]}}}"#,
            ]
            .join("\n"),
        });
        let index = read_session_index(&fixture.path("sessions/index.jsonl"));
        let entries = read_events_file(
            &fixture.path("sessions/session-260520-093639-1r02/events.jsonl"),
            &index,
        );

        assert_eq!(entries.len(), 1);
        // The index createdAt wins over the session dir name (which would be 2026-05-20).
        assert_eq!(entries[0].timestamp.as_millis(), 1748000000000);
        assert_eq!(
            index
                .get("session-260520-093639-1r02")
                .unwrap()
                .project_dir
                .as_deref(),
            Some("/tmp/proj-a")
        );
    }

    #[test]
    fn legacy_event_without_index_entry_falls_back_to_dir_name_timestamp() {
        let fixture = fs_fixture!({
            "sessions/session-260520-093639-1r02/events.jsonl": [
                r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"gpt-4.1-2025-04-14","cost":0.00225,"inputTokens":1121,"outputTokens":1}]}}}"#,
            ]
            .join("\n"),
        });
        let entries = read_events_file(
            &fixture.path("sessions/session-260520-093639-1r02/events.jsonl"),
            &BTreeMap::new(),
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(
            format_date_tz(entries[0].timestamp, Some(&utc())),
            "2026-05-20"
        );
    }

    #[test]
    fn skips_malformed_lines_and_zero_usage_entries() {
        let fixture = fs_fixture!({
            "sessions/session-260731-115705-er3h/events.jsonl": [
                "not json",
                r#"{"kind":"SessionA2uxEvent","event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"gpt-4.1-2025-04-14","inputTokens":0,"outputTokens":0,"cacheInputTokens":0,"cacheCreateTokens":0,"cost":0}]}},"timestampMs":1785470253773}"#,
                r#"{"kind":"SessionA2uxEvent","event":{"agentEvent":{"kind":"LlmResponseMetadataEvent""#,
                r#"{"kind":"SessionA2uxEvent","event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"gpt-4.1-2025-04-14","cost":0.00225,"inputTokens":1121,"outputTokens":1}]}},"timestampMs":1785470260000}"#,
            ]
            .join("\n"),
        });
        let entries = read_events_file(
            &fixture.path("sessions/session-260731-115705-er3h/events.jsonl"),
            &BTreeMap::new(),
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].input_tokens, 1121);
        assert_eq!(entries[0].timestamp.as_millis(), 1785470260000);
    }

    #[test]
    fn auto_mode_keeps_recorded_cost_as_stored_cost() {
        let pricing = PricingMap::load_embedded();
        let entry = JunieUsageEntry {
            timestamp: TimestampMs::from_millis(1785470253773),
            timestamp_text: format_rfc3339_millis(TimestampMs::from_millis(1785470253773)),
            session_id: "session-260731-115705-er3h".to_string(),
            model: "gpt-4.1-2025-04-14".to_string(),
            input_tokens: 1121,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_create_tokens: 0,
            cost_usd: Some(0.00225),
        };

        let auto =
            junie_entry_to_loaded(entry.clone(), "/tmp/proj-a", None, CostMode::Auto, &pricing);
        assert_eq!(auto.stored_cost, Some(0.00225));
        assert!((auto.cost - 0.00225).abs() < 1e-12);
        assert_eq!(auto.project.as_ref(), "/tmp/proj-a");
        assert_eq!(auto.project_path.as_ref(), "/tmp/proj-a");

        let calculate = junie_entry_to_loaded(
            entry.clone(),
            "/tmp/proj-a",
            None,
            CostMode::Calculate,
            &pricing,
        );
        assert_eq!(calculate.stored_cost, None);
        assert!(
            calculate.cost > 0.0,
            "calculate must recompute from embedded pricing"
        );
        assert_eq!(calculate.missing_pricing_model, None);

        let display =
            junie_entry_to_loaded(entry, "/tmp/proj-a", None, CostMode::Display, &pricing);
        assert_eq!(display.stored_cost, None);
        assert!((display.cost - 0.00225).abs() < 1e-12);
    }
}
