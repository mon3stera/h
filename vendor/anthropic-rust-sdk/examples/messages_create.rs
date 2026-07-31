//! 基础 Messages API 示例。

use anthropic_rust_sdk::{Anthropic, MessageContent, MessageCreateParams, MessageParam, Role};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Anthropic::new()?;

    let params = MessageCreateParams::new(
        "claude-opus-4-6",
        1024,
        vec![MessageParam {
            role: Role::User,
            content: MessageContent::Text("Hello, Claude".into()),
        }],
    );

    let result = client.messages().create(params).await?;
    if let anthropic_rust_sdk::MessageCreateResult::Message(message) = result {
        let message = message.as_ref();
        for block in &message.content {
            if let Some(text) = block.text() {
                println!("{text}");
            }
        }
    }

    Ok(())
}
