use std::time::Instant;

use crate::{
    bus::EventBus,
    context::{Context, Message},
    event::{AgentEvent, AgentViewEvent, CompletedReason, ProviderSignal},
    provider::Provider,
    tool::{FetchTool, FileBufferStore, ReadFileTool, ToolRegistry, WriteFileTool},
};
use futures::StreamExt;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::Instrument;
use uuid::Uuid;

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
    context: Context<Message>,
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

    pub fn with_internal_tools(&mut self) -> anyhow::Result<&mut Self> {
        let file_buffers = FileBufferStore::default();

        self.tool
            .register_with_presenter(
                ReadFileTool::new(file_buffers.clone()),
                crate::tool::ReadFilePresenter,
            )
            .register_with_presenter(
                WriteFileTool::new(file_buffers),
                crate::tool::WriteFilePresenter,
            )
            .register_with_presenter(FetchTool::new()?, crate::tool::FetchPresenter);
        Ok(self)
    }

    pub async fn with_global_prompts(&mut self) -> anyhow::Result<&mut Self> {
        self.context.inject_global_prompts().await?;
        Ok(self)
    }

    pub fn initialize(&mut self) -> anyhow::Result<()> {
        let definitions = self.tool.definitions()?;

        self.provider.define_tools(definitions)?;

        Ok(())
    }

    fn append_prompt(&mut self, prompt: impl AsRef<str>) {
        self.context
            .histories_mut()
            .push(Message::User(prompt.as_ref().to_string()));
    }

    fn merge_text_delta(&mut self) {
        self.context
            .finalize_buf(Box::new(|buf| Message::Assistant(buf)));
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

        async {
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
        .await
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

        let mut stream = self.provider.stream(self.context.histories()).await?;

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
