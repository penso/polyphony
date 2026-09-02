use ratatui::layout::Rect;
use ratatui_opentui_loader::KittLoader;

use crate::settings::TuiSettings;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Route {
    #[default]
    Inbox,
    Detail,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum DetailInputMode {
    #[default]
    None,
    Hijack,
}

#[derive(Clone, Debug)]
pub(crate) struct IssueIntervention {
    pub issue_id: String,
    pub run_id: Option<String>,
    pub prompt: String,
}

#[derive(Clone, Debug)]
pub(crate) struct IssueNotice {
    pub issue_id: String,
    pub message: String,
}

#[derive(Clone, Debug)]
pub(crate) struct CreateIssueBackendOption {
    pub label: String,
    pub repo_id: Option<String>,
    pub tracker_source: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CreateIssueModalState {
    pub title: String,
    pub description: String,
    pub backend_options: Vec<CreateIssueBackendOption>,
    pub selected_backend: usize,
    pub focused_field: u8,
    pub cursor: usize,
}

impl CreateIssueModalState {
    pub(crate) const BACKEND_FIELD: u8 = 1;
    pub(crate) const DESCRIPTION_FIELD: u8 = 2;
    pub(crate) const TITLE_FIELD: u8 = 0;

    pub(crate) fn selected_backend(&self) -> Option<&CreateIssueBackendOption> {
        self.backend_options.get(self.selected_backend)
    }

    pub(crate) fn focus_next(&mut self) {
        self.focused_field = match self.focused_field {
            Self::TITLE_FIELD => Self::BACKEND_FIELD,
            Self::BACKEND_FIELD => Self::DESCRIPTION_FIELD,
            _ => Self::TITLE_FIELD,
        };
        self.clamp_cursor();
    }

    pub(crate) fn select_backend_next(&mut self) {
        if !self.backend_options.is_empty() {
            self.selected_backend = (self.selected_backend + 1) % self.backend_options.len();
        }
    }

    pub(crate) fn select_backend_previous(&mut self) {
        if self.backend_options.is_empty() {
            return;
        }
        self.selected_backend = if self.selected_backend == 0 {
            self.backend_options.len() - 1
        } else {
            self.selected_backend - 1
        };
    }

    pub(crate) fn insert_char(&mut self, c: char) {
        if self.focused_field == Self::BACKEND_FIELD {
            return;
        }
        let cursor = self.cursor;
        let text = self.focused_text_mut();
        let index = char_to_byte_index(text, cursor);
        text.insert(index, c);
        self.cursor += 1;
    }

    pub(crate) fn insert_newline(&mut self) {
        if self.focused_field == Self::DESCRIPTION_FIELD {
            self.insert_char('\n');
        }
    }

    pub(crate) fn backspace(&mut self) {
        if self.focused_field == Self::BACKEND_FIELD || self.cursor == 0 {
            return;
        }
        let cursor = self.cursor;
        let text = self.focused_text_mut();
        *text = text
            .chars()
            .take(cursor - 1)
            .chain(text.chars().skip(cursor))
            .collect();
        self.cursor -= 1;
    }

    pub(crate) fn move_left(&mut self) {
        if self.focused_field == Self::BACKEND_FIELD {
            self.select_backend_previous();
        } else {
            self.cursor = self.cursor.saturating_sub(1);
        }
    }

    pub(crate) fn move_right(&mut self) {
        if self.focused_field == Self::BACKEND_FIELD {
            self.select_backend_next();
        } else {
            self.cursor = (self.cursor + 1).min(self.focused_text().chars().count());
        }
    }

    pub(crate) fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub(crate) fn move_end(&mut self) {
        self.cursor = self.focused_text().chars().count();
    }

    pub(crate) fn focused_text(&self) -> &str {
        match self.focused_field {
            Self::TITLE_FIELD => &self.title,
            Self::DESCRIPTION_FIELD => &self.description,
            _ => "",
        }
    }

    fn focused_text_mut(&mut self) -> &mut String {
        match self.focused_field {
            Self::TITLE_FIELD => &mut self.title,
            Self::DESCRIPTION_FIELD => &mut self.description,
            _ => &mut self.title,
        }
    }

    fn clamp_cursor(&mut self) {
        self.cursor = self.cursor.min(self.focused_text().chars().count());
    }
}

fn char_to_byte_index(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .map(|(index, _)| index)
        .nth(char_index)
        .unwrap_or(text.len())
}

#[derive(Default)]
pub(crate) struct AppState {
    pub route: Route,
    pub selected: usize,
    pub scroll: usize,
    pub detail_scroll: u16,
    pub detail_follow_bottom: bool,
    pub detail_scroll_max: u16,
    pub detail_scrollbar_rect: Rect,
    pub detail_scrollbar_active: bool,
    pub visible_rows: usize,
    pub list_rect: Rect,
    pub search_query: String,
    pub input: String,
    pub detail_input_mode: DetailInputMode,
    pub status_message: Option<String>,
    pub interventions: Vec<IssueIntervention>,
    pub notices: Vec<IssueNotice>,
    pub tick: u32,
    pub command_palette_open: bool,
    pub command_query: String,
    pub command_selected: usize,
    pub command_scroll: usize,
    pub dispatch_mode_picker_open: bool,
    pub dispatch_mode_selected: usize,
    pub children_expanded: bool,
    pub children_expand_rect: Rect,
    pub workspace_path_rect: Rect,
    pub workspace_path_to_copy: Option<String>,
    pub toast_message: Option<String>,
    pub toast_until_tick: u32,
    pub session_text_rect: Rect,
    pub session_visible_lines: Vec<String>,
    pub session_selection_start: Option<(u16, u16)>,
    pub session_selection_end: Option<(u16, u16)>,
    pub session_selecting: bool,
    pub sidebar_text_rect: Rect,
    pub sidebar_visible_lines: Vec<String>,
    pub sidebar_selection_start: Option<(u16, u16)>,
    pub sidebar_selection_end: Option<(u16, u16)>,
    pub sidebar_selecting: bool,
    pub create_issue_modal: Option<CreateIssueModalState>,
    pub settings: TuiSettings,
    pub loader: KittLoader,
    pub mouse_pos: Option<(u16, u16)>,
}

pub(crate) fn clamp_selection(app: &mut AppState, len: usize) {
    app.selected = app.selected.min(len.saturating_sub(1));
}
