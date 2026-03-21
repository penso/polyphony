use polyphony_core::{DispatchMode, RuntimeSnapshot};
use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Paragraph, Wrap},
};

use crate::{app::AppState, theme::Theme};

pub fn draw_leaving_modal(frame: &mut ratatui::Frame<'_>, theme: Theme) {
    let full_area = frame.area();
    frame.render_widget(Clear, full_area);
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.background)),
        full_area,
    );
    let area = centered_rect(full_area, 24, 3);
    let text = Line::from(Span::styled(
        "Leaving...",
        Style::default().fg(theme.foreground),
    ));
    frame.render_widget(
        Paragraph::new(text)
            .alignment(ratatui::layout::Alignment::Center)
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme.border))
                    .style(Style::default().bg(theme.background)),
            ),
        area,
    );
}

const MODE_OPTIONS: [(DispatchMode, &str, &str); 5] = [
    (
        DispatchMode::Manual,
        "Manual",
        "You choose which issues to dispatch",
    ),
    (
        DispatchMode::Automatic,
        "Automatic",
        "Issues are dispatched automatically",
    ),
    (
        DispatchMode::Nightshift,
        "Nightshift",
        "Auto + code improvements when idle",
    ),
    (
        DispatchMode::Idle,
        "Idle",
        "Only opportunistic dispatch when idle and budgets say there is headroom",
    ),
    (
        DispatchMode::Stop,
        "Stop",
        "Abort all running agents and pause all new dispatching",
    ),
];

pub fn draw_mode_modal(frame: &mut ratatui::Frame<'_>, snapshot: &RuntimeSnapshot, app: &AppState) {
    let theme = app.theme;
    let modal_height =
        u16::try_from(MODE_OPTIONS.len().saturating_mul(4).saturating_add(2)).unwrap_or(u16::MAX);
    let area = centered_rect(frame.area(), 52, modal_height);
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(Line::from(Span::styled(
            " Dispatch Mode ",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        )))
        .title_bottom(
            Line::from(vec![
                Span::styled(" j/k", Style::default().fg(theme.highlight)),
                Span::styled(":navigate  ", Style::default().fg(theme.muted)),
                Span::styled("Enter", Style::default().fg(theme.highlight)),
                Span::styled(":select  ", Style::default().fg(theme.muted)),
                Span::styled("Esc", Style::default().fg(theme.highlight)),
                Span::styled(":close ", Style::default().fg(theme.muted)),
            ])
            .right_aligned(),
        )
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.highlight))
        .style(Style::default().bg(theme.background));

    frame.render_widget(block, area);

    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });

    let mut constraints =
        Vec::with_capacity(MODE_OPTIONS.len().saturating_mul(2).saturating_add(2));
    constraints.push(Constraint::Length(1));
    for index in 0..MODE_OPTIONS.len() {
        constraints.push(Constraint::Length(3));
        if index + 1 != MODE_OPTIONS.len() {
            constraints.push(Constraint::Length(1));
        }
    }
    constraints.push(Constraint::Min(0));

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    for (i, (mode, label, desc)) in MODE_OPTIONS.iter().enumerate() {
        let is_selected = i == app.mode_modal_selected;
        let is_active = *mode == snapshot.dispatch_mode;

        let marker = if is_active {
            "● "
        } else {
            "  "
        };
        let marker_color = if is_active {
            theme.success
        } else {
            theme.muted
        };

        let label_style = if is_selected {
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.foreground)
        };

        let row_area = rows[1 + i * 2];
        let row_style = if is_selected {
            Style::default().bg(theme.panel_alt)
        } else {
            Style::default()
        };

        frame.render_widget(Block::default().style(row_style), row_area);

        let row_sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(row_area);
        let desc_columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(2), Constraint::Fill(1)])
            .split(row_sections[1]);

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(marker, Style::default().fg(marker_color)),
                Span::styled(*label, label_style),
            ]))
            .style(row_style),
            row_sections[0],
        );
        frame.render_widget(
            Paragraph::new(*desc)
                .style(row_style.fg(theme.muted))
                .wrap(Wrap { trim: false }),
            desc_columns[1],
        );
    }
}

pub fn draw_agent_picker_modal(
    frame: &mut ratatui::Frame<'_>,
    snapshot: &RuntimeSnapshot,
    app: &AppState,
) {
    use polyphony_core::AgentProfileSource;

    let theme = app.theme;
    let profiles = &snapshot.agent_profiles;
    let profile_count = profiles.len();
    // Each profile takes 2 rows (name + description) plus 1 separator, except the last.
    let has_descriptions = profiles.iter().any(|p| p.description.is_some());
    let row_height: u16 = if has_descriptions { 3 } else { 1 };
    let content_height = ((profile_count as u16) * row_height).clamp(1, 20);
    let total_height = content_height + 4;
    let area = centered_rect(frame.area(), 64, total_height);
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(Line::from(Span::styled(
            " Dispatch to Agent ",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        )))
        .title_bottom(
            Line::from(vec![
                Span::styled(" j/k", Style::default().fg(theme.highlight)),
                Span::styled(":navigate  ", Style::default().fg(theme.muted)),
                Span::styled("Enter", Style::default().fg(theme.highlight)),
                Span::styled(":dispatch  ", Style::default().fg(theme.muted)),
                Span::styled("Esc", Style::default().fg(theme.highlight)),
                Span::styled(":close ", Style::default().fg(theme.muted)),
            ])
            .right_aligned(),
        )
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.highlight))
        .style(Style::default().bg(theme.background));

    frame.render_widget(block, area);

    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });

    let mut y_offset: u16 = 0;
    for (i, profile) in profiles.iter().enumerate() {
        if y_offset >= inner.height {
            break;
        }
        let is_selected = i == app.agent_picker_selected;

        let marker = if is_selected { "▸ " } else { "  " };
        let label_style = if is_selected {
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.foreground)
        };
        let bg = if is_selected {
            Style::default().bg(theme.panel_alt)
        } else {
            Style::default()
        };

        let source_label = match profile.source {
            AgentProfileSource::UserGlobal => " ⌂",
            AgentProfileSource::Repository => " ⊙",
            AgentProfileSource::Config => "",
        };

        // Name row
        let name_area = Rect {
            x: inner.x,
            y: inner.y + y_offset,
            width: inner.width,
            height: 1,
        };
        let mut spans = vec![
            Span::styled(marker, Style::default().fg(theme.highlight)),
            Span::styled(profile.name.clone(), label_style),
            Span::styled(
                format!(" ({})", profile.kind),
                Style::default().fg(theme.muted),
            ),
        ];
        if !source_label.is_empty() {
            spans.push(Span::styled(
                source_label,
                Style::default().fg(theme.info),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)).style(bg), name_area);
        y_offset += 1;

        // Description row
        if has_descriptions {
            if y_offset < inner.height {
                let desc_area = Rect {
                    x: inner.x,
                    y: inner.y + y_offset,
                    width: inner.width,
                    height: 1,
                };
                let desc_text = profile
                    .description
                    .as_deref()
                    .unwrap_or("");
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        format!("  {desc_text}"),
                        Style::default().fg(theme.muted),
                    )))
                    .style(bg),
                    desc_area,
                );
                y_offset += 1;
            }

            // Separator
            if i + 1 < profile_count && y_offset < inner.height {
                y_offset += 1;
            }
        }
    }
}

pub(crate) fn draw_confirm_quit(frame: &mut ratatui::Frame<'_>, app: &AppState) {
    let theme = app.theme;
    let area = centered_rect(frame.area(), 34, 3);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Quit? ", Style::default().fg(theme.foreground)),
            Span::styled("y", Style::default().fg(theme.highlight)),
            Span::styled("/", Style::default().fg(theme.muted)),
            Span::styled("n", Style::default().fg(theme.highlight)),
        ]))
        .alignment(ratatui::layout::Alignment::Center)
        .block(
            Block::default()
                .borders(ratatui::widgets::Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.warning))
                .style(Style::default().bg(theme.panel_alt)),
        ),
        area,
    );
}

pub fn draw_help_modal(frame: &mut ratatui::Frame<'_>, app: &AppState) {
    let theme = app.theme;

    const KEYBINDINGS: &[(&str, &str, &str)] = &[
        // (key, short, description)
        ("Navigation", "", ""),
        ("1-6", "tabs", "Switch between Triggers, Orchestration, Tasks, Outcomes, Agents, Logs"),
        ("j / k", "navigate", "Move selection up/down in lists"),
        ("PgUp / PgDn", "page", "Page up/down in lists and detail views"),
        ("g / G", "top/bottom", "Jump to top or bottom"),
        ("Enter", "details", "Open detail view for selected item"),
        ("Esc", "back", "Close detail view, clear search, or switch focus"),
        ("Tab", "focus", "Toggle focus between list and detail in split view"),
        ("/", "search", "Filter items by keyword"),
        ("", "", ""),
        ("Agent Actions", "", ""),
        ("S", "stop agent", "Stop a running agent (Agents/Orchestration tabs)"),
        ("c", "cast", "View live log (running) or replay recording (finished)"),
        ("w", "workspace", "Open terminal at agent's workspace directory"),
        ("d", "dispatch", "Manually dispatch selected trigger to an agent"),
        ("", "", ""),
        ("Workflow", "", ""),
        ("a", "approve", "Approve a waiting trigger or accept a deliverable"),
        ("x", "reject", "Reject a deliverable"),
        ("t", "retry task", "Retry a failed pipeline task"),
        ("R", "resolve task", "Mark a task as completed manually"),
        ("m", "mode", "Change dispatch mode (Manual/Auto/Nightshift/Idle/Stop)"),
        ("", "", ""),
        ("Other", "", ""),
        ("o / O", "open", "Open issue/PR in browser (o) or full URL (O)"),
        ("s", "sort", "Toggle sort order in Agents tab"),
        ("r", "refresh", "Force refresh from trackers"),
        ("?", "help", "Show this help"),
        ("q", "quit", "Quit polyphony"),
    ];

    let content_lines = KEYBINDINGS.len() as u16;
    let modal_height = (content_lines + 4).min(frame.area().height.saturating_sub(4));
    let area = centered_rect(frame.area(), 78, modal_height);
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(Line::from(Span::styled(
            " Keybindings ",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        )))
        .title_bottom(
            Line::from(vec![
                Span::styled("Esc", Style::default().fg(theme.highlight)),
                Span::styled(" or ", Style::default().fg(theme.muted)),
                Span::styled("?", Style::default().fg(theme.highlight)),
                Span::styled(":close ", Style::default().fg(theme.muted)),
            ])
            .right_aligned(),
        )
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.highlight))
        .style(Style::default().bg(theme.background));
    frame.render_widget(&block, area);

    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });

    let lines: Vec<Line<'_>> = KEYBINDINGS
        .iter()
        .map(|(key, short, desc)| {
            if key.is_empty() {
                // Blank separator line
                Line::default()
            } else if short.is_empty() {
                // Section header
                Line::from(Span::styled(
                    format!("── {key} ──"),
                    Style::default()
                        .fg(theme.highlight)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                // Key binding row
                Line::from(vec![
                    Span::styled(
                        format!("{key:>12}"),
                        Style::default().fg(theme.highlight),
                    ),
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        (*desc).to_string(),
                        Style::default().fg(theme.foreground),
                    ),
                ])
            }
        })
        .collect();

    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        inner,
    );
}

pub(crate) fn centered_rect(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = area.width.min(max_width).max(1);
    let height = area.height.min(max_height).max(1);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width, height)
}
