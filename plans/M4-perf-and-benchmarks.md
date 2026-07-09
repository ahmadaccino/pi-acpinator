# M4 — Performance, robustness, and benchmarks

Deliver the project's headline claims (low memory, fast startup, low streaming overhead) and
prove them.

## A. Delta coalescing (hot path)
- pi emits many small `text_delta`/`thinking_delta` events. Buffer them and flush **once per
  tick** (or at N bytes / on any non-text event / on turn/tool boundary) as a single ACP
  `agent_message_chunk`/`agent_thought_chunk`.
- Implement in `run_prompt` (or a small `Coalescer` in `acp/translate.rs`): accumulate a
  `String` per stream kind; flush via `tokio::task::yield_now`/`interval` boundary or when the
  next event is not the same kind. Flush immediately on `agent_end`/tool events.
- Make it opt-out (`PI_ACPINATOR_COALESCE=0`) for latency-sensitive callers.

## B. Backpressure
- ACP writes (`send_notification`) and pi stdin writes should not buffer unboundedly. The pi
  writer task already serializes via an unbounded mpsc — consider a **bounded** channel so a
  slow pi applies backpressure to the ACP side. For the ACP stdout side, rely on the crate's
  transport; if we ever write directly, honor `AsyncWrite` readiness.
- Bound `PiIncoming` similarly (bounded mpsc) so a slow ACP client slows the pi reader.

## C. Runtime + supervision
- Confirm `#[tokio::main(flavor = "current_thread")]` (already set) is sufficient; a single
  stdio connection doesn't need the multi-thread scheduler → lower memory. If any `Send` bound
  from the ACP crate forces multi-thread, document why.
- Process supervision: pi already `kill_on_drop`. Add: detect pi exit (await `child.wait()` in a
  task; store a shared "exited" flag) and, on unexpected exit mid-turn, resolve the prompt with
  an error / emit a session error, and mark the session dead so later requests fail cleanly.
- Reject in-flight `pi.request` waiters on stdout close (already done in `client.rs`).

## D. Memory hygiene
- No conversation history retained in the bridge (pi owns it). Keep only the per-session map +
  pending permission ids. Verify no unbounded `Vec` growth in `run_prompt`.

## E. Benchmark harness
- `benches/` or a `scripts/bench.mjs`: drive a scripted prompt (`--no-session`, a cheap/mock
  model or a fixed short prompt) through, and measure for each of:
  raw `pi --mode rpc`, `svkozak/pi-acp`, `@victor-software-house/pi-acp`, and `pi-acpinator`:
  - peak & steady RSS (e.g. `/usr/bin/time -l` on macOS, or sample `ps`),
  - cold-start ms (spawn → `initialize` response),
  - time-to-first-token, tokens/sec, CPU time per 10k tokens.
- Publish a table in the README. Add a CI perf-smoke that fails on regression vs our own
  recorded baseline (JSON in `benches/baseline.json`).
- Target budgets to confirm: RSS < ~10 MB, cold start < ~10 ms, added TTFT < ~2 ms vs raw pi.

## Tests
- Coalescer unit tests (flush boundaries, ordering vs tool events).
- Supervision: fake pi that exits mid-turn → prompt resolves with error, session marked dead.

## Effort: ~1 day + benchmark harness (~0.5 day).
