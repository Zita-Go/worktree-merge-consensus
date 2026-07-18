use std::{fs, path::PathBuf};

use serde_json::json;
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::oneshot,
};

use crate::{
    store::{SqliteRunStore, StoreError},
    wire::{DaemonRequest, DaemonResponse, ping_result},
};

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

    let result = loop {
        tokio::select! {
            _ = &mut shutdown => break Ok(()),
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let store = store.clone();
                        tokio::spawn(async move {
                            let _ = serve_connection(stream, store).await;
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

#[cfg(unix)]
async fn serve_connection(
    stream: tokio::net::UnixStream,
    store: SqliteRunStore,
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
                Ok(request) => handle_request(&store, request),
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

fn handle_request(store: &SqliteRunStore, request: DaemonRequest) -> DaemonResponse {
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
        DaemonRequest::Start { state } => {
            let run_id = state.facts.run_id.to_string();
            match store.insert_run(&state) {
                Ok(()) => DaemonResponse::success(json!({"run_id": run_id})),
                Err(error) => store_failure(error),
            }
        }
        DaemonRequest::Resume { run_id } => match store.load_run(&run_id) {
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
        },
        DaemonRequest::Cancel { run_id } => match store.load_run(&run_id) {
            Ok(Some(mut state)) => {
                state.cancel();
                match store.save_state(&state) {
                    Ok(()) => serialize_success(&state),
                    Err(error) => store_failure(error),
                }
            }
            Ok(None) => DaemonResponse::failure("RUN_NOT_FOUND", "run does not exist"),
            Err(error) => store_failure(error),
        },
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

fn remove_if_exists(path: &std::path::Path) -> Result<(), std::io::Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
