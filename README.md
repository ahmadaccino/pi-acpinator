# pi-acpinator

A fast, tiny [ACP](https://agentclientprotocol.com) (Agent Client Protocol) adapter for the
[`pi`](https://github.com/earendil-works/pi) coding agent, written in Rust.

It speaks ACP JSON-RPC 2.0 over stdio to any ACP client (e.g. the Zed editor) and drives
`pi --mode rpc` underneath. Because it spawns `pi` rather than embedding a JS runtime, the
adapter is a small native binary — low memory, fast startup.

## Status

Working today (live-verified against real pi):

- `initialize` handshake
- `session/new` — spawns and supervises a `pi --mode rpc` process per session; advertises
  pi's thinking levels (`off`..`xhigh`) as ACP session modes
- `session/prompt` — streams pi's assistant output and reasoning back as ACP
  `agent_message_chunk` / `agent_thought_chunk`, coalescing delta bursts into fewer frames
- tool calls — pi tool execution maps to `tool_call` / `tool_call_update` (kind, status,
  output, cwd-resolved locations)
- `session/request_permission` — a bundled pi extension gates tools via `ctx.ui.confirm`,
  surfaced to the client as ACP permission requests (scope: `off` | `mutating` | `all`)
- `session/set_mode` — switches pi's thinking level
- `session/cancel` — aborts the turn and resolves it with `StopReason::Cancelled`
- fails a turn loudly if pi exits before completing it

Measured: ~3 ms cold start, ~4 MiB idle bridge RSS, 1.9 MB stripped binary.

Deferred (see `plans/`): model selection via config options, `session/load` history replay,
terminal auth advertisement, bounded-channel backpressure.

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
cargo test          # unit tests (framing, event classification, translation, coalescing)
node scripts/bench.mjs   # cold-start + idle RSS
RUST_LOG=debug cargo run
```

## License

MIT
