# M3 — Config options + session lifecycle + auth + cancel correctness

## A. Model + thinking as ACP config options
Advertise pi's models + thinking levels so clients can switch them per session.

- On `session/new`, after spawn: `pi.request(GetAvailableModels)` → `response.data.models`
  (array of pi `Model{ id, name, provider, reasoning, contextWindow, ... }`). Also
  `pi.request(GetState)` → current `model`, `thinkingLevel`.
- Return them on `NewSessionResponse` (schema::v1):
  - `NewSessionResponse::new(session_id).models(Some(SessionModelState{ available_models, current_model_id }))`
    — models as `ModelInfo { model_id: "<provider>/<id>", name, description }`.
  - Thinking levels via `.config_options(Some(vec![SessionConfigOption{...select...}]))` OR the
    modes field — check `SessionModelState`/`SessionConfigOption`/`SessionModeState` shapes in
    `schema-1.4.0/src/v1/client.rs` and mirror how Zed's `acp.rs` reads them (config_options is
    the modern path; `modes` is the back-compat path for thinking levels).
- Handle inbound `SetSessionModelRequest` (add an `on_receive_request` handler): split
  `provider/id` and `pi.request(SetModel{provider, modelId})`. Handle
  `SetSessionConfigOptionRequest`/`SetSessionModeRequest` for thinking level →
  `pi.request(SetThinkingLevel{level})`. (`Command::SetModel`/`SetThinkingLevel` already exist.)
- Emit `SessionUpdate::CurrentModeUpdate`/`ConfigOptionUpdate` if pi reports a change.

## B. session/load + resume
- Advertise `agent_capabilities.load_session = true` (+ resume capability) in `initialize`.
- Add `on_receive_request` for `LoadSessionRequest{ session_id, cwd, ... }`: spawn pi with
  `--session <file>` (map ACP sessionId → pi session file via a small on-disk map, e.g.
  `~/.pi/pi-acpinator/session-map.json`, OR reuse pi's own `--session <id>`). Per the ACP spec,
  **replay history**: after loading, stream the prior turns to the client as `session/update`
  notifications *before* responding to the load request (use pi `get_messages`; add
  `Command::GetMessages`). Then respond `LoadSessionResponse`.
- Resume similarly (`ResumeSessionRequest`).

## C. Auth methods (terminal auth)
- In `initialize`, advertise a terminal `AuthMethod` (pi has no `login` subcommand; auth is per-
  provider keys in `~/.pi/agent`). Mirror how bridges expose "Terminal Auth" so the client shows
  an Authenticate banner that launches `pi` in a terminal. Add an `AuthenticateRequest` handler
  (may be a no-op that returns success, since pi auth is external).

## D. Cancel correctness (fix from M0)
- `session/cancel` sends pi `abort`. Per ACP, the pending `session/prompt` MUST then resolve with
  `StopReason::Cancelled` (not EndTurn). In `run_prompt`: track whether an abort was requested
  for this session (flag on `Session`, set by the cancel handler), and when pi emits `agent_end`
  after an abort, return `StopReason::Cancelled`. Reset the flag per turn.

## E. Multiple sessions per process (optional)
- Current design spawns one pi per session (fine). If memory matters, keep as-is; do NOT
  multiplex a single pi across ACP sessions (pi session state is per-process).

## Tests
- Pure: `provider/id` slug split; model/thinking option construction; cancel→Cancelled logic.
- Component: SetModel/SetThinkingLevel handlers issue the right pi commands; load replays
  history notifications before responding.
- Live: switch model via the client; cancel a long prompt and assert `stopReason: cancelled`.

## Effort: ~1–1.5 days (session/load history replay is the meatiest).
