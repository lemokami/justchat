//! Framework-independent application state for KiroUI.
//!
//! [`AppState`] is deliberately free of any GPUI types so the full
//! command/event state machine can be unit-tested headlessly. The GPUI layer
//! wraps it in an `Entity<AppState>` and calls [`AppState::apply_event`] from
//! the event pump, then `cx.notify()` to re-render.

use kiro_acp::protocol::{Command, Event, ModelOption, PermissionOptionInfo};
use kiro_acp::{Attachment, CommandSender};

/// Connection/handshake status shown in the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// Subprocess spawned, handshake in progress.
    Connecting,
    /// Handshake complete.
    Connected {
        /// Negotiated protocol version.
        protocol_version: u16,
    },
    /// The agent is unavailable or the connection was lost.
    Disconnected {
        /// Human-readable reason.
        message: String,
    },
}

/// Who authored a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The human user.
    User,
    /// The Kiro agent.
    Agent,
    /// System/diagnostic text.
    System,
}

/// Per-session agent activity indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    /// Not currently processing a turn.
    Idle,
    /// A prompt turn is in flight.
    Thinking,
}

/// A pending permission request attached to a tool call.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionPrompt {
    /// Correlates with [`Command::PermissionDecision`].
    pub request_id: u64,
    /// Options the user can choose from.
    pub options: Vec<PermissionOptionInfo>,
}

/// A visualized tool call within an agent message.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallView {
    /// Tool call id.
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// Tool kind (snake_case).
    pub kind: String,
    /// Execution status (snake_case): pending/in_progress/completed/failed.
    pub status: String,
    /// Accumulated textual output.
    pub output: Option<String>,
    /// Whether the output region is expanded in the UI.
    pub expanded: bool,
    /// A pending permission request, if the agent asked for approval.
    pub permission: Option<PermissionPrompt>,
}

/// A single chat message.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// Author.
    pub role: Role,
    /// Visible content (markdown for agent messages).
    pub content: String,
    /// Accumulated "thought" text (agent only).
    pub thoughts: String,
    /// Embedded tool calls (agent only).
    pub tool_calls: Vec<ToolCallView>,
    /// Files attached to this message (user only).
    pub attachments: Vec<Attachment>,
    /// Whether this message is still being streamed.
    pub streaming: bool,
}

impl Message {
    /// Create a user message with optional attachments.
    pub fn user(content: impl Into<String>, attachments: Vec<Attachment>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            thoughts: String::new(),
            tool_calls: Vec::new(),
            attachments,
            streaming: false,
        }
    }

    /// Create an empty, streaming agent message.
    pub fn agent_streaming() -> Self {
        Self {
            role: Role::Agent,
            content: String::new(),
            thoughts: String::new(),
            tool_calls: Vec::new(),
            attachments: Vec::new(),
            streaming: true,
        }
    }

    /// Create a system message.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            thoughts: String::new(),
            tool_calls: Vec::new(),
            attachments: Vec::new(),
            streaming: false,
        }
    }
}

/// A conversation session.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// Stable session id from the agent.
    pub id: String,
    /// Derived display title.
    pub title: String,
    /// Ordered messages.
    pub messages: Vec<Message>,
    /// Activity indicator.
    pub status: AgentStatus,
}

impl Session {
    fn new(id: String) -> Self {
        Self {
            id,
            title: "New session".to_string(),
            messages: Vec::new(),
            status: AgentStatus::Idle,
        }
    }

    /// The last streaming agent message, creating one if necessary.
    fn streaming_agent_message(&mut self) -> &mut Message {
        let needs_new = !matches!(
            self.messages.last(),
            Some(Message {
                role: Role::Agent,
                streaming: true,
                ..
            })
        );
        if needs_new {
            self.messages.push(Message::agent_streaming());
        }
        self.messages.last_mut().expect("just ensured present")
    }

    fn find_tool_call(&mut self, id: &str) -> Option<&mut ToolCallView> {
        self.messages
            .iter_mut()
            .rev()
            .flat_map(|m| m.tool_calls.iter_mut())
            .find(|tc| tc.id == id)
    }
}

/// The whole application's state.
pub struct AppState {
    /// Connection/handshake status.
    pub connection: ConnectionStatus,
    /// All known sessions.
    pub sessions: Vec<Session>,
    /// The currently displayed session.
    pub active_session_id: Option<String>,
    /// Current text in the input editor.
    pub input: String,
    /// Files staged to be sent with the next prompt.
    pub pending_attachments: Vec<Attachment>,
    /// Models the agent offers for selection.
    pub available_models: Vec<ModelOption>,
    /// The currently selected model id, if known.
    pub current_model: Option<String>,
    /// Outbound command channel to the protocol thread (None in some tests).
    commands: Option<CommandSender>,
}

impl AppState {
    /// Create a new state bound to the given command sender.
    pub fn new(commands: CommandSender) -> Self {
        Self {
            connection: ConnectionStatus::Connecting,
            sessions: Vec::new(),
            active_session_id: None,
            input: String::new(),
            pending_attachments: Vec::new(),
            available_models: Vec::new(),
            current_model: None,
            commands: Some(commands),
        }
    }

    /// Create a detached state with no command channel (for unit tests).
    pub fn detached() -> Self {
        Self {
            connection: ConnectionStatus::Connecting,
            sessions: Vec::new(),
            active_session_id: None,
            input: String::new(),
            pending_attachments: Vec::new(),
            available_models: Vec::new(),
            current_model: None,
            commands: None,
        }
    }

    fn send(&self, command: Command) {
        if let Some(tx) = &self.commands {
            let _ = tx.send(command);
        }
    }

    /// Replace the outbound command channel (used when reconnecting after a
    /// crash). In-memory session history is preserved.
    pub fn set_commands(&mut self, commands: CommandSender) {
        self.commands = Some(commands);
    }

    /// Mark the connection as reconnecting (keeps existing sessions/messages as
    /// history, but starts a fresh session once reconnected).
    pub fn mark_reconnecting(&mut self) {
        self.connection = ConnectionStatus::Connecting;
        self.active_session_id = None;
    }

    /// Whether the agent connection is currently lost.
    pub fn is_disconnected(&self) -> bool {
        matches!(self.connection, ConnectionStatus::Disconnected { .. })
    }

    /// The active session, if any.
    pub fn active_session(&self) -> Option<&Session> {
        let id = self.active_session_id.as_ref()?;
        self.sessions.iter().find(|s| &s.id == id)
    }

    fn session_mut(&mut self, id: &str) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    /// Whether the active session is currently processing a turn.
    pub fn is_thinking(&self) -> bool {
        self.active_session()
            .map(|s| s.status == AgentStatus::Thinking)
            .unwrap_or(false)
    }

    /// Request a new session from the agent.
    pub fn request_new_session(&self) {
        self.send(Command::CreateSession);
    }

    /// Switch the active session.
    pub fn switch_session(&mut self, id: &str) {
        if self.sessions.iter().any(|s| s.id == id) {
            self.active_session_id = Some(id.to_string());
        }
    }

    /// Stage a file to be attached to the next prompt.
    pub fn add_attachment(&mut self, path: impl Into<std::path::PathBuf>) {
        let att = Attachment::from_path(path);
        if !self.pending_attachments.iter().any(|a| a.path == att.path) {
            self.pending_attachments.push(att);
        }
    }

    /// Remove a staged attachment by index.
    pub fn remove_attachment(&mut self, index: usize) {
        if index < self.pending_attachments.len() {
            self.pending_attachments.remove(index);
        }
    }

    /// Switch the active model for the active session.
    pub fn set_model(&mut self, model_id: impl Into<String>) {
        let model_id = model_id.into();
        if let Some(session_id) = self.active_session_id.clone() {
            self.current_model = Some(model_id.clone());
            self.send(Command::SetModel {
                session_id,
                model_id,
            });
        }
    }

    /// The display name of the current model, if known.
    pub fn current_model_name(&self) -> Option<String> {
        let id = self.current_model.as_ref()?;
        let name = self
            .available_models
            .iter()
            .find(|m| &m.id == id)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| id.clone());
        Some(name)
    }

    /// Submit the current input as a prompt to the active session.
    ///
    /// Returns `true` if a prompt was sent (input non-empty and a session is
    /// active and idle).
    pub fn submit_input(&mut self) -> bool {
        let text = self.input.trim().to_string();
        // Allow sending with attachments only (no text).
        if text.is_empty() && self.pending_attachments.is_empty() {
            return false;
        }
        let Some(session_id) = self.active_session_id.clone() else {
            return false;
        };

        let attachments = self.pending_attachments.clone();

        if let Some(session) = self.session_mut(&session_id) {
            if session.status == AgentStatus::Thinking {
                // Don't send while a turn is active.
                return false;
            }
            if session.title == "New session" {
                let title_src = if text.is_empty() {
                    attachments
                        .first()
                        .map(|a| a.name.clone())
                        .unwrap_or_else(|| "Attachments".into())
                } else {
                    text.clone()
                };
                session.title = derive_title(&title_src);
            }
            session
                .messages
                .push(Message::user(text.clone(), attachments.clone()));
            session.messages.push(Message::agent_streaming());
            session.status = AgentStatus::Thinking;
        } else {
            return false;
        }

        self.input.clear();
        self.pending_attachments.clear();
        self.send(Command::SendPrompt {
            session_id,
            text,
            attachments,
        });
        true
    }

    /// Cancel the active session's in-flight turn.
    pub fn cancel_active(&self) {
        if let Some(session) = self.active_session() {
            if session.status == AgentStatus::Thinking {
                self.send(Command::Cancel {
                    session_id: session.id.clone(),
                });
            }
        }
    }

    /// Resolve a pending permission request.
    pub fn decide_permission(&mut self, request_id: u64, option_id: Option<String>) {
        // Clear the matching prompt from whichever tool call holds it.
        for session in &mut self.sessions {
            for message in &mut session.messages {
                for tc in &mut message.tool_calls {
                    if tc
                        .permission
                        .as_ref()
                        .is_some_and(|p| p.request_id == request_id)
                    {
                        tc.permission = None;
                    }
                }
            }
        }
        self.send(Command::PermissionDecision {
            request_id,
            option_id,
        });
    }

    /// Apply a protocol [`Event`] to the state.
    pub fn apply_event(&mut self, event: Event) {
        match event {
            Event::Connected {
                protocol_version, ..
            } => {
                self.connection = ConnectionStatus::Connected { protocol_version };
                // Create a fresh session if none is active (initial connect or
                // after a reconnect that cleared the active session).
                if self.active_session_id.is_none() {
                    self.request_new_session();
                }
            }
            Event::Disconnected { message } => {
                self.connection = ConnectionStatus::Disconnected { message };
            }
            Event::SessionCreated { session_id } => {
                if !self.sessions.iter().any(|s| s.id == session_id) {
                    self.sessions.push(Session::new(session_id.clone()));
                }
                if self.active_session_id.is_none() {
                    self.active_session_id = Some(session_id);
                }
            }
            Event::ModelsAvailable {
                current, models, ..
            } => {
                self.available_models = models;
                self.current_model = Some(current);
            }
            Event::MessageChunk { session_id, text } => {
                if let Some(session) = self.session_mut(&session_id) {
                    session.streaming_agent_message().content.push_str(&text);
                }
            }
            Event::ThoughtChunk { session_id, text } => {
                if let Some(session) = self.session_mut(&session_id) {
                    // Thoughts stream in as word/token chunks; concatenate them
                    // verbatim (the agent includes its own line breaks).
                    session.streaming_agent_message().thoughts.push_str(&text);
                }
            }
            Event::ToolCall {
                session_id,
                id,
                title,
                kind,
                status,
            } => {
                if let Some(session) = self.session_mut(&session_id) {
                    session
                        .streaming_agent_message()
                        .tool_calls
                        .push(ToolCallView {
                            id,
                            title,
                            kind,
                            status,
                            output: None,
                            expanded: false,
                            permission: None,
                        });
                }
            }
            Event::ToolCallUpdate {
                session_id,
                id,
                status,
                output,
            } => {
                if let Some(session) = self.session_mut(&session_id) {
                    if let Some(tc) = session.find_tool_call(&id) {
                        if let Some(status) = status {
                            tc.status = status;
                        }
                        if let Some(output) = output {
                            tc.output.get_or_insert_with(String::new).push_str(&output);
                        }
                    }
                }
            }
            Event::Plan {
                session_id,
                entries,
            } => {
                if let Some(session) = self.session_mut(&session_id) {
                    let plan = format!("Plan:\n- {}", entries.join("\n- "));
                    session.streaming_agent_message().thoughts = plan;
                }
            }
            Event::PermissionRequested {
                request_id,
                session_id,
                title,
                options,
            } => {
                if let Some(session) = self.session_mut(&session_id) {
                    let prompt = PermissionPrompt {
                        request_id,
                        options,
                    };
                    // Attach to the named tool call if present, else to the
                    // most recent tool call, else create a placeholder one.
                    let msg = session.streaming_agent_message();
                    let target = msg
                        .tool_calls
                        .iter()
                        .position(|tc| tc.title == title)
                        .or_else(|| msg.tool_calls.len().checked_sub(1));
                    if let Some(idx) = target {
                        msg.tool_calls[idx].permission = Some(prompt);
                    } else {
                        msg.tool_calls.push(ToolCallView {
                            id: format!("perm-{request_id}"),
                            title,
                            kind: "other".into(),
                            status: "pending".into(),
                            output: None,
                            expanded: true,
                            permission: Some(prompt),
                        });
                    }
                }
            }
            Event::TurnEnded { session_id, .. } => {
                if let Some(session) = self.session_mut(&session_id) {
                    session.status = AgentStatus::Idle;
                    if let Some(last) = session.messages.last_mut() {
                        if last.role == Role::Agent {
                            last.streaming = false;
                            // Any tool calls still shown as running are no longer
                            // active once the turn has ended.
                            for tc in &mut last.tool_calls {
                                if tc.status == "pending" || tc.status == "in_progress" {
                                    tc.status = "completed".into();
                                }
                            }
                        }
                    }
                }
            }
            Event::Error { message } => {
                if let Some(id) = self.active_session_id.clone() {
                    if let Some(session) = self.session_mut(&id) {
                        session.status = AgentStatus::Idle;
                        session
                            .messages
                            .push(Message::system(format!("Error: {message}")));
                    }
                }
            }
        }
    }
}

/// Derive a short session title from the first prompt.
fn derive_title(text: &str) -> String {
    let trimmed = text.trim();
    let first_line = trimmed.lines().next().unwrap_or(trimmed);
    let mut title: String = first_line.chars().take(40).collect();
    if first_line.chars().count() > 40 {
        title.push('…');
    }
    title
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected_state() -> AppState {
        let mut s = AppState::detached();
        s.apply_event(Event::Connected {
            protocol_version: 1,
            supported: true,
            load_session: true,
        });
        s.apply_event(Event::SessionCreated {
            session_id: "s1".into(),
        });
        s
    }

    #[test]
    fn connected_then_session_active() {
        let s = connected_state();
        assert_eq!(
            s.connection,
            ConnectionStatus::Connected {
                protocol_version: 1
            }
        );
        assert_eq!(s.active_session_id.as_deref(), Some("s1"));
        assert_eq!(s.sessions.len(), 1);
    }

    #[test]
    fn submit_pushes_user_and_streaming_agent_and_sets_thinking() {
        let mut s = connected_state();
        s.input = "hello".into();
        assert!(s.submit_input());
        assert!(s.input.is_empty(), "input cleared after send");
        let session = s.active_session().unwrap();
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].role, Role::User);
        assert_eq!(session.messages[0].content, "hello");
        assert_eq!(session.messages[1].role, Role::Agent);
        assert!(session.messages[1].streaming);
        assert!(s.is_thinking());
        assert_eq!(session.title, "hello");
    }

    #[test]
    fn empty_submit_is_noop() {
        let mut s = connected_state();
        s.input = "   ".into();
        assert!(!s.submit_input());
        assert_eq!(s.active_session().unwrap().messages.len(), 0);
    }

    #[test]
    fn streaming_chunks_accumulate_then_turn_ends() {
        let mut s = connected_state();
        s.input = "hi".into();
        s.submit_input();
        s.apply_event(Event::MessageChunk {
            session_id: "s1".into(),
            text: "Hello ".into(),
        });
        s.apply_event(Event::MessageChunk {
            session_id: "s1".into(),
            text: "world".into(),
        });
        assert!(s.is_thinking());
        s.apply_event(Event::TurnEnded {
            session_id: "s1".into(),
            stop_reason: "end_turn".into(),
        });
        let session = s.active_session().unwrap();
        assert_eq!(session.messages[1].content, "Hello world");
        assert!(!session.messages[1].streaming);
        assert!(!s.is_thinking());
    }

    #[test]
    fn thought_chunks_are_separate_from_content() {
        let mut s = connected_state();
        s.input = "x".into();
        s.submit_input();
        s.apply_event(Event::ThoughtChunk {
            session_id: "s1".into(),
            text: "pondering".into(),
        });
        s.apply_event(Event::MessageChunk {
            session_id: "s1".into(),
            text: "answer".into(),
        });
        let m = &s.active_session().unwrap().messages[1];
        assert_eq!(m.thoughts, "pondering");
        assert_eq!(m.content, "answer");
    }

    #[test]
    fn tool_call_lifecycle_updates_in_place() {
        let mut s = connected_state();
        s.input = "read".into();
        s.submit_input();
        s.apply_event(Event::ToolCall {
            session_id: "s1".into(),
            id: "t1".into(),
            title: "Read file".into(),
            kind: "read".into(),
            status: "pending".into(),
        });
        s.apply_event(Event::ToolCallUpdate {
            session_id: "s1".into(),
            id: "t1".into(),
            status: Some("completed".into()),
            output: Some("file contents".into()),
        });
        let tc = &s.active_session().unwrap().messages[1].tool_calls[0];
        assert_eq!(tc.status, "completed");
        assert_eq!(tc.output.as_deref(), Some("file contents"));
    }

    #[test]
    fn permission_request_then_decision_clears_prompt() {
        let mut s = connected_state();
        s.input = "run".into();
        s.submit_input();
        s.apply_event(Event::ToolCall {
            session_id: "s1".into(),
            id: "t1".into(),
            title: "Run command".into(),
            kind: "execute".into(),
            status: "pending".into(),
        });
        s.apply_event(Event::PermissionRequested {
            request_id: 7,
            session_id: "s1".into(),
            title: "Run command".into(),
            options: vec![PermissionOptionInfo {
                id: "allow".into(),
                name: "Allow".into(),
                kind: "allow_once".into(),
            }],
        });
        let tc = &s.active_session().unwrap().messages[1].tool_calls[0];
        assert!(tc.permission.is_some());
        assert_eq!(tc.permission.as_ref().unwrap().request_id, 7);

        s.decide_permission(7, Some("allow".into()));
        let tc = &s.active_session().unwrap().messages[1].tool_calls[0];
        assert!(tc.permission.is_none());
    }

    #[test]
    fn disconnected_sets_status() {
        let mut s = AppState::detached();
        s.apply_event(Event::Disconnected {
            message: "boom".into(),
        });
        assert_eq!(
            s.connection,
            ConnectionStatus::Disconnected {
                message: "boom".into()
            }
        );
    }

    #[test]
    fn switch_session_changes_active() {
        let mut s = connected_state();
        s.apply_event(Event::SessionCreated {
            session_id: "s2".into(),
        });
        assert_eq!(s.active_session_id.as_deref(), Some("s1"));
        s.switch_session("s2");
        assert_eq!(s.active_session_id.as_deref(), Some("s2"));
    }

    #[test]
    fn add_and_remove_attachments() {
        let mut s = connected_state();
        s.add_attachment("/tmp/a.png");
        s.add_attachment("/tmp/b.txt");
        s.add_attachment("/tmp/a.png"); // duplicate ignored
        assert_eq!(s.pending_attachments.len(), 2);
        assert!(s.pending_attachments[0].is_image);
        s.remove_attachment(0);
        assert_eq!(s.pending_attachments.len(), 1);
        assert_eq!(s.pending_attachments[0].name, "b.txt");
    }

    #[test]
    fn submit_with_attachment_only_sends_and_clears() {
        let mut s = connected_state();
        s.add_attachment("/tmp/pic.png");
        assert!(s.input.is_empty());
        assert!(
            s.submit_input(),
            "should send with attachment even without text"
        );
        assert!(
            s.pending_attachments.is_empty(),
            "attachments cleared after send"
        );
        let msg = &s.active_session().unwrap().messages[0];
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].name, "pic.png");
    }

    #[test]
    fn empty_submit_with_no_attachments_is_noop() {
        let mut s = connected_state();
        assert!(!s.submit_input());
        assert_eq!(s.active_session().unwrap().messages.len(), 0);
    }

    #[test]
    fn models_available_then_set_model() {
        let mut s = connected_state();
        s.apply_event(Event::ModelsAvailable {
            session_id: "s1".into(),
            current: "claude-opus-4.8".into(),
            models: vec![
                ModelOption {
                    id: "auto".into(),
                    name: "Auto".into(),
                    description: None,
                },
                ModelOption {
                    id: "claude-opus-4.8".into(),
                    name: "Opus 4.8".into(),
                    description: None,
                },
            ],
        });
        assert_eq!(s.available_models.len(), 2);
        assert_eq!(s.current_model.as_deref(), Some("claude-opus-4.8"));
        assert_eq!(s.current_model_name().as_deref(), Some("Opus 4.8"));

        s.set_model("auto");
        assert_eq!(s.current_model.as_deref(), Some("auto"));
        assert_eq!(s.current_model_name().as_deref(), Some("Auto"));
    }
}
