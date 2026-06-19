# KiroUI — Architecture

A native desktop client for [`kiro-cli`](https://kiro.dev) that speaks the
[Agent Client Protocol](https://agentclientprotocol.com) (ACP, JSON-RPC 2.0 over
stdio). This document describes how the app is structured, how data flows, the
key design decisions, and how to put a different UI (Electron, Tauri, web, …) on
top of the same core.

---

## 1. Goals & shape

- **Native, fast UI** with streaming responses, Markdown, tool visualization,
  and a permission/approval flow.
- **Strict separation** between the protocol/runtime, the application state, and
  the rendering layer. No GPUI types leak below the UI crate; the protocol layer
  knows nothing about the UI.
- **Headless testability**: everything except pixel rendering is unit/integration
  tested without launching a window or the real `kiro-cli`.

The result is a **3-crate Cargo workspace**:

| Crate | Depends on | Role | Tests |
|-------|-----------|------|-------|
| `kiro_acp` | — (protocol crates) | Subprocess + ACP client + thread bridge + settings + fs/terminal handlers | 21 |
| `kiro_core` | `kiro_acp` | Framework-free `AppState`, Markdown parser, syntax tokenizer | 24 |
| `kiro_ui` | `kiro_core`, `gpui` | GPUI window, views, event pump, rendering | — |

---

## 2. Runtime topology

```
┌─ GPUI main thread ──────────────────┐         ┌─ protocol thread (current-thread tokio + LocalSet) ─┐
│ kiro_ui::WorkspaceView              │ Command │ kiro_acp::bridge                                     │
│   holds Entity<AppState>            │ ──────► │   command loop                                       │
│   event pump (cx.spawn, ~8ms poll)  │         │   ClientSideConnection (ACP, !Send futures)          │  stdio   ┌──────────┐
│ kiro_core::AppState                 │         │     ▲          │                                     │ ───────► │ kiro-cli │
│   apply_event(Event) + cx.notify()  │ ◄────── │     │ KiroClient (request_permission, fs/*, term/*)  │ ◄─────── │   acp    │
└──────────────────────────────────────┘  Event │     └── idshim (string↔int JSON-RPC ids)             │          └──────────┘
                                                 │           └── Subprocess (spawn/kill, piped stdio)   │
                                                 └──────────────────────────────────────────────────────┘
```

- The ACP runtime futures are `!Send`, so they live entirely on a **dedicated OS
  thread** running a single-threaded tokio runtime + `LocalSet`.
- The UI thread and protocol thread communicate **only** through two channels
  carrying `Command` (UI → protocol) and `Event` (protocol → UI). This is the
  single seam of the whole application.

---

## 3. The Command / Event contract

This enum pair (in `kiro_acp::protocol`) is the entire public surface between
"a UI" and "the agent runtime".

```text
Command  (UI → protocol)                Event  (protocol → UI)
─────────────────────────               ───────────────────────────────
CreateSession                           Connected { protocol_version, supported, load_session }
SendPrompt { session_id, text,          SessionCreated { session_id }
             attachments }              ModelsAvailable { session_id, current, models }
Cancel { session_id }                   MessageChunk { session_id, text }
PermissionDecision { request_id,        ThoughtChunk { session_id, text }
                     option_id }         ToolCall { session_id, id, title, kind, status }
SetModel { session_id, model_id }       ToolCallUpdate { session_id, id, status, output }
Shutdown                                Plan { session_id, entries }
                                        PermissionRequested { request_id, session_id, title, options }
                                        TurnEnded { session_id, stop_reason }
                                        Error { message }
                                        Disconnected { message }
```

Everything the UI can do is a `Command`; everything it needs to render is an
`Event`. The enums are plain data (`Debug + Clone`), deliberately free of
protocol types.

---

## 4. Crate-by-crate

### `kiro_acp` — protocol & runtime (headless)

| File | Responsibility |
|------|----------------|
| `subprocess.rs` | Spawn `kiro-cli acp`, pipe stdio, drain stderr to logs, graceful shutdown (`kill_on_drop`). |
| `idshim.rs` | Transport shim that rewrites **string** JSON-RPC ids (used by kiro) to **integer** ids (assumed by the `agent-client-protocol 0.4.x` crate) and back. Without it, agent-initiated requests fail to parse. |
| `protocol.rs` | The `Command`/`Event` enums, `KiroClient` (implements the ACP `Client` trait: `request_permission`, `fs/read|write_text_file`, `terminal/*`), and helpers `initialize`, `create_session`, `send_prompt`, `set_model`. |
| `bridge.rs` | `start(BridgeConfig) -> BridgeHandle`. Spawns the protocol thread, runs the command loop, watches for subprocess death. |
| `settings.rs` | `acp_settings.json` model: auto-approve policy (`ask`/`allow_all`), cwd, env. |
| `terminals.rs` | `TerminalManager` for the client-side `terminal/*` methods (spawn, capture output, wait, kill, release). |
| `attachment.rs` | Classify files (image vs other), MIME detection, `file://` URIs, base64 encoding. |
| `bin/mock_agent.rs` | A scripted ACP agent used by tests/examples (flags for tools, permission, crash, bad version). |

### `kiro_core` — application logic (no GPUI)

| File | Responsibility |
|------|----------------|
| `app_state.rs` | `AppState`: the command/event state machine. Holds sessions, messages (with streamed content, thoughts, tool calls, attachments), connection status, staged attachments, model list. `apply_event` mutates state; methods (`submit_input`, `set_model`, `decide_permission`, …) emit commands. |
| `markdown.rs` | Block-level Markdown parser (`parse(&str) -> Vec<Block>`). |
| `highlight.rs` | Dependency-free syntax tokenizer for fenced code blocks. |

### `kiro_ui` — GPUI presentation

| File | Responsibility |
|------|----------------|
| `main.rs` | Entry point: load settings → `bridge::start` → open window → create `Entity<AppState>` + `WorkspaceView`. |
| `workspace.rs` | Root view: sidebar (sessions), chat (message bubbles, tool blocks, permission dialog), input (+ attachments, attach button), model dropdown, status bar, and the **event pump** (`cx.spawn` loop draining events into the entity). |
| `markdown.rs` | Renders parsed Markdown blocks (with highlighted code) into GPUI elements. |
| `theme.rs` | Color palette. |

---

## 5. Lifecycle of a prompt turn

1. User types and hits Enter → `WorkspaceView::on_key` → `AppState::submit_input()`.
2. `submit_input` pushes a User message + a streaming Agent placeholder, sets the
   session to *Thinking*, and sends `Command::SendPrompt`.
3. The bridge's command loop spawns the prompt task → `conn.prompt(...)`.
4. `kiro-cli` streams `session/update` notifications (thoughts, message chunks,
   tool calls). `KiroClient::session_notification` maps each into an `Event`.
5. If a tool needs approval, kiro sends `session/request_permission` → surfaces as
   `Event::PermissionRequested`; the UI shows Allow/Reject → `Command::PermissionDecision`
   resolves the pending request (unless `allow_all` auto-approves non-destructive ops).
6. The event pump drains events every ~8 ms, calls `AppState::apply_event`, and
   `cx.notify()` re-renders. Chunks accumulate into the active message.
7. The turn ends → `Event::TurnEnded` clears the thinking indicator and finalizes
   tool statuses.

---

## 6. Key design decisions

- **Trait-based ACP `0.4.x`, not `0.14`.** The newer crate replaced the
  `Client`/`Agent` trait + `ClientSideConnection` model with a builder API that's
  awkward for a long-lived streaming client. `0.4.x` is wire-compatible with
  kiro's protocol v1.
- **JSON-RPC id shim.** kiro uses string ids (spec-legal); the crate assumes
  integers. The shim makes them interoperate without forking the crate.
- **Dedicated protocol thread.** ACP futures are `!Send`; isolating them keeps the
  UI responsive and the threading model simple.
- **GPUI-free core.** `kiro_acp` + `kiro_core` carry all logic and 45 tests;
  GPUI is a thin rendering shell over a tested state machine.
- **Lightweight syntax highlighter** instead of `syntect` — avoids heavy syntax/
  theme asset loading and is unit-testable.

---

## 7. Testing strategy

- `kiro_acp` integration tests drive a **scripted `mock_agent` binary** as a real
  subprocess (handshake, prompt round-trip, tool calls, permission loop, crash
  detection, missing binary). fs/terminal/permission handlers are unit-tested
  directly.
- `kiro_core` unit tests cover every `apply_event` transition, submit/attachment/
  model flows, the Markdown parser, and the syntax tokenizer.
- The `examples/` (`handshake`, `chat`, `bridge`) exercise the **real** `kiro-cli`
  from the terminal using the same `Command`/`Event` API the GUI uses.

---

## 8. Putting a different UI on top (Electron / Tauri / web)

**Is it easy? Yes — the architecture was built for it.** The UI only ever talks
to `kiro_acp::bridge` through the `Command`/`Event` enums. Any frontend that can
produce `Command`s and consume `Event`s is a drop-in replacement for `kiro_ui`.
There is exactly one thing to add for a cross-language frontend: a transport.

### Option A — Tauri (recommended)
Tauri keeps the **Rust core unchanged** and runs a web frontend (React/Svelte/…)
in the same process.
- Reuse `kiro_acp` + `kiro_core` as-is.
- Map Tauri **commands** to our `Command`s and Tauri **events** to our `Event`s.
- Drain `BridgeHandle::events` in a Tauri async task and `app.emit("acp-event", …)`.
- Effort: low. No second process, no protocol re-implementation. Add
  `#[derive(Serialize, Deserialize)]` to `Command`/`Event` (one-line change).

### Option B — Electron (or any web app over a socket)
Electron is a separate Node/Chromium process, so it needs an explicit IPC bridge.
Two sub-options:
1. **Wrap `kiro_acp` in a tiny server**: a Rust binary that exposes the bridge
   over a local WebSocket/stdio, serializing `Command`/`Event` as JSON. Electron
   connects to it. Reuses all of `kiro_acp`/`kiro_core`. Moderate effort.
2. **Talk ACP directly**: skip `kiro_acp` and have Electron use the official
   TypeScript ACP SDK to drive `kiro-cli acp` itself. You'd then re-implement (in
   TS) the things `kiro_acp` provides — the **id shim is NOT needed** (the TS SDK
   handles string ids), but you'd re-do settings, terminal handling, the
   permission flow, and state management. Higher effort, but no Rust at all.

### Option C — any native toolkit (egui, Slint, Qt via FFI, …)
Same as `kiro_ui`: depend on `kiro_core`, hold the `BridgeHandle`, pump events
into your widgets. Low effort for Rust toolkits; FFI for C++/Qt.

### What needs to change in this repo to support cross-process UIs
1. `#[derive(Serialize, Deserialize)]` on `Command`, `Event`, and the small value
   types (`Attachment`, `ModelOption`, `PermissionOptionInfo`). They're already
   plain data.
2. A thin transport binary (≈100 lines): `bridge::start` → forward stdin JSON →
   `Command`, forward `Event` → stdout JSON. That single binary serves Electron,
   web, Python, anything.

### Recommendation
- **Maximum native performance, all-Rust, single binary:** keep **GPUI**
  (current). Trade-off: GPUI is young and needs the macOS Metal toolchain to build.
- **Best balance of effort, ecosystem, and reuse:** **Tauri** — you keep the
  entire tested Rust core and get the mature web-UI ecosystem with almost no glue.
- **Only if you specifically need the Electron/Node ecosystem:** Electron via the
  WebSocket bridge (Option B1), so you still reuse the Rust core.

In short: the `Command`/`Event` seam means the agent runtime is already a
reusable "headless engine." GPUI is the best *native* choice; **Tauri is the best
choice if you want a web-based UI without throwing away the Rust core.**

---

## 9. Build & run

```bash
cargo run -p kiro_ui            # launch the desktop app
just test                       # all crates
just clippy                     # cargo clippy --all-targets -D warnings
cargo run -p kiro_acp --example chat -- "hello"   # headless, no GUI
```

Prerequisites: macOS + Xcode **Metal Toolchain**
(`xcodebuild -downloadComponent MetalToolchain`), Rust 1.96+, an authenticated
`kiro-cli`.
