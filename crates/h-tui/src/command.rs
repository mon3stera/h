use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use h_core::command::Command;

fn label_width() -> usize {
    Command::ALL
        .into_iter()
        .map(|command| command.label().len())
        .max()
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandEvent {
    Ignored,
    Consumed,
    Complete(Command),
    Submit(Command),
}

#[derive(Default)]
pub struct CommandMenu {
    input: String,
    matches: Vec<Command>,
    selected: usize,
    dismissed: bool,
}

impl CommandMenu {
    pub fn update(&mut self, input: &str) {
        if self.input == input {
            return;
        }

        self.input = input.to_owned();
        self.matches = Command::matching(input);
        self.selected = 0;
        self.dismissed = false;
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn len(&self) -> usize {
        if self.dismissed {
            0
        } else {
            self.matches.len()
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> CommandEvent {
        if self.dismissed || self.matches.is_empty() {
            return CommandEvent::Ignored;
        }

        let count = self.matches.len();

        match key.code {
            KeyCode::Up if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.selected = (self.selected + count - 1) % count;
                CommandEvent::Consumed
            }
            KeyCode::Down if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.selected = (self.selected + 1) % count;
                CommandEvent::Consumed
            }
            KeyCode::Tab => CommandEvent::Complete(self.matches[self.selected]),
            KeyCode::Enter => CommandEvent::Submit(self.matches[self.selected]),
            KeyCode::Esc => {
                self.dismissed = true;
                CommandEvent::Consumed
            }
            _ => CommandEvent::Ignored,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let lines = self
            .matches
            .iter()
            .enumerate()
            .map(|(index, command)| self.row(index, *command))
            .collect::<Vec<_>>();

        frame.render_widget(Paragraph::new(lines), area);
    }

    fn row(&self, index: usize, command: Command) -> Line<'static> {
        let selected = index == self.selected;
        let marker = if selected { "❯ " } else { "  " };
        let label = format!("{:<width$}", command.label(), width = label_width());
        let style = if selected {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };

        Line::from(vec![
            Span::styled(marker, style),
            Span::styled(label, style),
            Span::styled(
                format!("  {}", command.description()),
                Style::default().fg(Color::DarkGray),
            ),
        ])
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn drawn(menu: &mut CommandMenu) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(48, menu.len() as u16)).unwrap();

        terminal
            .draw(|frame| menu.render(frame, frame.area()))
            .unwrap();

        terminal
            .backend()
            .buffer()
            .content()
            .chunks(48)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .map(|row| row.trim_end().to_owned())
            .collect()
    }

    #[test]
    fn rows_show_commands_and_descriptions() {
        let mut menu = CommandMenu::default();
        menu.update("/");

        assert_eq!(
            drawn(&mut menu),
            [
                "❯ /clear    start a new session",
                "  /compact  compact this session",
            ]
        );
    }

    #[test]
    fn arrows_choose_and_enter_submits() {
        let mut menu = CommandMenu::default();
        menu.update("/");

        assert_eq!(
            menu.handle_key(press(KeyCode::Down)),
            CommandEvent::Consumed
        );
        assert_eq!(
            menu.handle_key(press(KeyCode::Enter)),
            CommandEvent::Submit(Command::Compact)
        );
    }

    #[test]
    fn typing_a_new_prefix_resets_the_selection() {
        let mut menu = CommandMenu::default();
        menu.update("/");
        menu.handle_key(press(KeyCode::Down));

        menu.update("/c");

        assert_eq!(
            menu.handle_key(press(KeyCode::Enter)),
            CommandEvent::Submit(Command::Clear)
        );
    }

    #[test]
    fn escape_hides_the_current_menu_until_the_input_changes() {
        let mut menu = CommandMenu::default();
        menu.update("/");

        assert_eq!(menu.handle_key(press(KeyCode::Esc)), CommandEvent::Consumed);
        assert_eq!(menu.len(), 0);

        menu.update("/c");
        assert_eq!(menu.len(), 2);
    }
}
