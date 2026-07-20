use std::{
    collections::VecDeque,
    fmt, io,
    path::PathBuf,
    pin::Pin,
    process::Stdio,
    sync::{Arc, Mutex as StdMutex},
    task::{Context, Poll},
    time::Duration,
};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    io::{
        AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf, duplex, split,
    },
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    time::timeout,
};
use tokio_tungstenite::{WebSocketStream, client_async, tungstenite::Message};

use crate::{
    compat::{check_compatibility, parse_managed_user_agent},
    transport::{JsonRpcTransport, RpcError},
    types::{
        AppEvent, InitializeInfo, ThreadDetail, ThreadPage, ThreadSummary, TurnExecutionPolicy,
        TurnHandle,
    },
};

const STDERR_TAIL_LINES: usize = 40;
const PROXY_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct ConnectOptions {
    pub codex_binary: PathBuf,
    pub control_socket: Option<PathBuf>,
    pub start_daemon: bool,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            codex_binary: PathBuf::from("codex"),
            control_socket: None,
            start_daemon: true,
        }
    }
}

#[derive(Debug, Error)]
pub enum AppServerError {
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("invalid App Server response: {0}")]
    InvalidResponse(String),
    #[error("Codex process failed: {0}")]
    Process(String),
    #[error("INCOMPATIBLE_CODEX: {0}")]
    IncompatibleCodex(String),
}

#[async_trait]
pub trait AppServer: Send + Sync {
    async fn initialize(&self) -> Result<InitializeInfo, AppServerError>;
    async fn list_threads(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<ThreadPage, AppServerError>;
    async fn read_thread(&self, thread_id: &str) -> Result<ThreadDetail, AppServerError>;
    async fn resume_thread(&self, thread_id: &str) -> Result<ThreadDetail, AppServerError>;
    async fn start_turn(
        &self,
        thread_id: &str,
        prompt: &str,
        policy: &TurnExecutionPolicy,
    ) -> Result<TurnHandle, AppServerError>;
    async fn respond_to_request(&self, id: Value, result: Value) -> Result<(), AppServerError>;
    async fn next_event(&self) -> Option<AppEvent>;
}

#[derive(Clone)]
pub struct CodexAppServer {
    transport: JsonRpcTransport,
    process: Option<Arc<ProcessGuard>>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
}

pub struct ReconnectingCodexAppServer {
    options: ConnectOptions,
    inner: Mutex<CodexAppServer>,
}

impl CodexAppServer {
    pub fn from_transport(transport: JsonRpcTransport) -> Self {
        Self {
            transport,
            process: None,
            stderr_tail: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub fn transport(&self) -> &JsonRpcTransport {
        &self.transport
    }

    pub async fn connect(options: ConnectOptions) -> Result<Self, AppServerError> {
        let version = Command::new(&options.codex_binary)
            .arg("--version")
            .output()
            .await
            .map_err(|error| {
                AppServerError::Process(format!("could not execute codex --version: {error}"))
            })?;
        if !version.status.success() {
            return Err(AppServerError::Process(format!(
                "codex --version exited with {:?}",
                version.status.code()
            )));
        }
        let version_output = String::from_utf8(version.stdout).map_err(|error| {
            AppServerError::Process(format!("codex --version returned invalid UTF-8: {error}"))
        })?;
        let compatibility = check_compatibility(&version_output);
        if !compatibility.compatible {
            return Err(AppServerError::IncompatibleCodex(compatibility.detail));
        }

        if options.start_daemon {
            let started = Command::new(&options.codex_binary)
                .args(["app-server", "daemon", "start"])
                .output()
                .await
                .map_err(|error| {
                    AppServerError::Process(format!(
                        "could not start the managed App Server daemon: {error}"
                    ))
                })?;
            if !started.status.success() {
                return Err(AppServerError::Process(format!(
                    "App Server daemon start failed: {}",
                    redact_stderr(&String::from_utf8_lossy(&started.stderr))
                )));
            }
        }

        let mut command = Command::new(&options.codex_binary);
        command
            .args(["app-server", "proxy"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(socket) = options.control_socket {
            command.arg("--sock").arg(socket);
        }
        let mut child = command.spawn().map_err(|error| {
            AppServerError::Process(format!("could not start App Server proxy: {error}"))
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            AppServerError::Process("App Server proxy stdin was unavailable".to_owned())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            AppServerError::Process("App Server proxy stdout was unavailable".to_owned())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            AppServerError::Process("App Server proxy stderr was unavailable".to_owned())
        })?;

        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        tokio::spawn(drain_stderr(stderr, Arc::clone(&stderr_tail)));
        let proxy_stream = ProxyStdio { stdin, stdout };
        let (websocket, _) = timeout(
            PROXY_HANDSHAKE_TIMEOUT,
            client_async("ws://localhost/", proxy_stream),
        )
        .await
        .map_err(|_| {
            AppServerError::Process(
                "timed out waiting for App Server proxy WebSocket handshake".to_owned(),
            )
        })?
        .map_err(|error| {
            AppServerError::Process(format!(
                "App Server proxy WebSocket handshake failed: {error}"
            ))
        })?;
        let client = Self {
            transport: websocket_json_transport(websocket),
            process: Some(Arc::new(ProcessGuard {
                child: StdMutex::new(Some(child)),
            })),
            stderr_tail,
        };
        let initialized = timeout(PROXY_HANDSHAKE_TIMEOUT, client.initialize())
            .await
            .map_err(|_| {
                AppServerError::Process(
                    "timed out waiting for App Server initialize response".to_owned(),
                )
            })??;
        let installed_version = compatibility.installed_version.as_deref().ok_or_else(|| {
            AppServerError::IncompatibleCodex(
                "compatible version report omitted the installed version".to_owned(),
            )
        })?;
        validate_managed_identity(&initialized, installed_version)?;
        Ok(client)
    }

    pub async fn redacted_stderr_tail(&self) -> Vec<String> {
        self.stderr_tail.lock().await.iter().cloned().collect()
    }

    async fn shutdown_proxy(&self) {
        if let Some(process) = &self.process {
            process.shutdown().await;
        }
    }

    async fn rpc_request(&self, method: &str, params: Value) -> Result<Value, AppServerError> {
        match self.transport.request(method, params).await {
            Ok(value) => Ok(value),
            Err(RpcError::Remote {
                code: -32_601,
                message,
                ..
            }) => Err(AppServerError::IncompatibleCodex(format!(
                "required App Server method {method} is unavailable: {message}"
            ))),
            Err(error) => Err(error.into()),
        }
    }
}

struct ProxyStdio {
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl AsyncRead for ProxyStdio {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stdout).poll_read(context, buffer)
    }
}

impl AsyncWrite for ProxyStdio {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stdin).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stdin).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stdin).poll_shutdown(context)
    }
}

fn websocket_json_transport(websocket: WebSocketStream<ProxyStdio>) -> JsonRpcTransport {
    let (transport_side, bridge_side) = duplex(256 * 1024);
    let (transport_reader, transport_writer) = split(transport_side);
    let (bridge_reader, bridge_writer) = split(bridge_side);
    let transport = JsonRpcTransport::new(transport_reader, transport_writer);
    tokio::spawn(run_websocket_bridge(
        websocket,
        bridge_reader,
        bridge_writer,
        transport.clone(),
    ));
    transport
}

async fn run_websocket_bridge(
    websocket: WebSocketStream<ProxyStdio>,
    bridge_reader: tokio::io::ReadHalf<tokio::io::DuplexStream>,
    mut bridge_writer: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    transport: JsonRpcTransport,
) {
    let (mut websocket_writer, mut websocket_reader) = websocket.split();
    let mut outgoing = BufReader::new(bridge_reader).lines();
    loop {
        tokio::select! {
            line = outgoing.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        if let Err(error) = websocket_writer.send(Message::Text(line)).await {
                            transport.close_with_error(RpcError::Io(format!(
                                "App Server proxy WebSocket send failed: {error}"
                            ))).await;
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        transport.close_with_error(RpcError::Io(format!(
                            "App Server proxy outbound bridge failed: {error}"
                        ))).await;
                        break;
                    }
                }
            }
            message = websocket_reader.next() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let delivered = async {
                            bridge_writer.write_all(text.as_bytes()).await?;
                            bridge_writer.write_all(b"\n").await?;
                            bridge_writer.flush().await
                        }.await;
                        if let Err(error) = delivered {
                            transport.close_with_error(RpcError::Io(format!(
                                "App Server proxy inbound bridge failed: {error}"
                            ))).await;
                            break;
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if let Err(error) = websocket_writer.send(Message::Pong(payload)).await {
                            transport.close_with_error(RpcError::Io(format!(
                                "App Server proxy WebSocket pong failed: {error}"
                            ))).await;
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | None => {
                        transport.close_with_error(RpcError::Closed).await;
                        break;
                    }
                    Some(Err(error)) => {
                        transport.close_with_error(RpcError::Protocol(format!(
                            "App Server proxy WebSocket failed: {error}"
                        ))).await;
                        break;
                    }
                    Some(Ok(Message::Binary(_))) => {
                        transport.close_with_error(RpcError::Protocol(
                            "App Server proxy sent an unsupported binary WebSocket frame".to_owned(),
                        )).await;
                        break;
                    }
                    Some(Ok(Message::Frame(_))) => {
                        transport.close_with_error(RpcError::Protocol(
                            "App Server proxy sent an unsupported raw WebSocket frame".to_owned(),
                        )).await;
                        break;
                    }
                }
            }
        }
    }
}

impl fmt::Debug for CodexAppServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexAppServer")
            .field("managed_process", &self.process.is_some())
            .finish_non_exhaustive()
    }
}

struct ProcessGuard {
    child: StdMutex<Option<Child>>,
}

impl ProcessGuard {
    async fn shutdown(&self) {
        let child = self.child.lock().ok().and_then(|mut child| child.take());
        if let Some(mut child) = child {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            if let Some(mut child) = child.take() {
                let _ = child.start_kill();
            }
        }
    }
}

impl ReconnectingCodexAppServer {
    pub async fn connect(options: ConnectOptions) -> Result<Self, AppServerError> {
        let client = CodexAppServer::connect(options.clone()).await?;
        Ok(Self {
            options,
            inner: Mutex::new(client),
        })
    }

    async fn reconnect_locked(
        &self,
        client: &mut CodexAppServer,
        operation: &str,
        original_error: &AppServerError,
    ) -> Result<(), AppServerError> {
        let replacement = CodexAppServer::connect(self.options.clone())
            .await
            .map_err(|reconnect_error| {
                AppServerError::Process(format!(
                    "{operation} failed because the managed App Server connection closed ({original_error}); reconnecting failed: {reconnect_error}"
                ))
            })?;
        client.shutdown_proxy().await;
        *client = replacement;
        Ok(())
    }
}

fn reconnectable(error: &AppServerError) -> bool {
    matches!(
        error,
        AppServerError::Rpc(RpcError::Io(_) | RpcError::Protocol(_) | RpcError::Closed)
    )
}

impl fmt::Debug for ReconnectingCodexAppServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReconnectingCodexAppServer")
            .field("codex_binary", &self.options.codex_binary)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl AppServer for ReconnectingCodexAppServer {
    async fn initialize(&self) -> Result<InitializeInfo, AppServerError> {
        let mut client = self.inner.lock().await;
        match client.initialize().await {
            Ok(info) => Ok(info),
            Err(error) if reconnectable(&error) => {
                self.reconnect_locked(&mut client, "initialize", &error)
                    .await?;
                client.initialize().await
            }
            Err(error) => Err(error),
        }
    }

    async fn list_threads(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<ThreadPage, AppServerError> {
        let mut client = self.inner.lock().await;
        match client.list_threads(cursor.clone(), limit).await {
            Ok(page) => Ok(page),
            Err(error) if reconnectable(&error) => {
                self.reconnect_locked(&mut client, "thread/list", &error)
                    .await?;
                client.list_threads(cursor, limit).await
            }
            Err(error) => Err(error),
        }
    }

    async fn read_thread(&self, thread_id: &str) -> Result<ThreadDetail, AppServerError> {
        let mut client = self.inner.lock().await;
        match client.read_thread(thread_id).await {
            Ok(detail) => Ok(detail),
            Err(error) if reconnectable(&error) => {
                self.reconnect_locked(&mut client, "thread/read", &error)
                    .await?;
                client.read_thread(thread_id).await
            }
            Err(error) => Err(error),
        }
    }

    async fn resume_thread(&self, thread_id: &str) -> Result<ThreadDetail, AppServerError> {
        let mut client = self.inner.lock().await;
        match client.resume_thread(thread_id).await {
            Ok(detail) => Ok(detail),
            Err(error) if reconnectable(&error) => {
                self.reconnect_locked(&mut client, "thread/resume", &error)
                    .await?;
                client.resume_thread(thread_id).await
            }
            Err(error) => Err(error),
        }
    }

    async fn start_turn(
        &self,
        thread_id: &str,
        prompt: &str,
        policy: &TurnExecutionPolicy,
    ) -> Result<TurnHandle, AppServerError> {
        let mut client = self.inner.lock().await;
        if client.transport().is_closed().await {
            let error = AppServerError::Rpc(RpcError::Closed);
            self.reconnect_locked(&mut client, "turn/start preflight", &error)
                .await?;
        }
        client.start_turn(thread_id, prompt, policy).await
    }

    async fn respond_to_request(&self, id: Value, result: Value) -> Result<(), AppServerError> {
        self.inner.lock().await.respond_to_request(id, result).await
    }

    async fn next_event(&self) -> Option<AppEvent> {
        self.inner.lock().await.next_event().await
    }
}

#[async_trait]
impl AppServer for CodexAppServer {
    async fn initialize(&self) -> Result<InitializeInfo, AppServerError> {
        let raw = self
            .rpc_request(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "worktree-merge-consensus",
                        "title": "Worktree Merge Consensus",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "capabilities": {
                        "experimentalApi": true,
                    },
                }),
            )
            .await?;
        let initialized = serde_json::from_value::<InitializeInfo>(raw).map_err(|error| {
            AppServerError::IncompatibleCodex(format!(
                "initialize response does not match the pinned schema: {error}"
            ))
        })?;
        validate_initialize_shape(&initialized)?;
        self.transport.notify("initialized", json!({})).await?;
        Ok(initialized)
    }

    async fn list_threads(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<ThreadPage, AppServerError> {
        let raw = self
            .rpc_request(
                "thread/list",
                json!({
                    "cursor": cursor,
                    "limit": limit,
                    "sortKey": "updated_at",
                    "sortDirection": "desc",
                }),
            )
            .await?;
        let data = raw
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("thread/list result is missing data"))?
            .iter()
            .cloned()
            .map(|thread| {
                serde_json::from_value(thread)
                    .map_err(|error| invalid(format!("invalid thread summary: {error}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ThreadPage {
            data,
            next_cursor: optional_string(&raw, "nextCursor")?,
            backwards_cursor: optional_string(&raw, "backwardsCursor")?,
        })
    }

    async fn read_thread(&self, thread_id: &str) -> Result<ThreadDetail, AppServerError> {
        let raw = self
            .rpc_request(
                "thread/read",
                json!({"threadId": thread_id, "includeTurns": true}),
            )
            .await?;
        parse_thread_response(raw)
    }

    async fn resume_thread(&self, thread_id: &str) -> Result<ThreadDetail, AppServerError> {
        let raw = self
            .rpc_request("thread/resume", json!({"threadId": thread_id}))
            .await?;
        parse_thread_response(raw)
    }

    async fn start_turn(
        &self,
        thread_id: &str,
        prompt: &str,
        policy: &TurnExecutionPolicy,
    ) -> Result<TurnHandle, AppServerError> {
        let (cwd, runtime_workspace_roots, approval_policy, sandbox_policy) =
            turn_policy_params(policy)?;
        let raw = self
            .rpc_request(
                "turn/start",
                json!({
                    "threadId": thread_id,
                    "input": [{
                        "type": "text",
                        "text": prompt,
                        "text_elements": [],
                    }],
                    "cwd": cwd,
                    "runtimeWorkspaceRoots": runtime_workspace_roots,
                    "approvalPolicy": approval_policy,
                    "approvalsReviewer": "user",
                    "environments": [],
                    "sandboxPolicy": sandbox_policy,
                }),
            )
            .await?;
        let turn = raw
            .get("turn")
            .cloned()
            .ok_or_else(|| invalid("turn/start result is missing turn"))?;
        serde_json::from_value(turn)
            .map_err(|error| invalid(format!("invalid turn/start result: {error}")))
    }

    async fn next_event(&self) -> Option<AppEvent> {
        self.transport.next_event().await
    }

    async fn respond_to_request(&self, id: Value, result: Value) -> Result<(), AppServerError> {
        self.transport.respond(id, result).await.map_err(Into::into)
    }
}

fn turn_policy_params(
    policy: &TurnExecutionPolicy,
) -> Result<(Value, Value, Value, Value), AppServerError> {
    let absolute = |path: &PathBuf, label: &str| {
        if path.is_absolute() {
            Ok(path.clone())
        } else {
            Err(AppServerError::IncompatibleCodex(format!(
                "{label} must be an absolute path"
            )))
        }
    };
    match policy {
        TurnExecutionPolicy::ReadOnly { cwd } => {
            let cwd = absolute(cwd, "turn cwd")?;
            Ok((
                json!(cwd),
                json!([cwd]),
                json!("never"),
                json!({"type": "readOnly", "networkAccess": false}),
            ))
        }
        TurnExecutionPolicy::PrimaryIntegration {
            cwd,
            git_common_dir,
        } => {
            let cwd = absolute(cwd, "integration cwd")?;
            let git_common_dir = absolute(git_common_dir, "Git common directory")?;
            Ok((
                json!(cwd),
                json!([cwd]),
                json!("untrusted"),
                json!({
                    "type": "workspaceWrite",
                    "writableRoots": [cwd, git_common_dir],
                    "networkAccess": false,
                    "excludeSlashTmp": true,
                    "excludeTmpdirEnvVar": true,
                }),
            ))
        }
        TurnExecutionPolicy::PrimaryVerification { cwd } => {
            let cwd = absolute(cwd, "verification cwd")?;
            Ok((
                json!(cwd),
                json!([cwd]),
                json!("untrusted"),
                json!({
                    "type": "workspaceWrite",
                    "writableRoots": [cwd],
                    "networkAccess": false,
                    "excludeSlashTmp": false,
                    "excludeTmpdirEnvVar": false,
                }),
            ))
        }
    }
}

fn validate_initialize_shape(info: &InitializeInfo) -> Result<(), AppServerError> {
    if !info.codex_home.is_absolute() {
        return Err(AppServerError::IncompatibleCodex(
            "initialize.codexHome must be absolute".to_owned(),
        ));
    }
    if info.platform_family != "unix" {
        return Err(AppServerError::IncompatibleCodex(format!(
            "initialize.platformFamily must be unix, found {}",
            info.platform_family
        )));
    }
    if info.platform_os.trim().is_empty() || info.user_agent.trim().is_empty() {
        return Err(AppServerError::IncompatibleCodex(
            "initialize response contains an empty platformOs or userAgent".to_owned(),
        ));
    }
    Ok(())
}

fn validate_managed_identity(
    info: &InitializeInfo,
    installed_version: &str,
) -> Result<(), AppServerError> {
    let managed_version = parse_managed_user_agent(&info.user_agent).ok_or_else(|| {
        AppServerError::IncompatibleCodex(
            "initialize.userAgent is not a recognized managed Codex identity".to_owned(),
        )
    })?;
    if managed_version.to_string() != installed_version {
        return Err(AppServerError::IncompatibleCodex(format!(
            "initialize.userAgent version {managed_version} does not match codex --version {installed_version}"
        )));
    }
    Ok(())
}

fn parse_thread_response(raw: Value) -> Result<ThreadDetail, AppServerError> {
    let thread = raw
        .get("thread")
        .cloned()
        .ok_or_else(|| invalid("thread response is missing thread"))?;
    let summary = serde_json::from_value::<ThreadSummary>(thread.clone())
        .map_err(|error| invalid(format!("invalid thread response: {error}")))?;
    let turns = thread
        .get("turns")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(ThreadDetail {
        summary,
        turns,
        raw: thread,
    })
}

fn optional_string(value: &Value, key: &str) -> Result<Option<String>, AppServerError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(invalid(format!("{key} must be a string or null"))),
    }
}

fn invalid(detail: impl Into<String>) -> AppServerError {
    AppServerError::InvalidResponse(detail.into())
}

async fn drain_stderr(stderr: tokio::process::ChildStderr, tail: Arc<Mutex<VecDeque<String>>>) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut tail = tail.lock().await;
        if tail.len() == STDERR_TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(redact_stderr(&line));
    }
}

fn redact_stderr(value: &str) -> String {
    let lowercase = value.to_ascii_lowercase();
    if ["authorization", "api_key", "api-key", "secret", "token"]
        .iter()
        .any(|marker| lowercase.contains(marker))
    {
        return "[redacted sensitive App Server diagnostic]".to_owned();
    }
    let mut redacted = value.to_owned();
    if let Some(home) = std::env::var_os("HOME").and_then(|home| home.into_string().ok()) {
        redacted = redacted.replace(&home, "~");
    }
    redacted.chars().take(2_000).collect()
}
