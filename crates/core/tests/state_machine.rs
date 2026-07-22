use std::path::PathBuf;

use consensus_core::{
    canonical_json_hash,
    protocol::{ProtocolMessage, validate_message},
    state::{NextAction, Phase, Role, RunDiagnostic, RunFacts, RunState, RunStatus},
};
use serde_json::{Value, json};
use uuid::Uuid;

const RUN_ID: &str = "4b230bd8-d870-4ef4-bf20-05a4c61020af";
const PRIMARY_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const REVIEWER_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[test]
fn integration_is_impossible_before_plan_approval() {
    let mut state = fixture_plan_state();

    let error = state.request_integration().unwrap_err();

    assert_eq!(error.code(), "PLAN_NOT_APPROVED");
}

#[test]
fn integration_requires_an_isolated_verification_turn_before_result_review() {
    let integration_sha = "cccccccccccccccccccccccccccccccccccccccc";
    let mut state = fixture_plan_state();
    let plan_hash = canonical_json_hash(state.current_plan_payload.as_ref().unwrap());
    state.apply_message(approved_plan(&plan_hash)).unwrap();

    let next = state
        .apply_message(integration_created(integration_sha))
        .unwrap();

    assert_eq!(next, NextAction::RequestPrimaryVerification);
    assert_eq!(state.phase, Phase::Verify);
    assert!(state.test_evidence.is_empty());

    let next = state
        .apply_message(integration_ready(integration_sha))
        .unwrap();
    assert_eq!(next, NextAction::RequestReviewerResultVerdict);
    assert_eq!(state.phase, Phase::ResultReview);
}

#[test]
fn plan_approval_is_bound_to_the_exact_canonical_plan_hash() {
    let mut state = fixture_plan_state();
    let error = state
        .apply_message(approved_plan(
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        ))
        .unwrap_err();

    assert_eq!(error.code(), "STALE_PLAN_HASH");
}

#[test]
fn stale_result_approval_is_rejected() {
    let mut state = fixture_result_state("cccccccccccccccccccccccccccccccccccccccc");
    let stale = approved_result("dddddddddddddddddddddddddddddddddddddddd");

    let error = state.apply_message(stale).unwrap_err();

    assert_eq!(error.code(), "STALE_INTEGRATION_SHA");
}

#[test]
fn sixth_unapproved_round_blocks() {
    let mut state = fixture_plan_state();
    for round in 1..=6 {
        state
            .apply_message(changes_required(round, format!("issue-{round}")))
            .unwrap();
    }

    assert_eq!(state.status, RunStatus::Blocked);
    assert_eq!(state.reason_code.as_deref(), Some("ROUND_LIMIT"));
}

#[test]
fn two_identical_issue_sets_block_as_no_progress() {
    let mut state = fixture_plan_state();
    state
        .apply_message(changes_required_with_ids(
            1,
            json!(["missing-test", "missing-edge-case"]),
        ))
        .unwrap();
    state
        .apply_message(changes_required_with_ids(
            2,
            json!(["missing-edge-case", "missing-test"]),
        ))
        .unwrap();

    assert_eq!(state.status, RunStatus::Blocked);
    assert_eq!(state.reason_code.as_deref(), Some("NO_PROGRESS"));
}

#[test]
fn exact_result_approval_requires_read_only_revalidation_before_acceptance() {
    let integration_sha = "cccccccccccccccccccccccccccccccccccccccc";
    let mut state = fixture_result_state(integration_sha);

    let action = state
        .apply_message(approved_result(integration_sha))
        .unwrap();

    assert_eq!(action, NextAction::RevalidateAndAccept);
    assert_eq!(state.status, RunStatus::Running);
    state.accept_after_revalidation().unwrap();
    assert_eq!(state.status, RunStatus::Accepted);
    let accepted = state.accepted_result.as_ref().unwrap();
    assert_eq!(accepted.integration_branch, "consensus/test-run");
    assert_eq!(accepted.integration_sha, integration_sha);
    assert_eq!(accepted.tests.len(), 1);
    assert_eq!(accepted.tests[0].command, "cargo test");
    assert_eq!(accepted.tests[0].exit_code, 0);
    assert_eq!(accepted.tests[0].turn_id, "verification-turn");
    assert_eq!(accepted.tests[0].item_id, "test-command-1");
    assert!(accepted.source_refs_unchanged);
    assert!(accepted.publication.local_only);
    assert!(!accepted.publication.pushed);
    assert!(!accepted.publication.pull_request_created);
    assert!(!accepted.publication.merged_into_existing_branch);
}

#[test]
fn acceptance_rechecks_nonempty_successful_test_evidence() {
    let integration_sha = "cccccccccccccccccccccccccccccccccccccccc";
    let mut state = fixture_result_state(integration_sha);
    state
        .apply_message(approved_result(integration_sha))
        .unwrap();
    state.test_evidence.clear();

    let error = state.accept_after_revalidation().unwrap_err();

    assert_eq!(error.code(), "TEST_FAILURE");
    assert_eq!(state.status, RunStatus::Running);
}

#[test]
fn acceptance_requires_exactly_one_evidence_item_per_frozen_command() {
    let integration_sha = "cccccccccccccccccccccccccccccccccccccccc";
    let mut state = fixture_result_state(integration_sha);
    state
        .apply_message(approved_result(integration_sha))
        .unwrap();
    state.test_evidence.push(state.test_evidence[0].clone());

    let error = state.accept_after_revalidation().unwrap_err();

    assert_eq!(error.code(), "TEST_FAILURE");
    assert_eq!(state.status, RunStatus::Running);
}

#[test]
fn persisted_accepted_result_must_match_authoritative_state_and_publication_boundary() {
    let integration_sha = "cccccccccccccccccccccccccccccccccccccccc";
    let mut state = fixture_result_state(integration_sha);
    state
        .apply_message(approved_result(integration_sha))
        .unwrap();
    state.accept_after_revalidation().unwrap();
    state.accepted_result.as_mut().unwrap().publication.pushed = true;

    let error = state.validate_persisted().unwrap_err();

    assert_eq!(error.code(), "INCOMPATIBLE_STATE");
}

#[test]
fn stale_blocked_message_cannot_terminate_the_current_round() {
    let mut state = fixture_plan_state();
    let blocked = message(json!({
        "message_type": "BLOCKED",
        "phase": "CONTRACT",
        "round": 99,
        "plan_revision": null,
        "integration_branch": null,
        "integration_sha": null,
        "reason_code": "MODEL_BLOCKED",
        "payload": {"detail": "stale"}
    }));

    let error = state.apply_message(blocked).unwrap_err();

    assert_eq!(error.code(), "WRONG_PHASE");
    assert_eq!(state.status, RunStatus::Running);
}

#[test]
fn pause_and_resume_preserve_the_pending_action() {
    let mut state = fixture_plan_state();
    let expected = state.next_action;

    state.pause("PERMISSION_REQUIRED").unwrap();
    assert_eq!(state.status, RunStatus::PausedUserAction);

    let resumed = state.resume().unwrap();
    assert_eq!(resumed, expected);
    assert_eq!(state.status, RunStatus::Running);
}

#[test]
fn legacy_invalid_test_block_restores_only_its_read_only_declaration_action() {
    let mut state = RunState::new(facts());
    state.record_error(RunDiagnostic {
        code: "INVALID_TEST_COMMAND".into(),
        detail: "git test command is forbidden".into(),
        operation: None,
        action: NextAction::RequestPrimaryContract,
        role: Some(Role::Primary),
        thread_id: Some("primary-thread".into()),
    });
    state.block("INVALID_TEST_COMMAND");

    let action = state.retry_blocked_invalid_test_command().unwrap();

    assert_eq!(action, NextAction::RequestPrimaryContract);
    assert_eq!(state.status, RunStatus::Running);
    assert_eq!(state.phase, Phase::Contract);
    assert_eq!(state.next_action, NextAction::RequestPrimaryContract);
    assert!(state.reason_code.is_none());
    assert!(state.last_error.is_none());
}

#[test]
fn blocked_preintegration_invalid_plan_verdict_restores_the_exact_action() {
    let mut state = fixture_plan_state();
    state.record_error(RunDiagnostic {
        code: "INVALID_RESPONSE".into(),
        detail: "approved plan revision does not match the envelope".into(),
        operation: None,
        action: NextAction::RequestReviewerPlanVerdict,
        role: Some(Role::Reviewer),
        thread_id: Some("reviewer-thread".into()),
    });
    state.block("INVALID_RESPONSE");

    let action = state
        .retry_blocked_preintegration_invalid_response()
        .unwrap();

    assert_eq!(action, NextAction::RequestReviewerPlanVerdict);
    assert_eq!(state.status, RunStatus::Running);
    assert_eq!(state.phase, Phase::PlanReview);
    assert_eq!(state.next_action, NextAction::RequestReviewerPlanVerdict);
    assert!(state.reason_code.is_none());
    assert!(state.last_error.is_none());
}

#[test]
fn blocked_postintegration_invalid_response_is_not_retryable() {
    let mut state = fixture_result_state("cccccccccccccccccccccccccccccccccccccccc");
    state.record_error(RunDiagnostic {
        code: "INVALID_RESPONSE".into(),
        detail: "invalid result verdict".into(),
        operation: None,
        action: NextAction::RequestReviewerResultVerdict,
        role: Some(Role::Reviewer),
        thread_id: Some("reviewer-thread".into()),
    });
    state.block("INVALID_RESPONSE");

    let error = state
        .retry_blocked_preintegration_invalid_response()
        .unwrap_err();

    assert_eq!(error.code(), "NOT_RETRYABLE");
}

#[test]
fn completed_integration_with_an_invalid_result_can_retry_only_its_report_turn() {
    let mut state = fixture_plan_state();
    let plan_hash = canonical_json_hash(state.current_plan_payload.as_ref().unwrap());
    state.apply_message(approved_plan(&plan_hash)).unwrap();
    state.record_error(RunDiagnostic {
        code: "INVALID_RESPONSE".into(),
        detail: "protocol invariant failed: message requires an integration_branch".into(),
        operation: None,
        action: NextAction::RequestPrimaryIntegration,
        role: Some(Role::Primary),
        thread_id: Some(state.facts.primary_thread_id.clone()),
    });
    state.block("INVALID_RESPONSE");

    let action = state.retry_blocked_integration_invalid_response().unwrap();

    assert_eq!(action, NextAction::RequestPrimaryIntegration);
    assert_eq!(state.status, RunStatus::Running);
    assert_eq!(state.phase, Phase::Integrate);
    assert_eq!(state.integration_branch, None);
    assert_eq!(state.integration_sha, None);
    assert!(state.reason_code.is_none());
    assert!(state.last_error.is_none());
}

#[test]
fn verification_without_execution_restores_only_the_verification_action() {
    let integration_sha = "cccccccccccccccccccccccccccccccccccccccc";
    let mut state = fixture_plan_state();
    let plan_hash = canonical_json_hash(state.current_plan_payload.as_ref().unwrap());
    state.apply_message(approved_plan(&plan_hash)).unwrap();
    state
        .apply_message(integration_created(integration_sha))
        .unwrap();
    state.verification_worktree = Some(PathBuf::from("/state/verification/run"));
    state.record_error(RunDiagnostic {
        code: "TEST_FAILURE".into(),
        detail: "verification must execute each frozen command exactly once and no other command"
            .into(),
        operation: None,
        action: NextAction::RequestPrimaryVerification,
        role: Some(Role::Primary),
        thread_id: Some("primary-thread".into()),
    });
    state.block("TEST_FAILURE");

    let action = state
        .retry_blocked_verification_without_execution()
        .unwrap();

    assert_eq!(action, NextAction::RequestPrimaryVerification);
    assert_eq!(state.status, RunStatus::Running);
    assert_eq!(state.phase, Phase::Verify);
    assert_eq!(state.integration_sha.as_deref(), Some(integration_sha));
    assert!(state.reason_code.is_none());
    assert!(state.last_error.is_none());
}

#[test]
fn cargo_environment_blocker_restores_only_the_verification_action() {
    let integration_sha = "cccccccccccccccccccccccccccccccccccccccc";
    let mut state = fixture_plan_state();
    let plan_hash = canonical_json_hash(state.current_plan_payload.as_ref().unwrap());
    state.apply_message(approved_plan(&plan_hash)).unwrap();
    state
        .apply_message(integration_created(integration_sha))
        .unwrap();
    state.verification_worktree = Some(PathBuf::from("/state/verification/run"));
    state.block("CARGO_UNAVAILABLE");

    let action = state
        .retry_blocked_verification_environment_unavailable()
        .unwrap();

    assert_eq!(action, NextAction::RequestPrimaryVerification);
    assert_eq!(state.status, RunStatus::Running);
    assert_eq!(state.phase, Phase::Verify);
    assert_eq!(state.integration_sha.as_deref(), Some(integration_sha));
    assert!(state.reason_code.is_none());
    assert!(state.test_evidence.is_empty());
}

#[test]
fn completed_failed_verification_routes_machine_feedback_to_integration() {
    let integration_sha = "cccccccccccccccccccccccccccccccccccccccc";
    let mut state = fixture_plan_state();
    let plan_hash = canonical_json_hash(state.current_plan_payload.as_ref().unwrap());
    state.apply_message(approved_plan(&plan_hash)).unwrap();
    state
        .apply_message(integration_created(integration_sha))
        .unwrap();
    let mut verification = integration_ready(integration_sha);
    verification.payload["test_evidence"][0]["exit_code"] = json!(1);
    verification.payload["verification_failures"] = json!([{
        "command": "cargo test",
        "exit_code": 1,
        "item_id": "test-command-1",
        "output": "a compiler diagnostic"
    }]);

    let action = state.apply_message(verification).unwrap();

    assert_eq!(action, NextAction::RequestPrimaryIntegration);
    assert_eq!(state.status, RunStatus::Running);
    assert_eq!(state.phase, Phase::Integrate);
    assert_eq!(state.round, 2);
    assert_eq!(state.integration_sha.as_deref(), Some(integration_sha));
    assert_eq!(state.test_evidence[0].exit_code, 1);
    let feedback = state.last_result_feedback.as_ref().unwrap();
    assert_eq!(feedback["format"], "machine_verification");
    assert_eq!(
        feedback["failed_tests"][0]["output"],
        "a compiler diagnostic"
    );
    state.validate_persisted().unwrap();
}

#[test]
fn integration_invalid_response_retry_rejects_an_already_accepted_result() {
    let mut state = fixture_result_state("cccccccccccccccccccccccccccccccccccccccc");
    state.record_error(RunDiagnostic {
        code: "INVALID_RESPONSE".into(),
        detail: "late malformed response".into(),
        operation: None,
        action: NextAction::RequestPrimaryIntegration,
        role: Some(Role::Primary),
        thread_id: Some(state.facts.primary_thread_id.clone()),
    });
    state.block("INVALID_RESPONSE");

    let error = state
        .retry_blocked_integration_invalid_response()
        .unwrap_err();

    assert_eq!(error.code(), "NOT_RETRYABLE");
}

#[test]
fn side_effect_free_integration_tool_blocker_restores_the_integration_action() {
    let mut state = fixture_plan_state();
    let plan_hash = canonical_json_hash(state.current_plan_payload.as_ref().unwrap());
    let approval = message(json!({
        "message_type": "APPROVED_PLAN",
        "phase": "PLAN_REVIEW",
        "round": 1,
        "plan_revision": 1,
        "integration_branch": null,
        "integration_sha": null,
        "reason_code": null,
        "payload": {
            "approved_plan_revision": 1,
            "approved_primary_sha": PRIMARY_SHA,
            "approved_reviewer_sha": REVIEWER_SHA,
            "approved_plan_hash": plan_hash,
            "uncovered_items": []
        }
    }));
    state.apply_message(approval).unwrap();
    state.block("EXECUTION_TOOL_UNAVAILABLE");

    let action = state
        .retry_blocked_integration_execution_tool_unavailable()
        .unwrap();

    assert_eq!(action, NextAction::RequestPrimaryIntegration);
    assert_eq!(state.status, RunStatus::Running);
    assert_eq!(state.phase, Phase::Integrate);
    assert_eq!(state.next_action, NextAction::RequestPrimaryIntegration);
    assert!(state.reason_code.is_none());
}

#[test]
fn execution_tool_blocker_is_not_retryable_after_integration_identity_exists() {
    let mut state = fixture_result_state("cccccccccccccccccccccccccccccccccccccccc");
    state.block("EXECUTION_TOOL_UNAVAILABLE");

    let error = state
        .retry_blocked_integration_execution_tool_unavailable()
        .unwrap_err();

    assert_eq!(error.code(), "NOT_RETRYABLE");
}

#[test]
fn side_effect_free_primary_integration_forbidden_operation_is_retryable() {
    let mut state = fixture_plan_state();
    let plan_hash = canonical_json_hash(state.current_plan_payload.as_ref().unwrap());
    state.apply_message(approved_plan(&plan_hash)).unwrap();
    state.record_error(RunDiagnostic {
        code: "FORBIDDEN_OPERATION".into(),
        detail: "the task requested a command outside policy".into(),
        operation: None,
        action: NextAction::RequestPrimaryIntegration,
        role: Some(Role::Primary),
        thread_id: Some(state.facts.primary_thread_id.clone()),
    });
    state.block("FORBIDDEN_OPERATION");

    let action = state
        .retry_blocked_preintegration_forbidden_operation()
        .unwrap();

    assert_eq!(action, NextAction::RequestPrimaryIntegration);
    assert_eq!(state.status, RunStatus::Running);
    assert_eq!(state.phase, Phase::Integrate);
    assert!(state.reason_code.is_none());
    assert!(state.last_error.is_none());
}

#[test]
fn forbidden_operation_after_integration_identity_is_not_retryable() {
    let mut state = fixture_result_state("cccccccccccccccccccccccccccccccccccccccc");
    state.record_error(RunDiagnostic {
        code: "FORBIDDEN_OPERATION".into(),
        detail: "forbidden command".into(),
        operation: None,
        action: NextAction::RequestPrimaryIntegration,
        role: Some(Role::Primary),
        thread_id: Some(state.facts.primary_thread_id.clone()),
    });
    state.block("FORBIDDEN_OPERATION");

    let error = state
        .retry_blocked_preintegration_forbidden_operation()
        .unwrap_err();

    assert_eq!(error.code(), "NOT_RETRYABLE");
}

#[test]
fn incompatible_adapter_has_a_distinct_terminal_status() {
    let mut state = RunState::new(facts());

    state.mark_incompatible("INCOMPATIBLE_CODEX");

    assert_eq!(state.status, RunStatus::IncompatibleCodex);
    assert_eq!(state.phase, Phase::Blocked);
    assert_eq!(state.next_action, NextAction::Stop);
}

#[test]
fn contract_role_is_derived_from_the_bound_pending_task_when_omitted() {
    let mut state = RunState::new(facts());

    assert_eq!(
        state
            .apply_message(contract_ready_without_role(json!({
                "goal": "primary",
                "tests": ["cargo test -p primary"]
            })))
            .unwrap(),
        NextAction::RequestReviewerContract
    );
    assert_eq!(
        state
            .apply_message(contract_ready_without_role(json!({
                "goal": "reviewer",
                "tests": ["cargo test -p reviewer"]
            })))
            .unwrap(),
        NextAction::RequestPrimaryPlan
    );
    assert_eq!(state.primary_contract.as_ref().unwrap()["goal"], "primary");
    assert_eq!(
        state.reviewer_contract.as_ref().unwrap()["goal"],
        "reviewer"
    );
}

#[test]
fn explicit_contract_role_must_match_the_bound_pending_task() {
    let mut state = RunState::new(facts());

    let error = state
        .apply_message(contract_ready(
            Role::Reviewer,
            json!({"goal": "wrong task", "tests": ["cargo test"]}),
        ))
        .unwrap_err();

    assert_eq!(error.code(), "UNEXPECTED_ROLE");
    assert_eq!(state.next_action, NextAction::RequestPrimaryContract);
    assert!(state.primary_contract.is_none());
}

#[test]
fn user_contract_and_plan_tests_are_frozen_before_integration() {
    let mut state = RunState::new(facts());
    state
        .configure_integration(
            "consensus/test-run",
            vec!["cargo test -p user-required".into()],
        )
        .unwrap();
    state
        .apply_message(contract_ready(
            Role::Primary,
            json!({"goal": "primary", "tests": ["cargo test -p primary"]}),
        ))
        .unwrap();
    state
        .apply_message(contract_ready(
            Role::Reviewer,
            json!({"goal": "reviewer", "tests": ["cargo test -p reviewer"]}),
        ))
        .unwrap();
    state
        .record_plan(json!({"test_commands": ["cargo test -p integration"]}))
        .unwrap();

    assert_eq!(
        state.required_test_commands,
        vec![
            "cargo test -p user-required",
            "cargo test -p primary",
            "cargo test -p reviewer",
            "cargo test -p integration"
        ]
    );
}

fn fixture_plan_state() -> RunState {
    let mut state = RunState::new(facts());
    state
        .configure_integration("consensus/test-run", vec!["cargo test".into()])
        .unwrap();
    assert_eq!(
        state
            .apply_message(contract_ready(
                Role::Primary,
                json!({"goal": "primary", "tests": ["cargo test"]}),
            ))
            .unwrap(),
        NextAction::RequestReviewerContract
    );
    assert_eq!(
        state
            .apply_message(contract_ready(
                Role::Reviewer,
                json!({"goal": "reviewer", "tests": ["cargo test"]}),
            ))
            .unwrap(),
        NextAction::RequestPrimaryPlan
    );
    state
        .record_plan(json!({
            "revision": 1,
            "coverage": ["primary", "reviewer"],
            "test_commands": ["cargo test"]
        }))
        .unwrap();
    assert_eq!(state.required_test_commands, vec!["cargo test"]);
    assert_eq!(state.phase, Phase::PlanReview);
    state
}

fn fixture_result_state(integration_sha: &str) -> RunState {
    let mut state = fixture_plan_state();
    let plan_hash = canonical_json_hash(state.current_plan_payload.as_ref().unwrap());
    state.apply_message(approved_plan(&plan_hash)).unwrap();
    state
        .apply_message(integration_created(integration_sha))
        .unwrap();
    assert_eq!(state.phase, Phase::Verify);
    state
        .apply_message(integration_ready(integration_sha))
        .unwrap();
    assert_eq!(state.phase, Phase::ResultReview);
    state
}

fn facts() -> RunFacts {
    RunFacts {
        run_id: Uuid::parse_str(RUN_ID).unwrap(),
        primary_thread_id: "primary-thread".into(),
        reviewer_thread_id: "reviewer-thread".into(),
        primary_worktree: PathBuf::from("/repo/primary"),
        reviewer_worktree: PathBuf::from("/repo/reviewer"),
        git_common_dir: PathBuf::from("/repo/.git"),
        primary_sha: PRIMARY_SHA.into(),
        reviewer_sha: REVIEWER_SHA.into(),
        primary_ref: Some("refs/heads/primary".into()),
        reviewer_ref: Some("refs/heads/reviewer".into()),
    }
}

fn contract_ready(role: Role, payload: Value) -> ProtocolMessage {
    message(json!({
        "message_type": "CONTRACT_READY",
        "phase": "CONTRACT",
        "round": 1,
        "plan_revision": null,
        "integration_branch": null,
        "integration_sha": null,
        "reason_code": null,
        "payload": {"role": role, "contract": payload}
    }))
}

fn contract_ready_without_role(payload: Value) -> ProtocolMessage {
    message(json!({
        "message_type": "CONTRACT_READY",
        "phase": "CONTRACT",
        "round": 1,
        "plan_revision": null,
        "integration_branch": null,
        "integration_sha": null,
        "reason_code": null,
        "payload": {"contract": payload}
    }))
}

fn approved_plan(plan_hash: &str) -> ProtocolMessage {
    message(json!({
        "message_type": "APPROVED_PLAN",
        "phase": "PLAN_REVIEW",
        "round": 1,
        "plan_revision": 1,
        "integration_branch": null,
        "integration_sha": null,
        "reason_code": null,
        "payload": {
            "approved_plan_revision": 1,
            "approved_primary_sha": PRIMARY_SHA,
            "approved_reviewer_sha": REVIEWER_SHA,
            "approved_plan_hash": plan_hash,
            "uncovered_items": []
        }
    }))
}

fn integration_created(integration_sha: &str) -> ProtocolMessage {
    message(json!({
        "message_type": "INTEGRATION_READY",
        "phase": "INTEGRATE",
        "round": 1,
        "plan_revision": 1,
        "integration_branch": "consensus/test-run",
        "integration_sha": integration_sha,
        "reason_code": null,
        "payload": {
            "changed_files": ["combined.txt"],
            "integration_evidence": {"summary": "created"}
        }
    }))
}

fn changes_required(round: u32, issue: String) -> ProtocolMessage {
    changes_required_with_ids(round, json!([issue]))
}

fn changes_required_with_ids(round: u32, issue_ids: Value) -> ProtocolMessage {
    message(json!({
        "message_type": "CHANGES_REQUIRED",
        "phase": "PLAN_REVIEW",
        "round": round,
        "plan_revision": round,
        "integration_branch": null,
        "integration_sha": null,
        "reason_code": "COVERAGE_GAP",
        "payload": {"issue_ids": issue_ids}
    }))
}

fn integration_ready(integration_sha: &str) -> ProtocolMessage {
    message(json!({
        "message_type": "INTEGRATION_READY",
        "phase": "VERIFY",
        "round": 1,
        "plan_revision": 1,
        "integration_branch": "consensus/test-run",
        "integration_sha": integration_sha,
        "reason_code": null,
        "payload": {
            "changed_files": ["combined.txt"],
            "integration_evidence": {"summary": "created"},
            "test_evidence": [{
                "command": "cargo test",
                "exit_code": 0,
                "turn_id": "verification-turn",
                "item_id": "test-command-1",
                "cwd": "/state/verification/run"
            }]
        }
    }))
}

fn approved_result(integration_sha: &str) -> ProtocolMessage {
    message(json!({
        "message_type": "APPROVED_RESULT",
        "phase": "RESULT_REVIEW",
        "round": 1,
        "plan_revision": 1,
        "integration_branch": "consensus/test-run",
        "integration_sha": integration_sha,
        "reason_code": null,
        "payload": {
            "approved_plan_revision": 1,
            "approved_primary_sha": PRIMARY_SHA,
            "approved_reviewer_sha": REVIEWER_SHA,
            "approved_integration_branch": "consensus/test-run",
            "approved_integration_sha": integration_sha,
            "uncovered_items": []
        }
    }))
}

fn message(fields: Value) -> ProtocolMessage {
    let mut value = json!({
        "protocol": "worktree-merge-consensus/v1",
        "run_id": RUN_ID,
        "primary_sha": PRIMARY_SHA,
        "reviewer_sha": REVIEWER_SHA
    });
    value.as_object_mut().unwrap().extend(
        fields
            .as_object()
            .expect("message fixture must be an object")
            .clone(),
    );
    validate_message(value).expect("valid protocol fixture")
}
