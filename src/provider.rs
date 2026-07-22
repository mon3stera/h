use std::pin::Pin;

use futures::Stream;

use crate::{
    event::ProviderSignal,
    tool::{ToolDefinition, ToolResult},
};

pub mod openai;

pub enum TurnStart {
    UserMessage(UserMessage),
    Continue,
}

pub struct UserMessage {
    pub(crate) contents: Vec<Message>,
}

pub enum Message {
    Text(String),
}

impl From<String> for UserMessage {
    fn from(value: String) -> Self {
        UserMessage {
            contents: vec![Message::Text(value)],
        }
    }
}

impl From<&str> for UserMessage {
    fn from(value: &str) -> Self {
        value.to_string().into()
    }
}

type ProviderEventStream<C> = Pin<Box<dyn Stream<Item = anyhow::Result<C>> + Send>>;

#[async_trait::async_trait]
pub trait Provider: Send + Sync + 'static {
    type StreamEvent;

    fn define_tools(&mut self, specs: Vec<ToolDefinition>) -> anyhow::Result<()>;

    fn submit_tool_result(&mut self, result: ToolResult) -> anyhow::Result<()>;

    async fn handle(&mut self, event: Self::StreamEvent) -> anyhow::Result<ProviderSignal>;

    async fn stream(
        &self,
        start: TurnStart,
    ) -> anyhow::Result<ProviderEventStream<Self::StreamEvent>>;
}
