use std::pin::Pin;

use futures::Stream;

use crate::event::ProviderSignal;

pub mod openai;

type ProviderEventStream<C> = Pin<Box<dyn Stream<Item = anyhow::Result<C>> + Send>>;

#[async_trait::async_trait]
pub trait Provider: Send + Sync + 'static {
    type StreamEvent;

    async fn handle(&mut self, event: Self::StreamEvent) -> anyhow::Result<ProviderSignal>;

    async fn stream(
        &self,
        prompt: impl AsRef<str> + Send,
    ) -> anyhow::Result<ProviderEventStream<Self::StreamEvent>>;
}
