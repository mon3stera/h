use std::time::Duration;

use futures::StreamExt;
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use tokio::sync::mpsc::{Receiver, Sender, UnboundedReceiver};

use crate::{
    event::{AgentViewEvent, UiRequest},
    tui::{input::Input, transcript::Transcript},
    ui::{RenderUnit, ViewState, reduce_view_event},
};

const SPINNER: [(&str, &str); 4] = [
    ("◜", "h-..."),
    ("◝", "h-i..."),
    ("◞", "h-in..."),
    ("◟", "h-ing..."),
];

const SPINNER_PERIOD: Duration = Duration::from_millis(200);

/// How far the transcript moves for one page key.
const PAGE: isize = 10;

/// Runs the conversation view until the user quits.
///
/// Returns once Ctrl+C is pressed or the agent's event channel closes, at which
/// point dropping `committer` is what tells the worker to archive and stop.
pub async fn run(
    committer: Sender<String>,
    mut events: UnboundedReceiver<AgentViewEvent>,
    // Questions the agent is waiting on. Answering them is not wired up yet, the
    // same as before the move to ratatui.
    _requests: Receiver<UiRequest>,
) -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    let outcome = drive(&mut terminal, committer, &mut events).await;

    ratatui::restore();
    outcome
}

async fn drive(
    terminal: &mut DefaultTerminal,
    committer: Sender<String>,
    events: &mut UnboundedReceiver<AgentViewEvent>,
) -> anyhow::Result<()> {
    let mut app = App::default();
    let mut keys = EventStream::new();
    let mut spinner = tokio::time::interval(SPINNER_PERIOD);

    spinner.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        terminal.draw(|frame| app.render(frame))?;

        tokio::select! {
            key = keys.next() => match key {
                Some(Ok(Event::Key(key))) => {
                    if app.handle_key(key, &committer).await == Flow::Quit {
                        return Ok(());
                    }
                }
                // A closed input stream leaves nothing to drive the view.
                None => return Ok(()),
                _ => {}
            },
            event = events.recv() => match event {
                Some(event) => app.handle_agent_event(event),
                None => return Ok(()),
            },
            _ = spinner.tick() => app.advance_spinner(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Flow {
    Continue,
    Quit,
}

#[derive(Default)]
struct App {
    state: ViewState,
    transcript: Transcript,
    input: Input,
    spinner: usize,
    /// The transcript's height on the last draw, so scroll keys know the page
    /// size before the next one.
    viewport: u16,
}

impl App {
    fn handle_agent_event(&mut self, event: AgentViewEvent) {
        if reduce_view_event(&mut self.state, event).is_err() {
            tracing::error!(
                event = "ui.view_event.failed",
                operation = "reduce_view_event",
                error_class = "view_event_parse_error"
            );
        }
    }

    async fn handle_key(&mut self, key: KeyEvent, committer: &Sender<String>) -> Flow {
        if key.kind == KeyEventKind::Release {
            return Flow::Continue;
        }

        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Flow::Quit;
        }

        let page = self.viewport as isize;

        match key.code {
            KeyCode::PageUp => self.transcript.scroll(-page, self.viewport as usize),
            KeyCode::PageDown => self.transcript.scroll(page, self.viewport as usize),
            // Plain Up and Down belong to the prompt box; scrolling takes Shift.
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.transcript.scroll(-PAGE, self.viewport as usize);
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.transcript.scroll(PAGE, self.viewport as usize);
            }
            _ => {
                if let Some(prompt) = self.input.handle_key(key) {
                    self.submit(prompt, committer).await;
                }
            }
        }

        Flow::Continue
    }

    async fn submit(&mut self, prompt: String, committer: &Sender<String>) {
        // Echo it locally: nothing on the event bus carries the user's own turn
        // back to the view.
        self.state.units.push(RenderUnit::Prompt(prompt.clone()));
        self.state.revision += 1;

        // A prompt is why the newest output matters, so follow it again.
        self.transcript.pin();

        tracing::info!(event = "ui.prompt_submitted");

        if committer.send(prompt).await.is_err() {
            tracing::warn!(
                event = "ui.prompt_send.failed",
                operation = "prompt_channel_send",
                error_class = "prompt_channel_closed"
            );
        }
    }

    fn advance_spinner(&mut self) {
        self.spinner = (self.spinner + 1) % SPINNER.len();
    }

    fn render(&mut self, frame: &mut Frame) {
        let indicator_height = u16::from(self.state.turn_in_progress);
        let [transcript, indicator, input] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(indicator_height),
            Constraint::Length(self.input.height()),
        ])
        .areas(frame.area());

        self.viewport = transcript.height;
        self.render_transcript(frame, transcript);

        if indicator_height > 0 {
            frame.render_widget(Paragraph::new(self.spinner_line()), indicator);
        }

        self.input.render(frame, input);
    }

    fn render_transcript(&mut self, frame: &mut Frame, area: Rect) {
        self.transcript.sync(&self.state, area.width as usize);

        let rows = self.transcript.visible(area.height as usize);

        frame.render_widget(Paragraph::new(rows.to_vec()), area);
    }

    fn spinner_line(&self) -> Line<'static> {
        let (glyph, word) = SPINNER[self.spinner];

        Line::from(vec![
            Span::styled(format!("{glyph} "), Style::default().fg(Color::Cyan)),
            Span::styled(word, Style::default().fg(Color::DarkGray)),
        ])
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};
    use tokio::sync::mpsc;

    use super::*;

    fn app_with_size(width: u16, height: u16) -> (App, Terminal<TestBackend>) {
        (
            App::default(),
            Terminal::new(TestBackend::new(width, height)).unwrap(),
        )
    }

    fn drawn(app: &mut App, terminal: &mut Terminal<TestBackend>) -> Vec<String> {
        terminal.draw(|frame| app.render(frame)).unwrap();

        let width = terminal.backend().buffer().area.width as usize;

        terminal
            .backend()
            .buffer()
            .content()
            .chunks(width)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .map(|row| row.trim_end().to_owned())
            .collect()
    }

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[tokio::test]
    async fn ctrl_c_quits() {
        let (mut app, _) = app_with_size(40, 10);
        let (committer, _rx) = mpsc::channel(1);

        assert_eq!(
            app.handle_key(press(KeyCode::Char('c'), KeyModifiers::CONTROL), &committer)
                .await,
            Flow::Quit
        );
    }

    #[tokio::test]
    async fn a_submitted_prompt_is_echoed_and_sent() {
        let (mut app, _) = app_with_size(40, 10);
        let (committer, mut received) = mpsc::channel(1);

        for character in "hello".chars() {
            app.handle_key(
                press(KeyCode::Char(character), KeyModifiers::NONE),
                &committer,
            )
            .await;
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::ALT), &committer)
            .await;

        assert_eq!(received.recv().await, Some("hello".to_owned()));
        assert!(
            matches!(app.state.units.as_slice(), [RenderUnit::Prompt(text)] if text == "hello"),
            "the view has to echo the user's own turn"
        );
    }

    #[tokio::test]
    async fn typing_does_not_scroll_the_transcript() {
        let (mut app, _) = app_with_size(40, 10);
        let (committer, _rx) = mpsc::channel(1);

        app.handle_key(press(KeyCode::Up, KeyModifiers::NONE), &committer)
            .await;

        assert!(
            app.transcript.is_pinned(),
            "a bare arrow belongs to the prompt box"
        );
    }

    #[tokio::test]
    async fn shift_up_scrolls_the_transcript() {
        let (mut app, mut terminal) = app_with_size(40, 20);
        let (committer, _rx) = mpsc::channel(1);

        for index in 0..50 {
            app.state
                .units
                .push(RenderUnit::Prompt(format!("prompt {index}")));
        }
        app.state.revision += 1;
        drawn(&mut app, &mut terminal);

        app.handle_key(press(KeyCode::Up, KeyModifiers::SHIFT), &committer)
            .await;

        assert!(!app.transcript.is_pinned());
    }

    #[tokio::test]
    async fn submitting_returns_to_the_newest_output() {
        let (mut app, mut terminal) = app_with_size(40, 20);
        let (committer, _rx) = mpsc::channel(1);

        for index in 0..50 {
            app.state
                .units
                .push(RenderUnit::Prompt(format!("prompt {index}")));
        }
        app.state.revision += 1;
        drawn(&mut app, &mut terminal);
        app.handle_key(press(KeyCode::Up, KeyModifiers::SHIFT), &committer)
            .await;

        for character in "hi".chars() {
            app.handle_key(
                press(KeyCode::Char(character), KeyModifiers::NONE),
                &committer,
            )
            .await;
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::ALT), &committer)
            .await;

        assert!(app.transcript.is_pinned());
    }

    #[test]
    fn the_spinner_only_takes_a_row_while_a_turn_runs() {
        let (mut app, mut terminal) = app_with_size(40, 8);

        let idle = drawn(&mut app, &mut terminal);
        assert!(!idle.iter().any(|row| row.contains("h-")), "{idle:?}");

        app.state.turn_in_progress = true;
        let busy = drawn(&mut app, &mut terminal);

        assert!(busy.iter().any(|row| row.contains("h-...")), "{busy:?}");
    }

    #[test]
    fn the_spinner_advances_through_its_frames() {
        let (mut app, mut terminal) = app_with_size(40, 8);
        app.state.turn_in_progress = true;

        app.advance_spinner();
        let rows = drawn(&mut app, &mut terminal);

        assert!(rows.iter().any(|row| row.contains("h-i...")), "{rows:?}");
    }

    #[test]
    fn the_conversation_and_the_prompt_box_share_the_screen() {
        let (mut app, mut terminal) = app_with_size(40, 8);
        app.state.units.push(RenderUnit::Prompt("asked".to_owned()));
        app.state.revision += 1;

        let rows = drawn(&mut app, &mut terminal);

        assert!(rows.iter().any(|row| row.contains("❯ asked")), "{rows:?}");
        assert!(
            rows.iter().any(|row| row.contains("─")),
            "the prompt box keeps its rules: {rows:?}"
        );
    }

    #[tokio::test]
    async fn an_agent_event_reaches_the_view() {
        let (mut app, mut terminal) = app_with_size(40, 10);

        app.handle_agent_event(AgentViewEvent::TextDelta("streamed".to_owned()));
        app.handle_agent_event(AgentViewEvent::Completed);

        assert!(
            drawn(&mut app, &mut terminal)
                .iter()
                .any(|row| row.contains("streamed"))
        );
    }
}
