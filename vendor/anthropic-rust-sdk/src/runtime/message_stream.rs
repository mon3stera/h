//! 高级消息流，对齐上游 `src/lib/MessageStream.ts`。

use crate::client::Anthropic;
use crate::core::error::Error;
use crate::core::streaming::EventStream;
use crate::resources::messages::{
    ContentBlock, Message, MessageCreateParams, RawMessageStreamEvent, Usage,
};
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// 累积中的消息快照。
#[derive(Debug, Clone, Default)]
pub struct MessageSnapshot {
    /// 逐步重建的消息（随事件累积内容块）。
    pub message: Option<Message>,
    /// 所有 `text_delta` 拼接而成的纯文本。
    pub text: String,
}

/// 流式累积器：消息快照 + 各内容块的 partial JSON 缓冲。
#[derive(Debug, Clone, Default)]
pub(crate) struct Accumulator {
    pub snapshot: MessageSnapshot,
    json_buffers: HashMap<usize, String>,
}

/// 高级消息流封装。
pub struct MessageStream<'a> {
    client: &'a Anthropic,
    params: MessageCreateParams,
    acc: Arc<Mutex<Accumulator>>,
}

impl<'a> MessageStream<'a> {
    pub fn new(client: &'a Anthropic, mut params: MessageCreateParams) -> Self {
        params.stream = Some(true);
        Self {
            client,
            params,
            acc: Arc::new(Mutex::new(Accumulator::default())),
        }
    }

    /// 返回底层事件流。
    pub async fn events(&self) -> Result<EventStream<RawMessageStreamEvent>, Error> {
        let headers =
            crate::resources::messages::user_profile_headers(&self.params.user_profile_id);
        let response = self
            .client
            .post_streaming_with_headers("/v1/messages", &self.params, headers.as_ref())
            .await?;
        let byte_stream = response.bytes_stream().boxed();
        Ok(EventStream::new(byte_stream))
    }

    /// 迭代流式事件并累积快照，每个事件回调一次。
    pub async fn run<F>(&self, mut on_event: F) -> Result<Message, Error>
    where
        F: FnMut(RawMessageStreamEvent, MessageSnapshot),
    {
        let mut events = self.events().await?;

        while let Some(event) = events.next().await {
            let event = event?;
            let snapshot = {
                let mut acc = self.acc.lock().await;
                accumulate(&mut acc, &event);
                acc.snapshot.clone()
            };
            on_event(event, snapshot);
        }

        let message = self.acc.lock().await.snapshot.message.clone();
        message.ok_or_else(|| {
            Error::Anthropic(crate::core::error::AnthropicError(
                "stream ended without a final message".into(),
            ))
        })
    }

    /// 等待流结束并返回累积完成的最终消息。
    pub async fn final_message(&self) -> Result<Message, Error> {
        self.run(|_, _| {}).await
    }
}

/// 将单个流式事件累积进 `Accumulator`，对齐上游 MessageStream 的累积语义。
///
/// 在 `content_block_stop` 时，将 `input_json_delta` 累积的 partial JSON 解析为
/// 工具调用块的 `input`（对齐上游 `internal/message-stream-utils.ts`）。
pub(crate) fn accumulate(acc: &mut Accumulator, event: &RawMessageStreamEvent) {
    match event.event_type.as_str() {
        "message_start" => {
            if let Some(message) = event.fields.get("message") {
                if let Ok(msg) = serde_json::from_value::<Message>(message.clone()) {
                    acc.snapshot.message = Some(msg);
                }
            }
        }
        "content_block_start" => {
            let index = event_index(event);
            if let Some(content_block) = event.fields.get("content_block") {
                if let Ok(block) = serde_json::from_value::<ContentBlock>(content_block.clone()) {
                    let needs_json = matches!(
                        block.block_type.as_str(),
                        "tool_use" | "server_tool_use" | "mcp_tool_use"
                    );
                    if let Some(message) = acc.snapshot.message.as_mut() {
                        while message.content.len() <= index {
                            message.content.push(ContentBlock {
                                block_type: "text".to_string(),
                                fields: serde_json::json!({ "text": "" }),
                            });
                        }
                        message.content[index] = block;
                    }
                    if needs_json {
                        acc.json_buffers.insert(index, String::new());
                    }
                }
            }
        }
        "content_block_delta" => {
            let index = event_index(event);
            if let Some(delta) = event.fields.get("delta") {
                let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match delta_type {
                    "text_delta" => {
                        if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                            acc.snapshot.text.push_str(text);
                            append_str_field(acc, index, "text", text);
                        }
                    }
                    "input_json_delta" => {
                        if let Some(partial) = delta.get("partial_json").and_then(|v| v.as_str()) {
                            acc.json_buffers.entry(index).or_default().push_str(partial);
                        }
                    }
                    "thinking_delta" => {
                        if let Some(thinking) = delta.get("thinking").and_then(|v| v.as_str()) {
                            append_str_field(acc, index, "thinking", thinking);
                        }
                    }
                    "signature_delta" => {
                        if let Some(signature) = delta.get("signature").and_then(|v| v.as_str()) {
                            set_str_field(acc, index, "signature", signature);
                        }
                    }
                    _ => {}
                }
            }
        }
        "content_block_stop" => {
            let index = event_index(event);
            if let Some(buffer) = acc.json_buffers.get(&index).cloned() {
                let parsed = if buffer.trim().is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str::<Value>(&buffer).unwrap_or_else(|_| serde_json::json!({}))
                };
                if let Some(message) = acc.snapshot.message.as_mut() {
                    if let Some(block) = message.content.get_mut(index) {
                        if let Some(obj) = block.fields.as_object_mut() {
                            obj.insert("input".to_string(), parsed);
                        }
                    }
                }
            }
        }
        "message_delta" => {
            if let Some(message) = acc.snapshot.message.as_mut() {
                if let Some(delta) = event.fields.get("delta") {
                    if let Some(stop_reason) = delta.get("stop_reason") {
                        message.stop_reason = serde_json::from_value(stop_reason.clone()).ok();
                    }
                    if let Some(stop_sequence) = delta.get("stop_sequence").and_then(|v| v.as_str())
                    {
                        message.stop_sequence = Some(stop_sequence.to_string());
                    }
                }
                if let Some(usage) = event.fields.get("usage") {
                    merge_usage(&mut message.usage, usage);
                }
            }
        }
        _ => {}
    }
}

fn event_index(event: &RawMessageStreamEvent) -> usize {
    event
        .fields
        .get("index")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize
}

fn append_str_field(acc: &mut Accumulator, index: usize, key: &str, addition: &str) {
    if let Some(message) = acc.snapshot.message.as_mut() {
        if let Some(block) = message.content.get_mut(index) {
            if let Some(obj) = block.fields.as_object_mut() {
                let current = obj.get(key).and_then(|v| v.as_str()).unwrap_or("");
                let combined = format!("{current}{addition}");
                obj.insert(key.to_string(), Value::String(combined));
            }
        }
    }
}

fn set_str_field(acc: &mut Accumulator, index: usize, key: &str, value: &str) {
    if let Some(message) = acc.snapshot.message.as_mut() {
        if let Some(block) = message.content.get_mut(index) {
            if let Some(obj) = block.fields.as_object_mut() {
                obj.insert(key.to_string(), Value::String(value.to_string()));
            }
        }
    }
}

fn merge_usage(usage: &mut Usage, source: &Value) {
    if let Some(v) = source.get("input_tokens").and_then(|v| v.as_u64()) {
        usage.input_tokens = v;
    }
    if let Some(v) = source.get("output_tokens").and_then(|v| v.as_u64()) {
        usage.output_tokens = v;
    }
    if let Some(v) = source
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_u64())
    {
        usage.cache_creation_input_tokens = Some(v);
    }
    if let Some(v) = source
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_u64())
    {
        usage.cache_read_input_tokens = Some(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(json: serde_json::Value) -> RawMessageStreamEvent {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn accumulates_text_and_tool_input() {
        let mut acc = Accumulator::default();

        accumulate(
            &mut acc,
            &event(serde_json::json!({
                "type": "message_start",
                "message": {
                    "id": "msg_1",
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": "claude-opus-4-6",
                    "stop_reason": null,
                    "usage": {"input_tokens": 5, "output_tokens": 0}
                }
            })),
        );

        accumulate(
            &mut acc,
            &event(serde_json::json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text", "text": ""}
            })),
        );
        accumulate(
            &mut acc,
            &event(serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": "Hello "}
            })),
        );
        accumulate(
            &mut acc,
            &event(serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": "world"}
            })),
        );
        accumulate(
            &mut acc,
            &event(serde_json::json!({"type": "content_block_stop", "index": 0})),
        );

        accumulate(
            &mut acc,
            &event(serde_json::json!({
                "type": "content_block_start",
                "index": 1,
                "content_block": {"type": "tool_use", "id": "tu_1", "name": "get_weather", "input": {}}
            })),
        );
        accumulate(
            &mut acc,
            &event(serde_json::json!({
                "type": "content_block_delta",
                "index": 1,
                "delta": {"type": "input_json_delta", "partial_json": "{\"city\":"}
            })),
        );
        accumulate(
            &mut acc,
            &event(serde_json::json!({
                "type": "content_block_delta",
                "index": 1,
                "delta": {"type": "input_json_delta", "partial_json": " \"Paris\"}"}
            })),
        );
        accumulate(
            &mut acc,
            &event(serde_json::json!({"type": "content_block_stop", "index": 1})),
        );

        accumulate(
            &mut acc,
            &event(serde_json::json!({
                "type": "message_delta",
                "delta": {"type": "message_delta", "stop_reason": "tool_use"},
                "usage": {"output_tokens": 12}
            })),
        );

        let message = acc.snapshot.message.expect("final message");
        assert_eq!(acc.snapshot.text, "Hello world");
        assert_eq!(message.content[0].text(), Some("Hello world"));
        assert_eq!(message.content[1].block_type, "tool_use");
        assert_eq!(message.content[1].fields["input"]["city"], "Paris");
        assert_eq!(
            message.stop_reason,
            Some(crate::resources::messages::StopReason::ToolUse)
        );
        assert_eq!(message.usage.input_tokens, 5);
        assert_eq!(message.usage.output_tokens, 12);
    }
}
