# Vendored anthropic-rust-sdk

- Upstream: https://github.com/gaoyia/anthropic-rust-sdk
- Upstream tag: `sdk-v0.112.3`
- Upstream commit: `e75fc7eac5b60c63d25d05bdf377e1291e371db`
- License: MIT; see `LICENSE`

## Local changes

1. Allow clients to authenticate with only `ANTHROPIC_AUTH_TOKEN` or
   `ClientOptions::auth_token`.
2. Pass Anthropic SSE `error` events through `EventStream` instead of silently
   filtering them out.

## Updating

Import a newer upstream release, preserve this file and the upstream license,
then reapply or remove each local change after verifying whether upstream has
implemented it. Run the crate tests and the `h-core` test suite before updating
the pinned revision.
