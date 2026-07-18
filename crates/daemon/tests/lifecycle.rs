#![cfg(unix)]

use std::{path::PathBuf, time::Duration};

use consensus_daemon::{
    lifecycle::{EnsureDaemonOptions, LifecycleError, ensure_daemon_with_options},
    server::{ServerConfig, run_server},
    store::SqliteRunStore,
};
use tokio::sync::oneshot;

#[tokio::test]
async fn live_daemon_is_reused_without_spawning_the_configured_executable() {
    let temp = tempfile::tempdir().unwrap();
    let config = ServerConfig::new(temp.path());
    let store = SqliteRunStore::open(&config.database_path).unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(run_server(config.clone(), store, shutdown_rx));
    wait_for_socket(&config.socket_path).await;

    let options = EnsureDaemonOptions {
        executable: PathBuf::from("/definitely/missing/codex-consensus"),
        startup_timeout: Duration::from_millis(250),
        poll_interval: Duration::from_millis(10),
    };
    let client = ensure_daemon_with_options(&config, options).await.unwrap();

    client.ping().await.unwrap();
    shutdown_tx.send(()).unwrap();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn launcher_exit_before_readiness_is_reported() {
    let temp = tempfile::tempdir().unwrap();
    let config = ServerConfig::new(temp.path());
    let options = EnsureDaemonOptions {
        executable: PathBuf::from("/usr/bin/false"),
        startup_timeout: Duration::from_secs(1),
        poll_interval: Duration::from_millis(10),
    };

    let error = ensure_daemon_with_options(&config, options)
        .await
        .unwrap_err();

    assert!(matches!(error, LifecycleError::ExitedBeforeReady { .. }));
}

async fn wait_for_socket(socket: &std::path::Path) {
    for _ in 0..100 {
        if socket.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("socket was not created at {}", socket.display());
}
