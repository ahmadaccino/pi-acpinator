# pi-acpinator

A fast, tiny [ACP](https://agentclientprotocol.com) (Agent Client Protocol) adapter for the
[`pi`](https://github.com/earendil-works/pi) coding agent, written in Rust.

It speaks ACP JSON-RPC 2.0 over stdio to any ACP client (e.g. the Zed editor) and drives
`pi --mode rpc` underneath. Because it spawns `pi` rather than embedding a JS runtime, the
adapter is a small native binary — low memory, fast startup.

## Why pi-acpinator?

Every other pi ACP adapter is a Node.js process. `pi-acpinator` is a single native Rust binary,
so the adapter layer costs almost nothing. Measured head-to-head at the ACP `initialize` stage
(`node scripts/bench-compare.mjs`, Apple M-series, medians):

| Adapter | Architecture | Cold start | Resident memory |
|---|---|---|---|
| **pi-acpinator** | **Rust native binary, spawns `pi`** | **~4 ms** | **~3 MiB** |
| [`pi-acp`](https://github.com/svkozak/pi-acp) (svkozak, 472★) | Node, spawns `pi` | ~100 ms | ~76 MiB |
| [`@victor-software-house/pi-acp`](https://github.com/victor-software-house/pi-acp) | Bun, **embeds the pi SDK** in a persistent daemon | ~22 ms¹ | **~194 MiB**¹ |

That's roughly **20× faster startup and ~24× less memory** than the most popular adapter, and
**~60× less memory** than the daemon-based one — all before `pi` itself is even launched. The
reasons it wins across the board:

- **No runtime tax.** No Node/V8/Bun to boot or keep resident — the Node adapter carries ~76 MiB
  and ~100 ms of runtime overhead *per process*, and the Bun daemon sits at ~194 MiB resident,
  before either does any work. pi-acpinator is ~3 MiB and starts in single-digit milliseconds.
- **Process isolation, not embedding.** Like svkozak's, it spawns `pi` as a child (clean
  lifecycle, `kill_on_drop`). The `victor`/`harms-haus` adapters embed the entire pi SDK
  *inside* the adapter; `victor` runs it in a persistent background daemon (the ~194 MiB above).
- **Lower streaming overhead.** Delta bursts are coalesced (~45× fewer ACP frames in the
  benchmark), both pipes are bounded for backpressure, and a dropped `pi` fails the turn loudly
  instead of hanging.
- **More capability, not less.** It implements `session/request_permission` (a real tool
  permission gate) and a separate `agent_thought_chunk` reasoning stream — both of which the
  Node adapters document as *not* implemented — plus tool diffs, thinking modes, model
  selection, and `session/load` history replay.
- **Trivial to ship and run.** One ~2 MB static binary (musl included), no runtime dependency:
  `npx pi-acpinator`, `cargo install pi-acpinator`, or a prebuilt release binary.

¹ `@victor-software-house/pi-acp` requires the Bun runtime and a persistent background daemon
that embeds the full pi SDK. The ~194 MiB is that daemon at idle (no session); the ~22 ms cold
start is the thin client connecting to an *already-warm* daemon — the first invocation also pays
the daemon spawn. Measured with `node scripts/bench-compare.mjs` (svkozak) and its documented
Bun/daemon procedure (victor).

## Status

Working today (live-verified against real pi):

- `initialize` handshake; advertises `load_session` (no auth method — pi provider keys are
  configured externally via the pi CLI)
- `session/new` — spawns and supervises a persistent `pi --mode rpc` session; advertises
  pi's thinking levels (`off`..`xhigh`) as session modes and pi's models as a config option
- `session/prompt` — streams assistant output + reasoning as `agent_message_chunk` /
  `agent_thought_chunk`, coalescing delta bursts into far fewer frames; forwards image
  content blocks to pi
- tool calls — pi tool execution maps to `tool_call` / `tool_call_update` (kind, status,
  output, cwd-resolved locations); `write`/`edit` surface as structured diffs
- `session/request_permission` — a bundled pi extension gates tools via `ctx.ui.confirm`,
  surfaced to the client as ACP permission requests (scope: `off` | `mutating` | `all`)
- `session/set_mode` — switches pi's thinking level
- `session/set_config_option` — switches pi's model (validated; a bad model / missing key
  surfaces as an error)
- `session/load` — resumes a persisted session and replays its history to the client;
  reuses an already-live session instead of spawning a second pi
- `session/cancel` — aborts the turn and resolves it with `StopReason::Cancelled`
- fails a turn loudly if pi exits before completing it

Measured (deterministic bench, `node scripts/bench.mjs`): 1.95 MiB binary, ~3 ms cold start,
~1M deltas/s with ~45x delta coalescing, ~5.6 MiB peak RSS. Both pi stdin and the event
stream are bounded, so a slow peer applies backpressure instead of buffering without limit.

## Install

```bash
# prebuilt binary via npm (no Rust toolchain needed)
npx pi-acpinator

# or from source
cargo install pi-acpinator     # from crates.io
cargo build --release          # from a checkout -> target/release/pi-acpinator
```

## Use with an ACP client (Zed)

```json
"agent_servers": {
  "pi": {
    "type": "custom",
    "command": "npx",
    "args": ["-y", "pi-acpinator"],
    "env": {}
  }
}
```

Or point `command` at a `cargo build --release` binary. Set `PI_ACPINATOR_APPROVAL` to
`off` | `mutating` (default) | `all` to control tool permission prompts.

Requires `pi` on `PATH` (`npm install -g @earendil-works/pi-coding-agent`), configured with
your model provider / API key.

## Develop

```bash
cargo test                    # unit + transport tests (framing, translation, coalescing, correlation)
node scripts/component-test.mjs   # end-to-end component test against a scripted fake pi (no model)
node scripts/bench.mjs            # performance benchmarks (deterministic)
node scripts/bench-compare.mjs    # head-to-head vs other pi ACP adapters (installs them)
RUST_LOG=debug cargo run
```

## License

MIT
