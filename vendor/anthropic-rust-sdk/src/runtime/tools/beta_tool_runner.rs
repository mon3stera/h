//! Beta 工具循环骨架，对齐上游 `src/lib/tools/BetaToolRunner.ts`。

use crate::client::Anthropic;
use crate::core::error::Error;
use crate::resources::messages::{Message, MessageCreateParams, MessageParam, Role};
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

/// 可运行工具处理器。
pub type ToolHandler =
    Box<dyn Fn(Value) -> Pin<Box<dyn Future<Output = Result<Value, Error>> + Send>> + Send + Sync>;

/// Beta 工具循环执行器。
pub struct BetaToolRunner<'a> {
    client: &'a Anthropic,
    params: MessageCreateParams,
    tools: HashMap<String, ToolHandler>,
    max_turns: u32,
}

impl<'a> BetaToolRunner<'a> {
    pub fn new(client: &'a Anthropic, params: MessageCreateParams) -> Self {
        Self {
            client,
            params,
            tools: HashMap::new(),
            max_turns: 10,
        }
    }

    pub fn register_tool(mut self, name: impl Into<String>, handler: ToolHandler) -> Self {
        self.tools.insert(name.into(), handler);
        self
    }

    pub fn max_turns(mut self, max_turns: u32) -> Self {
        self.max_turns = max_turns;
        self
    }

    /// 运行工具循环直到模型不再请求工具或达到最大轮次。
    pub async fn run(&mut self) -> Result<Message, Error> {
        let mut turns = 0u32;
        loop {
            turns += 1;
            if turns > self.max_turns {
                return Err(crate::core::error::AnthropicError(
                    "max tool runner turns exceeded".into(),
                )
                .into());
            }

            let message = match self.client.messages().create(self.params.clone()).await? {
                crate::resources::messages::MessageCreateResult::Message(m) => *m,
                crate::resources::messages::MessageCreateResult::Stream(_) => {
                    return Err(crate::core::error::AnthropicError(
                        "tool runner requires non-streaming".into(),
                    )
                    .into());
                }
            };

            if message.stop_reason.as_ref()
                != Some(&crate::resources::messages::StopReason::ToolUse)
            {
                return Ok(message);
            }

            let tool_uses = extract_tool_uses(&message);
            if tool_uses.is_empty() {
                return Ok(message);
            }

            self.params.messages.push(MessageParam {
                role: Role::Assistant,
                content: crate::resources::messages::MessageContent::Blocks(
                    message
                        .content
                        .iter()
                        .map(|b| crate::resources::messages::ContentBlockParam {
                            block_type: b.block_type.clone(),
                            fields: b.fields.clone(),
                        })
                        .collect(),
                ),
            });

            let mut tool_results = Vec::new();
            for tool_use in tool_uses {
                let name = tool_use
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let id = tool_use
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let input = tool_use.get("input").cloned().unwrap_or(Value::Null);

                let output = if let Some(handler) = self.tools.get(name) {
                    handler(input).await?
                } else {
                    Value::String(format!("unknown tool: {name}"))
                };

                tool_results.push(serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": output,
                }));
            }

            self.params.messages.push(MessageParam {
                role: Role::User,
                content: crate::resources::messages::MessageContent::Blocks(
                    tool_results
                        .into_iter()
                        .map(|v| crate::resources::messages::ContentBlockParam {
                            block_type: v
                                .get("type")
                                .and_then(|t| t.as_str())
                                .unwrap_or("tool_result")
                                .to_string(),
                            fields: v,
                        })
                        .collect(),
                ),
            });
        }
    }
}

fn extract_tool_uses(message: &Message) -> Vec<Value> {
    message
        .content
        .iter()
        .filter(|b| b.block_type == "tool_use")
        .map(|b| b.fields.clone())
        .collect()
}
