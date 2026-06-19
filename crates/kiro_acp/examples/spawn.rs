//! Task 2 demo: launch a stub agent (`cat`), write a line, print the echo.
//!
//! Run with: `cargo run -p kiro_acp --example spawn`

use futures::{AsyncReadExt, AsyncWriteExt};
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
            // `cat` stands in for a real agent: it echoes stdin to stdout.
            let config = SubprocessConfig::command("cat", Vec::<String>::new(), cwd);
            let (proc, mut stdin, mut stdout) = Subprocess::spawn(&config)?;

            let line = "hello from KiroUI\n";
            print!("-> writing: {line}");
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await?;

            let mut buf = vec![0u8; line.len()];
            stdout.read_exact(&mut buf).await?;
            print!("<- echoed:  {}", String::from_utf8_lossy(&buf));

            drop(stdin);
            let status = proc.shutdown().await?;
            println!("subprocess exited: {status:?}");
            Ok::<(), anyhow::Error>(())
        })
        .await?;
    Ok(())
}
