use std::{
    collections::{HashMap, VecDeque},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::{
    bus::EventBus,
    command::Command,
    context::{
        Context, DEFAULT_TOOL_SUMMARY_TURN_INTERVAL, Message, SearchStatus, archive_dir,
        built_in_workspace_info,
    },
    event::{AgentCommand, AgentEvent, AgentViewEvent, CompletedReason, ProviderSignal},
    input::UserInput,
    interaction::Bridge,
    provider::{Provider, ProviderEventStream},
    skill::Registry as SkillRegistry,
    tool::{
        AskTool, BashTool, DynTool, EditTool, FetchTool, FileBufferStore, GrepTool, Presenter,
        ReadFileTool, ToolCall, ToolCallResult, ToolRegistry, WriteFileTool,
    },
};
use futures::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc::{Receiver, UnboundedReceiver, error::TryRecvError};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use uuid::Uuid;

/// How many times a provider request is attempted before giving up.
const STREAM_MAX_ATTEMPTS: u32 = 4;

const STREAM_RETRY_BASE_DELAY: Duration = Duration::from_millis(150);

/// Provider requests a single turn may take before it is called runaway.
///
/// A turn legitimately spans one request per tool call, and real ones have been
/// seen to reach the low twenties. This is not a budget but a backstop: a
/// provider that keeps asking for tool rounds without ever settling would
/// otherwise loop forever, and every round leaves another event on an unbounded
/// bus.
const MAX_TURN_ROUNDS: usize = 100;

const INTERRUPTED_BY_USER: &str = "interrupted by user";

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
    settled_tokens: Option<usize>,
    request_tokens: Option<RequestTokens>,
    total_tokens: Option<usize>,
}

struct RequestTokens {
    input: Option<usize>,
    output: Option<usize>,
    messages: Vec<Message>,
    buf: String,
}

impl TurnMetrics {
    fn new() -> Self {
        Self {
            settled_tokens: Some(0),
            total_tokens: Some(0),
            ..Self::default()
        }
    }

    fn start_request(&mut self, input: Option<usize>) {
        debug_assert!(self.request_tokens.is_none());
        self.request_tokens = Some(RequestTokens {
            input,
            output: Some(0),
            messages: Vec::new(),
            buf: String::new(),
        });
        self.refresh_total();
    }

    fn append_output(&mut self, text: &str) {
        if let Some(request) = self.request_tokens.as_mut() {
            request.buf.push_str(text);
        }
    }

    fn push_output(&mut self, message: Message) {
        let Some(request) = self.request_tokens.as_mut() else {
            return;
        };

        request.finish_buf();
        request.messages.push(message);
    }

    fn output(&self) -> Option<Vec<Message>> {
        self.request_tokens.as_ref().map(RequestTokens::output)
    }

    fn set_output_tokens(&mut self, output: Option<usize>) {
        if let Some(request) = self.request_tokens.as_mut() {
            request.output = output;
            self.refresh_total();
        }
    }

    fn finish_request(&mut self) {
        let total = self.total_tokens;

        if self.request_tokens.take().is_none() {
            return;
        }

        self.settled_tokens = total;
        self.total_tokens = self.settled_tokens;
    }

    fn add_tokens(&mut self, tokens: Option<usize>) {
        self.settled_tokens = match (self.settled_tokens, tokens) {
            (Some(total), Some(tokens)) => Some(total.saturating_add(tokens)),
            _ => None,
        };
        self.refresh_total();
    }

    fn refresh_total(&mut self) {
        self.total_tokens = match (&self.request_tokens, self.settled_tokens) {
            (Some(request), Some(settled)) => match (request.input, request.output) {
                (Some(input), Some(output)) => {
                    Some(settled.saturating_add(input).saturating_add(output))
                }
                _ => None,
            },
            (Some(_), None) => None,
            (None, settled) => settled,
        };
    }
}

impl RequestTokens {
    fn finish_buf(&mut self) {
        let text = std::mem::take(&mut self.buf);

        if !text.trim().is_empty() {
            self.messages.push(Message::Assistant(text));
        }
    }

    fn output(&self) -> Vec<Message> {
        let mut messages = self.messages.clone();

        if !self.buf.trim().is_empty() {
            messages.push(Message::Assistant(self.buf.clone()));
        }

        messages
    }
}

enum RequestAttempt {
    Completed,
    Interrupted,
    Retry {
        error: anyhow::Error,
        error_class: &'static str,
        had_output: bool,
    },
}

#[derive(Default)]
struct ResponseBatch {
    tool_calls: Vec<ToolCall>,
}

impl ResponseBatch {
    fn push(&mut self, call: ToolCall) {
        self.tool_calls.push(call);
    }

    fn take(&mut self) -> Vec<ToolCall> {
        std::mem::take(&mut self.tool_calls)
    }
}

enum CompactOutcome {
    Applied { total_tokens: Option<usize> },
    Empty,
    Unsupported,
}

pub enum NextTurn {
    Prompt(UserInput),
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
    auto_compact_token_limit: usize,
    compact_available: bool,
    tool_summary_turn_interval: NonZeroUsize,
    archive_dir: PathBuf,
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
            auto_compact_token_limit: usize::MAX,
            compact_available: true,
            tool_summary_turn_interval: NonZeroUsize::new(DEFAULT_TOOL_SUMMARY_TURN_INTERVAL)
                .expect("the default tool summary interval is non-zero"),
            archive_dir: archive_dir(),
        }
    }

    pub fn with_auto_compact_token_limit(&mut self, limit: usize) -> &mut Self {
        assert!(limit > 0, "the auto compact token limit must be non-zero");
        self.auto_compact_token_limit = limit;
        self
    }

    pub fn with_tool_summary_turn_interval(&mut self, interval: NonZeroUsize) -> &mut Self {
        self.tool_summary_turn_interval = interval;
        self
    }

    pub fn register_tool<T>(&mut self, tool: T) -> &mut Self
    where
        T: DynTool + 'static,
    {
        self.tool.register(tool);
        self
    }

    pub fn register_tool_with_presenter<T, R>(&mut self, tool: T, presenter: R) -> &mut Self
    where
        T: DynTool + 'static,
        R: Presenter + 'static,
    {
        self.tool.register_with_presenter(tool, presenter);
        self
    }

    pub fn with_system_prompt(&mut self, prompt: impl Into<String>) -> &mut Self {
        self.context.inject_system_prompt(prompt.into());
        self
    }

    #[cfg(test)]
    pub(crate) fn with_archive_dir(&mut self, directory: impl Into<PathBuf>) -> &mut Self {
        self.archive_dir = directory.into();
        self
    }

    pub fn subscribe(&self) -> UnboundedReceiver<AgentEvent> {
        self.event_bus.subscribe()
    }

    pub fn subscribe_view(&self) -> UnboundedReceiver<AgentViewEvent> {
        self.view_bus.subscribe()
    }

    /// Registers the built-in tools. `bridge` is handed to the tools that need
    /// an answer from the user; tool batches are executed sequentially, so at
    /// most one such request is outstanding at a time.
    pub fn with_internal_tools(&mut self, bridge: Bridge) -> anyhow::Result<&mut Self> {
        let file_buffers = FileBufferStore::default();

        self.tool
            .register_with_presenter(AskTool::new(bridge), crate::tool::AskPresenter);

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
            .register_with_presenter(EditTool, crate::tool::EditPresenter)
            .register_with_presenter(BashTool::new(), crate::tool::BashPresenter);

        Ok(self)
    }

    pub fn with_harness_prompt(&mut self, executable: &Path) -> &mut Self {
        self.context.inject_harness_prompt(executable);
        self
    }

    pub async fn with_global_prompts(&mut self) -> anyhow::Result<&mut Self> {
        self.context.inject_global_prompts().await?;
        Ok(self)
    }

    pub async fn with_skills(&mut self) -> anyhow::Result<&mut Self> {
        let registry = SkillRegistry::discover().await?;

        if let Some(prompt) = registry.prompt() {
            self.context.inject_skill_catalog(prompt);
        }

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
        self.refresh_token_count(None);

        Ok(())
    }

    /// Runs the long-lived command loop for an interactive frontend.
    ///
    /// Prompts and session commands retain their order. Cancellation targets
    /// the active turn immediately, while every other command waits for the
    /// current turn boundary.
    pub async fn run(&mut self, mut commands: Receiver<AgentCommand>) {
        let (mut queued, mut accepting) = (VecDeque::new(), true);

        loop {
            let command = loop {
                if !accepting {
                    break None;
                }

                if let Some(command) = queued.pop_front() {
                    break Some(command);
                }

                match commands.recv().await {
                    Some(AgentCommand::Cancel) => {}
                    Some(command) => break Some(command),
                    None => {
                        accepting = false;
                        break None;
                    }
                }
            };
            let Some(command) = command else {
                break;
            };

            match command {
                AgentCommand::Prompt(prompt) => {
                    let cancellation = CancellationToken::new();
                    let result = {
                        let turn =
                            self.continue_turn_with_persistence(prompt, cancellation.clone(), true);
                        tokio::pin!(turn);

                        loop {
                            tokio::select! {
                                result = &mut turn => break result,
                                command = commands.recv(), if accepting => match command {
                                    Some(AgentCommand::Cancel) => cancellation.cancel(),
                                    Some(command) => queued.push_back(command),
                                    None => {
                                        accepting = false;
                                        queued.clear();
                                        cancellation.cancel();
                                    }
                                }
                            }
                        }
                    };

                    if let Err(error) = result {
                        tracing::error!(
                            event = "agent.worker.failed",
                            operation = "continue_turn",
                            error_class = "agent_turn_error",
                            error = error.to_string(),
                        );
                    }
                }
                AgentCommand::Run(command) => {
                    let _ = self.run_command(command).await;
                }
                AgentCommand::Cancel => {}
            }

            // If the turn and a queued cancellation become ready together, the
            // turn has already ended. Do not carry it into the next prompt.
            loop {
                match commands.try_recv() {
                    Ok(AgentCommand::Cancel) => {}
                    Ok(command) => queued.push_back(command),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        accepting = false;
                        queued.clear();
                        break;
                    }
                }
            }
        }

        tracing::info!(event = "agent.worker.closed");
    }

    fn estimate_request_tokens(&self, messages: &[Message]) -> Option<usize> {
        match self.provider.estimate_request_tokens(messages) {
            Ok(count) => count,
            Err(error) => {
                tracing::warn!(
                    event = "provider.request_token_estimate.failed",
                    error_class = "tokenizer_error",
                    error = error.to_string(),
                );
                None
            }
        }
    }

    fn count_context_tokens(&self) -> Option<usize> {
        let messages = self.context.provider_messages_with_buf();
        self.estimate_request_tokens(&messages)
    }

    fn refresh_output_tokens(&self, metrics: &mut TurnMetrics) {
        let Some(output) = metrics.output() else {
            return;
        };
        let tokens = match self.provider.estimate_output_tokens(&output) {
            Ok(tokens) => tokens,
            Err(error) => {
                tracing::warn!(
                    event = "provider.output_token_estimate.failed",
                    error_class = "tokenizer_error",
                    error = error.to_string(),
                );
                None
            }
        };

        metrics.set_output_tokens(tokens);
    }

    fn broadcast_token_count(&mut self, count: Option<usize>, turn: Option<usize>) {
        self.context.set_token_count(count);
        self.view_bus.broadcast(AgentViewEvent::TokenUsage {
            context: count,
            turn,
        });
    }

    fn refresh_token_count(&mut self, turn: Option<usize>) {
        let count = self.count_context_tokens();
        self.broadcast_token_count(count, turn);
    }

    fn refresh_usage(&mut self, metrics: &mut TurnMetrics) {
        self.refresh_output_tokens(metrics);
        self.refresh_token_count(metrics.total_tokens);
    }

    fn settle_request(&mut self, metrics: &mut TurnMetrics) {
        self.refresh_output_tokens(metrics);
        metrics.finish_request();
        self.refresh_token_count(metrics.total_tokens);
    }

    fn append_prompt(&mut self, prompt: UserInput) {
        self.context.histories_mut().push(Message::User(prompt));
    }

    fn merge_text_delta(&mut self) {
        self.context.finalize_buf(Message::Assistant);
        self.context.prepare_buf();
    }

    async fn compact_context(&mut self) -> anyhow::Result<CompactOutcome> {
        let input = self.context.compaction_input();
        if input.is_empty() {
            return Ok(CompactOutcome::Empty);
        }

        let Some(compaction) = self.provider.compact(&input).await? else {
            return Ok(CompactOutcome::Unsupported);
        };
        let total_tokens = compaction.total_tokens();

        self.context.apply_compaction(compaction);
        Ok(CompactOutcome::Applied { total_tokens })
    }

    async fn auto_compact(&mut self, metrics: &mut TurnMetrics) -> Option<usize> {
        let before = self.count_context_tokens();
        self.context.set_token_count(before);

        if !self.compact_available {
            return before;
        }

        let before = before?;
        if before < self.auto_compact_token_limit {
            return Some(before);
        }

        let tools_compacted = self.context.compact_tool_outputs(&self.tool);
        let after_tool_compaction = self.count_context_tokens();
        self.context.set_token_count(after_tool_compaction);

        if tools_compacted {
            tracing::info!(
                event = "context.auto_compact.completed",
                method = "tool_output",
                before_tokens = before,
                after_tokens = after_tool_compaction,
            );
        }

        let after_tool_compaction = after_tool_compaction?;
        if after_tool_compaction < self.auto_compact_token_limit {
            return Some(after_tool_compaction);
        }

        match self.compact_context().await {
            Ok(CompactOutcome::Applied { total_tokens }) => {
                metrics.add_tokens(total_tokens);
                let after = self.count_context_tokens();
                self.context.set_token_count(after);
                self.view_bus.broadcast(AgentViewEvent::ContextCompacted);

                tracing::info!(
                    event = "context.auto_compact.completed",
                    method = "provider",
                    before_tokens = before,
                    after_tokens = after,
                );

                after
            }
            Ok(CompactOutcome::Empty) => {
                tracing::info!(
                    event = "context.auto_compact.skipped",
                    reason = "empty_history",
                    context_tokens = after_tool_compaction,
                );

                Some(after_tool_compaction)
            }
            Ok(CompactOutcome::Unsupported) => {
                self.compact_available = false;
                tracing::warn!(
                    event = "context.auto_compact.disabled",
                    reason = "provider_unsupported",
                    context_tokens = after_tool_compaction,
                );

                Some(after_tool_compaction)
            }
            Err(error) => {
                tracing::warn!(
                    event = "context.auto_compact.failed",
                    error_class = "provider_compact_error",
                    error = error.to_string(),
                    context_tokens = after_tool_compaction,
                );

                Some(after_tool_compaction)
            }
        }
    }

    async fn prepare_provider_context(
        &mut self,
        metrics: &mut TurnMetrics,
        cancellation: &CancellationToken,
    ) -> bool {
        let count = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return false,
            count = self.auto_compact(metrics) => count,
        };
        self.broadcast_token_count(count, metrics.total_tokens);

        true
    }

    async fn handle_tool_call(&self, call: &crate::tool::ToolCall) -> crate::tool::ToolCallResult {
        self.tool.call(call).await
    }

    fn record_tool_result(&mut self, call: &ToolCall, result: ToolCallResult) {
        let output = result.clone().into_provider_output();

        self.context.histories_mut().push(Message::ToolCallResult {
            call_id: call.id().as_str().to_owned(),
            output,
            summary: result.summary().cloned(),
        });

        self.event_bus
            .broadcast(AgentEvent::ToolCallCompleted(result.clone()));
        self.view_bus.broadcast(AgentViewEvent::Tool(
            self.tool.present_completed(call, &result),
        ));
    }

    fn interrupt_tool_calls(&mut self, calls: impl IntoIterator<Item = ToolCall>) {
        for call in calls {
            let result = ToolCallResult::failure(call.id().clone(), INTERRUPTED_BY_USER);
            self.record_tool_result(&call, result);
        }
    }

    async fn execute_response_batch(
        &mut self,
        batch: &mut ResponseBatch,
        metrics: &mut TurnMetrics,
        cancellation: &CancellationToken,
    ) -> bool {
        let calls = batch.take();
        if calls.is_empty() {
            return true;
        }

        // Provider output must remain contiguous. Finalize any trailing text
        // before local function outputs are appended to the context.
        self.merge_text_delta();

        let mut calls = calls.into_iter();
        while let Some(call) = calls.next() {
            if cancellation.is_cancelled() {
                self.interrupt_tool_calls(std::iter::once(call).chain(calls));
                return false;
            }

            // Keep the call future in its own scope so cancellation drops it
            // before the explicit tool hook runs. This releases any locks or
            // request handles the hook may need to terminate work.
            let result = {
                let call = self.handle_tool_call(&call);
                tokio::pin!(call);

                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => None,
                    result = &mut call => Some(result),
                }
            };
            let result = match result {
                Some(result) => result,
                None => {
                    if let Err(error) = self.tool.cancel(&call).await {
                        tracing::warn!(
                            event = "tool.cancel.failed",
                            tool_name = call.name(),
                            error_class = "tool_cancel_error",
                            error = error.to_string(),
                        );
                    }

                    ToolCallResult::failure(call.id().clone(), INTERRUPTED_BY_USER)
                }
            };
            let interrupted = cancellation.is_cancelled();

            self.record_tool_result(&call, result);

            if interrupted {
                self.interrupt_tool_calls(calls);
                return false;
            }
        }

        self.prepare_provider_context(metrics, cancellation).await
    }

    async fn handle_signal(
        &mut self,
        signal: &ProviderSignal,
        batch: &mut ResponseBatch,
        metrics: &mut TurnMetrics,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<()> {
        match signal {
            ProviderSignal::TextDelta(delta) => {
                metrics.text_delta_count += 1;
                metrics.text_delta_bytes += delta.len();
                metrics.append_output(delta);
                self.context.append_buf(delta);
                self.view_bus
                    .broadcast(AgentViewEvent::TextDelta(delta.clone()));
                self.refresh_usage(metrics);
            }
            ProviderSignal::Reasoning(item) => {
                self.merge_text_delta();

                let message = Message::Reasoning(item.clone());
                metrics.push_output(message.clone());
                self.context.histories_mut().push(message);
                self.refresh_usage(metrics);
            }
            ProviderSignal::Search(search) => {
                self.merge_text_delta();

                if search.status() != SearchStatus::Running {
                    let message = Message::Search(search.clone());
                    metrics.push_output(message.clone());
                    self.context.histories_mut().push(message);
                    self.refresh_usage(metrics);
                }

                self.view_bus
                    .broadcast(AgentViewEvent::Search(search.clone()));
            }
            ProviderSignal::ToolCallStarted(call) => {
                metrics.tool_call_count += 1;
                self.merge_text_delta();

                let arguments = serde_json::to_string(call.arguments())?;
                let message = Message::ToolCall {
                    call_id: call.id().as_str().to_owned(),
                    name: call.name().to_owned(),
                    arguments,
                };

                metrics.push_output(message.clone());
                self.context.histories_mut().push(message);
                self.refresh_usage(metrics);

                self.view_bus
                    .broadcast(AgentViewEvent::Tool(self.tool.present_running(call)));
                batch.push(call.clone());
            }
            ProviderSignal::ToolCallCompleted(result) => {
                self.merge_text_delta();

                let message = Message::ToolCallResult {
                    call_id: result.id().as_str().to_owned(),
                    output: result.clone().into_provider_output(),
                    summary: result.summary().cloned(),
                };

                metrics.push_output(message.clone());
                self.context.histories_mut().push(message);
                self.refresh_usage(metrics);

                self.prepare_provider_context(metrics, cancellation).await;
            }
            ProviderSignal::Completed { reason } => {
                self.refresh_output_tokens(metrics);
                metrics.finish_request();
                metrics.completion_reason = match reason {
                    CompletedReason::NeedCall => "needs_tool_call",
                    CompletedReason::Final => "final",
                };
                self.merge_text_delta();

                if !self
                    .execute_response_batch(batch, metrics, cancellation)
                    .await
                {
                    return Ok(());
                }

                self.view_bus.broadcast(AgentViewEvent::Completed);

                let final_answer = matches!(reason, CompletedReason::Final);
                if !final_answer {
                    self.turn = NextTurn::Continue;
                } else {
                    self.context
                        .complete_turn(self.tool_summary_turn_interval, &self.tool);
                }

                if final_answer {
                    let count = self.auto_compact(metrics).await;
                    self.broadcast_token_count(count, metrics.total_tokens);
                } else {
                    self.refresh_token_count(metrics.total_tokens);
                }
            }
            ProviderSignal::Unsupported => {
                metrics.unsupported_signal_count += 1;
            }
        }

        Ok(())
    }

    fn finish_interrupted(&mut self, metrics: &mut TurnMetrics) {
        self.merge_text_delta();
        metrics.completion_reason = "interrupted";

        self.view_bus
            .broadcast(AgentViewEvent::Err("Interrupted by user".to_owned()));
        self.view_bus.broadcast(AgentViewEvent::Completed);
        self.settle_request(metrics);

        tracing::info!(event = "agent.turn.interrupted");
    }

    pub async fn continue_turn(
        &mut self,
        prompt: impl Into<UserInput>,
        cancellation: CancellationToken,
    ) -> anyhow::Result<()> {
        self.continue_turn_with_persistence(prompt.into(), cancellation, false)
            .await
    }

    async fn continue_turn_with_persistence(
        &mut self,
        prompt: UserInput,
        cancellation: CancellationToken,
        archive_after_turn: bool,
    ) -> anyhow::Result<()> {
        let turn_id = Uuid::now_v7();
        let started = Instant::now();
        let span = tracing::info_span!("agent.turn", turn_id = %turn_id);

        self.view_bus.broadcast(AgentViewEvent::TurnStart);

        let result = async {
            tracing::info!(event = "agent.turn.started");

            let result = self.run_turn(prompt, &cancellation).await;
            match &result {
                Ok(metrics) => tracing::info!(
                    event = "agent.turn.completed",
                    provider_request_count = metrics.provider_requests,
                    text_delta_count = metrics.text_delta_count,
                    text_delta_bytes = metrics.text_delta_bytes,
                    tool_call_count = metrics.tool_call_count,
                    unsupported_signal_count = metrics.unsupported_signal_count,
                    total_tokens = metrics.total_tokens,
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

            result
        }
        .instrument(span)
        .await;

        if archive_after_turn {
            self.save_session("turn_finished").await;
        }

        // Only a turn that ran to a final answer is worth summarising; one that
        // failed already reported why.
        let completed = matches!(&result, Ok(metrics) if metrics.completion_reason == "final");

        self.view_bus
            .broadcast(AgentViewEvent::TurnFinished { completed });

        result.map(|_| ())
    }

    async fn run_turn(
        &mut self,
        prompt: UserInput,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<TurnMetrics> {
        self.turn = NextTurn::Prompt(prompt);
        let mut metrics = TurnMetrics::new();

        loop {
            if matches!(self.turn, NextTurn::Stop) {
                return Ok(metrics);
            }

            if metrics.provider_requests >= MAX_TURN_ROUNDS {
                let message = format!(
                    "stopped after {MAX_TURN_ROUNDS} provider requests without a final answer"
                );

                tracing::error!(
                    event = "agent.turn.runaway",
                    error_class = "turn_round_limit",
                    provider_request_count = metrics.provider_requests,
                    tool_call_count = metrics.tool_call_count,
                );
                self.view_bus
                    .broadcast(AgentViewEvent::Err(message.clone()));

                anyhow::bail!(message);
            }

            self.next_turn(&mut metrics, cancellation).await?
        }
    }

    pub async fn archive(&mut self) -> anyhow::Result<()> {
        // Starting `h` and quitting straight away should not leave a titleless
        // row in the session picker.
        if !self.context.has_exchange() {
            tracing::info!(event = "agent.archive.skipped", reason = "no_exchange");
            return Ok(());
        }

        self.context.archive_in(&self.archive_dir).await
    }

    async fn save_session(&mut self, trigger: &'static str) {
        if let Err(error) = self.archive().await {
            tracing::error!(
                event = "agent.archive.failed",
                operation = "autosave",
                error_class = "archive_error",
                trigger,
                error = error.to_string(),
            );
            self.view_bus.broadcast(AgentViewEvent::Err(format!(
                "Failed to save session: {error}"
            )));
        }
    }

    pub async fn run_command(&mut self, command: Command) -> anyhow::Result<()> {
        tracing::info!(event = "agent.command.started", command = command.label());

        let result = match command {
            Command::Clear => self.start_session().await,
            Command::Compact => match self.compact_context().await? {
                CompactOutcome::Applied { .. } | CompactOutcome::Empty => {
                    self.refresh_token_count(None);
                    self.view_bus.broadcast(AgentViewEvent::ContextCompacted);
                    Ok(())
                }
                CompactOutcome::Unsupported => {
                    anyhow::bail!("the current provider does not support context compaction")
                }
            },
        };

        match &result {
            Ok(()) => tracing::info!(event = "agent.command.completed", command = command.label()),
            Err(error) => {
                tracing::error!(
                    event = "agent.command.failed",
                    command = command.label(),
                    error_class = "command_error",
                    error = error.to_string(),
                );
                self.view_bus.broadcast(AgentViewEvent::Err(format!(
                    "Command {} failed: {error}",
                    command.label()
                )));
            }
        }

        self.view_bus
            .broadcast(AgentViewEvent::CommandFinished(command));

        result
    }

    async fn start_session(&mut self) -> anyhow::Result<()> {
        self.archive().await?;
        self.context.start_session();
        self.turn = NextTurn::Continue;

        self.view_bus.broadcast(AgentViewEvent::SessionStarted);
        self.refresh_token_count(None);

        Ok(())
    }

    /// What the user asked in this session, oldest first, for the prompt box to
    /// offer back on recall.
    pub fn prompts(&self) -> Vec<String> {
        self.context.prompts()
    }

    pub async fn resume(&mut self, id: impl AsRef<str>) -> anyhow::Result<&mut Self> {
        let context = Context::resume_in(&self.archive_dir, id.as_ref()).await?;
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
                Message::ToolCallResult {
                    call_id, output, ..
                } => Some((call_id.as_str(), output.as_str())),
                _ => None,
            })
            .collect::<HashMap<_, _>>();

        let mut replayed = 0_usize;

        for message in histories {
            match message {
                // Global prompts, workspace info, and results already folded
                // into the call above them were never on screen.
                Message::System(_)
                | Message::Reasoning(_)
                | Message::Compaction(_)
                | Message::ToolCallResult { .. } => continue,
                Message::Search(search) => {
                    self.view_bus
                        .broadcast(AgentViewEvent::Search(search.clone()));
                }
                Message::User(prompt) => {
                    self.view_bus
                        .broadcast(AgentViewEvent::Prompt(prompt.display()));
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

    async fn open_stream(
        &self,
        messages: &[Message],
        cancellation: &CancellationToken,
    ) -> anyhow::Result<Option<ProviderEventStream>> {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Ok(None),
            opened = self.provider.stream(messages) => opened.map(Some),
        }
    }

    async fn attempt_request(
        &mut self,
        metrics: &mut TurnMetrics,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<RequestAttempt> {
        if !self.prepare_provider_context(metrics, cancellation).await {
            return Ok(RequestAttempt::Interrupted);
        }

        let messages = self.context.provider_messages();
        let input_tokens = self.estimate_request_tokens(&messages);
        let mut stream = match self.open_stream(&messages, cancellation).await {
            Ok(Some(stream)) => stream,
            Ok(None) => return Ok(RequestAttempt::Interrupted),
            Err(error) => {
                return Ok(RequestAttempt::Retry {
                    error,
                    error_class: "provider_stream_open_error",
                    had_output: false,
                });
            }
        };

        metrics.start_request(input_tokens);
        self.refresh_token_count(metrics.total_tokens);

        let mut had_output = false;
        let mut batch = ResponseBatch::default();

        loop {
            let event = tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    self.execute_response_batch(&mut batch, metrics, cancellation).await;
                    self.settle_request(metrics);
                    return Ok(RequestAttempt::Interrupted);
                }
                event = stream.next() => event,
            };

            match event {
                Some(Ok(signal)) => {
                    if cancellation.is_cancelled() {
                        self.execute_response_batch(&mut batch, metrics, cancellation)
                            .await;
                        self.settle_request(metrics);
                        return Ok(RequestAttempt::Interrupted);
                    }

                    let completed = matches!(&signal, ProviderSignal::Completed { .. });
                    had_output |= !matches!(&signal, ProviderSignal::Unsupported);

                    let agent_event: AgentEvent = signal.clone().into();
                    if !completed {
                        self.event_bus.broadcast(agent_event.clone());
                    }

                    if let Err(error) = self
                        .handle_signal(&signal, &mut batch, metrics, cancellation)
                        .await
                    {
                        if !self
                            .execute_response_batch(&mut batch, metrics, cancellation)
                            .await
                        {
                            self.settle_request(metrics);
                            return Ok(RequestAttempt::Interrupted);
                        }

                        self.settle_request(metrics);
                        return Err(error);
                    }

                    if cancellation.is_cancelled() {
                        self.settle_request(metrics);
                        return Ok(RequestAttempt::Interrupted);
                    }

                    if completed {
                        self.event_bus.broadcast(agent_event);
                        self.settle_request(metrics);
                        return Ok(RequestAttempt::Completed);
                    }
                }
                Some(Err(error)) => {
                    if !self
                        .execute_response_batch(&mut batch, metrics, cancellation)
                        .await
                    {
                        self.settle_request(metrics);
                        return Ok(RequestAttempt::Interrupted);
                    }

                    self.settle_request(metrics);
                    return Ok(RequestAttempt::Retry {
                        error,
                        error_class: "provider_stream_error",
                        had_output,
                    });
                }
                None => {
                    if !self
                        .execute_response_batch(&mut batch, metrics, cancellation)
                        .await
                    {
                        self.settle_request(metrics);
                        return Ok(RequestAttempt::Interrupted);
                    }

                    self.settle_request(metrics);
                    return Ok(RequestAttempt::Retry {
                        error: anyhow::anyhow!("provider stream ended before response.completed"),
                        error_class: "provider_stream_eof",
                        had_output,
                    });
                }
            }
        }
    }

    async fn next_turn(
        &mut self,
        metrics: &mut TurnMetrics,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<()> {
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

        let mut attempt = 1;

        loop {
            match self.attempt_request(metrics, cancellation).await? {
                RequestAttempt::Completed => {
                    tracing::info!(
                        event = "agent.provider_request.completed",
                        request_index,
                        attempt,
                        duration_ms = request_started.elapsed().as_millis() as u64
                    );

                    return Ok(());
                }
                RequestAttempt::Interrupted => {
                    self.finish_interrupted(metrics);
                    return Ok(());
                }
                RequestAttempt::Retry {
                    error,
                    error_class,
                    had_output,
                } => {
                    // Preserve partial text and tool results before the next
                    // attempt builds a fresh provider-facing message list.
                    self.merge_text_delta();
                    if had_output {
                        self.view_bus.broadcast(AgentViewEvent::Completed);
                    }

                    self.view_bus
                        .broadcast(AgentViewEvent::Err(error.to_string()));

                    if attempt == STREAM_MAX_ATTEMPTS {
                        tracing::error!(
                            event = "agent.provider_request.exhausted",
                            request_index,
                            attempt,
                            error_class,
                            error = error.to_string(),
                            duration_ms = request_started.elapsed().as_millis() as u64
                        );

                        return Err(error);
                    }

                    let delay = stream_retry_delay(attempt);
                    tracing::warn!(
                        event = "agent.provider_request.retrying",
                        request_index,
                        attempt,
                        max_attempts = STREAM_MAX_ATTEMPTS,
                        delay_ms = delay.as_millis() as u64,
                        error_class,
                        error = error.to_string(),
                    );

                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => {
                            self.finish_interrupted(metrics);
                            return Ok(());
                        }
                        _ = tokio::time::sleep(delay) => {}
                    }

                    attempt += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs as std_fs,
        path::PathBuf,
        pin::Pin,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        task::{Context as TaskContext, Poll},
    };

    use futures::{Stream, stream};
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use tokio::sync::Notify;

    use super::*;
    use crate::{
        context::{Search, SearchAction},
        provider::Compaction,
        tool::{
            FileBufferStore, ReadFileTool, Summary, ToolCall, ToolCallStatus, ToolDefinition,
            ToolOutput, TypedTool,
        },
    };

    fn cancellation() -> CancellationToken {
        CancellationToken::new()
    }

    struct TempArchive {
        path: PathBuf,
    }

    impl TempArchive {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("h-agent-archive-{}", Uuid::new_v4()));
            std_fs::create_dir_all(&path).unwrap();

            Self { path }
        }
    }

    impl Drop for TempArchive {
        fn drop(&mut self) {
            let _ = std_fs::remove_dir_all(&self.path);
        }
    }

    fn completed(reason: CompletedReason) -> ProviderSignal {
        ProviderSignal::Completed { reason }
    }

    struct TestProvider;

    #[async_trait::async_trait]
    impl Provider for TestProvider {
        fn model(&self) -> &str {
            "test-model"
        }

        fn thinking_effort(&self) -> Option<&str> {
            Some("high")
        }

        fn define_tools(&mut self, _specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
            Ok(())
        }

        fn estimate_request_tokens(&self, input: &[Message]) -> anyhow::Result<Option<usize>> {
            Ok(Some(input.len()))
        }

        fn estimate_output_tokens(&self, output: &[Message]) -> anyhow::Result<Option<usize>> {
            Ok(Some(output.len()))
        }

        async fn stream(&self, _input: &[Message]) -> anyhow::Result<ProviderEventStream> {
            Ok(Box::pin(stream::empty()))
        }
    }

    struct CompactingProvider {
        input: Arc<Mutex<Vec<Message>>>,
        stream_saw_compaction: Option<Arc<AtomicBool>>,
    }

    #[async_trait::async_trait]
    impl Provider for CompactingProvider {
        fn model(&self) -> &str {
            "compacting-model"
        }

        fn thinking_effort(&self) -> Option<&str> {
            None
        }

        fn define_tools(&mut self, _specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
            Ok(())
        }

        fn estimate_request_tokens(&self, input: &[Message]) -> anyhow::Result<Option<usize>> {
            let count = if input
                .iter()
                .any(|message| matches!(message, Message::Compaction(_)))
            {
                50
            } else {
                input.len().saturating_mul(100)
            };

            Ok(Some(count))
        }

        fn estimate_output_tokens(&self, output: &[Message]) -> anyhow::Result<Option<usize>> {
            Ok(Some(output.len().saturating_mul(20)))
        }

        async fn compact(&self, input: &[Message]) -> anyhow::Result<Option<Compaction>> {
            *self.input.lock().unwrap() = input.to_vec();

            let state = serde_json::to_vec(&json!([{
                "type": "compaction",
                "id": "cmp-1",
                "encrypted_content": "opaque",
            }]))?;

            Ok(Some(Compaction::new(state, Some(30))))
        }

        async fn stream(&self, input: &[Message]) -> anyhow::Result<ProviderEventStream> {
            if let Some(stream_saw_compaction) = &self.stream_saw_compaction {
                let compacted = !self.input.lock().unwrap().is_empty()
                    && input
                        .iter()
                        .any(|message| matches!(message, Message::Compaction(_)));

                stream_saw_compaction.store(compacted, Ordering::SeqCst);
            }

            Ok(Box::pin(stream::once(async {
                Ok(completed(CompletedReason::Final))
            })))
        }
    }

    struct CapturingProvider {
        input: Arc<Mutex<Vec<Message>>>,
    }

    #[async_trait::async_trait]
    impl Provider for CapturingProvider {
        fn model(&self) -> &str {
            "capturing-model"
        }

        fn thinking_effort(&self) -> Option<&str> {
            None
        }

        fn define_tools(&mut self, _specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
            Ok(())
        }

        async fn stream(&self, input: &[Message]) -> anyhow::Result<ProviderEventStream> {
            *self.input.lock().unwrap() = input.to_vec();
            Ok(Box::pin(stream::empty()))
        }
    }

    struct PendingStream {
        polled: Arc<Notify>,
        dropped: Arc<AtomicBool>,
    }

    impl Stream for PendingStream {
        type Item = anyhow::Result<ProviderSignal>;

        fn poll_next(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
            self.polled.notify_one();
            Poll::Pending
        }
    }

    impl Drop for PendingStream {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    struct PendingStreamProvider {
        polled: Arc<Notify>,
        dropped: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl Provider for PendingStreamProvider {
        fn model(&self) -> &str {
            "pending-model"
        }

        fn thinking_effort(&self) -> Option<&str> {
            None
        }

        fn define_tools(&mut self, _specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
            Ok(())
        }

        fn estimate_request_tokens(&self, input: &[Message]) -> anyhow::Result<Option<usize>> {
            Ok(Some(input.len().saturating_mul(10)))
        }

        fn estimate_output_tokens(&self, output: &[Message]) -> anyhow::Result<Option<usize>> {
            Ok(Some(text_bytes(output)))
        }

        async fn stream(&self, _input: &[Message]) -> anyhow::Result<ProviderEventStream> {
            Ok(Box::pin(PendingStream {
                polled: self.polled.clone(),
                dropped: self.dropped.clone(),
            }))
        }
    }

    struct PartialProvider {
        handled: Arc<Notify>,
    }

    #[async_trait::async_trait]
    impl Provider for PartialProvider {
        fn model(&self) -> &str {
            "partial-model"
        }

        fn thinking_effort(&self) -> Option<&str> {
            None
        }

        fn define_tools(&mut self, _specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
            Ok(())
        }

        fn estimate_request_tokens(&self, input: &[Message]) -> anyhow::Result<Option<usize>> {
            Ok(Some(input.len().saturating_mul(10)))
        }

        fn estimate_output_tokens(&self, output: &[Message]) -> anyhow::Result<Option<usize>> {
            Ok(Some(text_bytes(output)))
        }

        async fn stream(&self, _input: &[Message]) -> anyhow::Result<ProviderEventStream> {
            self.handled.notify_one();
            let events =
                stream::once(async { Ok(ProviderSignal::TextDelta("partial answer".to_owned())) })
                    .chain(stream::pending());

            Ok(Box::pin(events))
        }
    }

    struct ToolProvider {
        calls: Vec<ToolCall>,
    }

    #[async_trait::async_trait]
    impl Provider for ToolProvider {
        fn model(&self) -> &str {
            "tool-model"
        }

        fn thinking_effort(&self) -> Option<&str> {
            None
        }

        fn define_tools(&mut self, _specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
            Ok(())
        }

        async fn stream(&self, _input: &[Message]) -> anyhow::Result<ProviderEventStream> {
            let mut events = self
                .calls
                .iter()
                .cloned()
                .map(ProviderSignal::ToolCallStarted)
                .collect::<Vec<_>>();
            events.push(completed(CompletedReason::NeedCall));

            Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
        }
    }

    struct CancelAfterToolCallStream {
        cancellation: CancellationToken,
        state: u8,
    }

    impl Stream for CancelAfterToolCallStream {
        type Item = anyhow::Result<ProviderSignal>;

        fn poll_next(
            mut self: Pin<&mut Self>,
            _cx: &mut TaskContext<'_>,
        ) -> Poll<Option<Self::Item>> {
            let event = match self.state {
                0 => ProviderSignal::ToolCallStarted(ToolCall::new("call-1", "missing", json!({}))),
                1 => {
                    self.cancellation.cancel();
                    ProviderSignal::Unsupported
                }
                _ => return Poll::Pending,
            };
            self.state += 1;

            Poll::Ready(Some(Ok(event)))
        }
    }

    struct CancelAfterToolCallProvider {
        cancellation: CancellationToken,
    }

    #[async_trait::async_trait]
    impl Provider for CancelAfterToolCallProvider {
        fn model(&self) -> &str {
            "cancel-after-tool-call-model"
        }

        fn thinking_effort(&self) -> Option<&str> {
            None
        }

        fn define_tools(&mut self, _specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
            Ok(())
        }

        async fn stream(&self, _input: &[Message]) -> anyhow::Result<ProviderEventStream> {
            Ok(Box::pin(CancelAfterToolCallStream {
                cancellation: self.cancellation.clone(),
                state: 0,
            }))
        }
    }

    #[derive(Clone, Deserialize, JsonSchema)]
    struct BlockingArgs {}

    #[derive(Serialize)]
    struct BlockingOutput {}

    struct BlockingTool {
        started: Arc<Notify>,
        cancelled: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl TypedTool for BlockingTool {
        type Arguments = BlockingArgs;
        type Output = BlockingOutput;

        fn name(&self) -> &'static str {
            "blocking"
        }

        fn description(&self) -> &'static str {
            "wait forever for cancellation tests"
        }

        async fn call(
            &self,
            _arguments: Self::Arguments,
        ) -> anyhow::Result<ToolOutput<Self::Output>> {
            self.started.notify_one();
            std::future::pending().await
        }

        async fn cancel(&self, _arguments: Self::Arguments) -> anyhow::Result<()> {
            self.cancelled.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    /// Reports a scripted completion reason per provider request, so a turn that
    /// spans several rounds can be driven without a provider.
    ///
    /// Once the script runs out it reports nothing, which exercises the
    /// premature-EOF path. A provider that answered `NeedCall` forever would
    /// keep [`Agent::run_turn`] opening fresh requests and never return.
    struct ScriptedProvider {
        reasons: Mutex<std::collections::VecDeque<CompletedReason>>,
    }

    impl ScriptedProvider {
        fn new(reasons: impl IntoIterator<Item = CompletedReason>) -> Self {
            Self {
                reasons: Mutex::new(reasons.into_iter().collect()),
            }
        }
    }

    #[async_trait::async_trait]
    impl Provider for ScriptedProvider {
        fn model(&self) -> &str {
            "scripted-model"
        }

        fn thinking_effort(&self) -> Option<&str> {
            None
        }

        fn define_tools(&mut self, _specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
            Ok(())
        }

        async fn stream(&self, _input: &[Message]) -> anyhow::Result<ProviderEventStream> {
            let signal = self
                .reasons
                .lock()
                .unwrap()
                .pop_front()
                .map_or(ProviderSignal::Unsupported, completed);

            Ok(Box::pin(stream::once(async move { Ok(signal) })))
        }
    }

    struct EstimatingProvider {
        completions: Mutex<std::collections::VecDeque<CompletedReason>>,
    }

    impl EstimatingProvider {
        fn new(completions: impl IntoIterator<Item = CompletedReason>) -> Self {
            Self {
                completions: Mutex::new(completions.into_iter().collect()),
            }
        }
    }

    #[async_trait::async_trait]
    impl Provider for EstimatingProvider {
        fn model(&self) -> &str {
            "estimating-model"
        }

        fn thinking_effort(&self) -> Option<&str> {
            None
        }

        fn define_tools(&mut self, _specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
            Ok(())
        }

        fn estimate_request_tokens(&self, input: &[Message]) -> anyhow::Result<Option<usize>> {
            Ok(Some(input.len()))
        }

        fn estimate_output_tokens(&self, output: &[Message]) -> anyhow::Result<Option<usize>> {
            Ok(Some(output.len()))
        }

        async fn stream(&self, _input: &[Message]) -> anyhow::Result<ProviderEventStream> {
            let signal = self
                .completions
                .lock()
                .unwrap()
                .pop_front()
                .map_or(ProviderSignal::Unsupported, completed);

            Ok(Box::pin(stream::once(async move { Ok(signal) })))
        }
    }

    struct SignalProvider {
        streams: Mutex<std::collections::VecDeque<Vec<ProviderSignal>>>,
        estimate_output: fn(&[Message]) -> usize,
        inputs: Option<Arc<Mutex<Vec<Vec<Message>>>>>,
    }

    impl SignalProvider {
        fn new(
            streams: impl IntoIterator<Item = Vec<ProviderSignal>>,
            estimate_output: fn(&[Message]) -> usize,
        ) -> Self {
            Self {
                streams: Mutex::new(streams.into_iter().collect()),
                estimate_output,
                inputs: None,
            }
        }

        fn record_inputs(mut self, inputs: Arc<Mutex<Vec<Vec<Message>>>>) -> Self {
            self.inputs = Some(inputs);
            self
        }
    }

    #[async_trait::async_trait]
    impl Provider for SignalProvider {
        fn model(&self) -> &str {
            "signal-model"
        }

        fn thinking_effort(&self) -> Option<&str> {
            None
        }

        fn define_tools(&mut self, _specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
            Ok(())
        }

        fn estimate_request_tokens(&self, input: &[Message]) -> anyhow::Result<Option<usize>> {
            Ok(Some(input.len().saturating_mul(10)))
        }

        fn estimate_output_tokens(&self, output: &[Message]) -> anyhow::Result<Option<usize>> {
            Ok(Some((self.estimate_output)(output)))
        }

        async fn stream(&self, input: &[Message]) -> anyhow::Result<ProviderEventStream> {
            if let Some(inputs) = &self.inputs {
                inputs.lock().unwrap().push(input.to_vec());
            }

            let events = self.streams.lock().unwrap().pop_front().unwrap_or_default();

            Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
        }
    }

    fn text_bytes(output: &[Message]) -> usize {
        output
            .iter()
            .filter_map(|message| match message {
                Message::Assistant(text) => Some(text.len()),
                _ => None,
            })
            .sum()
    }

    fn boundary_tokens(output: &[Message]) -> usize {
        let text = output
            .iter()
            .filter_map(|message| match message {
                Message::Assistant(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();

        match text.as_str() {
            "" => 0,
            "hel" | "lo" => 2,
            "hello" => 1,
            _ => text.len(),
        }
    }

    fn turn_finished(events: &mut UnboundedReceiver<AgentViewEvent>) -> Option<bool> {
        std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
            AgentViewEvent::TurnFinished { completed } => Some(completed),
            _ => None,
        })
    }

    fn error_messages(events: &mut UnboundedReceiver<AgentViewEvent>) -> Vec<String> {
        std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event {
                AgentViewEvent::Err(message) => Some(message),
                _ => None,
            })
            .collect()
    }

    async fn receive_turn_finished(events: &mut UnboundedReceiver<AgentViewEvent>) -> bool {
        loop {
            match events.recv().await {
                Some(AgentViewEvent::TurnFinished { completed }) => return completed,
                Some(_) => {}
                None => panic!("the view event stream closed before the turn finished"),
            }
        }
    }

    #[tokio::test]
    async fn command_loop_executes_a_prompt_to_completion() {
        let archive = TempArchive::new();
        let mut agent = Agent::new(SignalProvider::new(
            [vec![
                ProviderSignal::TextDelta("hello".to_owned()),
                completed(CompletedReason::Final),
            ]],
            text_bytes,
        ));
        agent.with_archive_dir(&archive.path);
        let (archive_path, session_id) = (archive.path.clone(), agent.context.id());
        let mut events = agent.subscribe_view();
        let (commands, receiver) = tokio::sync::mpsc::channel(1);

        let run = agent.run(receiver);
        let drive = async move {
            commands
                .send(AgentCommand::Prompt("say hello".into()))
                .await
                .unwrap();

            assert!(receive_turn_finished(&mut events).await);
            assert!(archive_path.join(format!("{session_id}.archive")).is_file());
            drop(commands);
        };

        tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(run, drive);
        })
        .await
        .expect("the command loop should finish after its sender closes");

        let saved = Context::resume_in(&archive.path, &agent.context.id())
            .await
            .unwrap();

        assert!(matches!(
            saved.histories(),
            [Message::User(prompt), Message::Assistant(answer)]
                if prompt.text() == "say hello" && answer == "hello"
        ));
    }

    #[tokio::test]
    async fn command_loop_preserves_queued_prompt_order() {
        let archive = TempArchive::new();
        let mut agent = Agent::new(SignalProvider::new(
            [
                vec![
                    ProviderSignal::TextDelta("first answer".to_owned()),
                    completed(CompletedReason::Final),
                ],
                vec![
                    ProviderSignal::TextDelta("second answer".to_owned()),
                    completed(CompletedReason::Final),
                ],
            ],
            text_bytes,
        ));
        agent.with_archive_dir(&archive.path);
        let mut events = agent.subscribe_view();
        let (commands, receiver) = tokio::sync::mpsc::channel(2);

        let run = agent.run(receiver);
        let drive = async move {
            commands
                .send(AgentCommand::Prompt("first prompt".into()))
                .await
                .unwrap();
            commands
                .send(AgentCommand::Prompt("second prompt".into()))
                .await
                .unwrap();

            assert!(receive_turn_finished(&mut events).await);
            assert!(receive_turn_finished(&mut events).await);
            drop(commands);
        };

        tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(run, drive);
        })
        .await
        .expect("queued prompts should both finish");

        assert!(matches!(
            agent.context.histories(),
            [
                Message::User(first_prompt),
                Message::Assistant(first_answer),
                Message::User(second_prompt),
                Message::Assistant(second_answer),
            ] if first_prompt.text() == "first prompt"
                && first_answer == "first answer"
                && second_prompt.text() == "second prompt"
                && second_answer == "second answer"
        ));
    }

    #[tokio::test]
    async fn command_loop_cancels_the_active_turn() {
        let archive = TempArchive::new();
        let (polled, dropped) = (Arc::new(Notify::new()), Arc::new(AtomicBool::new(false)));
        let mut agent = Agent::new(PendingStreamProvider {
            polled: polled.clone(),
            dropped: dropped.clone(),
        });
        agent.with_archive_dir(&archive.path);
        let mut events = agent.subscribe_view();
        let (commands, receiver) = tokio::sync::mpsc::channel(1);

        let run = agent.run(receiver);
        let drive = async move {
            commands
                .send(AgentCommand::Prompt("wait forever".into()))
                .await
                .unwrap();
            polled.notified().await;

            commands.send(AgentCommand::Cancel).await.unwrap();

            assert!(!receive_turn_finished(&mut events).await);
            drop(commands);
        };

        tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(run, drive);
        })
        .await
        .expect("cancellation should stop the active turn");

        let saved = Context::resume_in(&archive.path, &agent.context.id())
            .await
            .unwrap();

        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(saved.prompts(), ["wait forever"]);
    }

    #[tokio::test]
    async fn closing_the_command_channel_cancels_the_active_turn() {
        let archive = TempArchive::new();
        let (polled, dropped) = (Arc::new(Notify::new()), Arc::new(AtomicBool::new(false)));
        let mut agent = Agent::new(PendingStreamProvider {
            polled: polled.clone(),
            dropped: dropped.clone(),
        });
        agent.with_archive_dir(&archive.path);
        let mut events = agent.subscribe_view();
        let (commands, receiver) = tokio::sync::mpsc::channel(1);

        let run = agent.run(receiver);
        let drive = async move {
            commands
                .send(AgentCommand::Prompt("wait forever".into()))
                .await
                .unwrap();
            polled.notified().await;

            drop(commands);

            assert!(!receive_turn_finished(&mut events).await);
        };

        tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(run, drive);
        })
        .await
        .expect("closing the command channel should stop the worker");

        let saved = Context::resume_in(&archive.path, &agent.context.id())
            .await
            .unwrap();

        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(saved.prompts(), ["wait forever"]);
    }

    #[tokio::test(start_paused = true)]
    async fn command_loop_archives_a_turn_that_ends_with_an_error() {
        let archive = TempArchive::new();
        let mut agent = Agent::new(FlakyProvider::new(
            FailurePoint::Open,
            STREAM_MAX_ATTEMPTS as usize,
        ));
        agent.with_archive_dir(&archive.path);
        let (archive_path, session_id) = (archive.path.clone(), agent.context.id());
        let mut events = agent.subscribe_view();
        let (commands, receiver) = tokio::sync::mpsc::channel(1);

        let run = agent.run(receiver);
        let drive = async move {
            commands
                .send(AgentCommand::Prompt("save this prompt".into()))
                .await
                .unwrap();

            assert!(!receive_turn_finished(&mut events).await);
            assert!(archive_path.join(format!("{session_id}.archive")).is_file());
            drop(commands);
        };

        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(run, drive);
        })
        .await
        .expect("a failed turn should still reach its archive boundary");

        let saved = Context::resume_in(&archive.path, &agent.context.id())
            .await
            .unwrap();

        assert_eq!(saved.prompts(), ["save this prompt"]);
    }

    fn seed_read_summary<P: Provider>(agent: &mut Agent<P>) {
        agent
            .tool
            .register(ReadFileTool::new(FileBufferStore::default()));
        *agent.context.histories_mut() = vec![
            Message::ToolCall {
                call_id: "read-1".to_owned(),
                name: "read_file".to_owned(),
                arguments: json!({"path": "src/main.rs"}).to_string(),
            },
            Message::ToolCallResult {
                call_id: "read-1".to_owned(),
                output: json!({"content": "fn main() {}"}).to_string(),
                summary: Some(Summary::new(
                    1,
                    json!({
                        "path": "src/main.rs",
                        "lines": 1,
                    }),
                )),
            },
        ];
    }

    fn agent_with_read_summary() -> Agent<TestProvider> {
        let mut agent = Agent::new(TestProvider);
        seed_read_summary(&mut agent);

        agent
    }

    fn compacted_read_output(lines: usize) -> String {
        format!(
            "<tool-summary>Older tool output truncated. Tool \"read_file\" succeeded. Read {lines} lines from \"src/main.rs\".</tool-summary>"
        )
    }

    fn has_compacted_tool_output<P: Provider>(agent: &Agent<P>) -> bool {
        matches!(
            agent.context.provider_messages().as_slice(),
            [
                Message::ToolCall { call_id: call, .. },
                Message::ToolCallResult { call_id: result, output, .. },
            ] if call == "read-1"
                && result == call
                && output == &compacted_read_output(1)
        )
    }

    #[tokio::test]
    async fn reasoning_signals_are_stored_without_view_content() {
        let reasoning =
            br#"{"type":"reasoning","id":"rs-1","summary":[],"encrypted_content":"opaque"}"#
                .to_vec();
        let mut agent = Agent::new(TestProvider);
        let mut events = agent.subscribe_view();
        let mut metrics = TurnMetrics::new();
        let mut batch = ResponseBatch::default();

        agent
            .handle_signal(
                &ProviderSignal::Reasoning(reasoning.clone()),
                &mut batch,
                &mut metrics,
                &cancellation(),
            )
            .await
            .unwrap();

        assert!(matches!(
            agent.context.histories(),
            [Message::Reasoning(item)] if item == &reasoning
        ));
        let emitted = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        assert!(
            emitted
                .iter()
                .all(|event| matches!(event, AgentViewEvent::TokenUsage { .. }))
        );

        agent.rebroadcast_all_view();
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn search_signals_are_stored_and_sent_to_the_view() {
        let search = Search::new(
            "ws-1",
            SearchStatus::Succeeded,
            Some(SearchAction::Query {
                query: "Rust async runtimes".to_owned(),
                sources: Vec::new(),
            }),
            br#"{"type":"web_search_call","id":"ws-1"}"#.to_vec(),
        );
        let mut agent = Agent::new(TestProvider);
        let mut events = agent.subscribe_view();
        let mut metrics = TurnMetrics::new();
        let mut batch = ResponseBatch::default();

        agent
            .handle_signal(
                &ProviderSignal::Search(search.clone()),
                &mut batch,
                &mut metrics,
                &cancellation(),
            )
            .await
            .unwrap();

        assert!(matches!(
            agent.context.histories(),
            [Message::Search(item)] if item == &search
        ));
        assert!(
            std::iter::from_fn(|| events.try_recv().ok())
                .any(|event| matches!(event, AgentViewEvent::Search(item) if item == search))
        );

        agent.rebroadcast_all_view();
        assert!(matches!(
            events.try_recv(),
            Ok(AgentViewEvent::Search(item)) if item == search
        ));
    }

    #[tokio::test]
    async fn need_call_does_not_advance_the_tool_summary_interval() {
        let mut agent = agent_with_read_summary();
        agent.with_tool_summary_turn_interval(NonZeroUsize::new(1).unwrap());
        let mut metrics = TurnMetrics::default();
        let mut batch = ResponseBatch::default();

        agent
            .handle_signal(
                &completed(CompletedReason::NeedCall),
                &mut batch,
                &mut metrics,
                &cancellation(),
            )
            .await
            .unwrap();

        assert!(!has_compacted_tool_output(&agent));

        agent
            .handle_signal(
                &completed(CompletedReason::Final),
                &mut batch,
                &mut metrics,
                &cancellation(),
            )
            .await
            .unwrap();

        assert!(has_compacted_tool_output(&agent));
    }

    #[tokio::test]
    async fn the_default_interval_freezes_on_the_eighth_final_turn() {
        let mut agent = agent_with_read_summary();
        let mut metrics = TurnMetrics::default();
        let mut batch = ResponseBatch::default();

        for _ in 0..7 {
            agent
                .handle_signal(
                    &completed(CompletedReason::Final),
                    &mut batch,
                    &mut metrics,
                    &cancellation(),
                )
                .await
                .unwrap();
        }

        assert!(!has_compacted_tool_output(&agent));

        agent
            .handle_signal(
                &completed(CompletedReason::Final),
                &mut batch,
                &mut metrics,
                &cancellation(),
            )
            .await
            .unwrap();

        assert!(has_compacted_tool_output(&agent));
    }

    #[tokio::test]
    async fn completion_recomputes_the_tokenized_context_size() {
        let mut agent = agent_with_read_summary();
        agent.with_tool_summary_turn_interval(NonZeroUsize::new(1).unwrap());
        let mut metrics = TurnMetrics::new();
        metrics.start_request(Some(7));
        let mut batch = ResponseBatch::default();

        agent
            .handle_signal(
                &completed(CompletedReason::Final),
                &mut batch,
                &mut metrics,
                &cancellation(),
            )
            .await
            .unwrap();

        assert!(has_compacted_tool_output(&agent));
        assert_eq!(agent.context.token_count(), Some(2));
        assert_eq!(metrics.total_tokens, Some(7));
    }

    #[tokio::test]
    async fn compact_command_preserves_tool_outputs_for_provider_compaction() {
        let input = Arc::new(Mutex::new(Vec::new()));
        let mut agent = Agent::new(CompactingProvider {
            input: input.clone(),
            stream_saw_compaction: None,
        });
        let mut events = agent.subscribe_view();
        seed_read_summary(&mut agent);

        agent.run_command(Command::Compact).await.unwrap();

        assert!(matches!(
            input.lock().unwrap().as_slice(),
            [
                Message::ToolCall { call_id: call, .. },
                Message::ToolCallResult { call_id: result, output, .. },
            ] if call == "read-1"
                && result == call
                && output == r#"{"content":"fn main() {}"}"#
        ));
        assert!(matches!(
            agent.context.provider_messages().as_slice(),
            [Message::Compaction(_)]
        ));
        let events = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();

        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentViewEvent::ContextCompacted)),
            "manual compaction identifies the provider compaction"
        );
    }

    #[tokio::test]
    async fn automatic_tool_result_compaction_does_not_emit_a_context_notice() {
        let mut agent = agent_with_read_summary();
        let mut events = agent.subscribe_view();
        let mut metrics = TurnMetrics::new();

        agent.with_auto_compact_token_limit(2);
        assert_eq!(agent.auto_compact(&mut metrics).await, Some(2));

        let events = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();

        assert!(has_compacted_tool_output(&agent));
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, AgentViewEvent::ContextCompacted))
        );
    }

    #[tokio::test]
    async fn tool_results_compact_with_the_active_prompt_before_continuing() {
        let input = Arc::new(Mutex::new(Vec::new()));
        let mut agent = Agent::new(CompactingProvider {
            input: input.clone(),
            stream_saw_compaction: None,
        });
        let mut events = agent.subscribe_view();
        agent.with_auto_compact_token_limit(200);
        agent.append_prompt("inspect the project".into());
        let mut metrics = TurnMetrics::new();
        let mut batch = ResponseBatch::default();

        agent
            .handle_signal(
                &ProviderSignal::ToolCallStarted(ToolCall::new(
                    "call-1",
                    "no_such_tool",
                    json!({}),
                )),
                &mut batch,
                &mut metrics,
                &cancellation(),
            )
            .await
            .unwrap();

        agent
            .handle_signal(
                &completed(CompletedReason::NeedCall),
                &mut batch,
                &mut metrics,
                &cancellation(),
            )
            .await
            .unwrap();

        assert!(matches!(
            input.lock().unwrap().as_slice(),
            [
                Message::User(prompt),
                Message::ToolCall { call_id, .. },
                Message::ToolCallResult {
                    call_id: result_id,
                    ..
                },
            ] if prompt.text() == "inspect the project"
                && call_id == "call-1"
                && result_id == "call-1"
        ));
        assert!(matches!(
            agent.context.provider_messages().as_slice(),
            [Message::Compaction(_)]
        ));
        assert_eq!(agent.prompts(), ["inspect the project"]);

        let seen = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        assert!(
            seen.iter()
                .any(|event| matches!(event, AgentViewEvent::ContextCompacted))
        );
        assert!(seen.iter().all(|event| !matches!(
            event,
            AgentViewEvent::TokenUsage {
                context: Some(count),
                ..
            } if *count > 200
        )));

        assert!(matches!(agent.turn, NextTurn::Continue));
        assert_eq!(agent.prompts(), ["inspect the project"]);
    }

    #[tokio::test]
    async fn requests_trigger_provider_compaction_at_the_configured_limit() {
        let input = Arc::new(Mutex::new(Vec::new()));
        let mut agent = Agent::new(CompactingProvider {
            input: input.clone(),
            stream_saw_compaction: None,
        });
        let mut events = agent.subscribe_view();
        agent.with_auto_compact_token_limit(100);
        agent
            .context
            .histories_mut()
            .push(Message::System("instructions".to_owned()));

        agent
            .continue_turn("inspect the project", cancellation())
            .await
            .unwrap();

        assert!(matches!(
            input.lock().unwrap().as_slice(),
            [Message::User(prompt)] if prompt.text() == "inspect the project"
        ));
        assert!(matches!(
            agent.context.provider_messages().as_slice(),
            [Message::System(system), Message::Compaction(_)] if system == "instructions"
        ));
        let events = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();

        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentViewEvent::ContextCompacted))
        );
        assert_eq!(
            events
                .iter()
                .filter_map(|event| match event {
                    AgentViewEvent::TokenUsage { turn, .. } => *turn,
                    _ => None,
                })
                .next_back(),
            Some(80),
            "the turn total includes local estimates for compaction and the normal request"
        );
    }

    #[tokio::test]
    async fn tool_grown_context_is_compacted_before_the_next_request() {
        let input = Arc::new(Mutex::new(Vec::new()));
        let stream_saw_compaction = Arc::new(AtomicBool::new(false));
        let mut agent = Agent::new(CompactingProvider {
            input: input.clone(),
            stream_saw_compaction: Some(stream_saw_compaction.clone()),
        });
        let mut events = agent.subscribe_view();
        agent.with_auto_compact_token_limit(100);
        seed_read_summary(&mut agent);
        let mut metrics = TurnMetrics::new();

        let attempt = agent
            .attempt_request(&mut metrics, &cancellation())
            .await
            .unwrap();

        assert!(!input.lock().unwrap().is_empty());
        assert!(stream_saw_compaction.load(Ordering::SeqCst));

        let events = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        let contexts = events
            .iter()
            .filter_map(|event| match event {
                AgentViewEvent::TokenUsage { context, .. } => *context,
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(matches!(attempt, RequestAttempt::Completed));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentViewEvent::ContextCompacted))
        );
        assert!(!contexts.is_empty());
        assert!(contexts.iter().all(|count| *count <= 100));
    }

    #[tokio::test]
    async fn clear_command_starts_a_new_context_and_notifies_the_view() {
        let mut agent = Agent::new(TestProvider);
        let mut events = agent.subscribe_view();
        let old_id = agent.context.id();

        agent
            .context
            .histories_mut()
            .push(Message::System("instructions".to_owned()));

        agent.run_command(Command::Clear).await.unwrap();

        assert_ne!(agent.context.id(), old_id);
        assert!(matches!(
            agent.context.histories(),
            [Message::System(instructions)] if instructions == "instructions"
        ));
        assert!(
            std::iter::from_fn(|| events.try_recv().ok())
                .any(|event| matches!(event, AgentViewEvent::SessionStarted))
        );
        assert!(
            std::iter::from_fn(|| events.try_recv().ok())
                .any(|event| matches!(event, AgentViewEvent::CommandFinished(Command::Clear)))
        );
    }

    #[tokio::test]
    async fn provider_stream_receives_the_compacted_projection() {
        let input = Arc::new(Mutex::new(Vec::new()));
        let mut agent = Agent::new(CapturingProvider {
            input: input.clone(),
        });
        agent
            .tool
            .register(ReadFileTool::new(FileBufferStore::default()));
        *agent.context.histories_mut() = vec![
            Message::ToolCall {
                call_id: "read-1".to_owned(),
                name: "read_file".to_owned(),
                arguments: "{}".to_owned(),
            },
            Message::ToolCallResult {
                call_id: "read-1".to_owned(),
                output: "large output".to_owned(),
                summary: Some(Summary::new(
                    1,
                    json!({
                        "path": "src/main.rs",
                        "lines": 200,
                    }),
                )),
            },
        ];
        agent
            .context
            .complete_turn(NonZeroUsize::new(1).unwrap(), &agent.tool);

        let messages = agent.context.provider_messages();
        agent.open_stream(&messages, &cancellation()).await.unwrap();

        assert!(matches!(
            input.lock().unwrap().as_slice(),
            [
                Message::ToolCall { call_id: call, .. },
                Message::ToolCallResult { call_id: result, output, .. },
            ] if call == "read-1"
                && result == call
                && output == &compacted_read_output(200)
        ));
        assert_eq!(agent.context.histories().len(), 2);
    }

    #[tokio::test]
    async fn cancellation_between_events_drops_the_stream_and_keeps_user_prompts() {
        let (polled, dropped) = (Arc::new(Notify::new()), Arc::new(AtomicBool::new(false)));
        let mut agent = Agent::new(PendingStreamProvider {
            polled: polled.clone(),
            dropped: dropped.clone(),
        });
        let mut events = agent.subscribe_view();

        agent.append_prompt("earlier prompt".into());

        let cancellation = cancellation();
        let trigger = cancellation.clone();
        let canceller = tokio::spawn(async move {
            polled.notified().await;
            trigger.cancel();
        });

        agent
            .continue_turn("cancel this prompt", cancellation)
            .await
            .unwrap();
        canceller.await.unwrap();

        assert!(
            dropped.load(Ordering::SeqCst),
            "dropping the provider stream terminates its HTTP/SSE connection"
        );
        assert_eq!(
            agent.prompts(),
            ["earlier prompt", "cancel this prompt"],
            "cancellation must not roll back user input"
        );

        let seen = drain(&mut events);
        assert!(seen.iter().any(
            |event| matches!(event, AgentViewEvent::Err(message) if message == "Interrupted by user")
        ));
        assert!(
            seen.iter()
                .any(|event| matches!(event, AgentViewEvent::TurnFinished { completed: false }))
        );
    }

    #[tokio::test]
    async fn cancellation_preserves_text_received_before_the_next_stream_event() {
        let handled = Arc::new(Notify::new());
        let mut agent = Agent::new(PartialProvider {
            handled: handled.clone(),
        });

        let cancellation = cancellation();
        let trigger = cancellation.clone();
        let canceller = tokio::spawn(async move {
            handled.notified().await;
            trigger.cancel();
        });

        agent
            .continue_turn("start answering", cancellation)
            .await
            .unwrap();
        canceller.await.unwrap();

        assert!(agent.context.histories().iter().any(|message| {
            matches!(message, Message::Assistant(text) if text == "partial answer")
        }));
    }

    #[tokio::test]
    async fn cancelling_a_tool_batch_interrupts_the_active_and_queued_calls() {
        let (started, cancelled) = (Arc::new(Notify::new()), Arc::new(AtomicBool::new(false)));
        let calls = vec![
            ToolCall::new("call-1", "blocking", json!({})),
            ToolCall::new("call-2", "missing", json!({})),
        ];
        let mut agent = Agent::new(ToolProvider { calls });

        agent.tool.register(BlockingTool {
            started: started.clone(),
            cancelled: cancelled.clone(),
        });

        let cancellation = cancellation();
        let trigger = cancellation.clone();
        let canceller = tokio::spawn(async move {
            started.notified().await;
            trigger.cancel();
        });

        agent
            .continue_turn("run the blocking tool", cancellation)
            .await
            .unwrap();
        canceller.await.unwrap();

        assert!(
            cancelled.load(Ordering::SeqCst),
            "the registry must delegate cancellation to the active tool"
        );

        let histories = agent.context.histories();
        let call_count = histories
            .iter()
            .filter(|message| matches!(message, Message::ToolCall { .. }))
            .count();
        let results = histories
            .iter()
            .filter_map(|message| match message {
                Message::ToolCallResult {
                    call_id, output, ..
                } => Some((call_id, output)),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(call_count, 2);
        assert_eq!(results.len(), 2);
        assert!(results.iter().zip(["call-1", "call-2"]).all(
            |((call_id, output), expected_id)| {
                *call_id == expected_id
                    && serde_json::from_str::<Value>(output).unwrap()
                        == json!({"error": INTERRUPTED_BY_USER})
            }
        ));
        assert!(matches!(
            histories,
            [
                Message::User(_),
                Message::ToolCall { call_id: call_1, .. },
                Message::ToolCall { call_id: call_2, .. },
                Message::ToolCallResult { call_id: result_1, .. },
                Message::ToolCallResult { call_id: result_2, .. },
            ] if call_1 == "call-1"
                && call_2 == "call-2"
                && result_1 == call_1
                && result_2 == call_2
        ));
    }

    #[tokio::test]
    async fn cancellation_before_event_handling_interrupts_buffered_tool_calls() {
        let cancellation = cancellation();
        let mut agent = Agent::new(CancelAfterToolCallProvider {
            cancellation: cancellation.clone(),
        });

        agent
            .continue_turn("run a tool", cancellation)
            .await
            .unwrap();

        assert!(matches!(
            agent.context.histories(),
            [
                Message::User(_),
                Message::ToolCall { call_id: call, .. },
                Message::ToolCallResult {
                    call_id: result,
                    output,
                    ..
                },
            ] if call == "call-1"
                && result == call
                && serde_json::from_str::<Value>(output).unwrap()
                    == json!({"error": INTERRUPTED_BY_USER})
        ));
    }

    /// Asks for another tool round forever. Safe to run only because the round
    /// cap stops it; without one this hangs and grows without bound.
    struct RunawayProvider;

    #[async_trait::async_trait]
    impl Provider for RunawayProvider {
        fn model(&self) -> &str {
            "runaway-model"
        }

        fn thinking_effort(&self) -> Option<&str> {
            None
        }

        fn define_tools(&mut self, _specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
            Ok(())
        }

        async fn stream(&self, _input: &[Message]) -> anyhow::Result<ProviderEventStream> {
            Ok(Box::pin(stream::once(async {
                Ok(completed(CompletedReason::NeedCall))
            })))
        }
    }

    #[tokio::test]
    async fn a_turn_that_never_settles_is_cut_off() {
        let mut agent = Agent::new(RunawayProvider);
        let mut events = agent.subscribe_view();

        let error = agent
            .continue_turn("ask something", cancellation())
            .await
            .unwrap_err();

        assert!(
            error.to_string().contains("without a final answer"),
            "the reason has to reach the caller: {error}"
        );

        let seen = drain(&mut events);

        assert!(
            seen.iter().any(|event| matches!(
                event,
                AgentViewEvent::Err(message) if message.contains("without a final answer")
            )),
            "and the screen, so the user is not left guessing"
        );
        assert!(
            seen.iter()
                .any(|event| matches!(event, AgentViewEvent::TurnFinished { completed: false })),
            "a cut-off turn is not a finished one"
        );
    }

    #[tokio::test]
    async fn a_turn_that_reaches_a_final_answer_reports_it_completed() {
        let mut agent = Agent::new(ScriptedProvider::new([CompletedReason::Final]));
        let mut events = agent.subscribe_view();

        agent
            .continue_turn("ask something", cancellation())
            .await
            .unwrap();

        assert_eq!(
            turn_finished(&mut events),
            Some(true),
            "the view needs this to decide whether to summarise the wait"
        );
    }

    #[tokio::test]
    async fn a_tool_round_does_not_end_the_turn() {
        let mut agent = Agent::new(ScriptedProvider::new([
            CompletedReason::NeedCall,
            CompletedReason::Final,
        ]));
        let mut events = agent.subscribe_view();

        agent
            .continue_turn("ask something", cancellation())
            .await
            .unwrap();

        assert_eq!(
            turn_finished(&mut events),
            Some(true),
            "the final answer came on the second request, and it still counts"
        );
    }

    #[tokio::test]
    async fn every_call_precedes_every_result_from_the_same_response() {
        let inputs = Arc::new(Mutex::new(Vec::new()));
        let reasoning =
            br#"{"type":"reasoning","content":[{"type":"reasoning_text","text":"inspect"}]}"#
                .to_vec();
        let provider = SignalProvider::new(
            [
                vec![
                    ProviderSignal::Reasoning(reasoning.clone()),
                    ProviderSignal::TextDelta("I will inspect the environment.".to_owned()),
                    ProviderSignal::ToolCallStarted(ToolCall::new("call-1", "missing", json!({}))),
                    ProviderSignal::ToolCallStarted(ToolCall::new("call-2", "missing", json!({}))),
                    ProviderSignal::ToolCallStarted(ToolCall::new("call-3", "missing", json!({}))),
                    completed(CompletedReason::NeedCall),
                ],
                vec![completed(CompletedReason::Final)],
            ],
            text_bytes,
        )
        .record_inputs(inputs.clone());
        let mut agent = Agent::new(provider);

        agent
            .continue_turn("inspect the environment", cancellation())
            .await
            .unwrap();

        let inputs = inputs.lock().unwrap();
        assert_eq!(inputs.len(), 2);
        assert!(matches!(
            inputs[1].as_slice(),
            [
                Message::User(_),
                Message::Reasoning(stored_reasoning),
                Message::Assistant(text),
                Message::ToolCall { call_id: call_1, .. },
                Message::ToolCall { call_id: call_2, .. },
                Message::ToolCall { call_id: call_3, .. },
                Message::ToolCallResult { call_id: result_1, .. },
                Message::ToolCallResult { call_id: result_2, .. },
                Message::ToolCallResult { call_id: result_3, .. },
            ] if stored_reasoning == &reasoning
                && text == "I will inspect the environment."
                && call_1 == "call-1"
                && call_2 == "call-2"
                && call_3 == "call-3"
                && result_1 == call_1
                && result_2 == call_2
                && result_3 == call_3
        ));
    }

    #[tokio::test]
    async fn local_estimates_accumulate_across_every_request_in_a_turn() {
        let mut agent = Agent::new(EstimatingProvider::new([
            CompletedReason::NeedCall,
            CompletedReason::Final,
        ]));
        let mut events = agent.subscribe_view();

        agent
            .continue_turn("ask something", cancellation())
            .await
            .unwrap();

        let seen = drain(&mut events);
        let last_usage = seen.iter().rev().find_map(|event| match event {
            AgentViewEvent::TokenUsage { context, turn } => Some((*context, *turn)),
            _ => None,
        });

        assert_eq!(last_usage, Some((Some(1), Some(2))));
        assert_eq!(agent.context.token_count(), Some(1));
    }

    #[tokio::test]
    async fn request_input_is_counted_before_the_first_stream_event() {
        let (polled, dropped) = (Arc::new(Notify::new()), Arc::new(AtomicBool::new(false)));
        let mut agent = Agent::new(PendingStreamProvider {
            polled: polled.clone(),
            dropped: dropped.clone(),
        });
        let mut events = agent.subscribe_view();
        let cancellation = cancellation();
        let trigger = cancellation.clone();

        let run = tokio::spawn(async move {
            let result = agent.continue_turn("wait for output", cancellation).await;

            (agent, result)
        });

        polled.notified().await;
        let seen = drain(&mut events);

        assert!(seen.iter().any(|event| matches!(
            event,
            AgentViewEvent::TokenUsage {
                context: Some(10),
                turn: Some(10),
            }
        )));

        trigger.cancel();
        let (_, result) = run.await.unwrap();

        result.unwrap();
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn text_deltas_update_turn_and_context_estimates_before_completion() {
        let handled = Arc::new(Notify::new());
        let mut agent = Agent::new(PartialProvider { handled });
        let mut events = agent.subscribe_view();
        let cancellation = cancellation();
        let trigger = cancellation.clone();

        let run = tokio::spawn(async move {
            let result = agent.continue_turn("start answering", cancellation).await;

            (agent, result)
        });

        let completed = tokio::time::timeout(Duration::from_secs(1), async {
            let mut completed = false;

            loop {
                match events.recv().await.unwrap() {
                    AgentViewEvent::Completed => completed = true,
                    AgentViewEvent::TokenUsage {
                        context: Some(20),
                        turn: Some(24),
                    } => break completed,
                    _ => {}
                }
            }
        })
        .await
        .unwrap();

        assert!(
            !completed,
            "the stream is still waiting for its completion event"
        );

        trigger.cancel();
        let (_, result) = run.await.unwrap();

        result.unwrap();
    }

    #[tokio::test]
    async fn request_and_output_estimates_accumulate_across_small_rounds() {
        let mut agent = Agent::new(SignalProvider::new(
            [
                vec![
                    ProviderSignal::TextDelta("one".to_owned()),
                    completed(CompletedReason::NeedCall),
                ],
                vec![
                    ProviderSignal::TextDelta("two".to_owned()),
                    completed(CompletedReason::Final),
                ],
            ],
            text_bytes,
        ));
        let mut events = agent.subscribe_view();

        agent
            .continue_turn("ask something", cancellation())
            .await
            .unwrap();

        let last_usage = drain(&mut events)
            .into_iter()
            .rev()
            .find_map(|event| match event {
                AgentViewEvent::TokenUsage { context, turn } => Some((context, turn)),
                _ => None,
            });

        assert_eq!(last_usage, Some((Some(30), Some(36))));
    }

    #[tokio::test]
    async fn chunked_text_is_retokenized_as_one_response() {
        let mut agent = Agent::new(SignalProvider::new(
            [vec![
                ProviderSignal::TextDelta("hel".to_owned()),
                ProviderSignal::TextDelta("lo".to_owned()),
                completed(CompletedReason::Final),
            ]],
            boundary_tokens,
        ));
        let mut events = agent.subscribe_view();

        agent
            .continue_turn("say hello", cancellation())
            .await
            .unwrap();

        let turns = drain(&mut events)
            .into_iter()
            .filter_map(|event| match event {
                AgentViewEvent::TokenUsage {
                    turn: Some(turn), ..
                } => Some(turn),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(
            turns.contains(&12),
            "the first chunk is estimated while streaming"
        );
        assert_eq!(turns.last(), Some(&11));
        assert!(
            !turns.contains(&14),
            "chunks are not estimated independently and summed"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_that_never_reached_a_final_answer_reports_that() {
        let mut agent = Agent::new(ScriptedProvider::new([CompletedReason::NeedCall]));
        let mut events = agent.subscribe_view();

        let error = agent
            .continue_turn("ask something", cancellation())
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("provider stream ended before response.completed")
        );
        assert_eq!(turn_finished(&mut events), Some(false));
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

    #[derive(Clone, Copy)]
    enum FailurePoint {
        Open,
        Stream,
        Eof,
        Decode,
    }

    /// Fails a provider request at one point a set number of times before
    /// completing, counting every attempt so the backoff can be asserted on.
    struct FlakyProvider {
        point: FailurePoint,
        failures_left: AtomicUsize,
        attempts: AtomicUsize,
    }

    impl FlakyProvider {
        fn new(point: FailurePoint, failures: usize) -> Self {
            Self {
                point,
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
        fn model(&self) -> &str {
            "flaky-model"
        }

        fn thinking_effort(&self) -> Option<&str> {
            None
        }

        fn define_tools(&mut self, _specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
            Ok(())
        }

        async fn stream(&self, _input: &[Message]) -> anyhow::Result<ProviderEventStream> {
            self.attempts.fetch_add(1, Ordering::SeqCst);

            let should_fail = self
                .failures_left
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                    left.checked_sub(1)
                })
                .is_ok();

            if should_fail {
                return match self.point {
                    FailurePoint::Open => anyhow::bail!("connection reset by peer"),
                    FailurePoint::Stream => Ok(Box::pin(stream::once(async {
                        Err(anyhow::anyhow!("stream reset by peer"))
                    }))),
                    FailurePoint::Eof => Ok(Box::pin(stream::empty())),
                    FailurePoint::Decode => Ok(Box::pin(stream::once(async {
                        Err(anyhow::anyhow!("provider response error"))
                    }))),
                };
            }

            Ok(Box::pin(stream::once(async {
                Ok(completed(CompletedReason::Final))
            })))
        }
    }

    struct PartialRetryProvider {
        point: FailurePoint,
        attempts: AtomicUsize,
        inputs: Arc<Mutex<Vec<Vec<Message>>>>,
    }

    #[async_trait::async_trait]
    impl Provider for PartialRetryProvider {
        fn model(&self) -> &str {
            "partial-retry-model"
        }

        fn thinking_effort(&self) -> Option<&str> {
            None
        }

        fn define_tools(&mut self, _specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
            Ok(())
        }

        async fn stream(&self, input: &[Message]) -> anyhow::Result<ProviderEventStream> {
            self.inputs.lock().unwrap().push(input.to_vec());
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
            let events = if attempt == 0 {
                let mut events = vec![
                    Ok(ProviderSignal::TextDelta("partial answer".to_owned())),
                    Ok(ProviderSignal::ToolCallStarted(ToolCall::new(
                        "call-1",
                        "missing",
                        json!({}),
                    ))),
                    Ok(ProviderSignal::ToolCallStarted(ToolCall::new(
                        "call-2",
                        "missing",
                        json!({}),
                    ))),
                ];

                if matches!(self.point, FailurePoint::Stream) {
                    events.push(Err(anyhow::anyhow!("stream reset by peer")));
                }

                events
            } else {
                vec![Ok(completed(CompletedReason::Final))]
            };

            Ok(Box::pin(stream::iter(events)))
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
        let mut agent = Agent::new(FlakyProvider::new(FailurePoint::Open, 2));
        let mut events = agent.subscribe_view();
        let started = tokio::time::Instant::now();

        agent
            .continue_turn("retry opening", cancellation())
            .await
            .unwrap();
        assert_eq!(agent.provider.attempts(), 3, "two failures, then a success");
        assert_eq!(
            started.elapsed(),
            Duration::from_millis(450),
            "waited 150ms then 300ms"
        );
        assert_eq!(
            error_messages(&mut events),
            vec!["connection reset by peer", "connection reset by peer"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_stream_that_never_opens_gives_up_after_the_last_attempt() {
        let mut agent = Agent::new(FlakyProvider::new(FailurePoint::Open, usize::MAX));
        let mut events = agent.subscribe_view();
        let started = tokio::time::Instant::now();

        assert!(
            agent
                .continue_turn("keep retrying", cancellation())
                .await
                .is_err()
        );
        assert_eq!(agent.provider.attempts(), STREAM_MAX_ATTEMPTS as usize);
        assert_eq!(
            started.elapsed(),
            Duration::from_millis(1050),
            "waited 150ms, 300ms, 600ms, then stopped"
        );
        assert_eq!(
            error_messages(&mut events),
            vec!["connection reset by peer"; STREAM_MAX_ATTEMPTS as usize]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_stream_that_opens_first_try_does_not_wait() {
        let mut agent = Agent::new(FlakyProvider::new(FailurePoint::Open, 0));
        let started = tokio::time::Instant::now();

        agent
            .continue_turn("open once", cancellation())
            .await
            .unwrap();
        assert_eq!(agent.provider.attempts(), 1);
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stream_error_is_retried() {
        let mut agent = Agent::new(FlakyProvider::new(FailurePoint::Stream, 1));
        let mut events = agent.subscribe_view();
        let started = tokio::time::Instant::now();

        agent
            .continue_turn("retry an event error", cancellation())
            .await
            .unwrap();

        assert_eq!(agent.provider.attempts(), 2);
        assert_eq!(started.elapsed(), Duration::from_millis(150));
        assert_eq!(error_messages(&mut events), vec!["stream reset by peer"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_premature_eof_is_retried() {
        let mut agent = Agent::new(FlakyProvider::new(FailurePoint::Eof, 1));
        let mut events = agent.subscribe_view();
        let started = tokio::time::Instant::now();

        agent
            .continue_turn("retry an eof", cancellation())
            .await
            .unwrap();

        assert_eq!(agent.provider.attempts(), 2);
        assert_eq!(started.elapsed(), Duration::from_millis(150));
        assert_eq!(
            error_messages(&mut events),
            vec!["provider stream ended before response.completed"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_provider_decode_error_is_broadcast_and_retried() {
        let mut agent = Agent::new(FlakyProvider::new(FailurePoint::Decode, 1));
        let mut events = agent.subscribe_view();
        let started = tokio::time::Instant::now();

        agent
            .continue_turn("retry a provider event error", cancellation())
            .await
            .unwrap();

        assert_eq!(agent.provider.attempts(), 2);
        assert_eq!(started.elapsed(), Duration::from_millis(150));
        assert_eq!(error_messages(&mut events), vec!["provider response error"]);
    }

    #[tokio::test(start_paused = true)]
    async fn retries_continue_from_ordered_partial_output() {
        for point in [FailurePoint::Stream, FailurePoint::Eof] {
            let inputs = Arc::new(Mutex::new(Vec::new()));
            let mut agent = Agent::new(PartialRetryProvider {
                point,
                attempts: AtomicUsize::new(0),
                inputs: inputs.clone(),
            });

            agent
                .continue_turn("start answering", cancellation())
                .await
                .unwrap();

            let inputs = inputs.lock().unwrap();
            assert_eq!(inputs.len(), 2);
            assert!(matches!(
                inputs[1].as_slice(),
                [
                    Message::User(_),
                    Message::Assistant(text),
                    Message::ToolCall { call_id: call_1, .. },
                    Message::ToolCall { call_id: call_2, .. },
                    Message::ToolCallResult { call_id: result_1, .. },
                    Message::ToolCallResult { call_id: result_2, .. },
                ] if text == "partial answer"
                    && call_1 == "call-1"
                    && call_2 == "call-2"
                    && result_1 == call_1
                    && result_2 == call_2
            ));
        }
    }

    #[tokio::test]
    async fn a_cancelled_stream_open_never_contacts_the_provider() {
        let agent = Agent::new(FlakyProvider::new(FailurePoint::Open, 0));
        let cancellation = cancellation();

        cancellation.cancel();
        let messages = agent.context.provider_messages();
        let stream = agent.open_stream(&messages, &cancellation).await.unwrap();

        assert!(stream.is_none());
        assert_eq!(agent.provider.attempts(), 0);
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
        agent.with_internal_tools(Bridge::new().0).unwrap();
        let mut metrics = TurnMetrics::default();
        let mut batch = ResponseBatch::default();

        // An unregistered tool fails in the registry rather than running
        // anything, which is all this needs to reach both boundaries.
        agent
            .handle_signal(
                &ProviderSignal::ToolCallStarted(ToolCall::new(
                    "call-1",
                    "no_such_tool",
                    json!({}),
                )),
                &mut batch,
                &mut metrics,
                &cancellation(),
            )
            .await
            .unwrap();
        agent
            .handle_signal(
                &completed(CompletedReason::Final),
                &mut batch,
                &mut metrics,
                &cancellation(),
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
        let agent = agent_with_histories(vec![
            Message::System("workspace info".to_owned()),
            Message::User("first question".into()),
            Message::Assistant("first answer".to_owned()),
            Message::User("second question".into()),
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
        let agent = agent_with_histories(vec![
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
                summary: None,
            },
        ]);
        agent.with_internal_tools(Bridge::new().0).unwrap();
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
                summary: None,
            },
        ]);
        agent.with_internal_tools(Bridge::new().0).unwrap();
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
        agent.with_internal_tools(Bridge::new().0).unwrap();
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
        agent.with_internal_tools(Bridge::new().0).unwrap();
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
