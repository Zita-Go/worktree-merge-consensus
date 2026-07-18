use std::{
    ffi::OsString,
    path::PathBuf,
    process::{ExitStatus, Stdio},
    time::Duration,
};

use thiserror::Error;
use tokio::{process::Command, time::Instant};

use crate::{
    server::ServerConfig,
    wire::{DaemonClient, DaemonClientError},
};

const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsureDaemonOptions {
    pub executable: PathBuf,
    pub startup_timeout: Duration,
    pub poll_interval: Duration,
}

impl EnsureDaemonOptions {
    pub fn for_current_executable() -> Result<Self, LifecycleError> {
        Ok(Self {
            executable: std::env::current_exe().map_err(LifecycleError::CurrentExecutable)?,
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            poll_interval: DEFAULT_POLL_INTERVAL,
        })
    }
}

#[derive(Debug, Error)]
pub enum LifecycleError {
    #[error("cannot locate the current executable: {0}")]
    CurrentExecutable(std::io::Error),
    #[error("cannot start consensus daemon with {executable}: {source}")]
    Spawn {
        executable: String,
        #[source]
        source: std::io::Error,
    },
    #[error("consensus daemon at {socket} is not safely reachable: {source}")]
    Unreachable {
        socket: String,
        #[source]
        source: DaemonClientError,
    },
    #[error(
        "consensus daemon launcher exited before readiness with {status}; last connection error: {last_error}"
    )]
    ExitedBeforeReady {
        status: ExitStatus,
        last_error: String,
    },
    #[error(
        "consensus daemon did not become ready at {socket} within {timeout_ms} ms; last connection error: {last_error}"
    )]
    StartupTimedOut {
        socket: String,
        timeout_ms: u128,
        last_error: String,
    },
    #[error("neither XDG_STATE_HOME nor HOME is available")]
    StateHomeUnavailable,
}

pub fn default_state_dir() -> Result<PathBuf, LifecycleError> {
    if let Some(path) = nonempty_env("XDG_STATE_HOME") {
        return Ok(PathBuf::from(path).join("codex-consensus"));
    }
    nonempty_env("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".local/state/codex-consensus"))
        .ok_or(LifecycleError::StateHomeUnavailable)
}

pub async fn ensure_daemon(config: &ServerConfig) -> Result<DaemonClient, LifecycleError> {
    ensure_daemon_with_options(config, EnsureDaemonOptions::for_current_executable()?).await
}

pub async fn ensure_daemon_with_options(
    config: &ServerConfig,
    options: EnsureDaemonOptions,
) -> Result<DaemonClient, LifecycleError> {
    let client = DaemonClient::new(&config.socket_path);
    let initial_error = match client.ping().await {
        Ok(()) => return Ok(client),
        Err(error) if is_daemon_absent(&error) => error.to_string(),
        Err(source) => {
            return Err(LifecycleError::Unreachable {
                socket: config.socket_path.display().to_string(),
                source,
            });
        }
    };

    let mut child = Command::new(&options.executable)
        .arg("daemon")
        .arg("serve")
        .arg("--state-dir")
        .arg(&config.state_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false)
        .spawn()
        .map_err(|source| LifecycleError::Spawn {
            executable: options.executable.display().to_string(),
            source,
        })?;

    let started = Instant::now();
    let mut last_error = initial_error;
    if let Some(status) = child.try_wait().map_err(|source| LifecycleError::Spawn {
        executable: options.executable.display().to_string(),
        source,
    })? {
        return Err(LifecycleError::ExitedBeforeReady { status, last_error });
    }
    loop {
        match client.ping().await {
            Ok(()) => {
                tokio::spawn(async move {
                    let _ = child.wait().await;
                });
                return Ok(client);
            }
            Err(error) if is_daemon_absent(&error) => {
                last_error = error.to_string();
            }
            Err(source) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(LifecycleError::Unreachable {
                    socket: config.socket_path.display().to_string(),
                    source,
                });
            }
        }

        if let Some(status) = child.try_wait().map_err(|source| LifecycleError::Spawn {
            executable: options.executable.display().to_string(),
            source,
        })? {
            return Err(LifecycleError::ExitedBeforeReady { status, last_error });
        }

        if started.elapsed() >= options.startup_timeout {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(LifecycleError::StartupTimedOut {
                socket: config.socket_path.display().to_string(),
                timeout_ms: options.startup_timeout.as_millis(),
                last_error,
            });
        }
        tokio::time::sleep(options.poll_interval).await;
    }
}

fn is_daemon_absent(error: &DaemonClientError) -> bool {
    matches!(
        error,
        DaemonClientError::Io(source)
            if matches!(
                source.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            )
    )
}

fn nonempty_env(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}
