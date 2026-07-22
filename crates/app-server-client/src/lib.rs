//! Codex App Server transport and typed client.

pub mod client;
pub mod compat;
pub mod transport;
pub mod types;

pub use client::{
    AppServer, AppServerError, CONTROLLED_PATCH_APPROVAL_KEY, CONTROLLED_PATCH_APPROVAL_MODE,
    CodexAppServer, ConnectOptions, ReconnectingCodexAppServer,
};
pub use types::{
    AppEvent, CommandExecRequest, CommandExecResult, InitializeInfo, ThreadDetail, ThreadPage,
    ThreadSummary, TurnExecutionPolicy, TurnHandle,
};
