# anthropic-rust-sdk

一个由 AI 辅助的，将 [anthropic-sdk-typescript](https://github.com/anthropics/anthropic-sdk-typescript) 同步至 anthropic-rust-sdk 的项目。

An AI-assisted synchronization of the [anthropic-sdk-typescript](https://github.com/anthropics/anthropic-sdk-typescript) to the anthropic-rust-sdk.

## 概述

本项目以官方 TypeScript SDK 为上游参考实现，借助 AI 辅助将 API 定义、类型结构与行为逐步对齐并迁移为 Rust 实现，目标是提供与官方 SDK 能力对等的 Rust 客户端库。

**范围说明：** 仅同步主包 `anthropic-sdk-typescript/src/`；`packages/` 下的云厂商变体（Bedrock、Vertex、AWS、Foundry）不在支持范围内。

## 快速开始

```toml
[dependencies]
anthropic-rust-sdk = "0.112"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust
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

    if let anthropic_rust_sdk::MessageCreateResult::Message(message) =
        client.messages().create(params).await?
    {
        for block in &message.as_ref().content {
            if let Some(text) = block.text() {
                println!("{text}");
            }
        }
    }

    Ok(())
}
```

设置环境变量 `ANTHROPIC_API_KEY` 后即可运行。更多示例见 `examples/`。

> **注意：** 本库为社区维护的非官方 SDK，crates.io 包名为 `anthropic-rust-sdk`，代码中通过 `use anthropic_rust_sdk::...` 导入。

## 文档

- [架构说明](docs/ARCHITECTURE.md)
- [路线图](docs/ROADMAP.md)
- [实施计划](.plan/anthropic-rust-sdk.plan.md)

## 上游依赖

本仓库通过 git submodule 引用官方 TypeScript SDK：

| 项目 | 路径 | 仓库 |
|------|------|------|
| anthropic-sdk-typescript | `anthropic-sdk-typescript/` | [anthropics/anthropic-sdk-typescript](https://github.com/anthropics/anthropic-sdk-typescript) |

### 克隆与初始化

```bash
# 克隆时一并拉取 submodule
git clone --recurse-submodules <repo-url>

# 或克隆后初始化
git submodule update --init --recursive
```

### 更新上游 SDK

```bash
cd anthropic-sdk-typescript
git fetch origin
git checkout <tag-or-commit>
cd ..
git add anthropic-sdk-typescript
git commit -m "更新 anthropic-sdk-typescript submodule"
```

## 许可证

本项目采用 [MIT License](LICENSE)。
