# Vendored forks

This directory contains source-level forks of third-party Rust crates that need
small compatibility fixes before they can be used by `h`.

Each fork must retain its upstream license and include a `VENDORED.md` file with
the upstream repository, pinned revision, local changes, and update procedure.
Workspace crates should use explicit path dependencies so the selected source is
visible and reproducible.

## Crates

- `anthropic-rust-sdk`: Anthropic Messages API client with compatibility fixes
  for bearer-only gateways and streamed error events.
