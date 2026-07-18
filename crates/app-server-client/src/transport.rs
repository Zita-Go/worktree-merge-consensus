use std::{collections::HashMap, sync::Arc};

use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    sync::{Mutex, mpsc, oneshot},
};

use crate::types::AppEvent;

type PendingResult = Result<Value, RpcError>;
type PendingMap = HashMap<u64, oneshot::Sender<PendingResult>>;

#[derive(Debug, Clone, PartialEq, Error)]
pub enum RpcError {
    #[error("JSON-RPC remote error {code}: {message}")]
    Remote {
        code: i64,
        message: String,
        data: Option<Value>,
    },
    #[error("JSON-RPC transport I/O failed: {0}")]
    Io(String),
    #[error("invalid JSON-RPC message: {0}")]
    Protocol(String),
    #[error("JSON-RPC transport closed")]
    Closed,
}

#[derive(Clone)]
pub struct JsonRpcTransport {
    inner: Arc<Inner>,
}

struct Inner {
    writer: Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
    pending: Mutex<PendingMap>,
    events_tx: mpsc::UnboundedSender<AppEvent>,
    events_rx: Mutex<mpsc::UnboundedReceiver<AppEvent>>,
    next_id: std::sync::atomic::AtomicU64,
}

impl JsonRpcTransport {
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            writer: Mutex::new(Box::new(writer)),
            pending: Mutex::new(HashMap::new()),
            events_tx,
            events_rx: Mutex::new(events_rx),
            next_id: std::sync::atomic::AtomicU64::new(1),
        });
        tokio::spawn(read_loop(reader, Arc::clone(&inner)));
        Self { inner }
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        if method.is_empty() {
            return Err(RpcError::Protocol(
                "request method must not be empty".to_owned(),
            ));
        }
        let id = self
            .inner
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, sender);

        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if let Err(error) = self.write_message(&message).await {
            self.inner.pending.lock().await.remove(&id);
            return Err(error);
        }

        receiver.await.map_err(|_| RpcError::Closed)?
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), RpcError> {
        if method.is_empty() {
            return Err(RpcError::Protocol(
                "notification method must not be empty".to_owned(),
            ));
        }
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }

    pub async fn respond(&self, id: Value, result: Value) -> Result<(), RpcError> {
        if !id.is_number() && !id.is_string() {
            return Err(RpcError::Protocol(
                "response id must be a number or string".to_owned(),
            ));
        }
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }))
        .await
    }

    pub async fn next_event(&self) -> Option<AppEvent> {
        self.inner.events_rx.lock().await.recv().await
    }

    async fn write_message(&self, message: &Value) -> Result<(), RpcError> {
        let mut encoded =
            serde_json::to_vec(message).map_err(|error| RpcError::Protocol(error.to_string()))?;
        encoded.push(b'\n');
        let mut writer = self.inner.writer.lock().await;
        writer
            .write_all(&encoded)
            .await
            .map_err(|error| RpcError::Io(error.to_string()))?;
        writer
            .flush()
            .await
            .map_err(|error| RpcError::Io(error.to_string()))
    }
}

async fn read_loop<R>(reader: R, inner: Arc<Inner>)
where
    R: AsyncRead + Send + Unpin + 'static,
{
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => match serde_json::from_str::<Value>(&line) {
                Ok(message) => dispatch_message(&inner, message).await,
                Err(error) => {
                    let _ = inner.events_tx.send(AppEvent {
                        id: None,
                        method: "transport/error".to_owned(),
                        params: json!({"message": format!("invalid JSON: {error}")}),
                    });
                }
            },
            Ok(None) => {
                fail_pending(&inner, RpcError::Closed).await;
                let _ = inner.events_tx.send(AppEvent {
                    id: None,
                    method: "transport/closed".to_owned(),
                    params: Value::Null,
                });
                break;
            }
            Err(error) => {
                fail_pending(&inner, RpcError::Io(error.to_string())).await;
                let _ = inner.events_tx.send(AppEvent {
                    id: None,
                    method: "transport/error".to_owned(),
                    params: json!({"message": error.to_string()}),
                });
                break;
            }
        }
    }
}

async fn dispatch_message(inner: &Arc<Inner>, message: Value) {
    if let Some(method) = message.get("method").and_then(Value::as_str) {
        let _ = inner.events_tx.send(AppEvent {
            id: message.get("id").cloned(),
            method: method.to_owned(),
            params: message.get("params").cloned().unwrap_or(Value::Null),
        });
        return;
    }

    let Some(id) = message.get("id").and_then(Value::as_u64) else {
        let _ = inner.events_tx.send(AppEvent {
            id: None,
            method: "transport/error".to_owned(),
            params: json!({"message": "response is missing a numeric id"}),
        });
        return;
    };
    let Some(sender) = inner.pending.lock().await.remove(&id) else {
        return;
    };

    let result = if let Some(value) = message.get("result") {
        Ok(value.clone())
    } else if let Some(error) = message.get("error") {
        Err(RpcError::Remote {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(-32_603),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown remote error")
                .to_owned(),
            data: error.get("data").cloned(),
        })
    } else {
        Err(RpcError::Protocol(
            "response has neither result nor error".to_owned(),
        ))
    };
    let _ = sender.send(result);
}

async fn fail_pending(inner: &Arc<Inner>, error: RpcError) {
    let pending = std::mem::take(&mut *inner.pending.lock().await);
    for (_, sender) in pending {
        let _ = sender.send(Err(error.clone()));
    }
}
