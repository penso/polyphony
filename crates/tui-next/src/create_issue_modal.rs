use std::collections::HashSet;

use polyphony_core::{InboxItemKind, RuntimeSnapshot, TrackerKind};
use ratatui::{
    layout::{Constraint, Layout, Margin, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Widget, Wrap},
};

use crate::{
    app::{AppState, CreateIssueBackendOption, CreateIssueModalState},
    theme,
};

pub(crate) fn open(app: &mut AppState, snapshot: &RuntimeSnapshot) {
    let mut modal = CreateIssueModalState {
        backend_options: backend_options(snapshot),
        ..CreateIssueModalState::default()
    };
    if modal.backend_options.len() == 1 {
        modal.selected_backend = 0;
    }
    app.create_issue_modal = Some(modal);
}

pub(crate) fn render(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut AppState) {
    let Some(modal) = app.create_issue_modal.as_ref() else {
        return;
    };
    let width = 78u16.min(area.width.saturating_sub(4)).max(48);
    let height = 17u16.min(area.height.saturating_sub(4)).max(10);
    let modal_area = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, modal_area);
    frame.render_widget(
        Block::default().style(Style::new().bg(theme::element())),
        modal_area,
    );

    let inner = modal_area.inner(Margin::new(2, 1));
    let [header, title, backend, description, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(4),
        Constraint::Length(1),
    ])
    .areas(inner);

    draw_header(frame, header);
    draw_title(frame, title, modal);
    draw_backend(frame, backend, modal);
    draw_description(frame, description, modal);
    draw_footer(frame, footer);
    set_cursor(frame, title, description, modal);
}

fn draw_header(frame: &mut ratatui::Frame<'_>, area: Rect) {
    let gap = area
        .width
        .saturating_sub("New issue".len() as u16 + "esc".len() as u16);
    Line::from(vec![
        Span::styled("New issue", Style::new().fg(theme::text()).bold()),
        Span::raw(" ".repeat(gap as usize)),
        Span::styled("esc", Style::new().fg(theme::muted())),
    ])
    .render(area, frame.buffer_mut());
    Line::styled(
        "Tab fields · ←/→ backend · Ctrl+D create",
        Style::new().fg(theme::muted()),
    )
    .render(
        Rect::new(area.x, area.y.saturating_add(1), area.width, 1),
        frame.buffer_mut(),
    );
}

fn draw_title(frame: &mut ratatui::Frame<'_>, area: Rect, modal: &CreateIssueModalState) {
    let focused = modal.focused_field == CreateIssueModalState::TITLE_FIELD;
    draw_label(frame, area, "title", focused);
    let text = if modal.title.is_empty() {
        "Issue title"
    } else {
        modal.title.as_str()
    };
    let color = if modal.title.is_empty() {
        theme::muted()
    } else {
        theme::text()
    };
    Line::styled(text, Style::new().fg(color)).render(input_rect(area), frame.buffer_mut());
}

fn draw_backend(frame: &mut ratatui::Frame<'_>, area: Rect, modal: &CreateIssueModalState) {
    let focused = modal.focused_field == CreateIssueModalState::BACKEND_FIELD;
    draw_label(frame, area, "backend", focused);
    let text = modal
        .selected_backend()
        .map(|backend| format!("‹ {} ›", backend.label))
        .unwrap_or_else(|| "no issue backend available".to_string());
    let color = if modal.selected_backend().is_some() {
        theme::text()
    } else {
        theme::error()
    };
    Line::styled(text, Style::new().fg(color)).render(input_rect(area), frame.buffer_mut());
}

fn draw_description(frame: &mut ratatui::Frame<'_>, area: Rect, modal: &CreateIssueModalState) {
    let focused = modal.focused_field == CreateIssueModalState::DESCRIPTION_FIELD;
    draw_label(frame, area, "description", focused);
    let input = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    let text = if modal.description.is_empty() {
        "Optional description"
    } else {
        modal.description.as_str()
    };
    let color = if modal.description.is_empty() {
        theme::muted()
    } else {
        theme::text()
    };
    Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .style(Style::new().fg(color).bg(theme::element()))
        .render(input, frame.buffer_mut());
}

fn draw_footer(frame: &mut ratatui::Frame<'_>, area: Rect) {
    Line::from(vec![
        Span::styled("Ctrl+D", Style::new().fg(theme::text()).bold()),
        Span::styled(":create  ", Style::new().fg(theme::muted())),
        Span::styled("Enter", Style::new().fg(theme::text()).bold()),
        Span::styled(":newline in description  ", Style::new().fg(theme::muted())),
        Span::styled("n", Style::new().fg(theme::text()).bold()),
        Span::styled(":new issue", Style::new().fg(theme::muted())),
    ])
    .render(area, frame.buffer_mut());
}

fn draw_label(frame: &mut ratatui::Frame<'_>, area: Rect, label: &'static str, focused: bool) {
    let color = if focused {
        theme::primary()
    } else {
        theme::muted()
    };
    Line::styled(label, Style::new().fg(color).bold()).render(area, frame.buffer_mut());
}

fn input_rect(area: Rect) -> Rect {
    Rect::new(area.x, area.y.saturating_add(1), area.width, 1)
}

fn set_cursor(
    frame: &mut ratatui::Frame<'_>,
    title: Rect,
    description: Rect,
    modal: &CreateIssueModalState,
) {
    let (area, text) = match modal.focused_field {
        CreateIssueModalState::TITLE_FIELD => (input_rect(title), modal.title.as_str()),
        CreateIssueModalState::DESCRIPTION_FIELD => (
            Rect::new(
                description.x,
                description.y + 1,
                description.width,
                description.height.saturating_sub(1),
            ),
            modal.description.as_str(),
        ),
        _ => return,
    };
    let (line, col) = visual_cursor_position(text, modal.cursor, area.width as usize);
    frame.set_cursor_position((
        area.x + (col as u16).min(area.width.saturating_sub(1)),
        area.y + (line as u16).min(area.height.saturating_sub(1)),
    ));
}

fn visual_cursor_position(text: &str, cursor: usize, width: usize) -> (usize, usize) {
    let width = width.max(1);
    let mut line = 0usize;
    let mut col = 0usize;
    for ch in text.chars().take(cursor) {
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
            if col >= width {
                line += 1;
                col = 0;
            }
        }
    }
    (line, col)
}

fn backend_options(snapshot: &RuntimeSnapshot) -> Vec<CreateIssueBackendOption> {
    let mut options = snapshot
        .repo_registrations
        .iter()
        .filter(|repo| repo.tracker_kind != TrackerKind::None)
        .map(|repo| CreateIssueBackendOption {
            label: format!("{} · {}", repo.tracker_kind, repo.repo_id),
            repo_id: Some(repo.repo_id.clone()),
            tracker_source: Some(repo.tracker_kind.to_string()),
        })
        .collect::<Vec<_>>();
    let mut seen = options
        .iter()
        .map(|option| option.label.clone())
        .collect::<HashSet<_>>();
    for source in supplemental_issue_sources(snapshot) {
        let repo_id = repo_id_for_source(snapshot, &source);
        let label = match repo_id.as_deref() {
            Some(repo_id) if !repo_id.is_empty() => format!("{source} · {repo_id}"),
            _ => source.clone(),
        };
        if seen.insert(label.clone()) {
            options.push(CreateIssueBackendOption {
                label,
                repo_id,
                tracker_source: Some(source),
            });
        }
    }
    if options.is_empty() && snapshot.tracker_kind != TrackerKind::None {
        options.push(CreateIssueBackendOption {
            label: snapshot.tracker_kind.to_string(),
            repo_id: None,
            tracker_source: Some(snapshot.tracker_kind.to_string()),
        });
    }
    options
}

fn supplemental_issue_sources(snapshot: &RuntimeSnapshot) -> Vec<String> {
    let mut sources = snapshot
        .inbox_items
        .iter()
        .filter(|item| item.kind == InboxItemKind::Issue)
        .map(|item| item.source.as_str())
        .chain(snapshot.tracker_issues.iter().filter_map(|issue| {
            issue
                .issue_id
                .split(':')
                .next()
                .filter(|source| known_tracker_source(source))
        }))
        .filter(|source| known_tracker_source(source))
        .map(str::to_string)
        .collect::<Vec<_>>();
    sources.sort();
    sources.dedup();
    sources
}

fn repo_id_for_source(snapshot: &RuntimeSnapshot, source: &str) -> Option<String> {
    snapshot
        .inbox_items
        .iter()
        .find(|item| {
            item.kind == InboxItemKind::Issue && item.source == source && !item.repo_id.is_empty()
        })
        .map(|item| item.repo_id.clone())
        .or_else(|| {
            snapshot
                .tracker_issues
                .iter()
                .find(|issue| {
                    issue
                        .issue_id
                        .split(':')
                        .next()
                        .is_some_and(|prefix| prefix == source)
                        && !issue.repo_id.is_empty()
                })
                .map(|issue| issue.repo_id.clone())
        })
}

fn known_tracker_source(source: &str) -> bool {
    matches!(source, "beads" | "github" | "gitlab" | "linear" | "mock")
}
