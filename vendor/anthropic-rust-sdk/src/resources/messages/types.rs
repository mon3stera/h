//! Messages API 类型，对齐上游 `src/resources/messages/messages.ts`。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 消息角色。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
}

/// 输入消息参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageParam {
    pub role: Role,
    #[serde(with = "message_content")]
    pub content: MessageContent,
}

/// 消息内容：字符串或内容块数组。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlockParam>),
}

mod message_content {
    use super::{ContentBlockParam, MessageContent};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(value: &MessageContent, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            MessageContent::Text(s) => s.serialize(serializer),
            MessageContent::Blocks(b) => b.serialize(serializer),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<MessageContent, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = serde_json::Value::deserialize(deserializer)?;
        if let Some(s) = v.as_str() {
            return Ok(MessageContent::Text(s.to_string()));
        }
        if let Ok(blocks) = serde_json::from_value::<Vec<ContentBlockParam>>(v) {
            return Ok(MessageContent::Blocks(blocks));
        }
        Err(serde::de::Error::custom("invalid message content"))
    }
}

/// 内容块参数（请求侧）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContentBlockParam {
    #[serde(rename = "type")]
    pub block_type: String,
    #[serde(flatten)]
    pub fields: Value,
}

impl ContentBlockParam {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            block_type: "text".to_string(),
            fields: serde_json::json!({ "text": text.into() }),
        }
    }
}

/// 内容块（响应侧）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    #[serde(flatten)]
    pub fields: Value,
}

impl ContentBlock {
    pub fn text(&self) -> Option<&str> {
        if self.block_type == "text" {
            self.fields.get("text").and_then(|v| v.as_str())
        } else {
            None
        }
    }
}

/// 停止原因。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    PauseTurn,
    Refusal,
}

/// Token 用量。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
}

/// 模型响应消息。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: String,
    #[serde(rename = "type")]
    pub object_type: String,
    pub role: String,
    pub content: Vec<ContentBlock>,
    pub model: String,
    pub stop_reason: Option<StopReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,
    pub usage: Usage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_details: Option<Value>,
}

/// 创建消息请求（非流式）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageCreateParams {
    pub model: String,
    pub max_tokens: u64,
    pub messages: Vec<MessageParam>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<MessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<Value>,
    /// 用户档案 ID，作为 `anthropic-user-profile-id` 请求头发送，不写入请求体。
    #[serde(default, skip_serializing)]
    pub user_profile_id: Option<String>,
    #[serde(flatten)]
    pub extra: Value,
}

impl MessageCreateParams {
    pub fn new(model: impl Into<String>, max_tokens: u64, messages: Vec<MessageParam>) -> Self {
        Self {
            model: model.into(),
            max_tokens,
            messages,
            system: None,
            metadata: None,
            stop_sequences: None,
            stream: None,
            temperature: None,
            top_p: None,
            top_k: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            user_profile_id: None,
            extra: Value::Null,
        }
    }

    pub fn stream(mut self, stream: bool) -> Self {
        self.stream = Some(stream);
        self
    }
}

/// Token 计数请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageCountTokensParams {
    pub model: String,
    pub messages: Vec<MessageParam>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<MessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    /// 用户档案 ID，作为 `anthropic-user-profile-id` 请求头发送，不写入请求体。
    #[serde(default, skip_serializing)]
    pub user_profile_id: Option<String>,
    #[serde(flatten)]
    pub extra: Value,
}

/// Token 计数响应。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageTokensCount {
    pub input_tokens: u64,
}

/// 流式事件（原始）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RawMessageStreamEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(flatten)]
    pub fields: Value,
}
