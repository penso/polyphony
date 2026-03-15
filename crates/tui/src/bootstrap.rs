use crate::{prelude::*, *};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BootstrapChoice {
    Create,
    Cancel,
}

impl BootstrapChoice {
    fn toggle(&mut self) {
        *self = match self {
            Self::Create => Self::Cancel,
            Self::Cancel => Self::Create,
        };
    }
}

#[derive(Debug)]
pub(crate) struct BootstrapState {
    pub(crate) choice: BootstrapChoice,
}

impl Default for BootstrapState {
    fn default() -> Self {
        Self {
            choice: BootstrapChoice::Create,
        }
    }
}

impl BootstrapState {
    fn handle_key(&mut self, key: KeyCode) -> Option<bool> {
        match key {
            KeyCode::Enter => Some(self.choice == BootstrapChoice::Create),
            KeyCode::Char('y') | KeyCode::Char('Y') => Some(true),
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
                Some(false)
            },
            KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('h')
            | KeyCode::Char('l') => {
                self.choice.toggle();
                None
            },
            _ => None,
        }
    }
}

pub fn prompt_workflow_initialization(workflow_path: &Path) -> Result<bool, Error> {
    let theme = detect_terminal_theme().unwrap_or_else(default_theme);
    enable_raw_mode()?;
    drain_pending_input();
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut state = BootstrapState::default();

    let result = loop {
        terminal.draw(|frame| draw_workflow_bootstrap(frame, workflow_path, &state, theme))?;

        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
            && let Some(create) = state.handle_key(key.code)
        {
            break Ok(create);
        }
    };

    drain_pending_input();
    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

pub(crate) fn drain_pending_input() {
    while event::poll(Duration::from_millis(10)).unwrap_or(false) {
        let _ = event::read();
    }
}

pub(crate) fn mouse_in_rect(col: u16, row: u16, rect: Rect) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

fn centered_rect(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = area.width.min(max_width).max(1);
    let height = area.height.min(max_height).max(1);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width, height)
}

fn draw_workflow_bootstrap(
    frame: &mut ratatui::Frame<'_>,
    _workflow_path: &Path,
    state: &BootstrapState,
    theme: Theme,
) {
    let shell = centered_rect(frame.area(), 62, 11);
    frame.render_widget(Clear, shell);
    frame.render_widget(
        Block::default()
            .title(Line::from(Span::styled(
                " Initialize Polyphony ",
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            )))
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.highlight)),
        shell,
    );

    let inner = shell.inner(Margin {
        vertical: 1,
        horizontal: 3,
    });

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(2),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .split(inner);

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Creates a WORKFLOW.md file for this repository.",
                Style::default().fg(theme.foreground),
            )),
            Line::from(Span::styled(
                "No existing files will be modified.",
                Style::default().fg(theme.foreground),
            )),
        ])
        .wrap(ratatui::widgets::Wrap { trim: false }),
        rows[1],
    );

    let cancel_style = if state.choice == BootstrapChoice::Cancel {
        Style::default()
            .fg(Color::Black)
            .bg(theme.muted)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.muted)
    };
    let create_style = if state.choice == BootstrapChoice::Create {
        Style::default()
            .fg(Color::Black)
            .bg(theme.highlight)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.foreground)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" Cancel ", cancel_style),
            Span::raw("   "),
            Span::styled(" Initialize ", create_style),
        ]))
        .alignment(Alignment::Right),
        rows[3],
    );
}
