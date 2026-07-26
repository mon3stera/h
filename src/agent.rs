use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use crate::{
    bridge::UiBridge,
    bus::EventBus,
    context::{Context, Message, built_in_workspace_info},
    event::{AgentEvent, AgentViewEvent, CompletedReason, ProviderSignal},
    provider::{Provider, ProviderEventStream},
    tool::{
        AskTool, BashTool, EditTool, FetchTool, FileBufferStore, GrepTool, ReadFileTool, ToolCall,
        ToolCallResult, ToolRegistry, WriteFileTool,
    },
};
use futures::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::Instrument;
use uuid::Uuid;

/// How many times opening a provider stream is attempted before giving up.
const STREAM_MAX_ATTEMPTS: u32 = 4;

const STREAM_RETRY_BASE_DELAY: Duration = Duration::from_millis(150);

/// The wait after a failed attempt, doubling each time: 150ms, 300ms, 600ms.
fn stream_retry_delay(attempt: u32) -> Duration {
    STREAM_RETRY_BASE_DELAY * 2_u32.pow(attempt.saturating_sub(1))
}

/// Rebuilds a tool result from the output that was persisted for the provider.
///
/// [`ToolCallResult::into_provider_output`] renders a failure as `{"error": …}`,
/// so that shape reads back as a failure. A successful output that happens to
/// look the same replays as a failure; the archive keeps nothing that could tell
/// the two apart.
fn replayed_result(call_id: &str, output: &str) -> ToolCallResult {
    let value =
        serde_json::from_str::<Value>(output).unwrap_or_else(|_| Value::String(output.to_owned()));

    match value.get("error").and_then(Value::as_str) {
        Some(message) => ToolCallResult::failure(call_id.to_owned(), message),
        None => ToolCallResult::success(call_id.to_owned(), value),
    }
}

#[derive(Default)]
struct TurnMetrics {
    provider_requests: usize,
    text_delta_count: usize,
    text_delta_bytes: usize,
    tool_call_count: usize,
    unsupported_signal_count: usize,
    completion_reason: &'static str,
}

pub enum NextTurn {
    Prompt(String),
    Continue,
    Stop,
}

pub struct Agent<P> {
    event_bus: EventBus<AgentEvent>,
    view_bus: EventBus<AgentViewEvent>,
    context: Context,
    tool: ToolRegistry,
    provider: P,
    turn: NextTurn,
}

impl<P> Agent<P>
where
    P: Provider,
{
    pub fn new(provider: P) -> Self {
        Self {
            event_bus: EventBus::new(),
            view_bus: EventBus::new(),
            context: Context::new(),
            tool: ToolRegistry::new(),
            provider,
            turn: NextTurn::Continue,
        }
    }

    pub fn subscribe(&self) -> UnboundedReceiver<AgentEvent> {
        self.event_bus.subscribe()
    }

    pub fn subscribe_view(&self) -> UnboundedReceiver<AgentViewEvent> {
        self.view_bus.subscribe()
    }

    /// Registers the built-in tools. `bridge` is handed to the tools that need
    /// an answer from the user; note that [`Self::handle_signal`] awaits each
    /// tool call in turn, so at most one such request is outstanding at a time.
    pub fn with_internal_tools(&mut self, bridge: UiBridge) -> anyhow::Result<&mut Self> {
        let file_buffers = FileBufferStore::default();

        self.tool.register(AskTool::new(bridge));

        self.tool
            .register_with_presenter(
                ReadFileTool::new(file_buffers.clone()),
                crate::tool::ReadFilePresenter,
            )
            .register_with_presenter(
                WriteFileTool::new(file_buffers),
                crate::tool::WriteFilePresenter,
            )
            .register_with_presenter(FetchTool::new()?, crate::tool::FetchPresenter)
            .register_with_presenter(GrepTool, crate::tool::GrepPresenter)
            .register(EditTool)
            .register_with_presenter(BashTool::new(), crate::tool::BashPresenter);

        Ok(self)
    }

    pub async fn with_global_prompts(&mut self) -> anyhow::Result<&mut Self> {
        self.context.inject_global_prompts().await?;
        Ok(self)
    }

    pub async fn with_workspace_info(&mut self) -> anyhow::Result<&mut Self> {
        let info = built_in_workspace_info();
        self.context.inject_workspace_info(info).await?;
        Ok(self)
    }

    pub fn initialize(&mut self) -> anyhow::Result<()> {
        let definitions = self.tool.definitions()?;

        self.provider.define_tools(definitions)?;
        self.view_bus.broadcast(AgentViewEvent::Startup {
            model: self.provider.model().to_owned(),
            thinking_effort: self.provider.thinking_effort().map(str::to_owned),
        });

        Ok(())
    }

    fn append_prompt(&mut self, prompt: impl AsRef<str>) {
        self.context
            .histories_mut()
            .push(Message::User(prompt.as_ref().to_string()));
    }

    fn merge_text_delta(&mut self) {
        self.context.finalize_buf(Message::Assistant);
        self.context.prepare_buf();
    }

    async fn handle_tool_call(&self, call: &crate::tool::ToolCall) -> crate::tool::ToolCallResult {
        self.tool.call(call).await
    }

    async fn handle_signal(
        &mut self,
        signal: &ProviderSignal,
        metrics: &mut TurnMetrics,
    ) -> anyhow::Result<()> {
        match signal {
            ProviderSignal::TextDelta(delta) => {
                metrics.text_delta_count += 1;
                metrics.text_delta_bytes += delta.len();
                self.context.append_buf(delta);
                self.view_bus
                    .broadcast(AgentViewEvent::TextDelta(delta.clone()));
            }
            ProviderSignal::ToolCallStarted(call) => {
                metrics.tool_call_count += 1;
                self.merge_text_delta();

                let arguments = serde_json::to_string(call.arguments())?;
                self.context.histories_mut().push(Message::ToolCall {
                    call_id: call.id().as_str().to_owned(),
                    name: call.name().to_owned(),
                    arguments,
                });

                self.view_bus
                    .broadcast(AgentViewEvent::Tool(self.tool.present_running(call)));

                let result = self.handle_tool_call(call).await;
                let output = result.clone().into_provider_output();

                self.context.histories_mut().push(Message::ToolCallResult {
                    call_id: call.id().as_str().to_owned(),
                    output,
                });

                self.event_bus
                    .broadcast(AgentEvent::ToolCallCompleted(result.clone()));
                self.view_bus.broadcast(AgentViewEvent::Tool(
                    self.tool.present_completed(call, &result),
                ));
            }
            ProviderSignal::ToolCallCompleted(result) => {
                self.merge_text_delta();

                self.context.histories_mut().push(Message::ToolCallResult {
                    call_id: result.id().as_str().to_owned(),
                    output: result.clone().into_provider_output(),
                });
            }
            ProviderSignal::Completed(reason) => {
                metrics.completion_reason = match reason {
                    CompletedReason::NeedCall => "needs_tool_call",
                    CompletedReason::Final => "final",
                };
                self.merge_text_delta();
                self.view_bus.broadcast(AgentViewEvent::Completed);

                if matches!(reason, CompletedReason::NeedCall) {
                    self.turn = NextTurn::Continue;
                }
            }
            ProviderSignal::Unsupported => {
                metrics.unsupported_signal_count += 1;
            }
        }

        Ok(())
    }

    pub async fn continue_turn(&mut self, prompt: impl Into<String>) -> anyhow::Result<()> {
        let prompt = prompt.into();
        let turn_id = Uuid::now_v7();
        let started = Instant::now();
        let span = tracing::info_span!("agent.turn", turn_id = %turn_id);

        self.view_bus.broadcast(AgentViewEvent::TurnStart);

        let result = async {
            tracing::info!(event = "agent.turn.started");

            let result = self.run_turn(prompt).await;
            match &result {
                Ok(metrics) => tracing::info!(
                    event = "agent.turn.completed",
                    provider_request_count = metrics.provider_requests,
                    text_delta_count = metrics.text_delta_count,
                    text_delta_bytes = metrics.text_delta_bytes,
                    tool_call_count = metrics.tool_call_count,
                    unsupported_signal_count = metrics.unsupported_signal_count,
                    completion_reason = metrics.completion_reason,
                    duration_ms = started.elapsed().as_millis() as u64
                ),
                Err(_) => tracing::error!(
                    event = "agent.turn.failed",
                    operation = "continue_turn",
                    error_class = "agent_turn_error",
                    duration_ms = started.elapsed().as_millis() as u64
                ),
            }

            result.map(|_| ())
        }
        .instrument(span)
        .await;

        self.view_bus.broadcast(AgentViewEvent::TurnFinished);
        result
    }

    async fn run_turn(&mut self, prompt: String) -> anyhow::Result<TurnMetrics> {
        self.turn = NextTurn::Prompt(prompt);
        let mut metrics = TurnMetrics::default();

        loop {
            if matches!(self.turn, NextTurn::Stop) {
                return Ok(metrics);
            }

            self.next_turn(&mut metrics).await?
        }
    }

    pub async fn archive(&mut self) -> anyhow::Result<()> {
        // Starting `h` and quitting straight away should not leave a titleless
        // row in the session picker.
        if !self.context.has_exchange() {
            tracing::info!(event = "agent.archive.skipped", reason = "no_exchange");
            return Ok(());
        }

        self.context.archive().await
    }

    pub async fn resume(&mut self, id: impl AsRef<str>) -> anyhow::Result<&mut Self> {
        let context = Context::resume(id).await?;
        self.context = context;
        Ok(self)
    }

    /// Replays the whole conversation onto the view bus, so a resumed session
    /// opens on its history instead of a blank screen.
    ///
    /// The replay is coarser than the original: streaming granularity is gone,
    /// so each assistant message arrives as a single delta. `System` messages
    /// stay hidden, exactly as they were while live.
    pub fn rebroadcast_all_view(&self) {
        let histories = self.context.histories();

        // A tool call and the result answering it are separate messages; index
        // the results so each call can be presented in its finished form.
        let outputs = histories
            .iter()
            .filter_map(|message| match message {
                Message::ToolCallResult { call_id, output } => {
                    Some((call_id.as_str(), output.as_str()))
                }
                _ => None,
            })
            .collect::<HashMap<_, _>>();

        let mut replayed = 0_usize;

        for message in histories {
            match message {
                // Global prompts, workspace info, and results already folded
                // into the call above them were never on screen.
                Message::System(_) | Message::ToolCallResult { .. } => continue,
                Message::User(prompt) => {
                    self.view_bus
                        .broadcast(AgentViewEvent::Prompt(prompt.clone()));
                }
                Message::Assistant(text) => {
                    self.view_bus
                        .broadcast(AgentViewEvent::TextDelta(text.clone()));
                    // Closes the response the way the live path does, which is
                    // what draws the rule between exchanges.
                    self.view_bus.broadcast(AgentViewEvent::Completed);
                }
                Message::ToolCall {
                    call_id,
                    name,
                    arguments,
                } => {
                    let call = ToolCall::new(
                        call_id.clone(),
                        name.clone(),
                        serde_json::from_str(arguments).unwrap_or(Value::Null),
                    );

                    let presentation = match outputs.get(call_id.as_str()) {
                        Some(output) => self
                            .tool
                            .present_completed(&call, &replayed_result(call_id, output)),
                        // A call with no recorded result never came back; the
                        // session was archived or died mid-flight.
                        None => self.tool.present_running(&call),
                    };

                    self.view_bus.broadcast(AgentViewEvent::Tool(presentation));
                }
            }

            replayed += 1;
        }

        tracing::info!(
            event = "agent.view.rebroadcast",
            message_count = histories.len(),
            replayed_count = replayed,
        );
    }

    /// Opens a provider stream, retrying a failed open with an exponential
    /// backoff.
    ///
    /// Retrying is safe only here: nothing has reached the context or the view
    /// yet, so a fresh attempt starts from the same state. An error part-way
    /// through a stream is left alone — deltas have already been broadcast and
    /// folded into the context, so re-requesting would duplicate them.
    ///
    /// Every error is retried. The provider hands back `anyhow::Error`, which
    /// keeps a refused request (bad key, malformed input) indistinguishable from
    /// a dropped connection; retrying the former wastes about a second.
    async fn open_stream(
        &self,
        request_index: usize,
    ) -> anyhow::Result<ProviderEventStream<P::StreamEvent>> {
        let mut attempt = 1;

        loop {
            match self.provider.stream(self.context.histories()).await {
                Ok(stream) => {
                    if attempt > 1 {
                        tracing::info!(
                            event = "agent.provider_stream.recovered",
                            request_index,
                            attempt
                        );
                    }

                    return Ok(stream);
                }
                Err(e) if attempt < STREAM_MAX_ATTEMPTS => {
                    let delay = stream_retry_delay(attempt);

                    tracing::warn!(
                        event = "agent.provider_stream.retrying",
                        request_index,
                        attempt,
                        max_attempts = STREAM_MAX_ATTEMPTS,
                        delay_ms = delay.as_millis() as u64,
                        error_class = "provider_stream_open_error",
                        error = e.to_string(),
                    );

                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(e) => {
                    tracing::error!(
                        event = "agent.provider_stream.exhausted",
                        request_index,
                        attempt,
                        error_class = "provider_stream_open_error",
                        error = e.to_string(),
                    );

                    return Err(e);
                }
            }
        }
    }

    async fn next_turn(&mut self, metrics: &mut TurnMetrics) -> anyhow::Result<()> {
        match &self.turn {
            NextTurn::Prompt(prompt) => {
                self.append_prompt(prompt.clone());
            }
            NextTurn::Continue => {}
            NextTurn::Stop => return Ok(()),
        }

        self.turn = NextTurn::Stop;

        self.context.prepare_buf();

        metrics.provider_requests += 1;
        let request_index = metrics.provider_requests;
        let request_started = Instant::now();
        tracing::info!(
            event = "agent.provider_request.started",
            request_index,
            message_count = self.context.histories().len()
        );

        let mut stream = match self.open_stream(request_index).await {
            Ok(stream) => stream,
            Err(e) => {
                self.view_bus.broadcast(AgentViewEvent::Err(e.to_string()));
                return Err(e);
            }
        };

        loop {
            match stream.next().await {
                Some(Ok(event)) => {
                    let signal = self.provider.handle(event).await?;

                    let agent_event: AgentEvent = signal.clone().into();
                    self.event_bus.broadcast(agent_event);
                    self.handle_signal(&signal, metrics).await?;
                }
                Some(e) => {
                    tracing::warn!(
                        event = "agent.provider_request.failed",
                        request_index,
                        error_class = "provider_stream_error",
                        duration_ms = request_started.elapsed().as_millis() as u64
                    );

                    match e {
                        Ok(_) => {}
                        Err(e) => {
                            self.view_bus.broadcast(AgentViewEvent::Err(e.to_string()));
                            break Err(e);
                        }
                    }
                }
                None => {
                    tracing::info!(
                        event = "agent.provider_request.completed",
                        request_index,
                        duration_ms = request_started.elapsed().as_millis() as u64
                    );
                    break Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        pin::Pin,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use futures::{Stream, stream};
    use serde_json::json;

    use super::*;
    use crate::tool::{ToolCall, ToolCallStatus, ToolDefinition};

    struct TestProvider;

    #[async_trait::async_trait]
    impl Provider for TestProvider {
        type StreamEvent = ();

        fn model(&self) -> &str {
            "test-model"
        }

        fn thinking_effort(&self) -> Option<&str> {
            Some("high")
        }

        fn define_tools(&mut self, _specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
            Ok(())
        }

        async fn handle(&mut self, _event: Self::StreamEvent) -> anyhow::Result<ProviderSignal> {
            Ok(ProviderSignal::Unsupported)
        }

        async fn stream(
            &self,
            _input: &[Message],
        ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<Self::StreamEvent>> + Send>>>
        {
            Ok(Box::pin(stream::empty()))
        }
    }

    #[test]
    fn initialize_broadcasts_startup_to_existing_view_subscriber() {
        let mut agent = Agent::new(TestProvider);
        let mut receiver = agent.subscribe_view();

        agent.initialize().unwrap();

        assert!(matches!(
            receiver.try_recv().unwrap(),
            AgentViewEvent::Startup {
                model,
                thinking_effort: Some(thinking_effort),
            } if model == "test-model" && thinking_effort == "high"
        ));
    }

    /// Fails `stream` a set number of times before succeeding, counting every
    /// attempt so the backoff can be asserted on.
    struct FlakyProvider {
        failures_left: AtomicUsize,
        attempts: AtomicUsize,
    }

    impl FlakyProvider {
        fn new(failures: usize) -> Self {
            Self {
                failures_left: AtomicUsize::new(failures),
                attempts: AtomicUsize::new(0),
            }
        }

        fn attempts(&self) -> usize {
            self.attempts.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl Provider for FlakyProvider {
        type StreamEvent = ();

        fn model(&self) -> &str {
            "flaky-model"
        }

        fn thinking_effort(&self) -> Option<&str> {
            None
        }

        fn define_tools(&mut self, _specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
            Ok(())
        }

        async fn handle(&mut self, _event: Self::StreamEvent) -> anyhow::Result<ProviderSignal> {
            Ok(ProviderSignal::Unsupported)
        }

        async fn stream(
            &self,
            _input: &[Message],
        ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<Self::StreamEvent>> + Send>>>
        {
            self.attempts.fetch_add(1, Ordering::SeqCst);

            if self
                .failures_left
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                    left.checked_sub(1)
                })
                .is_ok()
            {
                anyhow::bail!("connection reset by peer");
            }

            Ok(Box::pin(stream::empty()))
        }
    }

    #[test]
    fn the_backoff_doubles_from_150ms() {
        assert_eq!(stream_retry_delay(1), Duration::from_millis(150));
        assert_eq!(stream_retry_delay(2), Duration::from_millis(300));
        assert_eq!(stream_retry_delay(3), Duration::from_millis(600));
    }

    #[tokio::test(start_paused = true)]
    async fn a_transient_failure_is_retried_until_the_stream_opens() {
        let agent = Agent::new(FlakyProvider::new(2));
        let started = tokio::time::Instant::now();

        assert!(agent.open_stream(1).await.is_ok());
        assert_eq!(agent.provider.attempts(), 3, "two failures, then a success");
        assert_eq!(
            started.elapsed(),
            Duration::from_millis(450),
            "waited 150ms then 300ms"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_stream_that_never_opens_gives_up_after_the_last_attempt() {
        let agent = Agent::new(FlakyProvider::new(usize::MAX));
        let started = tokio::time::Instant::now();

        assert!(agent.open_stream(1).await.is_err());
        assert_eq!(agent.provider.attempts(), STREAM_MAX_ATTEMPTS as usize);
        assert_eq!(
            started.elapsed(),
            Duration::from_millis(1050),
            "waited 150ms, 300ms, 600ms, then stopped"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_stream_that_opens_first_try_does_not_wait() {
        let agent = Agent::new(FlakyProvider::new(0));
        let started = tokio::time::Instant::now();

        assert!(agent.open_stream(1).await.is_ok());
        assert_eq!(agent.provider.attempts(), 1);
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    fn agent_with_histories(histories: Vec<Message>) -> Agent<TestProvider> {
        let mut agent = Agent::new(TestProvider);
        *agent.context.histories_mut() = histories;

        agent
    }

    fn drain(receiver: &mut UnboundedReceiver<AgentViewEvent>) -> Vec<AgentViewEvent> {
        let mut events = Vec::new();

        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }

        events
    }

    /// The shape that filled archived sessions with blank replies: a tool call
    /// with nothing said before or after it. Every boundary flushes the stream
    /// buffer, and each empty flush used to become an assistant turn.
    #[tokio::test]
    async fn a_silent_tool_call_leaves_no_empty_assistant_turn() {
        let mut agent = Agent::new(TestProvider);
        agent.with_internal_tools(UiBridge::new().0).unwrap();
        let mut metrics = TurnMetrics::default();

        // An unregistered tool fails in the registry rather than running
        // anything, which is all this needs to reach both boundaries.
        agent
            .handle_signal(
                &ProviderSignal::ToolCallStarted(ToolCall::new(
                    "call-1",
                    "no_such_tool",
                    json!({}),
                )),
                &mut metrics,
            )
            .await
            .unwrap();
        agent
            .handle_signal(
                &ProviderSignal::Completed(CompletedReason::Final),
                &mut metrics,
            )
            .await
            .unwrap();

        let blank = agent
            .context
            .histories()
            .iter()
            .filter(|message| matches!(message, Message::Assistant(text) if text.trim().is_empty()))
            .count();

        assert_eq!(blank, 0, "an empty assistant turn must never be recorded");
        assert!(
            agent
                .context
                .histories()
                .iter()
                .any(|message| matches!(message, Message::ToolCall { .. })),
            "the call itself is still recorded"
        );
    }

    #[test]
    fn rebroadcast_replays_prompts_and_responses_in_order() {
        let mut agent = agent_with_histories(vec![
            Message::System("workspace info".to_owned()),
            Message::User("first question".to_owned()),
            Message::Assistant("first answer".to_owned()),
            Message::User("second question".to_owned()),
            Message::Assistant("second answer".to_owned()),
        ]);
        let mut receiver = agent.subscribe_view();

        agent.rebroadcast_all_view();

        let events = drain(&mut receiver);
        let described = events
            .iter()
            .map(|event| match event {
                AgentViewEvent::Prompt(prompt) => format!("prompt:{prompt}"),
                AgentViewEvent::TextDelta(delta) => format!("text:{delta}"),
                AgentViewEvent::Completed => "completed".to_owned(),
                other => format!("unexpected:{other:?}"),
            })
            .collect::<Vec<_>>();

        assert_eq!(
            described,
            [
                "prompt:first question",
                "text:first answer",
                "completed",
                "prompt:second question",
                "text:second answer",
                "completed",
            ]
        );
    }

    #[test]
    fn rebroadcast_hides_system_messages() {
        let mut agent = agent_with_histories(vec![
            Message::System("global prompts".to_owned()),
            Message::System("workspace info".to_owned()),
        ]);
        let mut receiver = agent.subscribe_view();

        agent.rebroadcast_all_view();

        assert!(drain(&mut receiver).is_empty());
    }

    #[test]
    fn rebroadcast_pairs_a_tool_call_with_its_result() {
        let mut agent = agent_with_histories(vec![
            Message::ToolCall {
                call_id: "call-1".to_owned(),
                name: "bash".to_owned(),
                arguments: json!({"action": "run_blocking", "command": "cargo test"}).to_string(),
            },
            Message::ToolCallResult {
                call_id: "call-1".to_owned(),
                output: json!({"stdout": "ok", "exit_code": 0}).to_string(),
            },
        ]);
        agent.with_internal_tools(UiBridge::new().0).unwrap();
        let mut receiver = agent.subscribe_view();

        agent.rebroadcast_all_view();

        let events = drain(&mut receiver);

        assert_eq!(events.len(), 1, "the result folds into the call it answers");
        assert!(
            matches!(
                &events[0],
                AgentViewEvent::Tool(presentation)
                    if presentation.name == "Bash"
                        && presentation.target.as_deref() == Some("cargo test")
                        && matches!(presentation.status, ToolCallStatus::Succeeded)
            ),
            "unexpected replay: {:?}",
            events[0]
        );
    }

    #[test]
    fn rebroadcast_reads_a_persisted_error_back_as_a_failure() {
        let mut agent = agent_with_histories(vec![
            Message::ToolCall {
                call_id: "call-1".to_owned(),
                name: "bash".to_owned(),
                arguments: json!({"action": "run_blocking", "command": "cargo test"}).to_string(),
            },
            Message::ToolCallResult {
                call_id: "call-1".to_owned(),
                output: json!({"error": "command not found"}).to_string(),
            },
        ]);
        agent.with_internal_tools(UiBridge::new().0).unwrap();
        let mut receiver = agent.subscribe_view();

        agent.rebroadcast_all_view();

        let events = drain(&mut receiver);

        assert!(
            matches!(
                &events[0],
                AgentViewEvent::Tool(presentation)
                    if matches!(
                        &presentation.status,
                        ToolCallStatus::Failed { message } if message == "command not found"
                    )
            ),
            "unexpected replay: {:?}",
            events[0]
        );
    }

    #[test]
    fn rebroadcast_leaves_an_unanswered_tool_call_running() {
        let mut agent = agent_with_histories(vec![Message::ToolCall {
            call_id: "call-1".to_owned(),
            name: "bash".to_owned(),
            arguments: json!({"action": "run_blocking", "command": "cargo test"}).to_string(),
        }]);
        agent.with_internal_tools(UiBridge::new().0).unwrap();
        let mut receiver = agent.subscribe_view();

        agent.rebroadcast_all_view();

        let events = drain(&mut receiver);

        assert!(
            matches!(
                &events[0],
                AgentViewEvent::Tool(presentation)
                    if matches!(presentation.status, ToolCallStatus::Running)
            ),
            "unexpected replay: {:?}",
            events[0]
        );
    }

    #[test]
    fn a_persisted_output_that_is_not_json_replays_as_a_success() {
        let result = replayed_result("call-1", "plain text output");

        assert!(matches!(
            result.outcome(),
            crate::tool::ToolCallOutcome::Success(Value::String(text)) if text == "plain text output"
        ));
    }

    #[test]
    fn internal_bash_tool_uses_bash_presenter() {
        let mut agent = Agent::new(TestProvider);
        agent.with_internal_tools(UiBridge::new().0).unwrap();
        let call = ToolCall::new(
            "call-1",
            "bash",
            json!({
                "action": "run_blocking",
                "command": "cargo test",
            }),
        );

        let presentation = agent.tool.present_running(&call);

        assert_eq!(presentation.name, "Bash");
        assert_eq!(presentation.label, "built-in");
        assert_eq!(presentation.target.as_deref(), Some("cargo test"));
        assert!(matches!(presentation.status, ToolCallStatus::Running));
    }
}
