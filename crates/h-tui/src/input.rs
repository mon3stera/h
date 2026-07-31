use std::cell::{Cell, RefCell};

use h_core::input::{Image as InputImage, UserInput};

use ratatui::{
    Frame,
    buffer::Buffer,
    crossterm::event::{
        KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    layout::{Alignment, Constraint, Layout, Rect, Size},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Widget},
};
use ratatui_image::{Image as ImageWidget, Resize, picker::Picker, protocol::Protocol};
use ratatui_textarea::{CursorMove, TextArea, WrapMode};
use unicode_width::UnicodeWidthStr;

const PROMPT_MARKER: &str = "❯ ";
const MARKER_WIDTH: u16 = 2;
/// Kept empty so the cursor remains visible when text fills a visual line.
const CURSOR_GUTTER_WIDTH: u16 = 1;

/// Prompts kept for recall. Old enough entries are not worth the memory.
const HISTORY_LIMIT: usize = 200;

const MIN_HEIGHT: u16 = 3;
const MAX_HEIGHT: u16 = 10;
/// The top and bottom rules.
const BORDER_HEIGHT: u16 = 2;
const ATTACHMENT_GAP: u16 = 2;
const THUMBNAIL_WIDTH: u16 = 12;
const THUMBNAIL_HEIGHT: u16 = 5;
const ATTACHMENT_HEIGHT: u16 = THUMBNAIL_HEIGHT + 1;

use Direction::{Newer, Older};

/// Which way through the history an arrow is asking to go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Older,
    Newer,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Focus {
    #[default]
    Text,
    Image(usize),
}

struct Tile {
    index: usize,
    x: u16,
    width: u16,
}

struct Attachment {
    image: InputImage,
    thumbnail: Option<Protocol>,
}

/// The prompt box.
///
/// A bare Enter inserts a newline; submitting takes a modified one. Ctrl+Enter
/// reaches us only over the kitty keyboard protocol — without it, terminals send
/// a plain carriage return for it, indistinguishable from Enter. Alt+Enter
/// arrives as an ESC-prefixed return, which nearly every terminal sends.
pub struct Input {
    area: TextArea<'static>,
    images: Vec<Attachment>,
    picker: Picker,
    focus: Focus,
    /// Prompts already sent, oldest first.
    history: Vec<String>,
    /// Which entry the box is showing. `None` means it is showing the draft.
    recalled: Option<usize>,
    /// The unsent text set aside while browsing, so leaving the history returns
    /// whatever was half-typed.
    draft: String,
    /// Field width and the wrapped rows measured for it.
    wrapped: Cell<Option<(u16, u16)>>,
    /// First visual row shown by the textarea on its last render.
    scroll_top: Cell<u16>,
    /// Close-button hit boxes from the latest render.
    close_areas: RefCell<Vec<(Rect, usize)>>,
}

impl Default for Input {
    fn default() -> Self {
        Self::new(Picker::halfblocks())
    }
}

impl Input {
    pub(crate) fn new(picker: Picker) -> Self {
        let mut area = TextArea::default();

        area.set_cursor_line_style(Style::default());
        area.set_wrap_mode(WrapMode::WordOrGlyph);

        Self {
            area,
            images: Vec::new(),
            picker,
            focus: Focus::Text,
            history: Vec::new(),
            recalled: None,
            draft: String::new(),
            wrapped: Cell::new(None),
            scroll_top: Cell::new(0),
            close_areas: RefCell::new(Vec::new()),
        }
    }

    /// Takes a key. A submitted prompt is handed back and the box is cleared.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<UserInput> {
        if key.kind == KeyEventKind::Release {
            return None;
        }

        if key.code == KeyCode::Enter
            && key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return self.take();
        }

        if let Focus::Image(index) = self.focus {
            match key.code {
                KeyCode::Left => self.focus = Focus::Image(index.saturating_sub(1)),
                KeyCode::Right => {
                    self.focus = Focus::Image((index + 1).min(self.images.len() - 1));
                }
                KeyCode::Backspace | KeyCode::Delete => self.remove_image(index),
                KeyCode::Esc | KeyCode::Tab => self.focus = Focus::Text,
                _ => {}
            }

            return None;
        }

        if matches!(key.code, KeyCode::BackTab)
            || key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT)
        {
            if !self.images.is_empty() {
                self.focus = Focus::Image(0);
            }

            return None;
        }

        if key.code == KeyCode::Backspace && self.text().is_empty() && !self.images.is_empty() {
            self.images.pop();

            return None;
        }

        // Arrows reach the history only from the edges of the text, so a
        // multi-line or soft-wrapped prompt can still be moved around in.
        match key.code {
            KeyCode::Up if self.cursor_at_top() => return self.recall(Older),
            KeyCode::Down if self.cursor_at_bottom() => {
                return self.recall(Newer);
            }
            _ => {}
        }

        if self.area.input(key) {
            self.wrapped.set(None);
        }

        None
    }

    fn cursor_at_top(&self) -> bool {
        self.area.screen_cursor().row == 0
    }

    fn cursor_at_bottom(&self) -> bool {
        let row = self.area.screen_cursor().row;
        let mut end = self.area.clone();

        end.move_cursor(CursorMove::Bottom);
        end.move_cursor(CursorMove::End);

        row == end.screen_cursor().row
    }

    pub(crate) fn take(&mut self) -> Option<UserInput> {
        let (text, images) = (
            self.area.lines().join("\n"),
            std::mem::take(&mut self.images)
                .into_iter()
                .map(|attachment| attachment.image)
                .collect(),
        );
        let input = UserInput::from_text_and_images(text.clone(), images);

        if input.is_empty() {
            return None;
        }

        if !text.trim().is_empty() {
            self.remember(text);
        }
        self.show("");
        self.focus = Focus::Text;
        self.recalled = None;
        self.draft.clear();
        self.close_areas.borrow_mut().clear();

        Some(input)
    }

    /// Fills the history from a resumed session, so what was asked before can be
    /// recalled as if it had just been typed.
    pub fn seed(&mut self, prompts: impl IntoIterator<Item = String>) {
        for prompt in prompts {
            self.remember(prompt);
        }
    }

    /// Files a sent prompt, skipping a repeat of the newest so holding one key
    /// does not fill the history with it.
    fn remember(&mut self, prompt: String) {
        if self.history.last() == Some(&prompt) {
            return;
        }

        if self.history.len() >= HISTORY_LIMIT {
            self.history.remove(0);
        }

        self.history.push(prompt);
    }

    /// Steps through the history, stashing the draft on the way in and putting
    /// it back on the way out.
    fn recall(&mut self, direction: Direction) -> Option<UserInput> {
        let target = match (direction, self.recalled) {
            (Older, None) => {
                self.draft = self.area.lines().join("\n");
                self.history.len().checked_sub(1)
            }
            // Already at the oldest; stay rather than wrap around.
            (Older, Some(index)) => Some(index.saturating_sub(1)),
            (Newer, None) => None,
            (Newer, Some(index)) => Some(index + 1),
        };

        match target.filter(|index| *index < self.history.len()) {
            Some(index) => {
                self.recalled = Some(index);
                let entry = self.history[index].clone();
                self.show(&entry);
            }
            None if self.recalled.is_some() && direction == Newer => {
                // Past the newest entry is the draft again.
                self.recalled = None;
                let draft = std::mem::take(&mut self.draft);
                self.show(&draft);
            }
            None => {}
        }

        None
    }

    /// Replaces the box with `text`, cursor at the end so typing continues it.
    fn show(&mut self, text: &str) {
        self.area.select_all();
        self.area.cut();
        self.area.insert_str(text);
        self.area.move_cursor(CursorMove::Bottom);
        self.area.move_cursor(CursorMove::End);
        self.wrapped.set(None);
    }

    /// Replaces the current input without filing it in prompt history.
    pub(crate) fn replace(&mut self, text: &str) {
        self.show(text);
    }

    pub(crate) fn insert_text(&mut self, text: &str) {
        self.area.insert_str(text);
        self.wrapped.set(None);
    }

    pub(crate) fn add_image(&mut self, image: InputImage) {
        let thumbnail = match thumbnail(&self.picker, &image) {
            Ok(thumbnail) => Some(thumbnail),
            Err(error) => {
                tracing::warn!(
                    event = "ui.image_thumbnail.prepare_failed",
                    media_type = image.media_type(),
                    width = image.width(),
                    height = image.height(),
                    error = error.to_string(),
                );

                None
            }
        };

        self.images.push(Attachment { image, thumbnail });
    }

    pub(crate) fn has_images(&self) -> bool {
        !self.images.is_empty()
    }

    pub(crate) fn attachments_focused(&self) -> bool {
        matches!(self.focus, Focus::Image(_))
    }

    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent) -> bool {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return false;
        }

        let index = self
            .close_areas
            .borrow()
            .iter()
            .find(|(area, _)| {
                mouse.column >= area.x
                    && mouse.column < area.x.saturating_add(area.width)
                    && mouse.row >= area.y
                    && mouse.row < area.y.saturating_add(area.height)
            })
            .map(|(_, index)| *index);

        let Some(index) = index else {
            return false;
        };

        self.remove_image(index);
        true
    }

    fn remove_image(&mut self, index: usize) {
        if index >= self.images.len() {
            self.focus = Focus::Text;

            return;
        }

        self.images.remove(index);
        self.focus = if self.images.is_empty() {
            Focus::Text
        } else {
            Focus::Image(index.min(self.images.len() - 1))
        };
    }

    /// Clears the draft and recall entries for a newly started session.
    pub(crate) fn reset(&mut self) {
        self.show("");
        self.history.clear();
        self.images.clear();
        self.focus = Focus::Text;
        self.recalled = None;
        self.draft.clear();
        self.close_areas.borrow_mut().clear();
    }

    /// What the box currently holds.
    pub fn text(&self) -> String {
        self.area.lines().join("\n")
    }

    /// Rows the box needs, grown to fit what has been typed and capped so it
    /// never crowds out the conversation.
    pub fn height(&self, width: u16) -> u16 {
        self.wrapped_rows(width)
            .saturating_add(self.attachment_rows(width))
            .saturating_add(BORDER_HEIGHT)
            .clamp(MIN_HEIGHT, MAX_HEIGHT)
    }

    fn attachment_rows(&self, _width: u16) -> u16 {
        if self.images.is_empty() {
            0
        } else {
            ATTACHMENT_HEIGHT
        }
    }

    fn wrapped_rows(&self, width: u16) -> u16 {
        let width = width
            .saturating_sub(MARKER_WIDTH + CURSOR_GUTTER_WIDTH)
            .max(1);

        if let Some((measured, rows)) = self.wrapped.get()
            && measured == width
        {
            return rows;
        }

        // Ask the textarea to lay out a clone so height uses exactly the same
        // word/glyph wrapping as the widget itself, including tabs and CJK.
        let mut area = self.area.clone();
        area.move_cursor(CursorMove::Bottom);
        area.move_cursor(CursorMove::End);

        let rect = Rect::new(0, 0, width, 1);
        let mut buffer = Buffer::empty(rect);
        (&area).render(rect, &mut buffer);

        let rows = area
            .screen_cursor()
            .row
            .saturating_add(1)
            .try_into()
            .unwrap_or(u16::MAX);
        self.wrapped.set(Some((width, rows)));

        rows
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = block.inner(area);

        frame.render_widget(block, area);

        let attachment_height = self
            .attachment_rows(area.width)
            .min(inner.height.saturating_sub(1));
        let [text, attachments] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(attachment_height)])
                .areas(inner);

        self.render_text(frame, text);
        self.render_attachments(frame, attachments);
    }

    fn render_text(&self, frame: &mut Frame, area: Rect) {
        // The marker sits beside the field rather than inside it, so it stays put
        // as the text scrolls and never lands in what gets submitted.
        let [marker, field, cursor_gutter] = Layout::horizontal([
            Constraint::Length(MARKER_WIDTH),
            Constraint::Min(0),
            Constraint::Length(CURSOR_GUTTER_WIDTH),
        ])
        .areas(area);

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                PROMPT_MARKER,
                Style::default().fg(Color::Cyan),
            ))),
            marker,
        );
        frame.render_widget(&self.area, field);

        self.render_cursor_gutter(frame, field, cursor_gutter);
    }

    fn render_attachments(&self, frame: &mut Frame, area: Rect) {
        self.close_areas.borrow_mut().clear();

        if area.is_empty() {
            return;
        }

        let [_marker, field, _gutter] = Layout::horizontal([
            Constraint::Length(MARKER_WIDTH),
            Constraint::Min(0),
            Constraint::Length(CURSOR_GUTTER_WIDTH),
        ])
        .areas(area);

        let focus = match self.focus {
            Focus::Image(index) => index,
            Focus::Text => self.images.len().saturating_sub(1),
        };

        for tile in tiles(field.width, self.images.len(), focus) {
            let area = Rect::new(
                field.x.saturating_add(tile.x),
                field.y,
                tile.width.min(field.width.saturating_sub(tile.x)),
                field.height.min(ATTACHMENT_HEIGHT),
            );
            let [preview, label] =
                Layout::vertical([Constraint::Length(THUMBNAIL_HEIGHT), Constraint::Length(1)])
                    .areas(area);

            self.render_thumbnail(frame, preview, &self.images[tile.index]);

            let selected = self.focus == Focus::Image(tile.index);
            let style = if selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::Cyan)
            };
            let text = format!("[Image {} ×]", tile.index + 1);
            let text_width = UnicodeWidthStr::width(text.as_str())
                .try_into()
                .unwrap_or(u16::MAX);

            frame.render_widget(
                Paragraph::new(Span::styled(text, style)).alignment(Alignment::Center),
                label,
            );

            let text_x = label
                .x
                .saturating_add(label.width.saturating_sub(text_width) / 2);
            let close_x = text_x.saturating_add(text_width.saturating_sub(2));
            if close_x < label.x.saturating_add(label.width) {
                self.close_areas
                    .borrow_mut()
                    .push((Rect::new(close_x, label.y, 1, 1), tile.index));
            }
        }
    }

    fn render_thumbnail(&self, frame: &mut Frame, area: Rect, attachment: &Attachment) {
        frame.render_widget(
            Block::default().style(Style::default().bg(Color::Rgb(24, 24, 24))),
            area,
        );

        let Some(thumbnail) = &attachment.thumbnail else {
            frame.render_widget(
                Paragraph::new("preview unavailable")
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(Color::DarkGray)),
                area,
            );

            return;
        };
        let size = thumbnail.size();
        let image = Rect::new(
            area.x
                .saturating_add(area.width.saturating_sub(size.width) / 2),
            area.y
                .saturating_add(area.height.saturating_sub(size.height) / 2),
            size.width.min(area.width),
            size.height.min(area.height),
        );

        if thumbnail.needs_placeholder(image).is_some() {
            frame.render_widget(
                Paragraph::new("[Image]")
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(Color::DarkGray)),
                area,
            );

            return;
        }

        frame.render_widget(ImageWidget::new(thumbnail), image);
    }

    /// Draws the synthetic end-of-line cursor that the textarea clips when it
    /// lands immediately beyond the field's final column.
    fn render_cursor_gutter(&self, frame: &mut Frame, field: Rect, gutter: Rect) {
        let cursor = self.area.screen_cursor();
        let cursor_row = cursor.row.try_into().unwrap_or(u16::MAX);
        let scroll_top = next_scroll_top(self.scroll_top.get(), cursor_row, field.height);

        self.scroll_top.set(scroll_top);

        if cursor.col != field.width as usize {
            return;
        }

        let row = cursor_row.saturating_sub(scroll_top);
        if row >= gutter.height {
            return;
        }

        let area = Rect::new(gutter.x, gutter.y.saturating_add(row), gutter.width, 1);
        frame.render_widget(
            Paragraph::new(Span::styled(" ", self.area.cursor_style())),
            area,
        );
    }
}

fn tiles(width: u16, count: usize, focus: usize) -> Vec<Tile> {
    if width == 0 || count == 0 {
        return Vec::new();
    }

    let stride = THUMBNAIL_WIDTH.saturating_add(ATTACHMENT_GAP);
    let capacity = usize::from(
        width
            .saturating_add(ATTACHMENT_GAP)
            .checked_div(stride)
            .unwrap_or(0)
            .max(1),
    );
    let visible = count.min(capacity);
    let focus = focus.min(count - 1);
    let start = focus.saturating_sub(visible / 2).min(count - visible);

    (start..start + visible)
        .enumerate()
        .map(|(offset, index)| {
            let x = u16::try_from(offset)
                .unwrap_or(u16::MAX)
                .saturating_mul(stride);

            Tile {
                index,
                x,
                width: THUMBNAIL_WIDTH.min(width.saturating_sub(x)),
            }
        })
        .collect()
}

fn thumbnail(picker: &Picker, image: &InputImage) -> anyhow::Result<Protocol> {
    let image = image::load_from_memory(&image.bytes())?;
    let thumbnail = picker.new_protocol(
        image,
        Size::new(THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT),
        Resize::Fit(None),
    )?;

    Ok(thumbnail)
}

/// Mirrors the textarea's viewport rule to place a cursor in the gutter.
fn next_scroll_top(previous: u16, cursor: u16, height: u16) -> u16 {
    if cursor < previous {
        cursor
    } else if previous.saturating_add(height) <= cursor {
        cursor.saturating_add(1).saturating_sub(height)
    } else {
        previous
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_WIDTH: u16 = 40;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text(input: &mut Input, text: &str) {
        for character in text.chars() {
            input.handle_key(press(KeyCode::Char(character)));
        }
    }

    fn image() -> InputImage {
        InputImage::new("image/png", [1, 2, 3], 32, 32).unwrap()
    }

    fn renderable_image() -> InputImage {
        use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};

        const WIDTH: u32 = 120;
        const HEIGHT: u32 = 100;

        let mut rgba = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
        for y in 0..HEIGHT {
            let color = if y < HEIGHT / 2 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            };

            for _ in 0..WIDTH {
                rgba.extend_from_slice(&color);
            }
        }

        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(&rgba, WIDTH, HEIGHT, ExtendedColorType::Rgba8)
            .unwrap();

        InputImage::new("image/png", png, WIDTH, HEIGHT).unwrap()
    }

    #[test]
    fn a_bare_enter_inserts_a_newline_instead_of_submitting() {
        let mut input = Input::default();

        type_text(&mut input, "one");
        assert_eq!(input.handle_key(press(KeyCode::Enter)), None);
        type_text(&mut input, "two");

        assert_eq!(
            input
                .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT))
                .map(|input| input.text()),
            Some("one\ntwo".to_owned())
        );
    }

    #[test]
    fn either_modifier_submits() {
        for modifier in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            let mut input = Input::default();
            type_text(&mut input, "hi");

            assert_eq!(
                input
                    .handle_key(KeyEvent::new(KeyCode::Enter, modifier))
                    .map(|input| input.text()),
                Some("hi".to_owned()),
                "{modifier:?} should submit"
            );
        }
    }

    #[test]
    fn submitting_clears_the_box() {
        let mut input = Input::default();

        type_text(&mut input, "hi");
        input.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));

        assert_eq!(input.height(TEST_WIDTH), MIN_HEIGHT);
        assert_eq!(
            input.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
            None,
            "nothing is left to submit"
        );
    }

    #[test]
    fn a_blank_box_submits_nothing() {
        let mut input = Input::default();

        type_text(&mut input, "   ");

        assert_eq!(
            input.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
            None
        );
    }

    #[test]
    fn an_image_can_be_submitted_without_text() {
        let mut input = Input::default();
        input.add_image(image());

        let submitted = input
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT))
            .unwrap();

        assert_eq!(submitted.text(), "");
        assert_eq!(submitted.image_count(), 1);
        assert!(!input.has_images());
    }

    #[test]
    fn backspace_on_empty_text_removes_the_last_image() {
        let mut input = Input::default();
        input.add_image(image());
        input.add_image(image());

        input.handle_key(press(KeyCode::Backspace));

        assert_eq!(input.take().unwrap().image_count(), 1);
    }

    #[test]
    fn attachment_focus_selects_and_deletes_images() {
        let mut input = Input::default();
        input.add_image(image());
        input.add_image(image());

        input.handle_key(press(KeyCode::BackTab));
        assert_eq!(input.focus, Focus::Image(0));

        input.handle_key(press(KeyCode::Right));
        assert_eq!(input.focus, Focus::Image(1));

        input.handle_key(press(KeyCode::Delete));
        assert_eq!(input.focus, Focus::Image(0));
        assert_eq!(input.take().unwrap().image_count(), 1);
    }

    #[test]
    fn escape_returns_attachment_focus_to_text() {
        let mut input = Input::default();
        input.add_image(image());

        input.handle_key(press(KeyCode::BackTab));
        input.handle_key(press(KeyCode::Esc));
        type_text(&mut input, "describe");

        let submitted = input.take().unwrap();
        assert_eq!(submitted.text(), "describe");
        assert_eq!(submitted.image_count(), 1);
    }

    #[test]
    fn rendered_close_button_removes_its_image() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut input = Input::default();
        input.add_image(image());
        input.add_image(image());
        let height = input.height(TEST_WIDTH);
        let mut terminal = Terminal::new(TestBackend::new(TEST_WIDTH, height)).unwrap();

        terminal
            .draw(|frame| input.render(frame, frame.area()))
            .unwrap();

        let close = input.close_areas.borrow()[0].0;
        let removed = input.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: close.x,
            row: close.y,
            modifiers: KeyModifiers::NONE,
        });

        assert!(removed);
        assert_eq!(input.take().unwrap().image_count(), 1);
    }

    #[test]
    fn attachment_bar_renders_a_thumbnail_and_label() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut input = Input::default();
        input.add_image(renderable_image());
        let height = input.height(TEST_WIDTH);
        let mut terminal = Terminal::new(TestBackend::new(TEST_WIDTH, height)).unwrap();

        terminal
            .draw(|frame| input.render(frame, frame.area()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let rendered_image = buffer.content().iter().any(|cell| {
            matches!(
                (cell.fg, cell.bg),
                (
                    Color::Rgb(255, 0, 0) | Color::Rgb(0, 0, 255),
                    Color::Rgb(255, 0, 0) | Color::Rgb(0, 0, 255)
                )
            )
        });
        let rendered_label = buffer
            .content()
            .chunks(TEST_WIDTH as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .any(|row| row.contains("[Image 1 ×]"));

        assert!(rendered_image);
        assert!(rendered_label);
        assert_eq!(height, 1 + ATTACHMENT_HEIGHT + BORDER_HEIGHT);
    }

    #[test]
    fn attachment_tiles_window_around_the_focus() {
        assert_eq!(
            tiles(THUMBNAIL_WIDTH * 2 + ATTACHMENT_GAP, 4, 0)
                .iter()
                .map(|tile| tile.index)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        assert_eq!(
            tiles(THUMBNAIL_WIDTH * 2 + ATTACHMENT_GAP, 4, 3)
                .iter()
                .map(|tile| tile.index)
                .collect::<Vec<_>>(),
            [2, 3]
        );
        assert_eq!(
            tiles(THUMBNAIL_WIDTH * 3 + ATTACHMENT_GAP * 2, 5, 2)
                .iter()
                .map(|tile| tile.index)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
    }

    #[test]
    fn attachment_bar_survives_a_terminal_narrower_than_a_thumbnail() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut input = Input::default();
        input.add_image(renderable_image());
        let height = input.height(6);
        let mut terminal = Terminal::new(TestBackend::new(6, height)).unwrap();

        terminal
            .draw(|frame| input.render(frame, frame.area()))
            .unwrap();

        assert_eq!(tiles(3, 1, 0)[0].width, 3);
    }

    #[test]
    fn the_box_grows_with_the_text_and_then_stops() {
        let mut input = Input::default();

        assert_eq!(input.height(TEST_WIDTH), MIN_HEIGHT);

        for _ in 0..3 {
            input.handle_key(press(KeyCode::Enter));
        }
        assert_eq!(input.height(TEST_WIDTH), 4 + BORDER_HEIGHT);

        for _ in 0..40 {
            input.handle_key(press(KeyCode::Enter));
        }
        assert_eq!(
            input.height(TEST_WIDTH),
            MAX_HEIGHT,
            "capped so the log stays visible"
        );
    }

    #[test]
    fn a_long_logical_line_wraps_and_grows_the_box() {
        let mut input = Input::default();

        type_text(&mut input, "abcdefghijk");

        assert_eq!(input.area.wrap_mode(), WrapMode::WordOrGlyph);
        assert_eq!(input.height(12), 2 + BORDER_HEIGHT);
    }

    #[test]
    fn wrapped_height_uses_terminal_width_for_cjk() {
        let mut input = Input::default();

        type_text(&mut input, "中文测试");

        // Seven columns leave four for text after the marker and cursor gutter:
        // two CJK glyphs per visual row.
        assert_eq!(input.height(7), 2 + BORDER_HEIGHT);
    }

    #[test]
    fn a_full_visual_line_keeps_the_cursor_visible() {
        use ratatui::{Terminal, backend::TestBackend, style::Modifier};

        let mut input = Input::default();

        // Ten columns leave seven for text and one for the cursor.
        type_text(&mut input, "abcdefg");
        assert_eq!(input.height(10), MIN_HEIGHT);

        let mut terminal = Terminal::new(TestBackend::new(10, MIN_HEIGHT)).unwrap();
        terminal
            .draw(|frame| input.render(frame, frame.area()))
            .unwrap();

        let cursor = &terminal.backend().buffer()[(9, 1)];
        assert!(cursor.modifier.contains(Modifier::REVERSED));

        type_text(&mut input, "h");
        assert_eq!(
            input.height(10),
            2 + BORDER_HEIGHT,
            "text wraps before consuming the cursor column"
        );
    }

    fn submit(input: &mut Input) -> Option<String> {
        input
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT))
            .map(|input| input.text())
    }

    fn send(input: &mut Input, prompt: &str) {
        type_text(input, prompt);
        submit(input);
    }

    #[test]
    fn up_recalls_the_last_prompt() {
        let mut input = Input::default();

        send(&mut input, "first");
        input.handle_key(press(KeyCode::Up));

        assert_eq!(input.text(), "first");
    }

    #[test]
    fn up_moves_inside_a_soft_wrapped_line_before_recalling_history() {
        let mut input = Input::default();

        send(&mut input, "history");
        type_text(&mut input, "abcdefghijk");

        let rect = Rect::new(0, 0, 10, 2);
        let mut buffer = Buffer::empty(rect);
        (&input.area).render(rect, &mut buffer);

        assert_eq!(input.area.screen_cursor().row, 1);

        input.handle_key(press(KeyCode::Up));

        assert_eq!(input.text(), "abcdefghijk");
        assert_eq!(input.area.screen_cursor().row, 0);
    }

    #[test]
    fn up_walks_further_back_and_down_walks_forward() {
        let mut input = Input::default();

        send(&mut input, "oldest");
        send(&mut input, "middle");
        send(&mut input, "newest");

        input.handle_key(press(KeyCode::Up));
        assert_eq!(input.text(), "newest");

        input.handle_key(press(KeyCode::Up));
        input.handle_key(press(KeyCode::Up));
        assert_eq!(input.text(), "oldest");

        input.handle_key(press(KeyCode::Down));
        assert_eq!(input.text(), "middle");
    }

    #[test]
    fn the_oldest_entry_is_the_end_of_the_road() {
        let mut input = Input::default();

        send(&mut input, "only");
        for _ in 0..5 {
            input.handle_key(press(KeyCode::Up));
        }

        assert_eq!(input.text(), "only", "it should not wrap around");
    }

    #[test]
    fn coming_back_past_the_newest_restores_the_draft() {
        let mut input = Input::default();

        send(&mut input, "sent");
        type_text(&mut input, "half typed");

        input.handle_key(press(KeyCode::Up));
        assert_eq!(input.text(), "sent");

        input.handle_key(press(KeyCode::Down));
        assert_eq!(
            input.text(),
            "half typed",
            "the draft was set aside, not thrown away"
        );
    }

    #[test]
    fn down_on_a_draft_does_nothing() {
        let mut input = Input::default();

        send(&mut input, "sent");
        type_text(&mut input, "typing");
        input.handle_key(press(KeyCode::Down));

        assert_eq!(input.text(), "typing");
    }

    #[test]
    fn an_arrow_inside_a_multiline_prompt_moves_the_cursor() {
        let mut input = Input::default();

        send(&mut input, "history entry");
        type_text(&mut input, "top");
        input.handle_key(press(KeyCode::Enter));
        type_text(&mut input, "bottom");

        // The cursor is on the last line, so Up belongs to the text.
        input.handle_key(press(KeyCode::Up));

        assert_eq!(input.text(), "top\nbottom", "the text is untouched");
        assert_eq!(input.area.cursor().0, 0, "the cursor moved instead");

        // Now at the top, Up reaches the history.
        input.handle_key(press(KeyCode::Up));
        assert_eq!(input.text(), "history entry");
    }

    #[test]
    fn a_recalled_prompt_can_be_edited_and_sent_again() {
        let mut input = Input::default();

        send(&mut input, "cargo test");
        input.handle_key(press(KeyCode::Up));
        type_text(&mut input, " --release");

        assert_eq!(submit(&mut input), Some("cargo test --release".to_owned()));
        assert_eq!(input.history, ["cargo test", "cargo test --release"]);
    }

    #[test]
    fn sending_leaves_the_history_at_its_newest_end() {
        let mut input = Input::default();

        send(&mut input, "one");
        input.handle_key(press(KeyCode::Up));
        type_text(&mut input, "!");
        submit(&mut input);

        assert_eq!(input.recalled, None, "sending leaves the history behind");
        input.handle_key(press(KeyCode::Up));
        assert_eq!(
            input.text(),
            "one!",
            "browsing starts from the newest again"
        );
    }

    #[test]
    fn the_same_prompt_twice_is_stored_once() {
        let mut input = Input::default();

        send(&mut input, "again");
        send(&mut input, "again");

        assert_eq!(input.history, ["again"]);
    }

    #[test]
    fn the_history_does_not_grow_without_bound() {
        let mut input = Input::default();

        for index in 0..HISTORY_LIMIT + 20 {
            send(&mut input, &format!("prompt {index}"));
        }

        assert_eq!(input.history.len(), HISTORY_LIMIT);
        assert_eq!(
            input.history.last().unwrap(),
            &format!("prompt {}", HISTORY_LIMIT + 19),
            "the newest survives; the oldest are dropped"
        );
    }

    #[test]
    fn a_seeded_history_is_recalled_newest_first() {
        let mut input = Input::default();

        input.seed(["oldest".to_owned(), "newest".to_owned()]);

        input.handle_key(press(KeyCode::Up));
        assert_eq!(input.text(), "newest");

        input.handle_key(press(KeyCode::Up));
        assert_eq!(input.text(), "oldest");
    }

    #[test]
    fn a_prompt_sent_after_seeding_lands_on_top() {
        let mut input = Input::default();

        input.seed(["from the archive".to_owned()]);
        send(&mut input, "asked just now");

        input.handle_key(press(KeyCode::Up));
        assert_eq!(input.text(), "asked just now");

        input.handle_key(press(KeyCode::Up));
        assert_eq!(input.text(), "from the archive");
    }

    #[test]
    fn seeding_obeys_the_same_cap() {
        let mut input = Input::default();

        input.seed((0..HISTORY_LIMIT + 20).map(|index| format!("prompt {index}")));

        assert_eq!(input.history.len(), HISTORY_LIMIT);
    }

    #[test]
    fn an_empty_history_ignores_the_arrows() {
        let mut input = Input::default();

        type_text(&mut input, "typing");
        input.handle_key(press(KeyCode::Up));

        assert_eq!(input.text(), "typing");
    }

    #[test]
    fn a_release_event_is_ignored() {
        let mut input = Input::default();
        let mut release = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;

        input.handle_key(release);
        input.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));

        assert_eq!(input.height(TEST_WIDTH), MIN_HEIGHT, "nothing was typed");
    }
}
