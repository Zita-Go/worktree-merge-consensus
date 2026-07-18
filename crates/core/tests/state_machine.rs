use std::path::PathBuf;

use consensus_core::{
    protocol::{ProtocolMessage, validate_message},
    state::{NextAction, Phase, Role, RunFacts, RunState, RunStatus},
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

fn fixture_plan_state() -> RunState {
    let mut state = RunState::new(facts());
    assert_eq!(
        state
            .apply_message(contract_ready(Role::Primary, json!({"goal": "primary"})))
            .unwrap(),
        NextAction::RequestReviewerContract
    );
    assert_eq!(
        state
            .apply_message(contract_ready(Role::Reviewer, json!({"goal": "reviewer"})))
            .unwrap(),
        NextAction::RequestPrimaryPlan
    );
    state
        .record_plan(json!({"revision": 1, "coverage": ["primary", "reviewer"]}))
        .unwrap();
    assert_eq!(state.phase, Phase::PlanReview);
    state
}

fn fixture_result_state(integration_sha: &str) -> RunState {
    let mut state = fixture_plan_state();
    state.apply_message(approved_plan()).unwrap();
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

fn approved_plan() -> ProtocolMessage {
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
            "uncovered_items": []
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
        "payload": {"tests": [{"command": "cargo test", "exit_code": 0}]}
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
