//! A scriptable ACP agent used by `kiro_acp` integration tests and examples.
//!
//! It speaks ACP over stdio (like the real `kiro-cli acp`) but its behaviour is
//! deterministic and controlled by environment variables so tests can assert
//! exact event sequences:
//!
//! * `KIRO_MOCK_BAD_VERSION=1` — respond to `initialize` with protocol V0
//!   (so the client can exercise its version-mismatch path).
//! * `KIRO_MOCK_EMIT_TOOL=1` — during a prompt, also emit a tool-call update
//!   sequence (used by tool-visualization tests).
//! * `KIRO_MOCK_REQUEST_PERMISSION=1` — during a prompt, call
//!   `session/request_permission` before finishing (used by permission tests).

use std::cell::Cell;

use agent_client_protocol::{self as acp, Client};
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _};

/// A unit of work the agent asks its connection-bound task to perform.
enum AgentAction {
    Notify(acp::SessionNotification),
    RequestPermission(acp::RequestPermissionRequest, oneshot::Sender<bool>),
}

struct MockAgent {
    tx: mpsc::UnboundedSender<(AgentAction, oneshot::Sender<()>)>,
    next_session_id: Cell<u64>,
}

impl MockAgent {
    fn new(tx: mpsc::UnboundedSender<(AgentAction, oneshot::Sender<()>)>) -> Self {
        Self {
            tx,
            next_session_id: Cell::new(0),
        }
    }

    /// Send an action to the connection task and wait for it to be processed,
    /// preserving ordering of notifications.
    async fn dispatch(&self, action: AgentAction) -> Result<(), acp::Error> {
        let (done_tx, done_rx) = oneshot::channel();
        self.tx
            .send((action, done_tx))
            .map_err(|_| acp::Error::internal_error())?;
        done_rx.await.map_err(|_| acp::Error::internal_error())
    }

    async fn notify(
        &self,
        session_id: &acp::SessionId,
        update: acp::SessionUpdate,
    ) -> Result<(), acp::Error> {
        self.dispatch(AgentAction::Notify(acp::SessionNotification {
            session_id: session_id.clone(),
            update,
            meta: None,
        }))
        .await
    }
}

fn flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1")
}

fn text_block(s: impl Into<String>) -> acp::ContentBlock {
    acp::ContentBlock::Text(acp::TextContent {
        text: s.into(),
        annotations: None,
        meta: None,
    })
}

#[async_trait::async_trait(?Send)]
impl acp::Agent for MockAgent {
    async fn initialize(
        &self,
        _args: acp::InitializeRequest,
    ) -> Result<acp::InitializeResponse, acp::Error> {
        let protocol_version = if flag("KIRO_MOCK_BAD_VERSION") {
            acp::V0
        } else {
            acp::V1
        };
        Ok(acp::InitializeResponse {
            protocol_version,
            agent_capabilities: acp::AgentCapabilities {
                load_session: true,
                ..Default::default()
            },
            auth_methods: Vec::new(),
            meta: None,
        })
    }

    async fn authenticate(
        &self,
        _args: acp::AuthenticateRequest,
    ) -> Result<acp::AuthenticateResponse, acp::Error> {
        Ok(acp::AuthenticateResponse::default())
    }

    async fn new_session(
        &self,
        _args: acp::NewSessionRequest,
    ) -> Result<acp::NewSessionResponse, acp::Error> {
        let n = self.next_session_id.get();
        self.next_session_id.set(n + 1);
        Ok(acp::NewSessionResponse {
            session_id: acp::SessionId(format!("mock-session-{n}").into()),
            modes: None,
            models: None,
            meta: None,
        })
    }

    async fn load_session(
        &self,
        _args: acp::LoadSessionRequest,
    ) -> Result<acp::LoadSessionResponse, acp::Error> {
        Ok(acp::LoadSessionResponse {
            modes: None,
            models: None,
            meta: None,
        })
    }

    async fn set_session_mode(
        &self,
        _args: acp::SetSessionModeRequest,
    ) -> Result<acp::SetSessionModeResponse, acp::Error> {
        Ok(acp::SetSessionModeResponse::default())
    }

    async fn set_session_model(
        &self,
        _args: acp::SetSessionModelRequest,
    ) -> Result<acp::SetSessionModelResponse, acp::Error> {
        Ok(acp::SetSessionModelResponse::default())
    }

    async fn prompt(&self, args: acp::PromptRequest) -> Result<acp::PromptResponse, acp::Error> {
        let sid = args.session_id.clone();

        if flag("KIRO_MOCK_CRASH_ON_PROMPT") {
            // Simulate the agent process dying mid-turn.
            std::process::exit(1);
        }

        // A thought chunk, then the echoed reply split into two chunks so tests
        // can verify streaming accumulation order.
        self.notify(
            &sid,
            acp::SessionUpdate::AgentThoughtChunk {
                content: text_block("thinking about it"),
            },
        )
        .await?;

        let prompt_text: String = args
            .prompt
            .iter()
            .map(|b| match b {
                acp::ContentBlock::Text(t) => t.text.clone(),
                _ => String::new(),
            })
            .collect();

        self.notify(
            &sid,
            acp::SessionUpdate::AgentMessageChunk {
                content: text_block("Hello! You said: "),
            },
        )
        .await?;
        self.notify(
            &sid,
            acp::SessionUpdate::AgentMessageChunk {
                content: text_block(prompt_text),
            },
        )
        .await?;

        if flag("KIRO_MOCK_EMIT_TOOL") {
            let tool_id = acp::ToolCallId("tool-1".into());
            self.notify(
                &sid,
                acp::SessionUpdate::ToolCall(acp::ToolCall {
                    id: tool_id.clone(),
                    title: "Read file".into(),
                    kind: acp::ToolKind::Read,
                    status: acp::ToolCallStatus::Pending,
                    content: Vec::new(),
                    locations: Vec::new(),
                    raw_input: None,
                    raw_output: None,
                    meta: None,
                }),
            )
            .await?;
            self.notify(
                &sid,
                acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate {
                    id: tool_id,
                    fields: acp::ToolCallUpdateFields {
                        status: Some(acp::ToolCallStatus::Completed),
                        content: Some(vec![acp::ToolCallContent::from(text_block(
                            "file contents",
                        ))]),
                        ..Default::default()
                    },
                    meta: None,
                }),
            )
            .await?;
        }

        if flag("KIRO_MOCK_REQUEST_PERMISSION") {
            let (granted_tx, granted_rx) = oneshot::channel();
            self.dispatch(AgentAction::RequestPermission(
                acp::RequestPermissionRequest {
                    session_id: sid.clone(),
                    tool_call: acp::ToolCallUpdate {
                        id: acp::ToolCallId("tool-perm".into()),
                        fields: acp::ToolCallUpdateFields {
                            title: Some("Run command".into()),
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
                },
                granted_tx,
            ))
            .await?;
            let granted = granted_rx.await.unwrap_or(false);
            let verdict = if granted { "granted" } else { "denied" };
            self.notify(
                &sid,
                acp::SessionUpdate::AgentMessageChunk {
                    content: text_block(format!(" [permission {verdict}]")),
                },
            )
            .await?;
        }

        Ok(acp::PromptResponse {
            stop_reason: acp::StopReason::EndTurn,
            meta: None,
        })
    }

    async fn cancel(&self, _args: acp::CancelNotification) -> Result<(), acp::Error> {
        Ok(())
    }

    async fn ext_method(&self, _args: acp::ExtRequest) -> Result<acp::ExtResponse, acp::Error> {
        Err(acp::Error::method_not_found())
    }

    async fn ext_notification(&self, _args: acp::ExtNotification) -> Result<(), acp::Error> {
        Ok(())
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let outgoing = tokio::io::stdout().compat_write();
    let incoming = tokio::io::stdin().compat();

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let (tx, mut rx) = mpsc::unbounded_channel::<(AgentAction, oneshot::Sender<()>)>();
            let (conn, handle_io) =
                acp::AgentSideConnection::new(MockAgent::new(tx), outgoing, incoming, |fut| {
                    tokio::task::spawn_local(fut);
                });

            // The connection-bound task performs notifications and outgoing
            // requests (request_permission) on behalf of the agent.
            tokio::task::spawn_local(async move {
                while let Some((action, done)) = rx.recv().await {
                    match action {
                        AgentAction::Notify(n) => {
                            let _ = conn.session_notification(n).await;
                        }
                        AgentAction::RequestPermission(req, reply) => {
                            let granted = match conn.request_permission(req).await {
                                Ok(resp) => matches!(
                                    resp.outcome,
                                    acp::RequestPermissionOutcome::Selected { option_id }
                                        if option_id.0.as_ref() == "allow"
                                ),
                                Err(_) => false,
                            };
                            let _ = reply.send(granted);
                        }
                    }
                    let _ = done.send(());
                }
            });

            handle_io.await
        })
        .await
}
