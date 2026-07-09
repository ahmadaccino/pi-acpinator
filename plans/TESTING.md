# Testing strategy

Three layers; keep unit/component tests hermetic (no real pi, no model, no network).

## 1. Unit (pure functions) — fast, no I/O
- `pi/events.rs`: `parse_line` classification, `text_delta`/`thinking_delta` extraction
  (existing tests here — extend). Add: LF framing edge cases if a custom codec is introduced
  (currently `tokio_util` `LinesCodec` splits on `\n` bytes — compliant with the
  `U+2028`/`U+2029` footgun; add a test asserting a JSON string containing `U+2028` survives).
- `acp/translate.rs` (M1.5): `tool_kind`, `tool_result_text`, title formatting, diff extraction.
- M2: allowlist decision + permission option→confirmed mapping.
- M3: `provider/id` slug split; cancel→`Cancelled` logic.
- M4: coalescer flush-boundary semantics.

## 2. Component — bridge behavior without real pi or a real client
Two injectable seams to add:
- **Fake pi transport.** Refactor `PiClient` so `spawn` is one constructor and add
  `PiClient::from_channels(outgoing_lines_rx, incoming_tx)` (or generic over
  `AsyncWrite + AsyncRead`) so a test can (a) capture the JSON command lines the bridge writes
  and (b) feed scripted stdout lines (`response`/`event`/`extension_ui_request`). Keep the real
  spawn path unchanged; the test path just swaps the byte streams.
- **In-memory ACP client.** The `agent-client-protocol` crate exposes `Channel::duplex()` — one
  end drives our `Agent` (`Agent.builder()...connect_to(agent_side)`), the other is a test
  `Client` (`Client.builder()...connect_with(client_side, ...)`). Send `initialize`/`session/new`/
  `session/prompt` from the test client; assert the `session/update` notifications + responses.
  (See Zed's `crates/agent_servers/src/acp.rs` test module for the exact `Channel::duplex` +
  builder pattern.)

Component cases to cover:
- initialize handshake; session/new registers a session (fake pi answers `get_state`).
- prompt: fake pi streams `text_delta`×N + `agent_end` → assert coalesced `agent_message_chunk`
  + `PromptResponse{EndTurn}`.
- tool lifecycle: start/update/end → assert `tool_call` + `tool_call_update(Completed)`.
- thinking_delta → `agent_thought_chunk`.
- permission (M2): fake pi emits `extension_ui_request(confirm)` → assert ACP
  `session/request_permission` sent; feed Selected(allow) → assert `extension_ui_response
  {confirmed:true}` written to fake pi.
- cancel (M3): `session/cancel` → fake pi sees `abort`; after `agent_end`, prompt resolves
  `Cancelled`.
- supervision (M4): fake pi stdout closes mid-turn → prompt resolves with error, pending pi
  requests fail fast.

## 3. Live smoke (manual / CI-gated, needs real pi + auth)
Node driver over the built binary's stdio (pattern in `plans/README.md`): initialize →
session/new → session/prompt "reply PONG" → assert streamed text + `stopReason`. Extend per
milestone (force a tool for M1.5; permission allow/deny for M2; model switch + cancel for M3).
Run bench harness (M4) here too.

## Commands
`cargo test` (unit + component). Live smoke is a separate script (not in `cargo test`).
