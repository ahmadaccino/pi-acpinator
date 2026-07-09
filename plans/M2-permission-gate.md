# M2 — Permission gate via `session/request_permission` (the differentiator)

pi has **no native per-tool permission surface** (only project trust). But its `tool_call`
extension hook runs *before* a tool and can veto (`{ block: true, reason }`), and can call
`ctx.ui.confirm(...)` which, in `--mode rpc`, emits an `extension_ui_request` on stdout and
awaits a matching `extension_ui_response` on stdin. We exploit this to implement real ACP
`session/request_permission` — the capability the existing bridges lack.

## Flow
```
pi tool_call hook --confirm--> extension_ui_request(confirm) --stdout--> pi-acpinator
   --ACP session/request_permission--> client --decision--> pi-acpinator
   --extension_ui_response{confirmed}--> pi --allow/block--> tool runs or is vetoed
```

## Pieces

### 1. Bundled pi extension (TypeScript), embedded in the binary
- `assets/permission-gate.ts` — a pi extension: `import type { ExtensionAPI }`; on load
  `pi.registerCommand("acp-permission-gate", {...})` (sentinel for a load handshake); hook
  `pi.on("tool_call", ...)`: for tools NOT in a read-only allowlist (`read,grep,find,ls,glob`,
  configurable via env), call `ctx.ui.confirm("Run <tool>?", <detail>)`; return
  `{ block:true, reason:"Denied" }` when `!ctx.hasUI` or not confirmed, else `undefined`.
  (This mirrors the logic proven to work against pi in earlier testing.)
- Embed with `include_str!("../assets/permission-gate.ts")`. At `start_session`, write it to a
  temp file (`std::env::temp_dir()`/uuid, or the session dir) and pass `pi -e <tmpfile>`.
  Delete the temp file when the session is dropped (store the path on `Session`, remove in Drop).

### 2. Spawn wiring (src/main.rs `start_session`)
- Add `-e <permission-gate temp path>` to the pi args when permission gating is enabled
  (default on; gate scope from an env like `PI_ACPINATOR_APPROVAL=mutating|all|off`, passed to
  the extension via `T3-free` env var e.g. `PI_ACP_APPROVAL_MODE`).
- **Load handshake:** after spawn, `pi.request(GetCommands)` (add `Command::GetCommands`) and
  verify the `acp-permission-gate` command is present; if gating is required and it's absent,
  fail `session/new` (fail closed) — don't advertise an ungated session.

### 3. Bridge extension_ui ↔ ACP (src/main.rs `run_prompt` loop + client.rs)
- `PiIncoming` already yields `Incoming::ExtensionUiRequest(ExtensionUiRequest{id,method,title,message,options})`.
- In `run_prompt`, on `Incoming::ExtensionUiRequest` with `method=="confirm"`:
  - Build an ACP `RequestPermissionRequest`:
    `RequestPermissionRequest { session_id, tool_call: ToolCallUpdate::new(<id>, ToolCallUpdateFields::new().title(title)), options: vec![allow, reject] }`
    where options are `PermissionOption::new(PermissionOptionId::new("allow"), "Allow", PermissionOptionKind::AllowOnce)` and a `RejectOnce` (+ optionally AllowAlways/RejectAlways).
  - Send it to the client and await the decision (see async caveat below).
  - Map `RequestPermissionResponse.outcome`:
    `RequestPermissionOutcome::Selected(SelectedPermissionOutcome{option_id})` → confirmed =
    option_id starts with "allow"; `Cancelled` → confirmed=false.
  - Reply to pi: `session.pi.respond_ui(ExtensionUiResponse{ id, confirmed:Some(bool), value:None, cancelled:None })`.
  - For `method` `select`/`input`/`editor` (from other pi extensions): map to ACP as best-effort
    (a `select` → request_permission-style options, or skip for v1). `notify`/`setStatus`/etc.
    are fire-and-forget — ignore.

### Async caveat (important)
`conn.send_request(req).block_task().await` may deadlock **inside an SDK handler future**
(documented in the ACP crate / observed in Zed). `run_prompt` runs inside the `session/prompt`
handler. Use the non-blocking form: `conn.send_request(req).on_receiving_result(|r| { tx.send(r) })`
bridged through a `tokio::sync::oneshot`, then `rx.await` — i.e. don't `block_task()` inside the
handler. Encapsulate as `async fn request_from_handler<R>(conn, req) -> Result<R>`. Verify the
exact method names on `SentRequest` in the crate source (`on_receiving_result` / `block_task`).

### Teardown
On session stop / prompt end, settle any in-flight permission (respond `cancelled:true` to pi)
so pi's hook doesn't hang. Track pending permission ids on `Session`.

## Edge cases
- Extension fails to load (handshake absent) → fail closed (§2) when gating required.
- Client denies → pi hook blocks the tool; the turn continues; surface the block as a
  `tool_call_update(Failed)` if pi emits one.
- Extension temp file cleanup on crash (Drop + `kill_on_drop` already set).

## Tests
- Pure: allowlist decision + option→confirmed mapping.
- Component: fake pi emits an `extension_ui_request(confirm)`; assert bridge sends ACP
  `session/request_permission`; feed a Selected(allow) response; assert `extension_ui_response
  {confirmed:true}` is written back to the fake pi.
- Live: prompt that triggers `bash`; confirm the client gets a permission request and that
  allow/deny actually gates the tool.

## Effort: ~1 day (async request-from-handler plumbing is the tricky part).
