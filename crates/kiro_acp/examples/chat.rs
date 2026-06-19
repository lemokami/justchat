//! Task 4 demo: create a session against the real `kiro-cli acp`, send a
//! prompt, and print the assembled agent response plus the stop reason.
//!
//! Run with: `cargo run -p kiro_acp --example chat -- "your prompt"`

use kiro_acp::protocol::{self, Event};
use kiro_acp::{Subprocess, SubprocessConfig};

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
        .unwrap_or_else(|| "Say hello in one short sentence.".to_string());

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let cwd = std::env::current_dir()?;
            let config = SubprocessConfig::kiro(cwd.clone());
            let (proc, outgoing, incoming) = Subprocess::spawn(&config)?;

            let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
            let (conn, _shared, io) = protocol::new_connection(event_tx, outgoing, incoming);
            tokio::task::spawn_local(async move {
                if let Err(e) = io.await {
                    tracing::warn!("io task ended: {e}");
                }
            });

            // Print streamed chunks as they arrive on a background task.
            tokio::task::spawn_local(async move {
                while let Some(ev) = event_rx.recv().await {
                    match ev {
                        Event::MessageChunk { text, .. } => {
                            print!("{text}");
                            use std::io::Write as _;
                            let _ = std::io::stdout().flush();
                        }
                        Event::ThoughtChunk { text, .. } => eprintln!("[thought] {text}"),
                        Event::ToolCall { title, .. } => eprintln!("[tool] {title}"),
                        _ => {}
                    }
                }
            });

            protocol::initialize(&conn).await?;
            let session_id = protocol::create_session(&conn, cwd).await?.id;
            eprintln!("session: {session_id}");
            println!("> {prompt}");
            print!("Kiro: ");

            let stop = protocol::send_prompt(&conn, &session_id, prompt, vec![]).await?;
            println!("\n[stop reason: {stop}]");

            let _ = proc.shutdown().await;
            Ok::<(), anyhow::Error>(())
        })
        .await?;
    Ok(())
}
