use std::pin::Pin;

use futures::Stream;
use serde::{Deserialize, Serialize};

use crate::{context::Message, event::ProviderSignal, tool::ToolDefinition};

pub mod openai;

pub type ProviderEventStream<C> = Pin<Box<dyn Stream<Item = anyhow::Result<C>> + Send>>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Compaction {
    state: Vec<u8>,
    /// Locally estimated tokens used by the compaction request and response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    total_tokens: Option<usize>,
}

impl Compaction {
    pub fn new(state: Vec<u8>, total_tokens: Option<usize>) -> Self {
        Self {
            state,
            total_tokens,
        }
    }

    pub fn state(&self) -> &[u8] {
        &self.state
    }

    pub fn total_tokens(&self) -> Option<usize> {
        self.total_tokens
    }
}

#[async_trait::async_trait]
pub trait Provider: Send + Sync + 'static {
    type StreamEvent;

    fn model(&self) -> &str;

    fn thinking_effort(&self) -> Option<&str>;

    fn define_tools(&mut self, specs: Vec<ToolDefinition>) -> anyhow::Result<()>;

    /// Estimates a normal request, including provider-specific framing and tools.
    fn estimate_request_tokens(&self, _input: &[Message]) -> anyhow::Result<Option<usize>> {
        Ok(None)
    }

    /// Estimates model output without counting request-only tool definitions.
    fn estimate_output_tokens(&self, _output: &[Message]) -> anyhow::Result<Option<usize>> {
        Ok(None)
    }

    /// Replaces the supplied provider-facing history with an opaque compacted
    /// window. `None` means this provider does not support compaction.
    async fn compact(&self, _input: &[Message]) -> anyhow::Result<Option<Compaction>> {
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
