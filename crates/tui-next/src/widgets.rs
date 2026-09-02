use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Padding, Paragraph, Widget, Wrap},
};

use crate::theme;

#[derive(Clone, Copy, Debug)]
pub(crate) struct LeftBorderPanel {
    border_color: Color,
    content_bg: Option<Color>,
    padding: Padding,
}

impl LeftBorderPanel {
    pub(crate) const fn new() -> Self {
        Self {
            border_color: Color::Reset,
            content_bg: None,
            padding: Padding::ZERO,
        }
    }

    #[must_use]
    pub(crate) const fn border_color(mut self, color: Color) -> Self {
        self.border_color = color;
        self
    }

    #[must_use]
    pub(crate) const fn content_bg(mut self, color: Color) -> Self {
        self.content_bg = Some(color);
        self
    }

    #[must_use]
    pub(crate) const fn padding(mut self, padding: Padding) -> Self {
        self.padding = padding;
        self
    }

    pub(crate) fn render(self, area: Rect, buf: &mut Buffer) -> Rect {
        let border_block = Block::new()
            .borders(Borders::LEFT)
            .border_type(BorderType::Thick)
            .border_style(Style::new().fg(self.border_color));

        let after_border = border_block.inner(area);
        border_block.render(area, buf);

        let mut content_block = Block::new().padding(self.padding);
        if let Some(bg) = self.content_bg {
            content_block = content_block.style(Style::new().bg(bg));
        }

        let inner = content_block.inner(after_border);
        content_block.render(after_border, buf);

        inner
    }
}

pub(crate) struct LeftRailPanel {
    lines: Vec<Line<'static>>,
    max_height: Option<u16>,
    timestamp_label: Option<String>,
    bg: Color,
    border_color: Color,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct InputBottomPanel<'a> {
    input: &'a str,
    focused: bool,
    cursor_visible: bool,
    blink_on: bool,
    border_color: Color,
    content_bg: Color,
    text_color: Color,
    muted_color: Color,
    label_accent: Color,
    bottom_half_bg: Color,
    padding: Padding,
    label_mode: &'a str,
    label_model: &'a str,
    label_provider: &'a str,
}

impl<'a> InputBottomPanel<'a> {
    pub(crate) const fn new(input: &'a str) -> Self {
        Self {
            input,
            focused: false,
            cursor_visible: true,
            blink_on: false,
            border_color: Color::Reset,
            content_bg: Color::Reset,
            text_color: Color::Reset,
            muted_color: Color::Reset,
            label_accent: Color::Reset,
            bottom_half_bg: Color::Reset,
            padding: Padding::ZERO,
            label_mode: "",
            label_model: "",
            label_provider: "",
        }
    }

    #[must_use]
    pub(crate) const fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    #[must_use]
    pub(crate) const fn cursor_visible(mut self, cursor_visible: bool) -> Self {
        self.cursor_visible = cursor_visible;
        self
    }

    #[must_use]
    pub(crate) const fn blink_on(mut self, blink_on: bool) -> Self {
        self.blink_on = blink_on;
        self
    }

    #[must_use]
    pub(crate) const fn border_color(mut self, color: Color) -> Self {
        self.border_color = color;
        self
    }

    #[must_use]
    pub(crate) const fn content_bg(mut self, color: Color) -> Self {
        self.content_bg = color;
        self
    }

    #[must_use]
    pub(crate) const fn text_color(mut self, color: Color) -> Self {
        self.text_color = color;
        self
    }

    #[must_use]
    pub(crate) const fn muted_color(mut self, color: Color) -> Self {
        self.muted_color = color;
        self
    }

    #[must_use]
    pub(crate) const fn label_accent(mut self, color: Color) -> Self {
        self.label_accent = color;
        self
    }

    #[must_use]
    pub(crate) const fn bottom_half_bg(mut self, color: Color) -> Self {
        self.bottom_half_bg = color;
        self
    }

    #[must_use]
    pub(crate) const fn padding(mut self, padding: Padding) -> Self {
        self.padding = padding;
        self
    }

    #[must_use]
    pub(crate) const fn labels(mut self, mode: &'a str, model: &'a str, provider: &'a str) -> Self {
        self.label_mode = mode;
        self.label_model = model;
        self.label_provider = provider;
        self
    }

    pub(crate) fn render(self, area: Rect, buf: &mut Buffer) {
        let inner = LeftBorderPanel::new()
            .border_color(self.border_color)
            .content_bg(self.content_bg)
            .padding(self.padding)
            .render(area, buf);
        if inner.is_empty() {
            return;
        }

        Paragraph::new(self.input_lines())
            .wrap(Wrap { trim: false })
            .render(inner, buf);

        if inner.height >= 3 {
            let label_area = Rect::new(inner.x, inner.y + inner.height - 2, inner.width, 1);
            Paragraph::new(self.label_line()).render(label_area, buf);

            let half_space_y = inner.y + inner.height - 1;
            let content_x = area.x.saturating_add(1);
            for x in content_x..area.x + area.width {
                if let Some(cell) = buf.cell_mut((x, half_space_y)) {
                    cell.set_symbol("▀")
                        .set_style(Style::new().fg(self.content_bg).bg(self.bottom_half_bg));
                }
            }
            if let Some(cell) = buf.cell_mut((area.x, half_space_y)) {
                cell.set_symbol("╹")
                    .set_style(Style::new().fg(self.border_color).bg(self.bottom_half_bg));
            }
        }
    }

    fn input_lines(self) -> Vec<Line<'a>> {
        let mut lines = self
            .input
            .split('\n')
            .map(|line| Line::styled(line, Style::new().fg(self.text_color)))
            .collect::<Vec<_>>();
        if !self.cursor_visible {
            return lines;
        }
        let cursor = match (self.focused, self.blink_on) {
            (true, true) => "█",
            (true, false) => " ",
            (false, _) => "▒",
        };
        let cursor_style = match self.focused {
            true => Style::new().fg(self.text_color),
            false => Style::new().fg(self.muted_color),
        };
        match lines.last_mut() {
            Some(last) => last.spans.push(Span::styled(cursor, cursor_style)),
            None => lines.push(Line::styled(cursor, cursor_style)),
        }

        lines
    }

    fn label_line(self) -> Line<'a> {
        Line::from(vec![
            Span::styled(self.label_mode, Style::new().fg(self.label_accent)),
            Span::styled(" · ", Style::new().fg(self.muted_color)),
            Span::styled(self.label_model, Style::new().fg(self.text_color)),
            Span::styled(" ", Style::new().fg(self.muted_color)),
            Span::styled(self.label_provider, Style::new().fg(self.muted_color)),
        ])
    }
}

impl LeftRailPanel {
    const CONTENT_LEFT: u16 = 3;
    const CONTENT_RIGHT: u16 = 1;

    pub(crate) fn new(lines: Vec<Line<'static>>) -> Self {
        Self {
            lines,
            max_height: None,
            timestamp_label: None,
            bg: theme::element(),
            border_color: theme::border(),
        }
    }

    pub(crate) fn max_height(mut self, max_height: u16) -> Self {
        self.max_height = Some(max_height);
        self
    }

    pub(crate) fn border_color(mut self, border_color: Color) -> Self {
        self.border_color = border_color;
        self
    }

    pub(crate) fn timestamp_label(mut self, label: Option<String>) -> Self {
        self.timestamp_label = label;
        self
    }

    pub(crate) fn height(&self, width: u16) -> u16 {
        let content_width = width.saturating_sub(4).max(1) as usize;
        let content_height = self
            .lines
            .iter()
            .map(|line| line.width().div_ceil(content_width).max(1) as u16)
            .sum::<u16>()
            .max(1);
        content_height + 2
    }

    pub(crate) fn plain_height(&self, width: u16) -> u16 {
        let content_width = width
            .saturating_sub(Self::CONTENT_LEFT + Self::CONTENT_RIGHT)
            .max(1) as usize;
        let height = self
            .lines
            .iter()
            .map(|line| line.width().div_ceil(content_width).max(1) as u16)
            .sum::<u16>()
            .max(1);
        height + u16::from(self.timestamp_label.is_some())
    }

    pub(crate) fn render_plain(&self, area: Rect, bg: Color, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_style(Style::new().bg(bg));
                }
            }
        }

        Paragraph::new(Text::from(self.lines.clone()))
            .wrap(Wrap { trim: false })
            .style(Style::new().bg(bg))
            .render(self.plain_content_area(area), buf);
        self.render_timestamp(area, bg, buf);
    }

    pub(crate) fn render_plain_with_rail(&self, area: Rect, bg: Color, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_style(Style::new().bg(bg));
                }
            }
            if let Some(cell) = buf.cell_mut((area.x, y)) {
                cell.set_symbol("▕")
                    .set_style(Style::new().fg(self.border_color).bg(bg));
            }
        }

        Paragraph::new(Text::from(self.lines.clone()))
            .wrap(Wrap { trim: false })
            .style(Style::new().bg(bg))
            .render(self.plain_content_area(area), buf);
        self.render_timestamp(area, bg, buf);
    }

    fn plain_content_area(&self, area: Rect) -> Rect {
        Rect::new(
            area.x.saturating_add(Self::CONTENT_LEFT),
            area.y,
            area.width
                .saturating_sub(Self::CONTENT_LEFT + Self::CONTENT_RIGHT),
            area.height
                .saturating_sub(u16::from(self.timestamp_label.is_some())),
        )
    }

    fn render_timestamp(&self, area: Rect, bg: Color, buf: &mut Buffer) {
        let Some(label) = self.timestamp_label.as_deref() else {
            return;
        };
        if area.is_empty() {
            return;
        }
        let width = label.chars().count() as u16;
        let x = area
            .x
            .saturating_add(area.width.saturating_sub(width.saturating_add(1)));
        let y = area.y.saturating_add(area.height.saturating_sub(1));
        Line::styled(
            label.to_string(),
            Style::new().fg(theme::secondary()).bg(bg),
        )
        .render(Rect::new(x, y, width.min(area.width), 1), buf);
    }

    pub(crate) fn visible_height(&self, width: u16) -> u16 {
        let height = self.height(width);
        height.min(self.max_height.unwrap_or(height)).max(3)
    }

    pub(crate) fn render_clipped(&self, area: Rect, skip_rows: u16, buf: &mut Buffer) {
        self.render_clipped_with_bg(area, skip_rows, self.bg, buf);
    }

    pub(crate) fn render_clipped_with_bg(
        &self,
        area: Rect,
        skip_rows: u16,
        bg: Color,
        buf: &mut Buffer,
    ) {
        if area.is_empty() {
            return;
        }

        let inner = LeftBorderPanel::new()
            .border_color(self.border_color)
            .content_bg(bg)
            .padding(Padding::new(2, 1, 1, 1))
            .render(area, buf);

        if inner.is_empty() {
            return;
        }

        let content_skip = skip_rows.saturating_sub(1) as usize;
        let lines = self
            .lines
            .iter()
            .skip(content_skip)
            .cloned()
            .collect::<Vec<_>>();

        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .render(inner, buf);
        self.render_timestamp(area, bg, buf);
    }
}

impl Widget for &LeftRailPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.render_clipped(area, 0, buf);
    }
}
