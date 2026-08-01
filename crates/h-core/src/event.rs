use serde::Serialize;

use crate::{
    command::Command,
    context::{Search, SearchView},
    input::UserInput,
    tool::{Presentation, ToolCall, ToolCallResult},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCommand {
    Prompt(UserInput),
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
    Reasoning,
    Search(Search),
    ToolCallStarted(ToolCall),
    ToolCallCompleted(ToolCallResult),
    Completed,
    Unsupported,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AgentViewEvent {
    Startup {
        model: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        thinking_effort: Option<String>,
    },
    /// A prompt the user already submitted. The live path never needs this — the
    /// UI echoes what was typed as it is committed — but replaying an archived
    /// session has no other way to put the user's own turns back on screen.
    Prompt(String),
    TextDelta(String),
    Search(SearchView),
    Tool(Presentation),
    TurnStart,
    /// Both values are local estimates. `context` is the next request size,
    /// while `turn` accumulates estimated request and response tokens.
    TokenUsage {
        #[serde(skip_serializing_if = "Option::is_none")]
        context: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        turn: Option<usize>,
    },
    /// The previous context was archived and replaced by a fresh session.
    SessionStarted,
    /// A slash command finished, whether successfully or with an error already
    /// reported through [`Self::Err`].
    CommandFinished(Command),
    /// Context compaction completed and replaced the previous context window.
    ContextCompacted,
    /// `completed` is true when the turn ended because the model finished
    /// speaking, rather than because it failed part way through.
    TurnFinished {
        completed: bool,
    },
    Completed,
    #[serde(rename = "error")]
    Err(String),
}

#[derive(Debug, Clone)]
pub enum ProviderSignal {
    TextDelta(String),
    Reasoning(Vec<u8>),
    Search(Search),
    ToolCallStarted(ToolCall),
    ToolCallCompleted(ToolCallResult),
    Completed { reason: CompletedReason },
    Unsupported,
}

impl From<ProviderSignal> for AgentEvent {
    fn from(value: ProviderSignal) -> Self {
        match value {
            ProviderSignal::TextDelta(delta) => AgentEvent::TextDelta(delta),
            ProviderSignal::Reasoning(_) => AgentEvent::Reasoning,
            ProviderSignal::Search(search) => AgentEvent::Search(search),
            ProviderSignal::ToolCallStarted(call) => AgentEvent::ToolCallStarted(call),
            ProviderSignal::ToolCallCompleted(result) => AgentEvent::ToolCallCompleted(result),
            ProviderSignal::Completed { .. } => AgentEvent::Completed,
            ProviderSignal::Unsupported => AgentEvent::Unsupported,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        context::{SearchAction, SearchStatus, SearchView},
        tool::{DisplayBlock, Presentation, ToolCallId, ToolCallStatus},
    };

    fn to_json(event: &AgentViewEvent) -> serde_json::Value {
        serde_json::to_value(event).expect("view events must serialize")
    }

    #[test]
    fn view_events_serialize_with_a_type_tag() {
        assert_eq!(
            to_json(&AgentViewEvent::TextDelta("hi".to_owned())),
            json!({"type": "text_delta", "data": "hi"})
        );
        assert_eq!(
            to_json(&AgentViewEvent::TurnFinished { completed: true }),
            json!({"type": "turn_finished", "data": {"completed": true}})
        );
        assert_eq!(
            to_json(&AgentViewEvent::TokenUsage {
                context: Some(12),
                turn: None,
            }),
            json!({"type": "token_usage", "data": {"context": 12}})
        );
        assert_eq!(
            to_json(&AgentViewEvent::CommandFinished(Command::Compact)),
            json!({"type": "command_finished", "data": "/compact"})
        );
        assert_eq!(
            to_json(&AgentViewEvent::Err("oops".to_owned())),
            json!({"type": "error", "data": "oops"})
        );
    }

    #[test]
    fn search_events_carry_only_the_renderable_projection() {
        let search = SearchView::new(
            "ws-1",
            SearchStatus::Succeeded,
            Some(SearchAction::Query {
                query: "Rust async runtimes".to_owned(),
                sources: Vec::new(),
            }),
        );

        assert_eq!(
            to_json(&AgentViewEvent::Search(search)),
            json!({
                "type": "search",
                "data": {
                    "id": "ws-1",
                    "status": "Succeeded",
                    "action": { "Query": { "query": "Rust async runtimes" } },
                }
            })
        );
    }

    #[test]
    fn tool_events_carry_the_presentation_tree() {
        let presentation = Presentation {
            call_id: ToolCallId("call-1".to_owned()),
            name: "bash".to_owned(),
            label: "Run bash".to_owned(),
            target: Some("echo hi".to_owned()),
            status: ToolCallStatus::Failed {
                message: "exit 1".to_owned(),
            },
            blocks: vec![DisplayBlock::Summary("done".to_owned())],
        };

        assert_eq!(
            to_json(&AgentViewEvent::Tool(presentation)),
            json!({
                "type": "tool",
                "data": {
                    "call_id": "call-1",
                    "name": "bash",
                    "label": "Run bash",
                    "target": "echo hi",
                    "status": { "type": "failed", "data": { "message": "exit 1" } },
                    "blocks": [{ "type": "summary", "data": "done" }],
                }
            })
        );
    }
}
