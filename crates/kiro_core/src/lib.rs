//! `kiro_core` — framework-independent application state for KiroUI.
//!
//! Holds the [`AppState`] command/event state machine, free of any GPUI types
//! so it can be unit-tested headlessly. The `kiro_ui` crate wraps this in a
//! GPUI entity.

pub mod app_state;
pub mod highlight;
pub mod markdown;

pub use app_state::{
    AgentStatus, AppState, ConnectionStatus, Message, PermissionPrompt, Role, Session, ToolCallView,
};
