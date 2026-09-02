use chrono::{DateTime, Utc};
use polyphony_core::{
    AgentContextEntry, AgentContextSnapshot, AgentEventKind, AgentModelCatalog, AgentProfileSource,
    AgentProfileSummary, AgentRunHistoryRow, InboxItemRow, RunRow, RunStatus, RunningAgentRow,
    RuntimeEvent, RuntimeSnapshot, StepStatus, TaskRow,
};
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

use crate::{
    app::{IssueIntervention, IssueNotice},
    status::{state_color, state_icon},
    theme,
};

pub(crate) const CHILDREN_COLLAPSED_LIMIT: usize = 8;
const TRANSCRIPT_COLLAPSED_LIMIT: usize = 3;

pub(crate) struct IssueSession {
    pub blocks: Vec<SessionBlock>,
    pub expand_block_index: Option<usize>,
    pub expandable: bool,
}

pub(crate) struct SessionBlock {
    pub lines: Vec<Line<'static>>,
    pub accent: Color,
    pub max_height: Option<u16>,
    pub style: SessionBlockStyle,
    pub timestamp: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy)]
pub(crate) enum SessionBlockStyle {
    Full,
    Subtle,
    Plain,
}

pub(crate) fn build_issue_session(
    snapshot: &RuntimeSnapshot,
    item: &InboxItemRow,
    children_expanded: bool,
    interventions: &[IssueIntervention],
    notices: &[IssueNotice],
    tick: u32,
) -> IssueSession {
    let mut blocks = Vec::new();
    blocks.push(issue_block(item));
    let mut expand_block_index = None;

    if let Some(description) = item
        .description
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        blocks.push(description_block(description));
    }

    let children = children_for_item(snapshot, item);
    let children_expandable = children.len() > CHILDREN_COLLAPSED_LIMIT;
    let children_block_index = match children.is_empty() {
        true => None,
        false => {
            let index = blocks.len();
            blocks.push(children_block(
                &children,
                children_expandable,
                children_expanded,
            ));
            if children_expandable {
                expand_block_index = Some(index);
            }
            Some(index)
        },
    };
    let _ = children_block_index;

    for intervention in interventions
        .iter()
        .filter(|intervention| intervention.issue_id == item.item_id)
    {
        blocks.push(intervention_block(intervention));
    }

    for notice in notices
        .iter()
        .filter(|notice| notice.issue_id == item.item_id)
    {
        blocks.push(notice_block(notice));
    }

    let mut timeline_blocks = Vec::new();
    let mut runs = runs_for_item(snapshot, item);
    runs.sort_by_key(|run| run.created_at);
    for run in runs {
        timeline_blocks.push(run_block(run));

        let mut steps = run.steps.iter().collect::<Vec<_>>();
        steps.sort_by_key(|step| step.ordinal);
        for step in steps {
            timeline_blocks.push(step_block(snapshot, run, step));
        }

        let mut tasks = snapshot
            .tasks
            .iter()
            .filter(|task| task.run_id == run.id)
            .collect::<Vec<_>>();
        tasks.sort_by_key(|task| task.ordinal);
        for task in tasks {
            timeline_blocks.push(task_block(task));
        }

        for agent in running_agents_for_run(snapshot, run) {
            timeline_blocks.push(running_agent_block(snapshot, agent, tick));
            if let Some(context) = saved_context_for_agent(snapshot, agent) {
                let block = transcript_block(
                    "Live transcript",
                    Some(agent.agent_name.as_str()),
                    &context.transcript,
                    children_expanded,
                    true,
                );
                timeline_blocks.push(block);
            } else if !agent.recent_log.is_empty() {
                let block = recent_log_block(agent, children_expanded);
                timeline_blocks.push(block);
            }
        }

        for history in history_for_run(snapshot, run) {
            timeline_blocks.push(agent_history_block(snapshot, history));
            if let Some(context) = history.saved_context.as_ref() {
                let block = transcript_block(
                    "Transcript",
                    Some(history.agent_name.as_str()),
                    &context.transcript,
                    children_expanded,
                    false,
                );
                timeline_blocks.push(block);
            }
        }

        if let Some(deliverable) = &run.deliverable {
            timeline_blocks.push(deliverable_block(run, deliverable));
        }
        if matches!(
            run.status,
            RunStatus::Delivered | RunStatus::Failed | RunStatus::Cancelled
        ) {
            timeline_blocks.push(run_terminal_block(
                run,
                latest_run_activity_timestamp(snapshot, run).unwrap_or(run.created_at),
            ));
        }
    }

    let events = events_for_item(snapshot, item);
    for event in events {
        timeline_blocks.push(event_block(event));
    }

    timeline_blocks.sort_by_key(|block| block.timestamp);
    blocks.extend(timeline_blocks);

    if expand_block_index.is_none() {
        expand_block_index = blocks.iter().position(|block| block.max_height.is_some());
    }

    IssueSession {
        blocks,
        expand_block_index,
        expandable: expand_block_index.is_some(),
    }
}

fn issue_block(item: &InboxItemRow) -> SessionBlock {
    SessionBlock {
        accent: state_color(&item.status),
        max_height: None,
        style: SessionBlockStyle::Plain,
        timestamp: item.created_at.or(item.updated_at),
        lines: vec![
            Line::from(vec![
                Span::styled(
                    format!("{} ", state_icon(&item.status)),
                    Style::new().fg(state_color(&item.status)),
                ),
                Span::styled(item.title.clone(), Style::new().fg(theme::text()).bold()),
            ]),
            Line::from(vec![
                Span::styled(
                    format!("{}  ", item.identifier),
                    Style::new().fg(theme::muted()),
                ),
                Span::styled(
                    format!("{}  ", item.source),
                    Style::new().fg(theme::primary()),
                ),
                Span::styled(item.status.clone(), Style::new().fg(theme::muted())),
            ]),
        ],
    }
}

fn description_block(description: &str) -> SessionBlock {
    let mut lines = vec![Line::styled(
        "Description",
        Style::new().fg(theme::text()).bold(),
    )];
    lines.extend(
        description
            .lines()
            .map(|line| Line::styled(line.to_string(), Style::new().fg(theme::muted()))),
    );
    SessionBlock {
        lines,
        accent: theme::border(),
        max_height: None,
        style: SessionBlockStyle::Plain,
        timestamp: None,
    }
}

fn children_block(children: &[&InboxItemRow], expandable: bool, expanded: bool) -> SessionBlock {
    let mut lines = vec![Line::styled(
        "Children",
        Style::new().fg(theme::text()).bold(),
    )];
    let last_idx = children.len().saturating_sub(1);
    let visible_children = match (expandable, expanded) {
        (true, false) => CHILDREN_COLLAPSED_LIMIT,
        _ => children.len(),
    };
    for (idx, child) in children.iter().take(visible_children).enumerate() {
        let connector = match (expandable && !expanded, idx == last_idx) {
            (false, true) => "└── ",
            _ => "├── ",
        };
        lines.push(Line::from(vec![
            Span::styled(connector, Style::new().fg(theme::muted())),
            Span::styled(
                format!("{} ", state_icon(&child.status)),
                Style::new().fg(state_color(&child.status)),
            ),
            Span::styled(child.title.clone(), Style::new().fg(theme::muted())),
        ]));
    }
    match (expandable, expanded) {
        (true, false) => {
            lines.push(Line::styled("…", Style::new().fg(theme::muted())));
            lines.push(Line::styled(
                "Click to expand",
                Style::new().fg(theme::secondary()),
            ));
        },
        (true, true) => lines.push(Line::styled(
            "Click to collapse",
            Style::new().fg(theme::secondary()),
        )),
        (false, _) => {},
    }
    let max_height = lines.len().saturating_add(2) as u16;
    SessionBlock {
        lines,
        accent: theme::border(),
        max_height: Some(max_height),
        style: match expandable {
            true => SessionBlockStyle::Subtle,
            false => SessionBlockStyle::Plain,
        },
        timestamp: None,
    }
}

fn run_block(run: &RunRow) -> SessionBlock {
    let progress = format!("{}/{} tasks", run.tasks_completed, run.task_count);
    let mut lines = vec![
        Line::from(vec![
            Span::styled("●", Style::new().fg(theme::primary())),
            Span::styled(" run started ", Style::new().fg(theme::muted())),
            Span::styled(run.kind.to_string(), Style::new().fg(theme::primary())),
        ]),
        Line::from(vec![
            Span::styled(run.title.clone(), Style::new().fg(theme::text()).bold()),
            Span::styled(format!("  {progress}"), Style::new().fg(theme::muted())),
        ]),
    ];
    if let Some(branch) = &run.workspace_key {
        lines.push(Line::from(vec![
            Span::styled("workspace ", Style::new().fg(theme::muted())),
            Span::styled(branch.clone(), Style::new().fg(theme::primary())),
        ]));
    }
    SessionBlock {
        lines,
        accent: theme::primary(),
        max_height: None,
        style: SessionBlockStyle::Full,
        timestamp: Some(run.created_at),
    }
}

fn run_terminal_block(run: &RunRow, timestamp: DateTime<Utc>) -> SessionBlock {
    let accent = run_status_color(&run.status);
    let mut lines = vec![Line::from(vec![
        Span::styled(run_status_icon(&run.status), Style::new().fg(accent)),
        Span::styled(" run finished ", Style::new().fg(theme::muted())),
        Span::styled(run.kind.to_string(), Style::new().fg(theme::text()).bold()),
        Span::styled(format!("  {}", run.status), Style::new().fg(accent)),
    ])];
    lines.push(Line::from(vec![
        Span::styled(
            run.issue_identifier
                .clone()
                .unwrap_or_else(|| run.id.clone()),
            Style::new().fg(theme::muted()),
        ),
        Span::styled(
            format!("  {}/{} tasks", run.tasks_completed, run.task_count),
            Style::new().fg(theme::muted()),
        ),
    ]));
    if let Some(reason) = run
        .cancel_reason
        .as_deref()
        .filter(|reason| !reason.is_empty())
    {
        lines.push(Line::styled(
            reason.to_string(),
            Style::new().fg(theme::secondary()),
        ));
    }
    let message = match run.status {
        RunStatus::Failed => "No more work will be dispatched for this run until it is retried.",
        RunStatus::Cancelled => "This run was cancelled and will not continue.",
        RunStatus::Delivered => "This run reached a terminal delivered state.",
        _ => "",
    };
    if !message.is_empty() {
        lines.push(Line::styled(message, Style::new().fg(theme::muted())));
    }
    SessionBlock {
        lines,
        accent,
        max_height: None,
        style: match run.status {
            RunStatus::Failed | RunStatus::Cancelled => SessionBlockStyle::Full,
            _ => SessionBlockStyle::Subtle,
        },
        timestamp: Some(timestamp),
    }
}

fn run_status_icon(status: &RunStatus) -> &'static str {
    match status {
        RunStatus::Delivered => "✓",
        RunStatus::Failed => "×",
        RunStatus::Cancelled => "⊘",
        RunStatus::InProgress | RunStatus::Planning | RunStatus::Review => "●",
        RunStatus::Pending => "·",
    }
}

fn run_status_color(status: &RunStatus) -> Color {
    match status {
        RunStatus::Delivered => theme::done(),
        RunStatus::Failed | RunStatus::Cancelled => theme::error(),
        RunStatus::InProgress | RunStatus::Planning | RunStatus::Review => theme::primary(),
        RunStatus::Pending => theme::muted(),
    }
}

fn step_block(
    snapshot: &RuntimeSnapshot,
    run: &RunRow,
    step: &polyphony_core::StepRecord,
) -> SessionBlock {
    let task_title = step
        .task_id
        .as_deref()
        .and_then(|task_id| snapshot.tasks.iter().find(|task| task.id == task_id))
        .map(|task| task.title.as_str());
    let mut lines = vec![Line::from(vec![
        Span::styled(
            step_icon(step.status),
            Style::new().fg(step_color(step.status)),
        ),
        Span::styled(" step ", Style::new().fg(theme::muted())),
        Span::styled(step.kind.to_string(), Style::new().fg(theme::text()).bold()),
        Span::styled(
            format!("  {}", step.status),
            Style::new().fg(step_color(step.status)),
        ),
    ])];
    if let Some(task_title) = task_title {
        lines.push(Line::styled(
            task_title.to_string(),
            Style::new().fg(theme::muted()),
        ));
    }
    if let Some(error) = &step.error {
        lines.push(Line::styled(
            error.clone(),
            Style::new().fg(theme::secondary()),
        ));
    }
    if !step.output.is_empty() {
        let mut output = step
            .output
            .iter()
            .filter_map(|(key, value)| {
                if let Some(value) = value.as_str().filter(|value| !value.trim().is_empty()) {
                    Some(format!("{key}={value}"))
                } else if value.is_number() || value.is_boolean() {
                    Some(format!("{key}={value}"))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        output.sort();
        if !output.is_empty() {
            lines.push(Line::styled(
                output.join("  "),
                Style::new().fg(theme::muted()),
            ));
        }
    }
    let _ = run;
    SessionBlock {
        lines,
        accent: step_color(step.status),
        max_height: None,
        style: match step.status {
            StepStatus::Failed => SessionBlockStyle::Full,
            StepStatus::Running | StepStatus::Pending => SessionBlockStyle::Plain,
            StepStatus::Succeeded | StepStatus::Skipped => SessionBlockStyle::Subtle,
        },
        timestamp: step.finished_at.or(step.started_at),
    }
}

fn task_block(task: &TaskRow) -> SessionBlock {
    let agent = task.agent_name.as_deref().unwrap_or("unassigned");
    let lines = vec![
        Line::from(vec![
            Span::styled(
                task_icon(&task.status),
                Style::new().fg(task_color(&task.status)),
            ),
            Span::styled(" task ", Style::new().fg(theme::muted())),
            Span::styled(task.title.clone(), Style::new().fg(theme::text()).bold()),
        ]),
        Line::from(vec![
            Span::styled(agent.to_string(), Style::new().fg(theme::primary())),
            Span::styled(
                format!(
                    "  {} turns  {} tokens",
                    task.turns_completed, task.total_tokens
                ),
                Style::new().fg(theme::muted()),
            ),
        ]),
    ];
    SessionBlock {
        lines,
        accent: task_color(&task.status),
        max_height: None,
        style: match task.status {
            polyphony_core::TaskStatus::InProgress | polyphony_core::TaskStatus::Failed => {
                SessionBlockStyle::Full
            },
            polyphony_core::TaskStatus::Completed | polyphony_core::TaskStatus::Cancelled => {
                SessionBlockStyle::Subtle
            },
            polyphony_core::TaskStatus::Pending => SessionBlockStyle::Plain,
        },
        timestamp: task
            .finished_at
            .or(task.started_at)
            .or(Some(task.updated_at)),
    }
}

fn intervention_block(intervention: &IssueIntervention) -> SessionBlock {
    let mut lines = vec![Line::from(vec![
        Span::styled("› ", Style::new().fg(theme::secondary())),
        Span::styled("you", Style::new().fg(theme::text()).bold()),
        Span::styled(" intervened", Style::new().fg(theme::muted())),
    ])];
    if let Some(run_id) = &intervention.run_id {
        lines.push(Line::from(vec![
            Span::styled("run ", Style::new().fg(theme::muted())),
            Span::styled(run_id.clone(), Style::new().fg(theme::primary())),
        ]));
    }
    lines.extend(
        intervention
            .prompt
            .lines()
            .map(|line| Line::styled(line.to_string(), Style::new().fg(theme::text()))),
    );
    SessionBlock {
        lines,
        accent: theme::secondary(),
        max_height: None,
        style: SessionBlockStyle::Full,
        timestamp: None,
    }
}

fn notice_block(notice: &IssueNotice) -> SessionBlock {
    SessionBlock {
        lines: vec![Line::from(vec![
            Span::styled("· ", Style::new().fg(theme::secondary())),
            Span::styled(notice.message.clone(), Style::new().fg(theme::muted())),
        ])],
        accent: theme::border(),
        max_height: None,
        style: SessionBlockStyle::Plain,
        timestamp: None,
    }
}

fn running_agent_block(
    snapshot: &RuntimeSnapshot,
    agent: &RunningAgentRow,
    tick: u32,
) -> SessionBlock {
    let spinner = theme::BRAILLE_SPINNER[(tick / 4) as usize % theme::BRAILLE_SPINNER.len()];
    let catalog = agent_catalog(snapshot, &agent.agent_name);
    let profile = agent_profile(snapshot, &agent.agent_name);
    let mut lines = vec![Line::from(vec![
        Span::styled(spinner, Style::new().fg(theme::primary())),
        Span::styled(" agent ", Style::new().fg(theme::muted())),
        Span::styled(
            agent.agent_name.clone(),
            Style::new().fg(theme::text()).bold(),
        ),
        Span::styled(
            format!("  {}", agent.state),
            Style::new().fg(theme::muted()),
        ),
    ])];
    push_agent_metadata(
        &mut lines,
        catalog
            .map(|catalog| catalog.provider_kind.as_str())
            .or_else(|| profile.map(|profile| profile.kind.as_str())),
        agent.model.as_deref(),
        catalog
            .and_then(|catalog| catalog.selected_model.as_deref())
            .or_else(|| profile.and_then(|profile| profile.model.as_deref())),
        agent.session_id.as_deref(),
        agent.thread_id.as_deref(),
        agent.turn_id.as_deref(),
    );
    if let Some(message) = agent
        .last_message
        .as_deref()
        .or(agent.last_event.as_deref())
    {
        lines.push(Line::styled(
            message.to_string(),
            Style::new().fg(theme::muted()),
        ));
    }
    SessionBlock {
        lines,
        accent: theme::primary(),
        max_height: None,
        style: SessionBlockStyle::Full,
        timestamp: Some(agent.last_event_at.unwrap_or(agent.started_at)),
    }
}

fn recent_log_block(agent: &RunningAgentRow, expanded: bool) -> SessionBlock {
    let entries = agent
        .recent_log
        .iter()
        .map(|message| AgentContextEntry {
            at: agent.last_event_at.unwrap_or(agent.started_at),
            kind: AgentEventKind::OtherMessage,
            message: message.clone(),
        })
        .collect::<Vec<_>>();
    transcript_block(
        "Live log",
        Some(agent.agent_name.as_str()),
        &entries,
        expanded,
        true,
    )
}

fn transcript_block(
    title: &str,
    agent_name: Option<&str>,
    entries: &[AgentContextEntry],
    expanded: bool,
    live: bool,
) -> SessionBlock {
    let expandable = entries.len() > TRANSCRIPT_COLLAPSED_LIMIT;
    let visible_entries = match (expandable, expanded) {
        (true, false) => entries.len().saturating_sub(TRANSCRIPT_COLLAPSED_LIMIT),
        _ => 0,
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            if live {
                "● "
            } else {
                "◷ "
            },
            Style::new().fg(if live {
                theme::primary()
            } else {
                theme::muted()
            }),
        ),
        Span::styled(title.to_string(), Style::new().fg(theme::text()).bold()),
        Span::styled(
            agent_name
                .map(|name| format!("  {name}"))
                .unwrap_or_default(),
            Style::new().fg(theme::secondary()),
        ),
    ])];

    for entry in entries.iter().skip(visible_entries) {
        lines.extend(transcript_entry_lines(entry));
    }
    match (expandable, expanded) {
        (true, false) => lines.push(Line::styled(
            format!(
                "Click to expand {} earlier events",
                entries.len().saturating_sub(TRANSCRIPT_COLLAPSED_LIMIT)
            ),
            Style::new().fg(theme::secondary()),
        )),
        (true, true) => lines.push(Line::styled(
            "Click to collapse",
            Style::new().fg(theme::secondary()),
        )),
        (false, _) => {},
    }

    SessionBlock {
        lines,
        accent: if live {
            theme::primary()
        } else {
            theme::border()
        },
        max_height: match (expandable, expanded) {
            (true, false) => Some(TRANSCRIPT_COLLAPSED_LIMIT.saturating_add(3) as u16),
            _ => None,
        },
        style: match live {
            true => SessionBlockStyle::Full,
            false => SessionBlockStyle::Subtle,
        },
        timestamp: entries.last().map(|entry| entry.at),
    }
}

fn transcript_entry_lines(entry: &AgentContextEntry) -> Vec<Line<'static>> {
    let (label, color) = agent_event_label(entry.kind);
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!(
                "{} ",
                entry.at.with_timezone(&chrono::Local).format("%H:%M:%S")
            ),
            Style::new().fg(theme::muted()),
        ),
        Span::styled(format!("{label:<14}"), Style::new().fg(color)),
    ])];
    lines.extend(entry.message.lines().map(|line| {
        Line::from(vec![
            Span::styled("  ", Style::new().fg(theme::muted())),
            Span::styled(line.to_string(), Style::new().fg(theme::muted())),
        ])
    }));
    lines
}

fn agent_event_label(kind: AgentEventKind) -> (&'static str, Color) {
    match kind {
        AgentEventKind::SessionStarted => ("session", theme::primary()),
        AgentEventKind::TurnStarted => ("turn", theme::primary()),
        AgentEventKind::TurnCompleted => ("turn done", theme::done()),
        AgentEventKind::TurnFailed
        | AgentEventKind::ToolCallFailed
        | AgentEventKind::StartupFailed => ("failed", theme::error()),
        AgentEventKind::TurnCancelled => ("cancelled", theme::secondary()),
        AgentEventKind::ToolCallStarted => ("tool", theme::muted()),
        AgentEventKind::ToolCallCompleted => ("tool done", theme::done()),
        AgentEventKind::Notification => ("notice", theme::muted()),
        AgentEventKind::UsageUpdated => ("usage", theme::muted()),
        AgentEventKind::RateLimitsUpdated => ("rate limit", theme::secondary()),
        AgentEventKind::OtherMessage => ("message", theme::text()),
        AgentEventKind::Outcome => ("outcome", theme::done()),
    }
}

fn agent_history_block(snapshot: &RuntimeSnapshot, history: &AgentRunHistoryRow) -> SessionBlock {
    let catalog = agent_catalog(snapshot, &history.agent_name);
    let profile = agent_profile(snapshot, &history.agent_name);
    let mut lines = vec![Line::from(vec![
        Span::styled(
            history_icon(history.status),
            Style::new().fg(history_color(history.status)),
        ),
        Span::styled(" agent ", Style::new().fg(theme::muted())),
        Span::styled(
            history.agent_name.clone(),
            Style::new().fg(theme::text()).bold(),
        ),
        Span::styled(
            format!("  {}", history.status),
            Style::new().fg(history_color(history.status)),
        ),
    ])];
    push_agent_metadata(
        &mut lines,
        catalog
            .map(|catalog| catalog.provider_kind.as_str())
            .or_else(|| profile.map(|profile| profile.kind.as_str())),
        history.model.as_deref(),
        catalog
            .and_then(|catalog| catalog.selected_model.as_deref())
            .or_else(|| profile.and_then(|profile| profile.model.as_deref())),
        history.session_id.as_deref(),
        history.thread_id.as_deref(),
        history.turn_id.as_deref(),
    );
    if let Some(message) = history
        .last_message
        .as_deref()
        .or(history.last_event.as_deref())
    {
        lines.push(Line::styled(
            message.to_string(),
            Style::new().fg(theme::muted()),
        ));
    }
    if let Some(error) = &history.error {
        lines.push(Line::styled(
            error.clone(),
            Style::new().fg(theme::secondary()),
        ));
        if let Some(hint) = agent_failure_hint(error, profile) {
            lines.push(Line::styled(hint, Style::new().fg(theme::secondary())));
        }
    }
    SessionBlock {
        lines,
        accent: history_color(history.status),
        max_height: None,
        style: match history.status {
            polyphony_core::AttemptStatus::Succeeded
            | polyphony_core::AttemptStatus::CancelledByReconciliation
            | polyphony_core::AttemptStatus::CancelledByUser => SessionBlockStyle::Subtle,
            polyphony_core::AttemptStatus::Failed
            | polyphony_core::AttemptStatus::TimedOut
            | polyphony_core::AttemptStatus::Stalled => SessionBlockStyle::Full,
        },
        timestamp: history
            .finished_at
            .or(history.last_event_at)
            .or(Some(history.started_at)),
    }
}

fn push_agent_metadata(
    lines: &mut Vec<Line<'static>>,
    provider: Option<&str>,
    model: Option<&str>,
    selected_model: Option<&str>,
    session_id: Option<&str>,
    thread_id: Option<&str>,
    turn_id: Option<&str>,
) {
    let mut spans = Vec::new();
    if let Some(provider) = provider.filter(|value| !value.is_empty()) {
        spans.push(Span::styled("provider ", Style::new().fg(theme::muted())));
        spans.push(Span::styled(
            provider.to_string(),
            Style::new().fg(theme::primary()),
        ));
    }
    if let Some(model) = model.or(selected_model).filter(|value| !value.is_empty()) {
        if !spans.is_empty() {
            spans.push(Span::styled("  ", Style::new().fg(theme::muted())));
        }
        spans.push(Span::styled("model ", Style::new().fg(theme::muted())));
        spans.push(Span::styled(
            model.to_string(),
            Style::new().fg(theme::primary()),
        ));
    }
    if let Some(session_id) = session_id.filter(|value| !value.is_empty()) {
        if !spans.is_empty() {
            spans.push(Span::styled("  ", Style::new().fg(theme::muted())));
        }
        spans.push(Span::styled("session ", Style::new().fg(theme::muted())));
        spans.push(Span::styled(
            short_id(session_id),
            Style::new().fg(theme::secondary()),
        ));
    }
    if let Some(thread_id) = thread_id.filter(|value| !value.is_empty()) {
        if !spans.is_empty() {
            spans.push(Span::styled("  ", Style::new().fg(theme::muted())));
        }
        spans.push(Span::styled("thread ", Style::new().fg(theme::muted())));
        spans.push(Span::styled(
            short_id(thread_id),
            Style::new().fg(theme::secondary()),
        ));
    }
    if let Some(turn_id) = turn_id.filter(|value| !value.is_empty()) {
        if !spans.is_empty() {
            spans.push(Span::styled("  ", Style::new().fg(theme::muted())));
        }
        spans.push(Span::styled("turn ", Style::new().fg(theme::muted())));
        spans.push(Span::styled(
            short_id(turn_id),
            Style::new().fg(theme::secondary()),
        ));
    }
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }
}

fn agent_catalog<'a>(
    snapshot: &'a RuntimeSnapshot,
    agent_name: &str,
) -> Option<&'a AgentModelCatalog> {
    snapshot
        .agent_catalogs
        .iter()
        .find(|catalog| catalog.agent_name == agent_name)
}

fn agent_profile<'a>(
    snapshot: &'a RuntimeSnapshot,
    agent_name: &str,
) -> Option<&'a AgentProfileSummary> {
    snapshot
        .agent_profiles
        .iter()
        .find(|profile| profile.name == agent_name)
}

fn agent_failure_hint(error: &str, profile: Option<&AgentProfileSummary>) -> Option<String> {
    let profile = profile?;
    let command = profile
        .command
        .as_deref()
        .filter(|value| !value.is_empty())?;
    let executable = command.split_whitespace().next().unwrap_or(command);
    let source = agent_profile_source_label(profile);
    let prefix = if error.contains("401") {
        "401 trying to run"
    } else {
        "failed trying to run"
    };
    Some(format!(
        "{prefix} `{executable}` using {} provider defined in {source}",
        profile.kind
    ))
}

fn agent_profile_source_label(profile: &AgentProfileSummary) -> String {
    match profile.source {
        AgentProfileSource::Repository => format!(".polyphony/agents/{}.md", profile.name),
        AgentProfileSource::UserGlobal => format!("~/.polyphony/agents/{}.md", profile.name),
        AgentProfileSource::Config => "workflow config".to_string(),
    }
}

fn short_id(value: &str) -> String {
    const MAX: usize = 12;
    if value.chars().count() <= MAX {
        value.to_string()
    } else {
        format!("{}…", value.chars().take(MAX).collect::<String>())
    }
}

fn deliverable_block(run: &RunRow, deliverable: &polyphony_core::Deliverable) -> SessionBlock {
    let mut lines = vec![Line::from(vec![
        Span::styled("◷", Style::new().fg(theme::primary())),
        Span::styled(" outcome ", Style::new().fg(theme::muted())),
        Span::styled(
            deliverable.kind.to_string(),
            Style::new().fg(theme::text()).bold(),
        ),
        Span::styled(
            format!("  {}", deliverable.status),
            Style::new().fg(theme::primary()),
        ),
    ])];
    if let Some(title) = &deliverable.title {
        lines.push(Line::styled(title.clone(), Style::new().fg(theme::muted())));
    }
    if let Some(url) = &deliverable.url {
        lines.push(Line::styled(url.clone(), Style::new().fg(theme::primary())));
    }
    if let Some(workspace) = &run.workspace_key {
        lines.push(Line::from(vec![
            Span::styled("branch ", Style::new().fg(theme::muted())),
            Span::styled(workspace.clone(), Style::new().fg(theme::primary())),
        ]));
    }
    SessionBlock {
        lines,
        accent: theme::primary(),
        max_height: None,
        style: SessionBlockStyle::Full,
        timestamp: Some(run.created_at),
    }
}

fn event_block(event: &RuntimeEvent) -> SessionBlock {
    SessionBlock {
        lines: vec![Line::from(vec![
            Span::styled("◷", Style::new().fg(theme::muted())),
            Span::styled(
                format!(" {} ", event.scope),
                Style::new().fg(theme::primary()),
            ),
            Span::styled(event.message.clone(), Style::new().fg(theme::muted())),
        ])],
        accent: theme::border(),
        max_height: None,
        style: SessionBlockStyle::Plain,
        timestamp: Some(event.at),
    }
}

fn children_for_item<'a>(
    snapshot: &'a RuntimeSnapshot,
    item: &InboxItemRow,
) -> Vec<&'a InboxItemRow> {
    let mut children = snapshot
        .inbox_items
        .iter()
        .filter(|candidate| {
            candidate.parent_id.as_deref() == Some(item.item_id.as_str())
                || candidate.parent_id.as_deref() == Some(item.identifier.as_str())
        })
        .collect::<Vec<_>>();
    children.sort_by(|a, b| a.identifier.cmp(&b.identifier));
    children
}

fn runs_for_item<'a>(snapshot: &'a RuntimeSnapshot, item: &InboxItemRow) -> Vec<&'a RunRow> {
    snapshot
        .runs
        .iter()
        .filter(|run| run.issue_identifier.as_deref() == Some(item.identifier.as_str()))
        .collect()
}

fn running_agents_for_run<'a>(
    snapshot: &'a RuntimeSnapshot,
    run: &RunRow,
) -> Vec<&'a RunningAgentRow> {
    let mut agents = snapshot
        .running
        .iter()
        .filter(|agent| polyphony_core::running_agent_matches_run(run, agent))
        .collect::<Vec<_>>();
    agents.sort_by_key(|agent| agent.last_event_at.unwrap_or(agent.started_at));
    agents
}

fn history_for_run<'a>(snapshot: &'a RuntimeSnapshot, run: &RunRow) -> Vec<&'a AgentRunHistoryRow> {
    let mut history = snapshot
        .agent_run_history
        .iter()
        .filter(|history| polyphony_core::agent_history_matches_run(run, history))
        .collect::<Vec<_>>();
    history.sort_by_key(|history| {
        history
            .finished_at
            .or(history.last_event_at)
            .unwrap_or(history.started_at)
    });
    history
}

fn saved_context_for_agent<'a>(
    snapshot: &'a RuntimeSnapshot,
    agent: &RunningAgentRow,
) -> Option<&'a AgentContextSnapshot> {
    snapshot
        .saved_contexts
        .iter()
        .filter(|context| {
            context.issue_id == agent.issue_id
                && context.agent_name == agent.agent_name
                && context
                    .session_id
                    .as_ref()
                    .is_none_or(|session_id| agent.session_id.as_ref() == Some(session_id))
        })
        .max_by_key(|context| context.updated_at)
}

fn events_for_item<'a>(
    snapshot: &'a RuntimeSnapshot,
    item: &InboxItemRow,
) -> Vec<&'a RuntimeEvent> {
    let mut events = snapshot
        .recent_events
        .iter()
        .filter(|event| {
            event.message.contains(&item.identifier) || event.message.contains(&item.item_id)
        })
        .collect::<Vec<_>>();
    events.sort_by_key(|event| event.at);
    events
}

fn latest_run_activity_timestamp(
    snapshot: &RuntimeSnapshot,
    run: &RunRow,
) -> Option<DateTime<Utc>> {
    let step_times = run
        .steps
        .iter()
        .filter_map(|step| step.finished_at.or(step.started_at));
    let task_times = snapshot
        .tasks
        .iter()
        .filter(|task| task.run_id == run.id)
        .map(|task| {
            task.finished_at
                .or(task.started_at)
                .unwrap_or(task.updated_at)
        });
    let running_agent_times = snapshot
        .running
        .iter()
        .filter(|agent| polyphony_core::running_agent_matches_run(run, agent))
        .map(|agent| agent.last_event_at.unwrap_or(agent.started_at));
    let history_times = snapshot
        .agent_run_history
        .iter()
        .filter(|history| polyphony_core::agent_history_matches_run(run, history))
        .map(|history| {
            history
                .finished_at
                .or(history.last_event_at)
                .unwrap_or(history.started_at)
        });

    step_times
        .chain(task_times)
        .chain(running_agent_times)
        .chain(history_times)
        .max()
}

fn step_icon(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Succeeded => "✓",
        StepStatus::Failed => "×",
        StepStatus::Running => "●",
        StepStatus::Skipped => "⊘",
        StepStatus::Pending => "◷",
    }
}

fn step_color(status: StepStatus) -> Color {
    match status {
        StepStatus::Succeeded => theme::done(),
        StepStatus::Failed => theme::error(),
        StepStatus::Running => theme::primary(),
        StepStatus::Skipped | StepStatus::Pending => theme::muted(),
    }
}

fn task_icon(status: &polyphony_core::TaskStatus) -> &'static str {
    match status {
        polyphony_core::TaskStatus::Completed => "✓",
        polyphony_core::TaskStatus::InProgress => "●",
        polyphony_core::TaskStatus::Failed => "×",
        polyphony_core::TaskStatus::Cancelled => "⊘",
        polyphony_core::TaskStatus::Pending => "◷",
    }
}

fn task_color(status: &polyphony_core::TaskStatus) -> Color {
    match status {
        polyphony_core::TaskStatus::Completed => theme::done(),
        polyphony_core::TaskStatus::InProgress => theme::primary(),
        polyphony_core::TaskStatus::Failed => theme::error(),
        polyphony_core::TaskStatus::Cancelled | polyphony_core::TaskStatus::Pending => {
            theme::muted()
        },
    }
}

fn history_icon(status: polyphony_core::AttemptStatus) -> &'static str {
    match status {
        polyphony_core::AttemptStatus::Succeeded => "✓",
        polyphony_core::AttemptStatus::Failed
        | polyphony_core::AttemptStatus::TimedOut
        | polyphony_core::AttemptStatus::Stalled => "×",
        polyphony_core::AttemptStatus::CancelledByReconciliation
        | polyphony_core::AttemptStatus::CancelledByUser => "⊘",
    }
}

fn history_color(status: polyphony_core::AttemptStatus) -> Color {
    match status {
        polyphony_core::AttemptStatus::Succeeded => theme::done(),
        polyphony_core::AttemptStatus::Failed
        | polyphony_core::AttemptStatus::TimedOut
        | polyphony_core::AttemptStatus::Stalled => theme::error(),
        polyphony_core::AttemptStatus::CancelledByReconciliation
        | polyphony_core::AttemptStatus::CancelledByUser => theme::secondary(),
    }
}
