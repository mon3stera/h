//! 结构化输出解析，对齐上游 `src/lib/parser.ts`。

use crate::core::error::{AnthropicError, Error};
use crate::resources::messages::{ContentBlock, Message, ParsedMessage};

/// 从消息内容块中提取 JSON 并解析为 `parsed_output`。
pub fn parse_message(message: &Message) -> Result<ParsedMessage, Error> {
    let mut parsed_output = None;

    for block in &message.content {
        if let Some(json) = extract_json_from_block(block) {
            parsed_output = Some(json);
            break;
        }
    }

    Ok(ParsedMessage {
        message: message.clone(),
        parsed_output,
    })
}

fn extract_json_from_block(block: &ContentBlock) -> Option<serde_json::Value> {
    if block.block_type != "text" {
        return None;
    }
    let text = block.fields.get("text")?.as_str()?;
    serde_json::from_str(text).ok()
}

/// 从 output_config 声明的 JSON Schema 名称验证解析结果（占位，供 helpers 扩展）。
pub fn validate_parsed_output(
    value: &serde_json::Value,
    _schema: &serde_json::Value,
) -> Result<(), Error> {
    if value.is_null() {
        return Err(Error::Anthropic(AnthropicError(
            "parsed output is null".into(),
        )));
    }
    Ok(())
}
