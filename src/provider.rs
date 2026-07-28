use std::{fmt::Debug, pin::Pin};

use futures::Stream;

use crate::{context::Message, event::ProviderSignal, tool::ToolDefinition};

pub mod openai;

pub type ProviderEventStream<C> = Pin<Box<dyn Stream<Item = anyhow::Result<C>> + Send>>;

#[async_trait::async_trait]
pub trait Provider: Send + Sync + 'static {
    type StreamEvent;

    fn model(&self) -> &str;

    fn thinking_effort(&self) -> Option<&str>;

    fn define_tools(&mut self, specs: Vec<ToolDefinition>) -> anyhow::Result<()>;

    /// Estimates the tokens occupied by the next provider request. Providers
    /// own this because message framing and tool definitions are protocol-specific.
    fn count_tokens(&self, _input: &[Message]) -> anyhow::Result<Option<usize>> {
        Ok(None)
    }

    async fn handle(&mut self, event: Self::StreamEvent) -> anyhow::Result<ProviderSignal>;

    /// Opens one in-flight provider request. The returned stream owns that
    /// request, so dropping it must release the connection and stop upstream
    /// work for providers whose protocol supports cancellation by disconnect.
    async fn stream(
        &self,
        input: &[Message],
    ) -> anyhow::Result<ProviderEventStream<Self::StreamEvent>>;
}
