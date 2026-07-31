# 架构文档

## 概述

`anthropic-rust-sdk` 以官方 [anthropic-sdk-typescript](../anthropic-sdk-typescript/) 主包为上游参考，将 API 定义与运行时行为迁移为 Rust 实现。

## 模块边界

```mermaid
flowchart TB
  subgraph public [公开 API]
    Client[Anthropic]
    Resources[resources]
    Lib[lib]
    Helpers[helpers]
  end

  subgraph core_layer [core]
    Error[error]
    Pagination[pagination]
    Streaming[streaming]
    Middleware[middleware]
  end

  subgraph internal_layer [internal]
    HTTP[http transport]
    Backoff[backoff]
    Sse[sse decoder]
  end

  Client --> Resources
  Client --> core_layer
  Resources --> core_layer
  Lib --> Resources
  Helpers --> Lib
  core_layer --> internal_layer
```

## 目录对照

| TypeScript | Rust | 可见性 |
|------------|------|--------|
| `src/client.ts` | `src/client.rs` | `pub` |
| `src/resources/` | `src/resources/` | `pub` |
| `src/core/` | `src/core/` | `pub` |
| `src/internal/` | `src/internal/` | `pub(crate)` |
| `src/lib/` | `src/runtime/` | `pub` |
| `src/helpers/` | `src/helpers/` | `pub` |

## 技术选型

| 能力 | 依赖 |
|------|------|
| 异步运行时 | `tokio` |
| HTTP | `reqwest`（rustls） |
| 序列化 | `serde` / `serde_json` |
| 错误 | `thiserror` |
| SSE 流 | `reqwest` stream + 内部 SSE 解码器 |

## 生成层与手写层

- **生成/迁移层**：`resources/` 中的请求/响应类型，对齐 OpenAPI spec（与 TS Stainless 生成层对应）。
- **手写层**：`lib/`（MessageStream、ToolRunner）、`core/middleware`、`helpers/`。

## 明确不支持

`anthropic-sdk-typescript/packages/` 下的云厂商变体不在本 crate 范围内：

| 上游子包 | 状态 |
|----------|------|
| `bedrock-sdk` | 不支持 |
| `vertex-sdk` | 不支持 |
| `aws-sdk` | 不支持 |
| `foundry-sdk` | 不支持 |

## 版本

- 上游 TypeScript SDK 参考版本：`0.112.3`（见 `anthropic-sdk-typescript/src/version.ts`）。
- 本 crate 版本：`0.112.3`（见 [`Cargo.toml`](../Cargo.toml)）。

### 版本策略

- 主次版本（`major.minor`）与上游 `anthropic-sdk-typescript` 对齐，标示已同步到的上游能力基线。
- patch 版本（第三位）由 Rust 侧自主管理，用于上游同一主次版本内的缺陷修复，以及针对第三方 Anthropic 兼容网关（如智谱 GLM）的健壮性增强。
- 因此本 crate 的 patch 位可能与上游不同（例如上游 `0.106.0` 对应 Rust `0.106.1`）。已发布版本不可变更，修复只能通过递增 patch 发布新版本。

上游同步的逐项对照见 [`docs/ROADMAP.md`](ROADMAP.md) 的「上游同步」小节。
