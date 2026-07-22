use std::collections::BTreeMap;

use async_openai::{
    Client,
    config::OpenAIConfig,
    types::responses::{CreateResponseArgs, EasyInputMessage, InputItem, ResponseStreamEvent},
};
use futures::{StreamExt, TryStreamExt};
use parking_lot::Mutex;

use crate::{
    context::Context,
    event::ProviderSignal,
    provider::{Provider, ProviderEventStream},
};

macro_rules! expect_env {
    ($value:expr) => {
        std::env::var($value)?
    };
}

pub struct OpenAIProviderConfig {
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenAIProviderConfig {
    pub fn new() -> Self {
        Self {
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
        }
    }

    pub fn with_base_url(self, base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            ..self
        }
    }

    pub fn with_api_key(self, api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            ..self
        }
    }

    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            base_url: expect_env!("OPENAI_BASE_URL"),
            api_key: expect_env!("OPENAI_API_KEY"),
            model: expect_env!("OPENAI_MODEL"),
        })
    }
}

struct PendingItem {
    item: InputItem,
}

pub struct OpenAIProvider {
    config: OpenAIProviderConfig,
    client: Client<OpenAIConfig>,
    context: Context<InputItem>,
    prompt: Mutex<String>,
    pending: BTreeMap<u32, PendingItem>,
}

impl OpenAIProvider {
    pub fn from_config(config: OpenAIProviderConfig) -> Self {
        let client_config = OpenAIConfig::new()
            .with_api_base(&config.base_url)
            .with_api_key(&config.api_key);

        let client = Client::with_config(client_config);

        Self {
            config,
            client,
            context: Context::new(),
            prompt: Mutex::new(String::new()),
            pending: BTreeMap::new(),
        }
    }

    fn take_pending(&mut self) -> (String, BTreeMap<u32, PendingItem>) {
        let mut prompt = String::new();
        let mut pending = BTreeMap::new();

        std::mem::swap(&mut prompt, &mut self.prompt.lock());
        std::mem::swap(&mut pending, &mut self.pending);

        (prompt, pending)
    }

    fn build_input(&self, prompt: impl AsRef<str>) -> Vec<InputItem> {
        let prompt = prompt.as_ref();

        let mut inputs = Vec::<InputItem>::new();

        inputs.push(EasyInputMessage::from(prompt).into());
        inputs.extend(self.context.histories().iter().cloned());

        inputs
    }

    fn record_history(&mut self, event: &ResponseStreamEvent) -> anyhow::Result<()> {
        match event {
            ResponseStreamEvent::ResponseOutputItemDone(item) => {
                self.pending.insert(
                    item.output_index,
                    PendingItem {
                        item: item.item.clone().into(),
                    },
                );
            }
            ResponseStreamEvent::ResponseCompleted(_) => {
                let (prompt, pending) = self.take_pending();

                let items = pending.into_values().map(|e| e.item);

                let histories = self.context.histories_mut();

                if !prompt.is_empty() {
                    histories.push(EasyInputMessage::from(prompt).into());
                }

                histories.extend(items);
            }
            _ => {}
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl Provider for OpenAIProvider {
    type StreamEvent = async_openai::types::responses::ResponseStreamEvent;

    async fn handle(&mut self, event: Self::StreamEvent) -> anyhow::Result<ProviderSignal> {
        self.record_history(&event)?;

        match &event {
            ResponseStreamEvent::ResponseOutputTextDelta(delta) => {
                return Ok(ProviderSignal::TextDelta(delta.delta.clone()));
            }
            ResponseStreamEvent::ResponseCompleted(_) => Ok(ProviderSignal::Completed),
            _ => Ok(ProviderSignal::Unsupported),
        }
    }

    async fn stream(
        &self,
        prompt: impl AsRef<str> + Send,
    ) -> anyhow::Result<ProviderEventStream<Self::StreamEvent>> {
        let prompt = prompt.as_ref().to_string();

        let request = CreateResponseArgs::default()
            .model(&self.config.model)
            .input(self.build_input(&prompt))
            .stream(true)
            .build()?;

        let stream = self
            .client
            .responses()
            .create_stream(request)
            .await?
            .map_err(anyhow::Error::from)
            .boxed();

        *self.prompt.lock() = prompt;

        Ok(stream)
    }
}
