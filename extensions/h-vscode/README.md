# h VS Code extension

Chat with [h](https://github.com/) inside VS Code through a Webview panel. The
extension spawns `h serve` as a stdio child process and speaks the JSON-RPC
protocol documented in [`docs/vscode-integration.md`](../../docs/vscode-integration.md).

## Status

M3 (full chat surface): spawn + `server/hello` handshake, streaming text,
cancel, `ask/question` modal (options + free text), tool cards (Presentation
blocks: summary/code/diff/table/key-value), session picker
(create/resume/attach/close), `/clear` `/compact`, and token usage in the
header. Remaining M4 polish: markdown rendering, image paste, packaging
readiness.

## Prerequisites

- Node 20+ (`npm install`)
- A built `h` binary: `cargo build` at the repository root. The extension looks
  for `h` on PATH, or use the `h.path` setting to point at the binary.

## Build

```sh
npm install
npm run build      # tsc (extension host) + vite (webview bundle)
npm run typecheck  # type-check both sides without emitting
```

## Run

- **Development**: open this folder in VS Code and press F5 (launches the
  Extension Development Host), then run `h: Open Chat` from the command
  palette.
- **Installed**: `npm run package` produces `h-vscode.vsix`, install with
  `code --install-extension h-vscode.vsix`.

## Protocol smoke test (no VS Code needed)

```sh
npm run build:extension
node scripts/smoke.cjs /path/to/h   # defaults to `h` on PATH
```

Exercises the full loop against a real `h serve`: hello handshake,
`session/create`, `turn/submit` with streamed `text_delta`, `turn_finished`,
`session/close`, and graceful `server/shutdown`.

`node scripts/smoke-sessions.cjs /path/to/h` additionally exercises the M3
session lifecycle: list, close→archive, resume and attach transcript replay
(`prompt` → `text_delta` → `completed`), `/clear`, and `/compact`.

## Layout

```
src/
  extension.ts   # activate: command, server lifetime, panel ↔ RPC bridge
  server.ts      # spawn h serve + JSON-RPC 2.0 client over stdio (pure Node)
  protocol.ts    # wire types, mirrors src/serve/protocol.rs (do not drift)
  webview/       # React + Vite chat UI
```
