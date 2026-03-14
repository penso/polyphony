use {
    chrono::{DateTime, Utc},
    polyphony_core::RuntimeSnapshot,
    ratatui::{
        layout::{Alignment, Constraint, Rect},
        style::{Modifier, Style},
        text::{Line, Span},
        widgets::{
            Block, BorderType, Cell, HighlightSpacing, Padding, Row, Scrollbar,
            ScrollbarOrientation, ScrollbarState, Table,
        },
    },
};

use crate::app::AppState;

const BRAILLE_SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub fn draw_issues_tab(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    let indices = &app.sorted_issue_indices;

    let now = Utc::now();

    // Precompute types and age so we can measure column widths
    let issue_data: Vec<_> = indices
        .iter()
        .filter_map(|&i| snapshot.visible_issues.get(i))
        .map(|issue| {
            let (issue_type, clean_title) = extract_type_and_title(&issue.title);
            let age = issue.created_at.map(|c| format_relative_time(c, now));
            (issue, issue_type, clean_title, age)
        })
        .collect();

    // Compute ID column width from actual identifiers (icon + id + padding)
    let max_id_len = issue_data
        .iter()
        .map(|(issue, _, _, _)| issue.issue_identifier.len() + 1) // +1 for source icon
        .max()
        .unwrap_or(4) as u16
        + 1; // +1 right padding

    // Compute Type column width from actual types + 1 padding
    let max_type_len = issue_data
        .iter()
        .map(|(_, t, _, _)| t.len())
        .max()
        .unwrap_or(4)
        .max(4) as u16 // min width = "Type" header
        + 1;

    // Compute Status column width: max of data and "Status" header + 1 trailing space
    let max_status_len = issue_data
        .iter()
        .map(|(issue, _, _, _)| issue.state.len())
        .max()
        .unwrap_or(6)
        .max(6) as u16 // min width = "Status" header
        + 1;

    // Age column: max of data and "Age" header + 1 padding
    let max_age_len = issue_data
        .iter()
        .filter_map(|(_, _, _, age)| age.as_ref().map(|a| a.len()))
        .max()
        .unwrap_or(3)
        .max(3) as u16 // min width = "Age" header
        + 1;

    let header = Row::new(vec![
        Cell::from(
            Line::from(Span::styled("ID", Style::default().fg(theme.muted)))
                .alignment(Alignment::Center),
        ),
        Cell::from(Span::styled("Title", Style::default().fg(theme.muted))),
        Cell::from(
            Line::from(Span::styled("Type", Style::default().fg(theme.muted)))
                .alignment(Alignment::Right),
        ),
        Cell::from(
            Line::from(Span::styled("Status", Style::default().fg(theme.muted)))
                .alignment(Alignment::Right),
        ),
        Cell::from(
            Line::from(vec![
                Span::styled("Age", Style::default().fg(theme.muted)),
                Span::raw(" "),
            ])
            .alignment(Alignment::Right),
        ),
    ])
    .height(1)
    .style(Style::default().add_modifier(Modifier::BOLD));

    // Available width for the title column
    // area.width - borders(2) - right_padding(1) - highlight_symbol(2) - id - type - status - age - column_gaps(4)
    let title_max_width = (area.width as usize).saturating_sub(
        2 + 1 + 2 + max_id_len as usize + max_type_len as usize + max_status_len as usize + max_age_len as usize + 4,
    );

    let rows: Vec<Row> = issue_data
        .iter()
        .map(|(issue, issue_type, clean_title, age)| {
            let state_color = state_color(&issue.state, theme);
            let source_icon = infer_source_icon(&issue.issue_identifier);
            let display_title = truncate_with_ellipsis(clean_title, title_max_width);
            let type_color = type_color(issue_type, theme);

            Row::new(vec![
                Cell::from(
                    Line::from(vec![
                        Span::styled(
                            format!("{source_icon}"),
                            Style::default().fg(theme.muted),
                        ),
                        Span::styled(
                            issue.issue_identifier.clone(),
                            Style::default().fg(theme.info),
                        ),
                    ])
                    .alignment(Alignment::Center),
                ),
                Cell::from(Span::styled(
                    display_title,
                    Style::default().fg(theme.foreground),
                )),
                Cell::from(
                    Line::from(Span::styled(
                        issue_type.clone(),
                        Style::default().fg(type_color),
                    ))
                    .alignment(Alignment::Right),
                ),
                Cell::from(
                    Line::from(vec![
                        Span::styled(
                            issue.state.clone(),
                            Style::default().fg(state_color),
                        ),
                        Span::raw(" "),
                    ])
                    .alignment(Alignment::Right),
                ),
                Cell::from(
                    Line::from(vec![
                        Span::styled(
                            age.clone().unwrap_or_default(),
                            Style::default().fg(theme.muted),
                        ),
                        Span::raw(" "),
                    ])
                    .alignment(Alignment::Right),
                ),
            ])
        })
        .collect();

    let selected_style = Style::default()
        .bg(theme.selection)
        .fg(theme.foreground)
        .add_modifier(Modifier::BOLD);

    let count = indices.len();
    let footer_info = selection_info(app.issues_state.selected(), count);
    let sort_label = app.issue_sort.label();

    // Build title with search indicator
    let title_spans = if app.search_active {
        vec![
            Span::styled(" Issues ", Style::default().fg(theme.foreground).add_modifier(Modifier::BOLD)),
            Span::styled("/", Style::default().fg(theme.highlight)),
            Span::styled(&app.search_query, Style::default().fg(theme.foreground)),
            Span::styled("▏", Style::default().fg(theme.highlight)),
        ]
    } else if !app.search_query.is_empty() {
        vec![
            Span::styled(" Issues ", Style::default().fg(theme.foreground).add_modifier(Modifier::BOLD)),
            Span::styled(
                format!("[{}] ", app.search_query),
                Style::default().fg(theme.highlight),
            ),
        ]
    } else {
        vec![Span::styled(
            " Issues ",
            Style::default().fg(theme.foreground).add_modifier(Modifier::BOLD),
        )]
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(max_id_len),
            Constraint::Fill(1),
            Constraint::Length(max_type_len),
            Constraint::Length(max_status_len),
            Constraint::Length(max_age_len),
        ],
    )
    .header(header)
    .row_highlight_style(selected_style)
    .highlight_spacing(HighlightSpacing::Always)
    .highlight_symbol("▸ ")
    .block({
        let mut block = Block::default()
            .title(Line::from(title_spans));
        if app.refresh_requested || snapshot.loading.fetching_issues {
            let spinner =
                BRAILLE_SPINNER[(app.frame_count / 4) as usize % BRAILLE_SPINNER.len()];
            block = block.title(
                Line::from(vec![
                    Span::styled(
                        format!(" {spinner} "),
                        Style::default().fg(theme.highlight),
                    ),
                    Span::styled(
                        "refreshing ",
                        Style::default().fg(theme.muted),
                    ),
                ])
                .right_aligned(),
            );
        }
        block.title_bottom(
                Line::from(vec![
                    Span::styled("─s:", Style::default().fg(theme.muted)),
                    Span::styled(sort_label, Style::default().fg(theme.highlight)),
                    Span::styled(
                        format!(" {footer_info}─"),
                        Style::default().fg(theme.muted),
                    ),
                ])
                .right_aligned(),
            )
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.border))
            .padding(Padding::right(1))
            .style(Style::default().bg(theme.panel))
    });

    frame.render_stateful_widget(table, area, &mut app.issues_state);
    draw_scrollbar(frame, area, count, app.issues_state.selected().unwrap_or(0));
}

/// Extract a type prefix like "[Feature]", "[Bug]", "Feature:", "Bug:" from the title.
/// Returns (type_label, cleaned_title).
fn extract_type_and_title(title: &str) -> (String, String) {
    let trimmed = title.trim();

    // Match [Type]: ... or [Type] ...
    if trimmed.starts_with('[') {
        if let Some(end) = trimmed.find(']') {
            let tag = &trimmed[1..end];
            let rest = trimmed[end + 1..].trim_start_matches(':').trim_start();
            return (normalize_type(tag), rest.to_string());
        }
    }

    // Match Type: ... (first word before colon)
    if let Some(colon_pos) = trimmed.find(':') {
        let candidate = &trimmed[..colon_pos];
        if !candidate.contains(' ') && is_known_type(candidate) {
            let rest = trimmed[colon_pos + 1..].trim_start();
            return (normalize_type(candidate), rest.to_string());
        }
    }

    ("".into(), trimmed.to_string())
}

fn is_known_type(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "feature" | "bug" | "feat" | "fix" | "model" | "chore" | "docs" | "refactor" | "test"
    )
}

fn normalize_type(s: &str) -> String {
    match s.to_ascii_lowercase().as_str() {
        "feat" | "feature" => "feature".into(),
        "bug" | "fix" => "bug".into(),
        "model" => "model".into(),
        "chore" => "chore".into(),
        "docs" => "docs".into(),
        "refactor" => "refac".into(),
        "test" => "test".into(),
        other => other.to_ascii_lowercase(),
    }
}

fn type_color(issue_type: &str, theme: crate::theme::Theme) -> ratatui::style::Color {
    match issue_type {
        "feature" => theme.success,
        "bug" => theme.danger,
        "model" => theme.highlight,
        "chore" => theme.muted,
        "docs" => theme.info,
        "test" => theme.warning,
        _ => theme.muted,
    }
}

pub fn infer_source_icon(identifier: &str) -> &'static str {
    if identifier.starts_with("GH-") || identifier.contains('#') {
        " "
    } else {
        "◆"
    }
}

pub fn state_color(state: &str, theme: crate::theme::Theme) -> ratatui::style::Color {
    match state.to_ascii_lowercase().as_str() {
        "open" | "in progress" | "started" | "in_progress" => theme.success,
        "todo" | "unstarted" | "backlog" => theme.info,
        "closed" | "done" | "completed" | "cancelled" | "canceled" => theme.muted,
        _ => theme.foreground,
    }
}

fn selection_info(selected: Option<usize>, total: usize) -> String {
    if total == 0 {
        return "empty".into();
    }
    format!("{} of {total}", selected.unwrap_or_default() + 1)
}

fn truncate_with_ellipsis(s: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if s.len() <= max_width {
        return s.to_string();
    }
    let end = max_width.saturating_sub(1);
    // Find a valid char boundary
    let end = s.floor_char_boundary(end);
    format!("{}…", &s[..end])
}

fn format_relative_time(dt: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = now.signed_duration_since(dt).num_seconds().max(0) as u64;
    if secs < 60 {
        "now".into()
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else if secs < 604800 {
        format!("{}d", secs / 86400)
    } else if secs < 2_592_000 {
        format!("{}w", secs / 604800)
    } else if secs < 31_536_000 {
        format!("{}mo", secs / 2_592_000)
    } else {
        format!("{}y", secs / 31_536_000)
    }
}

fn draw_scrollbar(frame: &mut ratatui::Frame<'_>, area: Rect, count: usize, position: usize) {
    let content_height = area.height.saturating_sub(3) as usize;
    if count > content_height {
        let mut scrollbar_state = ScrollbarState::new(count)
            .position(position)
            .viewport_content_length(content_height);
        // Render inside the border so it doesn't overwrite the rounded corner
        let scrollbar_area = Rect {
            x: area.x,
            y: area.y + 1,
            width: area.width,
            height: area.height.saturating_sub(2),
        };
        frame.render_stateful_widget(
            Scrollbar::default().orientation(ScrollbarOrientation::VerticalRight),
            scrollbar_area,
            &mut scrollbar_state,
        );
    }
}
