# h × VS Code 集成设计（P1：`h serve` + Webview 扩展）

> 状态：设计稿，等待评审。评审通过后按 M1–M4 里程碑实施。
> 范围：P1（真集成版）。P0（集成终端快捷方式）已明确跳过——集成终端里直接跑 `h` 本身就是 P0。

## 1. 目标与非目标

**目标**

- 给 h 增加无界面常驻 API 面 `h serve`：JSON-RPC 2.0 over stdio（JSONL 帧），多会话，事件推送，ask/answer 点对点交互。
- 一个 VS Code 扩展（`extensions/h-vscode/`）：扩展宿主 spawn `h serve`，Webview（React）做聊天界面——流式文本、工具卡片、提问弹窗、会话列表/恢复、取消、`/clear` `/compact`。
- 复用一切：profile 解析、archive/resume、identity 约束、权限、mcp、skill、compaction 全走现有 `build_agent`，与 TUI 同源。

**非目标（本阶段明确不做）**

- 工具执行审批门禁：默认无审批，与 TUI 一致。协议层已为审批预留扩展点（见 §7）。
- 独立守护进程：`h serve` 是扩展宿主的 stdio 子进程。VS Code 退出 → stdin EOF → serve 优雅收尾（存档全部会话）→ 退出。VS Code 重开后从 Webview 恢复会话。真正脱离宿主的 socket daemon 是 v2，不在本设计内。

## 2. 已验证的架构基础（当前代码事实）

| 结论 | 证据 |
|---|---|
| `Agent` 与 UI 解耦 | `crates/h-core/src/agent.rs`：`Agent::run(Receiver<AgentCommand>)` 消费 Prompt/Run/Cancel；`continue_turn(prompt, CancellationToken)`；`subscribe_view() -> UnboundedReceiver<AgentViewEvent>` |
| 事件语义齐全 | `event.rs`：`AgentViewEvent` 含 Startup/Prompt/TextDelta/Search/Tool/TurnStart/TokenUsage/SessionStarted/CommandFinished/ContextCompacted/TurnFinished/Completed/Err |
| `Bridge` 可承载 ask | `interaction.rs`：`Bridge::ask(AskQuestion) -> AskAnswer`，`Request::Ask{question, reply: oneshot}` 点对点 |
| 事件是 push 分发 | `bus.rs`：`EventBus<T>::subscribe()/broadcast()`，断连自动剪枝 |
| headless 只跑单轮 | `headless.rs`：`run(agent, prompt)` 消费 agent，无 archive |
| 无审批命令 | `command.rs`：`Command` 只有 `Clear`/`Compact` |
| `build_agent` 已抽出、UI 无关 | `src/main.rs`：返回 `(Agent<Client>, usize context_window, h_mcp::Runtime)`，TUI 与 headless 共用；`h serve` 可直接复用 |
| 序列化基础 | `UserInput`/`InputPart`/`Image`/`Search` 已 serde；`Presentation`/`ToolCall`/`ToolCallResult` 等需补 derive（见 §3.7） |
| stdout 纯净 | `logger.rs` 写 `h.log` 文件，不进 stdout；serve 可独占 stdout 走协议 |
| 会话存档/恢复 | `Agent::archive()`（无交换则跳过）、`Agent::resume(id)`（含 identity 校验）、`context::list_sessions() -> Vec<SessionMeta>` |

## 3. 协议设计（JSON-RPC 2.0 over stdio JSONL）

### 3.1 帧格式与通道纪律

- 每条消息一行 JSON，UTF-8，`\n` 结尾。不设长度前缀（对齐 Codex 验证过的模式）。
- **stdout 只出协议帧**。日志已确认走文件；serve 内禁止任何 `println!`/`print!`。
- 协议版本在握手里声明，客户端不匹配即快速失败。
- stdin EOF、`server/shutdown`、SIGTERM/SIGINT → 同一套优雅关闭路径（§4.5）。

### 3.2 握手（服务端启动即发一条通知）

```json
{"jsonrpc":"2.0","method":"server/hello","params":{"protocol_version":1,"version":"0.3.0","pid":1234}}
```

客户端据此校验 `h` 版本与协议兼容性。

### 3.3 客户端请求 → 服务端

| method | params | result | 说明 |
|---|---|---|---|
| `session/create` | `{profile?: str, instruction?: str}` | `{session_id, context_window}` | 新会话，走 `build_agent(None, profile, Bootstrap)`；`instruction` 映射 `Bootstrap::Instruction`；`context_window` 同时见于 `session/started` 通知 |
| `session/resume` | `{session_id}` | `{session_id, context_window}` | 走 `build_agent(Some(id), None, Default)`，identity 不符则报错 `-32002` |
| `session/list` | `{}` | `{archived: [{id,title,last_modified}], active: [{id}]}` | archived 来自 `list_sessions()`，active 来自会话池 |
| `session/close` | `{session_id}` | `{archived: true}` | 优雅关闭：drop 命令通道 → worker 收尾 → archive → 关 mcp → 移除；回合进行中则等它结束 |
| `turn/submit` | `{session_id, text, images?: [{media_type, data, width, height}]}` | `{accepted: true}` | `images` 经 `Image::from_base64` 校验后构造 `UserInput`；回合进行中的提交按 agent 现有队列语义排队 |
| `turn/cancel` | `{session_id}` | `{accepted: true}` | 映射 `AgentCommand::Cancel`（取消当前回合；排队中的 prompt 仍会执行） |
| `command/run` | `{session_id, command: "/clear"\|"/compact"}` | `{finished: true}` | 映射 `AgentCommand::Run`，等服务端 `command_finished` 事件后回包 |
| `session/attach` | `{session_id}` | `{replayed: true, context_window}` | 触发 `agent.rebroadcast_all_view()`，供 Webview 重开时重建视图 |
| `server/shutdown` | `{}` | `{ok: true}` | 触发优雅关闭并退出 |

### 3.4 服务端通知 → 客户端

- `server/hello`（无 session_id）
- `session/started` `{session_id, model, thinking_effort, context_window}`（来自 Startup 与 /clear 的 SessionStarted；`context_window` 为配置的上下文窗口，供前端渲染 `context current/limit` 指示器）
- `session/event` `{session_id, event: {…}}` —— 单一方法 + 统一形状 `{type, data}`（adjacent tagging），与 `AgentViewEvent` 一一对应：

| event.type | data |
|---|---|
| `prompt` | 字符串（resume 重放用） |
| `text_delta` | 字符串增量 |
| `search` | `{id, status, action}`（view 投影，无 provider 私有字节，见 §3.7） |
| `tool` | `Presentation`（`{call_id, name, label, target?, status, blocks}`） |
| `turn_start` | `null` |
| `token_usage` | `{context?, turn?}`（None 字段省略） |
| `session_started` | `null` |
| `command_finished` | `"/clear"` \| `"/compact"` |
| `context_compacted` | `null` |
| `turn_finished` | `{completed}` |
| `completed` | `null` |
| `error` | 错误消息字符串 |

> 注意：`search.data` 的 `status` 与 `action` 沿用存档格式——`SearchStatus` 序列化为帕斯卡字符串（`"Succeeded"`）、`SearchAction` 为外部标签（`{"Query": {…}}`）。这两个类型被 `Message::Search` 存档绑定，改 serde 属性会破坏旧存档兼容，故不做 wire 层美化。

> 备选：每个事件一个 method（`text/delta`、`tool/completed`…）。差别只在协议表面；客户端都是同一个 switch。选单方法 + tag 是更小的协议面，未来加事件不破坏客户端。

### 3.5 服务端发起请求（ask/question ↔ ask/answer）

`Bridge` 的 `Request::Ask` 到达时，serve 发起：

```json
{"jsonrpc":"2.0","id":17,"method":"ask/question",
 "params":{"session_id":"…","question":"…","options":[{"label":"…","description":"…"}]}}
```

客户端回复（`AskAnswer` 直接映射）：

```json
{"jsonrpc":"2.0","id":17,"result":{"answer":{"type":"option","data":{"index":0,"label":"…"}}}}
{"jsonrpc":"2.0","id":17,"result":{"answer":{"type":"free_text","data":"…"}}}
```

serve 持有该请求的 oneshot，把 answer 送回正在等待的 `Bridge::ask` 调用方，agent 继续。

### 3.6 错误码

- 标准：`-32700` 解析错、`-32600` 非法请求、`-32601` 未知方法、`-32602` 参数错
- 自定义：`-32000` 会话不存在、`-32001` 会话忙碌（若需要）、`-32002` resume 被拒（identity 不符）、`-32003` profile 错误、`-32004` 提供方/初始化失败

### 3.7 事件序列化策略（h-core 改动）

原则：**`AgentViewEvent` 整体裸 derive `Serialize`**。为此做一次小的类型重构——view 契约只携带可渲染字段，provider 私有字节不进 view（否则裸 derive 会把 `Search.state` 带上 wire，而 `Search` 本身因存档兼容不能动）。

- **新增 `SearchView` 投影**（`{id, status, action}`，`SearchStatus`/`SearchAction` 已 serde，从 `Search` 经 public getter 构造，`impl From<&Search>`）。
- **`AgentViewEvent::Search(Search)` 改为 `AgentViewEvent::Search(SearchView)`**——这是全计划唯一的**非加法** h-core 改动（约 30–60 行机械重构，收益是 §3.7 无需任何手写 Serialize）：
  - `agent.rs`：live 广播（`ProviderSignal::Search` → 投影）与 rebroadcast（`Message::Search` → 投影）两处；
  - `h-tui`：`RenderUnit::Search` 消费点换类型（render 逻辑不变，仍按 id/status/action）；
  - 测试：`agent.rs` / `ui.rs` / `transcript.rs` 的全等比较改为投影比较。
- **`AgentEvent::Search` 保留完整 `Search`**：它是语义事件流（headless 忽略、测试用于核对 context 存储），不是 view 契约。
- **`Message::Search` / `Search` 本体不动**：`Search` 已 serde 且是 `Message::Search` 的载荷，随 `Context::archive_in()` 序列化进存档文件——`state: Vec<u8>` 必须留在存档里供 resume 回放。改它任何 serde 属性都会破坏旧存档兼容。
- **其余载荷类型补 `#[derive(Serialize)]`**（纯加法，不改变现有行为）：
  - `Presentation` 子树（`tool/presentation.rs`）：`Presentation`、`DisplayBlock`、`DiffLine`、`DiffLineKind`、`KeyValueEntry`、`ToolCallStatus`。`Presentation` 本就是面向展示的契约（TUI 渲染的就是它），Webview 直接消费正合适。
  - `ToolCall` 族（`tool/mod.rs`）：`ToolCallId`、`ToolCall`、`ToolCallOutcome`、`ToolCallResult`（`Summary` 已 serde）。
  - `Command`（`command.rs`）：wire 上输出 `/clear`/`/compact` 标签。
  - `AskQuestion` / `AskOption` / `AskAnswer`（`interaction.rs`）：双向 serde（回包需要）。
- 所有 tag 化枚举用 **adjacent tagging**：`#[serde(tag = "type", content = "data", rename_all = "snake_case")]`，统一产出 `{type, data}`。原因：serde 的 internal tagging 无法序列化**含字符串的 newtype 变体**（`TextDelta(String)`、`DisplayBlock::Summary(String)`、`AskAnswer::FreeText(String)` 会直接报错），adjacent tagging 支持全部变体形状且零手写代码。`Err` 变体显式 `#[serde(rename = "error")]`（`rename_all` 会给它 `"err"`）。
- 已落地并测试锁定（`event.rs` 新增 wire 形状断言测试 + `interaction.rs` 往返测试）：`AgentViewEvent`、`DisplayBlock`、`ToolCallStatus`、`ToolCallOutcome`、`AskAnswer` 全部 `{type, data}` 形状；`Command` 输出 `/clear`/`/compact`；`DiffLineKind` 输出 `"removed"`/`"added"`/`"context"`。

## 4. `h serve` 实现设计

### 4.1 CLI 改动（小）

`cli.rs` 的 `Args` 加子命令字段：

```rust
#[command(name = "h", version, about, args_conflicts_with_subcommands = true)]
pub struct Args {
    // …现有 flags 原样保留…
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// 无界面 API 服务，供 IDE 集成
    Serve(ServeArgs),
}
```

- `h serve` 走 serve；`h -p …`、`h --resume …` 等现有形态不变（`args_conflicts_with_subcommands` 保证 `h serve` 与旧 flags 互斥）。
- 现有 cli.rs 测试应全部保持通过（子命令默认 `None`）。
- `main.rs`：`build_agent` 改 `pub(crate)`，`main` 里 `match args.command { Some(Serve(a)) => serve::run(a).await, None => … }`。

### 4.2 模块结构（根二进制 crate 内）

```
src/
  cli.rs          # 加 serve 子命令
  main.rs         # 分发 + build_agent 改 pub(crate)
  serve/
    mod.rs        # run_serve：握手、stdin 循环、关闭协调
    protocol.rs   # JSON-RPC 消息类型（serde）+ 错误码
    session.rs    # SessionManager、per-session worker、事件转发、ask 任务
```

依赖：root `Cargo.toml` 补 `serde_json`（目前只有 h-core 有）。

### 4.3 会话生命周期

```
session/create / resume
  └─ build_agent(id?, profile, bootstrap, bridge)   // 每会话一个 Bridge::new()
  └─ let bus_rx = agent.subscribe_view()            // 必须先于 initialize，捕获 Startup
  └─ agent.initialize()?
  └─ resume 时 agent.rebroadcast_all_view()         // 同 main_loop 顺序
  └─ spawn worker:  agent.run(command_rx).await → agent.archive()
  └─ spawn bridge 任务:  loop { Request::Ask → ask/question → 等 ask/answer → oneshot 回填 }
  └─ spawn 事件转发任务:  bus_rx → session/event 通知（写入共享输出写者）
  └─ 登记 SessionHandle { command_tx, worker, bridge_task, forwarder, mcp }
```

- 一个共享输出写者（`Arc<Mutex<BufWriter<Stdout>>>`），所有会话的事件/响应都经它逐行 flush——JSONL 单行写入天然原子，多会话交错不串帧。
- 请求分发器维护 `pending: HashMap<id, oneshot::Sender<…>>`：`ask/question` 的 id 注册进去，`ask/answer` 响应按 id 路由回对应 oneshot。
- MCP runtime：**每会话一个**（`build_agent` 原样返回，零重构，语义等同"每会话 = 一个 TUI 进程"）。`Runtime::register` 是 `&self`，多 agent 共享一个 runtime 在技术上可行，作为后续优化项，见 §7。

### 4.4 请求分发

- stdin 用 `tokio::io::BufReader` 逐行读。
- 解析失败 → 按规范回 `id: null` 的 parse error，继续读（不崩）。
- 请求 → 按 method 分发（session/create|resume|list|close|attach、turn/submit|cancel、command/run、server/shutdown）。
- 通知（无 id）→ 记录日志；本协议客户端不发通知，宽容处理。

### 4.5 关闭语义（stdin EOF / server/shutdown / 信号）

1. 停止接收新输入。
2. 对每个会话：`drop(command_tx)` → `await worker`（正在进行的回合被取消并收尾）→ `await bridge_task` → `mcp.close()`。
3. `worker` 结束后 serve 侧 `agent.archive()` 已由 worker 完成（agent 被移入 worker 时持有）。
4. 退出码 0。

> 注意：`agent` 被 move 进 worker，archive 在 worker 内执行（同 `main_loop` 现状）。若回合中途被取消，`archive()` 的"无交换则跳过"逻辑保证空会话不落盘。

## 5. 扩展设计（`extensions/h-vscode/`）

### 5.1 目录结构

```
extensions/h-vscode/
├── package.json        # name: h-vscode, displayName: h, publisher: h-dev(占位), engines.vscode
├── tsconfig.json
├── vite.config.ts      # webview bundle 构建
├── src/
│   ├── extension.ts    # activate：注册命令、spawn h serve、panel 生命周期
│   ├── server.ts       # child_process + JSON-RPC 客户端（行帧、pending map、通知分发）
│   ├── protocol.ts     # 与 §3 对应的协议类型（与 serve 手动对齐，勿漂移）
│   └── webview/
│       ├── main.tsx
│       ├── App.tsx
│       ├── hooks/useSession.ts
│       └── components/
│           ├── Chat.tsx            # 消息列表 + 输入框 + 取消按钮
│           ├── Markdown.tsx        # react-markdown 渲染 assistant 文本
│           ├── ToolCard.tsx        # Presentation → Summary/CodeBlock/Diff/Table 组件
│           ├── AskModal.tsx        # ask/question 弹窗（选项 + 自由文本）
│           ├── SessionPicker.tsx   # 活动/存档会话列表，resume/create
│           └── StatusBar.tsx       # token 用量 / 上下文条
└── media/style.css
```

### 5.2 extension host

- `activate`：注册命令 `h.openChat`。spawn `h serve`（h 路径：`h.path` 配置 > PATH；找不到给出安装提示）。
- 等待 `server/hello`（超时即报错退出），校验 `protocol_version`。
- WebviewPanel：`enableScripts: true`，加载 vite 构建产物；`webview.onDidReceiveMessage` ↔ RPC client 双向桥接。
- 面板关闭 ≠ 会话关闭：serve 继续跑，**会话跨面板关闭存活**（这是"常驻"在本设计内的落点）。只有扩展宿主退出（VS Code 关闭）时 serve 才随 stdin EOF 优雅存档。
- Webview 重开：新 panel 发 `session/attach` 重建视图。

### 5.3 Webview ↔ TUI 的映射

| TUI（h-tui） | Webview |
|---|---|
| `text.rs` 消息渲染 | `Markdown.tsx`（react-markdown；高亮为打磨项） |
| `tool.rs` Presenter 分块折叠 | `ToolCard.tsx`：`DisplayBlock::Summary/CodeBlock/Diff/Table` 一一对应组件；折叠/展开状态 |
| `input.rs` 粘贴图片 | 粘贴 → dataURL → `turn/submit.images`（`Image` 已支持 base64 + 尺寸校验） |
| `choice_list.rs` 选择 | `AskModal.tsx`（选项 + 自由文本，等价 `AskAnswer`） |
| `resume.rs` 会话选择 | `SessionPicker.tsx`（`session/list` + `session/resume`） |
| 状态行（token/上下文条） | `StatusBar.tsx`（`token_usage` 事件） |

### 5.4 打包与安装

- `vsce package` → VSIX → `code --install-extension`。publisher 用占位符（如 `h-dev`），本地安装不校验市场。
- 激活方式：`onCommand:h.openChat`，不自动启动。
- 版本兼容：`server/hello` 的 `version` 与扩展期望的 h 版本比对，不匹配给提示（如"请重建 h 二进制"）。

## 6. 里程碑

| 里程碑 | 内容 | 验收 |
|---|---|---|
| **M1** | serve 核心 + 协议（Rust 侧全部）：cli 子命令、protocol/framing/session、事件序列化、ask 回路、优雅关闭；单元测试 + `scripts/serve-smoke.sh` 手工冒烟 | JSONL 手工喂入能建会话、跑回合、看事件、答 ask |
| **M2** | 扩展骨架端到端：spawn serve、握手、Webview 流式显示文本事件（最小聊天） | VS Code 里发一句话，Webview 流式出回复 |
| **M3** | 完整 Webview：工具卡片、AskModal、会话列表/resume/create、取消、`/clear` `/compact`、token 状态条 | 对齐 TUI 主要交互 |
| **M4** | 打磨：markdown、图片粘贴、错误展示、`session/attach` 重连、打包与 README | 可日常使用 |

## 7. 已定默认与待确认点

**已按推荐默认采纳（可推翻）：**

1. 技术栈 React + Vite（webview 复杂度值得标准构建链）。
2. 默认无审批，与 TUI 一致。**预留扩展点**：协议已含 `ask/question`；将来若加审批，走 agent 层拦截钩子（h-core 新增，如 `Agent::with_approval_policy`），serve 只需把 `bash`/`write` 的执行包装成 ask 请求，协议无需变动。
3. 命名 `h-vscode`，本地 VSIX 安装，不外发。
4. `h serve` 独立子命令复用 `build_agent`，TUI/headless 零改动。

**需要你在评审时明确的点：**

- A. MCP runtime 每会话一个（零重构默认）还是先做共享 runtime 优化？默认前者。
- B. 事件走单一 `session/event`（默认）还是每事件一个 method？
- C. "常驻"范围：面板关闭会话存活、VS Code 退出存档可恢复（默认）。是否接受？还是要求真守护进程（v2，另行设计）？
- D. 扩展放本仓库 `extensions/h-vscode/`（默认）还是独立仓库？
