use async_openai::{
    traits::EventType,
    types::responses::{OutputItem, ResponseStreamEvent},
};

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TextDelta(String),
    ToolCallStarted { name: String, arguments: String },
    ToolCallCompleted { id: String, output: String },
    Completed,
    Unsupported,
}

pub enum ProviderSignal {
    TextDelta(String),
    ToolCallStarted { name: String, arguments: String },
    ToolCallCompleted { output: String },
}
