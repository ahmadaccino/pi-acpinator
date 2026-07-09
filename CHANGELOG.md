# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- ACP agent over stdio that spawns and drives `pi --mode rpc`.
- `initialize` (advertises `load_session`), `session/new`, `session/prompt`,
  `session/cancel` (`StopReason::Cancelled`).
- Streaming of assistant text and reasoning as `agent_message_chunk` /
  `agent_thought_chunk`, with delta-burst coalescing.
- Tool calls mapped to `tool_call` / `tool_call_update`; `write`/`edit` surface
  as structured diffs; image content blocks are forwarded to pi.
- `session/request_permission` via a bundled pi permission-gate extension
  (`PI_ACPINATOR_APPROVAL` = `off` | `mutating` | `all`).
- `session/set_mode` (thinking level) and `session/set_config_option` (model,
  validated).
- `session/load` — resume a persisted session and replay its history; reuses a
  live session instead of spawning a second pi.
- Bounded pi stdin/event channels for backpressure; fails a turn if pi exits early.

[Unreleased]: https://github.com/ahmadaccino/pi-acpinator
