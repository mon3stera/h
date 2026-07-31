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
            DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
            Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent,
            MouseEventKind,
        },
        execute,
    },
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
};
use ratatui_image::picker::Picker;
use tokio::sync::{
    mpsc::{Receiver, Sender, UnboundedReceiver},
    oneshot,
};
use unicode_width::UnicodeWidthStr;

use h_core::{
    command::Command,
    event::{AgentCommand, AgentViewEvent},
    input::UserInput,
    interaction::{AskAnswer, Request},
};

use crate::{
    choice_list::{ChoiceEvent, ChoiceItem, ChoiceList, ChoiceOutcome},
    clipboard::{self, Content as ClipboardContent},
    command::{CommandEvent, CommandMenu},
    format_tokens,
    input::Input,
    rainbow_spans,
    transcript::Transcript,
    ui::{RenderUnit, ViewState, reduce_view_event},
};

/// The rotating glyphs. The word itself stays still; its colors chase instead.
const SPINNER: [&str; 4] = ["◜", "◝", "◟", "◞"];

const SPINNER_PERIOD: Duration = Duration::from_millis(200);

/// The spinner word, always shown whole.
const SPINNER_WORD: &str = "h-ing...";

/// One chase step per character of [`SPINNER_WORD`] (ASCII, so byte length
/// equals character count), plus a rest frame where the whole word is back in
/// the default color before the gray wave restarts.
const SPINNER_CHASE_PERIOD: usize = SPINNER_WORD.len() + 1;

/// How far the transcript moves for one page key.
const PAGE: isize = 10;

/// Rows per notch of the wheel.
const WHEEL_STEP: isize = 3;

/// The two rules and the question line around a set of options.
const ASK_FRAME: u16 = 3;

const STATUS_HEIGHT: u16 = 1;

/// Runs the conversation view until the user quits.
///
/// Returns once Ctrl+C is pressed or the agent's event channel closes, at which
/// point dropping `commands` tells the worker to cancel, archive, and stop.
pub async fn run(
    commands: Sender<AgentCommand>,
    mut events: UnboundedReceiver<AgentViewEvent>,
    // Questions the agent is waiting on, each carrying the channel to answer on.
    mut requests: Receiver<Request>,
    // What a resumed session already asked, so recall reaches back into it.
    history: Vec<String>,
    context_window: usize,
) -> anyhow::Result<()> {
    let (mut terminal, picker) = enter()?;
    let outcome = drive(
        &mut terminal,
        picker,
        commands,
        &mut events,
        &mut requests,
        history,
        context_window,
    )
    .await;

    leave();
    outcome
}

/// Takes over the terminal, including the mouse so the wheel reaches us.
///
/// Capturing the mouse also takes the terminal's own selection, which most
/// terminals hand back while Shift is held.
fn enter() -> anyhow::Result<(DefaultTerminal, Picker)> {
    let terminal = ratatui::init();
    let picker = match Picker::from_query_stdio() {
        Ok(picker) => picker,
        Err(error) => {
            tracing::warn!(
                event = "ui.image_protocol.detect_failed",
                error = error.to_string(),
            );

            Picker::halfblocks()
        }
    };

    tracing::info!(
        event = "ui.image_protocol.selected",
        protocol = ?picker.protocol_type(),
    );

    execute!(stdout(), EnableMouseCapture, EnableBracketedPaste)?;

    // `ratatui::init` installs a hook that puts the screen back, but it knows
    // nothing about mouse capture; a panic would otherwise leave the terminal
    // reporting every mouse move as escape codes.
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), DisableBracketedPaste, DisableMouseCapture);
        previous(info);
    }));

    Ok((terminal, picker))
}

fn leave() {
    let _ = execute!(stdout(), DisableBracketedPaste, DisableMouseCapture);
    ratatui::restore();
}

async fn drive(
    terminal: &mut DefaultTerminal,
    picker: Picker,
    commands: Sender<AgentCommand>,
    events: &mut UnboundedReceiver<AgentViewEvent>,
    requests: &mut Receiver<Request>,
    history: Vec<String>,
    context_window: usize,
) -> anyhow::Result<()> {
    let mut app = App {
        context_window,
        input: Input::new(picker),
        ..App::default()
    };

    app.input.seed(history);
    let mut keys = EventStream::new();
    let mut spinner = tokio::time::interval(SPINNER_PERIOD);

    spinner.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        terminal.draw(|frame| app.render(frame))?;

        tokio::select! {
            key = keys.next() => match key {
                Some(Ok(Event::Key(key))) => {
                    if app.handle_key(key, &commands).await == Flow::Quit {
                        return Ok(());
                    }
                }
                Some(Ok(Event::Mouse(mouse))) => app.handle_mouse(mouse),
                Some(Ok(Event::Paste(text))) => app.handle_paste(&text),
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
    command_menu: CommandMenu,
    /// A submitted slash command awaiting its completion boundary. Keeping the
    /// input disabled prevents prompts and commands from being queued behind a
    /// context mutation whose result they cannot yet observe.
    pending_command: Option<Command>,
    /// Set while the agent is blocked on an answer. The prompt box steps aside
    /// for it, because nothing else can move until the question is settled.
    asking: Option<Asking>,
    spinner: usize,
    /// Where the gray wave sits in the spinner word: one character turns gray
    /// at a time, left to right, then the whole word is plain again.
    chase: usize,
    /// The transcript's height on the last draw, so scroll keys know the page
    /// size before the next one.
    viewport: u16,
    /// When the running turn began. A turn spans every provider request its tool
    /// calls set off, so this is not reset between them.
    started: Option<Instant>,
    context_window: usize,
}

impl App {
    fn handle_agent_event(&mut self, event: AgentViewEvent) {
        match &event {
            AgentViewEvent::TurnStart => {
                self.started = Some(Instant::now());
                // Each turn opens with the whole word plain before the gray
                // wave starts moving through it.
                self.chase = 0;
            }
            AgentViewEvent::TurnFinished { completed } => {
                // A cancelled `ask` keeps its reply sender alive until the
                // agent has observed cancellation and written the interrupted
                // tool result. Dropping it earlier could race into an ordinary
                // "question dismissed" result instead.
                self.asking = None;

                if let Some(started) = self.started.take()
                    && *completed
                {
                    self.state
                        .units
                        .push(RenderUnit::Done(started.elapsed(), self.state.turn_tokens));
                }
            }
            AgentViewEvent::SessionStarted => {
                self.started = None;
                self.asking = None;
                self.pending_command = None;
                self.input.reset();
                self.command_menu.reset();
                self.transcript.pin();
            }
            AgentViewEvent::CommandFinished(command) if self.pending_command == Some(*command) => {
                self.pending_command = None;
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
    fn begin_ask(&mut self, request: Request) {
        let Request::Ask { question, reply } = request;

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

    async fn handle_key(&mut self, key: KeyEvent, commands: &Sender<AgentCommand>) -> Flow {
        if key.kind == KeyEventKind::Release {
            return Flow::Continue;
        }

        // Quitting outranks the question; the agent learns the answer was
        // abandoned when the reply channel closes with the process.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Flow::Quit;
        }

        if self.asking.is_some() && key.code == KeyCode::Esc {
            // Keep an outstanding ask alive until TurnFinished so cancellation
            // wins over its reply channel closing; the agent can then record
            // the canonical interrupted ToolResult.
            self.cancel(commands).await;

            return Flow::Continue;
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

        if self.pending_command.is_some() {
            if key.code == KeyCode::Esc {
                self.cancel(commands).await;
            }

            return Flow::Continue;
        }

        if key.code == KeyCode::Char('v') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.paste_clipboard().await;

            return Flow::Continue;
        }

        self.update_command_menu();

        match self.command_menu.handle_key(key) {
            CommandEvent::Ignored => {}
            CommandEvent::Consumed => return Flow::Continue,
            CommandEvent::Complete(command) => {
                self.input.replace(command.label());
                self.command_menu.update(command.label());

                return Flow::Continue;
            }
            CommandEvent::Submit(command) => {
                self.input.replace(command.label());

                if let Some(input) = self.input.take() {
                    self.command_menu.reset();
                    self.submit(input, commands).await;
                }

                return Flow::Continue;
            }
        }

        if key.code == KeyCode::Esc && self.input.attachments_focused() {
            self.input.handle_key(key);

            return Flow::Continue;
        }

        if key.code == KeyCode::Esc {
            self.cancel(commands).await;

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
                if let Some(input) = self.input.handle_key(key) {
                    self.submit(input, commands).await;
                }

                self.update_command_menu();
            }
        }

        Flow::Continue
    }

    async fn submit(&mut self, input: UserInput, commands: &Sender<AgentCommand>) {
        if self.pending_command.is_some() {
            tracing::warn!(event = "ui.submission_blocked", reason = "command_pending",);

            return;
        }

        let text = input.text();
        if !input.has_images()
            && let Some(command) = Command::parse(&text)
        {
            tracing::info!(event = "ui.command_submitted", command = command.label());

            match commands.send(AgentCommand::Run(command)).await {
                Ok(()) => self.pending_command = Some(command),
                Err(_) => {
                    tracing::warn!(
                        event = "ui.command_send.failed",
                        operation = "command_channel_send",
                        error_class = "command_channel_closed"
                    );
                }
            }

            return;
        }

        // Echo it locally: nothing on the event bus carries the user's own turn
        // back to the view.
        self.state.units.push(RenderUnit::Prompt(input.display()));
        self.state.revision += 1;

        // A prompt is why the newest output matters, so follow it again.
        self.transcript.pin();

        tracing::info!(event = "ui.prompt_submitted");

        if commands.send(AgentCommand::Prompt(input)).await.is_err() {
            tracing::warn!(
                event = "ui.prompt_send.failed",
                operation = "prompt_channel_send",
                error_class = "prompt_channel_closed"
            );
        }
    }

    async fn paste_clipboard(&mut self) {
        match clipboard::read().await {
            Ok(ClipboardContent::Image(image)) => self.input.add_image(image),
            Ok(ClipboardContent::Text(text)) => self.input.insert_text(&text),
            Err(error) => {
                self.state
                    .units
                    .push(RenderUnit::Err(format!("Clipboard: {error}")));
                self.state.revision += 1;
                self.transcript.pin();
            }
        }

        self.update_command_menu();
    }

    fn handle_paste(&mut self, text: &str) {
        if self.asking.is_some() || self.pending_command.is_some() {
            return;
        }

        self.input.insert_text(text);
        self.update_command_menu();
    }

    fn update_command_menu(&mut self) {
        if self.input.has_images() {
            self.command_menu.reset();
        } else {
            self.command_menu.update(&self.input.text());
        }
    }

    async fn cancel(&self, commands: &Sender<AgentCommand>) {
        tracing::info!(event = "ui.turn_cancel_requested");

        if commands.send(AgentCommand::Cancel).await.is_err() {
            tracing::warn!(
                event = "ui.turn_cancel_send.failed",
                operation = "command_channel_send",
                error_class = "command_channel_closed"
            );
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if self.asking.is_none() && self.pending_command.is_none() && self.input.handle_mouse(mouse)
        {
            self.update_command_menu();

            return;
        }

        let step = match mouse.kind {
            MouseEventKind::ScrollUp => -WHEEL_STEP,
            MouseEventKind::ScrollDown => WHEEL_STEP,
            _ => return,
        };

        self.transcript.scroll(step, self.viewport as usize);
    }

    fn advance_spinner(&mut self) {
        self.spinner = (self.spinner + 1) % SPINNER.len();
        self.chase = (self.chase + 1) % SPINNER_CHASE_PERIOD;
    }

    fn render(&mut self, frame: &mut Frame) {
        let indicator_height = u16::from(self.state.turn_in_progress)
            .saturating_add(u16::from(self.pending_command.is_some()));
        // A gap on each side keeps the indicator from touching either the
        // transcript above or the input box below.
        let indicator_gap = u16::from(self.state.turn_in_progress);
        let [transcript, _above_gap, indicator, _below_gap, bottom] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(indicator_gap),
            Constraint::Length(indicator_height),
            Constraint::Length(indicator_gap),
            Constraint::Length(self.bottom_height(frame.area().width)),
        ])
        .areas(frame.area());

        self.viewport = transcript.height;
        self.render_transcript(frame, transcript);

        if indicator_height > 0 {
            frame.render_widget(Paragraph::new(self.indicator_lines()), indicator);
        }

        let (composer_height, command_height) = self.composer_heights(frame.area().width);
        let [composer, command_menu, status] = Layout::vertical([
            Constraint::Length(composer_height),
            Constraint::Length(command_height),
            Constraint::Length(STATUS_HEIGHT),
        ])
        .areas(bottom);

        // The question takes the prompt box's place: answering it is the only
        // thing that moves the session forward.
        match &mut self.asking {
            Some(asking) => render_ask(frame, composer, asking),
            None => self.input.render(frame, composer),
        }

        if command_height > 0 {
            self.command_menu.render(frame, command_menu);
        }

        let context_width = self
            .context_spans()
            .iter()
            .map(|span| span.content.width())
            .sum::<usize>() as u16;
        let [left, right] =
            Layout::horizontal([Constraint::Min(0), Constraint::Max(context_width)]).areas(status);

        frame.render_widget(Paragraph::new(self.model_line()), left);
        frame.render_widget(Paragraph::new(self.context_line()), right);
    }

    fn bottom_height(&self, width: u16) -> u16 {
        let (composer, commands) = self.composer_heights(width);

        composer
            .saturating_add(commands)
            .saturating_add(STATUS_HEIGHT)
    }

    fn composer_heights(&self, width: u16) -> (u16, u16) {
        let (composer, commands) = (
            match &self.asking {
                Some(asking) => asking.height(),
                None => self.input.height(width),
            },
            if self.asking.is_none() {
                self.command_menu.len().try_into().unwrap_or(u16::MAX)
            } else {
                0
            },
        );

        (composer, commands)
    }

    fn render_transcript(&mut self, frame: &mut Frame, area: Rect) {
        self.transcript.sync(&self.state, area.width as usize);

        let rows = self.transcript.visible(area.height as usize);

        frame.render_widget(Paragraph::new(rows.to_vec()), area);
    }

    fn spinner_line(&self) -> Line<'static> {
        let glyph = SPINNER[self.spinner];
        let elapsed = self
            .started
            .map(|started| {
                let tokens = self
                    .state
                    .turn_tokens
                    .map(|tokens| format!(" ↓ {}", format_tokens(tokens)))
                    .unwrap_or_default();

                format!(" ({}s{tokens})", started.elapsed().as_secs())
            })
            .unwrap_or_default();

        let mut spans = vec![Span::styled(
            format!("{glyph} "),
            Style::default().fg(Color::Cyan),
        )];
        spans.extend(self.chase_word());
        spans.push(Span::styled(elapsed, Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            "  Esc to cancel",
            Style::default().fg(Color::DarkGray),
        ));

        Line::from(spans)
    }

    /// The spinner word with one character grayed out at a time, moving left
    /// to right. The first step of each cycle shows the whole word in the
    /// default color.
    fn chase_word(&self) -> Vec<Span<'static>> {
        let gray = Style::default().fg(Color::DarkGray);
        let gray_position = self.chase.checked_sub(1);

        SPINNER_WORD
            .chars()
            .enumerate()
            .map(|(position, character)| {
                if gray_position == Some(position) {
                    Span::styled(character.to_string(), gray)
                } else {
                    Span::raw(character.to_string())
                }
            })
            .collect()
    }

    fn indicator_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::with_capacity(2);

        if self.state.turn_in_progress {
            lines.push(self.spinner_line());
        }

        if let Some(command) = self.pending_command {
            lines.push(self.command_line(command));
        }

        lines
    }

    fn command_line(&self, command: Command) -> Line<'static> {
        let (glyph, label) = (
            SPINNER[self.spinner],
            match command {
                Command::Clear => "starting session...",
                Command::Compact => "compacting...",
            },
        );

        Line::from(vec![
            Span::styled(format!("{glyph} "), Style::default().fg(Color::Yellow)),
            Span::styled(label, Style::default().fg(Color::Yellow)),
            Span::styled("  input disabled", Style::default().fg(Color::DarkGray)),
        ])
    }

    /// The left end of the status line: the model name, with the thinking
    /// effort when the provider reports one.
    fn model_line(&self) -> Line<'static> {
        let muted = Style::default().fg(Color::DarkGray);
        let mut spans = Vec::new();

        if let Some(startup) = &self.state.startup {
            spans.push(Span::raw(startup.model.clone()));
            if let Some(effort) = &startup.thinking_effort {
                spans.push(Span::styled(format!(" · {effort}"), muted));
            }
        }

        Line::from(spans)
    }

    fn context_spans(&self) -> Vec<Span<'static>> {
        let (current, percent, remaining) = match self.state.context_tokens {
            Some(current) if self.context_window > 0 => {
                let remaining = self.context_window.saturating_sub(current);

                (
                    format_tokens(current),
                    format!(
                        "{:.1}% left",
                        remaining as f64 / self.context_window as f64 * 100.0
                    ),
                    Some(remaining as f64 / self.context_window as f64),
                )
            }
            Some(current) => (format_tokens(current), "?% left".to_owned(), None),
            None => ("?".to_owned(), "?% left".to_owned(), None),
        };
        let limit = format_tokens(self.context_window);
        let muted = Style::default().fg(Color::DarkGray);
        let mut spans = vec![Span::styled(format!("context {current}/{limit} ("), muted)];

        if let Some(fraction) = remaining {
            spans.extend(context_bar(fraction, muted));
            spans.push(Span::raw(" "));
        }

        spans.extend(rainbow_spans(&percent, Style::default()));
        spans.push(Span::styled(")", muted));

        spans
    }

    fn context_line(&self) -> Line<'static> {
        Line::from(self.context_spans()).alignment(Alignment::Right)
    }
}

/// The remaining-context bar: ten cells, the filled share along the rainbow
/// ramp and the rest in muted gray, bracketed.
fn context_bar(fraction: f64, muted: Style) -> Vec<Span<'static>> {
    let filled = (fraction * 10.0).round() as usize;
    let mut spans = vec![Span::styled("[", muted)];

    if filled > 0 {
        spans.extend(rainbow_spans(&"█".repeat(filled), Style::default()));
    }
    if filled < 10 {
        spans.push(Span::styled("░".repeat(10 - filled), muted));
    }

    spans.push(Span::styled("]", muted));
    spans
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
    use h_core::input::Image;
    use ratatui::{Terminal, backend::TestBackend};
    use tokio::sync::mpsc;

    use super::*;

    fn app_with_size(width: u16, height: u16) -> (App, Terminal<TestBackend>) {
        (
            App {
                context_window: 200_000,
                ..App::default()
            },
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
    async fn escape_sends_a_turn_cancellation() {
        let (mut app, _) = app_with_size(40, 10);
        let (committer, mut received) = mpsc::channel(1);

        assert_eq!(
            app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE), &committer)
                .await,
            Flow::Continue
        );
        assert_eq!(received.recv().await, Some(AgentCommand::Cancel));
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

        assert_eq!(
            received.recv().await,
            Some(AgentCommand::Prompt("hello".into()))
        );
        assert!(
            matches!(app.state.units.as_slice(), [RenderUnit::Prompt(text)] if text == "hello"),
            "the view has to echo the user's own turn"
        );
    }

    #[tokio::test]
    async fn an_attachment_prevents_slash_text_from_becoming_a_command() {
        let (mut app, _) = app_with_size(48, 10);
        let (committer, mut received) = mpsc::channel(1);

        app.handle_paste("/clear");
        app.input
            .add_image(Image::new("image/png", [1, 2, 3], 32, 32).unwrap());
        app.handle_key(press(KeyCode::Enter, KeyModifiers::ALT), &committer)
            .await;

        let Some(AgentCommand::Prompt(input)) = received.recv().await else {
            panic!("an image-bearing slash prompt must remain a prompt");
        };

        assert_eq!(input.text(), "/clear");
        assert_eq!(input.image_count(), 1);
        assert!(app.pending_command.is_none());
    }

    #[test]
    fn bracketed_paste_inserts_text_at_the_cursor() {
        let (mut app, _) = app_with_size(48, 10);

        app.handle_paste("pasted\ntext");

        assert_eq!(app.input.text(), "pasted\ntext");
    }

    #[tokio::test]
    async fn a_slash_shows_commands_and_descriptions() {
        let (mut app, mut terminal) = app_with_size(48, 10);
        let (committer, _rx) = mpsc::channel(1);

        app.handle_key(press(KeyCode::Char('/'), KeyModifiers::NONE), &committer)
            .await;

        let rows = drawn(&mut app, &mut terminal);

        assert!(
            rows.iter()
                .any(|row| row.contains("❯ /clear    start a new session")),
            "{rows:?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("/compact  compact this session")),
            "{rows:?}"
        );
    }

    #[tokio::test]
    async fn command_prefixes_filter_the_menu() {
        let (mut app, mut terminal) = app_with_size(48, 10);
        let (committer, _rx) = mpsc::channel(1);

        for character in "/co".chars() {
            app.handle_key(
                press(KeyCode::Char(character), KeyModifiers::NONE),
                &committer,
            )
            .await;
        }

        let rows = drawn(&mut app, &mut terminal);

        assert!(rows.iter().any(|row| row.contains("/compact")), "{rows:?}");
        assert!(!rows.iter().any(|row| row.contains("/clear")), "{rows:?}");
    }

    #[tokio::test]
    async fn command_selection_is_sent_without_becoming_a_prompt() {
        let (mut app, _) = app_with_size(48, 10);
        let (committer, mut received) = mpsc::channel(1);

        app.handle_key(press(KeyCode::Char('/'), KeyModifiers::NONE), &committer)
            .await;
        app.handle_key(press(KeyCode::Down, KeyModifiers::NONE), &committer)
            .await;
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE), &committer)
            .await;

        assert_eq!(
            received.recv().await,
            Some(AgentCommand::Run(Command::Compact))
        );
        assert!(app.state.units.is_empty());
        assert_eq!(app.input.text(), "");
    }

    #[tokio::test]
    async fn compact_shows_progress_and_blocks_submissions_until_finished() {
        let (mut app, mut terminal) = app_with_size(48, 10);
        let (committer, mut received) = mpsc::channel(2);

        app.handle_key(press(KeyCode::Char('/'), KeyModifiers::NONE), &committer)
            .await;
        app.handle_key(press(KeyCode::Down, KeyModifiers::NONE), &committer)
            .await;
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE), &committer)
            .await;

        assert_eq!(
            received.recv().await,
            Some(AgentCommand::Run(Command::Compact))
        );

        let rows = drawn(&mut app, &mut terminal);
        assert!(
            rows.iter()
                .any(|row| row.contains("compacting...") && row.contains("input disabled")),
            "{rows:?}"
        );

        app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE), &committer)
            .await;
        app.handle_key(press(KeyCode::Enter, KeyModifiers::ALT), &committer)
            .await;

        assert_eq!(app.input.text(), "");
        assert!(
            received.try_recv().is_err(),
            "no prompt or command may queue behind compaction"
        );

        app.handle_agent_event(AgentViewEvent::CommandFinished(Command::Compact));
        app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE), &committer)
            .await;

        assert_eq!(app.input.text(), "x");
        assert!(
            drawn(&mut app, &mut terminal)
                .iter()
                .all(|row| !row.contains("compacting..."))
        );
    }

    #[tokio::test]
    async fn clear_blocks_new_input_until_the_session_boundary_finishes() {
        let (mut app, _) = app_with_size(48, 10);
        let (committer, mut received) = mpsc::channel(1);

        app.handle_key(press(KeyCode::Char('/'), KeyModifiers::NONE), &committer)
            .await;
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE), &committer)
            .await;

        assert_eq!(
            received.recv().await,
            Some(AgentCommand::Run(Command::Clear))
        );

        app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE), &committer)
            .await;
        assert_eq!(app.input.text(), "");

        app.handle_agent_event(AgentViewEvent::CommandFinished(Command::Clear));
        app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE), &committer)
            .await;

        assert_eq!(app.input.text(), "x");
    }

    #[tokio::test]
    async fn tab_completes_the_selected_command() {
        let (mut app, _) = app_with_size(48, 10);
        let (committer, mut received) = mpsc::channel(1);

        for character in "/co".chars() {
            app.handle_key(
                press(KeyCode::Char(character), KeyModifiers::NONE),
                &committer,
            )
            .await;
        }
        app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE), &committer)
            .await;

        assert_eq!(app.input.text(), "/compact");
        assert!(
            received.try_recv().is_err(),
            "completion must not execute it"
        );
    }

    #[tokio::test]
    async fn escape_dismisses_the_command_menu_before_cancelling() {
        let (mut app, mut terminal) = app_with_size(48, 10);
        let (committer, mut received) = mpsc::channel(1);

        app.handle_key(press(KeyCode::Char('/'), KeyModifiers::NONE), &committer)
            .await;
        app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE), &committer)
            .await;

        let rows = drawn(&mut app, &mut terminal);

        assert!(!rows.iter().any(|row| row.contains("/clear")), "{rows:?}");
        assert!(received.try_recv().is_err(), "Esc only dismissed the menu");
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

    use h_core::interaction::{AskOption, AskQuestion};

    fn ask(options: &[(&str, Option<&str>)]) -> (Request, oneshot::Receiver<AskAnswer>) {
        let (reply, answer) = oneshot::channel();

        (
            Request::Ask {
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
    async fn escape_cancels_the_turn_and_drops_the_pending_question() {
        let (mut app, _) = app_with_size(40, 12);
        let (committer, mut received) = mpsc::channel(1);
        let (request, answer) = ask(&[("left", None)]);

        app.begin_ask(request);
        app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE), &committer)
            .await;

        assert!(
            app.is_asking(),
            "the reply stays alive until cancellation lands"
        );
        assert_eq!(received.recv().await, Some(AgentCommand::Cancel));

        app.handle_agent_event(AgentViewEvent::TurnFinished { completed: false });

        assert!(!app.is_asking());
        assert!(
            answer.await.is_err(),
            "cancelling also drops the tool's pending reply channel"
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

        assert!(busy.iter().any(|row| row.contains("h-ing...")), "{busy:?}");
        assert!(
            busy.iter().any(|row| row.contains("Esc to cancel")),
            "{busy:?}"
        );
    }

    #[test]
    fn the_spinner_is_flanked_by_gaps() {
        let (mut app, mut terminal) = app_with_size(40, 8);
        app.state.turn_in_progress = true;

        let rows = drawn(&mut app, &mut terminal);
        let spinner = rows
            .iter()
            .position(|row| row.contains("h-ing..."))
            .expect("the spinner should be visible");

        assert!(
            rows[spinner - 1].is_empty(),
            "a gap should separate the display above: {rows:?}"
        );
        assert!(
            rows[spinner + 1].is_empty(),
            "a gap should separate the input below: {rows:?}"
        );
        assert!(rows[spinner + 2].contains('─'), "{rows:?}");
    }

    /// The word spans of the spinner line, skipping the leading glyph.
    fn spinner_word(app: &App) -> Vec<Span<'static>> {
        app.spinner_line().spans[1..1 + SPINNER_WORD.len()].to_vec()
    }

    #[test]
    fn the_chase_starts_with_the_whole_word_plain() {
        let (mut app, _) = app_with_size(40, 8);
        app.state.turn_in_progress = true;

        let word = spinner_word(&app);

        assert_eq!(word.len(), SPINNER_WORD.len());
        assert!(
            word.iter().all(|span| span.style.fg.is_none()),
            "the rest frame shows no gray: {word:?}"
        );
    }

    #[test]
    fn the_gray_moves_through_the_word_one_character_at_a_time() {
        let (mut app, _) = app_with_size(40, 8);
        app.state.turn_in_progress = true;

        app.advance_spinner();
        let word = spinner_word(&app);
        assert_eq!(word[0].content.as_ref(), "h");
        assert_eq!(word[0].style.fg, Some(Color::DarkGray));
        assert!(
            word[1..].iter().all(|span| span.style.fg.is_none()),
            "only the first character is gray: {word:?}"
        );

        app.advance_spinner();
        let word = spinner_word(&app);
        assert_eq!(word[0].style.fg, None, "the gray moves on");
        assert_eq!(word[1].content.as_ref(), "-");
        assert_eq!(word[1].style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn the_chase_restarts_with_the_whole_word_plain() {
        let (mut app, _) = app_with_size(40, 8);
        app.state.turn_in_progress = true;

        // The last character of the word turns gray...
        for _ in 0..SPINNER_WORD.len() {
            app.advance_spinner();
        }
        let word = spinner_word(&app);
        assert_eq!(word[SPINNER_WORD.len() - 1].style.fg, Some(Color::DarkGray));

        // ...and the next tick returns to the whole word plain again.
        app.advance_spinner();
        let word = spinner_word(&app);
        assert!(
            word.iter().all(|span| span.style.fg.is_none()),
            "the cycle restarts from the rest frame: {word:?}"
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
    fn the_spinner_shows_accumulated_turn_tokens_after_elapsed_time() {
        let (mut app, mut terminal) = app_with_size(40, 8);

        app.handle_agent_event(AgentViewEvent::TurnStart);
        app.handle_agent_event(AgentViewEvent::TokenUsage {
            context: Some(2_400),
            turn: Some(5_500),
        });

        assert!(
            drawn(&mut app, &mut terminal)
                .iter()
                .any(|row| row.contains("(0s ↓ 5.5K)"))
        );
    }

    #[test]
    fn the_counter_appears_only_once_a_turn_is_running() {
        let (mut app, mut terminal) = app_with_size(40, 8);
        app.state.turn_in_progress = true;

        assert!(
            !drawn(&mut app, &mut terminal)
                .iter()
                .any(|row| row.contains("(0s")),
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
        app.handle_agent_event(AgentViewEvent::TokenUsage {
            context: Some(2_400),
            turn: Some(5_500),
        });
        app.handle_agent_event(AgentViewEvent::TurnFinished { completed: true });

        assert!(
            drawn(&mut app, &mut terminal)
                .iter()
                .any(|row| row.starts_with("❃ Done for") && row.contains("↓ 5.5K")),
            "the record of the wait should outlive the spinner"
        );
    }

    #[test]
    fn context_usage_sits_below_the_input_and_uses_the_configured_limit() {
        let (mut app, mut terminal) = app_with_size(80, 8);

        app.handle_agent_event(AgentViewEvent::TokenUsage {
            context: Some(2_400),
            turn: None,
        });

        let rows = drawn(&mut app, &mut terminal);

        assert!(
            rows.last().is_some_and(|row| {
                row.ends_with("context 2.4K/200K ([██████████] 98.8% left)")
            }),
            "the status belongs on the bottom row: {rows:?}"
        );
    }

    #[test]
    fn the_status_line_names_the_model_and_effort_on_the_left() {
        let (mut app, mut terminal) = app_with_size(80, 8);

        app.handle_agent_event(AgentViewEvent::Startup {
            model: "gpt-5.6-sol".to_owned(),
            thinking_effort: Some("medium".to_owned()),
        });

        let rows = drawn(&mut app, &mut terminal);

        assert!(
            rows.last()
                .is_some_and(|row| row.starts_with("gpt-5.6-sol · medium")),
            "the model belongs on the bottom row: {rows:?}"
        );
    }

    #[test]
    fn the_model_alone_has_no_dangling_separator() {
        let mut app = App::default();

        app.handle_agent_event(AgentViewEvent::Startup {
            model: "claude".to_owned(),
            thinking_effort: None,
        });

        let text = app
            .model_line()
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(text, "claude");
    }

    #[test]
    fn context_percentage_uses_rainbow_spans() {
        let mut app = App {
            context_window: 100_000,
            ..App::default()
        };
        app.state.context_tokens = Some(45_000);

        let line = app.context_line();
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        let colors = line
            .spans
            .iter()
            .filter(|span| {
                span.content != "context 45K/100K ("
                    && span.content != ")"
                    && !["[", "]", "█", "░"].contains(&span.content.as_ref())
            })
            .filter_map(|span| span.style.fg)
            .collect::<std::collections::HashSet<_>>();

        assert_eq!(text, "context 45K/100K ([██████░░░░] 55.0% left)");
        assert!(
            colors.len() > 1,
            "the percentage should use the rainbow ramp"
        );
    }

    #[test]
    fn context_percentage_stops_at_zero_after_the_limit() {
        let mut app = App {
            context_window: 100_000,
            ..App::default()
        };
        app.state.context_tokens = Some(120_000);

        let text = app
            .context_line()
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(text, "context 120K/100K ([░░░░░░░░░░] 0.0% left)");
    }

    #[test]
    fn the_context_bar_tracks_the_remaining_share() {
        let text = |app: &App| {
            app.context_spans()
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };
        let mut app = App {
            context_window: 100_000,
            ..App::default()
        };

        app.state.context_tokens = Some(0);
        assert_eq!(text(&app), "context 0/100K ([██████████] 100.0% left)");

        app.state.context_tokens = Some(45_000);
        assert_eq!(text(&app), "context 45K/100K ([██████░░░░] 55.0% left)");
        assert!(
            app.context_spans()
                .iter()
                .filter(|span| span.content.contains('█'))
                .filter_map(|span| span.style.fg)
                .collect::<std::collections::HashSet<_>>()
                .len()
                > 1,
            "the filled bar should ride the rainbow ramp"
        );

        app.state.context_tokens = Some(100_000);
        assert_eq!(text(&app), "context 100K/100K ([░░░░░░░░░░] 0.0% left)");

        app.state.context_tokens = None;
        assert_eq!(text(&app), "context ?/100K (?% left)");
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
    fn the_spinner_word_stays_whole_as_the_glyphs_rotate() {
        let (mut app, mut terminal) = app_with_size(40, 8);
        app.state.turn_in_progress = true;

        app.advance_spinner();
        let rows = drawn(&mut app, &mut terminal);

        assert!(rows.iter().any(|row| row.contains("h-ing...")), "{rows:?}");
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

    #[test]
    fn a_long_input_wraps_inside_a_taller_prompt_box() {
        let (mut app, mut terminal) = app_with_size(12, 8);

        for character in "abcdefghijk".chars() {
            app.input
                .handle_key(press(KeyCode::Char(character), KeyModifiers::NONE));
        }

        let rows = drawn(&mut app, &mut terminal);

        assert!(
            rows.windows(2).any(|rows| rows == ["❯ abcdefghi", "  jk"]),
            "the whole logical line should remain visible: {rows:?}"
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
