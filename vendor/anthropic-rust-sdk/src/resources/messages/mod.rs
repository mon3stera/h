mod batches;
pub mod types;

use crate::client::Anthropic;
use crate::core::error::Error;
use crate::core::streaming::EventStream;
use crate::runtime::message_stream::MessageStream;
use futures::StreamExt;
use std::collections::HashMap;

pub use batches::*;
pub use types::*;

/// 将 `user_profile_id` 转换为 `anthropic-user-profile-id` 请求头映射。
///
/// 返回 `None` 表示无需附加该请求头。
pub(crate) fn user_profile_headers(
    user_profile_id: &Option<String>,
) -> Option<HashMap<String, String>> {
    user_profile_id.as_ref().map(|id| {
        let mut headers = HashMap::new();
        headers.insert("anthropic-user-profile-id".to_string(), id.clone());
        headers
    })
}

/// Messages API 资源。
pub struct Messages<'a> {
    client: &'a Anthropic,
}

impl<'a> Messages<'a> {
    pub(crate) fn new(client: &'a Anthropic) -> Self {
        Self { client }
    }

    /// 批处理子资源。
    pub fn batches(&self) -> Batches<'a> {
        Batches::new(self.client)
    }

    /// 创建消息（非流式或流式）。
    pub async fn create(&self, params: MessageCreateParams) -> Result<MessageCreateResult, Error> {
        let headers = user_profile_headers(&params.user_profile_id);
        let stream = params.stream.unwrap_or(false);
        if stream {
            let response = self
                .client
                .post_streaming_with_headers("/v1/messages", &params, headers.as_ref())
                .await?;
            let byte_stream = response.bytes_stream().boxed();
            let event_stream = EventStream::<RawMessageStreamEvent>::new(byte_stream);
            return Ok(MessageCreateResult::Stream(event_stream));
        }

        let mut non_streaming = params;
        non_streaming.stream = Some(false);
        let message: Message = self
            .client
            .post_with_headers("/v1/messages", &non_streaming, headers.as_ref())
            .await?;
        Ok(MessageCreateResult::Message(Box::new(message)))
    }

    /// 统计 Token 数量。
    pub async fn count_tokens(
        &self,
        params: MessageCountTokensParams,
    ) -> Result<MessageTokensCount, Error> {
        let headers = user_profile_headers(&params.user_profile_id);
        self.client
            .post_with_headers("/v1/messages/count_tokens", &params, headers.as_ref())
            .await
    }

    /// 创建高级消息流。
    pub fn stream(&self, params: MessageCreateParams) -> MessageStream<'a> {
        MessageStream::new(self.client, params)
    }

    /// 结构化输出解析（对齐 `messages.parse()`）。
    pub async fn parse(&self, params: MessageCreateParams) -> Result<ParsedMessage, Error> {
        let mut non_streaming = params;
        non_streaming.stream = Some(false);
        let message = match self.create(non_streaming).await? {
            MessageCreateResult::Message(m) => *m,
            MessageCreateResult::Stream(_) => {
                return Err(crate::core::error::AnthropicError(
                    "parse() requires non-streaming request".into(),
                )
                .into());
            }
        };
        crate::runtime::parser::parse_message(&message)
    }
}

/// 创建消息结果。
pub enum MessageCreateResult {
    Message(Box<Message>),
    Stream(EventStream<RawMessageStreamEvent>),
}

/// 带解析输出的消息。
#[derive(Debug, Clone)]
pub struct ParsedMessage {
    pub message: Message,
    pub parsed_output: Option<serde_json::Value>,
}
