//! ACP protocol layer: the serializable [`Command`]/[`Event`] contract, the
//! [`KiroClient`] implementation of [`acp::Client`], and helpers to drive a
//! [`acp::ClientSideConnection`] (initialize, create session, prompt).
//!
//! Everything here runs on a single-threaded executor because the ACP futures
//! are `!Send`. The [`crate::bridge`] module hosts that executor on a dedicated
//! OS thread and exposes the channel-based API to the UI.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::rc::Rc;

use agent_client_protocol::{self as acp, Agent as _};
use futures::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};

use crate::error::{AcpError, Result};
use crate::settings::Settings;
use crate::terminals::TerminalManager;

/// Sender half of the protocol → UI event channel.
pub type EventSender = mpsc::UnboundedSender<Event>;
/// Receiver half of the protocol → UI event channel.
pub type EventReceiver = mpsc::UnboundedReceiver<Event>;

/// Commands the UI (or a headless controller) sends to the protocol layer.
#[derive(Debug, Clone)]
pub enum Command {
    /// Create a new session.
    CreateSession,
    /// Send a user prompt to a session.
    SendPrompt {
        /// Target session.
        session_id: String,
        /// The user's prompt text.
        text: String,
        /// Files attached to the prompt (images and other resources).
        attachments: Vec<crate::attachment::Attachment>,
    },
    /// Cancel the in-flight turn for a session.
    Cancel {
        /// Target session.
        session_id: String,
    },
    /// Resolve a pending `session/request_permission` with the chosen option.
    PermissionDecision {
        /// Correlates with [`Event::PermissionRequested::request_id`].
        request_id: u64,
        /// The selected option id, or `None` to cancel/reject.
        option_id: Option<String>,
    },
    /// Switch the active model for a session.
    SetModel {
        /// Target session.
        session_id: String,
        /// The model id to switch to.
        model_id: String,
    },
    /// Gracefully shut down the protocol thread and subprocess.
    Shutdown,
}

/// A permission option presented to the user.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PermissionOptionInfo {
    /// Stable id used in [`Command::PermissionDecision`].
    pub id: String,
    /// Human-readable label.
    pub name: String,
    /// Option kind (e.g. `allow_once`, `reject_once`).
    pub kind: String,
}

/// A model the agent can run, surfaced for selection in the UI.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ModelOption {
    /// Stable model id used in [`Command::SetModel`].
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Optional description.
    pub description: Option<String>,
}

/// A slash command advertised by the agent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SlashCommand {
    /// Command name without the leading slash (e.g. `help`).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Hint shown for the command's input, if any.
    pub hint: Option<String>,
}

/// Events the protocol layer emits to the UI.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// The connection handshake completed.
    Connected {
        /// Negotiated protocol version number.
        protocol_version: u16,
        /// Whether the negotiated version is the one we support (v1).
        supported: bool,
        /// Whether the agent supports `session/load`.
        load_session: bool,
    },
    /// A new session was created.
    SessionCreated {
        /// The new session id.
        session_id: String,
    },
    /// The set of selectable models (and the current one) for a session.
    ModelsAvailable {
        /// Owning session.
        session_id: String,
        /// Currently active model id.
        current: String,
        /// Selectable models.
        models: Vec<ModelOption>,
    },
    /// The set of slash commands the agent advertises.
    CommandsAvailable {
        /// Owning session.
        session_id: String,
        /// Available commands.
        commands: Vec<SlashCommand>,
    },
    /// A chunk of the agent's visible reply.
    MessageChunk {
        /// Owning session.
        session_id: String,
        /// Text fragment to append.
        text: String,
    },
    /// A chunk of the agent's internal "thought".
    ThoughtChunk {
        /// Owning session.
        session_id: String,
        /// Text fragment to append.
        text: String,
    },
    /// A tool call was announced.
    ToolCall {
        /// Owning session.
        session_id: String,
        /// Tool call id.
        id: String,
        /// Human-readable title.
        title: String,
        /// Tool kind (snake_case).
        kind: String,
        /// Execution status (snake_case).
        status: String,
    },
    /// An update to an existing tool call.
    ToolCallUpdate {
        /// Owning session.
        session_id: String,
        /// Tool call id.
        id: String,
        /// New status, if changed (snake_case).
        status: Option<String>,
        /// Newly produced textual output, if any.
        output: Option<String>,
    },
    /// The agent published or updated its plan.
    Plan {
        /// Owning session.
        session_id: String,
        /// Plan entry descriptions.
        entries: Vec<String>,
    },
    /// The agent is asking the user to approve a tool call.
    PermissionRequested {
        /// Correlates with [`Command::PermissionDecision`].
        request_id: u64,
        /// Owning session.
        session_id: String,
        /// Title of the tool call needing approval.
        title: String,
        /// Available options.
        options: Vec<PermissionOptionInfo>,
    },
    /// The current prompt turn ended.
    TurnEnded {
        /// Owning session.
        session_id: String,
        /// Stop reason (snake_case).
        stop_reason: String,
    },
    /// A recoverable error occurred.
    Error {
        /// Human-readable message.
        message: String,
    },
    /// The connection to the agent was lost.
    Disconnected {
        /// Human-readable reason.
        message: String,
    },
}

/// A pending permission request awaiting a user decision.
///
/// Constructed by [`KiroClient::request_permission`] and resolved by the
/// command loop (Task 5/12) via [`Command::PermissionDecision`].
pub(crate) struct PendingPermission {
    pub(crate) options: Vec<acp::PermissionOptionId>,
    pub(crate) responder: oneshot::Sender<acp::RequestPermissionOutcome>,
}

/// Shared, single-threaded state used by [`KiroClient`] and the command loop.
#[derive(Clone)]
pub struct ClientShared {
    pub(crate) events: EventSender,
    pub(crate) pending: Rc<RefCell<HashMap<u64, PendingPermission>>>,
    pub(crate) next_request_id: Rc<RefCell<u64>>,
    pub(crate) settings: Rc<Settings>,
    pub(crate) terminals: Rc<RefCell<TerminalManager>>,
}

impl ClientShared {
    pub(crate) fn with_settings(events: EventSender, settings: Settings) -> Self {
        Self {
            events,
            pending: Rc::new(RefCell::new(HashMap::new())),
            next_request_id: Rc::new(RefCell::new(1)),
            settings: Rc::new(settings),
            terminals: Rc::new(RefCell::new(TerminalManager::default())),
        }
    }

    fn emit(&self, event: Event) {
        // Unbounded send only fails if the receiver was dropped (UI gone).
        let _ = self.events.send(event);
    }

    /// Resolve a pending permission request with the user's chosen option.
    ///
    /// `option_id == None` (or an unknown id) cancels the request. Returns
    /// `true` if a pending request was found and resolved.
    pub(crate) fn resolve_permission(&self, request_id: u64, option_id: Option<&str>) -> bool {
        let Some(pending) = self.pending.borrow_mut().remove(&request_id) else {
            return false;
        };
        let outcome = match option_id {
            Some(id) if pending.options.iter().any(|o| o.0.as_ref() == id) => {
                acp::RequestPermissionOutcome::Selected {
                    option_id: acp::PermissionOptionId(id.into()),
                }
            }
            _ => acp::RequestPermissionOutcome::Cancelled,
        };
        // The receiver may have gone away if the turn was cancelled; ignore.
        pending.responder.send(outcome).is_ok()
    }
}

/// The client end of the ACP connection. Translates agent notifications and
/// requests into [`Event`]s and resolves permission requests via the
/// [`Command::PermissionDecision`] path.
pub struct KiroClient {
    shared: ClientShared,
}

impl KiroClient {
    pub(crate) fn new(shared: ClientShared) -> Self {
        Self { shared }
    }
}

/// Extract displayable text from a content block.
fn content_text(block: &acp::ContentBlock) -> String {
    match block {
        acp::ContentBlock::Text(t) => t.text.clone(),
        acp::ContentBlock::ResourceLink(r) => r.uri.clone(),
        acp::ContentBlock::Image(_) => "<image>".into(),
        acp::ContentBlock::Audio(_) => "<audio>".into(),
        acp::ContentBlock::Resource(_) => "<resource>".into(),
    }
}

/// Serialize a serde enum to its snake_case string form.
fn snake<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Concatenate the textual content of a tool-call content list.
fn tool_content_text(content: &[acp::ToolCallContent]) -> Option<String> {
    let mut out = String::new();
    for c in content {
        if let acp::ToolCallContent::Content { content } = c {
            out.push_str(&content_text(content));
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[async_trait::async_trait(?Send)]
impl acp::Client for KiroClient {
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> std::result::Result<acp::RequestPermissionResponse, acp::Error> {
        // Allocate a request id and surface the request to the UI.
        let request_id = {
            let mut id = self.shared.next_request_id.borrow_mut();
            let v = *id;
            *id += 1;
            v
        };

        let title = args
            .tool_call
            .fields
            .title
            .clone()
            .unwrap_or_else(|| "Tool call".into());

        // Settings-driven auto-approval (Allow-All for non-destructive ops).
        if self.shared.settings.should_auto_approve(&title) {
            if let Some(opt) = args
                .options
                .iter()
                .find(|o| snake(&o.kind).contains("allow"))
                .or_else(|| args.options.first())
            {
                return Ok(acp::RequestPermissionResponse {
                    outcome: acp::RequestPermissionOutcome::Selected {
                        option_id: opt.id.clone(),
                    },
                    meta: None,
                });
            }
        }

        let options: Vec<PermissionOptionInfo> = args
            .options
            .iter()
            .map(|o| PermissionOptionInfo {
                id: o.id.0.to_string(),
                name: o.name.clone(),
                kind: snake(&o.kind),
            })
            .collect();

        let (tx, rx) = oneshot::channel();
        self.shared.pending.borrow_mut().insert(
            request_id,
            PendingPermission {
                options: args.options.iter().map(|o| o.id.clone()).collect(),
                responder: tx,
            },
        );

        self.shared.emit(Event::PermissionRequested {
            request_id,
            session_id: args.session_id.to_string(),
            title,
            options,
        });

        // Wait for the UI to decide (or the channel to drop => cancelled).
        match rx.await {
            Ok(outcome) => Ok(acp::RequestPermissionResponse {
                outcome,
                meta: None,
            }),
            Err(_) => {
                self.shared.pending.borrow_mut().remove(&request_id);
                Ok(acp::RequestPermissionResponse {
                    outcome: acp::RequestPermissionOutcome::Cancelled,
                    meta: None,
                })
            }
        }
    }

    async fn write_text_file(
        &self,
        args: acp::WriteTextFileRequest,
    ) -> std::result::Result<acp::WriteTextFileResponse, acp::Error> {
        if !args.path.is_absolute() {
            return Err(acp::Error::invalid_params().with_data("path must be absolute"));
        }
        if let Some(parent) = args.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&args.path, &args.content)
            .map_err(|e| acp::Error::internal_error().with_data(e.to_string()))?;
        Ok(acp::WriteTextFileResponse { meta: None })
    }

    async fn read_text_file(
        &self,
        args: acp::ReadTextFileRequest,
    ) -> std::result::Result<acp::ReadTextFileResponse, acp::Error> {
        if !args.path.is_absolute() {
            return Err(acp::Error::invalid_params().with_data("path must be absolute"));
        }
        let content = std::fs::read_to_string(&args.path)
            .map_err(|e| acp::Error::internal_error().with_data(e.to_string()))?;

        // Honor optional 1-based line offset and limit.
        let content = if args.line.is_some() || args.limit.is_some() {
            let start = args.line.unwrap_or(1).saturating_sub(1) as usize;
            let mut lines: Vec<&str> = content.lines().skip(start).collect();
            if let Some(limit) = args.limit {
                lines.truncate(limit as usize);
            }
            lines.join("\n")
        } else {
            content
        };

        Ok(acp::ReadTextFileResponse {
            content,
            meta: None,
        })
    }

    async fn create_terminal(
        &self,
        args: acp::CreateTerminalRequest,
    ) -> std::result::Result<acp::CreateTerminalResponse, acp::Error> {
        let terminal_id = self
            .shared
            .terminals
            .borrow_mut()
            .create(&args)
            .map_err(|e| acp::Error::internal_error().with_data(e.to_string()))?;
        Ok(acp::CreateTerminalResponse {
            terminal_id,
            meta: None,
        })
    }

    async fn terminal_output(
        &self,
        args: acp::TerminalOutputRequest,
    ) -> std::result::Result<acp::TerminalOutputResponse, acp::Error> {
        self.shared
            .terminals
            .borrow_mut()
            .output(&args.terminal_id)
            .ok_or_else(|| acp::Error::invalid_params().with_data("unknown terminal"))
    }

    async fn release_terminal(
        &self,
        args: acp::ReleaseTerminalRequest,
    ) -> std::result::Result<acp::ReleaseTerminalResponse, acp::Error> {
        self.shared
            .terminals
            .borrow_mut()
            .release(&args.terminal_id);
        Ok(acp::ReleaseTerminalResponse { meta: None })
    }

    async fn wait_for_terminal_exit(
        &self,
        args: acp::WaitForTerminalExitRequest,
    ) -> std::result::Result<acp::WaitForTerminalExitResponse, acp::Error> {
        // Avoid holding the RefCell borrow across await: poll cooperatively.
        loop {
            {
                let mut mgr = self.shared.terminals.borrow_mut();
                if let Some(resp) = mgr.output(&args.terminal_id) {
                    if let Some(status) = resp.exit_status {
                        return Ok(acp::WaitForTerminalExitResponse {
                            exit_status: status,
                            meta: None,
                        });
                    }
                } else {
                    return Err(acp::Error::invalid_params().with_data("unknown terminal"));
                }
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    async fn kill_terminal_command(
        &self,
        args: acp::KillTerminalCommandRequest,
    ) -> std::result::Result<acp::KillTerminalCommandResponse, acp::Error> {
        self.shared.terminals.borrow_mut().kill(&args.terminal_id);
        Ok(acp::KillTerminalCommandResponse { meta: None })
    }

    async fn session_notification(
        &self,
        args: acp::SessionNotification,
    ) -> std::result::Result<(), acp::Error> {
        let session_id = args.session_id.to_string();
        match args.update {
            acp::SessionUpdate::AgentMessageChunk { content } => {
                self.shared.emit(Event::MessageChunk {
                    session_id,
                    text: content_text(&content),
                });
            }
            acp::SessionUpdate::AgentThoughtChunk { content } => {
                self.shared.emit(Event::ThoughtChunk {
                    session_id,
                    text: content_text(&content),
                });
            }
            acp::SessionUpdate::UserMessageChunk { content } => {
                self.shared.emit(Event::MessageChunk {
                    session_id,
                    text: content_text(&content),
                });
            }
            acp::SessionUpdate::ToolCall(tc) => {
                self.shared.emit(Event::ToolCall {
                    session_id,
                    id: tc.id.0.to_string(),
                    title: tc.title,
                    kind: snake(&tc.kind),
                    status: snake(&tc.status),
                });
            }
            acp::SessionUpdate::ToolCallUpdate(update) => {
                self.shared.emit(Event::ToolCallUpdate {
                    session_id,
                    id: update.id.0.to_string(),
                    status: update.fields.status.as_ref().map(snake),
                    output: update
                        .fields
                        .content
                        .as_ref()
                        .and_then(|c| tool_content_text(c)),
                });
            }
            acp::SessionUpdate::Plan(plan) => {
                self.shared.emit(Event::Plan {
                    session_id,
                    entries: plan.entries.into_iter().map(|e| e.content).collect(),
                });
            }
            acp::SessionUpdate::AvailableCommandsUpdate { available_commands } => {
                let commands = available_commands
                    .into_iter()
                    .map(|c| {
                        let hint = match c.input {
                            Some(acp::AvailableCommandInput::Unstructured { hint }) => Some(hint),
                            _ => None,
                        };
                        SlashCommand {
                            name: c.name,
                            description: c.description,
                            hint,
                        }
                    })
                    .collect();
                self.shared.emit(Event::CommandsAvailable {
                    session_id,
                    commands,
                });
            }
            acp::SessionUpdate::CurrentModeUpdate { .. } => {}
        }
        Ok(())
    }

    async fn ext_method(
        &self,
        _args: acp::ExtRequest,
    ) -> std::result::Result<acp::ExtResponse, acp::Error> {
        Err(acp::Error::method_not_found())
    }

    async fn ext_notification(
        &self,
        _args: acp::ExtNotification,
    ) -> std::result::Result<(), acp::Error> {
        Ok(())
    }
}

/// The capabilities KiroUI advertises to the agent.
///
/// We honestly declare fs read/write and terminal support; the corresponding
/// handlers are completed in Task 13.
pub fn default_client_capabilities() -> acp::ClientCapabilities {
    acp::ClientCapabilities {
        fs: acp::FileSystemCapability {
            read_text_file: true,
            write_text_file: true,
            meta: None,
        },
        terminal: true,
        meta: None,
    }
}

/// Information gathered from the `initialize` handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedInfo {
    /// Negotiated protocol version number.
    pub protocol_version: u16,
    /// Whether the negotiated version is the one we support (v1).
    pub supported: bool,
    /// Whether the agent supports `session/load`.
    pub load_session: bool,
}

/// Create a client-side connection over the given byte streams.
///
/// Returns the connection (shared via [`Rc`]), the shared client state (for the
/// command loop to resolve permissions), and the I/O driver future that must be
/// spawned on the local executor.
#[allow(clippy::type_complexity)]
pub fn new_connection<W, R>(
    events: EventSender,
    outgoing: W,
    incoming: R,
) -> (
    Rc<acp::ClientSideConnection>,
    ClientShared,
    impl Future<Output = anyhow::Result<()>>,
)
where
    W: AsyncWrite + Unpin + 'static,
    R: AsyncRead + Unpin + 'static,
{
    new_connection_with_settings(events, Settings::default(), outgoing, incoming)
}

/// Like [`new_connection`] but with an explicit [`Settings`] policy.
#[allow(clippy::type_complexity)]
pub fn new_connection_with_settings<W, R>(
    events: EventSender,
    settings: Settings,
    outgoing: W,
    incoming: R,
) -> (
    Rc<acp::ClientSideConnection>,
    ClientShared,
    impl Future<Output = anyhow::Result<()>>,
)
where
    W: AsyncWrite + Unpin + 'static,
    R: AsyncRead + Unpin + 'static,
{
    let shared = ClientShared::with_settings(events, settings);
    let client = KiroClient::new(shared.clone());
    let (conn, io) = acp::ClientSideConnection::new(client, outgoing, incoming, |fut| {
        tokio::task::spawn_local(fut);
    });
    (Rc::new(conn), shared, io)
}

/// Perform the `initialize` handshake and return the negotiated info.
pub async fn initialize(conn: &acp::ClientSideConnection) -> Result<ConnectedInfo> {
    let resp = conn
        .initialize(acp::InitializeRequest {
            protocol_version: acp::V1,
            client_capabilities: default_client_capabilities(),
            meta: None,
        })
        .await
        .map_err(AcpError::from)?;

    let protocol_version = serde_json::to_value(&resp.protocol_version)
        .ok()
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u16;

    Ok(ConnectedInfo {
        protocol_version,
        supported: resp.protocol_version == acp::V1,
        load_session: resp.agent_capabilities.load_session,
    })
}

/// Result of creating a session: its id plus any advertised model state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInit {
    /// The new session id.
    pub id: String,
    /// Currently active model, if the agent reports models.
    pub current_model: Option<String>,
    /// Selectable models (empty if unsupported).
    pub models: Vec<ModelOption>,
}

/// Create a new session and return its id plus model state.
pub async fn create_session(
    conn: &acp::ClientSideConnection,
    cwd: std::path::PathBuf,
) -> Result<SessionInit> {
    let resp = conn
        .new_session(acp::NewSessionRequest {
            cwd,
            mcp_servers: Vec::new(),
            meta: None,
        })
        .await
        .map_err(AcpError::from)?;

    let (current_model, models) = match resp.models {
        Some(state) => (
            Some(state.current_model_id.0.to_string()),
            state
                .available_models
                .into_iter()
                .map(|m| ModelOption {
                    id: m.model_id.0.to_string(),
                    name: m.name,
                    description: m.description,
                })
                .collect(),
        ),
        None => (None, Vec::new()),
    };

    Ok(SessionInit {
        id: resp.session_id.to_string(),
        current_model,
        models,
    })
}

/// Switch the active model for a session.
pub async fn set_model(
    conn: &acp::ClientSideConnection,
    session_id: &str,
    model_id: &str,
) -> Result<()> {
    conn.set_session_model(acp::SetSessionModelRequest {
        session_id: acp::SessionId(session_id.into()),
        model_id: acp::ModelId(model_id.into()),
        meta: None,
    })
    .await
    .map_err(AcpError::from)?;
    Ok(())
}

/// Send a prompt and await the turn's stop reason (as a snake_case string).
///
/// While this future is pending, the agent streams `session/update`
/// notifications which [`KiroClient`] turns into [`Event`]s.
pub async fn send_prompt(
    conn: &acp::ClientSideConnection,
    session_id: &str,
    text: String,
    attachments: Vec<crate::attachment::Attachment>,
) -> Result<String> {
    let mut prompt: Vec<acp::ContentBlock> = Vec::new();
    if !text.is_empty() {
        prompt.push(text.into());
    }

    for att in attachments {
        if att.is_image {
            match std::fs::read(&att.path) {
                Ok(bytes) => {
                    prompt.push(acp::ContentBlock::Image(acp::ImageContent {
                        data: crate::attachment::base64_encode(&bytes),
                        mime_type: att.mime.clone().unwrap_or_else(|| "image/png".into()),
                        uri: Some(crate::attachment::file_uri(&att.path)),
                        annotations: None,
                        meta: None,
                    }));
                }
                Err(e) => {
                    tracing::warn!("could not read image {}: {e}", att.path.display());
                }
            }
        } else {
            prompt.push(acp::ContentBlock::ResourceLink(acp::ResourceLink {
                name: att.name.clone(),
                uri: crate::attachment::file_uri(&att.path),
                mime_type: att.mime.clone(),
                description: None,
                size: None,
                title: None,
                annotations: None,
                meta: None,
            }));
        }
    }

    // Ensure we always send at least one block.
    if prompt.is_empty() {
        prompt.push(String::new().into());
    }

    let resp = conn
        .prompt(acp::PromptRequest {
            session_id: acp::SessionId(session_id.into()),
            prompt,
            meta: None,
        })
        .await
        .map_err(AcpError::from)?;
    Ok(snake(&resp.stop_reason))
}

#[cfg(test)]
mod handler_tests {
    use super::*;
    use crate::settings::{AutoApprove, Settings};
    use agent_client_protocol::Client as _;

    fn client_with(settings: Settings) -> (KiroClient, EventReceiver) {
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = ClientShared::with_settings(tx, settings);
        (KiroClient::new(shared), rx)
    }

    fn perm_request(title: &str) -> acp::RequestPermissionRequest {
        acp::RequestPermissionRequest {
            session_id: acp::SessionId("s".into()),
            tool_call: acp::ToolCallUpdate {
                id: acp::ToolCallId("t".into()),
                fields: acp::ToolCallUpdateFields {
                    title: Some(title.into()),
                    ..Default::default()
                },
                meta: None,
            },
            options: vec![
                acp::PermissionOption {
                    id: acp::PermissionOptionId("allow".into()),
                    name: "Allow".into(),
                    kind: acp::PermissionOptionKind::AllowOnce,
                    meta: None,
                },
                acp::PermissionOption {
                    id: acp::PermissionOptionId("reject".into()),
                    name: "Reject".into(),
                    kind: acp::PermissionOptionKind::RejectOnce,
                    meta: None,
                },
            ],
            meta: None,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn allow_all_auto_approves_without_event() {
        let (client, mut rx) = client_with(Settings {
            auto_approve_permissions: AutoApprove::AllowAll,
            ..Default::default()
        });
        let resp = client
            .request_permission(perm_request("Read file"))
            .await
            .unwrap();
        match resp.outcome {
            acp::RequestPermissionOutcome::Selected { option_id } => {
                assert_eq!(option_id.0.as_ref(), "allow");
            }
            other => panic!("expected Selected(allow), got {other:?}"),
        }
        // No event should have been surfaced to the UI.
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn allow_all_still_prompts_destructive() {
        let (client, mut rx) = client_with(Settings {
            auto_approve_permissions: AutoApprove::AllowAll,
            ..Default::default()
        });
        // A destructive title must surface a PermissionRequested event rather
        // than auto-approving. request_permission would block awaiting the
        // decision, so resolve it concurrently.
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let fut = tokio::task::spawn_local(async move {
                    client
                        .request_permission(perm_request("sudo rm -rf /"))
                        .await
                });
                // The event should appear.
                let ev = loop {
                    if let Ok(ev) = rx.try_recv() {
                        break ev;
                    }
                    tokio::task::yield_now().await;
                };
                assert!(matches!(ev, Event::PermissionRequested { .. }));
                fut.abort();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fs_write_then_read_roundtrip() {
        let (client, _rx) = client_with(Settings::default());
        let path = std::env::temp_dir().join(format!("kiroui_test_{}.txt", std::process::id()));
        client
            .write_text_file(acp::WriteTextFileRequest {
                session_id: acp::SessionId("s".into()),
                path: path.clone(),
                content: "line1\nline2\nline3\n".into(),
                meta: None,
            })
            .await
            .unwrap();

        let resp = client
            .read_text_file(acp::ReadTextFileRequest {
                session_id: acp::SessionId("s".into()),
                path: path.clone(),
                line: None,
                limit: None,
                meta: None,
            })
            .await
            .unwrap();
        assert_eq!(resp.content, "line1\nline2\nline3\n");

        // line/limit honored.
        let resp = client
            .read_text_file(acp::ReadTextFileRequest {
                session_id: acp::SessionId("s".into()),
                path: path.clone(),
                line: Some(2),
                limit: Some(1),
                meta: None,
            })
            .await
            .unwrap();
        assert_eq!(resp.content, "line2");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fs_rejects_relative_path() {
        let (client, _rx) = client_with(Settings::default());
        let err = client
            .read_text_file(acp::ReadTextFileRequest {
                session_id: acp::SessionId("s".into()),
                path: std::path::PathBuf::from("relative.txt"),
                line: None,
                limit: None,
                meta: None,
            })
            .await;
        assert!(err.is_err(), "relative path must be rejected");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn terminal_create_capture_and_wait() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (client, _rx) = client_with(Settings::default());
                let created = client
                    .create_terminal(acp::CreateTerminalRequest {
                        session_id: acp::SessionId("s".into()),
                        command: "sh".into(),
                        args: vec!["-c".into(), "echo hello-terminal".into()],
                        env: Vec::new(),
                        cwd: None,
                        output_byte_limit: None,
                        meta: None,
                    })
                    .await
                    .unwrap();

                // Wait for it to exit.
                let exit = client
                    .wait_for_terminal_exit(acp::WaitForTerminalExitRequest {
                        session_id: acp::SessionId("s".into()),
                        terminal_id: created.terminal_id.clone(),
                        meta: None,
                    })
                    .await
                    .unwrap();
                assert_eq!(exit.exit_status.exit_code, Some(0));

                let out = client
                    .terminal_output(acp::TerminalOutputRequest {
                        session_id: acp::SessionId("s".into()),
                        terminal_id: created.terminal_id.clone(),
                        meta: None,
                    })
                    .await
                    .unwrap();
                assert!(
                    out.output.contains("hello-terminal"),
                    "captured output: {:?}",
                    out.output
                );

                client
                    .release_terminal(acp::ReleaseTerminalRequest {
                        session_id: acp::SessionId("s".into()),
                        terminal_id: created.terminal_id,
                        meta: None,
                    })
                    .await
                    .unwrap();
            })
            .await;
    }
}
