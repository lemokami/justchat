//! `acpc_core` — framework-independent application state for JustChat.
//!
//! Holds the [`AppState`] command/event state machine, free of any GPUI types
//! so it can be unit-tested headlessly. The `acpc_app` crate wraps this in a
//! GPUI entity.

pub mod app_state;
pub mod highlight;
pub mod markdown;

pub use app_state::{
    AgentStatus, AppState, ConnectionStatus, Message, PermissionPrompt, Role, Session, ToolCallView,
};
