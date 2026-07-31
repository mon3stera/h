# h

`h` is a small agentic coding CLI written in Rust. It connects an OpenAI
Responses-compatible model to a local coding environment, where the model can
inspect a repository, edit files, run commands, search code, fetch web pages,
and ask the user for decisions.

The project is under active development, so its configuration and internal APIs
may still change.

## Features

- Interactive terminal UI with streaming Markdown, syntax highlighting, diffs,
  tool activity, token estimates, and context usage.
- Clipboard image attachments for multimodal prompts, including keyboard and
  mouse removal controls.
- Headless mode for running a single prompt and printing the final response.
- Built-in tools for reading, writing, and editing files; searching with grep;
  fetching web pages; running Bash commands; and asking interactive questions.
- Blocking and background Bash commands, with persistent terminal sessions when
  `tmux` is available and a PTY fallback otherwise.
- Bounded tool output: long Bash, grep, and fetch results are saved to temporary
  files and presented as compact previews.
- Session archives and interactive resume support.
- Automatic context compaction and lightweight summaries for older tool output.
- Preservation of provider-native reasoning and tool-call history.
- Slash commands with prefix completion: `/clear` and `/compact`.
- Local Skill discovery compatible with h, Codex, Claude Code, and the common
  `.agents/skills` layout.
- Persistent user and project memory with bounded startup indexes and on-demand
  search, read, and write tools.
- Config-driven stdio MCP servers with automatic tool discovery and lifecycle
  management.
- Project and user instruction files for persistent guidance.

## What It Can Do

You can ask `h` to perform tasks such as:

- Explain an unfamiliar repository or trace a bug through the codebase.
- Implement a feature and update the relevant tests.
- Refactor code while preserving existing behavior.
- Run formatters, tests, builds, and other shell commands.
- Search source files and inspect large outputs without filling the context
  window.
- Fetch and summarize technical documentation.

## Requirements

- A recent stable Rust toolchain with Rust 2024 edition support.
- An OpenAI Responses-compatible API endpoint and model.
- A Unix-like operating system. The current Bash implementation uses Unix PTYs.
- `tmux` is optional, but enables the more capable persistent Bash backend.

## Installation

Build and install the binary from the repository:

```bash
cargo install --path .
```

For development, run it directly through Cargo:

```bash
cargo run --release
```

## Configuration

`h` reads its configuration from `~/.h/config.toml`. Create the directory and
configuration file before starting the CLI:

```toml
provider = "openai"
model = "gpt-5.6-sol"
reasoning_effort = "medium"
context_window = 200000
auto_compact_token_limit = 160000
tool_summary_turn_interval = 8

[providers.openai]
type = "openai"
name = "OpenAI"
base_url = "https://api.openai.com/v1"
bearer_token = "YOUR_API_KEY"
```

`reasoning_effort` accepts `none`, `minimal`, `low`, `medium`, `high`, `xhigh`,
or `max`. Provider support for individual values depends on the selected model
and endpoint.

The bearer token is currently stored directly in the configuration file. Keep
the file private and do not commit it to a repository.

### MCP servers

Add stdio MCP servers under `[mcp.servers.<id>]`:

```toml
[mcp.servers.search]
command = "node"
args = ["/path/to/search-server.mjs"]
cwd = "/path/to/server"
tools = ["query", "fetch"]

[mcp.servers.search.env]
API_KEY = "YOUR_API_KEY"
```

Configured servers are enabled by default. Set `enabled = false` in a server
table to keep its configuration without starting it. By default, every
discovered tool is exposed. Set `tools` to an allowlist of remote tool names to
expose only that subset; startup fails if a configured name is not provided by
the server. Exposed tools are registered as `<server>__<tool>`, such as
`search__query`. Server and tool names must therefore contain only ASCII
letters, digits, underscores, and hyphens.

`h` fails startup when an enabled MCP server cannot start or list its tools,
rather than silently ignoring a configured integration. MCP subprocesses are
closed when the interactive or headless session finishes.

## Usage

Start a new interactive session:

```bash
h
```

Run one prompt without opening the TUI:

```bash
h -p "Explain the architecture of this repository"
```

Headless sessions print only the final response and are not archived.

Choose an archived session to resume:

```bash
h --resume
```

Resume a known session directly:

```bash
h --resume <SESSION_ID>
```

Inside the TUI:

- `Alt+Enter` submits the prompt; `Ctrl+Enter` also works in terminals that
  support an enhanced keyboard protocol. Plain Enter inserts a newline.
- Paste an image with `Ctrl+V`. Attached images appear below the prompt and can
  be removed with their `×` button or with Backspace while the text is empty.
- `Shift+Tab` focuses image attachments; Left/Right selects one, Backspace or
  Delete removes it, and Esc or Tab returns to text input.
- `Esc` cancels the active turn.
- `Ctrl+C` exits the application.
- `/clear` archives the current context and starts a new session.
- `/compact` manually compacts the current context.

## Skills and Instructions

`h` discovers `SKILL.md` packages from user and project directories under:

- `.agents/skills`
- `.claude/skills`
- `.codex/skills`
- `.h/skills`

The same paths under the user's home directory are also searched. Only Skill
metadata is injected initially; the model reads the full `SKILL.md` when a task
matches it.

Persistent instructions can be placed in `.h/AGENTS.md`, `~/.h/AGENTS.md`, or
`~/.claude/CLAUDE.md`.

## Project Structure

```text
.
├── src/             CLI entry point, configuration wiring, and logging
├── crates/h-core/   Agent runtime, context, providers, tools, and Skills
├── crates/h-mcp/    MCP configuration, stdio clients, and Agent tool adapters
├── crates/h-memory/ Persistent user and project memory
└── crates/h-tui/    Terminal UI and rendering
```

`h-core` is independent of the terminal UI so other frontends can reuse the
agent runtime in the future.

## Memory

`h` stores persistent memory under `~/.h/memory`. User memory applies across
repositories, while project memory is isolated by the current Git repository.
Only bounded index snapshots are injected at startup; the agent can search and
read every stored topic on demand. Memory topics are plain Markdown, and their
generated `INDEX.md` files can be rebuilt from topic metadata.

## Safety

`h` can execute shell commands and modify files without an approval prompt.
Run it only in directories and with API endpoints that you trust, and review
important changes before committing them.
