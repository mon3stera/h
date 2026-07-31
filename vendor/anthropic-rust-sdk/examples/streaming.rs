//! 流式 Messages API 示例。

use anthropic_rust_sdk::{Anthropic, MessageContent, MessageCreateParams, MessageParam, Role};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Anthropic::new()?;

    let params = MessageCreateParams::new(
        "claude-opus-4-6",
        1024,
        vec![MessageParam {
            role: Role::User,
            content: MessageContent::Text("Say hello in one short sentence.".into()),
        }],
    )
    .stream(true);

    let stream = client.messages().stream(params);
    let message = stream.final_message().await?;
    for block in &message.content {
        if let Some(text) = block.text() {
            println!("{text}");
        }
    }

    Ok(())
}
