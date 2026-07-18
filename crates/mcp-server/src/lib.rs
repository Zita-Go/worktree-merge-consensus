mod tools;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

pub use tools::{MCP_TOOL_NAMES, tool_definitions};

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{code}: {message}")]
pub struct BackendError {
    code: String,
    message: String,
}

impl BackendError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[async_trait]
pub trait ToolBackend: Send + Sync + 'static {
    async fn call(&self, tool: &str, arguments: Value) -> Result<Value, BackendError>;
}

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("MCP stdio I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("MCP response serialization failed: {0}")]
    Json(#[from] serde_json::Error),
}

pub async fn serve<R, W>(
    mut reader: R,
    mut writer: W,
    backend: Arc<dyn ToolBackend>,
) -> Result<(), ServerError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            break;
        }

        let response = if line.len() > MAX_MESSAGE_BYTES {
            Some(error_response(
                Value::Null,
                -32600,
                format!("request exceeds {MAX_MESSAGE_BYTES} bytes"),
            ))
        } else {
            match serde_json::from_str::<Value>(&line) {
                Ok(request) => handle_request(request, backend.as_ref()).await,
                Err(error) => Some(error_response(
                    Value::Null,
                    -32700,
                    format!("parse error: {error}"),
                )),
            }
        };

        if let Some(response) = response {
            let mut encoded = serde_json::to_vec(&response)?;
            encoded.push(b'\n');
            writer.write_all(&encoded).await?;
            writer.flush().await?;
        }
    }
    Ok(())
}

pub async fn serve_stdio(backend: Arc<dyn ToolBackend>) -> Result<(), ServerError> {
    serve(
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
        backend,
    )
    .await
}

async fn handle_request(request: Value, backend: &dyn ToolBackend) -> Option<Value> {
    let Some(object) = request.as_object() else {
        return Some(error_response(
            Value::Null,
            -32600,
            "request must be a JSON object",
        ));
    };
    let id = object.get("id").cloned();
    let response_id = id.clone().unwrap_or(Value::Null);
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return id.map(|_| error_response(response_id, -32600, "jsonrpc must equal \"2.0\""));
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return id.map(|_| error_response(response_id, -32600, "method must be a string"));
    };

    match method {
        "notifications/initialized" => None,
        "initialize" => id.map(|_| match validate_initialize_params(object.get("params")) {
            Ok(()) => success_response(
                response_id,
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {
                        "name": "worktree-merge-consensus",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            ),
            Err(message) => error_response(response_id, -32602, message),
        }),
        "ping" => id.map(|_| match validate_empty_params(object.get("params")) {
            Ok(()) => success_response(response_id, json!({})),
            Err(message) => error_response(response_id, -32602, message),
        }),
        "tools/list" => id.map(|_| match validate_empty_params(object.get("params")) {
            Ok(()) => success_response(response_id, json!({"tools": tool_definitions()})),
            Err(message) => error_response(response_id, -32602, message),
        }),
        "tools/call" => {
            let _ = id?;
            Some(match parse_tool_call(object.get("params")) {
                Ok((name, arguments)) => match backend.call(&name, arguments).await {
                    Ok(value) => success_response(response_id, successful_tool_result(value)),
                    Err(error) => success_response(response_id, failed_tool_result(&error)),
                },
                Err(message) => error_response(response_id, -32602, message),
            })
        }
        _ => id.map(|_| error_response(response_id, -32601, "method not found")),
    }
}

fn validate_initialize_params(params: Option<&Value>) -> Result<(), String> {
    let object = params
        .and_then(Value::as_object)
        .ok_or_else(|| "initialize params must be an object".to_owned())?;
    match object.get("protocolVersion").and_then(Value::as_str) {
        Some(MCP_PROTOCOL_VERSION) => Ok(()),
        Some(version) => Err(format!("unsupported MCP protocol version {version}")),
        None => Err("protocolVersion must be a string".into()),
    }
}

fn validate_empty_params(params: Option<&Value>) -> Result<(), String> {
    match params {
        None => Ok(()),
        Some(Value::Object(object)) if object.is_empty() => Ok(()),
        _ => Err("params must be an empty object".into()),
    }
}

fn parse_tool_call(params: Option<&Value>) -> Result<(String, Value), String> {
    let object = params
        .and_then(Value::as_object)
        .ok_or_else(|| "tools/call params must be an object".to_owned())?;
    reject_unknown_fields(object, &["name", "arguments"])?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "tool name must be a non-empty string".to_owned())?;
    let arguments = object
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let arguments = tools::validate_arguments(name, arguments)?;
    Ok((name.to_owned(), arguments))
}

fn reject_unknown_fields(object: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    if let Some(field) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("unknown field {field}"));
    }
    Ok(())
}

fn successful_tool_result(value: Value) -> Value {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    let structured = if value.is_object() {
        value
    } else {
        json!({"result": value})
    };
    json!({
        "content": [{"type": "text", "text": text}],
        "structuredContent": structured,
        "isError": false
    })
}

fn failed_tool_result(error: &BackendError) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": format!("{}: {}", error.code(), error.message())
        }],
        "structuredContent": {
            "ok": false,
            "error": {"code": error.code(), "message": error.message()}
        },
        "isError": true
    })
}

fn success_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message.into()}
    })
}
