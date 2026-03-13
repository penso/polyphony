use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use {
    crossterm::{
        event::{self, Event, KeyCode},
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    },
    polyphony_core::RuntimeSnapshot,
    polyphony_orchestrator::RuntimeCommand,
    ratatui::{
        Terminal,
        backend::CrosstermBackend,
        layout::{Constraint, Direction, Layout, Rect},
        style::{Modifier, Style},
        text::Line,
        widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    },
    thiserror::Error,
    tokio::sync::{mpsc, watch},
};

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone, Debug)]
pub struct LogBuffer {
    lines: Arc<Mutex<VecDeque<String>>>,
    max_lines: usize,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::with_capacity(128)
    }
}

impl LogBuffer {
    pub fn with_capacity(max_lines: usize) -> Self {
        Self {
            lines: Arc::new(Mutex::new(VecDeque::with_capacity(max_lines))),
            max_lines,
        }
    }

    pub fn push_line(&self, line: impl Into<String>) {
        let line = line.into();
        if line.trim().is_empty() {
            return;
        }
        let mut lines = lock_or_recover(&self.lines);
        lines.push_front(line);
        while lines.len() > self.max_lines {
            lines.pop_back();
        }
    }

    pub fn recent_lines(&self, limit: usize) -> Vec<String> {
        lock_or_recover(&self.lines)
            .iter()
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn drain_oldest_first(&self) -> Vec<String> {
        let mut lines = lock_or_recover(&self.lines);
        let drained = lines.drain(..).collect::<Vec<_>>();
        drained.into_iter().rev().collect()
    }
}

pub async fn run(
    mut snapshot_rx: watch::Receiver<RuntimeSnapshot>,
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
    log_buffer: LogBuffer,
) -> Result<(), Error> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = loop {
        let snapshot = snapshot_rx.borrow().clone();
        terminal.draw(|frame| draw(frame, &snapshot, &log_buffer))?;

        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
        {
            match key.code {
                KeyCode::Char('q') => break Ok(()),
                KeyCode::Char('r') => {
                    let _ = command_tx.send(RuntimeCommand::Refresh);
                },
                _ => {},
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

fn draw(frame: &mut ratatui::Frame<'_>, snapshot: &RuntimeSnapshot, log_buffer: &LogBuffer) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(5),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(7),
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
    let running = Table::new(running_rows, [
        Constraint::Length(14),
        Constraint::Length(14),
        Constraint::Length(8),
        Constraint::Length(18),
        Constraint::Min(20),
    ])
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
    let retrying = Table::new(retry_rows, [
        Constraint::Length(14),
        Constraint::Length(8),
        Constraint::Length(12),
        Constraint::Min(20),
    ])
    .header(
        Row::new(["Issue", "Attempt", "Due", "Error"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(Block::default().title("Retry Queue").borders(Borders::ALL));
    frame.render_widget(retrying, areas[3]);

    let catalogs = snapshot
        .agent_catalogs
        .iter()
        .take(4)
        .map(|catalog| {
            let selected = catalog
                .selected_model
                .clone()
                .unwrap_or_else(|| "auto".into());
            let discovered = catalog.models.len();
            Line::from(format!(
                "{} [{}] selected={} discovered={}",
                catalog.agent_name, catalog.provider_kind, selected, discovered
            ))
        })
        .collect::<Vec<_>>();
    let agent_models = Paragraph::new(catalogs)
        .block(Block::default().title("Agent Models").borders(Borders::ALL));
    frame.render_widget(agent_models, areas[4]);

    let bottom = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(areas[5]);
    draw_recent_events(frame, bottom[0], snapshot);
    draw_logs(frame, bottom[1], log_buffer);
}

fn draw_recent_events(frame: &mut ratatui::Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
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
    frame.render_widget(events, area);
}

fn draw_logs(frame: &mut ratatui::Frame<'_>, area: Rect, log_buffer: &LogBuffer) {
    let lines = log_buffer
        .recent_lines(12)
        .into_iter()
        .map(Line::from)
        .collect::<Vec<_>>();
    let logs = Paragraph::new(lines).block(Block::default().title("Logs").borders(Borders::ALL));
    frame.render_widget(logs, area);
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(test)]
mod tests {
    use super::LogBuffer;

    #[test]
    fn log_buffer_keeps_newest_entries_within_capacity() {
        let buffer = LogBuffer::with_capacity(2);

        buffer.push_line("one");
        buffer.push_line("two");
        buffer.push_line("three");

        assert_eq!(buffer.recent_lines(3), vec!["three", "two"]);
    }

    #[test]
    fn log_buffer_drains_in_oldest_first_order() {
        let buffer = LogBuffer::with_capacity(3);

        buffer.push_line("one");
        buffer.push_line("two");
        buffer.push_line("three");

        assert_eq!(buffer.drain_oldest_first(), vec!["one", "two", "three"]);
        assert!(buffer.recent_lines(1).is_empty());
    }
}
