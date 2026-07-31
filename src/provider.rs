use h_core::{
    context::Message,
    provider::{
        Compaction, Provider, ProviderEventStream, anthropic::AnthropicProvider,
        openai::OpenAIProvider,
    },
    tool::ToolDefinition,
};

pub enum Client {
    OpenAI(OpenAIProvider),
    Anthropic(AnthropicProvider),
}

#[async_trait::async_trait]
impl Provider for Client {
    fn model(&self) -> &str {
        match self {
            Self::OpenAI(provider) => provider.model(),
            Self::Anthropic(provider) => provider.model(),
        }
    }

    fn thinking_effort(&self) -> Option<&str> {
        match self {
            Self::OpenAI(provider) => provider.thinking_effort(),
            Self::Anthropic(provider) => provider.thinking_effort(),
        }
    }

    fn define_tools(&mut self, specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
        match self {
            Self::OpenAI(provider) => provider.define_tools(specs),
            Self::Anthropic(provider) => provider.define_tools(specs),
        }
    }

    fn estimate_request_tokens(&self, input: &[Message]) -> anyhow::Result<Option<usize>> {
        match self {
            Self::OpenAI(provider) => provider.estimate_request_tokens(input),
            Self::Anthropic(provider) => provider.estimate_request_tokens(input),
        }
    }

    fn estimate_output_tokens(&self, output: &[Message]) -> anyhow::Result<Option<usize>> {
        match self {
            Self::OpenAI(provider) => provider.estimate_output_tokens(output),
            Self::Anthropic(provider) => provider.estimate_output_tokens(output),
        }
    }

    async fn compact(&self, input: &[Message]) -> anyhow::Result<Option<Compaction>> {
        match self {
            Self::OpenAI(provider) => provider.compact(input).await,
            Self::Anthropic(provider) => provider.compact(input).await,
        }
    }

    async fn stream(&self, input: &[Message]) -> anyhow::Result<ProviderEventStream> {
        match self {
            Self::OpenAI(provider) => provider.stream(input).await,
            Self::Anthropic(provider) => provider.stream(input).await,
        }
    }
}
