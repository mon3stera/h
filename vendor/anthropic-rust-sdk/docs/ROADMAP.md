# 路线图

状态标记：✅ 已完成 | ⚠️ 警告 | 💤 延期 | ❌ 未开始 | 🚫 放弃/不支持

## 范围

### 云厂商变体（不支持）

| 项目 | 状态 | 说明 |
|------|------|------|
| AWS Bedrock (`bedrock-sdk`) | 🚫 | `packages/` 不纳入 |
| Google Vertex AI (`vertex-sdk`) | 🚫 | `packages/` 不纳入 |
| AWS API Gateway (`aws-sdk`) | 🚫 | `packages/` 不纳入 |
| Azure Foundry (`foundry-sdk`) | 🚫 | `packages/` 不纳入 |

### 可暂缓

| 项目 | 状态 | 说明 |
|------|------|------|
| Legacy Completions API | 💤 | 阶段四低优先级 |
| Node Agent Toolset | 💤 | 无 CLI/agent 场景暂不 port |
| CLI 迁移工具 | 🚫 | 不在范围内 |

## 阶段零：工程脚手架

- ✅ Cargo 工程与模块骨架
- ✅ CI（fmt / clippy / test）
- ✅ `.gitignore` / `rust-toolchain.toml`
- ✅ `docs/ARCHITECTURE.md`
- ✅ `docs/ROADMAP.md`

## 阶段一：核心客户端与 HTTP 层

- ✅ `Anthropic` 客户端与 `ClientOptions`
- ✅ 环境变量 `ANTHROPIC_API_KEY` 读取
- ✅ 错误类型树
- ✅ 指数退避重试
- ✅ HTTP 请求基础设施

## 阶段二：Messages API

- ✅ Messages 核心类型
- ✅ `messages.create()`（非流式）
- ✅ `messages.count_tokens()`
- ✅ 示例 `examples/messages_create.rs`

## 阶段三：流式与 MessageStream

- ✅ SSE 流式解析
- ✅ `messages.create(stream: true)`
- ✅ `messages.stream()` / `MessageStream`
- ✅ 示例 `examples/streaming.rs`

## 阶段四：Models、Batches、Completions

- ✅ `models.list()` / `models.retrieve()`
- ✅ `messages.batches` CRUD
- ✅ `completions.create()`（legacy）

## 阶段五：Beta API 域

- ✅ `beta` 聚合入口与 `Anthropic-Beta` header
- ✅ `beta.messages` / `beta.models`
- ✅ `beta.files` / `beta.skills`
- ✅ `beta.agents` / `beta.environments` / `beta.sessions`
- ✅ `beta.deployments` / `beta.deployment_runs`
- ✅ `beta.vaults` / `beta.memory_stores`
- ✅ `beta.webhooks` / `beta.user_profiles`

## 阶段六：Helpers 与高级运行时

- ✅ `messages.parse()` 结构化输出
- ✅ `BetaToolRunner` 工具循环骨架
- ✅ Webhook `unwrap` 验签
- ✅ Middleware 钩子

## 上游同步：0.104.2 → 0.106.0

子模块 `anthropic-sdk-typescript` 已对标 `sdk-v0.106.0`。本轮变更逐项对照如下。

### 已对齐（行为层改动）

- ✅ `user_profile_id` 抽取为 `anthropic-user-profile-id` 请求头（`messages` 与 `beta.messages` 的 create / count_tokens 及流式），不写入请求体
- ✅ 透传 `system.message` 流式事件（加入 SSE 事件白名单）
- ✅ 子模块指针与文档参考版本更新至 0.106.0

### 开放结构自动兼容（无需改动）

- ✅ 退役模型移除与新增模型：`model: String` 自由透传
- ✅ `code_execution_20260521` 工具、`allowed_callers` 新值、工具新字段（`cache_control` / `defer_loading` / `strict`）：`tools: Vec<Value>` 透传
- ✅ refusal 新增 `military_weapons` 类别：`ContentBlock.fields: Value` 透传

### 已对齐（运行时增强）

- ✅ 流式累积 `MessageStream`：逐事件重建 `final_message`，将 `input_json_delta` 在 `content_block_stop` 解析为工具块 `input`，并合并 `message_delta` 的 `stop_reason` 与 `usage`（对齐上游 `internal/message-stream-utils.ts`）

### 延期（需独立立项，记录原因）

- 💤 `x-stainless-helper` 遥测请求头：上游依赖对象级 `Symbol` 标记收集 helper 来源；Rust 中 `tools` / `messages` 为 `serde_json::Value`，缺少对象级标记载体，且闭集遥测值（MCP / betaZodTool / compaction / environments / session-tool-runner 等）对应的 helper 尚未在 Rust 实现。决定随相关 helper 生态一并引入，避免无调用方的空壳实现（不留技术债）。
- 💤 client 端 fallback middleware（client-side fallbacks）：`src/core/middleware.rs` 已有中间件链执行框架，但尚未接入 `src/client.rs` 请求链路，且缺少 fallback 模型配置入口与 refusal 重试语义；buffered 中间件与流式请求的兼容需先行设计。属较大特性，须单独立项（架构优先）。

## Rust 独立增强：0.106.1

上游无对应版本号，本轮为 Rust 侧运行时增强（发布于 crates.io `0.106.1`）。

- ✅ 分页兼容网关省略字段：`PageCursor` / `TokenPage` 对 `has_more` 等缺失字段使用默认值反序列化，并补充测试（[src/core/pagination.rs](../src/core/pagination.rs)）
- ✅ `ModelInfo` 字段默认值、`display_name` 接受 `name` 别名、新增 `effective_display_name()`（缺失时回退 `id`），并补充反序列化测试（[src/resources/models.rs](../src/resources/models.rs)）

## 上游同步：0.107.0 → 0.110.0

子模块 `anthropic-sdk-typescript` 已对标 `sdk-v0.110.0`。本轮变更逐项对照如下。

### 已对齐（行为层改动）

- ✅ 透传 Managed Agents 事件流 `event_start` / `event_delta`（加入 SSE 事件白名单，对齐上游 `0.109.0`）
- ✅ 子模块指针与文档参考版本更新至 0.110.0

### 已提前对齐（历史轮次已实现）

- ✅ count_tokens 的 `user_profile_id` 抽取为 `anthropic-user-profile-id` 请求头（上游 `0.107.0`）：`messages` 与 `beta.messages` 的 count_tokens 已在 0.106.0 轮次实现

### 开放结构自动兼容（无需改动）

- ✅ 新工具 `web_fetch_20260318` / `web_search_20260318`（`0.107.0`）：`tools: Vec<Value>` 透传
- ✅ 新模型 `claude-sonnet-5`（`0.108.0`）：`model: String` 透传
- ✅ 新 beta 请求头值 `agent-memory-2026-07-22`（`0.110.0`）：`Anthropic-Beta` 字符串透传
- ✅ Managed Agents 类型扩展（agents / deployments / sessions / vaults / webhooks 等，`0.109.0`）：beta 资源以 `Value` 透传
- ✅ refusal 移除 `military_weapons` 类别、`0.109.1` 移除若干无功能类型：`ContentBlock.fields: Value` 与开放结构透传

## Rust 独立增强：0.110.1

上游无对应版本号，本轮为 Rust 侧运行时增强（发布于 crates.io `0.110.1`）。

- ✅ `PageCursor<T>` 增加显式 `prev_page` 字段与 `has_prev_page()`，对齐上游 `BidirectionalPageCursor` 的数据能力（上游 `0.109.0`，仅 beta sessions 端点返回）；此前 `prev_page` 仅经 `#[serde(flatten)] extra` 透传，现提升为一等字段并补充反序列化测试（[src/core/pagination.rs](../src/core/pagination.rs)）

## 上游同步：0.111.0 → 0.112.3

子模块 `anthropic-sdk-typescript` 已对标 `sdk-v0.112.3`。本轮变更逐项对照如下。

### 已对齐（新增 beta 资源）

- ✅ 新增 `dreams` beta 资源（dreaming，上游 `0.111.0`）：create / retrieve / list / archive / cancel（[src/resources/beta/dreams.rs](../src/resources/beta/dreams.rs)），补充 `dreaming-2026-04-21` 常量
- ✅ 新增 `tunnels` beta 资源（MCP Tunnels，上游 `0.112.0`）：create / retrieve / list / archive / reveal_token / rotate_token（[src/resources/beta/tunnels.rs](../src/resources/beta/tunnels.rs)），补充 `mcp-tunnels-2026-06-22` 常量
- ✅ 新增 `tunnels.certificates` 子资源：create / retrieve / list / archive，端点 `/v1/tunnels/{tunnel_id}/certificates`
- ✅ 子模块指针与文档参考版本更新至 0.112.3
- ✅ 新增 `wiremock` 集成测试，覆盖新资源核心路径、分页解析与 `anthropic-beta` 头注入（[tests/beta_resources_test.rs](../tests/beta_resources_test.rs)）

### 开放结构自动兼容（无需改动）

- ✅ `messages` / `beta.messages` 新增 `speed`（推理速度模式）参数（`0.112.0`）：经 `MessageCreateParams` 的 `#[serde(flatten)] extra` 透传
- ✅ 无新增 SSE 流式事件类型，`src/internal/sse.rs` 事件白名单无需改动
- ✅ `0.112.1` / `0.112.2` / `0.112.3` 主要为文档更新，无功能性代码改动

### 延期（helper 生态）

- 💤 `SessionToolRunner` 权限门控增强（上游 `0.112.0`）：归入阶段六「延期」的 helper 生态项，待 `session-tool-runner` 等 helper 在 Rust 落地时一并实现，避免空壳实现（不留技术债）
