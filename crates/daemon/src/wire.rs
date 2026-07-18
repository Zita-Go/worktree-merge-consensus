use std::path::{Path, PathBuf};

use consensus_core::state::RunState;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::coordinator::StartRequest;

const MAX_WIRE_LINE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum DaemonRequest {
    Ping,
    Status {
        run_id: Option<String>,
    },
    Start {
        state: Box<RunState>,
        request: StartRequest,
    },
    Resume {
        run_id: String,
    },
    Cancel {
        run_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<DaemonResponseError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonResponseError {
    pub code: String,
    pub message: String,
}

impl DaemonResponse {
    pub fn success(result: Value) -> Self {
        Self {
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(DaemonResponseError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Error)]
pub enum DaemonClientError {
    #[error("daemon I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("daemon wire JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("daemon returned no response")]
    EmptyResponse,
    #[error("daemon response exceeded {MAX_WIRE_LINE_BYTES} bytes")]
    ResponseTooLarge,
    #[error("Unix sockets are required by worktree-merge-consensus v1")]
    UnsupportedPlatform,
}

#[derive(Debug, Clone)]
pub struct DaemonClient {
    socket_path: PathBuf,
}

impl DaemonClient {
    pub fn new(socket_path: impl AsRef<Path>) -> Self {
        Self {
            socket_path: socket_path.as_ref().to_owned(),
        }
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub async fn ping(&self) -> Result<(), DaemonClientError> {
        let response = self.request(DaemonRequest::Ping).await?;
        if response.ok {
            Ok(())
        } else {
            Err(DaemonClientError::EmptyResponse)
        }
    }

    #[cfg(unix)]
    pub async fn request(
        &self,
        request: DaemonRequest,
    ) -> Result<DaemonResponse, DaemonClientError> {
        let mut stream = tokio::net::UnixStream::connect(&self.socket_path).await?;
        let mut encoded = serde_json::to_vec(&request)?;
        encoded.push(b'\n');
        stream.write_all(&encoded).await?;
        stream.shutdown().await?;

        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).await?;
        if line.is_empty() {
            return Err(DaemonClientError::EmptyResponse);
        }
        if line.len() > MAX_WIRE_LINE_BYTES {
            return Err(DaemonClientError::ResponseTooLarge);
        }
        serde_json::from_str(&line).map_err(Into::into)
    }

    #[cfg(not(unix))]
    pub async fn request(
        &self,
        _request: DaemonRequest,
    ) -> Result<DaemonResponse, DaemonClientError> {
        Err(DaemonClientError::UnsupportedPlatform)
    }
}

pub(crate) fn ping_result() -> Value {
    json!({
        "name": "worktree-merge-consensus",
        "version": env!("CARGO_PKG_VERSION"),
    })
}
