use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use app_server_client::{
    AppEvent, AppServer, AppServerError, InitializeInfo, ThreadDetail, ThreadPage, ThreadSummary,
    TurnHandle,
};
use async_trait::async_trait;
use consensus_core::{
    canonical_json_hash,
    state::{NextAction, RunFacts, RunState, RunStatus},
};
use consensus_daemon::{
    coordinator::{Coordinator, CoordinatorOptions, RepositorySafety, SafetyError, StartRequest},
    store::SqliteRunStore,
};
use serde_json::{Value, json};
use uuid::Uuid;

const RUN_ID: &str = "4b230bd8-d870-4ef4-bf20-05a4c61020af";
const PRIMARY_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const REVIEWER_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const INTEGRATION_SHA: &str = "cccccccccccccccccccccccccccccccccccccccc";

#[test]
fn checked_in_transcript_fixtures_are_valid_json() {
    for fixture in [
        include_str!("../../../tests/fixtures/transcripts/conflict-free.json"),
        include_str!("../../../tests/fixtures/transcripts/plan-revision.json"),
        include_str!("../../../tests/fixtures/transcripts/result-revision.json"),
    ] {
        let value: Value = serde_json::from_str(fixture).unwrap();
        assert!(value["scenario"].is_string());
        assert!(value["request_order"].is_array());
    }
}

#[tokio::test]
async fn conflict_free_run_waits_for_plan_approval_and_accepts_exact_result() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::new(conflict_free_replies()));
    let safety = Arc::new(RecordingSafety::default());
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store,
        Arc::clone(&safety),
        CoordinatorOptions {
            wait_timeout: Duration::from_secs(1),
            poll_interval: Duration::from_millis(1),
            communication_attempts: 2,
        },
    );
    let state = fixture_run();

    coordinator
        .start(
            state,
            StartRequest {
                integration_branch: Some("consensus/test-run".into()),
                test_commands: vec!["cargo test --workspace".into()],
            },
        )
        .await
        .unwrap();
    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(result.integration_sha.as_deref(), Some(INTEGRATION_SHA));
    assert_eq!(
        app.request_order(),
        vec![
            "primary:REQUEST_PRIMARY_CONTRACT",
            "reviewer:REQUEST_REVIEWER_CONTRACT",
            "primary:REQUEST_PRIMARY_PLAN",
            "reviewer:REQUEST_REVIEWER_PLAN_VERDICT",
            "primary:REQUEST_PRIMARY_INTEGRATION",
            "reviewer:REQUEST_REVIEWER_RESULT_VERDICT",
        ]
    );
    let safety_events = safety.events();
    let integration_request = app
        .request_order()
        .iter()
        .position(|entry| entry.ends_with("REQUEST_PRIMARY_INTEGRATION"))
        .unwrap();
    let approval = app
        .reply_types()
        .iter()
        .position(|entry| entry == "APPROVED_PLAN")
        .unwrap();
    assert!(approval < integration_request);
    assert!(safety_events.contains(&format!("result:consensus/test-run:{INTEGRATION_SHA}")));
}

#[tokio::test]
async fn target_branch_is_validated_and_normalized_before_the_run_is_stored() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let coordinator = Coordinator::new(
        Arc::new(FakeAppServer::new(conflict_free_replies())),
        store.clone(),
        Arc::new(RecordingSafety::default()),
        fast_options(),
    );

    let state = coordinator
        .start(
            fixture_run(),
            StartRequest {
                integration_branch: Some("refs/heads/consensus/test-run".into()),
                test_commands: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        state.target_integration_branch.as_deref(),
        Some("consensus/test-run")
    );

    coordinator.cancel(RUN_ID).await.unwrap();
    let mut second = fixture_run();
    second.facts.run_id = Uuid::new_v4();
    let error = coordinator
        .start(
            second.clone(),
            StartRequest {
                integration_branch: Some("invalid branch".into()),
                test_commands: Vec::new(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), "INVALID_BRANCH_NAME");
    assert!(
        store
            .load_run(&second.facts.run_id.to_string())
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn plan_rejection_resends_complete_feedback_before_second_approval() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::new(plan_revision_replies()));
    let safety = Arc::new(RecordingSafety::default());
    let coordinator = Coordinator::new(Arc::clone(&app), store, safety, fast_options());

    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();
    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(result.plan_revision, Some(2));
    let actions = app.request_order();
    assert_eq!(
        actions
            .iter()
            .filter(|action| action.ends_with("REQUEST_PRIMARY_PLAN"))
            .count(),
        2
    );
    assert_eq!(
        actions
            .iter()
            .filter(|action| action.ends_with("REQUEST_PRIMARY_INTEGRATION"))
            .count(),
        1
    );
    let second_plan_prompt = app
        .prompts()
        .into_iter()
        .filter(|prompt| prompt.contains("REQUEST_PRIMARY_PLAN"))
        .nth(1)
        .unwrap();
    assert!(second_plan_prompt.contains("missing-reviewer-edge"));
    assert!(second_plan_prompt.contains("merge both"));
}

#[tokio::test]
async fn result_rejection_requires_a_new_sha_and_resends_full_feedback() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::new(result_revision_replies()));
    let safety = Arc::new(RecordingSafety::default());
    let coordinator =
        Coordinator::new(Arc::clone(&app), store, Arc::clone(&safety), fast_options());

    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();
    let result = coordinator.drive(RUN_ID).await.unwrap();

    let revised_sha = "dddddddddddddddddddddddddddddddddddddddd";
    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(result.integration_sha.as_deref(), Some(revised_sha));
    assert_eq!(
        app.request_order()
            .iter()
            .filter(|action| action.ends_with("REQUEST_PRIMARY_INTEGRATION"))
            .count(),
        2
    );
    let second_integration_prompt = app
        .prompts()
        .into_iter()
        .filter(|prompt| prompt.contains("REQUEST_PRIMARY_INTEGRATION"))
        .nth(1)
        .unwrap();
    assert!(second_integration_prompt.contains("missing-result-edge"));
    assert!(second_integration_prompt.contains(INTEGRATION_SHA));
    assert!(
        safety
            .events()
            .contains(&format!("result:consensus/test-run:{revised_sha}"))
    );
}

#[tokio::test]
async fn unchanged_plan_and_issue_set_blocks_without_creating_an_integration_turn() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::new(no_progress_replies()));
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store,
        Arc::new(RecordingSafety::default()),
        fast_options(),
    );

    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();
    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Blocked);
    assert_eq!(result.reason_code.as_deref(), Some("NO_PROGRESS"));
    assert!(
        app.request_order()
            .iter()
            .all(|action| !action.ends_with("REQUEST_PRIMARY_INTEGRATION"))
    );
}

#[tokio::test]
async fn non_object_model_reply_blocks_as_invalid_response() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::new(vec![json!("looks good")]));
    let coordinator = Coordinator::new(
        app,
        store,
        Arc::new(RecordingSafety::default()),
        fast_options(),
    );

    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();
    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Blocked);
    assert_eq!(result.reason_code.as_deref(), Some("INVALID_RESPONSE"));
}

#[tokio::test]
async fn failed_required_test_blocks_before_result_review() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let mut replies = conflict_free_replies();
    replies[4]["payload"]["test_evidence"][0]["exit_code"] = json!(1);
    let app = Arc::new(FakeAppServer::new(replies));
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store,
        Arc::new(RecordingSafety::default()),
        fast_options(),
    );

    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();
    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Blocked);
    assert_eq!(result.reason_code.as_deref(), Some("TEST_FAILURE"));
    assert!(
        app.request_order()
            .iter()
            .all(|action| !action.ends_with("REQUEST_REVIEWER_RESULT_VERDICT"))
    );
}

#[tokio::test]
async fn completed_turn_is_recovered_without_duplicate_send_when_turn_id_was_not_saved() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let mut replies = conflict_free_replies();
    let recovered_reply = replies.remove(0);
    let app = Arc::new(FakeAppServer::new(replies));
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::new(RecordingSafety::default()),
        fast_options(),
    );
    let state = coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();
    let request_hash = first_request_hash(&state);
    store
        .record_pending_send(RUN_ID, "PRIMARY", "CONTRACT", 1, &request_hash)
        .unwrap();
    app.inject_completed_turn(
        "primary",
        "recovered-turn",
        &format!("recovered marker {{\"request_hash\":\"{request_hash}\"}}"),
        recovered_reply,
    );

    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(app.request_count(), 5);
    assert!(
        app.request_order()
            .iter()
            .all(|request| request != "primary:REQUEST_PRIMARY_CONTRACT")
    );
    assert!(store.pending_send(RUN_ID).unwrap().is_none());
}

#[tokio::test]
async fn pending_record_created_before_send_results_in_exactly_one_task_turn() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::new(conflict_free_replies()));
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::new(RecordingSafety::default()),
        fast_options(),
    );
    let state = coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();
    store
        .record_pending_send(
            RUN_ID,
            "PRIMARY",
            "CONTRACT",
            1,
            &first_request_hash(&state),
        )
        .unwrap();

    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(app.request_count(), 6);
}

#[tokio::test]
async fn permission_request_pauses_and_resume_reuses_the_same_turn() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::deferred(
        conflict_free_replies(),
        1,
        DeferMode::Permission,
    ));
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store,
        Arc::new(RecordingSafety::default()),
        fast_options(),
    );
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();

    let paused = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(paused.status, RunStatus::PausedUserAction);
    assert_eq!(paused.reason_code.as_deref(), Some("PERMISSION_REQUIRED"));
    assert_eq!(app.request_count(), 1);

    app.complete_deferred_turns();
    let result = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(app.request_count(), 6);
}

#[tokio::test]
async fn permission_during_integration_resumes_the_authorized_in_progress_turn() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::deferred(
        conflict_free_replies(),
        5,
        DeferMode::Permission,
    ));
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store,
        Arc::new(RecordingSafety::default()),
        fast_options(),
    );
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();

    let paused = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(paused.status, RunStatus::PausedUserAction);
    assert_eq!(paused.next_action, NextAction::RequestPrimaryIntegration);
    assert_eq!(app.request_count(), 5);

    app.complete_deferred_turns();
    let result = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(app.request_count(), 6);
}

#[tokio::test]
async fn recovered_integration_turn_skips_the_first_action_frozen_head_check() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::deferred(
        conflict_free_replies(),
        5,
        DeferMode::Permission,
    ));
    let safety = Arc::new(InProgressRecoverySafety::default());
    let coordinator =
        Coordinator::new(Arc::clone(&app), store, Arc::clone(&safety), fast_options());
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();
    let paused = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(paused.status, RunStatus::PausedUserAction);

    safety
        .integration_branch_active
        .store(true, Ordering::SeqCst);
    app.complete_deferred_turns();
    let result = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert!(safety.in_progress_calls.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn cancellation_stops_new_turns_without_interrupting_the_active_turn() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::deferred(
        conflict_free_replies(),
        1,
        DeferMode::Hold,
    ));
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store,
        Arc::new(RecordingSafety::default()),
        fast_options(),
    );
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();
    let driver = {
        let coordinator = coordinator.clone();
        tokio::spawn(async move { coordinator.drive(RUN_ID).await })
    };
    wait_for_request(&app).await;

    let cancelled = coordinator.cancel(RUN_ID).await.unwrap();
    let driver_result = driver.await.unwrap().unwrap();

    assert_eq!(cancelled.status, RunStatus::Cancelled);
    assert_eq!(driver_result.status, RunStatus::Cancelled);
    assert_eq!(app.request_count(), 1);
    assert!(app.detail("primary").summary.is_active());
}

#[tokio::test]
async fn active_turn_timeout_pauses_as_communication_failure() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::deferred(
        conflict_free_replies(),
        1,
        DeferMode::Hold,
    ));
    let coordinator = Coordinator::new(
        app,
        store,
        Arc::new(RecordingSafety::default()),
        CoordinatorOptions {
            wait_timeout: Duration::from_millis(15),
            poll_interval: Duration::from_millis(1),
            communication_attempts: 1,
        },
    );
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();

    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::PausedUserAction);
    assert_eq!(result.reason_code.as_deref(), Some("COMMUNICATION_FAILURE"));
}

#[tokio::test]
async fn mismatched_pending_send_blocks_when_canonical_history_cannot_recover_it() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::new(conflict_free_replies()));
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::new(RecordingSafety::default()),
        fast_options(),
    );
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();
    store
        .record_pending_send(RUN_ID, "PRIMARY", "CONTRACT", 1, "wrong-request-hash")
        .unwrap();

    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Blocked);
    assert_eq!(result.reason_code.as_deref(), Some("HISTORY_UNAVAILABLE"));
    assert_eq!(app.request_count(), 0);
}

#[tokio::test]
async fn repository_drift_and_dirty_sources_fail_closed_before_a_task_turn() {
    for reason in ["SOURCE_DRIFT", "DIRTY_WORKTREE"] {
        let temp = tempfile::tempdir().unwrap();
        let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
        let app = Arc::new(FakeAppServer::new(conflict_free_replies()));
        let coordinator = Coordinator::new(
            Arc::clone(&app),
            store,
            Arc::new(FailAfterStartSafety::new(reason)),
            fast_options(),
        );
        coordinator
            .start(fixture_run(), start_request())
            .await
            .unwrap();

        let result = coordinator.drive(RUN_ID).await.unwrap();

        assert_eq!(result.status, RunStatus::Blocked);
        assert_eq!(result.reason_code.as_deref(), Some(reason));
        assert_eq!(app.request_count(), 0);
    }
}

#[tokio::test]
async fn configured_round_limit_blocks_after_the_last_rejection() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let replies = no_progress_replies().into_iter().take(4).collect();
    let app = Arc::new(FakeAppServer::new(replies));
    let coordinator = Coordinator::new(
        app,
        store,
        Arc::new(RecordingSafety::default()),
        fast_options(),
    );
    let mut state = fixture_run();
    state.max_review_rounds = 1;
    coordinator.start(state, start_request()).await.unwrap();

    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Blocked);
    assert_eq!(result.reason_code.as_deref(), Some("ROUND_LIMIT"));
}

#[derive(Default)]
struct RecordingSafety {
    events: Mutex<Vec<String>>,
}

impl RecordingSafety {
    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
}

impl RepositorySafety for RecordingSafety {
    fn verify_frozen(&self, _facts: &RunFacts) -> Result<(), SafetyError> {
        self.events.lock().unwrap().push("frozen".into());
        Ok(())
    }

    fn verify_branch_absent(&self, _facts: &RunFacts, branch: &str) -> Result<(), SafetyError> {
        self.events.lock().unwrap().push(format!("absent:{branch}"));
        Ok(())
    }

    fn verify_integration(
        &self,
        _facts: &RunFacts,
        branch: &str,
        sha: &str,
        _changed_files: &[PathBuf],
    ) -> Result<(), SafetyError> {
        self.events
            .lock()
            .unwrap()
            .push(format!("result:{branch}:{sha}"));
        Ok(())
    }
}

#[derive(Default)]
struct InProgressRecoverySafety {
    integration_branch_active: AtomicBool,
    in_progress_calls: AtomicUsize,
}

impl RepositorySafety for InProgressRecoverySafety {
    fn verify_frozen(&self, _facts: &RunFacts) -> Result<(), SafetyError> {
        if self.integration_branch_active.load(Ordering::SeqCst) {
            Err(SafetyError::new(
                "SOURCE_DRIFT",
                "primary HEAD has moved to the authorized integration branch",
            ))
        } else {
            Ok(())
        }
    }

    fn verify_branch_absent(&self, _facts: &RunFacts, _branch: &str) -> Result<(), SafetyError> {
        Ok(())
    }

    fn verify_integration_in_progress(
        &self,
        _facts: &RunFacts,
        _target_branch: &str,
    ) -> Result<(), SafetyError> {
        self.in_progress_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn verify_integration(
        &self,
        _facts: &RunFacts,
        _branch: &str,
        _sha: &str,
        _changed_files: &[PathBuf],
    ) -> Result<(), SafetyError> {
        Ok(())
    }
}

struct FailAfterStartSafety {
    reason: &'static str,
    frozen_calls: AtomicUsize,
}

impl FailAfterStartSafety {
    fn new(reason: &'static str) -> Self {
        Self {
            reason,
            frozen_calls: AtomicUsize::new(0),
        }
    }
}

impl RepositorySafety for FailAfterStartSafety {
    fn verify_frozen(&self, _facts: &RunFacts) -> Result<(), SafetyError> {
        if self.frozen_calls.fetch_add(1, Ordering::SeqCst) > 0 {
            Err(SafetyError::new(self.reason, "scripted repository drift"))
        } else {
            Ok(())
        }
    }

    fn verify_branch_absent(&self, _facts: &RunFacts, _branch: &str) -> Result<(), SafetyError> {
        Ok(())
    }

    fn verify_integration(
        &self,
        _facts: &RunFacts,
        _branch: &str,
        _sha: &str,
        _changed_files: &[PathBuf],
    ) -> Result<(), SafetyError> {
        Ok(())
    }
}

struct FakeAppServer {
    replies: Mutex<VecDeque<Value>>,
    threads: Mutex<HashMap<String, Vec<Value>>>,
    requests: Mutex<Vec<String>>,
    reply_types: Mutex<Vec<String>>,
    prompts: Mutex<Vec<String>>,
    deferred: Option<(usize, DeferMode)>,
    deferred_replies: Mutex<HashMap<String, Value>>,
    events: Mutex<VecDeque<AppEvent>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeferMode {
    Permission,
    Hold,
}

impl FakeAppServer {
    fn new(replies: Vec<Value>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            threads: Mutex::new(HashMap::from([
                ("primary".into(), Vec::new()),
                ("reviewer".into(), Vec::new()),
            ])),
            requests: Mutex::new(Vec::new()),
            reply_types: Mutex::new(Vec::new()),
            prompts: Mutex::new(Vec::new()),
            deferred: None,
            deferred_replies: Mutex::new(HashMap::new()),
            events: Mutex::new(VecDeque::new()),
        }
    }

    fn deferred(replies: Vec<Value>, request_number: usize, mode: DeferMode) -> Self {
        let mut server = Self::new(replies);
        server.deferred = Some((request_number, mode));
        server
    }

    fn request_order(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn reply_types(&self) -> Vec<String> {
        self.reply_types.lock().unwrap().clone()
    }

    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }

    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn inject_completed_turn(&self, thread_id: &str, turn_id: &str, prompt: &str, reply: Value) {
        self.threads
            .lock()
            .unwrap()
            .get_mut(thread_id)
            .unwrap()
            .push(completed_turn(turn_id, prompt, &reply));
    }

    fn complete_deferred_turns(&self) {
        let replies = std::mem::take(&mut *self.deferred_replies.lock().unwrap());
        let mut threads = self.threads.lock().unwrap();
        for turns in threads.values_mut() {
            for turn in turns {
                let Some(turn_id) = turn.get("id").and_then(Value::as_str).map(str::to_owned)
                else {
                    continue;
                };
                let Some(reply) = replies.get(&turn_id) else {
                    continue;
                };
                turn["status"] = json!("completed");
                turn["items"].as_array_mut().unwrap().push(json!({
                    "id": format!("assistant-{turn_id}"),
                    "type": "agentMessage",
                    "text": serde_json::to_string(reply).unwrap(),
                    "phase": "final_answer"
                }));
            }
        }
    }

    fn detail(&self, thread_id: &str) -> ThreadDetail {
        let turns = self
            .threads
            .lock()
            .unwrap()
            .get(thread_id)
            .cloned()
            .unwrap();
        let mut summary = summary(thread_id);
        if turns
            .iter()
            .any(|turn| turn.get("status").and_then(Value::as_str) == Some("inProgress"))
        {
            summary.status = json!({"type": "active", "activeFlags": []});
        }
        ThreadDetail {
            summary,
            raw: json!({"id": thread_id, "turns": turns}),
            turns,
        }
    }
}

#[async_trait]
impl AppServer for FakeAppServer {
    async fn initialize(&self) -> Result<InitializeInfo, AppServerError> {
        Ok(InitializeInfo { raw: json!({}) })
    }

    async fn list_threads(
        &self,
        _cursor: Option<String>,
        _limit: u32,
    ) -> Result<ThreadPage, AppServerError> {
        Ok(ThreadPage {
            data: vec![summary("primary"), summary("reviewer")],
            next_cursor: None,
            backwards_cursor: None,
        })
    }

    async fn read_thread(&self, thread_id: &str) -> Result<ThreadDetail, AppServerError> {
        Ok(self.detail(thread_id))
    }

    async fn resume_thread(&self, thread_id: &str) -> Result<ThreadDetail, AppServerError> {
        Ok(self.detail(thread_id))
    }

    async fn start_turn(
        &self,
        thread_id: &str,
        prompt: &str,
        output_schema: Value,
    ) -> Result<TurnHandle, AppServerError> {
        assert_eq!(
            output_schema["title"],
            "Worktree Merge Consensus Protocol v1"
        );
        let action = prompt_action(prompt);
        self.requests
            .lock()
            .unwrap()
            .push(format!("{thread_id}:{action}"));
        self.prompts.lock().unwrap().push(prompt.to_owned());
        let reply = self.replies.lock().unwrap().pop_front().unwrap();
        self.reply_types.lock().unwrap().push(
            reply["message_type"]
                .as_str()
                .unwrap_or("<invalid>")
                .to_owned(),
        );
        let request_number = self.requests.lock().unwrap().len();
        let turn_id = format!("turn-{request_number}");
        let deferred_mode = self
            .deferred
            .filter(|(number, _)| *number == request_number)
            .map(|(_, mode)| mode);
        let turn = if deferred_mode.is_some() {
            self.deferred_replies
                .lock()
                .unwrap()
                .insert(turn_id.clone(), reply);
            json!({
                "id": turn_id,
                "status": "inProgress",
                "items": [{
                    "id": format!("user-{turn_id}"),
                    "type": "userMessage",
                    "content": [{"type": "text", "text": prompt, "text_elements": []}]
                }]
            })
        } else {
            completed_turn(&turn_id, prompt, &reply)
        };
        self.threads
            .lock()
            .unwrap()
            .get_mut(thread_id)
            .unwrap()
            .push(turn);
        if deferred_mode == Some(DeferMode::Permission) {
            self.events.lock().unwrap().push_back(AppEvent {
                id: Some(json!(1)),
                method: "item/commandExecution/requestApproval".into(),
                params: json!({"threadId": thread_id, "turnId": turn_id}),
            });
        }
        Ok(TurnHandle {
            id: turn_id,
            status: "completed".into(),
            items: Vec::new(),
        })
    }

    async fn next_event(&self) -> Option<AppEvent> {
        self.events.lock().unwrap().pop_front()
    }
}

fn completed_turn(turn_id: &str, prompt: &str, reply: &Value) -> Value {
    json!({
        "id": turn_id,
        "status": "completed",
        "items": [
            {
                "id": format!("user-{turn_id}"),
                "type": "userMessage",
                "content": [{"type": "text", "text": prompt, "text_elements": []}]
            },
            {
                "id": format!("assistant-{turn_id}"),
                "type": "agentMessage",
                "text": serde_json::to_string(reply).unwrap(),
                "phase": "final_answer"
            }
        ]
    })
}

async fn wait_for_request(app: &FakeAppServer) {
    for _ in 0..500 {
        if app.request_count() > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("fake App Server never received a turn");
}

fn summary(thread_id: &str) -> ThreadSummary {
    ThreadSummary {
        id: thread_id.into(),
        cwd: PathBuf::from(format!("/repo/{thread_id}")),
        name: Some(thread_id.into()),
        preview: String::new(),
        cli_version: "0.144.5".into(),
        created_at: 0,
        updated_at: 0,
        status: json!({"type": "idle"}),
        source: json!({}),
    }
}

fn prompt_action(prompt: &str) -> &'static str {
    for action in [
        "REQUEST_PRIMARY_CONTRACT",
        "REQUEST_REVIEWER_CONTRACT",
        "REQUEST_PRIMARY_PLAN",
        "REQUEST_REVIEWER_PLAN_VERDICT",
        "REQUEST_PRIMARY_INTEGRATION",
        "REQUEST_REVIEWER_RESULT_VERDICT",
    ] {
        if prompt.contains(action) {
            return action;
        }
    }
    panic!("prompt did not contain a known action")
}

fn fixture_run() -> RunState {
    RunState::new(RunFacts {
        run_id: Uuid::parse_str(RUN_ID).unwrap(),
        primary_thread_id: "primary".into(),
        reviewer_thread_id: "reviewer".into(),
        primary_worktree: PathBuf::from("/repo/primary"),
        reviewer_worktree: PathBuf::from("/repo/reviewer"),
        git_common_dir: PathBuf::from("/repo/.git"),
        primary_sha: PRIMARY_SHA.into(),
        reviewer_sha: REVIEWER_SHA.into(),
        primary_ref: Some("refs/heads/primary".into()),
        reviewer_ref: Some("refs/heads/reviewer".into()),
    })
}

fn fast_options() -> CoordinatorOptions {
    CoordinatorOptions {
        wait_timeout: Duration::from_secs(1),
        poll_interval: Duration::from_millis(1),
        communication_attempts: 2,
    }
}

fn start_request() -> StartRequest {
    StartRequest {
        integration_branch: Some("consensus/test-run".into()),
        test_commands: vec!["cargo test --workspace".into()],
    }
}

fn first_request_hash(state: &RunState) -> String {
    canonical_json_hash(&json!({
        "run_id": state.facts.run_id,
        "action": NextAction::RequestPrimaryContract,
        "phase": state.phase,
        "round": state.round,
        "plan_revision": state.plan_revision,
        "integration_sha": state.integration_sha,
        "payload": {
            "task_context": "derive the complete primary contract from this task and frozen SHA"
        },
    }))
}

fn conflict_free_replies() -> Vec<Value> {
    vec![
        message(
            "CONTRACT_READY",
            "CONTRACT",
            1,
            None,
            None,
            None,
            json!({"role": "PRIMARY", "contract": {"items": ["primary-feature"]}}),
        ),
        message(
            "CONTRACT_READY",
            "CONTRACT",
            1,
            None,
            None,
            None,
            json!({"role": "REVIEWER", "contract": {"items": ["reviewer-feature"]}}),
        ),
        message(
            "PLAN_READY",
            "PLAN_REVIEW",
            1,
            Some(1),
            None,
            None,
            json!({
                "primary_contract": {"items": ["primary-feature"]},
                "reviewer_contract": {"items": ["reviewer-feature"]},
                "plan": {"revision": 1, "steps": ["merge both"]},
                "coverage_matrix": [
                    {"item": "primary-feature", "covered_by": "merge both"},
                    {"item": "reviewer-feature", "covered_by": "merge both"}
                ]
            }),
        ),
        message(
            "APPROVED_PLAN",
            "PLAN_REVIEW",
            1,
            Some(1),
            None,
            None,
            json!({
                "approved_plan_revision": 1,
                "approved_primary_sha": PRIMARY_SHA,
                "approved_reviewer_sha": REVIEWER_SHA,
                "uncovered_items": []
            }),
        ),
        message(
            "INTEGRATION_READY",
            "INTEGRATE",
            1,
            Some(1),
            Some("consensus/test-run"),
            Some(INTEGRATION_SHA),
            json!({
                "changed_files": ["combined.txt"],
                "integration_evidence": {"summary": "both features are present"},
                "test_evidence": [{
                    "command": "cargo test --workspace",
                    "exit_code": 0
                }]
            }),
        ),
        message(
            "APPROVED_RESULT",
            "RESULT_REVIEW",
            1,
            Some(1),
            Some("consensus/test-run"),
            Some(INTEGRATION_SHA),
            json!({
                "approved_plan_revision": 1,
                "approved_primary_sha": PRIMARY_SHA,
                "approved_reviewer_sha": REVIEWER_SHA,
                "approved_integration_branch": "consensus/test-run",
                "approved_integration_sha": INTEGRATION_SHA,
                "uncovered_items": []
            }),
        ),
    ]
}

fn plan_revision_replies() -> Vec<Value> {
    let mut replies = conflict_free_replies();
    let first_plan = replies[2].clone();
    let integration = message(
        "INTEGRATION_READY",
        "INTEGRATE",
        2,
        Some(2),
        Some("consensus/test-run"),
        Some(INTEGRATION_SHA),
        json!({
            "changed_files": ["combined.txt"],
            "integration_evidence": {"summary": "revised plan implemented"},
            "test_evidence": [{"command": "cargo test --workspace", "exit_code": 0}]
        }),
    );
    let result_approval = message(
        "APPROVED_RESULT",
        "RESULT_REVIEW",
        1,
        Some(2),
        Some("consensus/test-run"),
        Some(INTEGRATION_SHA),
        json!({
            "approved_plan_revision": 2,
            "approved_primary_sha": PRIMARY_SHA,
            "approved_reviewer_sha": REVIEWER_SHA,
            "approved_integration_branch": "consensus/test-run",
            "approved_integration_sha": INTEGRATION_SHA,
            "uncovered_items": []
        }),
    );
    replies.truncate(2);
    replies.extend([
        first_plan,
        changes_required("PLAN_REVIEW", 1, 1, None, None, "missing-reviewer-edge"),
        message(
            "PLAN_READY",
            "PLAN_REVIEW",
            2,
            Some(2),
            None,
            None,
            json!({
                "primary_contract": {"items": ["primary-feature"]},
                "reviewer_contract": {"items": ["reviewer-feature"]},
                "plan": {"revision": 2, "steps": ["first-plan", "preserve reviewer edge"]},
                "coverage_matrix": [
                    {"item": "primary-feature", "covered_by": "first-plan"},
                    {"item": "reviewer-feature", "covered_by": "preserve reviewer edge"}
                ]
            }),
        ),
        message(
            "APPROVED_PLAN",
            "PLAN_REVIEW",
            2,
            Some(2),
            None,
            None,
            json!({
                "approved_plan_revision": 2,
                "approved_primary_sha": PRIMARY_SHA,
                "approved_reviewer_sha": REVIEWER_SHA,
                "uncovered_items": []
            }),
        ),
        integration,
        result_approval,
    ]);
    replies
}

fn result_revision_replies() -> Vec<Value> {
    let mut replies = conflict_free_replies();
    replies.truncate(5);
    let revised_sha = "dddddddddddddddddddddddddddddddddddddddd";
    replies.extend([
        changes_required(
            "RESULT_REVIEW",
            1,
            1,
            Some("consensus/test-run"),
            Some(INTEGRATION_SHA),
            "missing-result-edge",
        ),
        message(
            "INTEGRATION_READY",
            "INTEGRATE",
            2,
            Some(1),
            Some("consensus/test-run"),
            Some(revised_sha),
            json!({
                "changed_files": ["combined.txt", "reviewer-edge.txt"],
                "integration_evidence": {"summary": "reviewer edge restored"},
                "test_evidence": [{"command": "cargo test --workspace", "exit_code": 0}]
            }),
        ),
        message(
            "APPROVED_RESULT",
            "RESULT_REVIEW",
            2,
            Some(1),
            Some("consensus/test-run"),
            Some(revised_sha),
            json!({
                "approved_plan_revision": 1,
                "approved_primary_sha": PRIMARY_SHA,
                "approved_reviewer_sha": REVIEWER_SHA,
                "approved_integration_branch": "consensus/test-run",
                "approved_integration_sha": revised_sha,
                "uncovered_items": []
            }),
        ),
    ]);
    replies
}

fn no_progress_replies() -> Vec<Value> {
    let base = conflict_free_replies();
    let plan_payload = json!({
        "primary_contract": {"items": ["primary-feature"]},
        "reviewer_contract": {"items": ["reviewer-feature"]},
        "plan": {"steps": ["unchanged plan"]},
        "coverage_matrix": [{"item": "both", "covered_by": "unchanged plan"}]
    });
    vec![
        base[0].clone(),
        base[1].clone(),
        message(
            "PLAN_READY",
            "PLAN_REVIEW",
            1,
            Some(1),
            None,
            None,
            plan_payload.clone(),
        ),
        changes_required("PLAN_REVIEW", 1, 1, None, None, "same-issue"),
        message(
            "PLAN_READY",
            "PLAN_REVIEW",
            2,
            Some(2),
            None,
            None,
            plan_payload,
        ),
        changes_required("PLAN_REVIEW", 2, 2, None, None, "same-issue"),
    ]
}

fn changes_required(
    phase: &str,
    round: u32,
    plan_revision: u32,
    integration_branch: Option<&str>,
    integration_sha: Option<&str>,
    issue_id: &str,
) -> Value {
    let mut value = message(
        "CHANGES_REQUIRED",
        phase,
        round,
        Some(plan_revision),
        integration_branch,
        integration_sha,
        json!({
            "issue_ids": [issue_id],
            "evidence": [{"issue_id": issue_id, "detail": "must be preserved"}]
        }),
    );
    value["reason_code"] = json!("COVERAGE_GAP");
    value
}

fn message(
    message_type: &str,
    phase: &str,
    round: u32,
    plan_revision: Option<u32>,
    integration_branch: Option<&str>,
    integration_sha: Option<&str>,
    payload: Value,
) -> Value {
    json!({
        "protocol": "worktree-merge-consensus/v1",
        "run_id": RUN_ID,
        "message_type": message_type,
        "phase": phase,
        "round": round,
        "primary_sha": PRIMARY_SHA,
        "reviewer_sha": REVIEWER_SHA,
        "plan_revision": plan_revision,
        "integration_branch": integration_branch,
        "integration_sha": integration_sha,
        "reason_code": null,
        "payload": payload
    })
}
