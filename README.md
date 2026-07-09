# pi-acpinator

A fast, tiny [ACP](https://agentclientprotocol.com) (Agent Client Protocol) adapter for the
[`pi`](https://github.com/earendil-works/pi) coding agent, written in Rust.

It speaks ACP JSON-RPC 2.0 over stdio to any ACP client (e.g. the Zed editor) and drives
`pi --mode rpc` underneath. Because it spawns `pi` rather than embedding a JS runtime, the
adapter is a small native binary — low memory, fast startup.

## Status

Working today (live-verified against real pi):

- `initialize` handshake; advertises `load_session` + a `pi` auth method
- `session/new` — spawns and supervises a persistent `pi --mode rpc` session; advertises
  pi's thinking levels (`off`..`xhigh`) as session modes and pi's models as a config option
- `session/prompt` — streams assistant output + reasoning as `agent_message_chunk` /
  `agent_thought_chunk`, coalescing delta bursts into far fewer frames
- tool calls — pi tool execution maps to `tool_call` / `tool_call_update` (kind, status,
  output, cwd-resolved locations); `write`/`edit` surface as structured diffs
- `session/request_permission` — a bundled pi extension gates tools via `ctx.ui.confirm`,
  surfaced to the client as ACP permission requests (scope: `off` | `mutating` | `all`)
- `session/set_mode` — switches pi's thinking level
- `session/set_config_option` — switches pi's model
- `session/load` — resumes a persisted session and replays its history to the client
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
RUST_LOG=debug cargo run
```

## License

MIT
