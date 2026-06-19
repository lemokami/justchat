//! Integration tests for the subprocess wrapper (Task 2).
//!
//! Uses the system `cat`, which echoes stdin to stdout, to verify a byte
//! round-trip through the compat-wrapped streams and that the child is reaped
//! on shutdown.

use futures::{AsyncReadExt, AsyncWriteExt};
use kiro_acp::{Subprocess, SubprocessConfig};

#[tokio::test(flavor = "current_thread")]
async fn cat_echoes_bytes_and_shuts_down() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let cwd = std::env::current_dir().unwrap();
            let config = SubprocessConfig::command("cat", Vec::<String>::new(), cwd);

            let (mut proc, mut stdin, mut stdout) = Subprocess::spawn(&config).expect("spawn cat");

            // Write a line and read it back (cat echoes).
            stdin.write_all(b"hello kiro\n").await.unwrap();
            stdin.flush().await.unwrap();

            let mut buf = [0u8; 11];
            stdout.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"hello kiro\n");

            assert!(!proc.has_exited(), "cat should still be running");

            // Dropping stdin closes cat's input; it should then exit on its own.
            drop(stdin);
            let status = proc.shutdown().await.unwrap();
            assert!(status.is_some(), "child should have a final exit status");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn missing_program_reports_not_found() {
    let cwd = std::env::current_dir().unwrap();
    let config = SubprocessConfig::command(
        "definitely-not-a-real-program-xyz",
        Vec::<String>::new(),
        cwd,
    );
    match Subprocess::spawn(&config) {
        Err(kiro_acp::AcpError::ProgramNotFound { .. }) => {}
        Err(other) => panic!("expected ProgramNotFound, got: {other:?}"),
        Ok(_) => panic!("expected spawn to fail"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn missing_cwd_is_rejected() {
    let config = SubprocessConfig::command("cat", Vec::<String>::new(), "/no/such/dir/xyz123");
    match Subprocess::spawn(&config) {
        Err(kiro_acp::AcpError::CwdMissing(_)) => {}
        Err(other) => panic!("expected CwdMissing, got: {other:?}"),
        Ok(_) => panic!("expected spawn to fail"),
    }
}
