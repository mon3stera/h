use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Position, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

/// One row of a [`ChoiceList`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChoiceItem {
    /// A fixed option. Enter submits it as-is.
    Choice {
        prefix: Option<String>,
        label: String,
        description: Option<String>,
    },
    /// A row the user types into, for when none of the options fit. Enter
    /// submits whatever was typed and is ignored while the field is blank.
    FreeText { placeholder: String },
}

impl ChoiceItem {
    pub fn choice(label: impl Into<String>) -> Self {
        Self::Choice {
            prefix: None,
            label: label.into(),
            description: None,
        }
    }

    pub fn described(label: impl Into<String>, description: impl Into<String>) -> Self {
        Self::Choice {
            prefix: None,
            label: label.into(),
            description: Some(description.into()),
        }
    }

    pub fn prefixed(label: impl Into<String>, prefix: impl Into<String>) -> Self {
        Self::Choice {
            prefix: Some(prefix.into()),
            label: label.into(),
            description: None,
        }
    }

    pub fn free_text(placeholder: impl Into<String>) -> Self {
        Self::FreeText {
            placeholder: placeholder.into(),
        }
    }
}

/// What the user settled on. The index refers to the `items` that were passed
/// in, so callers can map it back to whatever they built the list from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChoiceOutcome {
    Choice { index: usize, label: String },
    FreeText { index: usize, text: String },
}

/// What a key press did to the list. Anything that is not a decision leaves the
/// caller to draw again and read the next key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChoiceEvent {
    Idle,
    Submitted(ChoiceOutcome),
    /// Dismissed without choosing — Esc or Ctrl+C.
    Dismissed,
}

const MARKER_WIDTH: u16 = 2;

/// A keyboard-driven list: Up/Down move the selection (wrapping at the ends),
/// Enter submits it, Esc abandons it.
///
/// Key handling is a plain state transition and drawing reads that state, so the
/// two can be exercised apart: neither needs a terminal.
pub struct ChoiceList {
    items: Vec<ChoiceItem>,
    selected: usize,
    /// Kept across moves of the selection, so leaving the free-text row and
    /// coming back does not lose what was typed.
    draft: String,
    max_visible: Option<usize>,
    /// Where the caret landed on the last draw, for the terminal cursor to
    /// follow. Only a selected free-text row has one.
    caret: Option<Position>,
}

impl ChoiceList {
    pub fn new(items: Vec<ChoiceItem>) -> Self {
        Self {
            items,
            selected: 0,
            draft: String::new(),
            max_visible: None,
            caret: None,
        }
    }

    /// Rows to show at once. The window follows the selection; `None` shows
    /// every item.
    pub fn with_max_visible(mut self, max_visible: Option<usize>) -> Self {
        self.max_visible = max_visible;
        self
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Rows the list would draw, for a caller that has to reserve room.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn on_free_text(&self) -> bool {
        matches!(
            self.items.get(self.selected),
            Some(ChoiceItem::FreeText { .. })
        )
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ChoiceEvent {
        // A held key repeats; a released one is the same press arriving twice.
        if key.kind == KeyEventKind::Release || self.items.is_empty() {
            return ChoiceEvent::Idle;
        }

        let count = self.items.len();

        match key.code {
            KeyCode::Esc => return ChoiceEvent::Dismissed,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return ChoiceEvent::Dismissed;
            }
            KeyCode::Up => self.selected = (self.selected + count - 1) % count,
            KeyCode::Down => self.selected = (self.selected + 1) % count,
            KeyCode::Enter => return self.submit(),
            // Typing reaches the field only while it is selected, otherwise every
            // keystroke meant for the list would land in it.
            KeyCode::Char(character)
                if self.on_free_text() && !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.draft.push(character);
            }
            KeyCode::Backspace if self.on_free_text() => {
                self.draft.pop();
            }
            _ => {}
        }

        ChoiceEvent::Idle
    }

    fn submit(&self) -> ChoiceEvent {
        let index = self.selected;

        match &self.items[index] {
            ChoiceItem::Choice { label, .. } => ChoiceEvent::Submitted(ChoiceOutcome::Choice {
                index,
                label: label.clone(),
            }),
            ChoiceItem::FreeText { .. } => {
                let text = self.draft.trim().to_owned();

                // A blank field is not an answer.
                if text.is_empty() {
                    ChoiceEvent::Idle
                } else {
                    ChoiceEvent::Submitted(ChoiceOutcome::FreeText { index, text })
                }
            }
        }
    }

    /// Where the terminal cursor should sit after the last draw.
    pub fn caret(&self) -> Option<Position> {
        self.caret
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let (lines, caret) = self.lines(area);

        frame.render_widget(Paragraph::new(lines), area);

        self.caret = caret;
        if let Some(caret) = caret {
            frame.set_cursor_position(caret);
        }
    }

    /// The drawn rows, and where the caret belongs among them.
    fn lines(&self, area: Rect) -> (Vec<Line<'static>>, Option<Position>) {
        let count = self.items.len();
        let (start, end) = visible_window(self.selected, count, self.max_visible);

        let mut lines = Vec::new();
        let mut caret = None;

        if start > 0 {
            lines.push(overflow_hint());
        }

        for index in start..end {
            let is_selected = index == self.selected;

            if is_selected && matches!(self.items[index], ChoiceItem::FreeText { .. }) {
                caret = Some(Position::new(
                    area.x + MARKER_WIDTH + self.draft.chars().count() as u16,
                    area.y + lines.len() as u16,
                ));
            }

            lines.push(self.row(index, is_selected));
        }

        if end < count {
            lines.push(overflow_hint());
        }

        (lines, caret)
    }

    fn row(&self, index: usize, is_selected: bool) -> Line<'static> {
        let marker = if is_selected { "❯ " } else { "  " };
        let selected_style = if is_selected {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        let muted = Style::default().fg(Color::DarkGray);

        let mut spans = vec![Span::styled(marker, selected_style)];

        match &self.items[index] {
            ChoiceItem::Choice {
                prefix,
                label,
                description,
            } => {
                if let Some(prefix) = prefix {
                    spans.push(Span::styled(prefix.clone(), muted));
                }

                spans.push(Span::styled(label.clone(), selected_style));

                if let Some(description) = description {
                    spans.push(Span::styled(format!("  {description}"), muted));
                }
            }
            ChoiceItem::FreeText { placeholder } => {
                if self.draft.is_empty() {
                    spans.push(Span::styled(placeholder.clone(), muted));
                } else {
                    spans.push(Span::styled(self.draft.clone(), selected_style));
                }
            }
        }

        Line::from(spans)
    }
}

fn overflow_hint() -> Line<'static> {
    Line::from(Span::styled("  ⋯", Style::default().fg(Color::DarkGray)))
}

/// The slice of items to draw, chosen so the selection stays inside it.
fn visible_window(selected: usize, count: usize, max_visible: Option<usize>) -> (usize, usize) {
    let Some(max_visible) = max_visible.map(|max| max.max(1)) else {
        return (0, count);
    };

    if count <= max_visible {
        return (0, count);
    }

    let start = selected
        .saturating_sub(max_visible / 2)
        .min(count - max_visible);

    (start, start + max_visible)
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn options() -> Vec<ChoiceItem> {
        vec![
            ChoiceItem::choice("first"),
            ChoiceItem::described("second", "with detail"),
            ChoiceItem::free_text("something else"),
        ]
    }

    /// Feeds a list of keys and reports the first decision it reached.
    fn drive(list: &mut ChoiceList, keys: Vec<KeyCode>) -> ChoiceEvent {
        for code in keys {
            match list.handle_key(press(code)) {
                ChoiceEvent::Idle => {}
                decided => return decided,
            }
        }

        ChoiceEvent::Idle
    }

    /// The rows as drawn, with trailing padding removed.
    fn drawn(list: &mut ChoiceList, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(40, height)).unwrap();

        terminal
            .draw(|frame| list.render(frame, frame.area()))
            .unwrap();

        terminal
            .backend()
            .buffer()
            .content()
            .chunks(40)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .map(|row| row.trim_end().to_owned())
            .collect()
    }

    #[test]
    fn enter_submits_the_selected_choice() {
        let mut list = ChoiceList::new(options());

        assert_eq!(
            drive(&mut list, vec![KeyCode::Down, KeyCode::Enter]),
            ChoiceEvent::Submitted(ChoiceOutcome::Choice {
                index: 1,
                label: "second".to_owned(),
            })
        );
    }

    #[test]
    fn up_wraps_to_the_last_item() {
        let mut list = ChoiceList::new(options());

        drive(&mut list, vec![KeyCode::Up]);

        assert_eq!(list.selected(), 2);
    }

    #[test]
    fn down_wraps_to_the_first_item() {
        let mut list = ChoiceList::new(options());

        drive(&mut list, vec![KeyCode::Down, KeyCode::Down, KeyCode::Down]);

        assert_eq!(list.selected(), 0);
    }

    #[test]
    fn typing_only_reaches_the_free_text_row_while_it_is_selected() {
        let mut list = ChoiceList::new(options());

        // Two rows above the field: these keystrokes belong to the list.
        drive(&mut list, vec![KeyCode::Char('x'), KeyCode::Char('y')]);
        drive(&mut list, vec![KeyCode::Up]);
        let outcome = drive(
            &mut list,
            vec![KeyCode::Char('o'), KeyCode::Char('k'), KeyCode::Enter],
        );

        assert_eq!(
            outcome,
            ChoiceEvent::Submitted(ChoiceOutcome::FreeText {
                index: 2,
                text: "ok".to_owned(),
            })
        );
    }

    #[test]
    fn enter_on_a_blank_free_text_row_does_not_submit() {
        let mut list = ChoiceList::new(options());

        assert_eq!(
            drive(&mut list, vec![KeyCode::Up, KeyCode::Enter]),
            ChoiceEvent::Idle
        );
    }

    #[test]
    fn a_draft_survives_leaving_the_row_and_coming_back() {
        let mut list = ChoiceList::new(options());

        drive(
            &mut list,
            vec![KeyCode::Up, KeyCode::Char('h'), KeyCode::Char('i')],
        );
        drive(&mut list, vec![KeyCode::Down, KeyCode::Up]);

        assert_eq!(
            drive(&mut list, vec![KeyCode::Enter]),
            ChoiceEvent::Submitted(ChoiceOutcome::FreeText {
                index: 2,
                text: "hi".to_owned(),
            })
        );
    }

    #[test]
    fn backspace_only_edits_the_field() {
        let mut list = ChoiceList::new(options());

        drive(
            &mut list,
            vec![KeyCode::Up, KeyCode::Char('a'), KeyCode::Char('b')],
        );
        drive(&mut list, vec![KeyCode::Backspace]);

        assert_eq!(
            drive(&mut list, vec![KeyCode::Enter]),
            ChoiceEvent::Submitted(ChoiceOutcome::FreeText {
                index: 2,
                text: "a".to_owned(),
            })
        );
    }

    #[test]
    fn esc_and_ctrl_c_abandon_the_list() {
        let mut list = ChoiceList::new(options());

        assert_eq!(list.handle_key(press(KeyCode::Esc)), ChoiceEvent::Dismissed);
        assert_eq!(
            list.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            ChoiceEvent::Dismissed,
        );
    }

    #[test]
    fn a_control_chord_is_not_typed_into_the_field() {
        let mut list = ChoiceList::new(options());

        drive(&mut list, vec![KeyCode::Up]);
        list.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));

        assert_eq!(
            drive(&mut list, vec![KeyCode::Enter]),
            ChoiceEvent::Idle,
            "the field should still be blank"
        );
    }

    #[test]
    fn an_empty_list_ignores_every_key() {
        let mut list = ChoiceList::new(Vec::new());

        assert_eq!(list.handle_key(press(KeyCode::Enter)), ChoiceEvent::Idle);
        assert_eq!(list.handle_key(press(KeyCode::Down)), ChoiceEvent::Idle);
    }

    #[test]
    fn the_selected_row_is_marked_and_descriptions_are_shown() {
        let mut list = ChoiceList::new(options());

        assert_eq!(
            drawn(&mut list, 3),
            ["❯ first", "  second  with detail", "  something else"]
        );
    }

    #[test]
    fn a_prefix_leads_the_label() {
        let mut list = ChoiceList::new(vec![ChoiceItem::prefixed("Hi!", " 20h ago   ")]);

        assert_eq!(drawn(&mut list, 1), ["❯  20h ago   Hi!"]);
    }

    #[test]
    fn a_long_list_windows_around_the_selection_and_hints_at_the_rest() {
        let items = (0..6)
            .map(|index| ChoiceItem::choice(format!("item {index}")))
            .collect::<Vec<_>>();
        let mut list = ChoiceList::new(items).with_max_visible(Some(3));

        drive(&mut list, vec![KeyCode::Down, KeyCode::Down, KeyCode::Down]);

        assert_eq!(
            drawn(&mut list, 5),
            ["  ⋯", "  item 2", "❯ item 3", "  item 4", "  ⋯"]
        );
    }

    #[test]
    fn the_caret_follows_the_typed_text() {
        let mut list = ChoiceList::new(options());

        drive(
            &mut list,
            vec![KeyCode::Up, KeyCode::Char('a'), KeyCode::Char('b')],
        );
        drawn(&mut list, 3);

        assert_eq!(
            list.caret(),
            Some(Position::new(MARKER_WIDTH + 2, 2)),
            "after the marker and the two typed characters, on the third row"
        );
    }
}
