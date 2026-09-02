use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Widget},
};

use crate::{app::AppState, theme};

pub(crate) struct Command {
    pub section: &'static str,
    pub name: &'static str,
    pub description: &'static str,
}

enum RenderRow {
    Section(&'static str),
    Command {
        filtered_idx: usize,
        command: &'static Command,
    },
}

pub(crate) const COMMANDS: &[Command] = &[
    Command {
        section: "Inbox",
        name: "Open selected inbox item",
        description: "Focus the currently selected orchestration",
    },
    Command {
        section: "Inbox",
        name: "Create new issue",
        description: "Draft an issue in an enabled backend",
    },
    Command {
        section: "Inbox",
        name: "Sort by oldest",
        description: "Show the oldest inbox item first",
    },
    Command {
        section: "Inbox",
        name: "Sort by priority",
        description: "Group urgent work at the top",
    },
    Command {
        section: "Runtime",
        name: "Refresh trackers",
        description: "Fetch GitHub, GitLab, Linear, and beads updates",
    },
    Command {
        section: "Runtime",
        name: "Switch dispatch mode",
        description: "Choose manual, automatic, nightshift, idle, or stop",
    },
    Command {
        section: "Runtime",
        name: "Toggle dispatch start/stop",
        description: "Switch global orchestrator mode between stop and manual",
    },
    Command {
        section: "Runtime",
        name: "Stop dispatching",
        description: "Pause automatic agent dispatch",
    },
    Command {
        section: "Runtime",
        name: "Manual dispatch mode",
        description: "Resume issue-scoped manual dispatch",
    },
    Command {
        section: "Runtime",
        name: "Switch to nightshift mode",
        description: "Let Polyphony work unattended",
    },
    Command {
        section: "Agents",
        name: "Dispatch implementer",
        description: "Assign implementation work to an agent",
    },
    Command {
        section: "Agents",
        name: "Dispatch reviewer",
        description: "Ask an agent to review the selected work",
    },
    Command {
        section: "View",
        name: "Toggle widget timestamps",
        description: "Show or hide timestamps on session widgets",
    },
    Command {
        section: "View",
        name: "Toggle logs overlay",
        description: "Inspect runtime events and agent logs",
    },
    Command {
        section: "View",
        name: "Open repository settings",
        description: "Manage tracker and repo configuration",
    },
];

pub(crate) fn selected_down(app: &mut AppState) {
    let count = filtered_commands(app).len();
    if count == 0 {
        app.command_selected = 0;
    } else {
        app.command_selected = (app.command_selected + 1) % count;
    }
}

pub(crate) fn selected_up(app: &mut AppState) {
    let count = filtered_commands(app).len();
    if count == 0 {
        app.command_selected = 0;
    } else if app.command_selected == 0 {
        app.command_selected = count - 1;
    } else {
        app.command_selected -= 1;
    }
}

pub(crate) fn reset(app: &mut AppState) {
    app.command_query.clear();
    app.command_selected = 0;
    app.command_scroll = 0;
}

pub(crate) fn push_query_char(app: &mut AppState, c: char) {
    app.command_query.push(c);
    app.command_selected = 0;
    app.command_scroll = 0;
}

pub(crate) fn pop_query_char(app: &mut AppState) {
    app.command_query.pop();
    app.command_selected = app
        .command_selected
        .min(filtered_commands(app).len().saturating_sub(1));
    app.command_scroll = 0;
}

pub(crate) fn selected_command(app: &AppState) -> Option<&'static Command> {
    filtered_commands(app)
        .get(app.command_selected)
        .map(|(_, command)| *command)
}

pub(crate) fn render(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut AppState) {
    let width = 74u16.min(area.width.saturating_sub(4)).max(42);
    let height = 22u16.min(area.height.saturating_sub(4)).max(8);
    let modal = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, modal);
    frame.render_widget(
        Block::default().style(Style::new().bg(theme::element())),
        modal,
    );

    let inner = modal.inner(ratatui::layout::Margin::new(2, 1));
    let content = inner;
    let visible_commands = visible_command_count(content.height);
    let filtered = filtered_commands(app);
    app.command_selected = app.command_selected.min(filtered.len().saturating_sub(1));
    let rows = render_rows(&filtered);
    sync_scroll_for_height(app, visible_commands, &rows);
    let search_line = if app.command_query.is_empty() {
        Line::styled("Search", Style::new().fg(theme::muted()))
    } else {
        Line::from(vec![
            Span::styled(&app.command_query, Style::new().fg(theme::text())),
            Span::styled("█", Style::new().fg(theme::primary())),
        ])
    };
    let title_gap = content
        .width
        .saturating_sub("Commands".len() as u16 + "esc".len() as u16);
    let mut lines = vec![Line::from(vec![
        Span::styled("Commands", Style::new().fg(theme::text()).bold()),
        Span::raw(" ".repeat(title_gap as usize)),
        Span::styled("esc", Style::new().fg(theme::muted())),
    ])];
    lines.push(search_line);
    lines.push(Line::raw(""));

    if filtered.is_empty() {
        lines.push(Line::styled(
            "No commands found",
            Style::new().fg(theme::muted()),
        ));
    }
    for row in rows.iter().skip(app.command_scroll).take(visible_commands) {
        match row {
            RenderRow::Section(section) => {
                lines.push(Line::styled(*section, Style::new().fg(theme::muted())));
            },
            RenderRow::Command {
                filtered_idx,
                command,
            } => {
                let selected = *filtered_idx == app.command_selected;
                let bg = if selected {
                    theme::bg()
                } else {
                    theme::element()
                };
                let marker = if selected {
                    "▶ "
                } else {
                    "  "
                };
                let left_width = marker.chars().count() + command.name.chars().count();
                let description_width = command.description.chars().count();
                let gap = content
                    .width
                    .saturating_sub((left_width + description_width) as u16)
                    .max(2) as usize;
                lines.push(Line::from(vec![
                    Span::styled(marker, Style::new().fg(theme::primary()).bg(bg)),
                    Span::styled(
                        command.name,
                        Style::new()
                            .fg(theme::text())
                            .bg(bg)
                            .add_modifier(if selected {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            }),
                    ),
                    Span::styled(" ".repeat(gap), Style::new().bg(bg)),
                    Span::styled(command.description, Style::new().fg(theme::muted()).bg(bg)),
                ]));
            },
        }
    }

    Paragraph::new(lines)
        .style(Style::new().bg(theme::element()))
        .render(content, frame.buffer_mut());
}

fn visible_command_count(content_height: u16) -> usize {
    // Header/search/spacer consume three rows; command rows are single-line.
    content_height.saturating_sub(3).max(1) as usize
}

fn sync_scroll_for_height(app: &mut AppState, visible_rows: usize, rows: &[RenderRow]) {
    let max_scroll = rows.len().saturating_sub(visible_rows);
    if rows.len() <= visible_rows {
        app.command_scroll = 0;
        return;
    }

    let selected_row = rows
        .iter()
        .position(|row| {
            matches!(
                row,
                RenderRow::Command { filtered_idx, .. } if *filtered_idx == app.command_selected
            )
        })
        .unwrap_or_default();
    let center = visible_rows / 2;
    app.command_scroll = selected_row.saturating_sub(center).min(max_scroll);
}

fn render_rows(filtered: &[(i64, &'static Command)]) -> Vec<RenderRow> {
    let mut rows = Vec::new();
    let mut last_section = "";
    for (idx, (_, command)) in filtered.iter().enumerate() {
        if command.section != last_section {
            rows.push(RenderRow::Section(command.section));
            last_section = command.section;
        }
        rows.push(RenderRow::Command {
            filtered_idx: idx,
            command,
        });
    }
    rows
}

fn filtered_commands(app: &AppState) -> Vec<(i64, &'static Command)> {
    let query = app.command_query.trim();
    if query.is_empty() {
        return COMMANDS.iter().map(|command| (0, command)).collect();
    }

    let matcher = SkimMatcherV2::default();
    let mut matches = COMMANDS
        .iter()
        .filter_map(|command| {
            let haystack = format!(
                "{} {} {}",
                command.section, command.name, command.description
            );
            matcher
                .fuzzy_match(&haystack, query)
                .map(|score| (score, command))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|(score_a, command_a), (score_b, command_b)| {
        score_b
            .cmp(score_a)
            .then_with(|| command_a.section.cmp(command_b.section))
            .then_with(|| command_a.name.cmp(command_b.name))
    });
    matches
}
