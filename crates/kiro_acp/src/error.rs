//! Error types for the `kiro_acp` crate.

use std::path::PathBuf;

/// Errors that can arise while spawning or driving the `kiro-cli acp` agent.
#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    /// The agent program could not be found on the system.
    #[error("agent program not found: {program} (is kiro-cli installed and on PATH?)")]
    ProgramNotFound {
        /// The program name/path we attempted to spawn.
        program: String,
    },

    /// The configured working directory does not exist.
    #[error("working directory does not exist: {0}")]
    CwdMissing(PathBuf),

    /// Spawning the subprocess failed for some other I/O reason.
    #[error("failed to spawn agent subprocess: {0}")]
    Spawn(#[source] std::io::Error),

    /// A generic I/O error while communicating with the subprocess.
    #[error("subprocess I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The ACP protocol layer returned an error.
    #[error("acp protocol error: {0}")]
    Protocol(String),

    /// The connection to the agent was lost (subprocess exited or stream closed).
    #[error("connection to agent lost: {0}")]
    ConnectionLost(String),

    /// A catch-all for other failures.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<agent_client_protocol::Error> for AcpError {
    fn from(e: agent_client_protocol::Error) -> Self {
        AcpError::Protocol(format!("{} (code {})", e.message, e.code))
    }
}

/// Convenience result alias for the crate.
pub type Result<T> = std::result::Result<T, AcpError>;
