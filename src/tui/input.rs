use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
};
use ratatui_textarea::TextArea;

const PROMPT_MARKER: &str = "❯ ";
const MARKER_WIDTH: u16 = 2;

const MIN_HEIGHT: u16 = 3;
const MAX_HEIGHT: u16 = 10;
/// The top and bottom rules.
const BORDER_HEIGHT: u16 = 2;

/// The prompt box.
///
/// A bare Enter inserts a newline; submitting takes a modified one. Ctrl+Enter
/// reaches us only over the kitty keyboard protocol — without it, terminals send
/// a plain carriage return for it, indistinguishable from Enter. Alt+Enter
/// arrives as an ESC-prefixed return, which nearly every terminal sends.
pub struct Input {
    area: TextArea<'static>,
}

impl Default for Input {
    fn default() -> Self {
        let mut area = TextArea::default();

        area.set_cursor_line_style(Style::default());

        Self { area }
    }
}

impl Input {
    /// Takes a key. A submitted prompt is handed back and the box is cleared.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<String> {
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

        self.area.input(key);
        None
    }

    fn take(&mut self) -> Option<String> {
        let prompt = self.area.lines().join("\n");

        if prompt.trim().is_empty() {
            return None;
        }

        self.area.select_all();
        self.area.cut();

        Some(prompt)
    }

    /// Rows the box needs, grown to fit what has been typed and capped so it
    /// never crowds out the conversation.
    pub fn height(&self) -> u16 {
        (self.area.lines().len() as u16)
            .saturating_add(BORDER_HEIGHT)
            .clamp(MIN_HEIGHT, MAX_HEIGHT)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = block.inner(area);

        frame.render_widget(block, area);

        // The marker sits beside the field rather than inside it, so it stays put
        // as the text scrolls and never lands in what gets submitted.
        let [marker, field] =
            Layout::horizontal([Constraint::Length(MARKER_WIDTH), Constraint::Min(0)]).areas(inner);

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                PROMPT_MARKER,
                Style::default().fg(Color::Cyan),
            ))),
            marker,
        );
        frame.render_widget(&self.area, field);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text(input: &mut Input, text: &str) {
        for character in text.chars() {
            input.handle_key(press(KeyCode::Char(character)));
        }
    }

    #[test]
    fn a_bare_enter_inserts_a_newline_instead_of_submitting() {
        let mut input = Input::default();

        type_text(&mut input, "one");
        assert_eq!(input.handle_key(press(KeyCode::Enter)), None);
        type_text(&mut input, "two");

        assert_eq!(
            input.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
            Some("one\ntwo".to_owned())
        );
    }

    #[test]
    fn either_modifier_submits() {
        for modifier in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            let mut input = Input::default();
            type_text(&mut input, "hi");

            assert_eq!(
                input.handle_key(KeyEvent::new(KeyCode::Enter, modifier)),
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

        assert_eq!(input.height(), MIN_HEIGHT);
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
    fn the_box_grows_with_the_text_and_then_stops() {
        let mut input = Input::default();

        assert_eq!(input.height(), MIN_HEIGHT);

        for _ in 0..3 {
            input.handle_key(press(KeyCode::Enter));
        }
        assert_eq!(input.height(), 4 + BORDER_HEIGHT);

        for _ in 0..40 {
            input.handle_key(press(KeyCode::Enter));
        }
        assert_eq!(
            input.height(),
            MAX_HEIGHT,
            "capped so the log stays visible"
        );
    }

    #[test]
    fn a_release_event_is_ignored() {
        let mut input = Input::default();
        let mut release = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;

        input.handle_key(release);
        input.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));

        assert_eq!(input.height(), MIN_HEIGHT, "nothing was typed");
    }
}
