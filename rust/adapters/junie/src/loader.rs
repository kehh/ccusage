use std::collections::{BTreeMap, HashSet};

use super::{
    parser::{junie_entry_key, junie_entry_to_loaded, read_events_file, read_session_index},
    paths::{discover_events_files, session_index_path},
};
use crate::{
    LoadedEntry, PricingMap, Result, cli::SharedArgs, debug_log, parse_tz, read_files_parallel,
};

pub fn load_entries(shared: &SharedArgs, pricing: &PricingMap) -> Result<Vec<LoadedEntry>> {
    crate::progress::track_usage_load(
        crate::progress::UsageLoadAgent("Junie"),
        shared.json,
        || load_entries_inner(shared, pricing),
    )
}

fn load_entries_inner(shared: &SharedArgs, pricing: &PricingMap) -> Result<Vec<LoadedEntry>> {
    let tz = parse_tz(shared.timezone.as_deref());
    let files = discover_events_files()?;
    // The index is shared by every session file, so read it once. A failed
    // index read still lets events load, but every project degrades to
    // "unknown" and legacy timestamps lose their createdAt, so log it.
    let index = session_index_path()
        .map(|path| {
            read_session_index(&path).unwrap_or_else(|error| {
                debug_log(
                    shared,
                    format!(
                        "Failed to read Junie session index {}: {error}",
                        path.display()
                    ),
                );
                BTreeMap::new()
            })
        })
        .unwrap_or_default();
    // Read session files in parallel, then apply the first-wins dedupe
    // sequentially over the original discovery order so the surviving entry
    // per key matches the single-threaded read. An unreadable file yields no
    // entries and logs why; the parser already skips malformed lines.
    let loaded = read_files_parallel(&files, shared.single_thread, |file| {
        read_events_file(file, &index).unwrap_or_else(|error| {
            debug_log(
                shared,
                format!(
                    "Failed to read Junie events file {}: {error}",
                    file.display()
                ),
            );
            Vec::new()
        })
    });
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for file_entries in loaded {
        for entry in file_entries {
            let key = junie_entry_key(&entry);
            if seen.insert(key) {
                let project = index
                    .get(entry.session_id.as_str())
                    .and_then(|meta| meta.project_dir.as_deref())
                    .unwrap_or("unknown");
                entries.push(junie_entry_to_loaded(
                    entry,
                    project,
                    tz.as_ref(),
                    shared.mode,
                    pricing,
                ));
            }
        }
    }
    entries.sort_by_key(|entry| entry.timestamp);
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::super::paths::JUNIE_HOME_ENV;
    use super::*;
    use ccusage_test_support::{EnvVarGuard, fs_fixture};

    #[test]
    fn loads_entries_across_sessions_with_index_project_paths() {
        let fixture = fs_fixture!({
            "sessions/index.jsonl": [
                r#"{"sessionId":"session-260731-115705-er3h","createdAt":1785470000000,"projectDir":"/tmp/proj-a"}"#,
                r#"{"sessionId":"session-260731-120000-abcd","createdAt":1785470100000,"projectDir":"/tmp/proj-b"}"#,
            ]
            .join("\n"),
            "sessions/session-260731-115705-er3h/events.jsonl": [
                r#"{"kind":"SessionA2uxEvent","event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"gpt-4.1-2025-04-14","cost":0.00225,"inputTokens":1121,"outputTokens":1}]}},"timestampMs":1785470253773}"#,
            ]
            .join("\n"),
            "sessions/session-260731-120000-abcd/events.jsonl": [
                r#"{"kind":"SessionA2uxEvent","event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"gpt-4.1-2025-04-14","cost":0.00225,"inputTokens":1121,"outputTokens":1}]}},"timestampMs":1785470353773}"#,
            ]
            .join("\n"),
        });
        let _cleanup = EnvVarGuard::set(JUNIE_HOME_ENV, fixture.root());
        let shared = SharedArgs {
            timezone: Some("UTC".to_string()),
            ..SharedArgs::default()
        };
        let entries = load_entries(&shared, &PricingMap::load_embedded()).unwrap();

        assert_eq!(entries.len(), 2);
        // Sorted by timestamp: the earlier event first.
        assert_eq!(entries[0].session_id.as_ref(), "session-260731-115705-er3h");
        assert_eq!(entries[1].session_id.as_ref(), "session-260731-120000-abcd");
        assert_eq!(entries[0].project.as_ref(), "/tmp/proj-a");
        assert_eq!(entries[0].project_path.as_ref(), "/tmp/proj-a");
        assert_eq!(entries[1].project.as_ref(), "/tmp/proj-b");
        assert_eq!(entries[1].project_path.as_ref(), "/tmp/proj-b");
        assert_eq!(entries[0].date, "2026-07-31");
    }

    #[test]
    fn loads_session_missing_from_index_with_fallback_project() {
        let fixture = fs_fixture!({
            "sessions/session-260731-115705-er3h/events.jsonl": [
                r#"{"kind":"SessionA2uxEvent","event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"gpt-4.1-2025-04-14","cost":0.00225,"inputTokens":1121,"outputTokens":1}]}},"timestampMs":1785470253773}"#,
            ]
            .join("\n"),
        });
        let _cleanup = EnvVarGuard::set(JUNIE_HOME_ENV, fixture.root());
        let shared = SharedArgs {
            timezone: Some("UTC".to_string()),
            ..SharedArgs::default()
        };
        let entries = load_entries(&shared, &PricingMap::load_embedded()).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].project.as_ref(), "unknown");
        assert_eq!(entries[0].project_path.as_ref(), "unknown");
        assert_eq!(entries[0].date, "2026-07-31");
    }

    #[test]
    fn dedupes_identical_entries_across_the_corpus() {
        let line = r#"{"kind":"SessionA2uxEvent","event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"gpt-4.1-2025-04-14","cost":0.00225,"inputTokens":1121,"outputTokens":1}]}},"timestampMs":1785470253773}"#;
        let fixture = fs_fixture!({
            "sessions/session-260731-115705-er3h/events.jsonl": [line, line].join("\n"),
            "sessions/session-260731-120000-abcd/events.jsonl": [line].join("\n"),
        });
        let _cleanup = EnvVarGuard::set(JUNIE_HOME_ENV, fixture.root());
        let entries = load_entries(&SharedArgs::default(), &PricingMap::load_embedded()).unwrap();

        // Identical usage repeated within one session collapses to one
        // record; the same usage under a different session id is distinct.
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.session_id.as_ref() == "session-260731-115705-er3h")
                .count(),
            1
        );
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.session_id.as_ref() == "session-260731-120000-abcd")
                .count(),
            1
        );
    }
}
