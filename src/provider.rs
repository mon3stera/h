use std::{fmt::Debug, pin::Pin};

use futures::Stream;

use crate::{context::Message, event::ProviderSignal, tool::ToolDefinition};

pub mod openai;

type ProviderEventStream<C> = Pin<Box<dyn Stream<Item = anyhow::Result<C>> + Send>>;

#[async_trait::async_trait]
pub trait Provider: Send + Sync + 'static {
    type StreamEvent;

    fn model(&self) -> &str;

    fn thinking_effort(&self) -> Option<&str>;

    fn define_tools(&mut self, specs: Vec<ToolDefinition>) -> anyhow::Result<()>;

    async fn handle(&mut self, event: Self::StreamEvent) -> anyhow::Result<ProviderSignal>;

    async fn stream(
        &self,
        input: &[Message],
    ) -> anyhow::Result<ProviderEventStream<Self::StreamEvent>>;
}
