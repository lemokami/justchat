# Product Requirements Document (PRD)

**Project Name:** KiroUI (Native GPUI Client for Kiro CLI)
**Primary Objective:** Build a blazing-fast, cross-platform desktop application using Rust and the GPUI framework (`gpui.rs`) that acts as a visual interface for `kiro-cli`. It will communicate with the CLI via the Agent Client Protocol (ACP) over standard I/O, allowing developers to chat, execute tasks, and manage AI-driven workflows seamlessly without living entirely in the terminal.

---

## 1. Architecture & Tech Stack

### Core Technologies

* **UI Framework:** [GPUI](https://www.gpui.rs/) – Zed’s GPU-accelerated UI framework for Rust. Chosen for zero-latency rendering and low resource consumption.
* **Language:** Rust (Edition 2021).
* **Async Runtime:** `tokio` (for non-blocking standard I/O and JSON-RPC stream handling).
* **Serialization/Protocol:** `serde` and `serde_json` for parsing ACP JSON-RPC 2.0 messages.
* **Rich Text:** `pulldown-cmark` for parsing markdown from the AI into GPUI text elements.

### System Diagram

```text
┌───────────────────────────────────────┐          ┌───────────────────────┐
│              KiroUI (GPUI)            │          │      kiro-cli         │
│                                       │          │                       │
│  ┌────────────┐       ┌────────────┐  │  stdin   │  ┌─────────────────┐  │
│  │ GPUI Event │ ────► │ ACP Client │ ─┼────────► │  │                 │  │
│  │    Loop    │       │   (Tokio)  │  │          │  │   ACP Server    │  │
│  └────────────┘       └────────────┘  │  stdout  │  │   (Headless)    │  │
│        ▲                     │        │ ◄────────┼─ │                 │  │
│        │ cx.notify()         │        │          │  └─────────────────┘  │
│        ▼                     ▼        │          └───────────────────────┘
│  ┌────────────┐       ┌────────────┐  │
│  │ View State │ ◄──── │ JSON-RPC   │  │
│  │  (Model)   │       │ Parser     │  │
│  └────────────┘       └────────────┘  │
└───────────────────────────────────────┘

```

---

## 2. ACP Integration & Subprocess Management

The application must spawn and manage `kiro-cli acp` as a headless subprocess.

### 2.1 Subprocess Lifecycle

* **Initialization:** Upon app launch, use `tokio::process::Command` to execute `kiro-cli acp`.
* **Environment:** Pass necessary environment variables (e.g., `KIRO_API_KEY`) if token-based authentication is required, bypassing the browser-based login flow for programmatic access.
* **Termination:** Bind the subprocess lifecycle to the GPUI application lifecycle. Ensure graceful termination (sending a shutdown signal or closing stdin) when the user closes the window.

### 2.2 JSON-RPC 2.0 Transport

* **Stream Framing:** Implement a buffer that reads from `stdout` line-by-line (or via `Content-Length` headers, depending on the exact ACP transport specification used by Kiro).
* **Message Types:**
* **Requests:** `initialize`, `sessions/create`, `client/runTool`
* **Notifications:** Streaming text chunks, agent state updates (e.g., "thinking").
* **Responses:** Acknowledgements of executed tasks or UI inputs.



---

## 3. GPUI Application Structure

GPUI operates on a reactive paradigm using `ModelContext` (for data) and `ViewContext` (for UI). The app must be structured to prevent blocking the main render thread.

### 3.1 State Management (The Model)

```rust
struct AppState {
    sessions: Vec<Session>,
    active_session_id: Option<String>,
    acp_client: Arc<AcpClient>, // Tokio channel sender to the background task
}

struct Session {
    id: String,
    messages: Vec<Message>,
    agent_status: AgentStatus, // Idle, Thinking, Executing Tool, Awaiting Approval
}

struct Message {
    role: Role, // User, Agent, System
    content: String,
    tool_calls: Vec<ToolCall>,
}

```

### 3.2 View Hierarchy (The Render Trait)

The UI will be built using GPUI's element builder pattern (`div()`, `flex()`, `text()`).

* **`WorkspaceView`:** The root view containing a horizontal split.
* **`SidebarView`:** Displays a list of past sessions. Expanding a session loads its history.
* **`ChatAreaView`:** * **`MessageList`:** A scrollable container holding `MessageBubble` views. Needs to handle rapid updates efficiently as streaming chunks arrive.
* **`MessageBubble`:** Renders user text or agent markdown. If the agent is using a tool, it embeds a `ToolExecutionBlock`.
* **`InputEditor`:** A text area bound to key events (`Enter` to send, `Shift+Enter` for new line).



---

## 4. Core Workflows (One-Shotted)

### Workflow 1: Sending a Prompt

1. User types a prompt and hits `Enter`.
2. GPUI handles the event, pushes a `Message` (Role: User) to the `AppState`.
3. `AppState` sends an asynchronous request via `AcpClient` over `stdin` to `kiro-cli`.
4. The UI immediately clears the input field and displays a skeleton loader or "Kiro is thinking..." indicator.

### Workflow 2: Streaming Responses

1. The Tokio background task reads `stdout` from `kiro-cli`.
2. As JSON-RPC stream notifications arrive, the background task sends them to the main thread via an `mpsc` channel.
3. The main thread appends the text chunks to the active `Message` in `AppState` and calls `cx.notify()`.
4. GPUI triggers a highly optimized re-render of the `ChatAreaView`, displaying the text smoothly.

### Workflow 3: Tool Execution & Interactivity (Crucial for Kiro)

`kiro-cli` is capable of autonomous action (reading files, executing bash). The UI must handle this gracefully.

1. `kiro-cli` sends a `client/runTool` request (e.g., `execute_command: "npm run build"`).
2. The UI renders a `ToolExecutionBlock` inside the chat flow.
3. **Security Gate:** If the command is destructive or requires elevation (like `sudo`), GPUI displays an inline approval dialog (Approve / Reject buttons).
4. Upon approval, the UI sends the confirmation back over ACP.
5. Live terminal output from the tool execution is streamed back from the ACP server and rendered in a collapsible code block in the GPUI window.

---

## 5. Security & Configuration

* **Permissions Engine:** Implement a settings model that allows users to configure `autoApprovePermissions`.
* *Allow All:* Automatically executes non-destructive read/write operations.
* *Ask:* Pauses the agent and requires a manual click in the UI for operations like terminal execution or modifying system files.


* **Working Directory Context:** Ensure the `initialize` payload sent to the ACP server correctly passes the absolute path of the user's current project workspace so Kiro is context-aware.
* **Environment Injection:** Allow the user to specify custom path variables or shell contexts in an `acp_settings.json` file so that `kiro-cli` executes within the correct Node/Rust/Python environment.

---

## 6. Implementation Milestones

* **Phase 1: Protocol Skeleton**
* Setup GPUI boilerplate.
* Implement the Tokio subprocess wrapper for `kiro-cli acp`.
* Establish two-way JSON-RPC logging to verify the handshake.


* **Phase 2: UI Foundation**
* Build the `WorkspaceView`, `SidebarView`, and `ChatAreaView`.
* Implement basic text sending and static receiving.


* **Phase 3: Streaming & Markdown**
* Implement the streaming parser.
* Integrate `pulldown-cmark` to format code blocks, bold text, and lists correctly in GPUI.


* **Phase 4: Tool & Permission Handling**
* Implement the visual state for `client/runTool`.
* Add the security approval loop (Approve/Deny UX).


* **Phase 5: Polish**
* Handle Edge cases: `kiro-cli` crashing, missing tokens, network timeouts.
* Add syntax highlighting to code blocks inside the chat.
