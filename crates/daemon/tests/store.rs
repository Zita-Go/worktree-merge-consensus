use std::path::PathBuf;

use consensus_core::state::{NextAction, Role, RunDiagnostic, RunFacts, RunState, RunStatus};
use consensus_daemon::store::{SqliteRunStore, VerificationCommandClaim};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use uuid::Uuid;

const RUN_ID: &str = "4b230bd8-d870-4ef4-bf20-05a4c61020af";

#[test]
fn pending_send_survives_reopen_without_storing_prompt() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let run = fixture_run(RUN_ID, "/repo/.git");
    let store = SqliteRunStore::open(&path).unwrap();
    store.insert_run(&run).unwrap();
    store
        .record_pending_send(RUN_ID, "PRIMARY", "PLAN_REVIEW", 2, "hash-1")
        .unwrap();
    drop(store);

    let reopened = SqliteRunStore::open(&path).unwrap();
    let send = reopened.pending_send(RUN_ID).unwrap().unwrap();

    assert_eq!(send.message_hash, "hash-1");
    assert_eq!(send.role, "PRIMARY");
    assert!(send.full_prompt.is_none());
}

#[test]
fn started_turn_identity_survives_reopen_for_crash_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let run = fixture_run(RUN_ID, "/repo/.git");
    let store = SqliteRunStore::open(&path).unwrap();
    store.insert_run(&run).unwrap();
    store
        .record_pending_send(RUN_ID, "PRIMARY", "CONTRACT", 1, "request-hash")
        .unwrap();
    store
        .record_turn_started(RUN_ID, "request-hash", "primary-thread", "turn-7")
        .unwrap();
    drop(store);

    let pending = SqliteRunStore::open(path)
        .unwrap()
        .pending_send(RUN_ID)
        .unwrap()
        .unwrap();

    assert_eq!(pending.thread_id.as_deref(), Some("primary-thread"));
    assert_eq!(pending.turn_id.as_deref(), Some("turn-7"));
    assert!(pending.full_prompt.is_none());
}

#[test]
fn completed_app_server_item_events_survive_reopen_as_turn_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let store = SqliteRunStore::open(&path).unwrap();
    store
        .insert_run(&fixture_run(RUN_ID, "/repo/.git"))
        .unwrap();
    store
        .record_pending_send(RUN_ID, "PRIMARY", "VERIFY", 1, "request-hash")
        .unwrap();
    store
        .record_turn_started(RUN_ID, "request-hash", "primary-thread", "turn-7")
        .unwrap();
    let started = json!({
        "id": "command-1",
        "type": "commandExecution",
        "command": "cargo test --workspace",
        "cwd": "/repo/verification",
        "status": "inProgress"
    });
    let completed = json!({
        "id": "command-1",
        "type": "commandExecution",
        "command": "cargo test --workspace",
        "cwd": "/repo/verification",
        "status": "completed",
        "exitCode": 0,
        "aggregatedOutput": "ok",
        "source": "agent"
    });
    store
        .record_turn_item_event(RUN_ID, "primary-thread", "turn-7", "item/started", &started)
        .unwrap();
    store
        .record_turn_item_event(
            RUN_ID,
            "primary-thread",
            "turn-7",
            "item/completed",
            &completed,
        )
        .unwrap();
    store
        .record_turn_completed_event(
            RUN_ID,
            "primary-thread",
            "turn-7",
            &json!({
                "id": "turn-7",
                "status": "completed",
                "items": []
            }),
        )
        .unwrap();
    drop(store);

    let reopened = SqliteRunStore::open(&path).unwrap();
    let evidence = reopened
        .turn_event_evidence(RUN_ID, "primary-thread", "turn-7")
        .unwrap()
        .unwrap();

    assert_eq!(evidence.completed_turn["id"], "turn-7");
    assert_eq!(evidence.completed_items, vec![completed.clone()]);
    reopened
        .record_turn_item_event(
            RUN_ID,
            "primary-thread",
            "turn-7",
            "item/completed",
            &completed,
        )
        .unwrap();
    let mut changed = completed;
    changed["exitCode"] = json!(1);
    assert_eq!(
        reopened
            .record_turn_item_event(
                RUN_ID,
                "primary-thread",
                "turn-7",
                "item/completed",
                &changed,
            )
            .unwrap_err()
            .code(),
        "INCOMPATIBLE_STATE"
    );
}

#[test]
fn one_successful_controlled_patch_is_persisted_per_request() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let store = SqliteRunStore::open(&path).unwrap();
    store
        .insert_run(&fixture_run(RUN_ID, "/repo/.git"))
        .unwrap();

    assert!(
        !store
            .successful_patch_recorded(RUN_ID, "request-hash")
            .unwrap()
    );
    store
        .record_successful_patch(RUN_ID, "request-hash", "patch-hash")
        .unwrap();
    assert!(
        store
            .successful_patch_recorded(RUN_ID, "request-hash")
            .unwrap()
    );
    assert_eq!(
        store
            .record_successful_patch(RUN_ID, "request-hash", "second-patch")
            .unwrap_err()
            .code(),
        "INCOMPATIBLE_STATE"
    );
    drop(store);

    assert!(
        SqliteRunStore::open(path)
            .unwrap()
            .successful_patch_recorded(RUN_ID, "request-hash")
            .unwrap()
    );
}

#[test]
fn terminal_turn_retry_is_archived_and_reset_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let store = SqliteRunStore::open(&path).unwrap();
    store
        .insert_run(&fixture_run(RUN_ID, "/repo/.git"))
        .unwrap();
    store
        .record_pending_send(RUN_ID, "REVIEWER", "CONTRACT", 1, "request-hash")
        .unwrap();
    store
        .record_turn_started(RUN_ID, "request-hash", "reviewer-thread", "turn-7")
        .unwrap();

    store
        .reset_terminal_turn_for_retry(
            RUN_ID,
            "request-hash",
            "reviewer-thread",
            "turn-7",
            "interrupted",
        )
        .unwrap();
    drop(store);

    let reopened = SqliteRunStore::open(&path).unwrap();
    let pending = reopened.pending_send(RUN_ID).unwrap().unwrap();
    assert_eq!(pending.message_hash, "request-hash");
    assert!(pending.thread_id.is_none());
    assert!(pending.turn_id.is_none());
    assert_eq!(reopened.turn_attempt_count(RUN_ID).unwrap(), 1);
    assert_eq!(
        reopened.archived_turn_ids(RUN_ID, "request-hash").unwrap(),
        vec!["turn-7"]
    );
}

#[test]
fn terminal_turn_retry_rejects_wrong_identity_or_status_without_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    store
        .insert_run(&fixture_run(RUN_ID, "/repo/.git"))
        .unwrap();
    store
        .record_pending_send(RUN_ID, "REVIEWER", "CONTRACT", 1, "request-hash")
        .unwrap();
    store
        .record_turn_started(RUN_ID, "request-hash", "reviewer-thread", "turn-7")
        .unwrap();

    let wrong_status = store
        .reset_terminal_turn_for_retry(
            RUN_ID,
            "request-hash",
            "reviewer-thread",
            "turn-7",
            "completed",
        )
        .unwrap_err();
    let wrong_turn = store
        .reset_terminal_turn_for_retry(
            RUN_ID,
            "request-hash",
            "reviewer-thread",
            "turn-8",
            "interrupted",
        )
        .unwrap_err();

    assert_eq!(wrong_status.code(), "TERMINAL_TURN_NOT_RETRYABLE");
    assert_eq!(wrong_turn.code(), "TERMINAL_TURN_NOT_RETRYABLE");
    let pending = store.pending_send(RUN_ID).unwrap().unwrap();
    assert_eq!(pending.thread_id.as_deref(), Some("reviewer-thread"));
    assert_eq!(pending.turn_id.as_deref(), Some("turn-7"));
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 0);
}

#[test]
fn completed_read_only_retry_reactivates_a_legacy_blocked_run_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let run = fixture_run(RUN_ID, "/repo/.git");
    store.insert_run(&run).unwrap();
    store
        .record_pending_send(RUN_ID, "PRIMARY", "CONTRACT", 1, "request-hash")
        .unwrap();
    store
        .record_turn_started(RUN_ID, "request-hash", "primary-thread", "turn-7")
        .unwrap();
    let mut blocked = run;
    blocked.record_error(RunDiagnostic {
        code: "INVALID_TEST_COMMAND".into(),
        detail: "git command is not an allowed test".into(),
        operation: None,
        action: NextAction::RequestPrimaryContract,
        role: Some(Role::Primary),
        thread_id: Some("primary-thread".into()),
    });
    blocked.block("INVALID_TEST_COMMAND");
    store.save_state(&blocked).unwrap();
    let mut resumed = blocked.clone();
    resumed.retry_blocked_invalid_test_command().unwrap();

    store
        .reactivate_blocked_run_with_completed_turn_retry(
            &blocked,
            &resumed,
            "request-hash",
            "primary-thread",
            "turn-7",
            "completed",
        )
        .unwrap();

    assert_eq!(store.load_run(RUN_ID).unwrap().unwrap(), resumed);
    let pending = store.pending_send(RUN_ID).unwrap().unwrap();
    assert!(pending.thread_id.is_none());
    assert!(pending.turn_id.is_none());
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 1);
    let competing = fixture_run("9f8a5c17-0f06-4df9-873f-589f3b54dbcc", "/repo/.git");
    assert_eq!(
        store.insert_run(&competing).unwrap_err().code(),
        "ACTIVE_RUN_EXISTS"
    );
}

#[test]
fn run_state_round_trips_as_structured_state() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let run = fixture_run(RUN_ID, "/repo/.git");
    store.insert_run(&run).unwrap();

    let loaded = store.load_run(RUN_ID).unwrap().unwrap();

    assert_eq!(loaded, run);
}

#[test]
fn legacy_state_without_schema_version_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let store = SqliteRunStore::open(&path).unwrap();
    store
        .insert_run(&fixture_run(RUN_ID, "/repo/.git"))
        .unwrap();
    drop(store);

    let connection = Connection::open(&path).unwrap();
    let encoded: String = connection
        .query_row(
            "SELECT state_json FROM runs WHERE run_id = ?1",
            [RUN_ID],
            |row| row.get(0),
        )
        .unwrap();
    let mut value = serde_json::from_str::<Value>(&encoded).unwrap();
    value.as_object_mut().unwrap().remove("schema_version");
    connection
        .execute(
            "UPDATE runs SET state_json = ?1 WHERE run_id = ?2",
            params![serde_json::to_string(&value).unwrap(), RUN_ID],
        )
        .unwrap();
    drop(connection);

    let error = SqliteRunStore::open(path)
        .unwrap()
        .load_run(RUN_ID)
        .unwrap_err();

    assert_eq!(error.code(), "INCOMPATIBLE_STATE");
}

#[test]
fn verification_command_completed_record_is_reused_after_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let store = SqliteRunStore::open(&path).unwrap();
    store
        .insert_run(&fixture_run(RUN_ID, "/repo/.git"))
        .unwrap();

    let claim = store
        .begin_verification_command(
            RUN_ID,
            "request-hash",
            "turn-7",
            0,
            "cargo test --locked",
            std::path::Path::new("/verify/run"),
        )
        .unwrap();
    let started = match claim {
        VerificationCommandClaim::Execute(record) => record,
        VerificationCommandClaim::Reuse(_) => panic!("new verification command should execute"),
    };
    assert_eq!(started.item_id, "coordinator-command/request-hash/0");
    assert_eq!(started.exit_code, None);

    let completed = store
        .complete_verification_command(RUN_ID, "request-hash", 0, 0, "ok", "")
        .unwrap();
    assert_eq!(completed.exit_code, Some(0));
    assert_eq!(completed.stdout.as_deref(), Some("ok"));
    assert_eq!(completed.stderr.as_deref(), Some(""));
    drop(store);

    let reopened = SqliteRunStore::open(path).unwrap();
    let claim = reopened
        .begin_verification_command(
            RUN_ID,
            "request-hash",
            "turn-7",
            0,
            "cargo test --locked",
            std::path::Path::new("/verify/run"),
        )
        .unwrap();

    assert!(matches!(
        claim,
        VerificationCommandClaim::Reuse(record)
            if record.exit_code == Some(0)
                && record.stdout.as_deref() == Some("ok")
                && record.stderr.as_deref() == Some("")
    ));
    assert_eq!(verification_command_row_count(&path), 1);
}

#[test]
fn verification_command_started_row_fails_closed_as_uncertain() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let store = SqliteRunStore::open(&path).unwrap();
    store
        .insert_run(&fixture_run(RUN_ID, "/repo/.git"))
        .unwrap();

    let first = store
        .begin_verification_command(
            RUN_ID,
            "request-hash",
            "turn-7",
            0,
            "cargo test --locked",
            std::path::Path::new("/verify/run"),
        )
        .unwrap();
    assert!(matches!(first, VerificationCommandClaim::Execute(_)));

    let error = store
        .begin_verification_command(
            RUN_ID,
            "request-hash",
            "turn-7",
            0,
            "cargo test --locked",
            std::path::Path::new("/verify/run"),
        )
        .unwrap_err();

    assert_eq!(error.code(), "VERIFICATION_EXECUTION_UNCERTAIN");
    assert_eq!(
        verification_command_status(&path, RUN_ID, "request-hash", 0),
        "STARTED"
    );
    assert_eq!(verification_command_row_count(&path), 1);
}

#[test]
fn verification_command_identity_mismatch_fails_without_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let store = SqliteRunStore::open(&path).unwrap();
    store
        .insert_run(&fixture_run(RUN_ID, "/repo/.git"))
        .unwrap();
    store
        .begin_verification_command(
            RUN_ID,
            "request-hash",
            "turn-7",
            0,
            "cargo test --locked",
            std::path::Path::new("/verify/run"),
        )
        .unwrap();
    store
        .complete_verification_command(RUN_ID, "request-hash", 0, 0, "ok", "")
        .unwrap();

    let changed_command = store
        .begin_verification_command(
            RUN_ID,
            "request-hash",
            "turn-7",
            0,
            "cargo test --workspace --locked",
            std::path::Path::new("/verify/run"),
        )
        .unwrap_err();
    let changed_cwd = store
        .begin_verification_command(
            RUN_ID,
            "request-hash",
            "turn-7",
            0,
            "cargo test --locked",
            std::path::Path::new("/verify/other"),
        )
        .unwrap_err();
    let changed_turn = store
        .begin_verification_command(
            RUN_ID,
            "request-hash",
            "turn-8",
            0,
            "cargo test --locked",
            std::path::Path::new("/verify/run"),
        )
        .unwrap_err();
    let changed_request = store
        .begin_verification_command(
            RUN_ID,
            "request-hash-2",
            "turn-7",
            0,
            "cargo test --locked",
            std::path::Path::new("/verify/run"),
        )
        .unwrap_err();

    assert_eq!(changed_command.code(), "INCOMPATIBLE_STATE");
    assert_eq!(changed_cwd.code(), "INCOMPATIBLE_STATE");
    assert_eq!(changed_turn.code(), "INCOMPATIBLE_STATE");
    assert_eq!(changed_request.code(), "INCOMPATIBLE_STATE");
    assert_eq!(verification_command_row_count(&path), 1);
    assert_eq!(
        verification_command_identity(&path, RUN_ID, "request-hash", 0),
        (
            "turn-7".into(),
            "cargo test --locked".into(),
            "/verify/run".into(),
            "COMPLETED".into(),
        )
    );
}

#[test]
fn verification_command_completion_updates_started_row_and_rejects_changed_second_completion() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let store = SqliteRunStore::open(&path).unwrap();
    store
        .insert_run(&fixture_run(RUN_ID, "/repo/.git"))
        .unwrap();

    let missing = store
        .complete_verification_command(RUN_ID, "request-hash", 0, 0, "ok", "")
        .unwrap_err();
    assert_eq!(missing.code(), "INCOMPATIBLE_STATE");

    store
        .begin_verification_command(
            RUN_ID,
            "request-hash",
            "turn-7",
            0,
            "cargo test --locked",
            std::path::Path::new("/verify/run"),
        )
        .unwrap();
    let first = store
        .complete_verification_command(RUN_ID, "request-hash", 0, 0, "ok", "")
        .unwrap();
    assert_eq!(first.exit_code, Some(0));

    let repeated = store
        .complete_verification_command(RUN_ID, "request-hash", 0, 0, "ok", "")
        .unwrap();
    assert_eq!(repeated, first);

    let changed = store
        .complete_verification_command(RUN_ID, "request-hash", 0, 1, "nope", "")
        .unwrap_err();
    assert_eq!(changed.code(), "INCOMPATIBLE_STATE");
    assert_eq!(
        verification_command_completion(&path, RUN_ID, "request-hash", 0),
        (Some(0), Some("ok".into()), Some("".into()))
    );
}

#[test]
fn second_active_run_for_same_repository_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    store
        .insert_run(&fixture_run(RUN_ID, "/repo/.git"))
        .unwrap();
    let second = fixture_run("9a0ca0d8-8dd4-4c96-aae8-0a8896464c45", "/repo/.git");

    let error = store.insert_run(&second).unwrap_err();

    assert_eq!(error.code(), "ACTIVE_RUN_EXISTS");
}

#[test]
fn accepting_response_and_advancing_state_is_atomic() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let mut run = fixture_run(RUN_ID, "/repo/.git");
    let store = SqliteRunStore::open(&path).unwrap();
    store.insert_run(&run).unwrap();
    store
        .record_pending_send(RUN_ID, "PRIMARY", "CONTRACT", 1, "request-hash")
        .unwrap();
    run.pause("PERMISSION_REQUIRED").unwrap();

    store
        .accept_response_and_advance(RUN_ID, "response-hash", &run)
        .unwrap();
    drop(store);

    let reopened = SqliteRunStore::open(path).unwrap();
    assert!(reopened.pending_send(RUN_ID).unwrap().is_none());
    assert_eq!(
        reopened.load_run(RUN_ID).unwrap().unwrap().status,
        RunStatus::PausedUserAction
    );
    assert_eq!(reopened.transition_count(RUN_ID).unwrap(), 1);
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

fn verification_command_row_count(path: &std::path::Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM verification_command_executions", [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn verification_command_status(
    path: &std::path::Path,
    run_id: &str,
    message_hash: &str,
    command_index: u32,
) -> String {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT status
             FROM verification_command_executions
             WHERE run_id = ?1 AND message_hash = ?2 AND command_index = ?3",
            params![run_id, message_hash, command_index],
            |row| row.get(0),
        )
        .unwrap()
}

fn verification_command_identity(
    path: &std::path::Path,
    run_id: &str,
    message_hash: &str,
    command_index: u32,
) -> (String, String, String, String) {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT turn_id, command, cwd, status
             FROM verification_command_executions
             WHERE run_id = ?1 AND message_hash = ?2 AND command_index = ?3",
            params![run_id, message_hash, command_index],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
}

fn verification_command_completion(
    path: &std::path::Path,
    run_id: &str,
    message_hash: &str,
    command_index: u32,
) -> (Option<i32>, Option<String>, Option<String>) {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT exit_code, stdout, stderr
             FROM verification_command_executions
             WHERE run_id = ?1 AND message_hash = ?2 AND command_index = ?3",
            params![run_id, message_hash, command_index],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}
