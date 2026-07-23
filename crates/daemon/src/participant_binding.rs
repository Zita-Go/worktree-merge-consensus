use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PrimaryBindingMode {
    Direct,
    EphemeralFork,
}

impl PrimaryBindingMode {
    pub(crate) fn as_database_value(self) -> &'static str {
        match self {
            Self::Direct => "DIRECT",
            Self::EphemeralFork => "EPHEMERAL_FORK",
        }
    }

    pub(crate) fn from_database_value(value: &str) -> Option<Self> {
        match value {
            "DIRECT" => Some(Self::Direct),
            "EPHEMERAL_FORK" => Some(Self::EphemeralFork),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrimaryParticipantBinding {
    pub run_id: String,
    pub source_primary_thread_id: String,
    pub effective_primary_thread_id: String,
    pub mode: PrimaryBindingMode,
    pub generation: u32,
    pub participant_server: String,
    pub source_history_hash: Option<String>,
    pub created_at: i64,
    pub verified_at: i64,
}
