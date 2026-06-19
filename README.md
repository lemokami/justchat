# KiroUI

A native, GPU-accelerated desktop client for [`kiro-cli`](https://kiro.dev), built
in Rust with [GPUI](https://www.gpui.rs/). KiroUI spawns `kiro-cli acp` as a
headless subprocess and talks to it over the
[Agent Client Protocol](https://agentclientprotocol.com) (ACP, JSON-RPC 2.0 over
stdio), giving you a chat UI with streaming responses, markdown rendering, tool
visualization, and an approval flow — without living in the terminal.

## Workspace layout

| Crate | Role |
|-------|------|
| `kiro_acp` | Headless protocol/runtime: subprocess management, the ACP `Client` implementation, a thread bridge exposing a `Command`/`Event` channel API, a settings model, and client-side fs/terminal handlers. Fully unit/integration tested without any UI. |
| `kiro_core` | Framework-independent application state (`AppState` command/event state machine), a Markdown block parser, and a lightweight syntax-highlight tokenizer. Pure logic, unit tested without GPUI. |
| `kiro_ui` | The GPUI binary: window, views (sidebar, chat, input, tool blocks, permission dialog), the event pump, and Markdown/syntax rendering. |

The protocol thread (single-threaded tokio + `LocalSet`, because the ACP
futures are `!Send`) is isolated from the GPUI main thread; the two communicate
only through serializable `Command`/`Event` enums over channels.

## Prerequisites

- macOS with Xcode and the **Metal Toolchain** (`xcodebuild -downloadComponent MetalToolchain`) — required to compile GPUI's Metal shaders.
- Rust 1.96+ (pinned via `rust-toolchain.toml`).
- `kiro-cli` installed and authenticated (browser login).

## Running

```bash
cargo run -p kiro_ui          # launch the desktop app
just run                      # same, via the justfile
```

Headless protocol demos (no GUI):

```bash
cargo run -p kiro_acp --example handshake   # ACP initialize against real kiro-cli
cargo run -p kiro_acp --example chat -- "hello"
cargo run -p kiro_acp --example bridge -- "ping"
```

## Configuration

Drop an `acp_settings.json` next to where you launch the app (see
`acp_settings.json.example`):

```json
{
  "autoApprovePermissions": "ask",   // or "allow_all"
  "cwd": null,
  "env": [{ "name": "FOO", "value": "bar" }]
}
```

- `ask` (default): every tool permission request is surfaced to the UI.
- `allow_all`: non-destructive requests are auto-approved; destructive/elevated
  operations (e.g. `rm`, `sudo`, `delete`) still prompt.

## Development

```bash
just test         # cargo test (all crates)
just clippy       # cargo clippy --all-targets -D warnings
just fmt-check    # cargo fmt --all -- --check
```
