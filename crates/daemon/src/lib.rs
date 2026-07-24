//! Persistent coordinator daemon for worktree merge consensus.

pub mod coordinator;
pub mod lifecycle;
pub mod observation;
mod participant_binding;
mod policy;
pub mod server;
pub mod store;
pub mod wire;

pub use participant_binding::{PrimaryBindingMode, PrimaryParticipantBinding};
