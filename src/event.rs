#[derive(Debug, Clone)]
pub enum AgentEvent {
    TextDelta(String),
    ToolCallStarted { name: String, arguments: String },
    ToolCallCompleted { output: String },
    Completed,
    Unsupported,
}

pub enum ProviderSignal {
    TextDelta(String),
    ToolCallStarted { name: String, arguments: String },
    ToolCallCompleted { output: String },
    Completed,
    Unsupported,
}

impl From<ProviderSignal> for AgentEvent {
    fn from(value: ProviderSignal) -> Self {
        match value {
            ProviderSignal::TextDelta(delta) => AgentEvent::TextDelta(delta),
            ProviderSignal::ToolCallStarted { name, arguments } => {
                AgentEvent::ToolCallStarted { name, arguments }
            }
            ProviderSignal::ToolCallCompleted { output } => {
                AgentEvent::ToolCallCompleted { output }
            }
            ProviderSignal::Completed => AgentEvent::Completed,
            ProviderSignal::Unsupported => AgentEvent::Unsupported,
        }
    }
}
