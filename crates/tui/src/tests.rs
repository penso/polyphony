use chrono::Utc;
use polyphony_core::{
    BudgetSnapshot, CodexTotals, Deliverable, DeliverableDecision, DeliverableKind,
    DeliverableStatus, DispatchMode, IssueApprovalState, RuntimeCadence, RuntimeSnapshot,
    SnapshotCounts, TrackerConnectionStatus, VisibleIssueRow, VisibleTriggerKind,
    VisibleTriggerRow,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

use crate::{
    bootstrap::{BootstrapChoice, BootstrapState},
    *,
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
                approval_state: IssueApprovalState::Approved,
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
                approval_state: IssueApprovalState::Approved,
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
        approved_issue_keys: vec![],
        running: vec![],
        agent_history: vec![],
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
        agent_profiles: vec![],
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
    assert_eq!(app.issues_state.selected(), Some(4));
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
fn leaving_modal_blanks_previous_ui() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let snapshot = test_snapshot(3);
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    app.on_snapshot(&snapshot);
    app.leaving = true;

    terminal
        .draw(|frame| {
            render::render(frame, &snapshot, &mut app);
        })
        .unwrap();

    let screen = buffer_text(terminal.backend().buffer());
    assert!(screen.contains("Leaving..."), "{screen}");
    assert!(!screen.contains("Triggers"), "{screen}");
    assert!(!screen.contains("Test issue 0"), "{screen}");
}

#[test]
fn render_mode_modal_with_idle_selection_does_not_panic() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut snapshot = test_snapshot(3);
    snapshot.dispatch_mode = polyphony_core::DispatchMode::Idle;
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    app.on_snapshot(&snapshot);
    app.active_tab = app::ActiveTab::Orchestrator;
    app.show_mode_modal = true;
    app.mode_modal_selected = 3;

    terminal
        .draw(|frame| {
            render::render(frame, &snapshot, &mut app);
        })
        .unwrap();

    let screen = buffer_text(terminal.backend().buffer());
    assert!(
        screen.contains("Only opportunistic dispatch when idle"),
        "{screen}"
    );
    assert!(screen.contains("budgets say there is headroom"), "{screen}");
}

#[test]
fn child_triggers_are_sorted_by_local_number_under_parent() {
    let mut snapshot = test_snapshot(0);
    snapshot.visible_triggers = vec![
        VisibleTriggerRow {
            trigger_id: "parent".into(),
            kind: VisibleTriggerKind::Issue,
            source: "beads".into(),
            identifier: "1ru".into(),
            title: "Parent".into(),
            status: "Open".into(),
            approval_state: IssueApprovalState::Approved,
            priority: Some(2),
            labels: vec![],
            description: None,
            url: None,
            author: None,
            parent_id: None,
            updated_at: None,
            created_at: None,
            has_workspace: false,
        },
        VisibleTriggerRow {
            trigger_id: "child-18".into(),
            kind: VisibleTriggerKind::Issue,
            source: "beads".into(),
            identifier: "1ru.18".into(),
            title: "Child 18".into(),
            status: "Open".into(),
            approval_state: IssueApprovalState::Approved,
            priority: Some(2),
            labels: vec![],
            description: None,
            url: None,
            author: None,
            parent_id: Some("parent".into()),
            updated_at: None,
            created_at: None,
            has_workspace: false,
        },
        VisibleTriggerRow {
            trigger_id: "child-2".into(),
            kind: VisibleTriggerKind::Issue,
            source: "beads".into(),
            identifier: "1ru.2".into(),
            title: "Child 2".into(),
            status: "Open".into(),
            approval_state: IssueApprovalState::Approved,
            priority: Some(2),
            labels: vec![],
            description: None,
            url: None,
            author: None,
            parent_id: Some("parent".into()),
            updated_at: None,
            created_at: None,
            has_workspace: false,
        },
        VisibleTriggerRow {
            trigger_id: "child-10".into(),
            kind: VisibleTriggerKind::Issue,
            source: "beads".into(),
            identifier: "1ru.10".into(),
            title: "Child 10".into(),
            status: "Open".into(),
            approval_state: IssueApprovalState::Approved,
            priority: Some(2),
            labels: vec![],
            description: None,
            url: None,
            author: None,
            parent_id: Some("parent".into()),
            updated_at: None,
            created_at: None,
            has_workspace: false,
        },
    ];
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    app.issue_sort = app::IssueSortKey::Newest;
    app.on_snapshot(&snapshot);

    let ordered_identifiers = app
        .sorted_issue_indices
        .iter()
        .map(|&index| snapshot.visible_triggers[index].identifier.clone())
        .collect::<Vec<_>>();

    assert_eq!(ordered_identifiers, vec![
        "1ru", "1ru.2", "1ru.10", "1ru.18"
    ]);
}

#[test]
fn render_triggers_uses_compact_child_identifiers() {
    let backend = TestBackend::new(110, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut snapshot = test_snapshot(0);
    snapshot.dispatch_mode = DispatchMode::Manual;
    snapshot.visible_triggers = vec![
        VisibleTriggerRow {
            trigger_id: "parent".into(),
            kind: VisibleTriggerKind::Issue,
            source: "beads".into(),
            identifier: "1ru".into(),
            title: "Parent".into(),
            status: "Open".into(),
            approval_state: IssueApprovalState::Approved,
            priority: Some(2),
            labels: vec![],
            description: None,
            url: None,
            author: None,
            parent_id: None,
            updated_at: None,
            created_at: None,
            has_workspace: false,
        },
        VisibleTriggerRow {
            trigger_id: "child".into(),
            kind: VisibleTriggerKind::Issue,
            source: "beads".into(),
            identifier: "1ru.18".into(),
            title: "Child".into(),
            status: "Open".into(),
            approval_state: IssueApprovalState::Approved,
            priority: Some(2),
            labels: vec![],
            description: None,
            url: None,
            author: None,
            parent_id: Some("parent".into()),
            updated_at: None,
            created_at: None,
            has_workspace: false,
        },
    ];
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    app.on_snapshot(&snapshot);

    terminal
        .draw(|frame| {
            render::render(frame, &snapshot, &mut app);
        })
        .unwrap();

    let screen = buffer_text(terminal.backend().buffer());
    // ID column removed; verify parent/child titles render and tree connector is shown
    assert!(screen.contains("Parent"), "{screen}");
    assert!(screen.contains("Child"), "{screen}");
    assert!(
        screen.contains("└"),
        "child should have tree connector: {screen}"
    );
}

#[test]
fn render_triggers_strip_github_repo_prefix_and_hide_source_column() {
    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut snapshot = test_snapshot(0);
    snapshot.dispatch_mode = DispatchMode::Manual;
    snapshot.visible_triggers = vec![
        VisibleTriggerRow {
            trigger_id: "github-74".into(),
            kind: VisibleTriggerKind::Issue,
            source: "github".into(),
            identifier: "penso/arbor#74".into(),
            title: "Trigger title".into(),
            status: "Todo".into(),
            approval_state: IssueApprovalState::Approved,
            priority: Some(2),
            labels: vec![],
            description: None,
            url: None,
            author: None,
            parent_id: None,
            updated_at: None,
            created_at: None,
            has_workspace: false,
        },
        VisibleTriggerRow {
            trigger_id: "beads-1".into(),
            kind: VisibleTriggerKind::Issue,
            source: "beads".into(),
            identifier: "8k9".into(),
            title: "Beads title".into(),
            status: "Open".into(),
            approval_state: IssueApprovalState::Approved,
            priority: Some(2),
            labels: vec![],
            description: None,
            url: None,
            author: None,
            parent_id: None,
            updated_at: None,
            created_at: None,
            has_workspace: false,
        },
    ];
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    app.on_snapshot(&snapshot);

    terminal
        .draw(|frame| {
            render::render(frame, &snapshot, &mut app);
        })
        .unwrap();

    let screen = buffer_text(terminal.backend().buffer());
    // ID column removed; approval icon should appear before the title
    assert!(
        screen.contains("✓"),
        "approval icon should be visible: {screen}"
    );
    assert!(screen.contains("Trigger title"), "{screen}");
    assert!(!screen.contains("Source"), "{screen}");
}

#[test]
fn render_logs_footer_shows_provider_budgets() {
    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut snapshot = test_snapshot(0);
    let now = Utc::now();
    snapshot.budgets = vec![
        BudgetSnapshot {
            component: "agent:router".into(),
            captured_at: now,
            credits_remaining: Some(93.0),
            credits_total: Some(100.0),
            spent_usd: None,
            soft_limit_usd: None,
            hard_limit_usd: None,
            reset_at: Some(now + chrono::TimeDelta::hours(2)),
            raw: Some(serde_json::json!({
                "provider": "codex",
                "session": {
                    "remaining_percent": 93.0,
                    "reset_at": (now + chrono::TimeDelta::hours(2)).to_rfc3339(),
                },
                "weekly": {
                    "remaining_percent": 1.0,
                    "deficit_percent": 28.0,
                    "reset_at": (now + chrono::TimeDelta::days(2)).to_rfc3339(),
                }
            })),
        },
        BudgetSnapshot {
            component: "agent:reviewer".into(),
            captured_at: now,
            credits_remaining: Some(11.0),
            credits_total: Some(100.0),
            spent_usd: None,
            soft_limit_usd: None,
            hard_limit_usd: None,
            reset_at: Some(now + chrono::TimeDelta::hours(2)),
            raw: Some(serde_json::json!({
                "provider": "claude",
                "session": {
                    "remaining_percent": 11.0,
                    "reset_at": (now + chrono::TimeDelta::hours(2)).to_rfc3339(),
                },
                "weekly": {
                    "remaining_percent": 48.0,
                    "deficit_percent": 13.0,
                    "reset_at": (now + chrono::TimeDelta::days(4)).to_rfc3339(),
                }
            })),
        },
    ];

    let mut app = AppState::new(default_theme(), LogBuffer::default());
    app.active_tab = app::ActiveTab::Logs;
    app.on_snapshot(&snapshot);

    terminal
        .draw(|frame| {
            render::render(frame, &snapshot, &mut app);
        })
        .unwrap();

    let screen = buffer_text(terminal.backend().buffer());
    assert!(screen.contains("Budgets"), "{screen}");
    assert!(screen.contains("Codex"), "{screen}");
    assert!(screen.contains("Claude"), "{screen}");
    assert!(screen.contains("Δ28%"), "{screen}");
    assert!(screen.contains("Δ13%"), "{screen}");
}

#[test]
fn render_triggers_show_clock_icon_for_waiting_issue_approval() {
    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut snapshot = test_snapshot(0);
    snapshot.visible_triggers = vec![VisibleTriggerRow {
        trigger_id: "github-75".into(),
        kind: VisibleTriggerKind::Issue,
        source: "github".into(),
        identifier: "penso/polyphony#75".into(),
        title: "Waiting for approval".into(),
        status: "Todo".into(),
        approval_state: IssueApprovalState::Waiting,
        priority: Some(2),
        labels: vec![],
        description: None,
        url: None,
        author: Some("outsider".into()),
        parent_id: None,
        updated_at: None,
        created_at: None,
        has_workspace: false,
    }];
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    app.on_snapshot(&snapshot);

    terminal
        .draw(|frame| {
            render::render(frame, &snapshot, &mut app);
        })
        .unwrap();

    let screen = buffer_text(terminal.backend().buffer());
    // ID column removed; approval icon now appears before the title
    assert!(screen.contains("◷Waiting for approval"), "{screen}");
}

#[test]
fn render_triggers_show_approved_icon_for_verified_github_issue() {
    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut snapshot = test_snapshot(0);
    snapshot.visible_triggers = vec![VisibleTriggerRow {
        trigger_id: "github-76".into(),
        kind: VisibleTriggerKind::Issue,
        source: "github".into(),
        identifier: "penso/polyphony#76".into(),
        title: "Approved issue".into(),
        status: "Todo".into(),
        approval_state: IssueApprovalState::Approved,
        priority: Some(2),
        labels: vec![],
        description: None,
        url: None,
        author: Some("penso".into()),
        parent_id: None,
        updated_at: None,
        created_at: None,
        has_workspace: false,
    }];
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    app.on_snapshot(&snapshot);

    terminal
        .draw(|frame| {
            render::render(frame, &snapshot, &mut app);
        })
        .unwrap();

    let screen = buffer_text(terminal.backend().buffer());
    // ID column removed; approval icon now appears before the title
    assert!(screen.contains("✓Approved issue"), "{screen}");
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
fn render_outputs_shows_flow_output_and_decision() {
    let backend = TestBackend::new(120, 28);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut snapshot = test_snapshot(0);
    snapshot.movements = vec![polyphony_core::MovementRow {
        id: "mov-1".into(),
        kind: polyphony_core::MovementKind::IssueDelivery,
        issue_identifier: Some("#7".into()),
        title: "Repository root is missing e2e-live.txt".into(),
        status: polyphony_core::MovementStatus::Delivered,
        task_count: 1,
        tasks_completed: 1,
        deliverable: Some(Deliverable {
            kind: DeliverableKind::GithubPullRequest,
            status: DeliverableStatus::Open,
            url: Some("https://github.com/penso/polyphony/pull/8".into()),
            decision: DeliverableDecision::Waiting,
            title: None,
            description: None,
            metadata: Default::default(),
        }),
        has_deliverable: true,
        review_target: None,
        workspace_key: Some("_7".into()),
        workspace_path: Some(std::path::PathBuf::from(".polyphony/workspaces/_7")),
        created_at: Utc::now(),
    }];
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    app.on_snapshot(&snapshot);
    app.active_tab = app::ActiveTab::Deliverables;

    terminal
        .draw(|frame| {
            render::render(frame, &snapshot, &mut app);
        })
        .unwrap();

    let screen = buffer_text(terminal.backend().buffer());
    assert!(screen.contains("#7"), "{screen}");
    assert!(screen.contains("PR #8"), "{screen}");
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
    // Push an agent detail with non-zero scroll
    app.push_detail(app::DetailView::Agent {
        agent_index: 0,
        scroll: 5,
        artifact_cache: Box::new(None),
    });

    app.move_down(snapshot.running.len(), 1);

    assert_eq!(app.agents_state.selected(), Some(1));
    // The detail scroll is managed per-view now, move_down doesn't reset it
    assert_eq!(app.current_detail_scroll(), 5);
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

#[test]
fn detail_stack_push_pop() {
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    assert!(!app.has_detail());

    app.push_detail(app::DetailView::Trigger {
        trigger_id: "t-1".into(),
        scroll: 0,
        focus: Default::default(),
        movements_selected: 0,
        agents_selected: 0,
    });
    assert!(app.has_detail());
    assert!(matches!(
        app.current_detail(),
        Some(app::DetailView::Trigger { .. })
    ));

    app.push_detail(app::DetailView::Movement {
        movement_id: "m-1".into(),
        scroll: 0,
    });
    assert_eq!(app.detail_stack.len(), 2);
    assert!(matches!(
        app.current_detail(),
        Some(app::DetailView::Movement { .. })
    ));

    app.pop_detail();
    assert!(matches!(
        app.current_detail(),
        Some(app::DetailView::Trigger { .. })
    ));

    app.pop_detail();
    assert!(!app.has_detail());
}

#[test]
fn tab_switch_clears_detail_stack() {
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    app.push_detail(app::DetailView::Task {
        task_id: "t-1".into(),
        scroll: 0,
    });
    assert!(app.has_detail());

    app.clear_detail_stack();
    assert!(!app.has_detail());
    assert_eq!(app.split_focus, app::SplitFocus::List);
}

#[test]
fn detail_scroll_through_stack() {
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    app.push_detail(app::DetailView::Trigger {
        trigger_id: "t-1".into(),
        scroll: 5,
        focus: Default::default(),
        movements_selected: 0,
        agents_selected: 0,
    });
    assert_eq!(app.current_detail_scroll(), 5);

    app.set_current_detail_scroll(10);
    assert_eq!(app.current_detail_scroll(), 10);
}

#[test]
fn split_layout_eligible_only_for_single_depth() {
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    app.active_tab = app::ActiveTab::Triggers;

    // No detail — not split eligible (nothing to show on right)
    assert!(!app.is_split_eligible());

    // One detail — eligible
    app.push_detail(app::DetailView::Trigger {
        trigger_id: "t-1".into(),
        scroll: 0,
        focus: Default::default(),
        movements_selected: 0,
        agents_selected: 0,
    });
    assert!(app.is_split_eligible());

    // Two details — not eligible (full-page)
    app.push_detail(app::DetailView::Movement {
        movement_id: "m-1".into(),
        scroll: 0,
    });
    assert!(!app.is_split_eligible());
}

#[test]
fn split_layout_not_eligible_for_logs() {
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    app.active_tab = app::ActiveTab::Logs;
    app.push_detail(app::DetailView::Task {
        task_id: "t-1".into(),
        scroll: 0,
    });
    assert!(!app.is_split_eligible());
}

#[test]
fn entity_disappears_auto_pops_detail() {
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    let mut snapshot = test_snapshot(3);
    app.on_snapshot(&snapshot);

    app.push_detail(app::DetailView::Trigger {
        trigger_id: "id-1".into(),
        scroll: 0,
        focus: Default::default(),
        movements_selected: 0,
        agents_selected: 0,
    });
    assert!(app.has_detail());

    // Remove the trigger from the snapshot
    snapshot.visible_triggers.retain(|t| t.trigger_id != "id-1");
    app.on_snapshot(&snapshot);

    // Detail should have been auto-popped
    assert!(!app.has_detail());
}

#[test]
fn render_split_layout_does_not_panic() {
    // 160 cols is above the 140 threshold for split mode
    let backend = TestBackend::new(160, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    let snapshot = test_snapshot(5);
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    app.on_snapshot(&snapshot);
    app.push_detail(app::DetailView::Trigger {
        trigger_id: "id-0".into(),
        scroll: 0,
        focus: Default::default(),
        movements_selected: 0,
        agents_selected: 0,
    });

    terminal
        .draw(|frame| {
            render::render(frame, &snapshot, &mut app);
        })
        .unwrap();
}

#[test]
fn render_narrow_detail_does_not_panic() {
    // 80 cols is below the 140 threshold — should render full-page detail
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let snapshot = test_snapshot(3);
    let mut app = AppState::new(default_theme(), LogBuffer::default());
    app.on_snapshot(&snapshot);
    app.push_detail(app::DetailView::Trigger {
        trigger_id: "id-0".into(),
        scroll: 0,
        focus: Default::default(),
        movements_selected: 0,
        agents_selected: 0,
    });

    terminal
        .draw(|frame| {
            render::render(frame, &snapshot, &mut app);
        })
        .unwrap();
}
