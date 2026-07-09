# pi-acpinator — design plan

A fast, tiny [ACP](https://agentclientprotocol.com) (Agent Client Protocol) adapter for
the [`pi`](https://github.com/earendil-works/pi) coding agent. Speaks ACP JSON-RPC 2.0 over
stdio to any ACP client (editors, headless agent runners) and drives `pi` underneath.

Goals, in priority order: **low memory, fast startup, low streaming overhead, correct.**

## 0. Language decision: Rust (primary)

Rust is the ideal fit here and directly serves every goal:

- **Memory:** a native Rust process is ~1–5 MB RSS. Node bridges carry a ~40–80 MB baseline;
  SDK-embedding bridges run into the hundreds of MB.
- **Startup:** a static binary starts in ~1–5 ms with no module graph and no `npx`
  resolve/install cost.
- **Throughput:** `serde_json`/`simd-json` + `tokio` async stdio, minimal allocation per token.
- **First-class ecosystem:** the Zed-maintained [`agent-client-protocol`](https://crates.io/crates/agent-client-protocol)
  crate (v1.x, `Agent`/`Client` traits, `Stdio`/`Lines` transports, derive macros) plus
  `agent-client-protocol-tokio` for spawning child agents. Zed's own ACP layer is Rust.

**Why Rust is even possible here:** we *spawn* `pi --mode rpc` rather than embedding the pi
JS SDK, so the bridge never needs a JS runtime. The only JS artifact is a ~40-line pi
permission extension (§9), which pi itself executes — we embed it with `include_str!` and
write it to a temp file at spawn. The bridge itself is 100% Rust.

(A TypeScript implementation is kept as a fallback only if a pure-`npx`, no-binary
distribution ever becomes a hard requirement — but see §3 for how we solve `npx` in Rust.)

---

## 1. Why another bridge

Existing bridges either **spawn `pi --mode rpc`** (double process, double JSON translation,
Node overhead) or **embed the pi SDK** (heavy RSS, slow import). And neither implements ACP
**`session/request_permission`**, so neither can gate tool calls. `pi-acpinator` aims to be
the **leanest** bridge *and* the first with real per-tool permission requests.

---

## 2. Architecture

```
ACP client ──ACP JSON-RPC 2.0 (stdio)──> pi-acpinator (Rust) ──JSONL (stdio)──> pi --mode rpc
```

One small Rust process. It implements the ACP **Agent** role toward the client and a lean
pi **RPC client** toward the child. `pi` is spawned once per connection (or lazily per
session), supervised, and torn down on exit. No JS in the bridge.

---

## 3. Distribution

- **Prebuilt static binaries** per target (darwin arm64/x64, linux gnu+musl, windows) via
  GitHub Releases; `musl` for a fully static Linux binary.
- **`cargo install pi-acpinator`** for Rust users.
- **Thin `npx`-able npm shim** (`pi-acpinator`) that resolves the correct
  `@pi-acpinator/<platform>` prebuilt-binary package and execs it — the esbuild /
  `@ff-labs/fff-bin` pattern. This preserves the zero-install `npx pi-acpinator` UX that
  ACP clients (e.g. Zed's registry) expect, while the actual work is native.
- Optional single-file `bun build --compile` is **not** needed — the Rust binary already is one.

---

## 4. Crate layout
```
src/
  main.rs               # entrypoint: build ACP Agent over Stdio, run tokio runtime
  acp/
    agent.rs            # impl the `Agent` trait: initialize/new/prompt/cancel/load/...
    permission.rs       # session/request_permission flow
    translate.rs        # pi events -> ACP session/update notifications (hot path)
  pi/
    process.rs          # spawn/supervise `pi --mode rpc` (tokio::process), lifecycle
    jsonl.rs            # LF-only framing codec (the pi footgun fix)
    rpc.rs              # typed pi command writer + id->oneshot response correlation
    events.rs           # pi RPC event/command types (serde structs, mirrored)
  session.rs            # acp SessionId <-> pi session file map; pending permissions
  config.rs             # env/flags; permission scope; model/thinking config options
assets/
  permission-gate.ts    # embedded via include_str!; pi loads it with `-e`
tests/ ...
```

---

## 5. Dependencies (lean)
- `agent-client-protocol` (+ `-tokio`) — ACP Agent/Client, transports, JSON-RPC. Or, if we
  want zero-dep control on the hot path, hand-roll the small JSON-RPC subset; start with the
  crate for correctness and optimize the notification encoder if profiling warrants.
- `tokio` (rt-multi-thread or current-thread; **current-thread** likely enough and lighter),
  `tokio::process`, `tokio::io`.
- `serde` + `serde_json` (consider `simd-json` for the pi stdout parse hot path).
- `bytes` for framing buffers; `anyhow`/`thiserror` for errors; `tracing` (opt-in, stderr).
- Keep the tree small; prefer `current_thread` runtime + `LocalSet` to avoid Send churn.

---

## 6. Performance tactics

1. **Delta coalescing.** Buffer pi `text_delta`/`thinking_delta` and flush once per tick
   (or N bytes) as a single ACP `agent_message_chunk`/`agent_thought_chunk`. Fewer encodes +
   writes on fast streams; flush immediately on turn/tool boundaries.
2. **Framing without copies.** Read child stdout into a reused `BytesMut`; scan for `\n`,
   split, parse complete lines only. One parse per line, no regex.
3. **Backpressure.** Respect `AsyncWrite` readiness on both the ACP stdout and pi stdin;
   pause the source when the sink is full. Bounded channels between reader/translator/writer.
4. **No history in the bridge.** pi owns sessions/persistence; the bridge holds only a tiny
   per-session map → flat memory regardless of conversation length.
5. **Current-thread tokio + `LocalSet`.** Avoids multi-thread scheduler overhead and `Send`
   bounds for a single-connection stdio process.
6. **Precomputed message shapes.** Fill only variable fields per event; avoid re-allocating
   constant structure.

Target budgets (verify in §10): RSS **< ~10 MB**, cold start **< ~10 ms**, added
time-to-first-token **< ~2 ms** over raw `pi --mode rpc`, per-token overhead one small
parse + one small encode.

---

## 7. Correctness: JSONL framing (the known footgun)

pi's RPC is **strict LF-delimited JSONL**; naive readers that also split on `U+2028`/`U+2029`
(valid inside JSON strings) corrupt frames. `pi/jsonl.rs`: a `tokio_util::codec`-style
decoder that splits on `\n` only, strips a trailing `\r`, and buffers partial lines. Same
discipline on the ACP side (the crate's `Lines`/`Stdio` transport handles that end).

---

## 8. Protocol mapping

**pi → ACP:** `text_delta`→`agent_message_chunk` (coalesced); `thinking_delta`→
`agent_thought_chunk`; `tool_execution_start`→`tool_call` (kind/title from tool name);
`tool_execution_update`→`tool_call_update` (progress); `tool_execution_end`→`tool_call_update`
(completed/failed + result, structured diff for edit/write); `agent_end`→prompt result.
Emit tool **locations** (resolve relative paths vs cwd) for follow-along clients.

**ACP → pi:** `initialize`→advertise caps + terminal `authMethods`; `session/new`→spawn
`pi --mode rpc [--session]`, resolve id via `get_state`; `session/prompt`→`prompt` (or
`steer`/`follow_up` while streaming) + images; `session/cancel`→`abort`; `session/load`→
`--session <file>`; model/thinking (`configOptions`)→`set_model`/`set_thinking_level`.

---

## 9. Differentiator: `session/request_permission`

pi has no native per-tool permission surface, but its `tool_call` extension hook can veto
and can call `ctx.ui.confirm(...)` (surfaced over RPC as `extension_ui_request`).

1. Embed `assets/permission-gate.ts` in the binary (`include_str!`); at spawn, write it to a
   temp file and pass `pi -e <tmpfile>` (default-deny mutating tools, read-only allowlist).
2. When it calls `ctx.ui.confirm`, the bridge sees the RPC `extension_ui_request` and issues
   an ACP **`session/request_permission`** to the client.
3. The client's decision returns over ACP → bridge writes the pi `extension_ui_response` →
   the hook allows/blocks.
4. Advertise the capability in `initialize`; scope configurable (`off`/`mutating`/`all`).

This is generically useful for any ACP client and is the capability the other bridges lack.

---

## 10. Benchmark plan

Harness driving a scripted prompt (`--no-session`, cheap/mocked model) through: raw
`pi --mode rpc`, svkozak `pi-acp`, victor `pi-acp`, and us. Measure peak/steady RSS,
cold-start ms, time-to-first-token, tokens/sec, CPU per 10k tokens. Publish a table in the
README; CI perf smoke fails on regression vs our baseline.

---

## 11. Testing
- Unit: jsonl codec (U+2028 in strings, split chunks, CRLF), RPC id-correlation, each
  translate map, permission round-trip, coalescing flush semantics.
- Component: a scripted fake `pi --mode rpc` stub ↔ bridge ↔ fake ACP client (the crate's
  in-memory `Channel::duplex` makes this clean); assert emitted ACP frames + backpressure.
- Conformance: initialize handshake, session lifecycle, cancel, request_permission.

---

## 12. Milestones
1. **M0** — tokio + ACP `Agent` over `Stdio`, spawn pi, `initialize`, jsonl codec.
2. **M1** — `session/new` + `session/prompt` + message/thought chunks + tool_call + cancel.
3. **M2** — embedded permission gate + `session/request_permission`.
4. **M3** — tool locations, structured diffs, `session/load`/resume, config options, auth.
5. **M4** — coalescing, backpressure, benchmark harness + README numbers; npm shim + prebuilt
   binary release pipeline.

---

## 13. Open decisions
- Use the `agent-client-protocol` crate vs hand-rolled JSON-RPC (recommend: crate first,
  optimize hot path only if measured).
- `current_thread` vs multi-thread tokio (recommend current_thread + LocalSet).
- Spawn pi per-connection vs per-session (per-session enables multiple ACP sessions per
  process; start per-connection, add multiplexing in M3).
- Permission default scope (`mutating` is the safe, useful default).
- Package/bin name final: `pi-acpinator`.
