use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use app_server_client::{
    AppEvent, AppServer, AppServerError, CommandExecRequest, CommandExecResult, InitializeInfo,
    McpServerStatus, PARTICIPANT_MCP_SERVER, PARTICIPANT_PATCH_TOOL, ParticipantMcpConfig,
    ThreadDetail, ThreadForkPolicy, ThreadPage, ThreadResumePolicy, ThreadRuntimeStatus,
    ThreadSummary, TurnExecutionPolicy, TurnHandle,
};
use async_trait::async_trait;
use consensus_core::{
    canonical_json_hash,
    git::GitInspector,
    state::{NextAction, Phase, Role, RunDiagnostic, RunFacts, RunState, RunStatus, TestEvidence},
};
use consensus_daemon::{
    PrimaryBindingMode,
    coordinator::{
        Coordinator, CoordinatorOptions, GitRepositorySafety, RepositorySafety, SafetyError,
        StartRequest,
    },
    store::SqliteRunStore,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use uuid::Uuid;

const RUN_ID: &str = "4b230bd8-d870-4ef4-bf20-05a4c61020af";
const PRIMARY_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const REVIEWER_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const INTEGRATION_SHA: &str = "cccccccccccccccccccccccccccccccccccccccc";
const CORRECTED_INTEGRATION_SHA: &str = "dddddddddddddddddddddddddddddddddddddddd";

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

#[test]
fn unavailable_frozen_worktree_has_stable_public_reason_code() {
    let facts = fixture_run().facts;
    let safety = GitRepositorySafety::default();

    let error = safety.verify_frozen(&facts).unwrap_err();
    let branch_error = safety
        .verify_branch_absent(&facts, "consensus/test-run")
        .unwrap_err();

    assert_eq!(error.code(), "WORKTREE_UNAVAILABLE");
    assert_eq!(branch_error.code(), "WORKTREE_UNAVAILABLE");
}

#[test]
fn replaced_frozen_worktree_is_reported_as_source_drift() {
    let root = tempfile::tempdir().unwrap();
    let primary_path = root.path().join("primary");
    let reviewer_path = root.path().join("reviewer");
    fs::create_dir(&primary_path).unwrap();
    run_git(&primary_path, &["init", "--initial-branch=primary"]);
    run_git(&primary_path, &["config", "user.name", "Consensus Test"]);
    run_git(
        &primary_path,
        &["config", "user.email", "consensus@example.invalid"],
    );
    fs::write(primary_path.join("base.txt"), "base\n").unwrap();
    run_git(&primary_path, &["add", "base.txt"]);
    run_git(&primary_path, &["commit", "-m", "base"]);
    run_git(&primary_path, &["branch", "reviewer"]);
    run_git(
        &primary_path,
        &[
            "worktree",
            "add",
            reviewer_path.to_str().unwrap(),
            "reviewer",
        ],
    );
    let inspector = GitInspector::default();
    let primary = inspector.inspect_worktree(&primary_path).unwrap();
    let reviewer = inspector.inspect_worktree(&reviewer_path).unwrap();
    let facts = RunFacts {
        run_id: Uuid::new_v4(),
        primary_thread_id: "primary-thread".into(),
        reviewer_thread_id: "reviewer-thread".into(),
        primary_worktree: primary.worktree.clone(),
        reviewer_worktree: reviewer.worktree.clone(),
        git_common_dir: primary.common_dir.clone(),
        primary_sha: primary.head_sha.clone(),
        reviewer_sha: reviewer.head_sha.clone(),
        primary_ref: primary.source_ref.map(|source| source.name),
        reviewer_ref: reviewer.source_ref.map(|source| source.name),
    };
    fs::rename(&reviewer_path, root.path().join("reviewer-moved")).unwrap();
    fs::create_dir(&reviewer_path).unwrap();

    let error = GitRepositorySafety::default()
        .verify_frozen(&facts)
        .unwrap_err();

    assert_eq!(error.code(), "SOURCE_DRIFT");
}

#[test]
fn unavailable_primary_before_verification_clone_keeps_public_reason_code() {
    let facts = fixture_run().facts;
    let destination_root = tempfile::tempdir().unwrap();
    let destination = destination_root.path().join("verification");

    let error = GitRepositorySafety::default()
        .prepare_verification_workspace(&facts, INTEGRATION_SHA, &destination)
        .unwrap_err();

    assert_eq!(error.code(), "WORKTREE_UNAVAILABLE");
}

#[test]
fn corrective_patch_rejects_head_movement_after_recovery_verification_without_mutation() {
    let fixture = RealGitSafetyFixture::integrated();
    let safety = GitRepositorySafety::default();
    safety
        .verify_integration(
            &fixture.facts,
            "consensus/test-run",
            &fixture.integration_sha,
            &fixture.changed_files,
        )
        .unwrap();
    fs::write(fixture.primary.join("moved.txt"), "moved\n").unwrap();
    run_git(&fixture.primary, &["add", "moved.txt"]);
    run_git(&fixture.primary, &["commit", "-m", "move-after-recovery"]);
    let status_before = git_stdout(&fixture.primary, &["status", "--porcelain"]);
    let primary_before = fs::read_to_string(fixture.primary.join("primary.txt")).unwrap();
    let patch = "diff --git a/primary.txt b/primary.txt\n--- a/primary.txt\n+++ b/primary.txt\n@@ -1 +1,2 @@\n primary\n+corrected\n";

    let error = safety
        .apply_corrective_integration_patch(
            &fixture.facts,
            "consensus/test-run",
            &fixture.integration_sha,
            patch,
        )
        .unwrap_err();

    assert_eq!(error.code(), "STALE_INTEGRATION_SHA");
    assert_eq!(
        fs::read_to_string(fixture.primary.join("primary.txt")).unwrap(),
        primary_before
    );
    assert_eq!(
        git_stdout(&fixture.primary, &["status", "--porcelain"]),
        status_before
    );
}

#[test]
fn production_git_safety_rejects_a_dirty_corrective_target() {
    let fixture = RealGitSafetyFixture::integrated();
    fs::write(fixture.primary.join("dirty.txt"), "dirty\n").unwrap();

    let error = GitRepositorySafety::default()
        .verify_integration(
            &fixture.facts,
            "consensus/test-run",
            &fixture.integration_sha,
            &fixture.changed_files,
        )
        .unwrap_err();

    assert_eq!(error.code(), "DIRTY_WORKTREE");
}

#[test]
fn production_git_safety_rejects_a_moved_corrective_target_head() {
    let fixture = RealGitSafetyFixture::integrated();
    run_git(
        &fixture.primary,
        &["commit", "--allow-empty", "-m", "move-target"],
    );

    let error = GitRepositorySafety::default()
        .verify_integration(
            &fixture.facts,
            "consensus/test-run",
            &fixture.integration_sha,
            &fixture.changed_files,
        )
        .unwrap_err();

    assert_eq!(error.code(), "STALE_INTEGRATION_SHA");
}

#[test]
fn production_git_safety_rejects_frozen_source_ref_drift() {
    let fixture = RealGitSafetyFixture::integrated();
    fs::write(fixture.reviewer.join("drift.txt"), "drift\n").unwrap();
    run_git(&fixture.reviewer, &["add", "drift.txt"]);
    run_git(&fixture.reviewer, &["commit", "-m", "drift-reviewer"]);

    let error = GitRepositorySafety::default()
        .verify_integration(
            &fixture.facts,
            "consensus/test-run",
            &fixture.integration_sha,
            &fixture.changed_files,
        )
        .unwrap_err();

    assert_eq!(error.code(), "SOURCE_DRIFT");
}

#[test]
fn production_git_safety_rejects_missing_frozen_source_ancestry() {
    let fixture = RealGitSafetyFixture::integrated();
    run_git(
        &fixture.primary,
        &["reset", "--hard", &fixture.facts.primary_sha],
    );
    let rewritten = GitInspector::default()
        .inspect_integration(&fixture.primary, &fixture.facts)
        .unwrap();

    let error = GitRepositorySafety::default()
        .verify_integration(
            &fixture.facts,
            "consensus/test-run",
            &rewritten.worktree.head_sha,
            &rewritten.changed_files,
        )
        .unwrap_err();

    assert_eq!(error.code(), "MISSING_SOURCE_ANCESTRY");
}

#[tokio::test]
async fn task_cwd_is_metadata_and_bound_worktrees_drive_turns() {
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
            participant_mcp_executable: participant_mcp_executable(),
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
    let accepted = result.accepted_result.as_ref().unwrap();
    assert_eq!(accepted.integration_sha, INTEGRATION_SHA);
    assert_eq!(accepted.tests[0].command, "cargo test --workspace");
    assert!(accepted.source_refs_unchanged);
    assert!(accepted.publication.local_only);
    assert!(!accepted.publication.pushed);
    assert_eq!(
        app.request_order(),
        vec![
            "primary:REQUEST_PRIMARY_CONTRACT",
            "reviewer:REQUEST_REVIEWER_CONTRACT",
            "primary:REQUEST_PRIMARY_PLAN",
            "reviewer:REQUEST_REVIEWER_PLAN_VERDICT",
            "primary:REQUEST_PRIMARY_INTEGRATION",
            "primary:REQUEST_PRIMARY_VERIFICATION",
            "reviewer:REQUEST_REVIEWER_RESULT_VERDICT",
        ]
    );
    assert_eq!(
        app.resume_order(),
        vec![
            "primary", "reviewer", "primary", "reviewer", "primary", "primary", "reviewer",
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
    assert!(!safety_events.iter().any(|event| event == "thread-cwd"));
    let policies = app.policies();
    assert_eq!(policies.len(), 7);
    for (index, expected) in [
        (0, "/repo/primary"),
        (1, "/repo/reviewer"),
        (2, "/repo/primary"),
        (3, "/repo/reviewer"),
        (6, "/repo/reviewer"),
    ] {
        assert!(matches!(
            &policies[index],
            TurnExecutionPolicy::ReadOnly { cwd } if cwd == &PathBuf::from(expected)
        ));
    }
    assert!(matches!(
        &policies[4],
        TurnExecutionPolicy::PrimaryIntegration { cwd, git_common_dir }
            if cwd == &PathBuf::from("/repo/primary")
                && git_common_dir == &PathBuf::from("/repo/.git")
    ));
    assert!(matches!(
        &policies[5],
        TurnExecutionPolicy::PrimaryVerification { cwd }
            if cwd.to_string_lossy().contains(RUN_ID)
    ));
    assert!(policies.iter().enumerate().all(|(index, policy)| {
        matches!(index, 4 | 5) || matches!(policy, TurnExecutionPolicy::ReadOnly { .. })
    }));
}

#[tokio::test]
async fn participant_patch_preflight_orders_resume_inventory_and_turn_start() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::new(conflict_free_replies()));
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store,
        Arc::new(RecordingSafety::default()),
        fast_options(),
    );

    coordinator
        .start(
            fixture_run(),
            StartRequest {
                integration_branch: Some("consensus/test-run".into()),
                test_commands: vec!["cargo test --workspace".into()],
            },
        )
        .await
        .unwrap();
    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    let methods = app.method_order();
    let integration_start = methods
        .iter()
        .position(|method| method == "turn/start:primary:REQUEST_PRIMARY_INTEGRATION")
        .unwrap();
    assert_eq!(
        &methods[integration_start - 2..=integration_start],
        [
            "thread/resume:primary",
            "mcpServerStatus/list:primary",
            "turn/start:primary:REQUEST_PRIMARY_INTEGRATION",
        ]
    );
    let resume_policies = app.resume_policies();
    assert!(
        resume_policies
            .iter()
            .all(|policy| policy == &ThreadResumePolicy::Default)
    );
    assert_primary_turns_have_exact_preflight(&app, "primary");
}

#[tokio::test]
async fn not_loaded_primary_binds_directly_before_the_first_primary_turn() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let inspection_store = store.clone();
    let app = Arc::new(
        FakeAppServer::new(conflict_free_replies())
            .without_primary_participant()
            .with_primary_runtime_status(ThreadRuntimeStatus::NotLoaded),
    );
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

    assert_eq!(result.status, RunStatus::Accepted);
    let binding = inspection_store
        .active_primary_binding(RUN_ID)
        .unwrap()
        .unwrap();
    assert_eq!(binding.mode, PrimaryBindingMode::Direct);
    assert_eq!(binding.generation, 1);
    assert_eq!(binding.source_primary_thread_id, "primary");
    assert_eq!(binding.effective_primary_thread_id, "primary");
    assert!(app.forks().is_empty());
    assert!(matches!(
        app.resume_policies().first(),
        Some(ThreadResumePolicy::Participant(ParticipantMcpConfig {
            participant_executable
        })) if participant_executable == &participant_mcp_executable()
    ));
    assert_primary_turns_have_exact_preflight(&app, "primary");
    assert!(
        app.request_order()
            .iter()
            .filter(|request| request.starts_with("reviewer:"))
            .all(|request| request.starts_with("reviewer:"))
    );
}

#[tokio::test]
async fn loaded_primary_with_existing_participant_binds_directly_without_fork() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let inspection_store = store.clone();
    let app = Arc::new(FakeAppServer::new(conflict_free_replies()));
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

    assert_eq!(result.status, RunStatus::Accepted);
    let binding = inspection_store
        .active_primary_binding(RUN_ID)
        .unwrap()
        .unwrap();
    assert_eq!(binding.mode, PrimaryBindingMode::Direct);
    assert_eq!(binding.source_primary_thread_id, "primary");
    assert_eq!(binding.effective_primary_thread_id, "primary");
    assert!(app.forks().is_empty());
    assert_primary_turns_have_exact_preflight(&app, "primary");
    let prompts = app.prompts();
    let primary_prompt = prompts
        .iter()
        .find(|prompt| prompt.contains("REQUEST_PRIMARY_CONTRACT"))
        .unwrap();
    assert_eq!(
        prompt_json_block(primary_prompt, "Primary participant execution identity:"),
        json!({
            "source_primary_thread_id": "primary",
            "effective_primary_thread_id": "primary",
            "binding_mode": "DIRECT",
            "binding_generation": 1
        })
    );
    assert!(
        prompts
            .iter()
            .filter(|prompt| prompt.contains("REQUEST_REVIEWER"))
            .all(|prompt| !prompt.contains("Primary participant execution identity:"))
    );
}

#[tokio::test]
async fn preloaded_primary_uses_ephemeral_summary_reads() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let inspection_store = store.clone();
    let app = Arc::new(FakeAppServer::new(conflict_free_replies()).without_primary_participant());
    app.inject_completed_turn(
        "primary",
        "source-history-1",
        "historical source prompt one",
        json!("historical source response one"),
    );
    app.inject_completed_turn(
        "primary",
        "source-history-2",
        "historical source prompt two",
        json!("historical source response two"),
    );
    let source_turn_ids = app.turn_ids("primary");
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

    assert_eq!(result.status, RunStatus::Accepted);
    let binding = inspection_store
        .active_primary_binding(RUN_ID)
        .unwrap()
        .unwrap();
    assert_eq!(binding.mode, PrimaryBindingMode::EphemeralFork);
    assert_eq!(binding.generation, 1);
    assert_eq!(binding.source_primary_thread_id, "primary");
    assert_eq!(
        binding.effective_primary_thread_id,
        "primary-consensus-mirror-1"
    );
    let forks = app.forks();
    assert_eq!(forks.len(), 1);
    assert!(matches!(
        &forks[0],
        (source, effective, ThreadForkPolicy::EphemeralParticipant(
            ParticipantMcpConfig {
                participant_executable
            }
        )) if source == "primary"
            && effective == "primary-consensus-mirror-1"
            && participant_executable == &participant_mcp_executable()
    ));
    assert!(
        app.request_order()
            .iter()
            .filter(|request| request.contains("REQUEST_PRIMARY"))
            .all(|request| request.starts_with("primary-consensus-mirror-1:"))
    );
    assert!(
        app.request_order()
            .iter()
            .filter(|request| request.contains("REQUEST_REVIEWER"))
            .all(|request| request.starts_with("reviewer:"))
    );
    let methods = app.method_order();
    let source_goal = methods
        .iter()
        .position(|method| method == "thread/goal/get:primary")
        .unwrap();
    let fork = methods
        .iter()
        .position(|method| method == "thread/fork:primary:primary-consensus-mirror-1")
        .unwrap();
    assert!(source_goal < fork);
    assert!(
        methods
            .iter()
            .all(|method| method != "thread/goal/get:primary-consensus-mirror-1")
    );
    assert_primary_turns_have_exact_preflight(&app, "primary-consensus-mirror-1");
    assert_eq!(app.turn_ids("primary"), source_turn_ids);
    assert!(
        app.turn_ids("primary-consensus-mirror-1")
            .starts_with(&source_turn_ids)
    );
    let primary_prompt = app
        .prompts()
        .into_iter()
        .find(|prompt| prompt.contains("REQUEST_PRIMARY_CONTRACT"))
        .unwrap();
    assert_eq!(
        prompt_json_block(&primary_prompt, "Primary participant execution identity:"),
        json!({
            "source_primary_thread_id": "primary",
            "effective_primary_thread_id": "primary-consensus-mirror-1",
            "binding_mode": "EPHEMERAL_FORK",
            "binding_generation": 1
        })
    );
    assert!(
        app.method_order()
            .iter()
            .all(|method| method != "thread/read-full:primary-consensus-mirror-1")
    );
    assert!(
        app.resumes()
            .iter()
            .all(|thread_id| thread_id != "primary-consensus-mirror-1")
    );
}

#[tokio::test]
async fn invalid_primary_mirror_fails_before_any_model_turn() {
    for (case, expected_reason) in [
        ("source-id", "AMBIGUOUS_THREAD"),
        ("reviewer-id", "AMBIGUOUS_THREAD"),
        ("missing-history", "HISTORY_UNAVAILABLE"),
        ("reordered-history", "HISTORY_UNAVAILABLE"),
        ("goal", "HISTORY_UNAVAILABLE"),
        ("active", "HISTORY_UNAVAILABLE"),
        ("missing-tool", "PATCH_TOOL_UNAVAILABLE"),
        ("expanded-inventory", "PATCH_TOOL_UNAVAILABLE"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
        let base = FakeAppServer::new(conflict_free_replies()).without_primary_participant();
        let configured = match case {
            "source-id" => base.with_fork_identity(ForkIdentity::Source),
            "reviewer-id" => base.with_fork_identity(ForkIdentity::Reviewer),
            "missing-history" => base.with_fork_history(ForkHistory::MissingLast),
            "reordered-history" => base.with_fork_history(ForkHistory::Reversed),
            "goal" => base.with_source_goal(json!({"status": "active"})),
            "active" => base.with_fork_runtime_status(ThreadRuntimeStatus::Active),
            "missing-tool" => base.with_participant_inventory(ParticipantInventory::MissingTool),
            "expanded-inventory" => {
                base.with_participant_inventory(ParticipantInventory::ExtraTool)
            }
            _ => unreachable!(),
        };
        let app = Arc::new(configured);
        app.inject_completed_turn(
            "primary",
            "source-history-1",
            "historical source prompt one",
            json!("historical source response one"),
        );
        app.inject_completed_turn(
            "primary",
            "source-history-2",
            "historical source prompt two",
            json!("historical source response two"),
        );
        let safety = Arc::new(RecordingSafety::default());
        let coordinator =
            Coordinator::new(Arc::clone(&app), store, Arc::clone(&safety), fast_options());

        coordinator
            .start(fixture_run(), start_request())
            .await
            .unwrap();
        let blocked = coordinator.drive(RUN_ID).await.unwrap();

        assert_eq!(blocked.status, RunStatus::Blocked, "case={case}");
        assert_eq!(
            blocked.reason_code.as_deref(),
            Some(expected_reason),
            "case={case}"
        );
        assert_eq!(
            app.forks().len(),
            usize::from(case != "goal"),
            "case={case}"
        );
        assert!(app.request_order().is_empty(), "case={case}");
        assert!(
            safety
                .events()
                .iter()
                .all(|event| !event.starts_with("result:")),
            "case={case}"
        );
    }
}

#[tokio::test]
async fn mirror_capability_failure_records_binding_identity() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(
        FakeAppServer::new(conflict_free_replies())
            .without_primary_participant()
            .with_participant_failure_after_status_calls(3),
    );
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
    let blocked = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(blocked.status, RunStatus::Blocked);
    assert_eq!(
        blocked.reason_code.as_deref(),
        Some("PATCH_TOOL_UNAVAILABLE")
    );
    let diagnostic = blocked.last_error.unwrap();
    assert_eq!(diagnostic.source_thread_id.as_deref(), Some("primary"));
    assert_eq!(
        diagnostic.effective_thread_id.as_deref(),
        Some("primary-consensus-mirror-1")
    );
    assert_eq!(diagnostic.participant_binding_generation, Some(1));
    assert_eq!(
        diagnostic.participant_binding_mode.as_deref(),
        Some("EPHEMERAL_FORK")
    );
    assert_eq!(
        diagnostic.participant_server.as_deref(),
        Some(PARTICIPANT_MCP_SERVER)
    );
}

#[tokio::test]
async fn missing_mirror_is_recreated_only_after_a_completed_action_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let store_path = temp.path().join("state.db");
    let store = SqliteRunStore::open(&store_path).unwrap();
    let app = Arc::new(
        FakeAppServer::new(conflict_free_replies())
            .without_primary_participant()
            .with_remove_mirror_after_request(2),
    );
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
    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    let forks = app.forks();
    assert_eq!(forks.len(), 2);
    assert_eq!(forks[0].1, "primary-consensus-mirror-1");
    assert_eq!(forks[1].1, "primary-consensus-mirror-2");
    let binding = store.active_primary_binding(RUN_ID).unwrap().unwrap();
    assert_eq!(binding.generation, 2);
    assert_eq!(
        binding.effective_primary_thread_id,
        "primary-consensus-mirror-2"
    );
    assert!(
        app.request_order()
            .contains(&"primary-consensus-mirror-1:REQUEST_PRIMARY_CONTRACT".to_owned())
    );
    assert!(
        app.request_order()
            .contains(&"primary-consensus-mirror-2:REQUEST_PRIMARY_PLAN".to_owned())
    );
    assert!(
        !app.request_order()
            .iter()
            .any(|request| request == "primary:REQUEST_PRIMARY_PLAN")
    );
    let contract_generation = Connection::open(store_path)
        .unwrap()
        .query_row(
            "SELECT participant_binding_generation
             FROM turns
             WHERE run_id = ?1 AND role = 'PRIMARY' AND phase = 'CONTRACT'",
            [RUN_ID],
            |row| row.get::<_, Option<u32>>(0),
        )
        .unwrap();
    assert_eq!(contract_generation, Some(1));
}

#[tokio::test]
async fn missing_mirror_with_uncertain_turn_is_never_reforked() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(
        FakeAppServer::new(conflict_free_replies())
            .without_primary_participant()
            .with_lost_first_start_response()
            .with_remove_mirror_after_request(1),
    );
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
    let paused = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(paused.status, RunStatus::PausedUserAction);
    assert_eq!(paused.reason_code.as_deref(), Some("COMMUNICATION_FAILURE"));
    let pending = store.pending_send(RUN_ID).unwrap().unwrap();
    assert_eq!(pending.thread_id, None);
    assert_eq!(pending.turn_id, None);
    assert!(pending.turn_start_intent_at.is_some());
    assert_eq!(pending.participant_binding_generation, Some(1));
    assert_eq!(app.forks().len(), 1);

    let resumed = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(resumed.status, RunStatus::PausedUserAction);
    assert_eq!(
        resumed.reason_code.as_deref(),
        Some("COMMUNICATION_FAILURE")
    );
    assert_eq!(app.forks().len(), 1);
    assert_eq!(
        store
            .active_primary_binding(RUN_ID)
            .unwrap()
            .unwrap()
            .generation,
        1
    );
}

#[tokio::test]
async fn participant_patch_inventory_mismatch_blocks_before_integration_turn() {
    for inventory in [
        ParticipantInventory::MissingServer,
        ParticipantInventory::MissingTool,
        ParticipantInventory::ExtraTool,
        ParticipantInventory::MalformedDefinition,
    ] {
        let temp = tempfile::tempdir().unwrap();
        let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
        let app = Arc::new(
            FakeAppServer::new(conflict_free_replies()).with_participant_inventory(inventory),
        );
        let safety = Arc::new(RecordingSafety::default());
        let coordinator =
            Coordinator::new(Arc::clone(&app), store, Arc::clone(&safety), fast_options());

        coordinator
            .start(
                fixture_run(),
                StartRequest {
                    integration_branch: Some("consensus/test-run".into()),
                    test_commands: vec!["cargo test --workspace".into()],
                },
            )
            .await
            .unwrap();
        let blocked = coordinator.drive(RUN_ID).await.unwrap();

        assert_eq!(
            blocked.status,
            RunStatus::Blocked,
            "inventory={inventory:?}"
        );
        assert_eq!(
            blocked.reason_code.as_deref(),
            Some("PATCH_TOOL_UNAVAILABLE"),
            "inventory={inventory:?}"
        );
        assert!(
            app.request_order()
                .iter()
                .all(|request| !request.ends_with("REQUEST_PRIMARY_INTEGRATION")),
            "inventory={inventory:?}"
        );
        assert!(
            app.method_order()
                .iter()
                .all(|method| method != "turn/start:primary:REQUEST_PRIMARY_INTEGRATION"),
            "inventory={inventory:?}"
        );
        assert!(
            safety
                .events()
                .iter()
                .all(|event| !event.starts_with("result:")),
            "inventory={inventory:?}"
        );
    }
}

#[tokio::test]
async fn participant_patch_status_method_unavailable_is_incompatible_codex() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(
        FakeAppServer::new(conflict_free_replies())
            .with_participant_inventory(ParticipantInventory::StatusUnavailable),
    );
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store,
        Arc::new(RecordingSafety::default()),
        fast_options(),
    );

    coordinator
        .start(
            fixture_run(),
            StartRequest {
                integration_branch: Some("consensus/test-run".into()),
                test_commands: vec!["cargo test --workspace".into()],
            },
        )
        .await
        .unwrap();
    let incompatible = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(incompatible.status, RunStatus::IncompatibleCodex);
    assert_eq!(
        incompatible.reason_code.as_deref(),
        Some("INCOMPATIBLE_CODEX")
    );
    assert!(
        app.method_order()
            .iter()
            .all(|method| method != "turn/start:primary:REQUEST_PRIMARY_INTEGRATION")
    );
}

#[tokio::test]
async fn marker_v2_keeps_plan_and_review_prose_free_form() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::new(marker_replies()).with_marker_protocol());
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
    let accepted = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(accepted.status, RunStatus::Accepted);
    assert_eq!(accepted.integration_sha.as_deref(), Some(INTEGRATION_SHA));
    let plan = accepted.current_plan_payload.as_ref().unwrap();
    assert_eq!(plan["plan"]["format"], "markdown");
    assert!(
        plan["plan"]["content"]
            .as_str()
            .unwrap()
            .contains("Preserve both implementations")
    );
    let integration = accepted.current_integration_payload.as_ref().unwrap();
    assert_eq!(integration["changed_files"], json!(["combined.txt"]));
    assert_eq!(accepted.test_evidence.len(), 1);
    assert!(app.prompts().iter().all(|prompt| {
        prompt.contains("worktree-merge-consensus/v2")
            && prompt.contains("Do not return the legacy v1 protocol envelope")
    }));
}

#[tokio::test]
async fn coordinator_owned_verification_executes_marker_only_commands_in_order() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::new(marker_replies()).with_marker_protocol());
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store,
        Arc::new(RecordingSafety::default()),
        fast_options(),
    );
    coordinator
        .start(
            fixture_run(),
            StartRequest {
                integration_branch: Some("consensus/test-run".into()),
                test_commands: vec![
                    "cargo fmt --all -- --check".into(),
                    "cargo test --locked".into(),
                ],
            },
        )
        .await
        .unwrap();

    let accepted = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(accepted.status, RunStatus::Accepted);
    assert_eq!(
        app.executed_commands(),
        vec![
            vec!["cargo", "fmt", "--all", "--", "--check"],
            vec!["cargo", "test", "--locked"],
            vec!["cargo", "test", "--workspace"],
        ]
    );
    assert_eq!(accepted.test_evidence.len(), 3);
    assert!(
        accepted
            .test_evidence
            .iter()
            .all(|item| item.item_id.starts_with("coordinator-command/"))
    );
    assert!(app.executed_command_requests().iter().all(|request| {
        request.cwd == accepted.test_evidence[0].cwd
            && request.timeout_ms == 1_000
            && request.output_bytes_cap == 65_536
    }));
    let verification_turn = app
        .detail("primary")
        .turns
        .into_iter()
        .find(|turn| {
            turn["items"][0]["content"][0]["text"]
                .as_str()
                .is_some_and(|prompt| prompt_action(prompt) == "REQUEST_PRIMARY_VERIFICATION")
        })
        .unwrap();
    assert!(
        verification_turn["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| {
                matches!(
                    item.get("type").and_then(Value::as_str),
                    Some("userMessage" | "agentMessage")
                )
            })
    );
}

#[tokio::test]
async fn coordinator_owned_verification_runs_after_nonzero_and_routes_bounded_diagnostics() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let mut replies = marker_replies();
    replies.insert(
        5,
        json!(
            "<consensus-result>INTEGRATION_READY</consensus-result>\n\nThe reported verification failure is corrected."
        ),
    );
    let app = Arc::new(
        FakeAppServer::new(replies)
            .with_marker_protocol()
            .with_verification_behavior(VerificationBehavior::FailedExecutionThenPass),
    );
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store,
        Arc::new(AdvancingIntegrationSafety::default()),
        fast_options(),
    );
    coordinator
        .start(
            fixture_run(),
            StartRequest {
                integration_branch: Some("consensus/test-run".into()),
                test_commands: vec![
                    "cargo fmt --all -- --check".into(),
                    "cargo test --locked".into(),
                ],
            },
        )
        .await
        .unwrap();

    let accepted = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(accepted.facts.run_id.to_string(), RUN_ID);
    assert_eq!(accepted.status, RunStatus::Accepted);
    assert_eq!(accepted.round, 2);
    assert_eq!(
        accepted.integration_sha.as_deref(),
        Some(CORRECTED_INTEGRATION_SHA)
    );
    assert_eq!(
        &app.executed_commands()[..3],
        &[
            vec!["cargo", "fmt", "--all", "--", "--check"],
            vec!["cargo", "test", "--locked"],
            vec!["cargo", "test", "--workspace"],
        ]
    );
    let corrective_prompt = app
        .prompts()
        .into_iter()
        .filter(|prompt| prompt_action(prompt) == "REQUEST_PRIMARY_INTEGRATION")
        .nth(1)
        .unwrap();
    let corrective_payload = prompt_json_block(&corrective_prompt, "Complete current payload");
    let diagnostic = corrective_payload["result_feedback"]["failed_tests"][0]["output"]
        .as_str()
        .unwrap();
    assert!(diagnostic.starts_with("[earlier output truncated]\n"));
    assert_eq!(
        corrective_payload["result_feedback"]["verification_summary"],
        "All frozen commands completed."
    );
    assert!(diagnostic.len() <= 16_384);
}

#[tokio::test]
async fn marker_only_verification_rejects_participant_side_effects_before_execution() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(
        FakeAppServer::new(marker_replies())
            .with_marker_protocol()
            .with_verification_item("commandExecution"),
    );
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

    let blocked = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(blocked.status, RunStatus::Blocked);
    assert_eq!(blocked.reason_code.as_deref(), Some("FORBIDDEN_OPERATION"));
    assert!(app.executed_commands().is_empty());
}

#[tokio::test]
async fn blocked_marker_only_verification_rejects_participant_side_effects_before_execution() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(
        FakeAppServer::new(marker_replies())
            .with_marker_protocol()
            .with_verification_behavior(VerificationBehavior::CargoUnavailable)
            .with_verification_item("fileChange"),
    );
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

    let blocked = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(blocked.status, RunStatus::Blocked);
    assert_eq!(blocked.reason_code.as_deref(), Some("FORBIDDEN_OPERATION"));
    assert_eq!(blocked.accepted_result, None);
    assert!(app.executed_commands().is_empty());
}

#[tokio::test]
async fn marker_v2_blocked_reason_is_bound_to_the_pending_turn() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(
        FakeAppServer::new(vec![json!(
            "<consensus-result>BLOCKED:SOURCE_BINDING_MISMATCH</consensus-result>\n\nThe frozen source does not match this task's implementation history."
        )])
        .with_marker_protocol(),
    );
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
    let blocked = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(blocked.status, RunStatus::Blocked);
    assert_eq!(
        blocked.reason_code.as_deref(),
        Some("SOURCE_BINDING_MISMATCH")
    );
    assert_eq!(app.request_count(), 1);
}

#[tokio::test]
async fn start_requires_the_exact_controlled_patch_approval_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app =
        Arc::new(FakeAppServer::new(conflict_free_replies()).with_approval_mode(Some("prompt")));
    let coordinator = Coordinator::new(
        app,
        store.clone(),
        Arc::new(RecordingSafety::default()),
        fast_options(),
    );

    let error = coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap_err();

    assert_eq!(error.code(), "APPROVAL_CONFIGURATION_REQUIRED");
    assert!(error.detail().contains("codex-consensus configure"));
    assert!(store.load_run(RUN_ID).unwrap().is_none());
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
async fn invalid_plan_approval_revision_can_resume_the_same_blocked_run() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let mut replies = conflict_free_replies();
    let mut invalid_approval = replies[3].clone();
    invalid_approval["payload"]["approved_plan_revision"] = json!(2);
    replies.insert(3, invalid_approval);
    let app = Arc::new(FakeAppServer::new(replies));
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

    let blocked = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(blocked.status, RunStatus::Blocked);
    assert_eq!(blocked.reason_code.as_deref(), Some("INVALID_RESPONSE"));
    assert_eq!(blocked.integration_branch, None);
    assert_eq!(blocked.integration_sha, None);
    assert!(
        blocked
            .last_error
            .as_ref()
            .unwrap()
            .detail
            .contains("approved_plan_revision")
    );
    assert!(
        app.request_order()
            .iter()
            .all(|action| !action.ends_with("REQUEST_PRIMARY_INTEGRATION"))
    );

    let accepted = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(accepted.status, RunStatus::Accepted);
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 1);
    let verdict_prompts = app
        .prompts()
        .into_iter()
        .filter(|prompt| prompt.contains("REQUEST_REVIEWER_PLAN_VERDICT"))
        .collect::<Vec<_>>();
    assert_eq!(verdict_prompts.len(), 2);
    assert!(
        verdict_prompts[1].contains("The coordinator binds every response to this exact task turn")
    );
    assert!(verdict_prompts[1].contains("<consensus-result>APPROVED</consensus-result>"));
}

#[tokio::test]
async fn legacy_capability_generation_allows_exact_invalid_integration_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let (coordinator, app, store, _safety) =
        seed_invalid_integration_recovery(&path, "worktreeMergeConsensus").await;
    set_turn_capability_generation(&path, None);

    let accepted = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(accepted.status, RunStatus::Accepted);
    assert_eq!(accepted.integration_sha.as_deref(), Some(INTEGRATION_SHA));
    assert_eq!(app.request_count(), 8);
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 1);
    let retry_prompt = app
        .prompts()
        .into_iter()
        .rfind(|prompt| prompt.contains("REQUEST_PRIMARY_INTEGRATION"))
        .unwrap();
    assert!(retry_prompt.contains("Coordinator recovery override"));
    assert!(retry_prompt.contains("Do not call consensus_apply_patch"));
    assert!(retry_prompt.contains("INTEGRATION_READY</consensus-result>"));
}

#[tokio::test]
async fn completed_integration_forbidden_read_only_nonzero_resumes_the_same_run() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let (coordinator, app, store, safety) =
        seed_invalid_integration_recovery(&path, PARTICIPANT_MCP_SERVER).await;
    let pending = store.pending_send(RUN_ID).unwrap().unwrap();
    let thread_id = pending.thread_id.clone().unwrap();
    let turn_id = pending.turn_id.clone().unwrap();
    app.set_patch_plugin_id(&thread_id, &turn_id, Value::Null);
    insert_completed_integration_command_evidence(&app, &thread_id, &turn_id);

    let mut blocked = store.load_run(RUN_ID).unwrap().unwrap();
    blocked.reason_code = Some("FORBIDDEN_OPERATION".into());
    blocked.last_error = Some(RunDiagnostic {
        code: "FORBIDDEN_OPERATION".into(),
        detail: "integration command is not canonically completed with exit code zero".into(),
        operation: None,
        action: NextAction::RequestPrimaryIntegration,
        role: Some(Role::Primary),
        thread_id: Some(thread_id),
        source_thread_id: None,
        effective_thread_id: None,
        participant_binding_generation: None,
        participant_binding_mode: None,
        participant_server: None,
    });
    store.save_state(&blocked).unwrap();
    safety
        .integration_branch_active
        .store(true, Ordering::SeqCst);

    let accepted = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(accepted.status, RunStatus::Accepted);
    assert_eq!(accepted.integration_sha.as_deref(), Some(INTEGRATION_SHA));
    assert!(safety.in_progress_calls.load(Ordering::SeqCst) > 0);
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 1);
    assert!(
        store
            .successful_patch_recorded(RUN_ID, &pending.message_hash)
            .unwrap()
    );
    let retry_prompt = app
        .prompts()
        .into_iter()
        .rfind(|prompt| prompt.contains("REQUEST_PRIMARY_INTEGRATION"))
        .unwrap();
    assert!(retry_prompt.contains("Coordinator recovery override"));
    assert!(retry_prompt.contains("Do not call consensus_apply_patch"));
    assert!(
        retry_prompt
            .contains("Confirm repository instructions with a successful `git ls-files` query")
    );
}

#[tokio::test]
async fn v0213_symbolic_ref_confirmation_blocker_resumes_the_same_run() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let (coordinator, app, store, safety) =
        seed_invalid_integration_recovery_with_preloaded_primary(
            &path,
            PARTICIPANT_MCP_SERVER,
            true,
        )
        .await;
    let pending = store.pending_send(RUN_ID).unwrap().unwrap();
    let thread_id = pending.thread_id.clone().unwrap();
    let turn_id = pending.turn_id.clone().unwrap();
    let binding = store.active_primary_binding(RUN_ID).unwrap().unwrap();
    app.set_patch_plugin_id(&thread_id, &turn_id, Value::Null);
    insert_completed_integration_command_evidence(&app, &thread_id, &turn_id);
    app.insert_turn_item_before_agent(
        &thread_id,
        &turn_id,
        json!({
            "id": "current-branch",
            "type": "commandExecution",
            "command": "/bin/bash -lc 'git symbolic-ref --short HEAD'",
            "cwd": "/repo/primary",
            "status": "completed",
            "exitCode": 0,
            "source": "unifiedExecStartup",
        }),
    );
    replace_persisted_ephemeral_turn_evidence(&path, &store, &app, &thread_id, &turn_id);

    let mut blocked = store.load_run(RUN_ID).unwrap().unwrap();
    blocked.reason_code = Some("FORBIDDEN_OPERATION".into());
    blocked.last_error = Some(RunDiagnostic {
        code: "FORBIDDEN_OPERATION".into(),
        detail: "patch-success confirmation executed a non-read-only command: /bin/bash -lc 'git symbolic-ref --short HEAD'".into(),
        operation: None,
        action: NextAction::RequestPrimaryIntegration,
        role: Some(Role::Primary),
        thread_id: Some(thread_id.clone()),
        source_thread_id: Some(binding.source_primary_thread_id.clone()),
        effective_thread_id: Some(thread_id.clone()),
        participant_binding_generation: Some(binding.generation),
        participant_binding_mode: Some("EPHEMERAL_FORK".into()),
        participant_server: Some(PARTICIPANT_MCP_SERVER.into()),
    });
    store.save_state(&blocked).unwrap();
    safety
        .integration_branch_active
        .store(true, Ordering::SeqCst);

    let accepted = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(
        accepted.status,
        RunStatus::Accepted,
        "{:?}",
        accepted.last_error
    );
    assert_eq!(accepted.integration_sha.as_deref(), Some(INTEGRATION_SHA));
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 1);
    assert_eq!(
        store
            .active_primary_binding(RUN_ID)
            .unwrap()
            .unwrap()
            .generation,
        binding.generation
    );
    assert!(
        store
            .successful_patch_recorded(RUN_ID, &pending.message_hash)
            .unwrap()
    );
    let retry_prompt = app
        .prompts()
        .into_iter()
        .rfind(|prompt| prompt.contains("REQUEST_PRIMARY_INTEGRATION"))
        .unwrap();
    assert!(retry_prompt.contains("Coordinator recovery override"));
    assert!(retry_prompt.contains("Do not call consensus_apply_patch"));
}

#[tokio::test]
async fn completed_integration_recovery_reforks_an_unloaded_ephemeral_primary() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let (coordinator, app, store, safety) =
        seed_invalid_integration_recovery_with_preloaded_primary(
            &path,
            PARTICIPANT_MCP_SERVER,
            true,
        )
        .await;
    let pending = store.pending_send(RUN_ID).unwrap().unwrap();
    let thread_id = pending.thread_id.clone().unwrap();
    let turn_id = pending.turn_id.clone().unwrap();
    let original_binding = store.active_primary_binding(RUN_ID).unwrap().unwrap();
    assert_eq!(original_binding.mode, PrimaryBindingMode::EphemeralFork);
    app.set_patch_plugin_id(&thread_id, &turn_id, Value::Null);
    insert_completed_integration_command_evidence(&app, &thread_id, &turn_id);
    replace_persisted_ephemeral_turn_evidence(&path, &store, &app, &thread_id, &turn_id);

    let mut blocked = store.load_run(RUN_ID).unwrap().unwrap();
    blocked.reason_code = Some("FORBIDDEN_OPERATION".into());
    blocked.last_error = Some(RunDiagnostic {
        code: "FORBIDDEN_OPERATION".into(),
        detail: "integration command is not canonically completed with exit code zero".into(),
        operation: None,
        action: NextAction::RequestPrimaryIntegration,
        role: Some(Role::Primary),
        thread_id: Some(thread_id.clone()),
        source_thread_id: Some("primary".into()),
        effective_thread_id: Some(thread_id.clone()),
        participant_binding_generation: Some(original_binding.generation),
        participant_binding_mode: Some("EPHEMERAL_FORK".into()),
        participant_server: Some(PARTICIPANT_MCP_SERVER.into()),
    });
    store.save_state(&blocked).unwrap();
    safety
        .integration_branch_active
        .store(true, Ordering::SeqCst);
    app.set_primary_runtime_status(ThreadRuntimeStatus::NotLoaded);
    app.remove_thread(&thread_id);

    let accepted = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(
        accepted.status,
        RunStatus::Accepted,
        "{:?}",
        accepted.last_error
    );
    assert_eq!(accepted.integration_sha.as_deref(), Some(INTEGRATION_SHA));
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 1);
    let binding = store.active_primary_binding(RUN_ID).unwrap().unwrap();
    assert_eq!(binding.mode, PrimaryBindingMode::EphemeralFork);
    assert_eq!(binding.generation, original_binding.generation + 1);
    assert_ne!(binding.effective_primary_thread_id, thread_id);
    assert_eq!(app.forks().len(), 2);
    assert!(app.request_order().iter().any(|request| {
        request
            == &format!(
                "{}:REQUEST_PRIMARY_INTEGRATION",
                binding.effective_primary_thread_id
            )
    }));
}

#[tokio::test]
async fn v0212_unloaded_source_blocker_resumes_the_same_run() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let (coordinator, app, store, safety) =
        seed_invalid_integration_recovery_with_preloaded_primary(
            &path,
            PARTICIPANT_MCP_SERVER,
            true,
        )
        .await;
    let pending = store.pending_send(RUN_ID).unwrap().unwrap();
    let thread_id = pending.thread_id.clone().unwrap();
    let turn_id = pending.turn_id.clone().unwrap();
    let original_binding = store.active_primary_binding(RUN_ID).unwrap().unwrap();
    app.set_patch_plugin_id(&thread_id, &turn_id, Value::Null);
    insert_completed_integration_command_evidence(&app, &thread_id, &turn_id);
    replace_persisted_ephemeral_turn_evidence(&path, &store, &app, &thread_id, &turn_id);

    let mut blocked = store.load_run(RUN_ID).unwrap().unwrap();
    blocked.reason_code = Some("FORBIDDEN_OPERATION".into());
    blocked.last_error = Some(RunDiagnostic {
        code: "FORBIDDEN_OPERATION".into(),
        detail: "integration command is not canonically completed with exit code zero".into(),
        operation: None,
        action: NextAction::RequestPrimaryIntegration,
        role: Some(Role::Primary),
        thread_id: Some(thread_id.clone()),
        source_thread_id: Some("primary".into()),
        effective_thread_id: Some(thread_id.clone()),
        participant_binding_generation: Some(original_binding.generation),
        participant_binding_mode: Some("EPHEMERAL_FORK".into()),
        participant_server: Some(PARTICIPANT_MCP_SERVER.into()),
    });
    store.save_state(&blocked).unwrap();
    safety
        .integration_branch_active
        .store(true, Ordering::SeqCst);

    let prepared = coordinator.prepare_resume(RUN_ID).await.unwrap();
    assert_eq!(prepared.status, RunStatus::Running);
    let unsent = store.pending_send(RUN_ID).unwrap().unwrap();
    assert_eq!(unsent.thread_id, None);
    assert_eq!(unsent.turn_id, None);
    assert_eq!(unsent.turn_start_intent_at, None);
    assert_eq!(
        unsent.participant_binding_generation,
        Some(original_binding.generation)
    );

    let mut legacy_blocked = store.load_run(RUN_ID).unwrap().unwrap();
    legacy_blocked.record_error(RunDiagnostic {
        code: "HISTORY_UNAVAILABLE".into(),
        detail: "Source Primary before safe mirror recreation is not idle".into(),
        operation: None,
        action: NextAction::RequestPrimaryIntegration,
        role: Some(Role::Primary),
        thread_id: Some(thread_id.clone()),
        source_thread_id: Some("primary".into()),
        effective_thread_id: Some(thread_id.clone()),
        participant_binding_generation: Some(original_binding.generation),
        participant_binding_mode: Some("EPHEMERAL_FORK".into()),
        participant_server: Some(PARTICIPANT_MCP_SERVER.into()),
    });
    legacy_blocked.block("HISTORY_UNAVAILABLE");
    store.save_state(&legacy_blocked).unwrap();

    store
        .record_turn_start_intent(RUN_ID, &unsent.message_hash)
        .unwrap();
    let mut unsafe_resumed = legacy_blocked.clone();
    unsafe_resumed
        .retry_blocked_unsent_ephemeral_source_recreation()
        .unwrap();
    let unsafe_error = store
        .reactivate_blocked_run_with_unsent_ephemeral_recreation_retry(
            &legacy_blocked,
            &unsafe_resumed,
        )
        .unwrap_err();
    assert!(
        unsafe_error
            .to_string()
            .contains("pending request is not at the exact unsent ephemeral recovery boundary")
    );
    assert_eq!(store.load_run(RUN_ID).unwrap().unwrap(), legacy_blocked);
    Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE turns
             SET turn_start_intent_at = NULL, capability_generation = NULL
             WHERE run_id = ?1 AND message_hash = ?2",
            params![RUN_ID, &unsent.message_hash],
        )
        .unwrap();

    app.set_primary_runtime_status(ThreadRuntimeStatus::NotLoaded);
    app.remove_thread(&thread_id);

    let accepted = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(
        accepted.status,
        RunStatus::Accepted,
        "{:?}",
        accepted.last_error
    );
    assert_eq!(accepted.integration_sha.as_deref(), Some(INTEGRATION_SHA));
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 1);
    let binding = store.active_primary_binding(RUN_ID).unwrap().unwrap();
    assert_eq!(binding.generation, original_binding.generation + 1);
    assert_ne!(binding.effective_primary_thread_id, thread_id);
    assert!(app.resumes().iter().any(|thread| thread == "primary"));
    assert!(app.request_order().iter().any(|request| {
        request
            == &format!(
                "{}:REQUEST_PRIMARY_INTEGRATION",
                binding.effective_primary_thread_id
            )
    }));
}

fn insert_completed_integration_command_evidence(
    app: &FakeAppServer,
    thread_id: &str,
    turn_id: &str,
) {
    let command = |id: &str, value: &str, status: &str, exit_code: i64| {
        json!({
            "id": id,
            "type": "commandExecution",
            "command": value,
            "cwd": "/repo/primary",
            "status": status,
            "exitCode": exit_code,
            "source": "unifiedExecStartup",
        })
    };
    for item in [
        command(
            "instructions",
            "/bin/bash -lc 'rg --files -g AGENTS.md'",
            "failed",
            127,
        ),
        command(
            "target-absent",
            "/bin/bash -lc 'git show --no-patch --format=%H refs/heads/consensus/test-run'",
            "failed",
            128,
        ),
        command(
            "branch",
            &format!("/bin/bash -lc 'git switch -c consensus/test-run {PRIMARY_SHA}'"),
            "completed",
            0,
        ),
        command(
            "merge",
            &format!("/bin/bash -lc 'git merge --no-ff --no-edit {REVIEWER_SHA}'"),
            "completed",
            0,
        ),
        command(
            "new-file-diff",
            "/bin/bash -lc 'git diff --no-index -- /dev/null combined.txt'",
            "failed",
            1,
        ),
        command("stage", "/bin/bash -lc 'git add -A'", "completed", 0),
        command(
            "commit",
            "/bin/bash -lc 'git commit -m compatibility_fixes'",
            "completed",
            0,
        ),
    ] {
        app.insert_turn_item_before_agent(thread_id, turn_id, item);
    }
}

fn replace_persisted_ephemeral_turn_evidence(
    path: &Path,
    store: &SqliteRunStore,
    app: &FakeAppServer,
    thread_id: &str,
    turn_id: &str,
) {
    let turn = app
        .detail(thread_id)
        .turns
        .into_iter()
        .find(|turn| turn.get("id").and_then(Value::as_str) == Some(turn_id))
        .unwrap();
    let connection = Connection::open(path).unwrap();
    let turn_record_id = connection
        .query_row(
            "SELECT id FROM turns
             WHERE run_id = ?1 AND thread_id = ?2 AND turn_id = ?3",
            params![RUN_ID, thread_id, turn_id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    connection
        .execute(
            "DELETE FROM turn_event_items WHERE turn_record_id = ?1",
            [turn_record_id],
        )
        .unwrap();
    connection
        .execute(
            "DELETE FROM turn_event_completions WHERE turn_record_id = ?1",
            [turn_record_id],
        )
        .unwrap();
    drop(connection);
    for item in turn["items"].as_array().unwrap() {
        store
            .record_turn_item_event(RUN_ID, thread_id, turn_id, "item/completed", item)
            .unwrap();
    }
    store
        .record_turn_completed_event(RUN_ID, thread_id, turn_id, &turn)
        .unwrap();
}

#[tokio::test]
async fn participant_capability_generation_rejects_legacy_invalid_integration_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let (coordinator, app, store, _safety) =
        seed_invalid_integration_recovery(&path, "worktreeMergeConsensus").await;

    let error = coordinator.resume(RUN_ID).await.unwrap_err();

    assert_eq!(error.code(), "MODEL_RESPONSE_RETRY_UNSAFE");
    assert!(error.detail().contains("outside"));
    assert_eq!(app.request_count(), 5);
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 0);
}

#[tokio::test]
async fn malformed_capability_generation_fails_closed_for_invalid_integration_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let (coordinator, app, store, _safety) =
        seed_invalid_integration_recovery(&path, "worktreeMergeConsensus").await;
    set_turn_capability_generation(&path, Some("unknown-generation"));

    let error = coordinator.resume(RUN_ID).await.unwrap_err();

    assert_eq!(error.code(), "MODEL_RESPONSE_RETRY_UNSAFE");
    assert!(error.detail().contains("capability generation"));
    assert_eq!(app.request_count(), 5);
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 0);
}

async fn seed_invalid_integration_recovery(
    path: &Path,
    patch_server: &str,
) -> (
    Coordinator<FakeAppServer, RecordingSafety>,
    Arc<FakeAppServer>,
    SqliteRunStore,
    Arc<RecordingSafety>,
) {
    seed_invalid_integration_recovery_with_preloaded_primary(path, patch_server, false).await
}

async fn seed_invalid_integration_recovery_with_preloaded_primary(
    path: &Path,
    patch_server: &str,
    preloaded_primary: bool,
) -> (
    Coordinator<FakeAppServer, RecordingSafety>,
    Arc<FakeAppServer>,
    SqliteRunStore,
    Arc<RecordingSafety>,
) {
    let store = SqliteRunStore::open(path).unwrap();
    let mut replies = conflict_free_replies();
    replies[4] = message(
        "INTEGRATION_READY",
        "INTEGRATE",
        1,
        Some(1),
        None,
        None,
        json!({
            "request_hash": "filled-from-pending",
            "approved_plan_revision": 1,
            "approved_primary_sha": PRIMARY_SHA,
            "approved_reviewer_sha": REVIEWER_SHA,
            "resulting_integration_branch": "consensus/test-run",
            "resulting_integration_sha": INTEGRATION_SHA,
            "changed_files": [{"status": "M", "path": "combined.txt"}],
            "uncovered_items": []
        }),
    );
    replies.insert(
        5,
        json!(
            "<consensus-result>INTEGRATION_READY</consensus-result>\n\nThe existing clean integration commit is ready."
        ),
    );
    replies[6] = json!("<consensus-result>APPROVED</consensus-result>");
    let app = FakeAppServer::new(replies).with_marker_protocol();
    let app = if preloaded_primary {
        app.without_primary_participant()
    } else {
        app
    };
    let app = Arc::new(app);
    let safety = Arc::new(RecordingSafety::default());
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::clone(&safety),
        fast_options(),
    );
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();

    let blocked = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(blocked.status, RunStatus::Blocked);
    assert_eq!(blocked.reason_code.as_deref(), Some("INVALID_RESPONSE"));
    let pending = store.pending_send(RUN_ID).unwrap().unwrap();
    let thread_id = pending.thread_id.clone().unwrap();
    let turn_id = pending.turn_id.clone().unwrap();
    let failed_patch = "*** Begin Patch\n*** End Patch";
    let successful_patch = "diff --git a/combined.txt b/combined.txt\n--- a/combined.txt\n+++ b/combined.txt\n@@ -1 +1 @@\n-old\n+new\n";
    for (suffix, status, patch) in [
        ("failed", "failed", failed_patch),
        ("completed", "completed", successful_patch),
    ] {
        app.insert_turn_item_before_agent(
            &thread_id,
            &turn_id,
            json!({
                "id": format!("patch-{suffix}"),
                "type": "mcpToolCall",
                "pluginId": "worktree-merge-consensus@worktree-merge-consensus",
                "server": patch_server,
                "tool": "consensus_apply_patch",
                "arguments": {
                    "run_id": RUN_ID,
                    "request_hash": pending.message_hash.clone(),
                    "patch": patch,
                },
                "status": status,
                "appContext": null,
            }),
        );
    }
    let successful_patch_hash = canonical_json_hash(&json!({"patch": successful_patch}));
    if preloaded_primary {
        let binding = store.active_primary_binding(RUN_ID).unwrap().unwrap();
        store
            .record_successful_patch_with_provenance(
                RUN_ID,
                &pending.message_hash,
                &successful_patch_hash,
                Some(&binding.source_primary_thread_id),
                Some(&binding.effective_primary_thread_id),
                Some(binding.generation),
            )
            .unwrap();
    } else {
        store
            .record_successful_patch(RUN_ID, &pending.message_hash, &successful_patch_hash)
            .unwrap();
    }

    (coordinator, app, store, safety)
}

fn set_turn_capability_generation(path: &Path, generation: Option<&str>) {
    Connection::open(path)
        .unwrap()
        .execute(
            "UPDATE turns
             SET capability_generation = ?1,
                 participant_binding_generation =
                    CASE WHEN ?1 IS NULL THEN NULL
                         ELSE participant_binding_generation END
             WHERE run_id = ?2 AND delivery_state IN ('PENDING', 'SENT')",
            params![generation, RUN_ID],
        )
        .unwrap();
}

#[tokio::test]
async fn side_effect_free_execution_tool_blocker_retries_the_same_integration_action() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let mut replies = conflict_free_replies();
    replies.insert(4, execution_tool_unavailable_blocker());
    let app = Arc::new(FakeAppServer::new(replies));
    let safety = Arc::new(RecordingSafety::default());
    let coordinator = Coordinator::new(Arc::clone(&app), store.clone(), safety, fast_options());
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();

    let blocked = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(blocked.status, RunStatus::Blocked);
    assert_eq!(
        blocked.reason_code.as_deref(),
        Some("EXECUTION_TOOL_UNAVAILABLE")
    );
    assert_eq!(blocked.integration_branch, None);
    assert_eq!(blocked.integration_sha, None);
    assert_eq!(app.request_count(), 5);

    let accepted = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(accepted.status, RunStatus::Accepted);
    assert_eq!(app.request_count(), 8);
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 1);
    assert_eq!(
        app.request_order()
            .iter()
            .filter(|request| request.ends_with("REQUEST_PRIMARY_INTEGRATION"))
            .count(),
        2
    );
}

#[tokio::test]
async fn recorded_primary_turns_require_their_exact_binding_generation() {
    for mutation in [
        "valid",
        "older-generation",
        "forged-thread",
        "forged-generation",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let store_path = temp.path().join("state.db");
        let store = SqliteRunStore::open(&store_path).unwrap();
        let mut replies = conflict_free_replies();
        replies.insert(4, execution_tool_unavailable_blocker());
        let app = Arc::new(FakeAppServer::new(replies).without_primary_participant());
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
        let blocked = coordinator.drive(RUN_ID).await.unwrap();
        assert_eq!(
            blocked.reason_code.as_deref(),
            Some("EXECUTION_TOOL_UNAVAILABLE")
        );
        let accepted = store.latest_accepted_turn(RUN_ID).unwrap().unwrap();
        assert_eq!(
            accepted.thread_id, "primary-consensus-mirror-1",
            "mutation={mutation}"
        );
        assert_eq!(accepted.participant_binding_generation, Some(1));
        match mutation {
            "valid" => {}
            "older-generation" => {
                let source_history_hash = store
                    .active_primary_binding(RUN_ID)
                    .unwrap()
                    .unwrap()
                    .source_history_hash
                    .unwrap();
                let newer = store
                    .activate_primary_binding(
                        RUN_ID,
                        "primary",
                        "primary-consensus-mirror-2",
                        PrimaryBindingMode::EphemeralFork,
                        PARTICIPANT_MCP_SERVER,
                        Some(&source_history_hash),
                    )
                    .unwrap();
                assert_eq!(newer.generation, 2);
            }
            "forged-thread" => {
                Connection::open(&store_path)
                    .unwrap()
                    .execute(
                        "UPDATE turns SET thread_id = 'forged-mirror'
                         WHERE run_id = ?1 AND delivery_state = 'ACCEPTED'",
                        [RUN_ID],
                    )
                    .unwrap();
            }
            "forged-generation" => {
                Connection::open(&store_path)
                    .unwrap()
                    .execute(
                        "UPDATE turns SET participant_binding_generation = 999
                         WHERE run_id = ?1 AND delivery_state = 'ACCEPTED'",
                        [RUN_ID],
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }

        if mutation == "valid" {
            let accepted = coordinator.resume(RUN_ID).await.unwrap();
            assert_eq!(accepted.status, RunStatus::Accepted);
            assert!(
                app.method_order()
                    .iter()
                    .all(|method| method != "thread/read-full:primary-consensus-mirror-1")
            );
            assert!(
                app.resumes()
                    .iter()
                    .all(|thread_id| !thread_id.contains("-consensus-mirror-"))
            );
        } else if mutation == "older-generation" {
            let resumed = coordinator.prepare_resume(RUN_ID).await.unwrap();
            assert_eq!(resumed.status, RunStatus::Running);
            assert_eq!(
                store
                    .active_primary_binding(RUN_ID)
                    .unwrap()
                    .unwrap()
                    .generation,
                2
            );
        } else {
            let error = coordinator.prepare_resume(RUN_ID).await.unwrap_err();
            assert_eq!(error.code(), "HISTORY_UNAVAILABLE", "mutation={mutation}");
        }
    }
}

#[tokio::test]
async fn execution_tool_blocker_with_command_evidence_is_not_retryable() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let mut replies = conflict_free_replies();
    replies.insert(4, execution_tool_unavailable_blocker());
    let app = Arc::new(FakeAppServer::new(replies));
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
    let blocked = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(blocked.status, RunStatus::Blocked);
    app.append_turn_item(
        "primary",
        "turn-5",
        json!({
            "id": "command-turn-5",
            "type": "commandExecution",
            "command": "git status --short",
            "cwd": "/repo/primary",
            "status": "completed",
            "exitCode": 0
        }),
    );

    let error = coordinator.resume(RUN_ID).await.unwrap_err();

    assert_eq!(error.code(), "MODEL_RESPONSE_RETRY_UNSAFE");
    assert_eq!(app.request_count(), 5);
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 0);
}

#[tokio::test]
async fn failed_required_test_routes_machine_feedback_to_a_corrective_integration_round() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let mut replies = conflict_free_replies();
    replies.insert(
        5,
        message(
            "INTEGRATION_READY",
            "INTEGRATE",
            2,
            Some(1),
            Some("consensus/test-run"),
            Some(CORRECTED_INTEGRATION_SHA),
            json!({
                "changed_files": ["combined.txt"],
                "integration_evidence": {"summary": "verification failures corrected"}
            }),
        ),
    );
    replies[6]["round"] = json!(2);
    replies[6]["integration_sha"] = json!(CORRECTED_INTEGRATION_SHA);
    replies[6]["payload"]["approved_integration_sha"] = json!(CORRECTED_INTEGRATION_SHA);
    let app = Arc::new(
        FakeAppServer::new(replies)
            .with_verification_behavior(VerificationBehavior::FailedExecutionThenPass),
    );
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

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(result.round, 2);
    assert_eq!(result.accepted_result.unwrap().tests[0].exit_code, 0);
    assert_eq!(
        app.request_order()
            .iter()
            .filter(|action| action.ends_with("REQUEST_PRIMARY_INTEGRATION"))
            .count(),
        2
    );
    assert_eq!(
        app.request_order()
            .iter()
            .filter(|action| action.ends_with("REQUEST_PRIMARY_VERIFICATION"))
            .count(),
        2
    );
    let corrective_prompt = app
        .prompts()
        .into_iter()
        .filter(|prompt| prompt_action(prompt) == "REQUEST_PRIMARY_INTEGRATION")
        .nth(1)
        .unwrap();
    assert!(corrective_prompt.contains("a machine-derived compiler diagnostic"));
}

#[tokio::test]
async fn corrective_patch_tool_blocker_reactivates_the_exact_same_run_and_correction_round() {
    let safety = Arc::new(CorrectiveRecoverySafety::default());
    let (_temp, coordinator, app, store, blocked) =
        seed_corrective_patch_tool_blocker(Arc::clone(&safety)).await;
    let accepted = store.latest_accepted_turn(RUN_ID).unwrap().unwrap();
    let absent_before = safety.branch_absent_calls();
    let integration_payload = blocked.current_integration_payload.clone();
    let failed_evidence = blocked.test_evidence.clone();
    let failure_feedback = blocked.last_result_feedback.clone();
    let facts = blocked.facts.clone();

    let resumed = coordinator.prepare_resume(RUN_ID).await.unwrap();

    assert_eq!(resumed.facts.run_id.to_string(), RUN_ID);
    assert_eq!(resumed.status, RunStatus::Running);
    assert_eq!(resumed.phase, Phase::Integrate);
    assert_eq!(resumed.next_action, NextAction::RequestPrimaryIntegration);
    assert_eq!(resumed.round, blocked.round);
    assert_eq!(resumed.integration_branch, blocked.integration_branch);
    assert_eq!(resumed.integration_sha, blocked.integration_sha);
    assert_eq!(resumed.current_integration_payload, integration_payload);
    assert_eq!(resumed.test_evidence, failed_evidence);
    assert_eq!(resumed.last_result_feedback, failure_feedback);
    assert_eq!(resumed.facts, facts);
    assert!(resumed.reason_code.is_none());
    assert!(resumed.accepted_result.is_none());
    assert_eq!(safety.branch_absent_calls(), absent_before);
    assert!(
        safety
            .verified_results()
            .contains(&format!("consensus/test-run:{INTEGRATION_SHA}"))
    );
    let pending = store.pending_send(RUN_ID).unwrap().unwrap();
    assert_eq!(pending.message_hash, accepted.message_hash);
    assert!(pending.thread_id.is_none());
    assert!(pending.turn_id.is_none());
    assert_eq!(
        store
            .archived_turn_ids(RUN_ID, &accepted.message_hash)
            .unwrap(),
        vec![accepted.turn_id.clone()]
    );

    let repeat = coordinator.prepare_resume(RUN_ID).await.unwrap_err();
    assert_eq!(repeat.code(), "NOT_PAUSED");
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 1);

    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(result.round, blocked.round);
    assert_eq!(
        result.integration_sha.as_deref(),
        Some(CORRECTED_INTEGRATION_SHA)
    );
    assert_eq!(
        app.request_order()
            .iter()
            .filter(|action| action.ends_with("REQUEST_PRIMARY_INTEGRATION"))
            .count(),
        3
    );
    let retried_prompt = app
        .prompts()
        .into_iter()
        .filter(|prompt| prompt_action(prompt) == "REQUEST_PRIMARY_INTEGRATION")
        .nth(2)
        .unwrap();
    assert!(retried_prompt.contains("a machine-derived compiler diagnostic"));
    assert!(retried_prompt.contains(INTEGRATION_SHA));
}

#[tokio::test]
async fn corrective_patch_tool_retry_allows_exactly_one_request_bound_patch() {
    let safety = Arc::new(CorrectiveRecoverySafety::default());
    let (_temp, coordinator, _app, store, blocked) =
        seed_corrective_patch_tool_blocker(Arc::clone(&safety)).await;
    coordinator.prepare_resume(RUN_ID).await.unwrap();
    let pending = store.pending_send(RUN_ID).unwrap().unwrap();
    store
        .record_turn_started(
            RUN_ID,
            &pending.message_hash,
            &blocked.facts.primary_thread_id,
            "retried-correction-turn",
        )
        .unwrap();

    let wrong = coordinator
        .apply_patch(RUN_ID, "wrong-request", "diff --git a/a b/a")
        .await
        .unwrap_err();
    assert_eq!(wrong.code(), "PATCH_NOT_AUTHORIZED");
    let applied = coordinator
        .apply_patch(
            RUN_ID,
            &pending.message_hash,
            "diff --git a/combined.txt b/combined.txt",
        )
        .await
        .unwrap();
    assert_eq!(applied.integration_branch, "consensus/test-run");
    assert_eq!(applied.base_sha, INTEGRATION_SHA);
    assert_eq!(applied.changed_files, vec![PathBuf::from("combined.txt")]);
    assert_eq!(safety.patch_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        safety.corrective_patch_bases.lock().unwrap().as_slice(),
        [INTEGRATION_SHA]
    );

    let duplicate = coordinator
        .apply_patch(
            RUN_ID,
            &pending.message_hash,
            "diff --git a/src/lib.rs b/src/lib.rs",
        )
        .await
        .unwrap_err();
    assert_eq!(duplicate.code(), "PATCH_ALREADY_APPLIED");
    assert_eq!(safety.patch_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn corrective_patch_tool_retry_rejects_every_side_effect_capable_item() {
    for (label, item) in [
        (
            "command",
            json!({"id": "extra-command", "type": "commandExecution"}),
        ),
        (
            "file-change",
            json!({"id": "extra-file-change", "type": "fileChange"}),
        ),
        ("mcp", json!({"id": "extra-mcp", "type": "mcpToolCall"})),
        (
            "dynamic-tool",
            json!({"id": "extra-dynamic-tool", "type": "dynamicToolCall"}),
        ),
        (
            "unknown",
            json!({"id": "extra-unknown", "type": "futureSideEffect"}),
        ),
    ] {
        let safety = Arc::new(CorrectiveRecoverySafety::default());
        let (_temp, coordinator, app, store, blocked) =
            seed_corrective_patch_tool_blocker(safety).await;
        let accepted = store.latest_accepted_turn(RUN_ID).unwrap().unwrap();
        app.insert_turn_item_before_agent("primary", &accepted.turn_id, item);

        let error = coordinator.prepare_resume(RUN_ID).await.unwrap_err();

        assert_eq!(
            error.code(),
            "MODEL_RESPONSE_RETRY_UNSAFE",
            "fixture={label}"
        );
        assert_eq!(store.load_run(RUN_ID).unwrap().unwrap(), blocked);
        assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 0);
        assert!(store.pending_send(RUN_ID).unwrap().is_none());
    }
}

#[tokio::test]
async fn corrective_patch_tool_retry_validates_request_marker_and_response_hash() {
    for (label, mutate_history, expected_code) in [
        ("request-marker", true, "HISTORY_UNAVAILABLE"),
        ("response-hash", false, "HISTORY_UNAVAILABLE"),
    ] {
        let safety = Arc::new(CorrectiveRecoverySafety::default());
        let (_temp, coordinator, app, store, blocked) =
            seed_corrective_patch_tool_blocker(safety).await;
        let accepted = store.latest_accepted_turn(RUN_ID).unwrap().unwrap();
        if mutate_history {
            app.set_user_prompt(
                "primary",
                &accepted.turn_id,
                "correction request without its deterministic marker",
            );
        } else {
            app.set_agent_text(
                "primary",
                &accepted.turn_id,
                "<consensus-result>BLOCKED:CONTROLLED_PATCH_TOOL_UNAVAILABLE</consensus-result>\n\nDifferent blocker evidence.",
            );
        }

        let error = coordinator.prepare_resume(RUN_ID).await.unwrap_err();

        assert_eq!(error.code(), expected_code, "fixture={label}");
        assert_eq!(store.load_run(RUN_ID).unwrap().unwrap(), blocked);
        assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 0);
    }
}

#[tokio::test]
async fn corrective_patch_tool_retry_rejects_patch_residue_and_missing_failed_verification() {
    for missing_failed_verification in [false, true] {
        let safety = Arc::new(CorrectiveRecoverySafety::default());
        let (_temp, coordinator, _app, store, mut blocked) =
            seed_corrective_patch_tool_blocker(safety).await;
        let accepted = store.latest_accepted_turn(RUN_ID).unwrap().unwrap();
        if missing_failed_verification {
            blocked.test_evidence.clear();
            store.save_state(&blocked).unwrap();
        } else {
            store
                .record_successful_patch(RUN_ID, &accepted.message_hash, "patch-hash")
                .unwrap();
        }

        let error = coordinator.prepare_resume(RUN_ID).await.unwrap_err();

        assert_eq!(error.code(), "MODEL_RESPONSE_RETRY_UNSAFE");
        assert_eq!(store.load_run(RUN_ID).unwrap().unwrap(), blocked);
        assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 0);
    }
}

#[tokio::test]
async fn corrective_patch_tool_retry_revalidates_target_sources_and_ancestry() {
    for code in [
        "DIRTY_WORKTREE",
        "STALE_INTEGRATION_SHA",
        "SOURCE_DRIFT",
        "MISSING_SOURCE_ANCESTRY",
    ] {
        let safety = Arc::new(CorrectiveRecoverySafety::default());
        let (_temp, coordinator, app, store, blocked) =
            seed_corrective_patch_tool_blocker(Arc::clone(&safety)).await;
        let approval_checks_before = app.approval_mode_request_count();
        safety.fail_result_verification(code);

        let error = coordinator.prepare_resume(RUN_ID).await.unwrap_err();

        assert_eq!(error.code(), code);
        assert_eq!(
            app.approval_mode_request_count(),
            approval_checks_before + 1
        );
        assert_eq!(store.load_run(RUN_ID).unwrap().unwrap(), blocked);
        assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 0);
        assert!(store.pending_send(RUN_ID).unwrap().is_none());
    }
}

#[tokio::test]
async fn corrective_patch_tool_retry_rejects_a_conflicting_repository_lock_atomically() {
    let safety = Arc::new(CorrectiveRecoverySafety::default());
    let (_temp, coordinator, _app, store, blocked) =
        seed_corrective_patch_tool_blocker(safety).await;
    let mut competing = fixture_run();
    competing.facts.run_id = Uuid::parse_str("9f8a5c17-0f06-4df9-873f-589f3b54dbcc").unwrap();
    store.insert_run(&competing).unwrap();

    let error = coordinator.prepare_resume(RUN_ID).await.unwrap_err();

    assert_eq!(error.code(), "ACTIVE_RUN_EXISTS");
    assert_eq!(store.load_run(RUN_ID).unwrap().unwrap(), blocked);
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 0);
    assert!(store.pending_send(RUN_ID).unwrap().is_none());
}

#[tokio::test]
async fn live_item_events_keep_marker_turns_free_of_participant_commands() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let mut replies = conflict_free_replies();
    replies.insert(
        5,
        message(
            "INTEGRATION_READY",
            "INTEGRATE",
            2,
            Some(1),
            Some("consensus/test-run"),
            Some(CORRECTED_INTEGRATION_SHA),
            json!({
                "changed_files": ["combined.txt"],
                "integration_evidence": {"summary": "event-only verification corrected"}
            }),
        ),
    );
    replies[6]["round"] = json!(2);
    replies[6]["integration_sha"] = json!(CORRECTED_INTEGRATION_SHA);
    replies[6]["payload"]["approved_integration_sha"] = json!(CORRECTED_INTEGRATION_SHA);
    let app = Arc::new(
        FakeAppServer::new(replies)
            .with_verification_behavior(VerificationBehavior::FailedExecutionThenPass)
            .with_event_only_turn_items(),
    );
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
    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(result.round, 2);
    let first_verification_request = app
        .request_order()
        .iter()
        .position(|action| action.ends_with("REQUEST_PRIMARY_VERIFICATION"))
        .unwrap()
        + 1;
    let evidence = store
        .turn_event_evidence(
            RUN_ID,
            "primary",
            &format!("turn-{first_verification_request}"),
        )
        .unwrap()
        .unwrap();
    assert!(
        evidence
            .completed_items
            .iter()
            .all(|item| { item.get("type").and_then(Value::as_str) != Some("commandExecution") })
    );
    assert_eq!(app.executed_commands().len(), 2);
    let corrective_prompt = app
        .prompts()
        .into_iter()
        .filter(|prompt| prompt_action(prompt) == "REQUEST_PRIMARY_INTEGRATION")
        .nth(1)
        .unwrap();
    assert!(corrective_prompt.contains("a machine-derived compiler diagnostic"));
}

#[tokio::test]
async fn empty_self_report_is_ignored_when_authoritative_tests_pass() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let coordinator = Coordinator::new(
        Arc::new(
            FakeAppServer::new(conflict_free_replies())
                .with_verification_behavior(VerificationBehavior::EmptyReport),
        ),
        store,
        Arc::new(RecordingSafety::default()),
        fast_options(),
    );
    let request = StartRequest {
        integration_branch: Some("consensus/test-run".into()),
        test_commands: Vec::new(),
    };
    coordinator.start(fixture_run(), request).await.unwrap();

    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(result.accepted_result.unwrap().tests.len(), 1);
}

#[tokio::test]
async fn legacy_self_report_shape_is_ignored_when_authoritative_tests_pass() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let coordinator = Coordinator::new(
        Arc::new(
            FakeAppServer::new(conflict_free_replies())
                .with_verification_behavior(VerificationBehavior::LegacyReport),
        ),
        store,
        Arc::new(RecordingSafety::default()),
        fast_options(),
    );
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();

    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(result.accepted_result.unwrap().tests.len(), 1);
}

#[tokio::test]
async fn coordinator_evidence_does_not_require_participant_command_items() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(
        FakeAppServer::new(conflict_free_replies())
            .with_verification_behavior(VerificationBehavior::MissingExecution),
    );
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

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(result.test_evidence.len(), 1);
    assert_eq!(
        app.executed_commands(),
        vec![vec!["cargo", "test", "--workspace"]]
    );
}

#[tokio::test]
async fn exact_legacy_history_migrates_only_the_pending_verification_turn() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(
        FakeAppServer::deferred(marker_replies(), 6, DeferMode::Hold).with_marker_protocol(),
    );
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::new(RecordingSafety::default()),
        legacy_migration_options(),
    );
    let seed = seed_legacy_unattended_verification_history(
        &coordinator,
        &app,
        &store,
        ["ready", "cargo-unavailable", "ready"],
        None,
    )
    .await;

    let resumed = coordinator.prepare_resume(RUN_ID).await.unwrap();

    assert_eq!(resumed.facts.run_id, seed.blocked.facts.run_id);
    assert_eq!(resumed.integration_branch, seed.blocked.integration_branch);
    assert_eq!(resumed.integration_sha, seed.blocked.integration_sha);
    assert_eq!(resumed.status, RunStatus::Running);
    assert_eq!(resumed.phase, Phase::Verify);
    assert_eq!(resumed.next_action, NextAction::RequestPrimaryVerification);
    assert!(
        store
            .successful_patch_recorded(RUN_ID, &seed.integration_request_hash)
            .unwrap()
    );
    assert_eq!(
        store
            .archived_turn_ids(RUN_ID, &seed.verification_request_hash)
            .unwrap(),
        vec![
            "turn-6",
            "legacy-verification-2",
            "legacy-verification-3",
            "legacy-verification-4",
        ]
    );
    let pending = store.pending_send(RUN_ID).unwrap().unwrap();
    assert!(pending.thread_id.is_none());
    assert!(pending.turn_id.is_none());
    assert_eq!(app.request_count(), 6);
    assert!(app.executed_commands().is_empty());
}

#[tokio::test]
async fn startup_recovery_accepts_only_the_exact_v025_completion_collision() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let store = SqliteRunStore::open(&path).unwrap();
    let app = Arc::new(
        FakeAppServer::deferred(marker_replies(), 6, DeferMode::Hold).with_marker_protocol(),
    );
    let safety = Arc::new(RecordingSafety::default());
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::clone(&safety),
        legacy_migration_options(),
    );
    let seed = seed_legacy_unattended_verification_history(
        &coordinator,
        &app,
        &store,
        ["ready", "cargo-unavailable", "ready"],
        None,
    )
    .await;
    let resumed = coordinator.prepare_resume(RUN_ID).await.unwrap();
    let current_turn_id = "post-migration-verification";
    store
        .record_turn_started(
            RUN_ID,
            &seed.verification_request_hash,
            "primary",
            current_turn_id,
        )
        .unwrap();
    app.inject_completed_turn(
        "primary",
        current_turn_id,
        &legacy_request_prompt(&seed.verification_request_hash),
        legacy_verification_reply("ready"),
    );
    store
        .record_turn_item_event(
            RUN_ID,
            "primary",
            current_turn_id,
            "item/completed",
            &legacy_agent_item(current_turn_id, "ready"),
        )
        .unwrap();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "INSERT INTO turn_event_completions (
                turn_record_id, run_id, thread_id, turn_id,
                completed_turn_json, recorded_at
             )
             SELECT id, run_id, 'primary', 'legacy-verification-4', ?3, 1
             FROM turns WHERE run_id = ?1 AND message_hash = ?2",
            params![
                RUN_ID,
                seed.verification_request_hash,
                r#"{"id":"legacy-verification-4","status":"completed","items":[]}"#
            ],
        )
        .unwrap();
    drop(connection);
    let mut collision = resumed;
    record_database_completion_collision_diagnostic(&mut collision, "primary");
    collision.block("DATABASE_ERROR");
    store.save_state(&collision).unwrap();
    let integration_checks_before = safety
        .events()
        .into_iter()
        .filter(|event| event == &format!("result:consensus/test-run:{INTEGRATION_SHA}"))
        .count();

    let recovered = coordinator.recover_startup_runs().await.unwrap();

    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].status, RunStatus::Running);
    assert_eq!(recovered[0].phase, Phase::Verify);
    assert_eq!(
        recovered[0].next_action,
        NextAction::RequestPrimaryVerification
    );
    assert_eq!(recovered[0].integration_sha, collision.integration_sha);
    assert_eq!(store.load_run(RUN_ID).unwrap().unwrap(), recovered[0]);
    assert_eq!(
        store
            .pending_send(RUN_ID)
            .unwrap()
            .unwrap()
            .turn_id
            .as_deref(),
        Some(current_turn_id)
    );
    let integration_checks_after = safety
        .events()
        .into_iter()
        .filter(|event| event == &format!("result:consensus/test-run:{INTEGRATION_SHA}"))
        .count();
    assert_eq!(integration_checks_after, integration_checks_before + 2);
    assert!(app.executed_commands().is_empty());
}

#[tokio::test]
async fn unattended_migration_rejects_a_final_turn_with_side_effects() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(
        FakeAppServer::deferred(marker_replies(), 6, DeferMode::Hold).with_marker_protocol(),
    );
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::new(RecordingSafety::default()),
        legacy_migration_options(),
    );
    let seed = seed_legacy_unattended_verification_history(
        &coordinator,
        &app,
        &store,
        ["ready", "cargo-unavailable", "ready"],
        Some(json!({
            "id": "legacy-participant-command",
            "type": "commandExecution",
            "command": "cargo test --workspace",
            "cwd": "/state/verification/run",
            "status": "completed",
            "exitCode": 0
        })),
    )
    .await;

    let error = coordinator.prepare_resume(RUN_ID).await.unwrap_err();

    assert_eq!(error.code(), "MODEL_RESPONSE_RETRY_UNSAFE");
    assert!(error.detail().contains("forbidden after any test command"));
    assert_eq!(store.load_run(RUN_ID).unwrap().unwrap(), seed.blocked);
    assert_eq!(
        store
            .archived_turn_ids(RUN_ID, &seed.verification_request_hash)
            .unwrap()
            .len(),
        3
    );
    assert_eq!(app.request_count(), 6);
    assert!(app.executed_commands().is_empty());
}

#[tokio::test]
async fn unattended_migration_rejects_a_different_archived_signal_sequence() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(
        FakeAppServer::deferred(marker_replies(), 6, DeferMode::Hold).with_marker_protocol(),
    );
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::new(RecordingSafety::default()),
        legacy_migration_options(),
    );
    let seed = seed_legacy_unattended_verification_history(
        &coordinator,
        &app,
        &store,
        ["ready", "ready", "ready"],
        None,
    )
    .await;

    let error = coordinator.prepare_resume(RUN_ID).await.unwrap_err();

    assert_eq!(error.code(), "MODEL_RESPONSE_RETRY_UNSAFE");
    assert!(error.detail().contains("limited to one retry"));
    assert_eq!(store.load_run(RUN_ID).unwrap().unwrap(), seed.blocked);
    assert_eq!(
        store
            .archived_turn_ids(RUN_ID, &seed.verification_request_hash)
            .unwrap()
            .len(),
        3
    );
    assert_eq!(app.request_count(), 6);
    assert!(app.executed_commands().is_empty());
}

#[tokio::test]
async fn unattended_migration_rejects_a_nonready_final_marker() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(
        FakeAppServer::deferred(marker_replies(), 6, DeferMode::Hold).with_marker_protocol(),
    );
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::new(RecordingSafety::default()),
        legacy_migration_options(),
    );
    let seed = seed_legacy_unattended_verification_history(
        &coordinator,
        &app,
        &store,
        ["ready", "cargo-unavailable", "ready"],
        None,
    )
    .await;
    app.set_agent_text(
        "primary",
        "legacy-verification-4",
        "<consensus-result>BLOCKED:CARGO_UNAVAILABLE</consensus-result>\n\nNot ready.",
    );

    let error = coordinator.prepare_resume(RUN_ID).await.unwrap_err();

    assert_eq!(error.code(), "MODEL_RESPONSE_RETRY_UNSAFE");
    assert!(error.detail().contains("final VERIFICATION_READY marker"));
    assert_eq!(store.load_run(RUN_ID).unwrap().unwrap(), seed.blocked);
}

#[tokio::test]
async fn unattended_migration_requires_every_archived_request_marker() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(
        FakeAppServer::deferred(marker_replies(), 6, DeferMode::Hold).with_marker_protocol(),
    );
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::new(RecordingSafety::default()),
        legacy_migration_options(),
    );
    let seed = seed_legacy_unattended_verification_history(
        &coordinator,
        &app,
        &store,
        ["ready", "cargo-unavailable", "ready"],
        None,
    )
    .await;
    app.set_user_prompt(
        "primary",
        "legacy-verification-2",
        "Legacy verification request with the deterministic marker removed.",
    );

    let error = coordinator.prepare_resume(RUN_ID).await.unwrap_err();

    assert_eq!(error.code(), "HISTORY_UNAVAILABLE");
    assert!(
        error
            .detail()
            .contains("lacks its deterministic request marker")
    );
    assert_eq!(store.load_run(RUN_ID).unwrap().unwrap(), seed.blocked);
}

#[tokio::test]
async fn unattended_migration_binds_the_evidence_archive_to_the_third_turn() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let store = SqliteRunStore::open(&path).unwrap();
    let app = Arc::new(
        FakeAppServer::deferred(marker_replies(), 6, DeferMode::Hold).with_marker_protocol(),
    );
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::new(RecordingSafety::default()),
        legacy_migration_options(),
    );
    let seed = seed_legacy_unattended_verification_history(
        &coordinator,
        &app,
        &store,
        ["ready", "cargo-unavailable", "ready"],
        None,
    )
    .await;
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE turn_attempts
             SET terminal_status = CASE turn_id
                WHEN 'turn-6' THEN 'completed-evidence-unavailable'
                WHEN 'legacy-verification-3' THEN 'completed'
                ELSE terminal_status
             END
             WHERE run_id = ?1 AND message_hash = ?2",
            params![RUN_ID, seed.verification_request_hash],
        )
        .unwrap();
    drop(connection);

    let error = coordinator.prepare_resume(RUN_ID).await.unwrap_err();

    assert_eq!(error.code(), "MODEL_RESPONSE_RETRY_UNSAFE");
    assert!(
        error
            .detail()
            .contains("exact archived verification history")
    );
    assert_eq!(store.load_run(RUN_ID).unwrap().unwrap(), seed.blocked);
}

#[tokio::test]
async fn unattended_migration_revalidates_source_and_integration_cleanliness() {
    for reason in ["SOURCE_DRIFT", "DIRTY_WORKTREE"] {
        let temp = tempfile::tempdir().unwrap();
        let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
        let app = Arc::new(
            FakeAppServer::deferred(marker_replies(), 6, DeferMode::Hold).with_marker_protocol(),
        );
        let seeding = Coordinator::new(
            Arc::clone(&app),
            store.clone(),
            Arc::new(RecordingSafety::default()),
            legacy_migration_options(),
        );
        let seed = seed_legacy_unattended_verification_history(
            &seeding,
            &app,
            &store,
            ["ready", "cargo-unavailable", "ready"],
            None,
        )
        .await;
        let coordinator = Coordinator::new(
            Arc::clone(&app),
            store.clone(),
            Arc::new(RejectingIntegrationSafety { reason }),
            legacy_migration_options(),
        );

        let error = coordinator.prepare_resume(RUN_ID).await.unwrap_err();

        assert_eq!(error.code(), reason);
        assert_eq!(store.load_run(RUN_ID).unwrap().unwrap(), seed.blocked);
    }
}

#[tokio::test]
async fn unattended_migration_rejects_already_recorded_test_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(
        FakeAppServer::deferred(marker_replies(), 6, DeferMode::Hold).with_marker_protocol(),
    );
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::new(RecordingSafety::default()),
        legacy_migration_options(),
    );
    let seed = seed_legacy_unattended_verification_history(
        &coordinator,
        &app,
        &store,
        ["ready", "cargo-unavailable", "ready"],
        None,
    )
    .await;
    let mut with_evidence = seed.blocked.clone();
    with_evidence.test_evidence.push(TestEvidence {
        command: "cargo test --workspace".into(),
        exit_code: 0,
        turn_id: "legacy-verification-4".into(),
        item_id: "legacy-command-evidence".into(),
        cwd: with_evidence.verification_worktree.clone().unwrap(),
    });
    store.save_state(&with_evidence).unwrap();

    let error = coordinator.prepare_resume(RUN_ID).await.unwrap_err();

    assert_eq!(error.code(), "MODEL_RESPONSE_RETRY_UNSAFE");
    assert!(
        error
            .detail()
            .contains("unchanged unaccepted integration result")
    );
    assert_eq!(store.load_run(RUN_ID).unwrap().unwrap(), with_evidence);
    assert_eq!(
        store
            .archived_turn_ids(RUN_ID, &seed.verification_request_hash)
            .unwrap()
            .len(),
        3
    );
}

#[tokio::test]
async fn cargo_environment_recovery_is_bounded_to_one_attempt() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(
        FakeAppServer::new(marker_replies())
            .with_marker_protocol()
            .with_verification_behavior(VerificationBehavior::CargoUnavailable),
    );
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

    assert_eq!(
        coordinator
            .drive(RUN_ID)
            .await
            .unwrap()
            .reason_code
            .as_deref(),
        Some("CARGO_UNAVAILABLE")
    );
    assert_eq!(
        coordinator
            .resume(RUN_ID)
            .await
            .unwrap()
            .reason_code
            .as_deref(),
        Some("CARGO_UNAVAILABLE")
    );

    let error = coordinator.prepare_resume(RUN_ID).await.unwrap_err();
    assert_eq!(error.code(), "MODEL_RESPONSE_RETRY_UNSAFE");
    assert!(error.detail().contains("limited to one retry"));
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 1);
}

#[tokio::test]
async fn verification_cannot_replace_canonical_integration_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(
        FakeAppServer::new(conflict_free_replies())
            .with_verification_behavior(VerificationBehavior::RewriteIntegrationEvidence),
    );
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

    assert_eq!(result.status, RunStatus::Accepted);
    let result_review_prompt = app
        .prompts()
        .into_iter()
        .find(|prompt| prompt.contains("REQUEST_REVIEWER_RESULT_VERDICT"))
        .unwrap();
    assert!(result_review_prompt.contains("both features are present"));
    assert!(!result_review_prompt.contains("forged verification replacement"));
}

#[tokio::test]
async fn forbidden_integration_command_is_cancelled_and_blocks_the_run() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::deferred(
        conflict_free_replies(),
        5,
        DeferMode::ForbiddenCommand,
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

    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Blocked);
    assert_eq!(result.reason_code.as_deref(), Some("FORBIDDEN_OPERATION"));
    assert_eq!(app.responses(), vec![json!({"decision": "cancel"})]);
    let diagnostic = result.last_error.as_ref().unwrap();
    assert!(
        diagnostic
            .detail
            .contains("outside the frozen integration or verification allowlist")
    );
    assert!(!diagnostic.detail.contains("git push"));
}

#[tokio::test]
async fn interrupted_side_effect_free_forbidden_operation_retries_the_same_run() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let mut replies = conflict_free_replies();
    replies.insert(4, replies[4].clone());
    let app = Arc::new(FakeAppServer::deferred(
        replies,
        5,
        DeferMode::ForbiddenCommand,
    ));
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

    let blocked = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(blocked.status, RunStatus::Blocked);
    assert_eq!(blocked.reason_code.as_deref(), Some("FORBIDDEN_OPERATION"));
    app.set_turn_status("primary", "turn-5", "interrupted");

    let accepted = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(accepted.status, RunStatus::Accepted);
    assert_eq!(app.request_count(), 8);
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 1);
    assert_eq!(
        app.request_order()
            .iter()
            .filter(|request| request.ends_with("REQUEST_PRIMARY_INTEGRATION"))
            .count(),
        2
    );
}

#[tokio::test]
async fn interrupted_forbidden_operation_with_terminal_read_only_queries_retries_the_same_run() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let mut replies = conflict_free_replies();
    replies.insert(4, replies[4].clone());
    let app = Arc::new(FakeAppServer::deferred(
        replies,
        5,
        DeferMode::ForbiddenCommand,
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

    let blocked = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(blocked.status, RunStatus::Blocked);
    app.append_turn_item(
        "primary",
        "turn-5",
        json!({
            "id": "context-compaction-turn-5",
            "type": "contextCompaction"
        }),
    );
    app.append_turn_item(
        "primary",
        "turn-5",
        json!({
            "id": "instruction-discovery-turn-5",
            "type": "commandExecution",
            "command": "/bin/bash -lc 'rg --files -g AGENTS.md'",
            "cwd": "/repo/primary",
            "status": "completed",
            "exitCode": 0,
            "source": "unifiedExecStartup"
        }),
    );
    app.append_turn_item(
        "primary",
        "turn-5",
        json!({
            "id": "read-only-command-turn-5",
            "type": "commandExecution",
            "command": "/bin/bash -lc 'git rev-parse HEAD'",
            "cwd": "/repo/primary",
            "status": "completed",
            "exitCode": 0,
            "source": "unifiedExecStartup"
        }),
    );
    app.append_turn_item(
        "primary",
        "turn-5",
        json!({
            "id": "declined-preflight-turn-5",
            "type": "commandExecution",
            "command": "/bin/bash -lc 'git show-ref --verify refs/heads/consensus/test-run'",
            "cwd": "/repo/primary",
            "status": "declined",
            "exitCode": null,
            "source": "unifiedExecStartup"
        }),
    );
    app.append_turn_item(
        "primary",
        "turn-5",
        json!({
            "id": "declined-branch-preflight-turn-5",
            "type": "commandExecution",
            "command": "/bin/bash -lc 'git branch --list consensus/test-run'",
            "cwd": "/repo/primary",
            "status": "declined",
            "exitCode": null,
            "source": "unifiedExecStartup"
        }),
    );
    app.append_turn_item(
        "primary",
        "turn-5",
        json!({
            "id": "declined-stale-launcher-skill-read-turn-5",
            "type": "commandExecution",
            "command": "sed -n '1,240p' /opt/codex-home/plugins/cache/worktree-merge-consensus/worktree-merge-consensus/0.1.11/skills/worktree-merge-consensus/SKILL.md",
            "cwd": "/repo/primary",
            "status": "declined",
            "exitCode": null,
            "source": "unifiedExecStartup"
        }),
    );
    app.set_turn_status("primary", "turn-5", "interrupted");

    let accepted = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(accepted.status, RunStatus::Accepted);
}

#[tokio::test]
async fn interrupted_forbidden_operation_with_completed_git_write_is_not_retryable() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::deferred(
        conflict_free_replies(),
        5,
        DeferMode::ForbiddenCommand,
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

    let blocked = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(blocked.status, RunStatus::Blocked);
    app.append_turn_item(
        "primary",
        "turn-5",
        json!({
            "id": "write-command-turn-5",
            "type": "commandExecution",
            "command": format!("git switch -c consensus/test-run {PRIMARY_SHA}"),
            "cwd": "/repo/primary",
            "status": "completed",
            "exitCode": 0,
            "source": "agent"
        }),
    );
    app.set_turn_status("primary", "turn-5", "interrupted");

    let error = coordinator.resume(RUN_ID).await.unwrap_err();

    assert_eq!(error.code(), "TERMINAL_TURN_RETRY_UNSAFE");
}

#[tokio::test]
async fn interrupted_forbidden_operation_with_in_progress_read_only_git_is_not_retryable() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::deferred(
        conflict_free_replies(),
        5,
        DeferMode::ForbiddenCommand,
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

    let blocked = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(blocked.status, RunStatus::Blocked);
    app.append_turn_item(
        "primary",
        "turn-5",
        json!({
            "id": "in-progress-command-turn-5",
            "type": "commandExecution",
            "command": "git rev-parse HEAD",
            "cwd": "/repo/primary",
            "status": "inProgress",
            "exitCode": null,
            "source": "agent"
        }),
    );
    app.set_turn_status("primary", "turn-5", "interrupted");

    let error = coordinator.resume(RUN_ID).await.unwrap_err();

    assert_eq!(error.code(), "TERMINAL_TURN_RETRY_UNSAFE");
}

#[tokio::test]
async fn file_change_grant_root_is_cancelled_and_blocks_the_run() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::deferred(
        conflict_free_replies(),
        5,
        DeferMode::FileGrantRoot,
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

    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Blocked);
    assert_eq!(result.reason_code.as_deref(), Some("FORBIDDEN_OPERATION"));
    assert_eq!(app.responses(), vec![json!({"decision": "cancel"})]);
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
        &legacy_request_prompt(&request_hash),
        recovered_reply,
    );

    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(app.request_count(), 6);
    assert!(
        app.request_order()
            .iter()
            .all(|request| request != "primary:REQUEST_PRIMARY_CONTRACT")
    );
    assert!(store.pending_send(RUN_ID).unwrap().is_none());
}

#[tokio::test]
async fn new_turn_start_intent_prevents_legacy_provenance_after_lost_rpc_response() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let store = SqliteRunStore::open(&path).unwrap();
    let app =
        Arc::new(FakeAppServer::new(conflict_free_replies()).with_lost_first_start_response());
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
    set_turn_capability_generation(&path, None);

    let paused = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(paused.status, RunStatus::PausedUserAction);
    assert_eq!(
        store
            .pending_send(RUN_ID)
            .unwrap()
            .unwrap()
            .capability_generation
            .as_deref(),
        Some(consensus_daemon::store::PARTICIPANT_CAPABILITY_GENERATION)
    );
    assert_eq!(app.request_count(), 1);

    drop(coordinator);
    drop(store);
    let reopened = SqliteRunStore::open(&path).unwrap();
    let restarted = Coordinator::new(
        Arc::clone(&app),
        reopened,
        Arc::new(RecordingSafety::default()),
        fast_options(),
    );
    let accepted = restarted.resume(RUN_ID).await.unwrap();

    assert_eq!(accepted.status, RunStatus::Accepted);
    assert_eq!(app.request_count(), 7);
    let generation = Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT capability_generation
             FROM turns
             WHERE run_id = ?1 AND message_hash = ?2",
            params![RUN_ID, request_hash],
            |row| row.get::<_, Option<String>>(0),
        )
        .unwrap();
    assert_eq!(
        generation.as_deref(),
        Some(consensus_daemon::store::PARTICIPANT_CAPABILITY_GENERATION)
    );
}

#[tokio::test]
async fn unrecorded_interrupted_turn_is_recovered_and_paused_without_duplicate_send() {
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
    let request_hash = first_request_hash(&state);
    store
        .record_pending_send(RUN_ID, "PRIMARY", "CONTRACT", 1, &request_hash)
        .unwrap();
    app.inject_interrupted_turn(
        "primary",
        "recovered-interrupted",
        &legacy_request_prompt(&request_hash),
    );

    let paused = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(paused.status, RunStatus::PausedUserAction);
    assert_eq!(paused.reason_code.as_deref(), Some("COMMUNICATION_FAILURE"));
    assert_eq!(app.request_count(), 0);
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 0);
    assert_eq!(
        store
            .pending_send(RUN_ID)
            .unwrap()
            .unwrap()
            .turn_id
            .as_deref(),
        Some("recovered-interrupted")
    );
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
    assert_eq!(app.request_count(), 7);
}

#[tokio::test]
async fn user_input_request_pauses_and_resume_reuses_the_same_turn() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::deferred(
        conflict_free_replies(),
        1,
        DeferMode::UserInput,
    ));
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

    let paused = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(paused.status, RunStatus::PausedUserAction);
    assert_eq!(paused.reason_code.as_deref(), Some("PERMISSION_REQUIRED"));
    assert_eq!(app.request_count(), 1);

    app.complete_deferred_turns();
    let result = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(app.request_count(), 7);
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 0);
}

#[tokio::test]
async fn explicit_resume_replaces_a_side_effect_free_interrupted_turn_once() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let mut replies = conflict_free_replies();
    replies.insert(2, replies[1].clone());
    let app = Arc::new(FakeAppServer::deferred(replies, 2, DeferMode::Interrupted));
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

    let paused = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(paused.status, RunStatus::PausedUserAction);
    assert_eq!(paused.reason_code.as_deref(), Some("COMMUNICATION_FAILURE"));
    assert_eq!(app.request_count(), 2);
    app.append_turn_item(
        "reviewer",
        "turn-2",
        json!({
            "id": "context-compaction-turn-2",
            "type": "contextCompaction"
        }),
    );

    let result = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(app.request_count(), 8);
    assert_eq!(
        app.request_order()
            .iter()
            .filter(|request| request.as_str() == "reviewer:REQUEST_REVIEWER_CONTRACT")
            .count(),
        2
    );
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 1);
    assert!(store.pending_send(RUN_ID).unwrap().is_none());
}

#[tokio::test]
async fn explicit_resume_rejects_an_interrupted_turn_with_execution_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::deferred(
        conflict_free_replies(),
        5,
        DeferMode::InterruptedCommand,
    ));
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

    let paused = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(paused.status, RunStatus::PausedUserAction);
    assert_eq!(paused.next_action, NextAction::RequestPrimaryIntegration);
    assert_eq!(app.request_count(), 5);

    let error = coordinator.resume(RUN_ID).await.unwrap_err();

    assert_eq!(error.code(), "TERMINAL_TURN_RETRY_UNSAFE");
    assert_eq!(app.request_count(), 5);
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 0);
    assert_eq!(
        store.load_run(RUN_ID).unwrap().unwrap().status,
        RunStatus::PausedUserAction
    );
}

#[tokio::test]
async fn invalid_declared_git_test_can_resume_the_same_legacy_blocked_run() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let mut replies = conflict_free_replies();
    let mut invalid_reviewer_contract = replies[1].clone();
    invalid_reviewer_contract["payload"]["contract"]["tests"] = json!(["git diff --check"]);
    replies.insert(1, invalid_reviewer_contract);
    let app = Arc::new(FakeAppServer::new(replies));
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

    let paused = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(paused.status, RunStatus::PausedUserAction);
    assert_eq!(paused.reason_code.as_deref(), Some("INVALID_TEST_COMMAND"));
    assert_eq!(paused.next_action, NextAction::RequestReviewerContract);
    assert_eq!(app.request_count(), 2);
    app.append_turn_item(
        "reviewer",
        "turn-2",
        json!({
            "id": "context-compaction-turn-2",
            "type": "contextCompaction"
        }),
    );
    app.append_turn_item(
        "reviewer",
        "turn-2",
        json!({
            "id": "read-only-mcp-turn-2",
            "type": "mcpToolCall",
            "pluginId": "worktree-merge-consensus@worktree-merge-consensus",
            "server": "worktreeMergeConsensus",
            "tool": "consensus_list_worktrees",
            "arguments": {"repository_path": "/repo/reviewer"},
            "status": "completed",
            "appContext": null
        }),
    );
    let mut legacy_blocked = paused;
    legacy_blocked.block("INVALID_TEST_COMMAND");
    store.save_state(&legacy_blocked).unwrap();

    let result = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(app.request_count(), 8);
    assert_eq!(
        app.request_order()
            .iter()
            .filter(|request| request.as_str() == "reviewer:REQUEST_REVIEWER_CONTRACT")
            .count(),
        2
    );
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 1);
    let reviewer_prompts = app
        .prompts()
        .into_iter()
        .filter(|prompt| prompt_action(prompt) == "REQUEST_REVIEWER_CONTRACT")
        .collect::<Vec<_>>();
    assert_eq!(reviewer_prompts.len(), 2);
    assert!(reviewer_prompts[1].contains("direct non-Git commands"));
    assert!(reviewer_prompts[1].contains("Do not call `worktreeMergeConsensus`"));
}

#[tokio::test]
async fn invalid_test_response_retry_rejects_file_change_history() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let mut replies = conflict_free_replies();
    let mut invalid_reviewer_contract = replies[1].clone();
    invalid_reviewer_contract["payload"]["contract"]["tests"] = json!(["git diff --check"]);
    replies.insert(1, invalid_reviewer_contract);
    let app = Arc::new(FakeAppServer::new(replies));
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
    let paused = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(paused.status, RunStatus::PausedUserAction);
    app.append_turn_item(
        "reviewer",
        "turn-2",
        json!({
            "id": "file-turn-2",
            "type": "fileChange",
            "status": "completed"
        }),
    );

    let error = coordinator.resume(RUN_ID).await.unwrap_err();

    assert_eq!(error.code(), "MODEL_RESPONSE_RETRY_UNSAFE");
    assert_eq!(app.request_count(), 2);
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 0);
    assert_eq!(
        store.load_run(RUN_ID).unwrap().unwrap().status,
        RunStatus::PausedUserAction
    );
}

#[tokio::test]
async fn invalid_test_response_retry_rejects_mutating_consensus_mcp_history() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let mut replies = conflict_free_replies();
    let mut invalid_reviewer_contract = replies[1].clone();
    invalid_reviewer_contract["payload"]["contract"]["tests"] = json!(["git diff --check"]);
    replies.insert(1, invalid_reviewer_contract);
    let app = Arc::new(FakeAppServer::new(replies));
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
    let paused = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(paused.status, RunStatus::PausedUserAction);
    app.append_turn_item(
        "reviewer",
        "turn-2",
        json!({
            "id": "mutating-mcp-turn-2",
            "type": "mcpToolCall",
            "pluginId": "worktree-merge-consensus@worktree-merge-consensus",
            "server": "worktreeMergeConsensus",
            "tool": "consensus_start",
            "arguments": {},
            "status": "completed",
            "appContext": null
        }),
    );

    let error = coordinator.resume(RUN_ID).await.unwrap_err();

    assert_eq!(error.code(), "MODEL_RESPONSE_RETRY_UNSAFE");
    assert!(error.detail().contains("consensus_start"));
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 0);
    assert_eq!(
        store.load_run(RUN_ID).unwrap().unwrap().status,
        RunStatus::PausedUserAction
    );
}

#[tokio::test]
async fn user_input_during_integration_resumes_the_authorized_in_progress_turn() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::deferred(
        conflict_free_replies(),
        5,
        DeferMode::UserInput,
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
    assert_eq!(app.request_count(), 7);
}

#[tokio::test]
async fn recovered_integration_turn_skips_the_first_action_frozen_head_check() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::deferred(
        conflict_free_replies(),
        5,
        DeferMode::UserInput,
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
async fn completed_bwrap_file_change_blocker_retries_the_same_run_and_existing_merge() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let normal = conflict_free_replies();
    let mut replies = normal[..4].to_vec();
    replies.push(file_change_tool_unavailable_blocker());
    replies.push(normal[4].clone());
    replies.push(normal[5].clone());
    let app = Arc::new(FakeAppServer::deferred(replies, 5, DeferMode::Hold));
    let safety = Arc::new(InProgressRecoverySafety::default());
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::clone(&safety),
        CoordinatorOptions {
            wait_timeout: Duration::from_millis(15),
            poll_interval: Duration::from_millis(1),
            communication_attempts: 1,
            participant_mcp_executable: participant_mcp_executable(),
        },
    );
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();

    let paused = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(paused.status, RunStatus::PausedUserAction);
    assert_eq!(paused.reason_code.as_deref(), Some("COMMUNICATION_FAILURE"));
    assert_eq!(paused.next_action, NextAction::RequestPrimaryIntegration);

    safety
        .integration_branch_active
        .store(true, Ordering::SeqCst);
    app.complete_deferred_turns();
    let result = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 1);
    assert_eq!(app.request_count(), 8);
    let integration_prompts = app
        .prompts()
        .into_iter()
        .filter(|prompt| prompt_action(prompt) == "REQUEST_PRIMARY_INTEGRATION")
        .collect::<Vec<_>>();
    assert_eq!(integration_prompts.len(), 2);
    assert!(integration_prompts[1].contains("consensus_apply_patch"));
    assert!(integration_prompts[1].contains("do not recreate or re-merge"));
}

#[tokio::test]
async fn pending_controlled_patch_approval_is_interrupted_and_retried_on_the_same_run() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let normal = conflict_free_replies();
    let mut replies = normal[..5].to_vec();
    replies.push(normal[4].clone());
    replies.push(normal[5].clone());
    let app = Arc::new(FakeAppServer::deferred(
        replies,
        5,
        DeferMode::PatchApproval,
    ));
    let safety = Arc::new(InProgressRecoverySafety::default());
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::clone(&safety),
        CoordinatorOptions {
            wait_timeout: Duration::from_millis(15),
            poll_interval: Duration::from_millis(1),
            communication_attempts: 1,
            participant_mcp_executable: participant_mcp_executable(),
        },
    );
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();

    let paused = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(paused.status, RunStatus::PausedUserAction);
    assert_eq!(paused.reason_code.as_deref(), Some("COMMUNICATION_FAILURE"));
    assert_eq!(paused.next_action, NextAction::RequestPrimaryIntegration);
    assert!(
        !store
            .successful_patch_recorded(
                RUN_ID,
                &store.pending_send(RUN_ID).unwrap().unwrap().message_hash
            )
            .unwrap()
    );

    safety
        .integration_branch_active
        .store(true, Ordering::SeqCst);
    let result = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(
        app.interrupts(),
        vec![("primary".to_owned(), "turn-5".to_owned())]
    );
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 1);
    assert_eq!(app.request_count(), 8);
}

#[tokio::test]
async fn completed_patch_rejected_while_paused_retries_the_same_run() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let normal = conflict_free_replies();
    let mut replies = normal[..5].to_vec();
    replies.push(normal[4].clone());
    replies.push(normal[5].clone());
    let app = Arc::new(FakeAppServer::deferred(
        replies,
        5,
        DeferMode::PatchApproval,
    ));
    let safety = Arc::new(InProgressRecoverySafety::default());
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::clone(&safety),
        CoordinatorOptions {
            wait_timeout: Duration::from_millis(15),
            poll_interval: Duration::from_millis(1),
            communication_attempts: 1,
            participant_mcp_executable: participant_mcp_executable(),
        },
    );
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();

    let paused = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(paused.status, RunStatus::PausedUserAction);
    assert_eq!(paused.reason_code.as_deref(), Some("COMMUNICATION_FAILURE"));
    assert_eq!(paused.next_action, NextAction::RequestPrimaryIntegration);

    safety
        .integration_branch_active
        .store(true, Ordering::SeqCst);
    app.reject_deferred_patch_not_authorized();
    let result = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert!(app.interrupts().is_empty());
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 1);
    assert_eq!(app.request_count(), 8);
    let integration_prompts = app
        .prompts()
        .into_iter()
        .filter(|prompt| prompt_action(prompt) == "REQUEST_PRIMARY_INTEGRATION")
        .collect::<Vec<_>>();
    assert_eq!(integration_prompts.len(), 2);
    assert!(integration_prompts[1].contains("consensus_apply_patch"));
    assert!(integration_prompts[1].contains("do not recreate or re-merge"));
}

#[tokio::test]
async fn completed_patch_rejection_without_redundant_text_retries_the_same_run() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let normal = conflict_free_replies();
    let mut replies = normal[..5].to_vec();
    replies.push(normal[4].clone());
    replies.push(normal[5].clone());
    let app = Arc::new(FakeAppServer::deferred(
        replies,
        5,
        DeferMode::PatchApproval,
    ));
    let safety = Arc::new(InProgressRecoverySafety::default());
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::clone(&safety),
        CoordinatorOptions {
            wait_timeout: Duration::from_millis(15),
            poll_interval: Duration::from_millis(1),
            communication_attempts: 1,
            participant_mcp_executable: participant_mcp_executable(),
        },
    );
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();

    let paused = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(paused.status, RunStatus::PausedUserAction);
    safety
        .integration_branch_active
        .store(true, Ordering::SeqCst);
    app.reject_deferred_patch_not_authorized_without_redundant_text();

    let result = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert!(app.interrupts().is_empty());
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 1);
    assert_eq!(app.request_count(), 8);
}

#[tokio::test]
async fn completed_patch_rejection_without_critical_identity_is_not_retryable() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::deferred(
        conflict_free_replies(),
        5,
        DeferMode::PatchApproval,
    ));
    let safety = Arc::new(InProgressRecoverySafety::default());
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::clone(&safety),
        CoordinatorOptions {
            wait_timeout: Duration::from_millis(15),
            poll_interval: Duration::from_millis(1),
            communication_attempts: 1,
            participant_mcp_executable: participant_mcp_executable(),
        },
    );
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();

    let paused = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(paused.status, RunStatus::PausedUserAction);
    safety
        .integration_branch_active
        .store(true, Ordering::SeqCst);
    app.reject_deferred_patch_not_authorized_without_plan_hash();

    let error = coordinator.resume(RUN_ID).await.unwrap_err();

    assert_eq!(error.code(), "TERMINAL_TURN_RETRY_UNSAFE");
    assert_eq!(app.request_count(), 5);
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 0);
    assert_eq!(
        store.load_run(RUN_ID).unwrap().unwrap().status,
        RunStatus::PausedUserAction
    );
}

#[tokio::test]
async fn in_progress_patch_rejected_while_paused_is_interrupted_and_retried() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let normal = conflict_free_replies();
    let mut replies = normal[..5].to_vec();
    replies.push(normal[4].clone());
    replies.push(normal[5].clone());
    let app = Arc::new(FakeAppServer::deferred(
        replies,
        5,
        DeferMode::PatchApproval,
    ));
    let safety = Arc::new(InProgressRecoverySafety::default());
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::clone(&safety),
        CoordinatorOptions {
            wait_timeout: Duration::from_millis(15),
            poll_interval: Duration::from_millis(1),
            communication_attempts: 1,
            participant_mcp_executable: participant_mcp_executable(),
        },
    );
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();

    let paused = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(paused.status, RunStatus::PausedUserAction);
    assert_eq!(paused.reason_code.as_deref(), Some("COMMUNICATION_FAILURE"));
    assert_eq!(paused.next_action, NextAction::RequestPrimaryIntegration);

    safety
        .integration_branch_active
        .store(true, Ordering::SeqCst);
    app.reject_deferred_patch_not_authorized_in_progress();
    let result = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(
        app.interrupts(),
        vec![("primary".to_owned(), "turn-5".to_owned())]
    );
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 1);
    assert_eq!(app.request_count(), 8);
    let integration_prompts = app
        .prompts()
        .into_iter()
        .filter(|prompt| prompt_action(prompt) == "REQUEST_PRIMARY_INTEGRATION")
        .collect::<Vec<_>>();
    assert_eq!(integration_prompts.len(), 2);
    assert!(integration_prompts[1].contains("consensus_apply_patch"));
    assert!(integration_prompts[1].contains("do not recreate or re-merge"));
}

#[tokio::test]
async fn failed_patch_without_final_json_is_interrupted_and_retried() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let normal = conflict_free_replies();
    let mut replies = normal[..5].to_vec();
    replies.push(normal[4].clone());
    replies.push(normal[5].clone());
    let app = Arc::new(FakeAppServer::deferred(
        replies,
        5,
        DeferMode::PatchApproval,
    ));
    let safety = Arc::new(InProgressRecoverySafety::default());
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::clone(&safety),
        CoordinatorOptions {
            wait_timeout: Duration::from_millis(15),
            poll_interval: Duration::from_millis(1),
            communication_attempts: 1,
            participant_mcp_executable: participant_mcp_executable(),
        },
    );
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();

    let paused = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(paused.status, RunStatus::PausedUserAction);
    assert_eq!(paused.reason_code.as_deref(), Some("COMMUNICATION_FAILURE"));
    safety
        .integration_branch_active
        .store(true, Ordering::SeqCst);
    app.fail_deferred_patch_without_final();

    let result = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(
        app.interrupts(),
        vec![("primary".to_owned(), "turn-5".to_owned())]
    );
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 1);
    assert_eq!(app.request_count(), 8);
}

#[tokio::test]
async fn failed_patch_without_final_json_rejects_unknown_turn_items() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::deferred(
        conflict_free_replies(),
        5,
        DeferMode::PatchApproval,
    ));
    let safety = Arc::new(InProgressRecoverySafety::default());
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::clone(&safety),
        CoordinatorOptions {
            wait_timeout: Duration::from_millis(15),
            poll_interval: Duration::from_millis(1),
            communication_attempts: 1,
            participant_mcp_executable: participant_mcp_executable(),
        },
    );
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();

    let paused = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(paused.status, RunStatus::PausedUserAction);
    safety
        .integration_branch_active
        .store(true, Ordering::SeqCst);
    app.fail_deferred_patch_without_final();
    app.append_turn_item(
        "primary",
        "turn-5",
        json!({
            "id": "unknown-turn-5",
            "type": "webSearch",
            "status": "completed"
        }),
    );

    let error = coordinator.resume(RUN_ID).await.unwrap_err();

    assert_eq!(error.code(), "TERMINAL_TURN_RETRY_UNSAFE");
    assert!(app.interrupts().is_empty());
    assert_eq!(app.request_count(), 5);
    assert_eq!(store.turn_attempt_count(RUN_ID).unwrap(), 0);
    assert_eq!(
        store.load_run(RUN_ID).unwrap().unwrap().status,
        RunStatus::PausedUserAction
    );
}

#[tokio::test]
async fn controlled_patch_requires_the_exact_active_request_and_succeeds_only_once() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::deferred(
        conflict_free_replies(),
        5,
        DeferMode::Hold,
    ));
    let safety = Arc::new(PatchSafety::default());
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::clone(&safety),
        CoordinatorOptions {
            wait_timeout: Duration::from_secs(10),
            poll_interval: Duration::from_millis(1),
            communication_attempts: 1,
            participant_mcp_executable: participant_mcp_executable(),
        },
    );
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();
    let driver = {
        let coordinator = coordinator.clone();
        tokio::spawn(async move { coordinator.drive(RUN_ID).await })
    };
    for _ in 0..1_000 {
        if app.request_count() >= 5 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(app.request_count(), 5);
    let pending = store.pending_send(RUN_ID).unwrap().unwrap();

    let wrong = coordinator
        .apply_patch(RUN_ID, "wrong-request", "diff --git a/a b/a")
        .await
        .unwrap_err();
    assert_eq!(wrong.code(), "PATCH_NOT_AUTHORIZED");
    let applied = coordinator
        .apply_patch(
            RUN_ID,
            &pending.message_hash,
            "diff --git a/src/lib.rs b/src/lib.rs",
        )
        .await
        .unwrap();
    assert_eq!(applied.integration_branch, "consensus/test-run");
    assert_eq!(applied.base_sha, INTEGRATION_SHA);
    assert_eq!(applied.changed_files, vec![PathBuf::from("src/lib.rs")]);
    assert_eq!(safety.patch_calls.load(Ordering::SeqCst), 1);

    let duplicate = coordinator
        .apply_patch(
            RUN_ID,
            &pending.message_hash,
            "diff --git a/src/main.rs b/src/main.rs",
        )
        .await
        .unwrap_err();
    assert_eq!(duplicate.code(), "PATCH_ALREADY_APPLIED");
    assert_eq!(safety.patch_calls.load(Ordering::SeqCst), 1);

    coordinator.cancel(RUN_ID).await.unwrap();
    assert_eq!(driver.await.unwrap().unwrap().status, RunStatus::Cancelled);
}

#[tokio::test]
async fn controlled_patch_on_mirror_records_exact_binding_provenance() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(
        FakeAppServer::deferred(conflict_free_replies(), 5, DeferMode::Hold)
            .without_primary_participant(),
    );
    let safety = Arc::new(PatchSafety::default());
    let coordinator = Coordinator::new(
        Arc::clone(&app),
        store.clone(),
        Arc::clone(&safety),
        CoordinatorOptions {
            wait_timeout: Duration::from_secs(10),
            poll_interval: Duration::from_millis(1),
            communication_attempts: 1,
            participant_mcp_executable: participant_mcp_executable(),
        },
    );
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();
    let driver = {
        let coordinator = coordinator.clone();
        tokio::spawn(async move { coordinator.drive(RUN_ID).await })
    };
    wait_for_request_count(&app, 5).await;
    let pending = wait_for_bound_pending_send(&store).await;

    assert_eq!(
        pending.thread_id.as_deref(),
        Some("primary-consensus-mirror-1")
    );
    assert_eq!(pending.participant_binding_generation, Some(1));
    coordinator
        .apply_patch(
            RUN_ID,
            &pending.message_hash,
            "diff --git a/src/lib.rs b/src/lib.rs",
        )
        .await
        .unwrap();
    let record = store
        .successful_patch_record(RUN_ID, &pending.message_hash)
        .unwrap()
        .unwrap();
    assert_eq!(record.source_primary_thread_id.as_deref(), Some("primary"));
    assert_eq!(
        record.effective_primary_thread_id.as_deref(),
        Some("primary-consensus-mirror-1")
    );
    assert_eq!(record.participant_binding_generation, Some(1));
    assert_eq!(safety.patch_calls.load(Ordering::SeqCst), 1);

    coordinator.cancel(RUN_ID).await.unwrap();
    assert_eq!(driver.await.unwrap().unwrap().status, RunStatus::Cancelled);
}

#[tokio::test]
async fn controlled_patch_rejects_each_binding_identity_mutation() {
    for mutation in [
        "pending-source",
        "pending-generation",
        "active-generation",
        "binding-source",
        "missing-generation",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let store_path = temp.path().join("state.db");
        let store = SqliteRunStore::open(&store_path).unwrap();
        let app = Arc::new(
            FakeAppServer::deferred(conflict_free_replies(), 5, DeferMode::Hold)
                .without_primary_participant(),
        );
        let safety = Arc::new(PatchSafety::default());
        let coordinator = Coordinator::new(
            Arc::clone(&app),
            store.clone(),
            Arc::clone(&safety),
            CoordinatorOptions {
                wait_timeout: Duration::from_secs(10),
                poll_interval: Duration::from_millis(1),
                communication_attempts: 1,
                participant_mcp_executable: participant_mcp_executable(),
            },
        );
        coordinator
            .start(fixture_run(), start_request())
            .await
            .unwrap();
        let driver = {
            let coordinator = coordinator.clone();
            tokio::spawn(async move { coordinator.drive(RUN_ID).await })
        };
        wait_for_request_count(&app, 5).await;
        let pending = wait_for_bound_pending_send(&store).await;
        let connection = Connection::open(&store_path).unwrap();
        match mutation {
            "pending-source" => {
                connection
                    .execute(
                        "UPDATE turns SET thread_id = 'primary'
                         WHERE run_id = ?1 AND delivery_state = 'SENT'",
                        [RUN_ID],
                    )
                    .unwrap();
            }
            "pending-generation" => {
                connection
                    .execute(
                        "UPDATE turns SET participant_binding_generation = 999
                         WHERE run_id = ?1 AND delivery_state = 'SENT'",
                        [RUN_ID],
                    )
                    .unwrap();
            }
            "active-generation" => {
                connection
                    .execute(
                        "UPDATE primary_participant_bindings SET active = 0
                         WHERE run_id = ?1 AND active = 1",
                        [RUN_ID],
                    )
                    .unwrap();
                connection
                    .execute(
                        "INSERT INTO primary_participant_bindings (
                            run_id, generation, source_primary_thread_id,
                            effective_primary_thread_id, mode, participant_server,
                            active, created_at, verified_at
                         ) VALUES (?1, 2, 'primary', 'primary-consensus-mirror-2',
                            'EPHEMERAL_FORK', ?2, 1, 1, 1)",
                        params![RUN_ID, PARTICIPANT_MCP_SERVER],
                    )
                    .unwrap();
            }
            "binding-source" => {
                connection
                    .execute(
                        "UPDATE primary_participant_bindings
                         SET source_primary_thread_id = 'forged-primary'
                         WHERE run_id = ?1 AND active = 1",
                        [RUN_ID],
                    )
                    .unwrap();
            }
            "missing-generation" => {
                connection
                    .execute(
                        "UPDATE turns SET participant_binding_generation = NULL
                         WHERE run_id = ?1 AND delivery_state = 'SENT'",
                        [RUN_ID],
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }

        let error = coordinator
            .apply_patch(
                RUN_ID,
                &pending.message_hash,
                "diff --git a/src/lib.rs b/src/lib.rs",
            )
            .await
            .unwrap_err();

        assert_eq!(error.code(), "PATCH_NOT_AUTHORIZED", "mutation={mutation}");
        assert_eq!(safety.patch_calls.load(Ordering::SeqCst), 0);
        coordinator.cancel(RUN_ID).await.unwrap();
        assert_eq!(driver.await.unwrap().unwrap().status, RunStatus::Cancelled);
    }
}

#[tokio::test]
async fn recovered_result_fix_turn_allows_head_to_advance_past_previous_result() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::deferred(
        result_revision_replies(),
        8,
        DeferMode::UserInput,
    ));
    let safety = Arc::new(InProgressRecoverySafety::with_stale_sha(INTEGRATION_SHA));
    let coordinator =
        Coordinator::new(Arc::clone(&app), store, Arc::clone(&safety), fast_options());
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();
    let paused = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(paused.status, RunStatus::PausedUserAction);
    assert_eq!(paused.integration_sha.as_deref(), Some(INTEGRATION_SHA));

    safety
        .integration_branch_active
        .store(true, Ordering::SeqCst);
    app.complete_deferred_turns();
    let result = coordinator.resume(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(
        result.integration_sha.as_deref(),
        Some("dddddddddddddddddddddddddddddddddddddddd")
    );
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
            participant_mcp_executable: participant_mcp_executable(),
        },
    );
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();

    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::PausedUserAction);
    assert_eq!(result.reason_code.as_deref(), Some("COMMUNICATION_FAILURE"));
    let diagnostic = result.last_error.as_ref().unwrap();
    assert_eq!(diagnostic.code, "COMMUNICATION_FAILURE");
    assert_eq!(diagnostic.action, NextAction::RequestPrimaryContract);
    assert_eq!(diagnostic.thread_id.as_deref(), Some("primary"));
    assert!(diagnostic.detail.contains("bounded idle wait"));
}

#[tokio::test]
async fn canonical_turn_progress_renews_the_bounded_idle_wait() {
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
        CoordinatorOptions {
            wait_timeout: Duration::from_millis(250),
            poll_interval: Duration::from_millis(1),
            communication_attempts: 1,
            participant_mcp_executable: participant_mcp_executable(),
        },
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

    for item_id in ["reasoning-progress-1", "reasoning-progress-2"] {
        tokio::time::sleep(Duration::from_millis(100)).await;
        app.append_turn_item(
            "primary",
            "turn-1",
            json!({"id": item_id, "type": "reasoning", "summary": []}),
        );
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    app.complete_deferred_turns();

    let result = driver.await.unwrap().unwrap();
    assert_eq!(result.status, RunStatus::Accepted);
}

#[tokio::test]
async fn turn_start_failure_persists_redacted_rpc_context() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(temp.path().join("state.db")).unwrap();
    let app = Arc::new(FakeAppServer::failing_start(
        "task must be resumed before turn/start",
    ));
    let coordinator = Coordinator::new(
        app,
        store.clone(),
        Arc::new(RecordingSafety::default()),
        fast_options(),
    );
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();

    let result = coordinator.drive(RUN_ID).await.unwrap();

    assert_eq!(result.status, RunStatus::PausedUserAction);
    let diagnostic = result.last_error.as_ref().unwrap();
    assert_eq!(diagnostic.code, "COMMUNICATION_FAILURE");
    assert_eq!(diagnostic.operation.as_deref(), Some("turn/start"));
    assert_eq!(diagnostic.action, NextAction::RequestPrimaryContract);
    assert_eq!(diagnostic.role, Some(consensus_core::state::Role::Primary));
    assert_eq!(diagnostic.thread_id.as_deref(), Some("primary"));
    assert!(diagnostic.detail.contains("must be resumed"));
    assert_eq!(
        store.load_run(RUN_ID).unwrap().unwrap().last_error,
        result.last_error
    );
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
    integration_branch_active: AtomicBool,
    in_progress_calls: AtomicUsize,
}

impl RecordingSafety {
    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
}

impl RepositorySafety for RecordingSafety {
    fn verify_frozen(&self, _facts: &RunFacts) -> Result<(), SafetyError> {
        if self.integration_branch_active.load(Ordering::SeqCst) {
            return Err(SafetyError::new(
                "SOURCE_DRIFT",
                "primary HEAD has moved to the authorized integration branch",
            ));
        }
        self.events.lock().unwrap().push("frozen".into());
        Ok(())
    }

    fn verify_branch_absent(&self, _facts: &RunFacts, branch: &str) -> Result<(), SafetyError> {
        self.events.lock().unwrap().push(format!("absent:{branch}"));
        Ok(())
    }

    fn verify_integration_in_progress(
        &self,
        _facts: &RunFacts,
        target_branch: &str,
    ) -> Result<(), SafetyError> {
        self.in_progress_calls.fetch_add(1, Ordering::SeqCst);
        self.events
            .lock()
            .unwrap()
            .push(format!("in-progress:{target_branch}"));
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

    fn authoritative_integration_result(
        &self,
        _facts: &RunFacts,
        _target_branch: &str,
    ) -> Result<(String, Vec<PathBuf>), SafetyError> {
        Ok((
            INTEGRATION_SHA.to_owned(),
            vec![PathBuf::from("combined.txt")],
        ))
    }

    fn prepare_verification_workspace(
        &self,
        _facts: &RunFacts,
        _integration_sha: &str,
        destination: &std::path::Path,
    ) -> Result<PathBuf, SafetyError> {
        Ok(destination.to_path_buf())
    }
}

#[derive(Default)]
struct AdvancingIntegrationSafety {
    authoritative_calls: AtomicUsize,
}

impl RepositorySafety for AdvancingIntegrationSafety {
    fn verify_frozen(&self, _facts: &RunFacts) -> Result<(), SafetyError> {
        Ok(())
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

    fn authoritative_integration_result(
        &self,
        _facts: &RunFacts,
        _target_branch: &str,
    ) -> Result<(String, Vec<PathBuf>), SafetyError> {
        let sha = if self.authoritative_calls.fetch_add(1, Ordering::SeqCst) == 0 {
            INTEGRATION_SHA
        } else {
            CORRECTED_INTEGRATION_SHA
        };
        Ok((sha.to_owned(), vec![PathBuf::from("combined.txt")]))
    }

    fn prepare_verification_workspace(
        &self,
        _facts: &RunFacts,
        _integration_sha: &str,
        destination: &Path,
    ) -> Result<PathBuf, SafetyError> {
        Ok(destination.to_path_buf())
    }
}

#[derive(Default)]
struct CorrectiveRecoverySafety {
    branch_absent_calls: AtomicUsize,
    patch_calls: AtomicUsize,
    corrective_patch_bases: Mutex<Vec<String>>,
    verified_results: Mutex<Vec<String>>,
    result_verification_error: Mutex<Option<&'static str>>,
}

impl CorrectiveRecoverySafety {
    fn branch_absent_calls(&self) -> usize {
        self.branch_absent_calls.load(Ordering::SeqCst)
    }

    fn verified_results(&self) -> Vec<String> {
        self.verified_results.lock().unwrap().clone()
    }

    fn fail_result_verification(&self, code: &'static str) {
        *self.result_verification_error.lock().unwrap() = Some(code);
    }
}

impl RepositorySafety for CorrectiveRecoverySafety {
    fn verify_frozen(&self, _facts: &RunFacts) -> Result<(), SafetyError> {
        Ok(())
    }

    fn verify_branch_absent(&self, _facts: &RunFacts, _branch: &str) -> Result<(), SafetyError> {
        self.branch_absent_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn verify_integration_in_progress(
        &self,
        _facts: &RunFacts,
        _target_branch: &str,
    ) -> Result<(), SafetyError> {
        Ok(())
    }

    fn apply_corrective_integration_patch(
        &self,
        _facts: &RunFacts,
        _target_branch: &str,
        expected_base_sha: &str,
        _patch: &str,
    ) -> Result<(String, Vec<PathBuf>), SafetyError> {
        self.patch_calls.fetch_add(1, Ordering::SeqCst);
        self.corrective_patch_bases
            .lock()
            .unwrap()
            .push(expected_base_sha.to_owned());
        Ok((
            expected_base_sha.to_owned(),
            vec![PathBuf::from("combined.txt")],
        ))
    }

    fn verify_integration(
        &self,
        _facts: &RunFacts,
        branch: &str,
        sha: &str,
        _changed_files: &[PathBuf],
    ) -> Result<(), SafetyError> {
        if let Some(code) = *self.result_verification_error.lock().unwrap() {
            return Err(SafetyError::new(
                code,
                "scripted corrective recovery repository failure",
            ));
        }
        self.verified_results
            .lock()
            .unwrap()
            .push(format!("{branch}:{sha}"));
        Ok(())
    }

    fn authoritative_integration_result(
        &self,
        _facts: &RunFacts,
        _target_branch: &str,
    ) -> Result<(String, Vec<PathBuf>), SafetyError> {
        Ok((
            CORRECTED_INTEGRATION_SHA.to_owned(),
            vec![PathBuf::from("combined.txt")],
        ))
    }

    fn prepare_verification_workspace(
        &self,
        _facts: &RunFacts,
        _integration_sha: &str,
        destination: &Path,
    ) -> Result<PathBuf, SafetyError> {
        Ok(destination.to_path_buf())
    }
}

struct RejectingIntegrationSafety {
    reason: &'static str,
}

impl RepositorySafety for RejectingIntegrationSafety {
    fn verify_frozen(&self, _facts: &RunFacts) -> Result<(), SafetyError> {
        Ok(())
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
        Err(SafetyError::new(
            self.reason,
            "scripted migration repository revalidation failure",
        ))
    }

    fn prepare_verification_workspace(
        &self,
        _facts: &RunFacts,
        _integration_sha: &str,
        destination: &Path,
    ) -> Result<PathBuf, SafetyError> {
        Ok(destination.to_path_buf())
    }
}

struct InProgressRecoverySafety {
    integration_branch_active: AtomicBool,
    in_progress_calls: AtomicUsize,
    stale_integration_sha: Option<&'static str>,
}

impl Default for InProgressRecoverySafety {
    fn default() -> Self {
        Self {
            integration_branch_active: AtomicBool::new(false),
            in_progress_calls: AtomicUsize::new(0),
            stale_integration_sha: None,
        }
    }
}

impl InProgressRecoverySafety {
    fn with_stale_sha(sha: &'static str) -> Self {
        Self {
            stale_integration_sha: Some(sha),
            ..Self::default()
        }
    }
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

    fn verify_integration_patch_ready(
        &self,
        _facts: &RunFacts,
        _target_branch: &str,
    ) -> Result<String, SafetyError> {
        if self.integration_branch_active.load(Ordering::SeqCst) {
            Ok(INTEGRATION_SHA.into())
        } else {
            Err(SafetyError::new(
                "UNEXPECTED_INTEGRATION_BRANCH",
                "integration branch is not active",
            ))
        }
    }

    fn verify_integration(
        &self,
        _facts: &RunFacts,
        _branch: &str,
        sha: &str,
        _changed_files: &[PathBuf],
    ) -> Result<(), SafetyError> {
        if self.integration_branch_active.load(Ordering::SeqCst)
            && self.stale_integration_sha == Some(sha)
        {
            return Err(SafetyError::new(
                "STALE_INTEGRATION_SHA",
                "integration HEAD advanced while the result-fix turn was pending",
            ));
        }
        Ok(())
    }

    fn prepare_verification_workspace(
        &self,
        _facts: &RunFacts,
        _integration_sha: &str,
        destination: &std::path::Path,
    ) -> Result<PathBuf, SafetyError> {
        Ok(destination.to_path_buf())
    }
}

struct FailAfterStartSafety {
    reason: &'static str,
    frozen_calls: AtomicUsize,
}

#[derive(Default)]
struct PatchSafety {
    patch_calls: AtomicUsize,
}

impl RepositorySafety for PatchSafety {
    fn verify_frozen(&self, _facts: &RunFacts) -> Result<(), SafetyError> {
        Ok(())
    }

    fn verify_branch_absent(&self, _facts: &RunFacts, _branch: &str) -> Result<(), SafetyError> {
        Ok(())
    }

    fn verify_integration_in_progress(
        &self,
        _facts: &RunFacts,
        _target_branch: &str,
    ) -> Result<(), SafetyError> {
        Ok(())
    }

    fn apply_integration_patch(
        &self,
        _facts: &RunFacts,
        _target_branch: &str,
        _patch: &str,
    ) -> Result<(String, Vec<PathBuf>), SafetyError> {
        self.patch_calls.fetch_add(1, Ordering::SeqCst);
        Ok((INTEGRATION_SHA.into(), vec![PathBuf::from("src/lib.rs")]))
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

    fn prepare_verification_workspace(
        &self,
        _facts: &RunFacts,
        _integration_sha: &str,
        destination: &std::path::Path,
    ) -> Result<PathBuf, SafetyError> {
        Ok(destination.to_path_buf())
    }
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

    fn prepare_verification_workspace(
        &self,
        _facts: &RunFacts,
        _integration_sha: &str,
        destination: &std::path::Path,
    ) -> Result<PathBuf, SafetyError> {
        Ok(destination.to_path_buf())
    }
}

struct FakeAppServer {
    replies: Mutex<VecDeque<Value>>,
    threads: Mutex<HashMap<String, Vec<Value>>>,
    requests: Mutex<Vec<String>>,
    method_order: Mutex<Vec<String>>,
    resumes: Mutex<Vec<String>>,
    resume_policies: Mutex<Vec<ThreadResumePolicy>>,
    resume_tickets: Mutex<HashMap<String, usize>>,
    reply_types: Mutex<Vec<String>>,
    prompts: Mutex<Vec<String>>,
    policies: Mutex<Vec<TurnExecutionPolicy>>,
    responses: Mutex<Vec<Value>>,
    interrupts: Mutex<Vec<(String, String)>>,
    approval_mode: Mutex<Option<String>>,
    approval_mode_requests: AtomicUsize,
    deferred: Option<(usize, DeferMode)>,
    deferred_replies: Mutex<HashMap<String, Value>>,
    events: Mutex<VecDeque<AppEvent>>,
    executed_commands: Mutex<Vec<CommandExecRequest>>,
    verification_command_counts: Mutex<HashMap<usize, usize>>,
    verification_behavior: VerificationBehavior,
    verification_item_type: Option<&'static str>,
    marker_protocol: bool,
    event_only_turn_items: bool,
    start_error: Option<String>,
    lose_next_start_response: AtomicBool,
    participant_inventory: ParticipantInventory,
    primary_runtime_status: Mutex<ThreadRuntimeStatus>,
    participant_threads: Mutex<BTreeSet<String>>,
    forks: Mutex<Vec<(String, String, ThreadForkPolicy)>>,
    goals: Mutex<BTreeMap<String, Option<Value>>>,
    fork_identity: ForkIdentity,
    fork_history: ForkHistory,
    fork_runtime_status: ThreadRuntimeStatus,
    participant_status_calls: AtomicUsize,
    participant_failure_after_status_calls: Option<usize>,
    remove_mirror_after_request: Option<usize>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ParticipantInventory {
    #[default]
    Available,
    MissingServer,
    MissingTool,
    ExtraTool,
    MalformedDefinition,
    StatusUnavailable,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ForkIdentity {
    #[default]
    Mirror,
    Source,
    Reviewer,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ForkHistory {
    #[default]
    Exact,
    MissingLast,
    Reversed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeferMode {
    UserInput,
    PatchApproval,
    ForbiddenCommand,
    FileGrantRoot,
    Hold,
    Interrupted,
    InterruptedCommand,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum VerificationBehavior {
    #[default]
    Pass,
    EmptyReport,
    LegacyReport,
    FailedExecution,
    FailedExecutionThenPass,
    CargoUnavailable,
    MissingExecution,
    // Retained for Task 4's legacy verification migration fixtures.
    #[allow(dead_code)]
    MissingExecutionThenPass,
    #[allow(dead_code)]
    MissingThenCargoUnavailableThenPass,
    #[allow(dead_code)]
    MissingThenCargoUnavailableThenMissing,
    #[allow(dead_code)]
    MissingThenCargoUnavailableThenMissingThenPass,
    RewriteIntegrationEvidence,
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
            method_order: Mutex::new(Vec::new()),
            resumes: Mutex::new(Vec::new()),
            resume_policies: Mutex::new(Vec::new()),
            resume_tickets: Mutex::new(HashMap::from([
                ("primary".into(), 0),
                ("reviewer".into(), 0),
            ])),
            reply_types: Mutex::new(Vec::new()),
            prompts: Mutex::new(Vec::new()),
            policies: Mutex::new(Vec::new()),
            responses: Mutex::new(Vec::new()),
            interrupts: Mutex::new(Vec::new()),
            approval_mode: Mutex::new(Some("approve".into())),
            approval_mode_requests: AtomicUsize::new(0),
            deferred: None,
            deferred_replies: Mutex::new(HashMap::new()),
            events: Mutex::new(VecDeque::new()),
            executed_commands: Mutex::new(Vec::new()),
            verification_command_counts: Mutex::new(HashMap::new()),
            verification_behavior: VerificationBehavior::Pass,
            verification_item_type: None,
            marker_protocol: false,
            event_only_turn_items: false,
            start_error: None,
            lose_next_start_response: AtomicBool::new(false),
            participant_inventory: ParticipantInventory::Available,
            primary_runtime_status: Mutex::new(ThreadRuntimeStatus::Idle),
            participant_threads: Mutex::new(BTreeSet::from(["primary".to_owned()])),
            forks: Mutex::new(Vec::new()),
            goals: Mutex::new(BTreeMap::new()),
            fork_identity: ForkIdentity::Mirror,
            fork_history: ForkHistory::Exact,
            fork_runtime_status: ThreadRuntimeStatus::Idle,
            participant_status_calls: AtomicUsize::new(0),
            participant_failure_after_status_calls: None,
            remove_mirror_after_request: None,
        }
    }

    fn failing_start(detail: impl Into<String>) -> Self {
        let mut server = Self::new(conflict_free_replies());
        server.start_error = Some(detail.into());
        server
    }

    fn with_lost_first_start_response(self) -> Self {
        self.lose_next_start_response.store(true, Ordering::SeqCst);
        self
    }

    fn with_verification_behavior(mut self, behavior: VerificationBehavior) -> Self {
        self.verification_behavior = behavior;
        self
    }

    fn with_marker_protocol(mut self) -> Self {
        self.marker_protocol = true;
        self
    }

    fn with_verification_item(mut self, item_type: &'static str) -> Self {
        self.verification_item_type = Some(item_type);
        self
    }

    fn with_event_only_turn_items(mut self) -> Self {
        self.event_only_turn_items = true;
        self
    }

    fn with_participant_inventory(mut self, inventory: ParticipantInventory) -> Self {
        self.participant_inventory = inventory;
        self
    }

    fn with_primary_runtime_status(self, status: ThreadRuntimeStatus) -> Self {
        *self.primary_runtime_status.lock().unwrap() = status;
        self
    }

    fn set_primary_runtime_status(&self, status: ThreadRuntimeStatus) {
        *self.primary_runtime_status.lock().unwrap() = status;
    }

    fn without_primary_participant(self) -> Self {
        self.participant_threads.lock().unwrap().remove("primary");
        self
    }

    fn with_fork_identity(mut self, identity: ForkIdentity) -> Self {
        self.fork_identity = identity;
        self
    }

    fn with_fork_history(mut self, history: ForkHistory) -> Self {
        self.fork_history = history;
        self
    }

    fn with_fork_runtime_status(mut self, status: ThreadRuntimeStatus) -> Self {
        self.fork_runtime_status = status;
        self
    }

    fn with_source_goal(self, goal: Value) -> Self {
        self.goals
            .lock()
            .unwrap()
            .insert("primary".to_owned(), Some(goal));
        self
    }

    fn with_participant_failure_after_status_calls(mut self, calls: usize) -> Self {
        self.participant_failure_after_status_calls = Some(calls);
        self
    }

    fn with_remove_mirror_after_request(mut self, request: usize) -> Self {
        self.remove_mirror_after_request = Some(request);
        self
    }

    fn with_approval_mode(self, mode: Option<&str>) -> Self {
        *self.approval_mode.lock().unwrap() = mode.map(str::to_owned);
        self
    }

    fn remove_thread(&self, thread_id: &str) {
        assert!(self.threads.lock().unwrap().remove(thread_id).is_some());
        self.resume_tickets.lock().unwrap().remove(thread_id);
        self.participant_threads.lock().unwrap().remove(thread_id);
    }

    fn deferred(replies: Vec<Value>, request_number: usize, mode: DeferMode) -> Self {
        let mut server = Self::new(replies);
        server.deferred = Some((request_number, mode));
        server
    }

    fn request_order(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn method_order(&self) -> Vec<String> {
        self.method_order.lock().unwrap().clone()
    }

    fn resume_order(&self) -> Vec<String> {
        self.resumes.lock().unwrap().clone()
    }

    fn resume_policies(&self) -> Vec<ThreadResumePolicy> {
        self.resume_policies.lock().unwrap().clone()
    }

    fn resumes(&self) -> Vec<String> {
        self.resumes.lock().unwrap().clone()
    }

    fn forks(&self) -> Vec<(String, String, ThreadForkPolicy)> {
        self.forks.lock().unwrap().clone()
    }

    fn turn_ids(&self, thread_id: &str) -> Vec<String> {
        self.threads.lock().unwrap()[thread_id]
            .iter()
            .map(|turn| turn["id"].as_str().unwrap().to_owned())
            .collect()
    }

    fn reply_types(&self) -> Vec<String> {
        self.reply_types.lock().unwrap().clone()
    }

    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }

    fn policies(&self) -> Vec<TurnExecutionPolicy> {
        self.policies.lock().unwrap().clone()
    }

    fn responses(&self) -> Vec<Value> {
        self.responses.lock().unwrap().clone()
    }

    fn interrupts(&self) -> Vec<(String, String)> {
        self.interrupts.lock().unwrap().clone()
    }

    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn executed_commands(&self) -> Vec<Vec<String>> {
        self.executed_command_requests()
            .iter()
            .map(|request| request.command.clone())
            .collect()
    }

    fn approval_mode_request_count(&self) -> usize {
        self.approval_mode_requests.load(Ordering::SeqCst)
    }

    fn executed_command_requests(&self) -> Vec<CommandExecRequest> {
        self.executed_commands.lock().unwrap().clone()
    }

    fn verification_behavior_for_request(
        &self,
        verification_request_number: usize,
    ) -> VerificationBehavior {
        match self.verification_behavior {
            VerificationBehavior::MissingExecutionThenPass if verification_request_number == 1 => {
                VerificationBehavior::MissingExecution
            }
            VerificationBehavior::MissingExecutionThenPass => VerificationBehavior::Pass,
            VerificationBehavior::FailedExecutionThenPass if verification_request_number == 1 => {
                VerificationBehavior::FailedExecution
            }
            VerificationBehavior::FailedExecutionThenPass => VerificationBehavior::Pass,
            VerificationBehavior::MissingThenCargoUnavailableThenPass => {
                match verification_request_number {
                    1 => VerificationBehavior::MissingExecution,
                    2 => VerificationBehavior::CargoUnavailable,
                    _ => VerificationBehavior::Pass,
                }
            }
            VerificationBehavior::MissingThenCargoUnavailableThenMissingThenPass => {
                match verification_request_number {
                    1 | 3 => VerificationBehavior::MissingExecution,
                    2 => VerificationBehavior::CargoUnavailable,
                    _ => VerificationBehavior::Pass,
                }
            }
            VerificationBehavior::MissingThenCargoUnavailableThenMissing => {
                match verification_request_number {
                    2 => VerificationBehavior::CargoUnavailable,
                    _ => VerificationBehavior::MissingExecution,
                }
            }
            behavior => behavior,
        }
    }

    fn inject_completed_turn(&self, thread_id: &str, turn_id: &str, prompt: &str, reply: Value) {
        self.threads
            .lock()
            .unwrap()
            .get_mut(thread_id)
            .unwrap()
            .push(completed_turn(turn_id, prompt, &reply));
    }

    fn inject_interrupted_turn(&self, thread_id: &str, turn_id: &str, prompt: &str) {
        self.threads
            .lock()
            .unwrap()
            .get_mut(thread_id)
            .unwrap()
            .push(json!({
                "id": turn_id,
                "status": "interrupted",
                "items": [{
                    "id": format!("user-{turn_id}"),
                    "type": "userMessage",
                    "content": [{"type": "text", "text": prompt, "text_elements": []}]
                }]
            }));
    }

    fn append_turn_item(&self, thread_id: &str, turn_id: &str, item: Value) {
        let mut threads = self.threads.lock().unwrap();
        let turn = threads
            .get_mut(thread_id)
            .unwrap()
            .iter_mut()
            .find(|turn| turn.get("id").and_then(Value::as_str) == Some(turn_id))
            .unwrap();
        turn["items"].as_array_mut().unwrap().push(item);
    }

    fn insert_turn_item_before_agent(&self, thread_id: &str, turn_id: &str, item: Value) {
        let mut threads = self.threads.lock().unwrap();
        let turn = threads
            .get_mut(thread_id)
            .unwrap()
            .iter_mut()
            .find(|turn| turn.get("id").and_then(Value::as_str) == Some(turn_id))
            .unwrap();
        let items = turn["items"].as_array_mut().unwrap();
        let index = items
            .iter()
            .position(|item| item.get("type").and_then(Value::as_str) == Some("agentMessage"))
            .unwrap_or(items.len());
        items.insert(index, item);
    }

    fn set_turn_status(&self, thread_id: &str, turn_id: &str, status: &str) {
        let mut threads = self.threads.lock().unwrap();
        let turn = threads
            .get_mut(thread_id)
            .unwrap()
            .iter_mut()
            .find(|turn| turn.get("id").and_then(Value::as_str) == Some(turn_id))
            .unwrap();
        turn["status"] = json!(status);
    }

    fn set_user_prompt(&self, thread_id: &str, turn_id: &str, prompt: &str) {
        let mut threads = self.threads.lock().unwrap();
        let turn = threads
            .get_mut(thread_id)
            .unwrap()
            .iter_mut()
            .find(|turn| turn.get("id").and_then(Value::as_str) == Some(turn_id))
            .unwrap();
        let item = turn["items"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|item| item.get("type").and_then(Value::as_str) == Some("userMessage"))
            .unwrap();
        item["content"][0]["text"] = json!(prompt);
    }

    fn set_agent_text(&self, thread_id: &str, turn_id: &str, text: &str) {
        let mut threads = self.threads.lock().unwrap();
        let turn = threads
            .get_mut(thread_id)
            .unwrap()
            .iter_mut()
            .find(|turn| turn.get("id").and_then(Value::as_str) == Some(turn_id))
            .unwrap();
        let item = turn["items"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|item| item.get("type").and_then(Value::as_str) == Some("agentMessage"))
            .unwrap();
        item["text"] = json!(text);
    }

    fn set_patch_plugin_id(&self, thread_id: &str, turn_id: &str, plugin_id: Value) {
        let mut threads = self.threads.lock().unwrap();
        let turn = threads
            .get_mut(thread_id)
            .unwrap()
            .iter_mut()
            .find(|turn| turn.get("id").and_then(Value::as_str) == Some(turn_id))
            .unwrap();
        let item = turn["items"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|item| {
                item.get("type").and_then(Value::as_str) == Some("mcpToolCall")
                    && item.get("tool").and_then(Value::as_str) == Some("consensus_apply_patch")
            })
            .unwrap();
        item["pluginId"] = plugin_id;
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

    fn reject_deferred_patch_not_authorized(&self) {
        let (turn_id, reply) = self.patch_not_authorized_reply();
        self.deferred_replies.lock().unwrap().insert(turn_id, reply);
        self.complete_deferred_turns();
    }

    fn reject_deferred_patch_not_authorized_without_redundant_text(&self) {
        let (turn_id, mut reply) = self.patch_not_authorized_reply();
        let payload = reply["payload"].as_object_mut().unwrap();
        payload.remove("role");
        payload.remove("blocking_condition");
        self.deferred_replies.lock().unwrap().insert(turn_id, reply);
        self.complete_deferred_turns();
    }

    fn reject_deferred_patch_not_authorized_without_plan_hash(&self) {
        let (turn_id, mut reply) = self.patch_not_authorized_reply();
        reply["payload"]
            .as_object_mut()
            .unwrap()
            .remove("approved_plan_hash");
        self.deferred_replies.lock().unwrap().insert(turn_id, reply);
        self.complete_deferred_turns();
    }

    fn reject_deferred_patch_not_authorized_in_progress(&self) {
        let (turn_id, reply) = self.patch_not_authorized_reply();
        self.deferred_replies.lock().unwrap().remove(&turn_id);
        self.append_turn_item(
            "primary",
            &turn_id,
            json!({
                "id": format!("assistant-{turn_id}"),
                "type": "agentMessage",
                "text": serde_json::to_string(&reply).unwrap(),
                "phase": "final_answer"
            }),
        );
    }

    fn fail_deferred_patch_without_final(&self) {
        let (turn_id, _) = self.patch_not_authorized_reply();
        self.deferred_replies.lock().unwrap().remove(&turn_id);
    }

    fn patch_not_authorized_reply(&self) -> (String, Value) {
        let (turn_id, prompt) = {
            let mut threads = self.threads.lock().unwrap();
            let turn = threads
                .values_mut()
                .flat_map(|turns| turns.iter_mut())
                .find(|turn| {
                    turn.get("status").and_then(Value::as_str) == Some("inProgress")
                        && turn
                            .get("items")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .any(|item| {
                                item.get("type").and_then(Value::as_str) == Some("mcpToolCall")
                                    && item.get("tool").and_then(Value::as_str)
                                        == Some("consensus_apply_patch")
                            })
                })
                .expect("a deferred controlled patch turn");
            let turn_id = turn["id"].as_str().unwrap().to_owned();
            let prompt = turn["items"][0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_owned();
            let patch_item = turn["items"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|item| {
                    item.get("type").and_then(Value::as_str) == Some("mcpToolCall")
                        && item.get("tool").and_then(Value::as_str) == Some("consensus_apply_patch")
                })
                .unwrap();
            patch_item["status"] = json!("failed");
            (turn_id, prompt)
        };

        let metadata = prompt_json_block(&prompt, "Authoritative turn metadata:");
        let payload = prompt_json_block(&prompt, "Complete current payload");
        let delivery =
            prompt_json_block(&prompt, "Coordinator delivery identity for crash recovery:");
        let mut reply = patch_not_authorized_blocker();
        reply["payload"]["request_hash"] = delivery["request_hash"].clone();
        reply["payload"]["approved_plan_revision"] = metadata["plan_revision"].clone();
        reply["payload"]["approved_primary_sha"] = metadata["primary_sha"].clone();
        reply["payload"]["approved_reviewer_sha"] = metadata["reviewer_sha"].clone();
        reply["payload"]["approved_plan_hash"] = payload["approval"]["approved_plan_hash"].clone();
        reply["payload"]["resulting_integration_branch"] =
            payload["target_integration_branch"].clone();
        (turn_id, reply)
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
        if thread_id == "primary" {
            summary.status = runtime_status_json(*self.primary_runtime_status.lock().unwrap());
        } else if thread_id.contains("-consensus-mirror-") {
            summary.status = runtime_status_json(self.fork_runtime_status);
        }
        if turns
            .iter()
            .any(|turn| turn.get("status").and_then(Value::as_str) == Some("inProgress"))
        {
            let waiting_on_approval = turns.iter().any(|turn| {
                turn.get("status").and_then(Value::as_str) == Some("inProgress")
                    && turn
                        .get("items")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .any(|item| {
                            item.get("type").and_then(Value::as_str) == Some("mcpToolCall")
                                && matches!(
                                    item.get("status").and_then(Value::as_str),
                                    Some("inProgress" | "failed")
                                )
                        })
            });
            summary.status = json!({
                "type": "active",
                "activeFlags": if waiting_on_approval {
                    vec!["waitingOnApproval"]
                } else {
                    Vec::<&str>::new()
                }
            });
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
        Ok(InitializeInfo {
            codex_home: PathBuf::from("/home/test/.codex"),
            platform_family: "unix".into(),
            platform_os: "linux".into(),
            user_agent: "codex-cli/0.144.5".into(),
        })
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
        if !self.threads.lock().unwrap().contains_key(thread_id) {
            return Err(AppServerError::InvalidResponse(format!(
                "task {thread_id} is unavailable"
            )));
        }
        if thread_id.contains("-consensus-mirror-") {
            self.method_order
                .lock()
                .unwrap()
                .push(format!("thread/read-full:{thread_id}"));
            return Err(AppServerError::InvalidRequest(
                "ephemeral threads do not support includeTurns".to_owned(),
            ));
        }
        Ok(self.detail(thread_id))
    }

    async fn read_thread_summary(&self, thread_id: &str) -> Result<ThreadSummary, AppServerError> {
        self.method_order
            .lock()
            .unwrap()
            .push(format!("thread/read-summary:{thread_id}"));
        if !self.threads.lock().unwrap().contains_key(thread_id) {
            return Err(AppServerError::InvalidResponse(format!(
                "task {thread_id} is unavailable"
            )));
        }
        Ok(self.detail(thread_id).summary)
    }

    async fn resume_thread(
        &self,
        thread_id: &str,
        policy: &ThreadResumePolicy,
    ) -> Result<ThreadDetail, AppServerError> {
        self.method_order
            .lock()
            .unwrap()
            .push(format!("thread/resume:{thread_id}"));
        if thread_id.contains("-consensus-mirror-") {
            return Err(AppServerError::InvalidRequest(format!(
                "no rollout found for thread id {thread_id}"
            )));
        }
        self.resumes.lock().unwrap().push(thread_id.to_owned());
        self.resume_policies.lock().unwrap().push(policy.clone());
        if thread_id == "primary"
            && *self.primary_runtime_status.lock().unwrap() == ThreadRuntimeStatus::NotLoaded
        {
            if let ThreadResumePolicy::Participant(ParticipantMcpConfig { .. }) = policy {
                self.participant_threads
                    .lock()
                    .unwrap()
                    .insert(thread_id.to_owned());
            }
            *self.primary_runtime_status.lock().unwrap() = ThreadRuntimeStatus::Idle;
        }
        *self
            .resume_tickets
            .lock()
            .unwrap()
            .entry(thread_id.to_owned())
            .or_default() += 1;
        Ok(self.detail(thread_id))
    }

    async fn fork_thread(
        &self,
        source_thread_id: &str,
        policy: &ThreadForkPolicy,
    ) -> Result<ThreadDetail, AppServerError> {
        let fork_number = self.forks.lock().unwrap().len() + 1;
        let effective_thread_id = match self.fork_identity {
            ForkIdentity::Mirror => {
                format!("{source_thread_id}-consensus-mirror-{fork_number}")
            }
            ForkIdentity::Source => source_thread_id.to_owned(),
            ForkIdentity::Reviewer => "reviewer".to_owned(),
        };
        let mut source_turns = self
            .threads
            .lock()
            .unwrap()
            .get(source_thread_id)
            .cloned()
            .ok_or_else(|| AppServerError::InvalidResponse("source task missing".to_owned()))?;
        match self.fork_history {
            ForkHistory::Exact => {}
            ForkHistory::MissingLast => {
                source_turns.pop();
            }
            ForkHistory::Reversed => source_turns.reverse(),
        }
        self.threads
            .lock()
            .unwrap()
            .insert(effective_thread_id.clone(), source_turns);
        self.resume_tickets
            .lock()
            .unwrap()
            .insert(effective_thread_id.clone(), 0);
        self.participant_threads
            .lock()
            .unwrap()
            .insert(effective_thread_id.clone());
        self.forks.lock().unwrap().push((
            source_thread_id.to_owned(),
            effective_thread_id.clone(),
            policy.clone(),
        ));
        self.method_order.lock().unwrap().push(format!(
            "thread/fork:{source_thread_id}:{effective_thread_id}"
        ));
        Ok(self.detail(&effective_thread_id))
    }

    async fn get_thread_goal(&self, thread_id: &str) -> Result<Option<Value>, AppServerError> {
        self.method_order
            .lock()
            .unwrap()
            .push(format!("thread/goal/get:{thread_id}"));
        Ok(self.goals.lock().unwrap().get(thread_id).cloned().flatten())
    }

    async fn list_mcp_server_status(
        &self,
        thread_id: &str,
    ) -> Result<Vec<McpServerStatus>, AppServerError> {
        self.method_order
            .lock()
            .unwrap()
            .push(format!("mcpServerStatus/list:{thread_id}"));
        let status_call = self.participant_status_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self
            .participant_failure_after_status_calls
            .is_some_and(|allowed| status_call > allowed)
        {
            return Ok(vec![McpServerStatus {
                name: "unrelatedServer".to_owned(),
                tools: BTreeMap::new(),
            }]);
        }
        if !self.participant_threads.lock().unwrap().contains(thread_id) {
            return Ok(vec![McpServerStatus {
                name: "unrelatedServer".to_owned(),
                tools: BTreeMap::new(),
            }]);
        }
        let patch_definition = match self.participant_inventory {
            ParticipantInventory::MalformedDefinition => json!("not-an-object"),
            _ => json!({"inputSchema": {"type": "object"}}),
        };
        let mut tools = BTreeMap::new();
        if !matches!(
            self.participant_inventory,
            ParticipantInventory::MissingTool
        ) {
            tools.insert(PARTICIPANT_PATCH_TOOL.to_owned(), patch_definition);
        }
        if matches!(self.participant_inventory, ParticipantInventory::ExtraTool) {
            tools.insert(
                "unexpected_patch_tool".to_owned(),
                json!({"inputSchema": {"type": "object"}}),
            );
        }
        match self.participant_inventory {
            ParticipantInventory::MissingServer => Ok(vec![McpServerStatus {
                name: "unrelatedServer".to_owned(),
                tools,
            }]),
            ParticipantInventory::StatusUnavailable => Err(AppServerError::IncompatibleCodex(
                "required App Server method mcpServerStatus/list is unavailable".to_owned(),
            )),
            _ => Ok(vec![McpServerStatus {
                name: PARTICIPANT_MCP_SERVER.to_owned(),
                tools,
            }]),
        }
    }

    async fn start_turn(
        &self,
        thread_id: &str,
        prompt: &str,
        policy: &TurnExecutionPolicy,
    ) -> Result<TurnHandle, AppServerError> {
        if !thread_id.contains("-consensus-mirror-") {
            let mut resume_tickets = self.resume_tickets.lock().unwrap();
            let resume_ticket = resume_tickets.get_mut(thread_id).unwrap();
            assert!(
                *resume_ticket > 0,
                "task {thread_id} must be resumed before turn/start"
            );
            *resume_ticket -= 1;
        }
        if let Some(detail) = &self.start_error {
            return Err(AppServerError::InvalidResponse(detail.clone()));
        }
        let action = prompt_action(prompt);
        self.method_order
            .lock()
            .unwrap()
            .push(format!("turn/start:{thread_id}:{action}"));
        let verification_request_number = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(format!("{thread_id}:{action}"));
            requests
                .iter()
                .filter(|request| request.ends_with("REQUEST_PRIMARY_VERIFICATION"))
                .count()
        };
        let verification_behavior =
            self.verification_behavior_for_request(verification_request_number);
        self.prompts.lock().unwrap().push(prompt.to_owned());
        self.policies.lock().unwrap().push(policy.clone());
        let mut reply = if action == "REQUEST_PRIMARY_VERIFICATION" {
            if verification_behavior == VerificationBehavior::CargoUnavailable {
                json!(
                    "<consensus-result>BLOCKED:CARGO_UNAVAILABLE</consensus-result>\n\nCargo is unavailable."
                )
            } else if self.marker_protocol {
                json!(
                    "<consensus-result>VERIFICATION_READY</consensus-result>\n\nAll frozen commands completed."
                )
            } else {
                verification_reply(prompt, verification_behavior)
            }
        } else {
            self.replies.lock().unwrap().pop_front().unwrap()
        };
        if action == "REQUEST_REVIEWER_PLAN_VERDICT" && reply["message_type"] == "APPROVED_PLAN" {
            reply["payload"]["approved_plan_hash"] =
                prompt_json_block(prompt, "Complete current payload")["plan_hash"].clone();
        }
        if action == "REQUEST_PRIMARY_INTEGRATION"
            && reply["message_type"] == "BLOCKED"
            && reply["reason_code"] == "EXECUTION_TOOL_UNAVAILABLE"
        {
            let metadata = prompt_json_block(prompt, "Authoritative turn metadata:");
            let payload = prompt_json_block(prompt, "Complete current payload");
            let delivery =
                prompt_json_block(prompt, "Coordinator delivery identity for crash recovery:");
            reply["payload"]["request_hash"] = delivery["request_hash"].clone();
            reply["payload"]["approved_plan_revision"] = metadata["plan_revision"].clone();
            reply["payload"]["approved_primary_sha"] = metadata["primary_sha"].clone();
            reply["payload"]["approved_reviewer_sha"] = metadata["reviewer_sha"].clone();
            reply["payload"]["approved_plan_hash"] =
                payload["approval"]["approved_plan_hash"].clone();
            reply["payload"]["target_integration_branch"] =
                payload["target_integration_branch"].clone();
        }
        if action == "REQUEST_PRIMARY_INTEGRATION"
            && reply["message_type"] == "BLOCKED"
            && reply["reason_code"] == "FILE_CHANGE_TOOL_UNAVAILABLE"
        {
            let metadata = prompt_json_block(prompt, "Authoritative turn metadata:");
            let payload = prompt_json_block(prompt, "Complete current payload");
            let delivery =
                prompt_json_block(prompt, "Coordinator delivery identity for crash recovery:");
            reply["payload"]["request_hash"] = delivery["request_hash"].clone();
            reply["payload"]["approved_plan_revision"] = metadata["plan_revision"].clone();
            reply["payload"]["approved_primary_sha"] = metadata["primary_sha"].clone();
            reply["payload"]["approved_reviewer_sha"] = metadata["reviewer_sha"].clone();
            reply["payload"]["approved_plan_hash"] =
                payload["approval"]["approved_plan_hash"].clone();
            reply["payload"]["resulting_integration_branch"] =
                payload["target_integration_branch"].clone();
        }
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
        let mut turn = match deferred_mode {
            Some(DeferMode::Interrupted | DeferMode::InterruptedCommand) => json!({
                "id": turn_id,
                "status": "interrupted",
                "items": [{
                    "id": format!("user-{turn_id}"),
                    "type": "userMessage",
                    "content": [{"type": "text", "text": prompt, "text_elements": []}]
                }]
            }),
            Some(_) => {
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
            }
            None => completed_turn(&turn_id, prompt, &reply),
        };
        if deferred_mode == Some(DeferMode::InterruptedCommand) {
            turn["items"].as_array_mut().unwrap().push(json!({
                "id": format!("command-{turn_id}"),
                "type": "commandExecution",
                "command": "git status --short",
                "cwd": "/repo/primary",
                "status": "completed",
                "exitCode": 0
            }));
        }
        if deferred_mode == Some(DeferMode::PatchApproval) {
            let metadata = prompt_json_block(prompt, "Authoritative turn metadata:");
            let delivery =
                prompt_json_block(prompt, "Coordinator delivery identity for crash recovery:");
            turn["items"].as_array_mut().unwrap().push(json!({
                "id": format!("patch-{turn_id}"),
                "type": "mcpToolCall",
                "pluginId": "worktree-merge-consensus@worktree-merge-consensus",
                "server": PARTICIPANT_MCP_SERVER,
                "tool": "consensus_apply_patch",
                "arguments": {
                    "run_id": metadata["run_id"],
                    "request_hash": delivery["request_hash"],
                    "patch": "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n"
                },
                "status": "inProgress",
                "appContext": null
            }));
        }
        if action == "REQUEST_PRIMARY_VERIFICATION" {
            if let Some(item_type) = self.verification_item_type {
                let items = turn["items"].as_array_mut().unwrap();
                let agent = items.pop().unwrap();
                items.push(json!({
                    "id": format!("participant-side-effect-{turn_id}"),
                    "type": item_type,
                }));
                items.push(agent);
            }
        }
        if action == "REQUEST_PRIMARY_VERIFICATION"
            && self.event_only_turn_items
            && turn.get("status").and_then(Value::as_str) == Some("completed")
        {
            let full_turn = turn.clone();
            let mut events = self.events.lock().unwrap();
            for item in full_turn["items"].as_array().unwrap() {
                events.push_back(AppEvent {
                    id: None,
                    method: "item/completed".into(),
                    params: json!({
                        "threadId": thread_id,
                        "turnId": turn_id,
                        "item": item,
                    }),
                });
            }
            events.push_back(AppEvent {
                id: None,
                method: "turn/completed".into(),
                params: json!({
                    "threadId": thread_id,
                    "turn": full_turn,
                }),
            });
            turn["items"].as_array_mut().unwrap().retain(|item| {
                matches!(
                    item.get("type").and_then(Value::as_str),
                    Some("userMessage" | "agentMessage")
                )
            });
        }
        if thread_id.contains("-consensus-mirror-")
            && turn.get("status").and_then(Value::as_str) == Some("completed")
        {
            let full_turn = turn.clone();
            let mut events = self.events.lock().unwrap();
            for item in full_turn["items"].as_array().unwrap() {
                events.push_back(AppEvent {
                    id: None,
                    method: "item/completed".into(),
                    params: json!({
                        "threadId": thread_id,
                        "turnId": turn_id,
                        "item": item,
                    }),
                });
            }
            events.push_back(AppEvent {
                id: None,
                method: "turn/completed".into(),
                params: json!({
                    "threadId": thread_id,
                    "turn": full_turn,
                }),
            });
        }
        self.threads
            .lock()
            .unwrap()
            .get_mut(thread_id)
            .unwrap()
            .push(turn);
        if self.remove_mirror_after_request == Some(request_number) {
            let mirror_id = self
                .forks
                .lock()
                .unwrap()
                .last()
                .map(|(_, effective, _)| effective.clone());
            if let Some(mirror_id) = mirror_id {
                self.threads.lock().unwrap().remove(&mirror_id);
                self.resume_tickets.lock().unwrap().remove(&mirror_id);
                self.participant_threads.lock().unwrap().remove(&mirror_id);
            }
        }
        if deferred_mode == Some(DeferMode::UserInput) {
            self.events.lock().unwrap().push_back(AppEvent {
                id: Some(json!(1)),
                method: "item/tool/requestUserInput".into(),
                params: json!({"threadId": thread_id, "turnId": turn_id}),
            });
        } else if deferred_mode == Some(DeferMode::ForbiddenCommand) {
            self.events.lock().unwrap().push_back(AppEvent {
                id: Some(json!(1)),
                method: "item/commandExecution/requestApproval".into(),
                params: json!({
                    "threadId": thread_id,
                    "turnId": turn_id,
                    "cwd": "/repo/primary",
                    "command": "git push origin HEAD"
                }),
            });
        } else if deferred_mode == Some(DeferMode::FileGrantRoot) {
            self.events.lock().unwrap().push_back(AppEvent {
                id: Some(json!(1)),
                method: "item/fileChange/requestApproval".into(),
                params: json!({
                    "threadId": thread_id,
                    "turnId": turn_id,
                    "grantRoot": "/repo"
                }),
            });
        }
        if self.lose_next_start_response.swap(false, Ordering::SeqCst) {
            return Err(AppServerError::InvalidResponse(
                "turn/start response lost after server committed the turn".into(),
            ));
        }
        Ok(TurnHandle {
            id: turn_id,
            status: "completed".into(),
            items: Vec::new(),
        })
    }

    async fn interrupt_turn(&self, thread_id: &str, turn_id: &str) -> Result<(), AppServerError> {
        self.interrupts
            .lock()
            .unwrap()
            .push((thread_id.to_owned(), turn_id.to_owned()));
        self.set_turn_status(thread_id, turn_id, "interrupted");
        Ok(())
    }

    async fn execute_command(
        &self,
        request: &CommandExecRequest,
    ) -> Result<CommandExecResult, AppServerError> {
        let verification_request_number = self
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|entry| entry.ends_with("REQUEST_PRIMARY_VERIFICATION"))
            .count();
        let command_index = {
            let mut counts = self.verification_command_counts.lock().unwrap();
            let count = counts.entry(verification_request_number).or_default();
            let index = *count;
            *count += 1;
            index
        };
        self.executed_commands.lock().unwrap().push(request.clone());
        let behavior = self.verification_behavior_for_request(verification_request_number);
        let failed = behavior == VerificationBehavior::FailedExecution && command_index == 0;
        Ok(CommandExecResult {
            exit_code: if failed { 1 } else { 0 },
            stdout: if failed {
                "a machine-derived compiler diagnostic\n".repeat(1_000)
            } else {
                String::new()
            },
            stderr: String::new(),
        })
    }

    async fn controlled_patch_approval_mode(&self) -> Result<Option<String>, AppServerError> {
        self.approval_mode_requests.fetch_add(1, Ordering::SeqCst);
        Ok(self.approval_mode.lock().unwrap().clone())
    }

    async fn next_event(&self) -> Option<AppEvent> {
        self.events.lock().unwrap().pop_front()
    }

    async fn respond_to_request(&self, _id: Value, result: Value) -> Result<(), AppServerError> {
        self.responses.lock().unwrap().push(result);
        Ok(())
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
                "text": reply_text(reply),
                "phase": "final_answer"
            }
        ]
    })
}

fn reply_text(reply: &Value) -> String {
    reply
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| serde_json::to_string(reply).unwrap())
}

async fn wait_for_request(app: &FakeAppServer) {
    wait_for_request_count(app, 1).await;
}

async fn wait_for_request_count(app: &FakeAppServer, count: usize) {
    for _ in 0..500 {
        if app.request_count() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("fake App Server never received {count} turns");
}

async fn wait_for_bound_pending_send(
    store: &SqliteRunStore,
) -> consensus_daemon::store::PendingSend {
    let mut last_state = None;
    let mut last_pending = None;
    for _ in 0..500 {
        let state = store.load_run(RUN_ID).unwrap().unwrap();
        let pending = store.pending_send(RUN_ID).unwrap();
        if let Some(pending) = pending.as_ref() {
            if state.status == RunStatus::Running
                && state.phase == Phase::Integrate
                && state.next_action == NextAction::RequestPrimaryIntegration
                && pending.thread_id.is_some()
                && pending.turn_id.is_some()
            {
                return pending.clone();
            }
        }
        last_state = Some(state);
        last_pending = pending;
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!(
        "coordinator never persisted the pending turn identity: state={last_state:?} pending={last_pending:?}"
    );
}

fn summary(thread_id: &str) -> ThreadSummary {
    ThreadSummary {
        id: thread_id.into(),
        cwd: PathBuf::from("/unrelated/non-git/task-home"),
        name: Some(thread_id.into()),
        preview: String::new(),
        cli_version: "0.144.5".into(),
        created_at: 0,
        updated_at: 0,
        status: json!({"type": "idle"}),
        source: json!({}),
    }
}

fn runtime_status_json(status: ThreadRuntimeStatus) -> Value {
    match status {
        ThreadRuntimeStatus::NotLoaded => json!({"type": "notLoaded"}),
        ThreadRuntimeStatus::Idle => json!({"type": "idle"}),
        ThreadRuntimeStatus::Active => json!({"type": "active", "activeFlags": []}),
        ThreadRuntimeStatus::SystemError => json!({"type": "systemError"}),
    }
}

fn prompt_action(prompt: &str) -> &'static str {
    for action in [
        "REQUEST_PRIMARY_CONTRACT",
        "REQUEST_REVIEWER_CONTRACT",
        "REQUEST_PRIMARY_PLAN",
        "REQUEST_REVIEWER_PLAN_VERDICT",
        "REQUEST_PRIMARY_INTEGRATION",
        "REQUEST_PRIMARY_VERIFICATION",
        "REQUEST_REVIEWER_RESULT_VERDICT",
    ] {
        if prompt.contains(action) {
            return action;
        }
    }
    panic!("prompt did not contain a known action")
}

fn assert_primary_turns_have_exact_preflight(app: &FakeAppServer, thread_id: &str) {
    let methods = app.method_order();
    let turn_prefix = format!("turn/start:{thread_id}:REQUEST_PRIMARY");
    let resume = format!("thread/resume:{thread_id}");
    let summary_read = format!("thread/read-summary:{thread_id}");
    let inventory = format!("mcpServerStatus/list:{thread_id}");
    let primary_turns = methods
        .iter()
        .enumerate()
        .filter(|(_, method)| method.starts_with(&turn_prefix))
        .collect::<Vec<_>>();
    assert!(!primary_turns.is_empty());
    for (index, _) in primary_turns {
        assert!(index >= 2);
        if thread_id.contains("-consensus-mirror-") {
            assert_eq!(methods[index - 2], inventory);
            assert_eq!(methods[index - 1], summary_read);
        } else {
            assert_eq!(methods[index - 2], resume);
            assert_eq!(methods[index - 1], inventory);
        }
    }
}

fn prompt_json_block(prompt: &str, heading: &str) -> Value {
    let start = prompt.find(heading).unwrap();
    let after_heading = &prompt[start + heading.len()..];
    let fence = after_heading.find("```json").unwrap();
    let json_start = fence + "```json".len();
    let fenced = &after_heading[json_start..];
    let json_end = fenced.find("```").unwrap();
    serde_json::from_str(fenced[..json_end].trim()).unwrap()
}

fn verification_reply(prompt: &str, behavior: VerificationBehavior) -> Value {
    let metadata = prompt_json_block(prompt, "Authoritative turn metadata:");
    let payload = prompt_json_block(prompt, "Complete current payload");
    let mut tests = payload["required_test_commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|command| json!({"command": command, "exit_code": 0}))
        .collect::<Vec<_>>();
    match behavior {
        VerificationBehavior::EmptyReport => tests.clear(),
        VerificationBehavior::LegacyReport => {
            tests = payload["required_test_commands"]
                .as_array()
                .unwrap()
                .iter()
                .map(|command| json!({"command": command, "status": "passed"}))
                .collect();
        }
        VerificationBehavior::Pass
        | VerificationBehavior::FailedExecution
        | VerificationBehavior::FailedExecutionThenPass
        | VerificationBehavior::CargoUnavailable
        | VerificationBehavior::MissingExecution
        | VerificationBehavior::MissingExecutionThenPass
        | VerificationBehavior::MissingThenCargoUnavailableThenPass
        | VerificationBehavior::MissingThenCargoUnavailableThenMissing
        | VerificationBehavior::MissingThenCargoUnavailableThenMissingThenPass
        | VerificationBehavior::RewriteIntegrationEvidence => {}
    }
    let integration_evidence =
        if matches!(behavior, VerificationBehavior::RewriteIntegrationEvidence) {
            json!({"summary": "forged verification replacement"})
        } else {
            payload["integration_evidence"].clone()
        };
    message(
        "INTEGRATION_READY",
        "VERIFY",
        metadata["round"].as_u64().unwrap() as u32,
        metadata["plan_revision"].as_u64().map(|value| value as u32),
        metadata["integration_branch"].as_str(),
        metadata["integration_sha"].as_str(),
        json!({
            "changed_files": payload["changed_files"],
            "integration_evidence": integration_evidence,
            "verification_summary": "All frozen commands completed.",
            "test_evidence": tests
        }),
    )
}

// Retained for Task 4's legacy verification migration fixtures.
#[allow(dead_code)]
fn append_verification_command_items(
    turn: &mut Value,
    prompt: &str,
    behavior: VerificationBehavior,
) {
    if matches!(
        behavior,
        VerificationBehavior::MissingExecution | VerificationBehavior::CargoUnavailable
    ) {
        return;
    }
    let payload = prompt_json_block(prompt, "Complete current payload");
    let cwd = payload["verification_worktree"].clone();
    let items = turn["items"].as_array_mut().unwrap();
    let Some(assistant_index) = items
        .iter()
        .position(|item| item.get("type").and_then(Value::as_str) == Some("agentMessage"))
    else {
        return;
    };
    let assistant = items.remove(assistant_index);
    for (index, command) in payload["required_test_commands"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let command = command.as_str().unwrap();
        let app_server_command = format!("/bin/bash -lc {}", shell_words::quote(command));
        items.push(json!({
            "id": format!("test-command-{}", index + 1),
            "type": "commandExecution",
            "command": app_server_command,
            "commandActions": [],
            "cwd": cwd,
            "status": "completed",
            "exitCode": if matches!(behavior, VerificationBehavior::FailedExecution) { 1 } else { 0 },
            "aggregatedOutput": if matches!(behavior, VerificationBehavior::FailedExecution) {
                "a machine-derived compiler diagnostic"
            } else {
                ""
            },
            "source": "agent"
        }));
    }
    items.push(assistant);
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
        participant_mcp_executable: participant_mcp_executable(),
    }
}

fn legacy_migration_options() -> CoordinatorOptions {
    CoordinatorOptions {
        wait_timeout: Duration::from_millis(15),
        poll_interval: Duration::from_millis(1),
        communication_attempts: 1,
        participant_mcp_executable: participant_mcp_executable(),
    }
}

fn participant_mcp_executable() -> PathBuf {
    PathBuf::from("/test/bin/codex-consensus")
}

struct LegacyMigrationSeed {
    blocked: RunState,
    verification_request_hash: String,
    integration_request_hash: String,
}

async fn seed_legacy_unattended_verification_history(
    coordinator: &Coordinator<FakeAppServer, RecordingSafety>,
    app: &FakeAppServer,
    store: &SqliteRunStore,
    archived_signals: [&str; 3],
    final_item: Option<Value>,
) -> LegacyMigrationSeed {
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();
    let paused = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(paused.status, RunStatus::PausedUserAction);
    assert_eq!(paused.phase, Phase::Verify);
    assert_eq!(paused.next_action, NextAction::RequestPrimaryVerification);
    assert_eq!(paused.reason_code.as_deref(), Some("COMMUNICATION_FAILURE"));

    let pending = store.pending_send(RUN_ID).unwrap().unwrap();
    let request_hash = pending.message_hash.clone();
    assert_eq!(pending.thread_id.as_deref(), Some("primary"));
    assert_eq!(pending.turn_id.as_deref(), Some("turn-6"));
    let prompt = legacy_request_prompt(&request_hash);

    app.set_turn_status("primary", "turn-6", "completed");
    app.append_turn_item(
        "primary",
        "turn-6",
        legacy_agent_item("turn-6", archived_signals[0]),
    );
    let mut active = paused;
    active.resume().unwrap();
    store.save_state(&active).unwrap();
    store
        .reset_completed_read_only_turn_for_retry(
            RUN_ID,
            &request_hash,
            "primary",
            "turn-6",
            "completed",
        )
        .unwrap();

    store
        .record_turn_started(RUN_ID, &request_hash, "primary", "legacy-verification-2")
        .unwrap();
    app.inject_completed_turn(
        "primary",
        "legacy-verification-2",
        &prompt,
        legacy_verification_reply(archived_signals[1]),
    );
    store
        .reset_completed_read_only_turn_for_retry(
            RUN_ID,
            &request_hash,
            "primary",
            "legacy-verification-2",
            "completed",
        )
        .unwrap();

    store
        .record_turn_started(RUN_ID, &request_hash, "primary", "legacy-verification-3")
        .unwrap();
    app.inject_completed_turn(
        "primary",
        "legacy-verification-3",
        &prompt,
        legacy_verification_reply(archived_signals[2]),
    );
    let mut compatibility_blocked = active;
    record_missing_verification_diagnostic(&mut compatibility_blocked);
    compatibility_blocked.block("TEST_FAILURE");
    store.save_state(&compatibility_blocked).unwrap();
    let mut compatibility_resumed = compatibility_blocked.clone();
    compatibility_resumed
        .retry_blocked_verification_without_execution()
        .unwrap();
    store
        .reactivate_blocked_run_with_verification_evidence_retry(
            &compatibility_blocked,
            &compatibility_resumed,
            &request_hash,
            "primary",
            "legacy-verification-3",
            "completed",
        )
        .unwrap();

    store
        .record_turn_started(RUN_ID, &request_hash, "primary", "legacy-verification-4")
        .unwrap();
    app.inject_completed_turn(
        "primary",
        "legacy-verification-4",
        &prompt,
        legacy_verification_reply("ready"),
    );
    if let Some(item) = final_item {
        app.insert_turn_item_before_agent("primary", "legacy-verification-4", item);
    }
    let integration_request_hash = "successful-integration-request".to_owned();
    store
        .record_successful_patch(RUN_ID, &integration_request_hash, "successful-patch-hash")
        .unwrap();
    let mut blocked = compatibility_resumed;
    record_missing_verification_diagnostic(&mut blocked);
    blocked.block("TEST_FAILURE");
    store.save_state(&blocked).unwrap();

    LegacyMigrationSeed {
        blocked,
        verification_request_hash: request_hash,
        integration_request_hash,
    }
}

fn legacy_request_prompt(request_hash: &str) -> String {
    format!(
        "Legacy verification request.\n\nCoordinator delivery identity for crash recovery:\n```json\n{{\"request_hash\":\"{request_hash}\"}}\n```\n"
    )
}

fn legacy_verification_reply(signal: &str) -> Value {
    match signal {
        "ready" => json!(
            "<consensus-result>VERIFICATION_READY</consensus-result>\n\nLegacy verification completed."
        ),
        "cargo-unavailable" => json!(
            "<consensus-result>BLOCKED:CARGO_UNAVAILABLE</consensus-result>\n\nCargo was unavailable."
        ),
        other => panic!("unsupported legacy verification signal {other}"),
    }
}

fn legacy_agent_item(turn_id: &str, signal: &str) -> Value {
    json!({
        "id": format!("assistant-{turn_id}"),
        "type": "agentMessage",
        "text": reply_text(&legacy_verification_reply(signal)),
        "phase": "final_answer"
    })
}

fn record_missing_verification_diagnostic(state: &mut RunState) {
    state.record_error(RunDiagnostic {
        code: "TEST_FAILURE".into(),
        detail: "verification must execute each frozen command exactly once and no other command"
            .into(),
        operation: None,
        action: NextAction::RequestPrimaryVerification,
        role: Some(Role::Primary),
        thread_id: Some("primary".into()),
        source_thread_id: None,
        effective_thread_id: None,
        participant_binding_generation: None,
        participant_binding_mode: None,
        participant_server: None,
    });
}

fn record_database_completion_collision_diagnostic(state: &mut RunState, thread_id: &str) {
    state.record_error(RunDiagnostic {
        code: "DATABASE_ERROR".into(),
        detail: "database error: UNIQUE constraint failed: turn_event_completions.turn_record_id"
            .into(),
        operation: None,
        action: NextAction::RequestPrimaryVerification,
        role: Some(Role::Primary),
        thread_id: Some(thread_id.into()),
        source_thread_id: None,
        effective_thread_id: None,
        participant_binding_generation: None,
        participant_binding_mode: None,
        participant_server: None,
    });
}

fn start_request() -> StartRequest {
    StartRequest {
        integration_branch: Some("consensus/test-run".into()),
        test_commands: vec!["cargo test --workspace".into()],
    }
}

async fn seed_corrective_patch_tool_blocker(
    safety: Arc<CorrectiveRecoverySafety>,
) -> (
    tempfile::TempDir,
    Coordinator<FakeAppServer, CorrectiveRecoverySafety>,
    Arc<FakeAppServer>,
    SqliteRunStore,
    RunState,
) {
    let temp = tempfile::tempdir().unwrap();
    let store_path = temp.path().join("state.db");
    let store = SqliteRunStore::open(&store_path).unwrap();
    let app = Arc::new(
        FakeAppServer::new(corrective_recovery_replies())
            .with_verification_behavior(VerificationBehavior::FailedExecutionThenPass),
    );
    let coordinator = Coordinator::new(Arc::clone(&app), store.clone(), safety, fast_options());
    coordinator
        .start(fixture_run(), start_request())
        .await
        .unwrap();
    let blocked = coordinator.drive(RUN_ID).await.unwrap();
    assert_eq!(blocked.status, RunStatus::Blocked);
    assert_eq!(
        blocked.reason_code.as_deref(),
        Some("CONTROLLED_PATCH_TOOL_UNAVAILABLE")
    );
    assert_eq!(blocked.phase, Phase::Blocked);
    assert_eq!(blocked.next_action, NextAction::Stop);
    assert_eq!(blocked.round, 2);
    assert_eq!(
        blocked.integration_branch.as_deref(),
        Some("consensus/test-run")
    );
    assert_eq!(blocked.integration_sha.as_deref(), Some(INTEGRATION_SHA));
    assert!(blocked.test_evidence.iter().any(|item| item.exit_code != 0));
    assert!(blocked.accepted_result.is_none());
    Connection::open(store_path)
        .unwrap()
        .execute(
            "UPDATE turns
             SET capability_generation = ?1,
                 participant_binding_generation = NULL
             WHERE id = (
                 SELECT id FROM turns
                 WHERE run_id = ?2 AND delivery_state = 'ACCEPTED'
                   AND role = 'PRIMARY' AND phase = 'INTEGRATE'
                 ORDER BY id DESC LIMIT 1
             )",
            params![
                consensus_daemon::store::LEGACY_PARTICIPANT_CAPABILITY_GENERATION,
                RUN_ID
            ],
        )
        .unwrap();
    (temp, coordinator, app, store, blocked)
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
            json!({
                "role": "PRIMARY",
                "contract": {
                    "items": ["primary-feature"],
                    "tests": ["cargo test --workspace"]
                }
            }),
        ),
        message(
            "CONTRACT_READY",
            "CONTRACT",
            1,
            None,
            None,
            None,
            json!({
                "role": "REVIEWER",
                "contract": {
                    "items": ["reviewer-feature"],
                    "tests": ["cargo test --workspace"]
                }
            }),
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
                ],
                "test_commands": ["cargo test --workspace"]
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
                "integration_evidence": {"summary": "both features are present"}
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

fn corrective_recovery_replies() -> Vec<Value> {
    let mut replies = conflict_free_replies();
    let mut result_approval = replies.pop().unwrap();
    result_approval["round"] = json!(2);
    result_approval["integration_sha"] = json!(CORRECTED_INTEGRATION_SHA);
    result_approval["payload"]["approved_integration_sha"] = json!(CORRECTED_INTEGRATION_SHA);
    replies.extend([
        json!(
            "<consensus-result>BLOCKED:CONTROLLED_PATCH_TOOL_UNAVAILABLE</consensus-result>\n\nThe participant controlled patch tool was unavailable before any action."
        ),
        json!(
            "<consensus-result>INTEGRATION_READY</consensus-result>\n\nThe failed verification diagnostic was corrected in one new commit."
        ),
        result_approval,
    ]);
    replies
}

fn marker_replies() -> Vec<Value> {
    vec![
        json!(
            "<consensus-result>CONTRACT_READY</consensus-result>\n{\"items\":[\"primary-feature\"],\"tests\":[\"cargo test --workspace\"]}"
        ),
        json!(
            "<consensus-result>CONTRACT_READY</consensus-result>\n```json\n{\"items\":[\"reviewer-feature\"],\"tests\":[\"cargo test --workspace\"]}\n```"
        ),
        json!(
            "<consensus-result>PLAN_READY</consensus-result>\n\n## Integration plan\n\nPreserve both implementations, resolve the shared parser deliberately, and run every frozen test."
        ),
        json!(
            "The proposal covers both contracts.\n\n<consensus-result>APPROVED</consensus-result>"
        ),
        json!(
            "<consensus-result>INTEGRATION_READY</consensus-result>\n\nBoth frozen implementations are present."
        ),
        json!(
            "<consensus-result>APPROVED</consensus-result>\n\nThe exact tested result preserves both contracts."
        ),
    ]
}

fn execution_tool_unavailable_blocker() -> Value {
    let mut blocked = message(
        "BLOCKED",
        "INTEGRATE",
        1,
        Some(1),
        None,
        None,
        json!({
            "role": "PRIMARY",
            "request_hash": "filled-from-prompt",
            "approved_plan_revision": 1,
            "approved_primary_sha": PRIMARY_SHA,
            "approved_reviewer_sha": REVIEWER_SHA,
            "approved_plan_hash": "filled-from-prompt",
            "target_integration_branch": "consensus/test-run",
            "evidence": ["the local execution tool was unavailable"],
            "writes_performed": false,
            "branch_created": false,
            "merge_performed": false,
            "files_modified": [],
            "tests_run": [],
            "safety_state": ["no integration branch was created"]
        }),
    );
    blocked["reason_code"] = json!("EXECUTION_TOOL_UNAVAILABLE");
    blocked
}

fn file_change_tool_unavailable_blocker() -> Value {
    let mut blocked = message(
        "BLOCKED",
        "INTEGRATE",
        1,
        Some(1),
        None,
        None,
        json!({
            "role": "PRIMARY",
            "request_hash": "filled-from-prompt",
            "approved_plan_revision": 1,
            "approved_primary_sha": PRIMARY_SHA,
            "approved_reviewer_sha": REVIEWER_SHA,
            "approved_plan_hash": "filled-from-prompt",
            "resulting_integration_branch": "consensus/test-run",
            "resulting_integration_sha": INTEGRATION_SHA,
            "blocking_condition": "The required file-change tool failed: bwrap Permission denied before any compatibility files were written."
        }),
    );
    blocked["reason_code"] = json!("FILE_CHANGE_TOOL_UNAVAILABLE");
    blocked
}

fn patch_not_authorized_blocker() -> Value {
    let mut blocked = message(
        "BLOCKED",
        "INTEGRATE",
        1,
        Some(1),
        None,
        None,
        json!({
            "role": "PRIMARY",
            "request_hash": "filled-from-prompt",
            "approved_plan_revision": 1,
            "approved_primary_sha": PRIMARY_SHA,
            "approved_reviewer_sha": REVIEWER_SHA,
            "approved_plan_hash": "filled-from-prompt",
            "resulting_integration_branch": "consensus/test-run",
            "resulting_integration_sha": INTEGRATION_SHA,
            "blocking_condition": "PATCH_NOT_AUTHORIZED: controlled patch is limited to the active primary integration turn before a result is reported."
        }),
    );
    blocked["reason_code"] = json!("PATCH_NOT_AUTHORIZED");
    blocked
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
            "integration_evidence": {"summary": "revised plan implemented"}
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
                ],
                "test_commands": ["cargo test --workspace"]
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
                "integration_evidence": {"summary": "reviewer edge restored"}
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
        ,"test_commands": ["cargo test --workspace"]
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

fn run_git(cwd: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(cwd: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

struct RealGitSafetyFixture {
    _root: tempfile::TempDir,
    primary: PathBuf,
    reviewer: PathBuf,
    facts: RunFacts,
    integration_sha: String,
    changed_files: Vec<PathBuf>,
}

impl RealGitSafetyFixture {
    fn integrated() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let primary = root.path().join("primary");
        let reviewer = root.path().join("reviewer");
        fs::create_dir(&repository).unwrap();
        run_git(&repository, &["init", "--initial-branch=base"]);
        run_git(&repository, &["config", "user.name", "Consensus Test"]);
        run_git(
            &repository,
            &["config", "user.email", "consensus@example.invalid"],
        );
        fs::write(repository.join("README.md"), "base\n").unwrap();
        run_git(&repository, &["add", "README.md"]);
        run_git(&repository, &["commit", "-m", "base"]);
        run_git(&repository, &["branch", "primary"]);
        run_git(&repository, &["branch", "reviewer"]);
        run_git(
            &repository,
            &["worktree", "add", primary.to_str().unwrap(), "primary"],
        );
        run_git(
            &repository,
            &["worktree", "add", reviewer.to_str().unwrap(), "reviewer"],
        );
        fs::write(primary.join("primary.txt"), "primary\n").unwrap();
        run_git(&primary, &["add", "primary.txt"]);
        run_git(&primary, &["commit", "-m", "primary-change"]);
        fs::write(reviewer.join("reviewer.txt"), "reviewer\n").unwrap();
        run_git(&reviewer, &["add", "reviewer.txt"]);
        run_git(&reviewer, &["commit", "-m", "reviewer-change"]);
        let inspector = GitInspector::default();
        let frozen_primary = inspector.inspect_worktree(&primary).unwrap();
        let frozen_reviewer = inspector.inspect_worktree(&reviewer).unwrap();
        let facts = RunFacts {
            run_id: Uuid::new_v4(),
            primary_thread_id: "primary-thread".into(),
            reviewer_thread_id: "reviewer-thread".into(),
            primary_worktree: frozen_primary.worktree.clone(),
            reviewer_worktree: frozen_reviewer.worktree.clone(),
            git_common_dir: frozen_primary.common_dir.clone(),
            primary_sha: frozen_primary.head_sha.clone(),
            reviewer_sha: frozen_reviewer.head_sha.clone(),
            primary_ref: frozen_primary.source_ref.map(|source| source.name),
            reviewer_ref: frozen_reviewer.source_ref.map(|source| source.name),
        };
        run_git(&primary, &["switch", "-c", "consensus/test-run"]);
        run_git(
            &primary,
            &["merge", "--no-ff", "reviewer", "-m", "integrate-reviewer"],
        );
        let integration = inspector.inspect_integration(&primary, &facts).unwrap();
        Self {
            _root: root,
            primary,
            reviewer,
            facts,
            integration_sha: integration.worktree.head_sha,
            changed_files: integration.changed_files,
        }
    }
}
