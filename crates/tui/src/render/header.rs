use {
    polyphony_core::{DispatchMode, RuntimeSnapshot, TrackerConnectionState},
    ratatui::{
        layout::{Alignment, Constraint, Direction, Layout, Rect},
        style::{Color, Modifier, Style},
        text::{Line, Span},
        widgets::{Block, BorderType, Paragraph, Tabs},
    },
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
    frame.render_widget(
        Paragraph::new(vec![status_summary]).block(
            Block::default()
                .title(status_title)
                .borders(ratatui::widgets::Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.border)),
        ),
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

fn header_status_width(area_width: u16, summary: &Line<'_>, title: &Line<'_>) -> u16 {
    let desired_content_width = summary.width().max(title.width());
    let desired_total_width = desired_content_width.saturating_add(2);
    let desired_total_width = u16::try_from(desired_total_width).unwrap_or(u16::MAX);
    let max_status_width = area_width.saturating_sub(MIN_TAB_SECTION_WIDTH);
    desired_total_width.min(max_status_width)
}
