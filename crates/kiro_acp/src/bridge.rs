//! Thread bridge between the (single-threaded, `!Send`) ACP protocol runtime
//! and a [`Send`] command/event channel API the UI can drive.
//!
//! [`start`] spawns a dedicated OS thread that runs a current-thread Tokio
//! runtime + `LocalSet`, hosts the `kiro-cli acp` subprocess and the ACP
//! connection, and processes [`Command`]s while streaming [`Event`]s back.

use std::rc::Rc;
use std::thread::JoinHandle;

use agent_client_protocol::Agent as _;
use tokio::sync::mpsc;

use crate::error::{AcpError, Result};
use crate::protocol::{self, ClientShared, Command, Event};
use crate::subprocess::{Subprocess, SubprocessConfig};

/// Sender half of the UI → protocol command channel.
pub type CommandSender = mpsc::UnboundedSender<Command>;
/// Receiver half of the UI → protocol command channel.
pub type CommandReceiver = mpsc::UnboundedReceiver<Command>;

/// Configuration for [`start`].
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// How to launch the agent subprocess.
    pub subprocess: SubprocessConfig,
    /// Permissions/configuration policy.
    pub settings: crate::settings::Settings,
}

impl BridgeConfig {
    /// Build a config that runs the real `kiro-cli acp` in `cwd`.
    pub fn kiro(cwd: impl Into<std::path::PathBuf>) -> Self {
        Self {
            subprocess: SubprocessConfig::kiro(cwd),
            settings: crate::settings::Settings::default(),
        }
    }

    /// Apply a settings policy (and inject its env vars into the subprocess).
    pub fn with_settings(mut self, settings: crate::settings::Settings) -> Self {
        for env in &settings.env {
            self.subprocess
                .env
                .push((env.name.clone().into(), env.value.clone().into()));
        }
        if let Some(cwd) = &settings.cwd {
            self.subprocess.cwd = cwd.clone();
        }
        self.settings = settings;
        self
    }
}

/// Handle to a running bridge: send [`Command`]s, receive [`Event`]s, and join
/// the worker thread on shutdown.
pub struct BridgeHandle {
    /// Send commands to the protocol thread.
    pub commands: CommandSender,
    /// Receive events from the protocol thread. Taken by the UI event pump via
    /// [`BridgeHandle::take_events`].
    events: Option<protocol::EventReceiver>,
    join: Option<JoinHandle<()>>,
}

impl BridgeHandle {
    /// Send a command to the protocol thread.
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// A clonable command sender.
    pub fn commands(&self) -> CommandSender {
        self.commands.clone()
    }

    /// Take the event receiver (can only be taken once).
    pub fn take_events(&mut self) -> Option<protocol::EventReceiver> {
        self.events.take()
    }

    /// Request shutdown and join the worker thread, blocking until it exits.
    pub fn shutdown_and_join(mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        // Best-effort: ask the thread to stop and detach if still running.
        let _ = self.commands.send(Command::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Start the protocol runtime on a dedicated thread.
///
/// Returns immediately with a [`BridgeHandle`]; the handshake and subsequent
/// work happen asynchronously, surfaced through [`Event`]s (e.g.
/// [`Event::Connected`] or [`Event::Disconnected`]).
pub fn start(config: BridgeConfig) -> Result<BridgeHandle> {
    let (command_tx, command_rx) = mpsc::unbounded_channel::<Command>();
    let (event_tx, event_rx) = mpsc::unbounded_channel::<Event>();

    let join = std::thread::Builder::new()
        .name("kiro-acp-protocol".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = event_tx.send(Event::Disconnected {
                        message: format!("failed to build runtime: {e}"),
                    });
                    return;
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, run_protocol(config, command_rx, event_tx));
        })
        .map_err(AcpError::Io)?;

    Ok(BridgeHandle {
        commands: command_tx,
        events: Some(event_rx),
        join: Some(join),
    })
}

/// The protocol thread's main async routine.
async fn run_protocol(
    config: BridgeConfig,
    mut command_rx: CommandReceiver,
    event_tx: protocol::EventSender,
) {
    // The directory used as each session's workspace context.
    let session_cwd = config.subprocess.cwd.clone();

    // Spawn the agent subprocess.
    let (proc, outgoing, incoming) = match Subprocess::spawn(&config.subprocess) {
        Ok(parts) => parts,
        Err(e) => {
            let _ = event_tx.send(Event::Disconnected {
                message: format!("failed to start agent: {e}"),
            });
            return;
        }
    };

    // Interpose the JSON-RPC id shim so string-id agents (kiro-cli) work with
    // the integer-id ACP crate.
    let (outgoing, incoming) = crate::idshim::wrap(outgoing, incoming);

    let (conn, shared, io) = protocol::new_connection_with_settings(
        event_tx.clone(),
        config.settings,
        outgoing,
        incoming,
    );

    // Signal when the I/O task ends (stream closed = subprocess died/exited).
    let (io_done_tx, io_done_rx) = tokio::sync::oneshot::channel();
    tokio::task::spawn_local(async move {
        if let Err(e) = io.await {
            tracing::warn!("acp io task ended: {e}");
        }
        let _ = io_done_tx.send(());
    });

    // Handshake.
    match protocol::initialize(&conn).await {
        Ok(info) => {
            let _ = event_tx.send(Event::Connected {
                protocol_version: info.protocol_version,
                supported: info.supported,
                load_session: info.load_session,
            });
        }
        Err(e) => {
            let _ = event_tx.send(Event::Disconnected {
                message: format!("initialize failed: {e}"),
            });
            let _ = proc.shutdown().await;
            return;
        }
    }

    // Command loop (also watches for subprocess death).
    command_loop(
        &conn,
        &shared,
        &event_tx,
        &mut command_rx,
        io_done_rx,
        session_cwd,
    )
    .await;

    let _ = proc.shutdown().await;
}

async fn command_loop(
    conn: &Rc<agent_client_protocol::ClientSideConnection>,
    shared: &ClientShared,
    event_tx: &protocol::EventSender,
    command_rx: &mut CommandReceiver,
    io_done_rx: tokio::sync::oneshot::Receiver<()>,
    session_cwd: std::path::PathBuf,
) {
    let mut io_done_rx = io_done_rx;
    loop {
        let command = tokio::select! {
            cmd = command_rx.recv() => cmd,
            _ = &mut io_done_rx => {
                // The agent's stdio closed: it crashed or exited unexpectedly.
                let _ = event_tx.send(Event::Disconnected {
                    message: "agent process exited unexpectedly".into(),
                });
                break;
            }
        };
        let Some(command) = command else { break };
        match command {
            Command::CreateSession => {
                match protocol::create_session(conn, session_cwd.clone()).await {
                    Ok(init) => {
                        let _ = event_tx.send(Event::SessionCreated {
                            session_id: init.id.clone(),
                        });
                        if let Some(current) = init.current_model {
                            let _ = event_tx.send(Event::ModelsAvailable {
                                session_id: init.id,
                                current,
                                models: init.models,
                            });
                        }
                    }
                    Err(e) => {
                        let _ = event_tx.send(Event::Error {
                            message: format!("create session failed: {e}"),
                        });
                    }
                }
            }
            Command::SendPrompt {
                session_id,
                text,
                attachments,
            } => {
                // Spawn so the loop stays responsive to Cancel / permission
                // decisions while the turn is in flight.
                let conn = conn.clone();
                let event_tx = event_tx.clone();
                tokio::task::spawn_local(async move {
                    match protocol::send_prompt(&conn, &session_id, text, attachments).await {
                        Ok(stop_reason) => {
                            // The ACP crate dispatches incoming `session/update`
                            // notifications as independent tasks, decoupled from
                            // the prompt response. Yield a few times so any
                            // already-queued chunk events are emitted before we
                            // mark the turn ended (keeps ordering tidy for the
                            // instant case; with a real agent the response always
                            // trails the stream anyway).
                            for _ in 0..8 {
                                tokio::task::yield_now().await;
                            }
                            let _ = event_tx.send(Event::TurnEnded {
                                session_id,
                                stop_reason,
                            });
                        }
                        Err(e) => {
                            let _ = event_tx.send(Event::Error {
                                message: format!("prompt failed: {e}"),
                            });
                        }
                    }
                });
            }
            Command::Cancel { session_id } => {
                let _ = conn
                    .cancel(agent_client_protocol::CancelNotification {
                        session_id: agent_client_protocol::SessionId(session_id.into()),
                        meta: None,
                    })
                    .await;
            }
            Command::PermissionDecision {
                request_id,
                option_id,
            } => {
                shared.resolve_permission(request_id, option_id.as_deref());
            }
            Command::SetModel {
                session_id,
                model_id,
            } => {
                if let Err(e) = protocol::set_model(conn, &session_id, &model_id).await {
                    let _ = event_tx.send(Event::Error {
                        message: format!("set model failed: {e}"),
                    });
                }
            }
            Command::Shutdown => break,
        }
    }
}
