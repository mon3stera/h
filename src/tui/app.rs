use std::{
    io::stdout,
    panic,
    time::{Duration, Instant},
};

use futures::StreamExt;
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::{
        event::{
            DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent,
            KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
        },
        execute,
    },
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
};
use tokio::sync::{
    mpsc::{Receiver, Sender, UnboundedReceiver},
    oneshot,
};

use crate::{
    event::{AgentViewEvent, AskAnswer, UiRequest},
    tui::{
        choice_list::{ChoiceEvent, ChoiceItem, ChoiceList, ChoiceOutcome},
        input::Input,
        transcript::Transcript,
    },
    ui::{RenderUnit, ViewState, reduce_view_event},
};

const SPINNER: [(&str, &str); 4] = [
    ("◜", "h-..."),
    ("◝", "h-i..."),
    ("◞", "h-in..."),
    ("◟", "h-ing..."),
];

const SPINNER_PERIOD: Duration = Duration::from_millis(200);

/// The widest spinner word. Padding to it keeps the elapsed time from shuffling
/// left and right as the animation cycles.
const SPINNER_WIDTH: usize = 8;

/// How far the transcript moves for one page key.
const PAGE: isize = 10;

/// Rows per notch of the wheel.
const WHEEL_STEP: isize = 3;

/// The two rules and the question line around a set of options.
const ASK_FRAME: u16 = 3;

/// Runs the conversation view until the user quits.
///
/// Returns once Ctrl+C is pressed or the agent's event channel closes, at which
/// point dropping `committer` is what tells the worker to archive and stop.
pub async fn run(
    committer: Sender<String>,
    mut events: UnboundedReceiver<AgentViewEvent>,
    // Questions the agent is waiting on, each carrying the channel to answer on.
    mut requests: Receiver<UiRequest>,
    // What a resumed session already asked, so recall reaches back into it.
    history: Vec<String>,
) -> anyhow::Result<()> {
    let mut terminal = enter()?;
    let outcome = drive(
        &mut terminal,
        committer,
        &mut events,
        &mut requests,
        history,
    )
    .await;

    leave();
    outcome
}

/// Takes over the terminal, including the mouse so the wheel reaches us.
///
/// Capturing the mouse also takes the terminal's own selection, which most
/// terminals hand back while Shift is held.
fn enter() -> anyhow::Result<DefaultTerminal> {
    let terminal = ratatui::init();

    execute!(stdout(), EnableMouseCapture)?;

    // `ratatui::init` installs a hook that puts the screen back, but it knows
    // nothing about mouse capture; a panic would otherwise leave the terminal
    // reporting every mouse move as escape codes.
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), DisableMouseCapture);
        previous(info);
    }));

    Ok(terminal)
}

fn leave() {
    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
}

async fn drive(
    terminal: &mut DefaultTerminal,
    committer: Sender<String>,
    events: &mut UnboundedReceiver<AgentViewEvent>,
    requests: &mut Receiver<UiRequest>,
    history: Vec<String>,
) -> anyhow::Result<()> {
    let mut app = App::default();

    app.input.seed(history);
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
                Some(Ok(Event::Mouse(mouse))) => app.handle_mouse(mouse),
                // A closed input stream leaves nothing to drive the view.
                None => return Ok(()),
                _ => {}
            },
            event = events.recv() => match event {
                Some(event) => app.handle_agent_event(event),
                None => return Ok(()),
            },
            // Guarded: pulling a second question while one is unanswered would
            // strand the first, and the agent would wait on it forever.
            request = requests.recv(), if !app.is_asking() => {
                if let Some(request) = request {
                    app.begin_ask(request);
                }
            }
            _ = spinner.tick() => app.advance_spinner(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Flow {
    Continue,
    Quit,
}

/// A question from the agent, and the channel it is waiting on.
struct Asking {
    question: String,
    list: ChoiceList,
    reply: oneshot::Sender<AskAnswer>,
}

#[derive(Default)]
struct App {
    state: ViewState,
    transcript: Transcript,
    input: Input,
    /// Set while the agent is blocked on an answer. The prompt box steps aside
    /// for it, because nothing else can move until the question is settled.
    asking: Option<Asking>,
    spinner: usize,
    /// The transcript's height on the last draw, so scroll keys know the page
    /// size before the next one.
    viewport: u16,
    /// When the running turn began. A turn spans every provider request its tool
    /// calls set off, so this is not reset between them.
    started: Option<Instant>,
}

impl App {
    fn handle_agent_event(&mut self, event: AgentViewEvent) {
        match &event {
            AgentViewEvent::TurnStart => self.started = Some(Instant::now()),
            AgentViewEvent::TurnFinished { completed } => {
                if let Some(started) = self.started.take() {
                    if *completed {
                        self.state.units.push(RenderUnit::Done(started.elapsed()));
                    }
                }
            }
            _ => {}
        }

        if reduce_view_event(&mut self.state, event).is_err() {
            tracing::error!(
                event = "ui.view_event.failed",
                operation = "reduce_view_event",
                error_class = "view_event_parse_error"
            );
        }
    }

    fn is_asking(&self) -> bool {
        self.asking.is_some()
    }

    /// Puts a question on screen, offering the agent's options plus a row for an
    /// answer it did not think of.
    fn begin_ask(&mut self, request: UiRequest) {
        let UiRequest::Ask { question, reply } = request;

        let mut items = question
            .options
            .iter()
            .map(|option| match &option.description {
                Some(description) => {
                    ChoiceItem::described(option.label.clone(), description.clone())
                }
                None => ChoiceItem::choice(option.label.clone()),
            })
            .collect::<Vec<_>>();

        // The offered options are never the only answers available.
        items.push(ChoiceItem::free_text("something else"));

        self.asking = Some(Asking {
            question: question.question,
            list: ChoiceList::new(items),
            reply,
        });
    }

    fn answer(&mut self, outcome: ChoiceOutcome) {
        let Some(asking) = self.asking.take() else {
            return;
        };

        let answer = match outcome {
            ChoiceOutcome::Choice { index, label } => AskAnswer::Option { index, label },
            ChoiceOutcome::FreeText { text, .. } => AskAnswer::FreeText(text),
        };

        if asking.reply.send(answer).is_err() {
            tracing::warn!(event = "ui.ask.reply_failed", error_class = "caller_gone",);
        }
    }

    async fn handle_key(&mut self, key: KeyEvent, committer: &Sender<String>) -> Flow {
        if key.kind == KeyEventKind::Release {
            return Flow::Continue;
        }

        // Quitting outranks the question; the agent learns the answer was
        // abandoned when the reply channel closes with the process.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Flow::Quit;
        }

        // An unanswered question owns the keyboard.
        if let Some(asking) = &mut self.asking {
            match asking.list.handle_key(key) {
                ChoiceEvent::Idle => {}
                ChoiceEvent::Submitted(outcome) => self.answer(outcome),
                // Dropping the reply channel is how the tool hears "dismissed".
                ChoiceEvent::Dismissed => self.asking = None,
            }

            return Flow::Continue;
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

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let step = match mouse.kind {
            MouseEventKind::ScrollUp => -WHEEL_STEP,
            MouseEventKind::ScrollDown => WHEEL_STEP,
            _ => return,
        };

        self.transcript.scroll(step, self.viewport as usize);
    }

    fn advance_spinner(&mut self) {
        self.spinner = (self.spinner + 1) % SPINNER.len();
    }

    fn render(&mut self, frame: &mut Frame) {
        let indicator_height = u16::from(self.state.turn_in_progress);
        let [transcript, indicator, bottom] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(indicator_height),
            Constraint::Length(self.bottom_height()),
        ])
        .areas(frame.area());

        self.viewport = transcript.height;
        self.render_transcript(frame, transcript);

        if indicator_height > 0 {
            frame.render_widget(Paragraph::new(self.spinner_line()), indicator);
        }

        // The question takes the prompt box's place: answering it is the only
        // thing that moves the session forward.
        match &mut self.asking {
            Some(asking) => render_ask(frame, bottom, asking),
            None => self.input.render(frame, bottom),
        }
    }

    fn bottom_height(&self) -> u16 {
        match &self.asking {
            Some(asking) => asking.height(),
            None => self.input.height(),
        }
    }

    fn render_transcript(&mut self, frame: &mut Frame, area: Rect) {
        self.transcript.sync(&self.state, area.width as usize);

        let rows = self.transcript.visible(area.height as usize);

        frame.render_widget(Paragraph::new(rows.to_vec()), area);
    }

    fn spinner_line(&self) -> Line<'static> {
        let (glyph, word) = SPINNER[self.spinner];
        let elapsed = self
            .started
            .map(|started| format!(" ({}s)", started.elapsed().as_secs()))
            .unwrap_or_default();

        Line::from(vec![
            Span::styled(format!("{glyph} "), Style::default().fg(Color::Cyan)),
            Span::styled(
                format!("{word:<SPINNER_WIDTH$}{elapsed}"),
                Style::default().fg(Color::DarkGray),
            ),
        ])
    }
}

impl Asking {
    /// A rule, the question, and one row per option.
    fn height(&self) -> u16 {
        ASK_FRAME + self.list.len() as u16
    }
}

fn render_ask(frame: &mut Frame, area: Rect, asking: &mut Asking) {
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);

    frame.render_widget(block, area);

    let [question, options] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            asking.question.clone(),
            Style::default().fg(Color::Cyan),
        ))),
        question,
    );
    asking.list.render(frame, options);
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

    fn wheel(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn the_wheel_scrolls_the_transcript() {
        let (mut app, mut terminal) = app_with_size(40, 20);

        for index in 0..50 {
            app.state
                .units
                .push(RenderUnit::Prompt(format!("prompt {index}")));
        }
        app.state.revision += 1;
        drawn(&mut app, &mut terminal);

        app.handle_mouse(wheel(MouseEventKind::ScrollUp));
        assert!(!app.transcript.is_pinned(), "up moves away from the newest");

        app.handle_mouse(wheel(MouseEventKind::ScrollDown));
        app.handle_mouse(wheel(MouseEventKind::ScrollDown));
        assert!(
            app.transcript.is_pinned(),
            "coming back down resumes following"
        );
    }

    #[test]
    fn other_mouse_events_are_ignored() {
        let (mut app, mut terminal) = app_with_size(40, 20);

        for index in 0..50 {
            app.state
                .units
                .push(RenderUnit::Prompt(format!("prompt {index}")));
        }
        app.state.revision += 1;
        drawn(&mut app, &mut terminal);

        app.handle_mouse(wheel(MouseEventKind::Moved));

        assert!(
            app.transcript.is_pinned(),
            "only the wheel scrolls; a move must not"
        );
    }

    use crate::event::{AskOption, AskQuestion};

    fn ask(options: &[(&str, Option<&str>)]) -> (UiRequest, oneshot::Receiver<AskAnswer>) {
        let (reply, answer) = oneshot::channel();

        (
            UiRequest::Ask {
                question: AskQuestion {
                    question: "which way?".to_owned(),
                    options: options
                        .iter()
                        .map(|(label, description)| AskOption {
                            label: (*label).to_owned(),
                            description: description.map(str::to_owned),
                        })
                        .collect(),
                },
                reply,
            },
            answer,
        )
    }

    #[test]
    fn a_question_takes_the_place_of_the_prompt_box() {
        let (mut app, mut terminal) = app_with_size(40, 12);
        let (request, _answer) = ask(&[("left", Some("go left")), ("right", None)]);

        app.begin_ask(request);
        let rows = drawn(&mut app, &mut terminal);

        assert!(
            rows.iter().any(|row| row.contains("which way?")),
            "{rows:?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("❯ left  go left")),
            "{rows:?}"
        );
        assert!(rows.iter().any(|row| row.contains("right")), "{rows:?}");
        assert!(
            rows.iter().any(|row| row.contains("something else")),
            "an answer the agent did not offer is always available: {rows:?}"
        );
    }

    #[tokio::test]
    async fn choosing_an_option_answers_the_agent() {
        let (mut app, _) = app_with_size(40, 12);
        let (committer, _rx) = mpsc::channel(1);
        let (request, answer) = ask(&[("left", None), ("right", None)]);

        app.begin_ask(request);
        app.handle_key(press(KeyCode::Down, KeyModifiers::NONE), &committer)
            .await;
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE), &committer)
            .await;

        assert_eq!(
            answer.await.unwrap(),
            AskAnswer::Option {
                index: 1,
                label: "right".to_owned(),
            }
        );
        assert!(!app.is_asking(), "the question is settled");
    }

    #[tokio::test]
    async fn a_written_answer_reaches_the_agent() {
        let (mut app, _) = app_with_size(40, 12);
        let (committer, _rx) = mpsc::channel(1);
        let (request, answer) = ask(&[("left", None)]);

        app.begin_ask(request);
        // Up from the first row wraps onto the free-text row.
        app.handle_key(press(KeyCode::Up, KeyModifiers::NONE), &committer)
            .await;
        for character in "neither".chars() {
            app.handle_key(
                press(KeyCode::Char(character), KeyModifiers::NONE),
                &committer,
            )
            .await;
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE), &committer)
            .await;

        assert_eq!(
            answer.await.unwrap(),
            AskAnswer::FreeText("neither".to_owned())
        );
    }

    #[tokio::test]
    async fn dismissing_tells_the_agent_the_question_went_unanswered() {
        let (mut app, _) = app_with_size(40, 12);
        let (committer, _rx) = mpsc::channel(1);
        let (request, answer) = ask(&[("left", None)]);

        app.begin_ask(request);
        app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE), &committer)
            .await;

        assert!(!app.is_asking());
        assert!(
            answer.await.is_err(),
            "a dropped reply channel is how the tool hears it"
        );
    }

    #[tokio::test]
    async fn typing_while_asked_does_not_reach_the_prompt_box() {
        let (mut app, _) = app_with_size(40, 12);
        let (committer, _rx) = mpsc::channel(1);
        let (request, _answer) = ask(&[("left", None)]);

        app.begin_ask(request);
        for character in "hello".chars() {
            app.handle_key(
                press(KeyCode::Char(character), KeyModifiers::NONE),
                &committer,
            )
            .await;
        }

        assert!(app.is_asking(), "the question is still waiting");
        assert_eq!(
            app.input.text(),
            "",
            "keystrokes meant for the question must not land in the prompt box"
        );
    }

    /// The loop pulls the next request only while `!is_asking()`. This pins that
    /// predicate; the `select!` arm that reads it is not itself under test.
    #[tokio::test]
    async fn a_second_question_waits_until_the_first_is_settled() {
        let (mut app, _) = app_with_size(40, 12);
        let (committer, _rx) = mpsc::channel(1);
        let (first, answer) = ask(&[("a", None)]);

        app.begin_ask(first);
        assert!(app.is_asking(), "the next request stays queued");

        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE), &committer)
            .await;

        assert!(answer.await.is_ok());
        assert!(!app.is_asking(), "now the loop may take the next one");
    }

    #[tokio::test]
    async fn ctrl_c_outranks_a_pending_question() {
        let (mut app, _) = app_with_size(40, 12);
        let (committer, _rx) = mpsc::channel(1);
        let (request, _answer) = ask(&[("left", None)]);

        app.begin_ask(request);

        assert_eq!(
            app.handle_key(press(KeyCode::Char('c'), KeyModifiers::CONTROL), &committer)
                .await,
            Flow::Quit
        );
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
    fn the_padding_matches_the_widest_spinner_word() {
        assert_eq!(
            SPINNER.iter().map(|(_, word)| word.len()).max().unwrap(),
            SPINNER_WIDTH,
            "a wider word would push the elapsed time out of its column"
        );
    }

    #[test]
    fn the_elapsed_time_sits_in_the_same_column_whatever_the_frame() {
        let (mut app, mut terminal) = app_with_size(40, 8);
        app.state.turn_in_progress = true;
        app.handle_agent_event(AgentViewEvent::TurnStart);

        let column = |app: &mut App, terminal: &mut Terminal<TestBackend>| {
            drawn(app, terminal)
                .into_iter()
                .find(|row| row.contains("(0s)"))
                .and_then(|row| row.find("(0s)"))
        };

        let first = column(&mut app, &mut terminal);
        app.advance_spinner();
        app.advance_spinner();
        let later = column(&mut app, &mut terminal);

        assert!(first.is_some(), "the counter should be drawn");
        assert_eq!(first, later, "it must not shuffle as the animation cycles");
    }

    #[test]
    fn the_counter_appears_only_once_a_turn_is_running() {
        let (mut app, mut terminal) = app_with_size(40, 8);
        app.state.turn_in_progress = true;

        assert!(
            !drawn(&mut app, &mut terminal)
                .iter()
                .any(|row| row.contains("(")),
            "no turn has started, so there is nothing to count"
        );

        app.handle_agent_event(AgentViewEvent::TurnStart);

        assert!(
            drawn(&mut app, &mut terminal)
                .iter()
                .any(|row| row.contains("(0s)"))
        );
    }

    #[tokio::test]
    async fn a_finished_turn_leaves_a_summary_in_the_transcript() {
        let (mut app, mut terminal) = app_with_size(40, 10);

        app.handle_agent_event(AgentViewEvent::TurnStart);
        app.handle_agent_event(AgentViewEvent::TurnFinished { completed: true });

        assert!(
            drawn(&mut app, &mut terminal)
                .iter()
                .any(|row| row.starts_with("❃ Done for")),
            "the record of the wait should outlive the spinner"
        );
    }

    #[tokio::test]
    async fn a_failed_turn_is_not_summarised() {
        let (mut app, mut terminal) = app_with_size(40, 10);

        app.handle_agent_event(AgentViewEvent::TurnStart);
        app.handle_agent_event(AgentViewEvent::TurnFinished { completed: false });

        assert!(
            !drawn(&mut app, &mut terminal)
                .iter()
                .any(|row| row.contains("Done for")),
            "a turn that failed already said why"
        );
    }

    #[tokio::test]
    async fn a_tool_call_does_not_restart_the_clock() {
        let (mut app, _) = app_with_size(40, 10);

        app.handle_agent_event(AgentViewEvent::TurnStart);
        let started = app.started;

        // A tool round ends with `Completed`, and the next request follows inside
        // the same turn.
        app.handle_agent_event(AgentViewEvent::Completed);
        app.handle_agent_event(AgentViewEvent::TextDelta("more".to_owned()));

        assert_eq!(
            app.started, started,
            "the clock spans the whole turn, not one provider request"
        );
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
