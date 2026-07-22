#![cfg(unix)]

use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use consensus_core::state::{RunFacts, RunState};
use consensus_daemon::{
    coordinator::{CoordinatorError, IntegrationPatchResult, StartRequest},
    server::{RunController, ServerConfig, ServerError, run_server, run_server_with_controller},
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
            request: Default::default(),
        })
        .await
        .unwrap();

    assert!(!response.ok);
    assert_eq!(response.error.unwrap().code, "ACTIVE_RUN_EXISTS");
    shutdown_tx.send(()).unwrap();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn controller_backed_start_returns_immediately_and_dispatches_background_drive() {
    let temp = tempfile::tempdir().unwrap();
    let config = ServerConfig::new(temp.path());
    let store = SqliteRunStore::open(&config.database_path).unwrap();
    let controller = Arc::new(FakeRunController {
        store: store.clone(),
        drives: AtomicUsize::new(0),
        health_checks: AtomicUsize::new(0),
        startup_recoveries: AtomicUsize::new(0),
    });
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let mut server = tokio::spawn(run_server_with_controller(
        config.clone(),
        store.clone(),
        controller.clone(),
        shutdown_rx,
    ));
    let client = wait_for_daemon(&config.socket_path, &mut server).await;
    let health = client.request(DaemonRequest::Health).await.unwrap();
    assert!(health.ok);
    assert_eq!(controller.health_checks.load(Ordering::SeqCst), 1);
    let run = fixture_run(RUN_ID, "/repo/.git");

    let response = client
        .request(DaemonRequest::Start {
            state: Box::new(run),
            request: StartRequest::default(),
        })
        .await
        .unwrap();

    assert!(response.ok);
    for _ in 0..100 {
        if controller.drives.load(Ordering::SeqCst) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(controller.drives.load(Ordering::SeqCst), 1);
    assert!(store.load_run(RUN_ID).unwrap().is_some());
    let patch = client
        .request(DaemonRequest::ApplyPatch {
            run_id: RUN_ID.into(),
            request_hash: "request-hash".into(),
            patch: "diff --git a/src/lib.rs b/src/lib.rs".into(),
        })
        .await
        .unwrap();
    assert!(patch.ok);
    assert_eq!(
        patch.result.unwrap()["integration_branch"],
        "consensus/test"
    );
    shutdown_tx.send(()).unwrap();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn daemon_restart_redispatches_only_recoverable_runs() {
    let temp = tempfile::tempdir().unwrap();
    let config = ServerConfig::new(temp.path());
    let store = SqliteRunStore::open(&config.database_path).unwrap();
    store
        .insert_run(&fixture_run(RUN_ID, "/repo/.git"))
        .unwrap();
    let controller = Arc::new(FakeRunController {
        store: store.clone(),
        drives: AtomicUsize::new(0),
        health_checks: AtomicUsize::new(0),
        startup_recoveries: AtomicUsize::new(0),
    });
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let mut server = tokio::spawn(run_server_with_controller(
        config.clone(),
        store,
        controller.clone(),
        shutdown_rx,
    ));
    let _client = wait_for_daemon(&config.socket_path, &mut server).await;

    for _ in 0..100 {
        if controller.drives.load(Ordering::SeqCst) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    assert_eq!(controller.drives.load(Ordering::SeqCst), 1);
    assert_eq!(controller.startup_recoveries.load(Ordering::SeqCst), 1);
    shutdown_tx.send(()).unwrap();
    server.await.unwrap().unwrap();
}

struct FakeRunController {
    store: SqliteRunStore,
    drives: AtomicUsize,
    health_checks: AtomicUsize,
    startup_recoveries: AtomicUsize,
}

#[async_trait]
impl RunController for FakeRunController {
    async fn check_app_server(&self) -> Result<(), CoordinatorError> {
        self.health_checks.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn recover_startup_runs(&self) -> Result<Vec<RunState>, CoordinatorError> {
        self.startup_recoveries.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }

    async fn start_run(
        &self,
        state: RunState,
        _request: StartRequest,
    ) -> Result<RunState, CoordinatorError> {
        self.store.insert_run(&state)?;
        Ok(state)
    }

    async fn drive_run(&self, run_id: &str) -> Result<RunState, CoordinatorError> {
        self.drives.fetch_add(1, Ordering::SeqCst);
        Ok(self.store.load_run(run_id)?.unwrap())
    }

    async fn prepare_resume_run(&self, run_id: &str) -> Result<RunState, CoordinatorError> {
        Ok(self.store.load_run(run_id)?.unwrap())
    }

    async fn apply_patch(
        &self,
        run_id: &str,
        _request_hash: &str,
        _patch: &str,
    ) -> Result<IntegrationPatchResult, CoordinatorError> {
        Ok(IntegrationPatchResult {
            run_id: run_id.to_owned(),
            integration_branch: "consensus/test".into(),
            base_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            changed_files: vec![PathBuf::from("src/lib.rs")],
        })
    }

    async fn cancel_run(&self, run_id: &str) -> Result<RunState, CoordinatorError> {
        let mut state = self.store.load_run(run_id)?.unwrap();
        state.cancel();
        self.store.save_state(&state)?;
        Ok(state)
    }
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
