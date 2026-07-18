//! Shared protocol and coordination primitives for worktree merge consensus.

pub mod hash;
pub mod prompts;
pub mod protocol;
pub mod state;

pub use hash::canonical_json_hash;
