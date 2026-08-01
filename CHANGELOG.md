# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0] - 2026-08-01

### Added

- **IDE integration server**: `h serve` exposes a multi-session JSON-RPC API
  over stdio, including session creation, resume and attachment, streaming
  events, image prompts, cancellation, slash commands, and interactive Ask
  responses.
- **VS Code extension**: use `h` from a dedicated chat panel with streamed
  Markdown, tool cards, image paste, session management, slash-command
  completion, context usage, and configurable font size.
- **Named profiles and `--profile`**: each profile bundles its provider
  protocol, endpoint, model, reasoning effort, and optional context limits.
- **Legacy archive migration utility**: preserve one selected session created
  before upstream identity tracking by patching its profile identity, with a
  dry-run option before the utility prunes the remaining archives.

### Changed

- **Profiles**: `[providers.<id>]` is now `[profiles.<id>]`, and each profile
  carries its own `model` and `reasoning_effort`. `context_window` and
  `auto_compact_token_limit` may be set per profile (optional) and fall back
  to the global values. The top-level `provider` key is renamed to `profile`.
  This is a breaking change: migrate `~/.h/config.toml` by moving `model` and
  `reasoning_effort` into each profile block.
- **Safer session resume**: archived sessions are now tied to the protocol and
  endpoint that created them, preventing provider-native reasoning and tool
  state from being replayed through an incompatible upstream.
- **Richer TUI status**: the status line now shows the active model, reasoning
  effort, and a rainbow remaining-context indicator.
- **Larger retained outputs**: tool-output previews and Memory reads now retain
  up to 16K characters before truncation.

### Fixed

- **Nested file creation**: `write_file` now creates missing parent
  directories before writing a new file.
- **VS Code session switching**: pending Ask state is cleared when moving to a
  different session.

## [0.3.0] - 2026-08-01

### Added

- **Anthropic provider support**: configure a provider with `type = "anthropic"`
  to talk to Anthropic-compatible endpoints, using the same top-level settings
  as OpenAI-compatible ones (`base_url`, `auth_token`, `model`, ...).
- **`--instruction` flag**: replace every default system prompt for a new
  session — the harness prompt, persistent instruction files, Skill catalog,
  Memory snapshot, and workspace information are skipped. It cannot be
  combined with `--resume`.

### Changed

- **Spinner animation**: a gray wave now chases through the spinner word one
  character at a time, and the word sits in the default color with gaps around
  the running indicator.
- **Resume session list**: ages are now compact and column-aligned (`5m ago`
  instead of `5 minutes ago`).
- **Choice lists scroll** when the entries overflow the available height,
  instead of clipping.

### Fixed

- **Text wrapping now follows Unicode line-break rules**, so wrapped text no
  longer breaks in the middle of a word or a CJK run.
