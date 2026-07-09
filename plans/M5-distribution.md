# M5 — Distribution

Ship `pi-acpinator` so ACP clients (esp. Zed) can run it with zero friction while keeping the
native performance win.

## A. Prebuilt binaries
- Cross-compile per target: `aarch64-apple-darwin`, `x86_64-apple-darwin`,
  `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl` (fully static),
  `aarch64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`.
- GitHub Actions matrix build → attach binaries to a GitHub Release. Strip symbols
  (`strip = true` in `[profile.release]`) and enable `lto = true`, `codegen-units = 1`,
  `opt-level = "z"` or `3` (measure size vs speed) for a small, fast binary.

## B. `cargo install`
- Publish to crates.io: `cargo install pi-acpinator`. Ensure the embedded `assets/permission-gate.ts`
  is bundled (it's `include_str!`, so it's compiled in — no extra packaging needed).

## C. npm shim (zero-install `npx` UX)
- Publish a thin `pi-acpinator` npm package whose `bin` is a tiny Node launcher that resolves
  the correct optional dependency `@pi-acpinator/<platform-arch>` (each an npm package wrapping
  one prebuilt binary) and `execFileSync`s it, forwarding stdio + args. This is the
  esbuild / `@ff-labs/fff-bin` pattern.
- Lets ACP clients use:
  ```json
  "agent_servers": { "pi": { "type": "custom", "command": "npx", "args": ["-y", "pi-acpinator"], "env": {} } }
  ```
- CI: on release, build binaries → publish `@pi-acpinator/<platform>` packages + the shim, all
  version-locked. Use npm trusted publishing (OIDC), no long-lived tokens.

## D. Docs
- README: install options (npx / cargo / prebuilt binary), the Zed `agent_servers` config, and
  the requirement that `pi` is installed + configured. Registry-style config note if ACP
  registries are targeted.

## E. Versioning / release
- semantic-release or manual tags; Conventional Commits. `npm pack --dry-run` +
  `cargo publish --dry-run` in CI before release.

## Effort: ~1 day (mostly CI matrix + npm shim wiring).
