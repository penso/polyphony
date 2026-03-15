
use {
    crate::{
        bootstrap::{BootstrapChoice, BootstrapState},
        *,
    },
    chrono::Utc,
    polyphony_core::{
        CodexTotals, RuntimeCadence, RuntimeSnapshot, SnapshotCounts, TrackerConnectionStatus,
        VisibleIssueRow, VisibleTriggerKind, VisibleTriggerRow,
    },
    ratatui::{Terminal, backend::TestBackend, buffer::Buffer},
};

fn test_snapshot(visible: usize) -> RuntimeSnapshot {
    RuntimeSnapshot {
        generated_at: Utc::now(),
        counts: SnapshotCounts {
            running: 0,
            retrying: 0,
            ..Default::default()
        },
        cadence: RuntimeCadence::default(),
        visible_issues: (0..visible)
            .map(|i| VisibleIssueRow {
                issue_id: format!("id-{i}"),
                issue_identifier: format!("GH-{i}"),
                title: format!("Test issue {i}"),
                state: "open".into(),
                priority: Some(2),
                labels: vec![],
                description: None,
                url: None,
                author: None,
                parent_id: None,
                updated_at: None,
                created_at: None,
                has_workspace: false,
            })
            .collect(),
        visible_triggers: (0..visible)
            .map(|i| VisibleTriggerRow {
                trigger_id: format!("id-{i}"),
                kind: VisibleTriggerKind::Issue,
                source: "github".into(),
                identifier: format!("GH-{i}"),
                title: format!("Test issue {i}"),
                status: "open".into(),
                priority: Some(2),
                labels: vec![],
                description: None,
                url: None,
                author: None,
                parent_id: None,
                updated_at: None,
                created_at: None,
                has_workspace: false,
            })
            .collect(),
        running: vec![],
        retrying: vec![],
        codex_totals: CodexTotals::default(),
        rate_limits: None,
        throttles: vec![],
        budgets: vec![],
        agent_catalogs: vec![],
        saved_contexts: vec![],
        recent_events: vec![],
        movements: vec![],
        tasks: vec![],
        loading: Default::default(),
        dispatch_mode: Default::default(),
        tracker_kind: Default::default(),
        tracker_connection: None,
        from_cache: false,
        cached_at: None,
        agent_profile_names: vec![],
    }
}

fn buffer_text(buffer: &Buffer) -> String {
    let width = buffer.area.width as usize;
    buffer
        .content
        .chunks(width)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn app_state_selection_syncs() {
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    let snapshot = test_snapshot(5);
    app.on_snapshot(&snapshot);
    assert_eq!(app.issues_state.selected(), Some(0));
}

#[test]
fn app_state_empty_snapshot() {
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    let snapshot = test_snapshot(0);
    app.on_snapshot(&snapshot);
    assert_eq!(app.issues_state.selected(), None);
}

#[test]
fn render_does_not_panic() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let snapshot = test_snapshot(3);
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    app.on_snapshot(&snapshot);

    terminal
        .draw(|frame| {
            render::render(frame, &snapshot, &mut app);
        })
        .unwrap();
}

#[test]
fn render_shows_connected_github_login_in_header() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut snapshot = test_snapshot(3);
    snapshot.tracker_connection = Some(TrackerConnectionStatus::connected("penso"));
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    app.on_snapshot(&snapshot);

    terminal
        .draw(|frame| {
            render::render(frame, &snapshot, &mut app);
        })
        .unwrap();

    let screen = buffer_text(terminal.backend().buffer());
    assert!(screen.contains("penso"), "{screen}");
}

#[test]
fn tab_switching() {
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    assert_eq!(app.active_tab, app::ActiveTab::Triggers);

    app.active_tab = app.active_tab.next();
    assert_eq!(app.active_tab, app::ActiveTab::Orchestrator);

    app.active_tab = app.active_tab.next();
    assert_eq!(app.active_tab, app::ActiveTab::Tasks);

    app.active_tab = app.active_tab.next();
    assert_eq!(app.active_tab, app::ActiveTab::Deliverables);

    app.active_tab = app.active_tab.next();
    assert_eq!(app.active_tab, app::ActiveTab::Agents);

    app.active_tab = app.active_tab.next();
    assert_eq!(app.active_tab, app::ActiveTab::Logs);

    app.active_tab = app.active_tab.next();
    assert_eq!(app.active_tab, app::ActiveTab::Triggers);
}

#[test]
fn agent_detail_scroll_resets_when_agent_selection_changes() {
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    let mut snapshot = test_snapshot(0);
    snapshot.running = vec![
        polyphony_core::RunningRow {
            issue_id: "issue-1".into(),
            issue_identifier: "GH-1".into(),
            agent_name: "opus".into(),
            model: Some("claude".into()),
            state: "running".into(),
            max_turns: 20,
            session_id: Some("opus-gh-1-0".into()),
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            turn_count: 1,
            last_event: Some("TurnStarted".into()),
            last_message: Some("hello".into()),
            started_at: Utc::now(),
            last_event_at: None,
            tokens: Default::default(),
            workspace_path: std::path::PathBuf::from("."),
            attempt: Some(0),
        },
        polyphony_core::RunningRow {
            issue_id: "issue-2".into(),
            issue_identifier: "GH-2".into(),
            agent_name: "codex".into(),
            model: Some("gpt-5".into()),
            state: "running".into(),
            max_turns: 20,
            session_id: Some("codex-gh-2-0".into()),
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            turn_count: 2,
            last_event: Some("TurnStarted".into()),
            last_message: Some("world".into()),
            started_at: Utc::now(),
            last_event_at: None,
            tokens: Default::default(),
            workspace_path: std::path::PathBuf::from("."),
            attempt: Some(0),
        },
    ];
    snapshot.counts.running = snapshot.running.len();

    app.on_snapshot(&snapshot);
    app.active_tab = app::ActiveTab::Agents;
    app.agents_detail_scroll = 5;

    app.move_down(snapshot.running.len(), 1);

    assert_eq!(app.agents_state.selected(), Some(1));
    assert_eq!(app.agents_detail_scroll, 0);
}

#[test]
fn bootstrap_state_defaults_to_create() {
    let state = BootstrapState::default();
    assert_eq!(state.choice, BootstrapChoice::Create);
}

#[test]
fn log_buffer_push_and_read() {
    let buf = LogBuffer::with_capacity(3);
    buf.push_line("one");
    buf.push_line("two");
    buf.push_line("three");
    buf.push_line("four");
    assert_eq!(buf.all_lines(), vec!["two", "three", "four"]);
}
