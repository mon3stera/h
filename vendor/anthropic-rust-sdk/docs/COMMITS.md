# 提交信息规范

采用 [Conventional Commits](https://www.conventionalcommits.org/)，格式：

```
<type>[scope][!]: <中文摘要> / <English summary>

<中文正文（可选）>

<English body（可选）>
```

## type 前缀

| 前缀 | 用途 |
|------|------|
| `feat` | 新功能 |
| `fix` | Bug 修复 |
| `docs` | 仅文档变更 |
| `refactor` | 重构（不改变外部行为） |
| `test` | 测试相关 |
| `build` | 构建系统、Cargo、依赖 |
| `ci` | CI 配置 |
| `chore` | 杂项（不影响源码逻辑） |
| `perf` | 性能优化 |

## 规则

- 标题一行：中文摘要 + ` / ` + 英文摘要。
- **破坏性变更**在 type 后加 `!`（如 `refactor!:`），正文注明迁移方式。
- 正文可选；需要时用中英各写一段。
- 每条提交只做一件事；不混无关改动。
- 不提交密钥、`.env` 等敏感文件。

## 示例

```
feat(messages): 支持流式 create / Add streaming messages.create

实现 SSE 事件解析与 MessageStream 封装。
Implement SSE parsing and MessageStream wrapper.
```

```
refactor!: 将库导入名改为 anthropic_rust_sdk / Rename lib import to anthropic_rust_sdk

Breaking change: use anthropic_rust_sdk:: instead of anthropic::
```
