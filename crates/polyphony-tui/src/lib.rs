use std::time::Duration;

use crossterm::event::{self, Event, KeyCode};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use polyphony_core::RuntimeSnapshot;
use polyphony_orchestrator::RuntimeCommand;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use thiserror::Error;
use tokio::sync::{mpsc, watch};

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub async fn run(
    mut snapshot_rx: watch::Receiver<RuntimeSnapshot>,
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
) -> Result<(), Error> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = loop {
        let snapshot = snapshot_rx.borrow().clone();
        terminal.draw(|frame| draw(frame, &snapshot))?;

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => break Ok(()),
                    KeyCode::Char('r') => {
                        let _ = command_tx.send(RuntimeCommand::Refresh);
                    }
                    _ => {}
                }
            }
        }

        tokio::select! {
            changed = snapshot_rx.changed() => {
                if changed.is_err() {
                    break Ok(());
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    };

    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn draw(frame: &mut ratatui::Frame<'_>, snapshot: &RuntimeSnapshot) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(5),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Min(8),
        ])
        .split(frame.area());

    let summary = Paragraph::new(format!(
        "running={} retrying={} input={} output={} total={} runtime={:.1}s | q quit | r refresh",
        snapshot.counts.running,
        snapshot.counts.retrying,
        snapshot.codex_totals.input_tokens,
        snapshot.codex_totals.output_tokens,
        snapshot.codex_totals.total_tokens,
        snapshot.codex_totals.seconds_running,
    ))
    .block(Block::default().title("Polyphony").borders(Borders::ALL));
    frame.render_widget(summary, areas[0]);

    let budget_lines = snapshot
        .budgets
        .iter()
        .map(|budget| {
            let credits = budget
                .credits_remaining
                .map(|value| format!("credits={value:.2}"))
                .unwrap_or_else(|| "credits=n/a".into());
            let spent = budget
                .spent_usd
                .map(|value| format!("spent=${value:.2}"))
                .unwrap_or_else(|| "spent=n/a".into());
            Line::from(format!("{} {} {}", budget.component, credits, spent))
        })
        .chain(snapshot.throttles.iter().map(|throttle| {
            Line::from(format!(
                "{} throttled until {} ({})",
                throttle.component,
                throttle.until.format("%H:%M:%S"),
                throttle.reason
            ))
        }))
        .take(4)
        .collect::<Vec<_>>();
    let budgets = Paragraph::new(budget_lines).block(
        Block::default()
            .title("Budgets And Throttles")
            .borders(Borders::ALL),
    );
    frame.render_widget(budgets, areas[1]);

    let running_rows = snapshot.running.iter().map(|running| {
        Row::new([
            Cell::from(running.issue_identifier.clone()),
            Cell::from(running.state.clone()),
            Cell::from(running.turn_count.to_string()),
            Cell::from(running.last_event.clone().unwrap_or_default()),
            Cell::from(running.last_message.clone().unwrap_or_default()),
        ])
    });
    let running = Table::new(
        running_rows,
        [
            Constraint::Length(14),
            Constraint::Length(14),
            Constraint::Length(8),
            Constraint::Length(18),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(["Issue", "State", "Turns", "Last Event", "Message"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(Block::default().title("Running").borders(Borders::ALL));
    frame.render_widget(running, areas[2]);

    let retry_rows = snapshot.retrying.iter().map(|retry| {
        Row::new([
            Cell::from(retry.issue_identifier.clone()),
            Cell::from(retry.attempt.to_string()),
            Cell::from(retry.due_at.format("%H:%M:%S").to_string()),
            Cell::from(retry.error.clone().unwrap_or_default()),
        ])
    });
    let retrying = Table::new(
        retry_rows,
        [
            Constraint::Length(14),
            Constraint::Length(8),
            Constraint::Length(12),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(["Issue", "Attempt", "Due", "Error"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(Block::default().title("Retry Queue").borders(Borders::ALL));
    frame.render_widget(retrying, areas[3]);

    let lines = snapshot
        .recent_events
        .iter()
        .take(12)
        .map(|event| {
            Line::from(format!(
                "{} [{}] {}",
                event.at.format("%H:%M:%S"),
                event.scope,
                event.message
            ))
        })
        .collect::<Vec<_>>();
    let events = Paragraph::new(lines).block(
        Block::default()
            .title("Recent Events")
            .borders(Borders::ALL),
    );
    frame.render_widget(events, areas[4]);
}
