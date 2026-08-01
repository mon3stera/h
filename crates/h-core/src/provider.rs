use std::pin::Pin;

use futures::Stream;
use serde::{Deserialize, Serialize};

use crate::{context::Message, event::ProviderSignal, tool::ToolDefinition};

pub mod anthropic;
pub mod openai;
mod schema;

pub type ProviderEventStream = Pin<Box<dyn Stream<Item = anyhow::Result<ProviderSignal>> + Send>>;

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

/// The wire protocol a provider speaks. Mirrors the `type` tag of a profile
/// config entry, so archived sessions can be matched to the profile that
/// recorded them.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    OpenAI,
    Anthropic,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::OpenAI => "openai",
            Self::Anthropic => "anthropic",
        })
    }
}

/// What identifies the upstream a session belongs to: the request format
/// (`protocol`) and the provider (`base_url`). A resumed session must be
/// replayed under the same upstream, so its archive carries the identity it
/// was recorded under and resume rejects a mismatch.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Identity {
    pub protocol: Protocol,
    pub base_url: String,
}

impl Identity {
    /// Whether two identities address the same upstream. The protocol must
    /// match exactly; base_urls compare with a trailing slash ignored, since
    /// most servers treat it as optional.
    pub fn compatible_with(&self, other: &Identity) -> bool {
        self.protocol == other.protocol
            && self.base_url.trim_end_matches('/') == other.base_url.trim_end_matches('/')
    }
}

#[async_trait::async_trait]
pub trait Provider: Send + Sync + 'static {
    fn model(&self) -> &str;

    fn thinking_effort(&self) -> Option<&str>;

    /// The upstream this provider addresses, when it can say. `None` is for
    /// providers that cannot identify themselves (test doubles); resume only
    /// enforces the constraint when both sides report an identity.
    fn identity(&self) -> Option<Identity> {
        None
    }

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

    /// Opens one in-flight provider request. The returned stream owns that
    /// request, so dropping it must release the connection and stop upstream
    /// work for providers whose protocol supports cancellation by disconnect.
    async fn stream(&self, input: &[Message]) -> anyhow::Result<ProviderEventStream>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(protocol: Protocol, base_url: &str) -> Identity {
        Identity {
            protocol,
            base_url: base_url.to_owned(),
        }
    }

    #[test]
    fn compatible_ignores_a_trailing_slash_on_base_url() {
        assert!(
            identity(Protocol::Anthropic, "https://api.deepseek.com/anthropic/").compatible_with(
                &identity(Protocol::Anthropic, "https://api.deepseek.com/anthropic")
            )
        );
        assert!(
            identity(Protocol::OpenAI, "https://api.openai.com/v1")
                .compatible_with(&identity(Protocol::OpenAI, "https://api.openai.com/v1/"))
        );
    }

    #[test]
    fn incompatible_when_the_protocol_differs() {
        assert!(
            !identity(Protocol::Anthropic, "https://example.com")
                .compatible_with(&identity(Protocol::OpenAI, "https://example.com"))
        );
    }

    #[test]
    fn incompatible_when_the_base_url_differs() {
        assert!(
            !identity(Protocol::Anthropic, "https://api.anthropic.com").compatible_with(&identity(
                Protocol::Anthropic,
                "https://api.deepseek.com/anthropic"
            ))
        );
    }
}
