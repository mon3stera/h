# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
