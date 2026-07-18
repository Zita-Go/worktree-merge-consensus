use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppEvent {
    pub id: Option<Value>,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InitializeInfo {
    pub raw: Value,
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
        self.status.get("type").and_then(Value::as_str) == Some("active")
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnHandle {
    pub id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub items: Vec<Value>,
}
