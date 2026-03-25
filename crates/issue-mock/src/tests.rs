use polyphony_core::{IssueTracker, TrackerQuery};

use crate::{MockAgentRuntime, MockTracker};

#[tokio::test]
async fn seeded_demo_returns_three_issues() {
    let tracker = MockTracker::seeded_demo();
    let query = TrackerQuery {
        project_slug: None,
        repository: None,
        active_states: vec!["Todo".into(), "In Progress".into()],
        terminal_states: Vec::new(),
    };
    let issues = tracker.fetch_candidate_issues(&query).await.unwrap();
    assert_eq!(issues.len(), 3);
}

#[tokio::test]
async fn set_state_updates_issue() {
    let tracker = MockTracker::seeded_demo();
    tracker.set_state("FAC-101", "Done").await;
    let issues = tracker
        .fetch_issues_by_ids(&["FAC-101".into()])
        .await
        .unwrap();
    assert_eq!(issues[0].state, "Done");
}

#[tokio::test]
async fn fetch_issues_by_ids_returns_only_matching() {
    let tracker = MockTracker::seeded_demo();
    let issues = tracker
        .fetch_issues_by_ids(&["FAC-101".into(), "NONEXISTENT".into()])
        .await
        .unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].identifier, "FAC-101");
}

#[tokio::test]
async fn fetch_issues_by_states_filters_correctly() {
    let tracker = MockTracker::seeded_demo();
    let issues = tracker
        .fetch_issues_by_states(None, &["In Progress".into()])
        .await
        .unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].identifier, "FAC-102");
}

#[tokio::test]
async fn fetch_issue_states_by_ids_returns_state_updates() {
    let tracker = MockTracker::seeded_demo();
    let states = tracker
        .fetch_issue_states_by_ids(&["FAC-103".into()])
        .await
        .unwrap();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].state, "Todo");
}

#[tokio::test]
async fn mock_runtime_runs_turns_and_transitions_state() {
    use polyphony_core::{AgentDefinition, AgentRunSpec, AttemptStatus, Issue};

    let tracker = MockTracker::seeded_demo();
    let runtime = MockAgentRuntime::new(tracker.clone());
    let spec = AgentRunSpec {
        issue: Issue {
            id: "FAC-101".into(),
            identifier: "FAC-101".into(),
            title: "Build workflow loader".into(),
            state: "Todo".into(),
            ..Issue::default()
        },
        attempt: Some(0),
        workspace_path: std::env::temp_dir(),
        prompt: "implement this".into(),
        max_turns: 2,
        agent: AgentDefinition {
            name: "mock".into(),
            ..AgentDefinition::default()
        },
        prior_context: None,
    };
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let result = polyphony_core::AgentRuntime::run(&runtime, spec, event_tx)
        .await
        .unwrap();

    assert_eq!(result.status, AttemptStatus::Succeeded);
    assert_eq!(result.turns_completed, 2);
    assert_eq!(result.final_issue_state.as_deref(), Some("Human Review"));

    // Verify events were emitted
    let mut count = 0;
    while event_rx.try_recv().is_ok() {
        count += 1;
    }
    assert!(count > 0, "expected agent events to be emitted");

    // Verify tracker state was updated
    let issues = tracker
        .fetch_issues_by_ids(&["FAC-101".into()])
        .await
        .unwrap();
    assert_eq!(issues[0].state, "Human Review");
}

#[tokio::test]
async fn fetch_budget_returns_snapshot() {
    let tracker = MockTracker::seeded_demo();
    let budget = tracker.fetch_budget().await.unwrap();
    assert!(budget.is_some());
    assert_eq!(budget.unwrap().component, "tracker:mock");
}
