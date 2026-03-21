use polyphony_core::{DispatchMode, RuntimeSnapshot, TrackerConnectionState};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph, Tabs},
};

use crate::app::{ActiveTab, AppState, TAB_DIVIDER, TAB_PADDING_LEFT, TAB_PADDING_RIGHT};

const GITHUB_MARK: &str = "";
const MIN_TAB_SECTION_WIDTH: u16 = 56;

pub fn draw_header(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    let status_summary = header_summary_line(snapshot, theme);
    let status_title = header_status_title(snapshot, theme);
    let status_width = header_status_width(area.width, &status_summary, &status_title);

    let sections = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Fill(1), Constraint::Length(status_width)])
        .split(area);

    // Tab bar
    let mut tab_block = Block::default()
        .title(Line::from(Span::styled(
            " Polyphony ",
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        )))
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border));

    if let Some((github_label, github_color)) = github_connection_label(snapshot, theme.success) {
        tab_block = tab_block.title(
            Line::from(Span::styled(
                format!(" {github_label} "),
                Style::default()
                    .fg(github_color)
                    .add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Right),
        );
    }

    // When a detail view is active, show breadcrumb in the bottom title
    if app.has_detail() {
        let breadcrumb = super::detail_common::build_breadcrumb(app, snapshot);
        if !breadcrumb.spans.is_empty() {
            let mut bc_spans = vec![Span::styled(" ", Style::default())];
            bc_spans.extend(breadcrumb.spans);
            bc_spans.push(Span::styled(" ", Style::default()));
            tab_block = tab_block.title_bottom(Line::from(bc_spans));
        }
    }

    let tabs = Tabs::new(
        ActiveTab::ALL
            .into_iter()
            .map(|tab| Line::from(Span::styled(tab.title(), Style::default().fg(theme.muted))))
            .collect::<Vec<_>>(),
    )
    .select(app.active_tab.index())
    .divider(Span::raw(TAB_DIVIDER))
    .padding(TAB_PADDING_LEFT, TAB_PADDING_RIGHT)
    .highlight_style(
        Style::default()
            .fg(theme.highlight)
            .add_modifier(Modifier::BOLD),
    )
    .block(tab_block);
    // Store the inner area of the tab block for mouse click hit-testing.
    app.tab_inner_area = sections[0].inner(ratatui::layout::Margin {
        vertical: 1,
        horizontal: 1,
    });
    frame.render_widget(tabs, sections[0]);

    // Status summary
    let mut status_block = Block::default()
        .title(status_title)
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border));

    let budget_footer = compact_budget_footer(snapshot, theme, sections[1].width.saturating_sub(2));
    if !budget_footer.spans.is_empty() {
        status_block = status_block.title_bottom(budget_footer);
    }

    frame.render_widget(
        Paragraph::new(vec![status_summary]).block(status_block),
        sections[1],
    );
}

fn github_connection_label(
    snapshot: &RuntimeSnapshot,
    success_color: Color,
) -> Option<(String, Color)> {
    let status = snapshot.tracker_connection.as_ref()?;
    match status.state {
        TrackerConnectionState::Connected => status
            .label
            .as_deref()
            .filter(|label| !label.is_empty())
            .map(|label| (format!("{GITHUB_MARK} {label}"), success_color)),
        TrackerConnectionState::Disconnected => Some((
            format!(
                "{GITHUB_MARK} {}",
                status.detail.as_deref().unwrap_or("disconnected")
            ),
            Color::Yellow,
        )),
        TrackerConnectionState::Unknown => {
            Some((format!("{GITHUB_MARK} checking"), Color::DarkGray))
        },
    }
}

fn header_summary_line(snapshot: &RuntimeSnapshot, theme: crate::theme::Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled("triggers ", Style::default().fg(theme.muted)),
        Span::styled(
            snapshot.visible_triggers.len().to_string(),
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled("running ", Style::default().fg(theme.muted)),
        Span::styled(
            snapshot.counts.running.to_string(),
            Style::default()
                .fg(theme.success)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        if snapshot.counts.worktrees > 0 {
            Span::styled("worktrees ", Style::default().fg(theme.muted))
        } else {
            Span::raw("")
        },
        if snapshot.counts.worktrees > 0 {
            Span::styled(
                format!("{}  ", snapshot.counts.worktrees),
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw("")
        },
        Span::styled("tasks ", Style::default().fg(theme.muted)),
        Span::styled(
            format!(
                "{}/{}",
                snapshot.counts.tasks_completed,
                snapshot.counts.tasks_pending
                    + snapshot.counts.tasks_in_progress
                    + snapshot.counts.tasks_completed
            ),
            Style::default().fg(theme.info).add_modifier(Modifier::BOLD),
        ),
    ])
}

fn header_status_title(snapshot: &RuntimeSnapshot, theme: crate::theme::Theme) -> Line<'static> {
    let (mode_label, mode_color) = match snapshot.dispatch_mode {
        DispatchMode::Manual => ("manual", theme.info),
        DispatchMode::Automatic => ("auto", theme.success),
        DispatchMode::Nightshift => ("nightshift", theme.highlight),
        DispatchMode::Idle => ("idle", theme.warning),
        DispatchMode::Stop => ("stop", theme.danger),
    };

    let mut status_spans = vec![
        Span::styled("● ", Style::default().fg(mode_color)),
        Span::styled(
            mode_label,
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    if snapshot.from_cache {
        status_spans.push(Span::styled(
            " (cached)",
            Style::default().fg(theme.warning),
        ));
    }

    status_spans.push(Span::styled(" ", Style::default()));
    Line::from(status_spans)
}

/// Session and weekly budget percentages for a provider.
struct BudgetPcts {
    session: Option<u32>,
    weekly: Option<u32>,
}

fn compact_budget_footer(
    snapshot: &RuntimeSnapshot,
    theme: crate::theme::Theme,
    max_width: u16,
) -> Line<'static> {
    let has_budgets = !snapshot.budgets.is_empty();
    let has_budget_throttles = snapshot
        .throttles
        .iter()
        .any(|t| t.component.starts_with("budget:"));
    if !has_budgets && !has_budget_throttles {
        return Line::default();
    }

    // Deduplicate by provider kind so agents sharing the same provider show once.
    // The raw JSON contains a "provider" field (e.g. "codex", "claude").
    // Fall back to the agent name if no provider field is present.
    let mut seen = std::collections::BTreeMap::<String, BudgetPcts>::new();
    for budget in &snapshot.budgets {
        let label = budget
            .raw
            .as_ref()
            .and_then(|raw| raw.get("provider").and_then(|v| v.as_str()))
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                budget
                    .component
                    .strip_prefix("agent:")
                    .unwrap_or(&budget.component)
                    .to_string()
            });
        if seen.contains_key(&label) {
            continue;
        }
        let session = match (budget.credits_remaining, budget.credits_total) {
            (Some(remaining), Some(total)) if total > 0.0 => {
                Some((remaining / total * 100.0).round() as u32)
            },
            _ => None,
        };
        let weekly = budget
            .raw
            .as_ref()
            .and_then(|raw| raw.get("weekly_remaining"))
            .and_then(|v| v.as_f64())
            .map(|pct| pct.round() as u32);
        seen.insert(label, BudgetPcts { session, weekly });
    }

    let mut spans: Vec<Span<'static>> = Vec::new();
    for (label, pcts) in &seen {
        if !spans.is_empty() {
            spans.push(Span::styled(" ", Style::default().fg(theme.border)));
        }
        let worst = pcts
            .session
            .into_iter()
            .chain(pcts.weekly)
            .min()
            .unwrap_or(100);
        let color = if worst <= 10 {
            theme.danger
        } else if worst <= 30 {
            theme.warning
        } else {
            theme.muted
        };
        let text = match (pcts.session, pcts.weekly) {
            (Some(s), Some(w)) => format!("{label}:{s}%/{w}%"),
            (Some(s), None) => format!("{label}:{s}%"),
            (None, Some(w)) => format!("{label}:{w}%"),
            (None, None) => label.clone(),
        };
        spans.push(Span::styled(text, Style::default().fg(color)));
    }

    // Show throttled budget providers that have no snapshot yet
    for throttle in &snapshot.throttles {
        let Some(kind) = throttle.component.strip_prefix("budget:") else {
            continue;
        };
        if seen.contains_key(kind) {
            continue;
        }
        if !spans.is_empty() {
            spans.push(Span::styled(" ", Style::default().fg(theme.border)));
        }
        spans.push(Span::styled(
            format!("{kind}:429"),
            Style::default().fg(theme.danger),
        ));
    }

    // Trim if too wide
    let total_width: usize = spans.iter().map(|s| s.content.len()).sum();
    if total_width > max_width as usize {
        let mut trimmed = Vec::new();
        let mut used = 0;
        for span in spans {
            let w = span.content.len();
            if used + w > max_width as usize {
                break;
            }
            used += w;
            trimmed.push(span);
        }
        return Line::from(trimmed);
    }

    Line::from(spans)
}

fn header_status_width(area_width: u16, summary: &Line<'_>, title: &Line<'_>) -> u16 {
    let desired_content_width = summary.width().max(title.width());
    let desired_total_width = desired_content_width.saturating_add(2);
    let desired_total_width = u16::try_from(desired_total_width).unwrap_or(u16::MAX);
    let max_status_width = area_width.saturating_sub(MIN_TAB_SECTION_WIDTH);
    desired_total_width.min(max_status_width)
}
