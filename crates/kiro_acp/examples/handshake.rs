//! Task 3 demo: connect to the real `kiro-cli acp`, run the `initialize`
//! handshake, and print the negotiated protocol version and capabilities.
//!
//! Run with: `cargo run -p kiro_acp --example handshake`

use kiro_acp::protocol;
use kiro_acp::{Subprocess, SubprocessConfig};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let cwd = std::env::current_dir()?;
            let config = SubprocessConfig::kiro(cwd);
            let (proc, outgoing, incoming) = Subprocess::spawn(&config)?;

            let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
            let (conn, _shared, io) = protocol::new_connection(event_tx, outgoing, incoming);
            tokio::task::spawn_local(async move {
                if let Err(e) = io.await {
                    tracing::warn!("io task ended: {e}");
                }
            });

            let info = protocol::initialize(&conn).await?;
            println!("== ACP handshake ==");
            println!("protocol version : {}", info.protocol_version);
            println!("supported (v1)   : {}", info.supported);
            println!("loadSession      : {}", info.load_session);

            let status = proc.shutdown().await?;
            println!("agent exited     : {status:?}");
            Ok::<(), anyhow::Error>(())
        })
        .await?;
    Ok(())
}
