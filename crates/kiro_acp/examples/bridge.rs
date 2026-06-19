//! Task 5 demo: run a full prompt round-trip against the real `kiro-cli`
//! purely through the bridge's [`Command`]/[`Event`] channel API.
//!
//! Run with: `cargo run -p kiro_acp --example bridge -- "your prompt"`

use kiro_acp::bridge::{self, BridgeConfig};
use kiro_acp::protocol::{Command, Event};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Reply with exactly: pong".to_string());

    let cwd = std::env::current_dir()?;
    let mut handle = bridge::start(BridgeConfig::kiro(cwd))?;
    let mut events = handle.take_events().expect("events");

    let mut session_id: Option<String> = None;
    print!("Kiro: ");
    use std::io::Write as _;

    while let Some(event) = events.recv().await {
        match event {
            Event::Connected {
                protocol_version, ..
            } => {
                eprintln!("[connected: protocol v{protocol_version}]");
                handle.send(Command::CreateSession);
            }
            Event::SessionCreated { session_id: id } => {
                eprintln!("[session: {id}]");
                session_id = Some(id.clone());
                handle.send(Command::SendPrompt {
                    session_id: id,
                    text: prompt.clone(),
                    attachments: vec![],
                });
            }
            Event::MessageChunk { text, .. } => {
                print!("{text}");
                let _ = std::io::stdout().flush();
            }
            Event::ThoughtChunk { text, .. } => eprintln!("\n[thought] {text}"),
            Event::ToolCall { title, .. } => eprintln!("\n[tool] {title}"),
            Event::ToolCallUpdate { status, .. } => {
                eprintln!("[tool-update] {status:?}")
            }
            Event::PermissionRequested {
                request_id,
                options,
                title,
                ..
            } => {
                eprintln!("\n[permission requested: {title}] auto-approving");
                let allow = options
                    .iter()
                    .find(|o| o.kind.contains("allow"))
                    .map(|o| o.id.clone());
                handle.send(Command::PermissionDecision {
                    request_id,
                    option_id: allow,
                });
            }
            Event::TurnEnded { stop_reason, .. } => {
                println!("\n[stop: {stop_reason}]");
                break;
            }
            Event::Error { message } | Event::Disconnected { message } => {
                eprintln!("\n[error: {message}]");
                break;
            }
            _ => {}
        }
    }

    let _ = session_id;
    handle.shutdown_and_join();
    Ok(())
}
