//! `acpc_protocol` — headless protocol and runtime layer for JustChat.
//!
//! This crate manages the `kiro-cli acp` subprocess and speaks the Agent
//! Client Protocol (ACP) over its stdio, exposing a small, serializable
//! command/event API that a UI (or a headless test) can drive without any
//! knowledge of the underlying protocol types.

pub mod attachment;
pub mod bridge;
pub mod error;
pub mod idshim;
pub mod protocol;
pub mod settings;
pub mod subprocess;
pub mod terminals;

pub use attachment::Attachment;
pub use bridge::{start, BridgeConfig, BridgeHandle, CommandSender};
pub use error::{AcpError, Result};
pub use protocol::{
    Command, ConnectedInfo, Event, EventReceiver, EventSender, KiroClient, PermissionOptionInfo,
};
pub use settings::{AutoApprove, Settings};
pub use subprocess::{AgentStdin, AgentStdout, Subprocess, SubprocessConfig};

/// Crate version string, exposed for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_present() {
        assert!(!VERSION.is_empty());
    }
}
