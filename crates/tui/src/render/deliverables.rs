use polyphony_core::{
    Deliverable, DeliverableDecision, DeliverableKind, DeliverableStatus, RuntimeSnapshot,
};
use ratatui::{
    layout::{Alignment, Constraint, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Cell, HighlightSpacing, Row, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Table,
    },
};

use crate::app::AppState;

pub fn draw_deliverables_tab(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;

    // Collect and sort oldest first (newest at bottom)
    let mut deliverables: Vec<_> = snapshot
        .runs
        .iter()
        .filter(|run| run.deliverable.is_some())
        .collect();
    deliverables.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    let header = Row::new(vec![
        Cell::from(Span::styled("", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Title", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Output", Style::default().fg(theme.muted))),
        Cell::from(
            Line::from(Span::styled("PR", Style::default().fg(theme.muted)))
                .alignment(Alignment::Right),
        ),
        Cell::from(
            Line::from(Span::styled("Dec", Style::default().fg(theme.muted)))
                .alignment(Alignment::Right),
        ),
    ])
    .height(1)
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = deliverables
        .iter()
        .map(|run| {
            let deliverable = run.deliverable.as_ref().expect("filtered");
            let (status_icon, status_color) = status_indicator(deliverable.status, theme);
            let (decision_icon, decision_color) = decision_indicator(deliverable.decision, theme);
            Row::new(vec![
                Cell::from(Span::styled(
                    super::format_listing_time(run.created_at),
                    Style::default().fg(theme.muted),
                )),
                Cell::from(Span::styled(
                    run.title.clone(),
                    Style::default().fg(theme.foreground),
                )),
                Cell::from(Span::styled(
                    output_label(deliverable),
                    Style::default().fg(theme.foreground),
                )),
                Cell::from(
                    Line::from(Span::styled(status_icon, Style::default().fg(status_color)))
                        .alignment(Alignment::Right),
                ),
                Cell::from(
                    Line::from(Span::styled(
                        decision_icon,
                        Style::default().fg(decision_color),
                    ))
                    .alignment(Alignment::Right),
                ),
            ])
        })
        .collect();

    let selected_style = Style::default().add_modifier(Modifier::BOLD);

    let count = deliverables.len();
    let footer_info = if count == 0 {
        "no deliverables".into()
    } else {
        format!(
            "{} of {count}",
            app.deliverables_state.selected().unwrap_or_default() + 1
        )
    };

    let table = Table::new(rows, [
        Constraint::Length(16),
        Constraint::Fill(1),
        Constraint::Length(16),
        Constraint::Length(4),
        Constraint::Length(4),
    ])
    .header(header)
    .row_highlight_style(selected_style)
    .highlight_symbol("▶ ")
    .highlight_spacing(HighlightSpacing::WhenSelected)
    .block(
        Block::default()
            .title(Line::from(Span::styled(
                " Outcomes ",
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD),
            )))
            .title_bottom(
                Line::from(vec![
                    Span::styled(footer_info, Style::default().fg(theme.muted)),
                    Span::styled(" • ", Style::default().fg(theme.border)),
                    Span::styled("review queue", Style::default().fg(theme.highlight)),
                ])
                .right_aligned(),
            )
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(if app.list_border_focused {
                theme.highlight
            } else {
                theme.border
            }))
            .style(Style::default().bg(theme.panel)),
    );

    frame.render_stateful_widget(table, area, &mut app.deliverables_state);

    if count > 0 {
        let content_height = area.height.saturating_sub(3) as usize;
        if count > content_height {
            let mut scrollbar_state = ScrollbarState::new(count)
                .position(app.deliverables_state.selected().unwrap_or(0))
                .viewport_content_length(content_height);
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
}

fn flow_label(run: &polyphony_core::RunRow) -> String {
    run.review_target
        .as_ref()
        .map(|target| format!("{}#{}", target.repository, target.number))
        .or_else(|| run.issue_identifier.clone())
        .unwrap_or_else(|| run.id.clone())
}

pub(crate) fn flow_label_pub(run: &polyphony_core::RunRow) -> String {
    flow_label(run)
}

fn output_label(deliverable: &Deliverable) -> String {
    match deliverable.kind {
        DeliverableKind::GithubPullRequest => deliverable
            .url
            .as_deref()
            .and_then(github_pull_request_number)
            .map(|number| format!("PR #{number}"))
            .unwrap_or_else(|| "PR".into()),
        DeliverableKind::GitlabMergeRequest => "MR".into(),
        DeliverableKind::LocalBranch => deliverable
            .metadata
            .get("branch")
            .and_then(|v| v.as_str())
            .map(|b| format!("branch:{b}"))
            .unwrap_or_else(|| "branch".into()),
        DeliverableKind::Patch => "Patch".into(),
        DeliverableKind::PullRequestReview => {
            deliverable.title.as_deref().unwrap_or("Review").to_string()
        },
    }
}

fn deliverable_label(deliverable: &Deliverable) -> String {
    let base = output_label(deliverable);
    format!("{base} ({})", deliverable.status)
}

pub(crate) fn deliverable_label_pub(deliverable: &Deliverable) -> String {
    deliverable_label(deliverable)
}

fn github_pull_request_number(url: &str) -> Option<&str> {
    url.rsplit("/pull/").next().filter(|suffix| *suffix != url)
}

fn status_indicator(
    status: DeliverableStatus,
    theme: crate::theme::Theme,
) -> (&'static str, ratatui::style::Color) {
    match status {
        DeliverableStatus::Pending => ("…", theme.info),
        DeliverableStatus::Open => ("●", theme.success),
        DeliverableStatus::Merged => ("✓", theme.highlight),
        DeliverableStatus::Closed => ("✕", theme.muted),
        DeliverableStatus::Reviewed => ("✓", theme.success),
    }
}

fn decision_indicator(
    decision: DeliverableDecision,
    theme: crate::theme::Theme,
) -> (&'static str, ratatui::style::Color) {
    match decision {
        DeliverableDecision::Waiting => ("◷", theme.warning),
        DeliverableDecision::Accepted => ("✓", theme.success),
        DeliverableDecision::Rejected => ("✕", theme.danger),
    }
}

#[cfg(test)]
mod tests {
    use polyphony_core::{Deliverable, DeliverableDecision, DeliverableKind, DeliverableStatus};

    #[test]
    fn deliverable_label_uses_pr_number_when_available() {
        let deliverable = Deliverable {
            kind: DeliverableKind::GithubPullRequest,
            status: DeliverableStatus::Open,
            url: Some("https://github.com/penso/polyphony/pull/8".into()),
            decision: DeliverableDecision::Waiting,
            title: None,
            description: None,
            metadata: Default::default(),
        };

        assert_eq!(
            crate::render::deliverables::deliverable_label(&deliverable),
            "PR #8 (open)"
        );
    }
}
