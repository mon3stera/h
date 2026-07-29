use tokio::sync::oneshot;

use crate::{
    command::Command,
    tool::{Presentation, ToolCall, ToolCallResult},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCommand {
    Prompt(String),
    Run(Command),
    Cancel,
}

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
    /// A prompt the user already submitted. The live path never needs this — the
    /// UI echoes what was typed as it is committed — but replaying an archived
    /// session has no other way to put the user's own turns back on screen.
    Prompt(String),
    TextDelta(String),
    Tool(Presentation),
    TurnStart,
    /// Both values are local estimates. `context` is the next request size,
    /// while `turn` accumulates estimated request and response tokens.
    TokenUsage {
        context: Option<usize>,
        turn: Option<usize>,
    },
    /// The previous context was archived and replaced by a fresh session.
    SessionStarted,
    /// A slash command finished, whether successfully or with an error already
    /// reported through [`Self::Err`].
    CommandFinished(Command),
    /// Tool call/result pairs were replaced by deterministic tool summaries.
    ToolResultsCompacted,
    /// Context compaction completed and replaced the previous context window.
    ContextCompacted,
    /// `completed` is true when the turn ended because the model finished
    /// speaking, rather than because it failed part way through.
    TurnFinished {
        completed: bool,
    },
    Completed,
    Err(String),
}

/// A question the agent needs the user to answer.
#[derive(Debug, Clone)]
pub struct AskQuestion {
    pub question: String,
    pub options: Vec<AskOption>,
}

#[derive(Debug, Clone)]
pub struct AskOption {
    pub label: String,
    pub description: Option<String>,
}

/// The user's reply. `Option` carries the index into [`AskQuestion::options`];
/// `FreeText` is what they typed when none of the options fit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskAnswer {
    Option { index: usize, label: String },
    FreeText(String),
}

/// Something the agent needs the UI to resolve before it can carry on.
///
/// This cannot travel over [`EventBus`](crate::bus::EventBus): broadcasting
/// requires `Clone`, and a request owns its single-use reply channel. It is
/// delivered over a dedicated point-to-point channel instead, which also gives
/// every kind of request one shared order of arrival.
#[derive(Debug)]
pub enum UiRequest {
    Ask {
        question: AskQuestion,
        reply: oneshot::Sender<AskAnswer>,
    },
}

#[derive(Debug, Clone)]
pub enum ProviderSignal {
    TextDelta(String),
    ToolCallStarted(ToolCall),
    ToolCallCompleted(ToolCallResult),
    Completed { reason: CompletedReason },
    Unsupported,
}

impl From<ProviderSignal> for AgentEvent {
    fn from(value: ProviderSignal) -> Self {
        match value {
            ProviderSignal::TextDelta(delta) => AgentEvent::TextDelta(delta),
            ProviderSignal::ToolCallStarted(call) => AgentEvent::ToolCallStarted(call),
            ProviderSignal::ToolCallCompleted(result) => AgentEvent::ToolCallCompleted(result),
            ProviderSignal::Completed { .. } => AgentEvent::Completed,
            ProviderSignal::Unsupported => AgentEvent::Unsupported,
        }
    }
}
