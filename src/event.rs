use crate::tool::{Presentation, ToolCall, ToolCallResult};

#[derive(Debug, Clone, Copy)]
pub enum CompletedReason {
    Final,
    NeedCall,
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TextDelta(String),
    ToolCallStarted(ToolCall),
    ToolCallCompleted(ToolCallResult),
    Completed,
    Unsupported,
}

#[derive(Debug, Clone)]
pub enum AgentViewEvent {
    Startup {
        model: String,
        thinking_effort: Option<String>,
    },
    TextDelta(String),
    Tool(Presentation),
    Completed,
    Err(String),
}

#[derive(Debug, Clone)]
pub enum ProviderSignal {
    TextDelta(String),
    ToolCallStarted(ToolCall),
    ToolCallCompleted(ToolCallResult),
    Completed(CompletedReason),
    Unsupported,
}

impl From<ProviderSignal> for AgentEvent {
    fn from(value: ProviderSignal) -> Self {
        match value {
            ProviderSignal::TextDelta(delta) => AgentEvent::TextDelta(delta),
            ProviderSignal::ToolCallStarted(call) => AgentEvent::ToolCallStarted(call),
            ProviderSignal::ToolCallCompleted(result) => AgentEvent::ToolCallCompleted(result),
            ProviderSignal::Completed(_) => AgentEvent::Completed,
            ProviderSignal::Unsupported => AgentEvent::Unsupported,
        }
    }
}
