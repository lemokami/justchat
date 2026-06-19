//! KiroUI — Tauri backend.
//!
//! A generic ACP client: the frontend chooses an *agent profile* (any command +
//! args + env), and this backend spawns it via the `kiro_acp` engine, forwarding
//! protocol [`Event`]s to the webview over the `acp-event` channel.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use kiro_acp::bridge::{self, BridgeConfig};
use kiro_acp::protocol::{Command, Event};
use kiro_acp::{Attachment, BridgeHandle, CommandSender, Settings, SubprocessConfig};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

/// Managed backend state: the live command channel and bridge handle.
#[derive(Default)]
struct Backend {
    commands: Mutex<Option<CommandSender>>,
    handle: Mutex<Option<BridgeHandle>>,
}

impl Backend {
    fn send(&self, command: Command) {
        if let Some(tx) = self.commands.lock().unwrap().as_ref() {
            let _ = tx.send(command);
        }
    }
}

// ---- Agent profiles -------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EnvPair {
    name: String,
    value: String,
}

/// A user-configurable ACP agent: how to launch it and with what environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentProfile {
    id: String,
    name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: Vec<EnvPair>,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct AgentStore {
    agents: Vec<AgentProfile>,
    #[serde(rename = "activeId")]
    active_id: Option<String>,
}

fn kiroui_dir() -> PathBuf {
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(".kiroui");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn default_chat_workspace() -> PathBuf {
    let dir = kiroui_dir().join("workspace");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn chats_dir() -> PathBuf {
    let dir = kiroui_dir().join("chats");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn agents_file() -> PathBuf {
    kiroui_dir().join("agents.json")
}

/// Built-in starter agents. Users edit/extend these in the UI.
fn default_agents() -> AgentStore {
    let env = |n: &str| {
        vec![EnvPair {
            name: n.into(),
            value: String::new(),
        }]
    };
    AgentStore {
        active_id: Some("kiro".into()),
        agents: vec![
            AgentProfile {
                id: "kiro".into(),
                name: "Kiro CLI".into(),
                command: "kiro-cli".into(),
                args: vec!["acp".into()],
                env: vec![],
                cwd: None,
            },
            AgentProfile {
                id: "gemini".into(),
                name: "Gemini CLI".into(),
                command: "gemini".into(),
                args: vec!["--experimental-acp".into()],
                env: env("GEMINI_API_KEY"),
                cwd: None,
            },
            AgentProfile {
                id: "claude-code".into(),
                name: "Claude Code".into(),
                command: "npx".into(),
                args: vec!["-y".into(), "@zed-industries/claude-code-acp".into()],
                env: env("ANTHROPIC_API_KEY"),
                cwd: None,
            },
        ],
    }
}

/// Load configured agents (or the built-in defaults on first run).
#[tauri::command]
fn load_agents() -> AgentStore {
    match std::fs::read_to_string(agents_file()) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|_| default_agents()),
        Err(_) => default_agents(),
    }
}

/// Persist the agent list and active selection.
#[tauri::command]
fn save_agents(store: AgentStore) {
    if let Ok(text) = serde_json::to_string_pretty(&store) {
        let _ = std::fs::write(agents_file(), text);
    }
}

// ---- Chat transcript persistence -----------------------------------------

fn safe_id(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect()
}

#[tauri::command]
fn load_chats() -> Vec<serde_json::Value> {
    let mut chats: Vec<serde_json::Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(chats_dir()) {
        for entry in entries.flatten() {
            if entry.path().extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(entry.path()) {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                    chats.push(value);
                }
            }
        }
    }
    chats.sort_by(|a, b| {
        let ta = a.get("updatedAt").and_then(|v| v.as_i64()).unwrap_or(0);
        let tb = b.get("updatedAt").and_then(|v| v.as_i64()).unwrap_or(0);
        tb.cmp(&ta)
    });
    chats
}

#[tauri::command]
fn save_chat(id: String, data: serde_json::Value) {
    let id = safe_id(&id);
    if id.is_empty() {
        return;
    }
    if let Ok(text) = serde_json::to_string_pretty(&data) {
        let _ = std::fs::write(chats_dir().join(format!("{id}.json")), text);
    }
}

#[tauri::command]
fn delete_chat(id: String) {
    let id = safe_id(&id);
    if !id.is_empty() {
        let _ = std::fs::remove_file(chats_dir().join(format!("{id}.json")));
    }
}

// ---- Connection -----------------------------------------------------------

fn build_config(agent: &AgentProfile) -> BridgeConfig {
    let cwd = agent
        .cwd
        .as_ref()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_chat_workspace);
    let _ = std::fs::create_dir_all(&cwd);

    let mut sub = SubprocessConfig::command(agent.command.clone(), agent.args.clone(), cwd);
    for e in &agent.env {
        if !e.name.trim().is_empty() {
            sub.env
                .push((e.name.clone().into(), e.value.clone().into()));
        }
    }

    // A macOS .app launched from Finder inherits a minimal PATH and won't find
    // user-installed CLIs. Augment PATH with common locations unless the agent
    // profile sets its own PATH.
    if !agent.env.iter().any(|e| e.name == "PATH") {
        let home = std::env::var("HOME").unwrap_or_default();
        let existing = std::env::var("PATH").unwrap_or_default();
        let path = format!("{home}/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:{existing}");
        sub.env.push(("PATH".into(), path.into()));
    }

    // Auto-approve policy comes from acp_settings.json (if present).
    let launch_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let settings = Settings::load(launch_dir.join("acp_settings.json")).unwrap_or_default();

    BridgeConfig {
        subprocess: sub,
        settings,
    }
}

/// (Re)connect to the given agent, replacing any existing connection.
#[tauri::command]
fn connect(app: AppHandle, backend: State<Backend>, agent: AgentProfile) {
    // Tear down any existing connection first.
    if let Some(old) = backend.handle.lock().unwrap().take() {
        old.shutdown_and_join();
    }
    *backend.commands.lock().unwrap() = None;

    let mut handle = match bridge::start(build_config(&agent)) {
        Ok(h) => h,
        Err(e) => {
            let _ = app.emit(
                "acp-event",
                Event::Disconnected {
                    message: format!("failed to start agent '{}': {e}", agent.name),
                },
            );
            return;
        }
    };

    *backend.commands.lock().unwrap() = Some(handle.commands());
    let mut events = match handle.take_events() {
        Some(rx) => rx,
        None => return,
    };
    *backend.handle.lock().unwrap() = Some(handle);

    // Forward protocol events to the frontend.
    std::thread::spawn(move || loop {
        match events.try_recv() {
            Ok(ev) => {
                let _ = app.emit("acp-event", ev);
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
        }
    });
}

/// Tear down the current agent connection.
#[tauri::command]
fn disconnect(app: AppHandle, backend: State<Backend>) {
    if let Some(old) = backend.handle.lock().unwrap().take() {
        old.shutdown_and_join();
    }
    *backend.commands.lock().unwrap() = None;
    let _ = app.emit(
        "acp-event",
        Event::Disconnected {
            message: "Disconnected".into(),
        },
    );
}

#[tauri::command]
fn create_session(backend: State<Backend>) {
    backend.send(Command::CreateSession);
}

#[tauri::command]
fn send_prompt(backend: State<Backend>, session_id: String, text: String, paths: Vec<String>) {
    let attachments: Vec<Attachment> = paths.into_iter().map(Attachment::from_path).collect();
    backend.send(Command::SendPrompt {
        session_id,
        text,
        attachments,
    });
}

#[tauri::command]
fn cancel(backend: State<Backend>, session_id: String) {
    backend.send(Command::Cancel { session_id });
}

#[tauri::command]
fn permission_decision(backend: State<Backend>, request_id: u64, option_id: Option<String>) {
    backend.send(Command::PermissionDecision {
        request_id,
        option_id,
    });
}

#[tauri::command]
fn set_model(backend: State<Backend>, session_id: String, model_id: String) {
    backend.send(Command::SetModel {
        session_id,
        model_id,
    });
}

/// Open a native file picker and return the chosen absolute paths.
#[tauri::command]
async fn pick_files() -> Vec<String> {
    match rfd::AsyncFileDialog::new().pick_files().await {
        Some(files) => files
            .into_iter()
            .map(|f| f.path().display().to_string())
            .collect(),
        None => Vec::new(),
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tauri::Builder::default()
        .manage(Backend::default())
        .invoke_handler(tauri::generate_handler![
            connect,
            disconnect,
            create_session,
            send_prompt,
            cancel,
            permission_decision,
            set_model,
            pick_files,
            load_chats,
            save_chat,
            delete_chat,
            load_agents,
            save_agents
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
