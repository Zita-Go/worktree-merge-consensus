#![cfg(unix)]

use std::{os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

use consensus_core::state::{RunFacts, RunState};
use consensus_daemon::{
    server::{ServerConfig, ServerError, run_server},
    store::SqliteRunStore,
    wire::{DaemonClient, DaemonRequest},
};
use tokio::sync::oneshot;
use uuid::Uuid;

const RUN_ID: &str = "4b230bd8-d870-4ef4-bf20-05a4c61020af";

#[tokio::test]
async fn unix_socket_is_private_and_status_round_trips() {
    let temp = tempfile::tempdir().unwrap();
    let config = ServerConfig::new(temp.path());
    let store = SqliteRunStore::open(&config.database_path).unwrap();
    store
        .insert_run(&fixture_run(RUN_ID, "/repo/.git"))
        .unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let mut server = tokio::spawn(run_server(config.clone(), store, shutdown_rx));
    let client = wait_for_daemon(&config.socket_path, &mut server).await;

    let mode = std::fs::metadata(&config.socket_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);

    let response = client
        .request(DaemonRequest::Status {
            run_id: Some(RUN_ID.into()),
        })
        .await
        .unwrap();
    assert!(response.ok);
    assert_eq!(response.result.unwrap()["status"], "RUNNING");

    shutdown_tx.send(()).unwrap();
    server.await.unwrap().unwrap();
    assert!(!config.socket_path.exists());
}

#[tokio::test]
async fn start_rpc_rejects_second_active_run_for_repository() {
    let temp = tempfile::tempdir().unwrap();
    let config = ServerConfig::new(temp.path());
    let store = SqliteRunStore::open(&config.database_path).unwrap();
    store
        .insert_run(&fixture_run(RUN_ID, "/repo/.git"))
        .unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let mut server = tokio::spawn(run_server(config.clone(), store, shutdown_rx));
    let client = wait_for_daemon(&config.socket_path, &mut server).await;
    let second = fixture_run("9a0ca0d8-8dd4-4c96-aae8-0a8896464c45", "/repo/.git");

    let response = client
        .request(DaemonRequest::Start {
            state: Box::new(second),
        })
        .await
        .unwrap();

    assert!(!response.ok);
    assert_eq!(response.error.unwrap().code, "ACTIVE_RUN_EXISTS");
    shutdown_tx.send(()).unwrap();
    server.await.unwrap().unwrap();
}

async fn wait_for_daemon(
    socket: &std::path::Path,
    server: &mut tokio::task::JoinHandle<Result<(), ServerError>>,
) -> DaemonClient {
    let mut last_error = None;
    for _ in 0..100 {
        if server.is_finished() {
            let outcome = server.await;
            panic!("daemon exited before becoming ready: {outcome:?}");
        }
        let client = DaemonClient::new(socket);
        match client.ping().await {
            Ok(()) => return client,
            Err(error) => last_error = Some(error.to_string()),
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "daemon did not become ready at {}; socket_exists={}; last_error={:?}",
        socket.display(),
        socket.exists(),
        last_error
    );
}

fn fixture_run(run_id: &str, common_dir: &str) -> RunState {
    RunState::new(RunFacts {
        run_id: Uuid::parse_str(run_id).unwrap(),
        primary_thread_id: "primary-thread".into(),
        reviewer_thread_id: "reviewer-thread".into(),
        primary_worktree: PathBuf::from("/repo/primary"),
        reviewer_worktree: PathBuf::from("/repo/reviewer"),
        git_common_dir: PathBuf::from(common_dir),
        primary_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        reviewer_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
        primary_ref: Some("refs/heads/primary".into()),
        reviewer_ref: Some("refs/heads/reviewer".into()),
    })
}
