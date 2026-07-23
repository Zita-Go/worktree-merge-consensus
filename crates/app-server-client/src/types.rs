use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppEvent {
    pub id: Option<Value>,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeInfo {
    pub codex_home: PathBuf,
    pub platform_family: String,
    pub platform_os: String,
    pub user_agent: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnExecutionPolicy {
    ReadOnly {
        cwd: PathBuf,
    },
    PrimaryIntegration {
        cwd: PathBuf,
        git_common_dir: PathBuf,
    },
    PrimaryVerification {
        cwd: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantMcpConfig {
    pub participant_executable: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadResumePolicy {
    Default,
    Participant(ParticipantMcpConfig),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadForkPolicy {
    EphemeralParticipant(ParticipantMcpConfig),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadRuntimeStatus {
    NotLoaded,
    Idle,
    Active,
    SystemError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandExecRequest {
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub timeout_ms: u64,
    pub output_bytes_cap: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSummary {
    pub id: String,
    #[serde(default)]
    pub cwd: PathBuf,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub preview: String,
    #[serde(default)]
    pub cli_version: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub status: Value,
    #[serde(default)]
    pub source: Value,
}

impl ThreadSummary {
    pub fn is_active(&self) -> bool {
        self.runtime_status() == Ok(ThreadRuntimeStatus::Active)
    }

    pub fn runtime_status(&self) -> Result<ThreadRuntimeStatus, String> {
        match self.status.get("type").and_then(Value::as_str) {
            Some("notLoaded") => Ok(ThreadRuntimeStatus::NotLoaded),
            Some("idle") => Ok(ThreadRuntimeStatus::Idle),
            Some("active") => Ok(ThreadRuntimeStatus::Active),
            Some("systemError") => Ok(ThreadRuntimeStatus::SystemError),
            Some(status) => Err(format!("unsupported thread status: {status}")),
            None => Err("thread status is missing a string type".to_owned()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadDetail {
    pub summary: ThreadSummary,
    pub turns: Vec<Value>,
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadPage {
    pub data: Vec<ThreadSummary>,
    pub next_cursor: Option<String>,
    pub backwards_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct McpServerStatus {
    pub name: String,
    pub tools: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnHandle {
    pub id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub items: Vec<Value>,
}
