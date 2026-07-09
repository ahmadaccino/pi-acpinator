# pi-acpinator — remaining work (plan index)

Self-contained plans for the milestones after the working M0/M1-core baseline. Each file
assumes no memory of prior chat; it cites the real code + the real crate/pi APIs.

## Repo facts
- Path: `/Users/ahmad/Documents/projects/pi-acpinator` (Rust, `cargo build` / `cargo test`).
- `pi` on PATH (v0.80.3+, logged in), spawned as `pi --mode rpc --no-session` in the session cwd.
- ACP deps: `agent-client-protocol` 1.2.0 + `agent-client-protocol-schema` 1.4.0.
  Import protocol/schema types from `agent_client_protocol::schema::v1::*`.
  Crate source (read for exact shapes):
  `~/.cargo/registry/src/index.crates.io-*/agent-client-protocol-{1.2.0,schema-1.4.0}/src`.
- pi RPC protocol reference: `pi` repo `packages/coding-agent/docs/rpc.md` (commands, events,
  extension-UI sub-protocol). Full text was captured during design; re-fetch if needed.

## Current code anchors
- `src/main.rs` — ACP agent (`Agent.builder()...connect_to(Stdio::new())`).
  - `State { sessions: Arc<Mutex<HashMap<SessionId, Arc<Session>>>> }`,
    `Session { pi: Arc<PiClient>, incoming: Mutex<PiIncoming> }`.
  - handlers: `initialize`, `session/new` (`start_session` spawns pi + `get_state`),
    `session/prompt` (`run_prompt` streams), `session/cancel` (abort). Dispatch fallback.
  - `run_prompt` currently maps only `text_delta` → `SessionUpdate::AgentMessageChunk`.
- `src/pi/client.rs` — `PiClient::spawn(program,args,cwd,env) -> (PiClient, PiIncoming)`.
  `PiIncoming = mpsc::UnboundedReceiver<Incoming>`. Methods: `send(Command)`,
  `request(Command, id, timeout) -> Response`, `respond_ui(ExtensionUiResponse)`, `next_id()`.
- `src/pi/events.rs` — `Command` enum (Prompt/Steer/Abort/GetState/SetModel/SetThinkingLevel/
  GetAvailableModels), `Image`, `ExtensionUiResponse`, `Incoming{Response,ExtensionUiRequest,
  Event,Other}`, `Response{id,command,success,error,data}`, `ExtensionUiRequest{id,method,
  title,message,options}`, `Event{kind,message,assistant_message_event,tool_call_id,tool_name,
  args,result,is_error,will_retry}`, `parse_line`, `Event::text_delta()/thinking_delta()`.

## Live smoke pattern (Node driver over the binary's stdio)
Send LF-delimited JSON-RPC 2.0:
1. `{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1}}`
2. `{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/tmp","mcpServers":[],"additionalDirectories":[]}}`
3. `{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"<from #2>","prompt":[{"type":"text","text":"..."}]}}`
Collect `session/update` notifications + the id:3 result (`stopReason`).

## Milestones
- `M1_5-tools-and-thinking.md` — [DONE] tool_call/tool_call_update mapping, thought streaming, locations.
- `M2-permission-gate.md` — [DONE] bundled pi extension + `session/request_permission` (the differentiator).
- `M3-config-and-sessions.md` — [DONE] thinking-level modes, model config option (+ set_config_option), session/load history replay, terminal auth, cancel→Cancelled.
- `M4-perf-and-benchmarks.md` — [DONE] delta coalescing, pi-exit supervision, bounded outbound + incoming backpressure (fire-and-forget UI events dropped at the reader so the bounded event queue can't deadlock between turns), release profile, benchmark harness.
- `M5-distribution.md` — [DONE] npm shim + platform packages, cross-build release workflow, CI, cargo metadata.
- `TESTING.md` — [DONE] unit + transport tests (fake pi via duplex) + deterministic component harness (fake pi drives the real binary over ACP, 12 checks) + live smokes.

## Verified end-to-end
Against real pi and against a scripted fake pi (component test, no model):
initialize (caps+auth) · session/new (6 modes + 159-model config) · authenticate · prompt
(coalesced text+thought+tool streaming) · request_permission (allow/reject) · set_mode ·
set_config_option · session/load (history replay) · cancel (Cancelled).

Bench (deterministic): 1.95 MiB binary · ~2.3 ms cold start · ~0.3 ms TTFT · ~1.2M deltas/s ·
~44x delta coalescing · ~5.5 MiB peak RSS.
