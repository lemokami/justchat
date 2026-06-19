# JustChat

A native desktop chat client for **any [ACP](https://agentclientprotocol.com)
(Agent Client Protocol) agent** — bring your own agent (kiro‑cli, Gemini CLI,
Claude Code, or a custom one) and your own provider credentials, and chat with a
polished, app‑like UI.

It's built as a small Rust engine that speaks ACP (JSON‑RPC 2.0 over stdio) to a
headless agent subprocess, wrapped in a [Tauri](https://tauri.app) desktop shell
with a [Vue 3](https://vuejs.org) + [AI Elements](https://www.ai-elements-vue.com)
frontend.

## Features

- **Connect any ACP agent** — configure command, arguments, working directory,
  and per‑agent environment variables (API keys). Built‑in presets for Kiro CLI,
  Gemini CLI, and Claude Code; add your own. Connect / disconnect with one click.
- **Modern chat UX** — streaming responses, Markdown + syntax‑highlighted code,
  collapsible "thinking", tool‑call cards, and an inline permission/approval flow.
- **Persistent history** — chats are saved locally, listed in the sidebar,
  reopenable across restarts, with editable titles and delete.
- **Attachments** — send images and files to capable agents.
- **Model selection** & **slash commands** — surfaced from the agent over ACP.

## Architecture

A Cargo workspace with a clean split — the protocol engine is UI‑agnostic and
fully unit‑tested without any UI.

| Crate | Role |
|-------|------|
| `acpc_protocol` | Headless ACP engine: subprocess management, the ACP `Client` implementation, a thread bridge exposing a serializable `Command`/`Event` channel API, a JSON‑RPC id‑compat shim, settings, and fs/terminal handlers. |
| `acpc_core` | Framework‑independent state machine + Markdown/syntax helpers (pure logic, unit‑tested; legacy support crate). |
| `acpc_app` | The Tauri desktop app: Rust backend (commands + event forwarding + agent profiles + chat persistence) and the Vue 3 web frontend under `ui/`. |

The ACP futures are `!Send`, so they run on a dedicated thread (current‑thread
Tokio + `LocalSet`); the UI thread and protocol thread communicate only through
serializable `Command`/`Event` enums.

```
┌─ Webview (Vue 3 + AI Elements) ─┐  invoke()   ┌─ Tauri backend (Rust) ─┐  stdio  ┌────────────┐
│  chat UI, agents, history       │ ─────────►  │  acpc_protocol engine  │ ──────► │  ACP agent │
│                                 │ ◄─────────  │  (bridge + id‑shim)    │ ◄────── │ subprocess │
└─────────────────────────────────┘ acp-event   └────────────────────────┘         └────────────┘
```

## Prerequisites

- **macOS** with Xcode and the **Metal Toolchain**
  (`xcodebuild -downloadComponent MetalToolchain`) — required to compile Tauri's
  Metal shaders. (Other platforms work too; build on that OS.)
- **Rust 1.96+** (pinned via `rust-toolchain.toml`).
- **Node.js 18+** (to build the web frontend).
- **An ACP agent installed and on `PATH`** — e.g. `kiro-cli`, `gemini`, or
  `npx` (for the Claude Code adapter). JustChat is a *client*; it spawns the
  agent you choose.

## Develop

```bash
# 1. Build the web frontend (embedded into the app)
cd crates/acpc_app/ui && npm install && npm run build

# 2. Run the desktop app (debug)
cd ../../.. && cargo run -p acpc_app
```

Headless protocol demos (no GUI), useful for hacking on the engine:

```bash
cargo run -p acpc_protocol --example handshake     # ACP initialize against a real agent
cargo run -p acpc_protocol --example chat -- "hi"
cargo run -p acpc_protocol --example bridge -- "ping"
```

Tests / lint:

```bash
cargo test                                   # engine + core (45 tests)
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

## Build & distribute

The Tauri app *is* the `acpc_app` crate (no `src-tauri/` folder), so:

```bash
cd crates/acpc_app/ui && npm run build          # production frontend
cd ..                                           # crates/acpc_app
ui/node_modules/.bin/tauri build                # release build + .app + .dmg
```

Artifacts land in `target/release/bundle/` (`macos/JustChat.app`,
`dmg/*.dmg`). Builds are **per‑OS** (Tauri can't cross‑compile); use CI
(`tauri-action`) for multi‑platform releases.

> Distributed builds are **unsigned** by default — recipients must right‑click →
> Open (macOS Gatekeeper). For frictionless installs, codesign + notarize with an
> Apple Developer ID.

## Configuring agents

Click the **gear** in the header to manage agents. For each agent set the
`command`, `args`, optional working directory, and **environment variables**
(e.g. `GEMINI_API_KEY`, `ANTHROPIC_API_KEY`). Hit **Connect**. Presets:

| Agent | Command | Env |
|-------|---------|-----|
| Kiro CLI | `kiro-cli acp` | — |
| Gemini CLI | `gemini --experimental-acp` | `GEMINI_API_KEY` |
| Claude Code | `npx -y @zed-industries/claude-code-acp` | `ANTHROPIC_API_KEY` |

## Data & configuration

| Path | Contents |
|------|----------|
| `~/.acp-chatbot/agents.json` | Configured agents + active selection |
| `~/.acp-chatbot/chats/*.json` | Saved chat transcripts |
| `~/.acp-chatbot/workspace/` | Clean working dir used for context‑free chats |
| `acp_settings.json` (cwd) | Optional: `autoApprovePermissions` (`ask`/`allow_all`), `cwd`, `env` |

## Tech stack

Rust · `agent-client-protocol` · Tokio · Tauri 2 · Vue 3 · Tailwind CSS 4 ·
shadcn‑vue · AI Elements Vue · marked · highlight.js.

## License

Apache‑2.0.
