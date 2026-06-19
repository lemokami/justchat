//! Subprocess lifecycle management for the `kiro-cli acp` agent.
//!
//! [`Subprocess`] spawns the agent with piped stdio, exposes the streams as
//! `futures`-compatible async readers/writers (so the ACP connection can use
//! them directly), forwards the child's stderr to `tracing`, and terminates the
//! child gracefully on [`Subprocess::shutdown`] or drop.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncBufReadExt;
use tokio::process::{Child, Command};
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::error::{AcpError, Result};

/// How long to wait for the agent to exit after closing its stdin before we
/// forcibly kill it during a graceful shutdown.
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(1500);

/// Configuration describing how to launch the agent subprocess.
///
/// The program/args/cwd/env are all injectable so tests can substitute a fake
/// agent (or a plain `cat`) for the real `kiro-cli`.
#[derive(Debug, Clone)]
pub struct SubprocessConfig {
    /// The program to execute.
    pub program: OsString,
    /// Arguments passed to the program.
    pub args: Vec<OsString>,
    /// Working directory for the child (also used as the ACP session cwd).
    pub cwd: PathBuf,
    /// Extra environment variables to set for the child.
    pub env: Vec<(OsString, OsString)>,
}

impl SubprocessConfig {
    /// Build a config that launches `kiro-cli acp` in the given working dir.
    pub fn kiro(cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: OsString::from("kiro-cli"),
            args: vec![OsString::from("acp")],
            cwd: cwd.into(),
            env: Vec::new(),
        }
    }

    /// Build a config for an arbitrary program (used in tests).
    pub fn command(
        program: impl Into<OsString>,
        args: impl IntoIterator<Item = impl Into<OsString>>,
        cwd: impl Into<PathBuf>,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            cwd: cwd.into(),
            env: Vec::new(),
        }
    }

    /// Add an environment variable.
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

/// The writable side of the agent's stdin, as a `futures::AsyncWrite`.
pub type AgentStdin = Compat<tokio::process::ChildStdin>;
/// The readable side of the agent's stdout, as a `futures::AsyncRead`.
pub type AgentStdout = Compat<tokio::process::ChildStdout>;

/// A spawned agent subprocess together with its (compat-wrapped) stdio streams.
pub struct Subprocess {
    child: Child,
}

impl Subprocess {
    /// Spawn the agent described by `config`.
    ///
    /// Returns the [`Subprocess`] handle (for lifecycle control) plus the
    /// outgoing (stdin) and incoming (stdout) streams to hand to an ACP
    /// connection. The child's stderr is drained to `tracing` in the
    /// background.
    pub fn spawn(config: &SubprocessConfig) -> Result<(Self, AgentStdin, AgentStdout)> {
        if !config.cwd.exists() {
            return Err(AcpError::CwdMissing(config.cwd.clone()));
        }

        let mut command = Command::new(&config.program);
        command
            .args(&config.args)
            .current_dir(&config.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in &config.env {
            command.env(k, v);
        }

        let mut child = command.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AcpError::ProgramNotFound {
                    program: config.program.to_string_lossy().into_owned(),
                }
            } else {
                AcpError::Spawn(e)
            }
        })?;

        let stdin = child
            .stdin
            .take()
            .expect("child spawned with piped stdin")
            .compat_write();
        let stdout = child
            .stdout
            .take()
            .expect("child spawned with piped stdout")
            .compat();

        if let Some(stderr) = child.stderr.take() {
            let program = config.program.to_string_lossy().into_owned();
            tokio::task::spawn(drain_stderr(program, stderr));
        }

        tracing::info!(
            program = %config.program.to_string_lossy(),
            cwd = %config.cwd.display(),
            "spawned agent subprocess"
        );

        Ok((Self { child }, stdin, stdout))
    }

    /// Whether the child has already exited (non-blocking check).
    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Gracefully terminate the agent.
    ///
    /// Closing stdin was already handled by dropping the writer; here we wait a
    /// bounded amount of time for the child to exit on its own, then kill it if
    /// it overstays. Returns the exit status if the process had/has one.
    pub async fn shutdown(mut self) -> Result<Option<std::process::ExitStatus>> {
        // Give the agent a chance to exit cleanly after its stdin closed.
        match tokio::time::timeout(GRACEFUL_SHUTDOWN_TIMEOUT, self.child.wait()).await {
            Ok(status) => {
                let status = status?;
                tracing::info!(?status, "agent subprocess exited gracefully");
                Ok(Some(status))
            }
            Err(_elapsed) => {
                tracing::warn!("agent subprocess did not exit in time; killing");
                // `start_kill` then await to reap and avoid a zombie.
                let _ = self.child.start_kill();
                let status = self.child.wait().await.ok();
                Ok(status)
            }
        }
    }
}

/// Forward each line the child writes to stderr into `tracing` at warn level.
async fn drain_stderr(program: String, stderr: tokio::process::ChildStderr) {
    let mut lines = tokio::io::BufReader::new(stderr).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => tracing::warn!(target: "kiro_acp::agent_stderr", %program, "{line}"),
            Ok(None) => break,
            Err(e) => {
                tracing::debug!(%program, "error reading agent stderr: {e}");
                break;
            }
        }
    }
}

// Note: `kill_on_drop(true)` ensures the OS process is signalled when the
// `Child` is dropped, so callers that skip `shutdown()` still don't leak the
// subprocess.
