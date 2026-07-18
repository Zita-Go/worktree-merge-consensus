use std::{fs, path::PathBuf};

use app_server_client::AppServer;
use async_trait::async_trait;
use consensus_core::state::RunState;
use serde_json::json;
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::oneshot,
};

use crate::{
    coordinator::{Coordinator, CoordinatorError, RepositorySafety, StartRequest},
    store::{SqliteRunStore, StoreError},
    wire::{DaemonRequest, DaemonResponse, ping_result},
};

#[async_trait]
pub trait RunController: Send + Sync {
    async fn start_run(
        &self,
        state: RunState,
        request: StartRequest,
    ) -> Result<RunState, CoordinatorError>;
    async fn drive_run(&self, run_id: &str) -> Result<RunState, CoordinatorError>;
    async fn prepare_resume_run(&self, run_id: &str) -> Result<RunState, CoordinatorError>;
    async fn cancel_run(&self, run_id: &str) -> Result<RunState, CoordinatorError>;
}

#[async_trait]
impl<A, R> RunController for Coordinator<A, R>
where
    A: AppServer + 'static,
    R: RepositorySafety + 'static,
{
    async fn start_run(
        &self,
        state: RunState,
        request: StartRequest,
    ) -> Result<RunState, CoordinatorError> {
        Coordinator::start(self, state, request).await
    }

    async fn drive_run(&self, run_id: &str) -> Result<RunState, CoordinatorError> {
        Coordinator::drive(self, run_id).await
    }

    async fn prepare_resume_run(&self, run_id: &str) -> Result<RunState, CoordinatorError> {
        Coordinator::prepare_resume(self, run_id).await
    }

    async fn cancel_run(&self, run_id: &str) -> Result<RunState, CoordinatorError> {
        Coordinator::cancel(self, run_id).await
    }
}

const MAX_REQUEST_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub state_dir: PathBuf,
    pub database_path: PathBuf,
    pub socket_path: PathBuf,
    pub pid_path: PathBuf,
}

impl ServerConfig {
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        let state_dir = state_dir.into();
        Self {
            database_path: state_dir.join("state.db"),
            socket_path: state_dir.join("daemon.sock"),
            pid_path: state_dir.join("daemon.pid"),
            state_dir,
        }
    }
}

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("daemon I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("daemon state failed: {0}")]
    Store(#[from] StoreError),
    #[error("another daemon is already listening at {0}")]
    AlreadyRunning(String),
    #[error("Unix sockets are required by worktree-merge-consensus v1")]
    UnsupportedPlatform,
}

#[cfg(unix)]
pub async fn run_server(
    config: ServerConfig,
    store: SqliteRunStore,
    shutdown: oneshot::Receiver<()>,
) -> Result<(), ServerError> {
    run_server_inner(config, store, None, shutdown).await
}

#[cfg(unix)]
pub async fn run_server_with_controller(
    config: ServerConfig,
    store: SqliteRunStore,
    controller: std::sync::Arc<dyn RunController>,
    shutdown: oneshot::Receiver<()>,
) -> Result<(), ServerError> {
    run_server_inner(config, store, Some(controller), shutdown).await
}

#[cfg(unix)]
async fn run_server_inner(
    config: ServerConfig,
    store: SqliteRunStore,
    controller: Option<std::sync::Arc<dyn RunController>>,
    mut shutdown: oneshot::Receiver<()>,
) -> Result<(), ServerError> {
    use std::os::unix::fs::PermissionsExt;

    fs::create_dir_all(&config.state_dir)?;
    fs::set_permissions(&config.state_dir, fs::Permissions::from_mode(0o700))?;
    if config.socket_path.exists() {
        if tokio::net::UnixStream::connect(&config.socket_path)
            .await
            .is_ok()
        {
            return Err(ServerError::AlreadyRunning(
                config.socket_path.display().to_string(),
            ));
        }
        fs::remove_file(&config.socket_path)?;
    }

    let listener = tokio::net::UnixListener::bind(&config.socket_path)?;
    fs::set_permissions(&config.socket_path, fs::Permissions::from_mode(0o600))?;
    fs::write(&config.pid_path, format!("{}\n", std::process::id()))?;
    fs::set_permissions(&config.pid_path, fs::Permissions::from_mode(0o600))?;

    if let Some(controller) = &controller {
        for run in store
            .list_runs()?
            .into_iter()
            .filter(|run| matches!(run.status.as_str(), "RUNNING" | "WAITING_THREAD"))
        {
            dispatch_drive(std::sync::Arc::clone(controller), run.run_id);
        }
    }

    let result = loop {
        tokio::select! {
            _ = &mut shutdown => break Ok(()),
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let store = store.clone();
                        let controller = controller.clone();
                        tokio::spawn(async move {
                            let _ = serve_connection(stream, store, controller).await;
                        });
                    }
                    Err(error) => break Err(ServerError::Io(error)),
                }
            }
        }
    };

    drop(listener);
    remove_if_exists(&config.socket_path)?;
    remove_if_exists(&config.pid_path)?;
    result
}

#[cfg(not(unix))]
pub async fn run_server(
    _config: ServerConfig,
    _store: SqliteRunStore,
    _shutdown: oneshot::Receiver<()>,
) -> Result<(), ServerError> {
    Err(ServerError::UnsupportedPlatform)
}

#[cfg(not(unix))]
pub async fn run_server_with_controller(
    _config: ServerConfig,
    _store: SqliteRunStore,
    _controller: std::sync::Arc<dyn RunController>,
    _shutdown: oneshot::Receiver<()>,
) -> Result<(), ServerError> {
    Err(ServerError::UnsupportedPlatform)
}

#[cfg(unix)]
async fn serve_connection(
    stream: tokio::net::UnixStream,
    store: SqliteRunStore,
    controller: Option<std::sync::Arc<dyn RunController>>,
) -> Result<(), std::io::Error> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).await?;
        if bytes == 0 {
            break;
        }
        let response = if line.len() > MAX_REQUEST_BYTES {
            DaemonResponse::failure("REQUEST_TOO_LARGE", "daemon request exceeds 1 MiB")
        } else {
            match serde_json::from_str::<DaemonRequest>(&line) {
                Ok(request) => handle_request(&store, controller.as_ref(), request).await,
                Err(error) => DaemonResponse::failure("INVALID_REQUEST", error.to_string()),
            }
        };
        let mut encoded = match serde_json::to_vec(&response) {
            Ok(encoded) => encoded,
            Err(error) => serde_json::to_vec(&DaemonResponse::failure(
                "SERIALIZATION_ERROR",
                error.to_string(),
            ))
            .expect("error response is always serializable"),
        };
        encoded.push(b'\n');
        writer.write_all(&encoded).await?;
        writer.flush().await?;
    }
    Ok(())
}

async fn handle_request(
    store: &SqliteRunStore,
    controller: Option<&std::sync::Arc<dyn RunController>>,
    request: DaemonRequest,
) -> DaemonResponse {
    match request {
        DaemonRequest::Ping => DaemonResponse::success(ping_result()),
        DaemonRequest::Status { run_id } => match run_id {
            Some(run_id) => match store.load_run(&run_id) {
                Ok(Some(state)) => serialize_success(&state),
                Ok(None) => DaemonResponse::failure("RUN_NOT_FOUND", "run does not exist"),
                Err(error) => store_failure(error),
            },
            None => match store.list_runs() {
                Ok(runs) => serialize_success(&runs),
                Err(error) => store_failure(error),
            },
        },
        DaemonRequest::Start { state, request } => {
            let run_id = state.facts.run_id.to_string();
            if let Some(controller) = controller {
                match controller.start_run(*state, request).await {
                    Ok(state) => {
                        dispatch_drive(std::sync::Arc::clone(controller), run_id.clone());
                        DaemonResponse::success(json!({
                            "run_id": run_id,
                            "status": state.status,
                        }))
                    }
                    Err(error) => coordinator_failure(error),
                }
            } else {
                match store.insert_run(&state) {
                    Ok(()) => DaemonResponse::success(json!({"run_id": run_id})),
                    Err(error) => store_failure(error),
                }
            }
        }
        DaemonRequest::Resume { run_id } => {
            if let Some(controller) = controller {
                match controller.prepare_resume_run(&run_id).await {
                    Ok(state) => {
                        dispatch_drive(std::sync::Arc::clone(controller), run_id.clone());
                        serialize_success(&state)
                    }
                    Err(error) => coordinator_failure(error),
                }
            } else {
                match store.load_run(&run_id) {
                    Ok(Some(mut state)) => match state.resume() {
                        Ok(action) => match store.save_state(&state) {
                            Ok(()) => DaemonResponse::success(json!({
                                "run_id": run_id,
                                "next_action": action,
                                "state": state,
                            })),
                            Err(error) => store_failure(error),
                        },
                        Err(error) => DaemonResponse::failure(error.code(), error.to_string()),
                    },
                    Ok(None) => DaemonResponse::failure("RUN_NOT_FOUND", "run does not exist"),
                    Err(error) => store_failure(error),
                }
            }
        }
        DaemonRequest::Cancel { run_id } => {
            if let Some(controller) = controller {
                match controller.cancel_run(&run_id).await {
                    Ok(state) => serialize_success(&state),
                    Err(error) => coordinator_failure(error),
                }
            } else {
                match store.load_run(&run_id) {
                    Ok(Some(mut state)) => {
                        state.cancel();
                        match store.save_state(&state) {
                            Ok(()) => serialize_success(&state),
                            Err(error) => store_failure(error),
                        }
                    }
                    Ok(None) => DaemonResponse::failure("RUN_NOT_FOUND", "run does not exist"),
                    Err(error) => store_failure(error),
                }
            }
        }
    }
}

fn serialize_success(value: &impl serde::Serialize) -> DaemonResponse {
    match serde_json::to_value(value) {
        Ok(value) => DaemonResponse::success(value),
        Err(error) => DaemonResponse::failure("SERIALIZATION_ERROR", error.to_string()),
    }
}

fn store_failure(error: StoreError) -> DaemonResponse {
    DaemonResponse::failure(error.code(), error.to_string())
}

fn coordinator_failure(error: CoordinatorError) -> DaemonResponse {
    let code = error.code().to_owned();
    DaemonResponse::failure(code, error.to_string())
}

fn dispatch_drive(controller: std::sync::Arc<dyn RunController>, run_id: String) {
    tokio::spawn(async move {
        let _ = controller.drive_run(&run_id).await;
    });
}

fn remove_if_exists(path: &std::path::Path) -> Result<(), std::io::Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
