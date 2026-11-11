use std::{env, path::PathBuf};

use crate::Result;

/// Official Junie home override (single root; Junie defines one home).
pub(super) const JUNIE_HOME_ENV: &str = "JUNIE_HOME";
const JUNIE_SESSIONS_DIR_NAME: &str = "sessions";
const JUNIE_EVENTS_FILE_NAME: &str = "events.jsonl";
const JUNIE_INDEX_FILE_NAME: &str = "index.jsonl";

/// Resolve the Junie data root from `JUNIE_HOME`, then `~/.junie`.
pub(super) fn junie_home() -> Option<PathBuf> {
    if let Ok(home) = env::var(JUNIE_HOME_ENV)
        && !home.trim().is_empty()
    {
        return Some(PathBuf::from(home.trim()));
    }

    let home = crate::home::home_dir()?;
    Some(home.join(".junie"))
}

/// Discover every `sessions/<session>/events.jsonl` under the resolved root.
///
/// Only direct child directories of `sessions/` are session dirs; nested
/// `events.jsonl` files (a session recording a task that itself ran Junie)
/// belong to that inner session, not this root.
pub(super) fn discover_events_files() -> Result<Vec<PathBuf>> {
    let Some(home) = junie_home() else {
        return Ok(Vec::new());
    };
    let mut files = Vec::new();
    let sessions = home.join(JUNIE_SESSIONS_DIR_NAME);
    if let Ok(entries) = std::fs::read_dir(&sessions) {
        for entry in entries.flatten() {
            let session_dir = entry.path();
            if !session_dir.is_dir() {
                continue;
            }
            let events = session_dir.join(JUNIE_EVENTS_FILE_NAME);
            if events.is_file() {
                files.push(events);
            }
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

/// Path of the session index, `sessions/index.jsonl`, if a root resolves.
pub(super) fn session_index_path() -> Option<PathBuf> {
    junie_home().map(|home| {
        home.join(JUNIE_SESSIONS_DIR_NAME)
            .join(JUNIE_INDEX_FILE_NAME)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccusage_test_support::{EnvVarGuard, fs_fixture};
    use std::path::PathBuf;

    #[test]
    fn discovers_events_jsonl_in_direct_session_dirs_only() {
        let fixture = fs_fixture!({
            "sessions/session-a/events.jsonl": "{}\n",
            "sessions/session-b/task-1/events.jsonl": "{}\n",
            "sessions/session-c/other.jsonl": "{}\n",
        });
        let _cleanup = EnvVarGuard::set(JUNIE_HOME_ENV, fixture.root());
        let files = discover_events_files().unwrap();

        assert_eq!(files, vec![fixture.path("sessions/session-a/events.jsonl")]);
    }

    #[test]
    fn junie_home_env_overrides_default_root() {
        let fixture = fs_fixture!({
            "sessions/session-a/events.jsonl": "{}\n",
        });
        let _cleanup = EnvVarGuard::set(JUNIE_HOME_ENV, fixture.root());

        assert_eq!(junie_home(), Some(fixture.root().to_path_buf()));
        assert_eq!(
            discover_events_files().unwrap(),
            vec![fixture.path("sessions/session-a/events.jsonl")]
        );
        assert_eq!(
            session_index_path(),
            Some(fixture.path("sessions/index.jsonl"))
        );
    }

    #[test]
    fn missing_root_yields_no_files() {
        let fixture = fs_fixture!({});
        let _cleanup = EnvVarGuard::set(JUNIE_HOME_ENV, fixture.root());

        assert_eq!(discover_events_files().unwrap(), Vec::<PathBuf>::new());
        assert_eq!(
            session_index_path(),
            Some(fixture.path("sessions/index.jsonl"))
        );
    }
}
