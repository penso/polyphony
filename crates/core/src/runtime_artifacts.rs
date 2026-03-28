use std::path::{Path, PathBuf};

use crate::{AgentContextSnapshot, Error, PersistedAgentRunRecord};

const WORKSPACE_RUNTIME_DIR: &str = "runtime";
const WORKSPACE_SAVED_CONTEXT_FILE: &str = "saved-context.json";
const WORKSPACE_AGENT_RUN_HISTORY_FILE: &str = "agent-run-history.jsonl";
const WORKSPACE_AGENT_EVENTS_FILE: &str = "agent-events.jsonl";

pub fn workspace_runtime_artifact_dir(workspace_path: &Path) -> PathBuf {
    workspace_path
        .join(".polyphony")
        .join(WORKSPACE_RUNTIME_DIR)
}

pub fn workspace_saved_context_artifact_path(workspace_path: &Path) -> PathBuf {
    workspace_runtime_artifact_dir(workspace_path).join(WORKSPACE_SAVED_CONTEXT_FILE)
}

pub fn workspace_agent_run_history_artifact_path(workspace_path: &Path) -> PathBuf {
    workspace_runtime_artifact_dir(workspace_path).join(WORKSPACE_AGENT_RUN_HISTORY_FILE)
}

pub fn workspace_agent_events_artifact_path(workspace_path: &Path) -> PathBuf {
    workspace_runtime_artifact_dir(workspace_path).join(WORKSPACE_AGENT_EVENTS_FILE)
}

/// Path to the asciicast recording for a given agent run.
pub fn workspace_cast_artifact_path(
    workspace_path: &Path,
    agent_name: &str,
    transport: &str,
) -> PathBuf {
    workspace_path
        .join(".polyphony")
        .join(format!("{agent_name}-{transport}.cast"))
}

pub fn load_workspace_saved_context_artifact(
    workspace_path: &Path,
) -> Result<Option<AgentContextSnapshot>, Error> {
    let path = workspace_saved_context_artifact_path(workspace_path);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::Adapter(error.to_string())),
    };
    let context = serde_json::from_str::<AgentContextSnapshot>(&raw)
        .map_err(|error| Error::Adapter(error.to_string()))?;
    Ok(Some(context))
}

pub fn load_workspace_agent_run_history_record(
    workspace_path: &Path,
    issue_id: &str,
    started_at: chrono::DateTime<chrono::Utc>,
    agent_name: &str,
    attempt: Option<u32>,
) -> Result<Option<PersistedAgentRunRecord>, Error> {
    let path = workspace_agent_run_history_artifact_path(workspace_path);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::Adapter(error.to_string())),
    };

    let mut matched = None;
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let record = serde_json::from_str::<PersistedAgentRunRecord>(line)
            .map_err(|error| Error::Adapter(error.to_string()))?;
        if record.issue_id == issue_id
            && record.started_at == started_at
            && record.agent_name == agent_name
            && record.attempt == attempt
        {
            matched = Some(record);
        }
    }

    Ok(matched)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{AttemptStatus, TokenUsage};

    #[test]
    fn workspace_paths_live_under_polyphony_runtime() {
        let workspace = PathBuf::from("/tmp/example");
        assert_eq!(
            workspace_saved_context_artifact_path(&workspace),
            PathBuf::from("/tmp/example/.polyphony/runtime/saved-context.json")
        );
        assert_eq!(
            workspace_agent_run_history_artifact_path(&workspace),
            PathBuf::from("/tmp/example/.polyphony/runtime/agent-run-history.jsonl")
        );
        assert_eq!(
            workspace_agent_events_artifact_path(&workspace),
            PathBuf::from("/tmp/example/.polyphony/runtime/agent-events.jsonl")
        );
    }

    #[test]
    fn load_workspace_agent_run_history_record_matches_latest_line() {
        let root = std::env::temp_dir().join(format!(
            "polyphony-core-artifacts-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(workspace_runtime_artifact_dir(&root)).unwrap();
        let now = chrono::Utc::now();
        let record = PersistedAgentRunRecord {
            repo_id: String::new(),
            issue_id: "issue-1".into(),
            issue_identifier: "DOG-1".into(),
            agent_name: "implementer".into(),
            model: None,
            session_id: None,
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            status: AttemptStatus::Succeeded,
            attempt: Some(1),
            max_turns: 3,
            turn_count: 1,
            last_event: None,
            last_message: None,
            started_at: now,
            finished_at: Some(now),
            last_event_at: Some(now),
            tokens: TokenUsage::default(),
            workspace_path: Some(root.clone()),
            error: None,
            saved_context: None,
        };
        let path = workspace_agent_run_history_artifact_path(&root);
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&record).unwrap(),
                serde_json::to_string(&record).unwrap()
            ),
        )
        .unwrap();

        let loaded =
            load_workspace_agent_run_history_record(&root, "issue-1", now, "implementer", Some(1))
                .unwrap()
                .unwrap();
        assert_eq!(loaded.issue_identifier, "DOG-1");
        std::fs::remove_dir_all(root).unwrap();
    }
}
