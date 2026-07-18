use std::path::PathBuf;

use consensus_core::{
    prompts::build_turn_prompt,
    protocol::{ProtocolMessage, validate_message},
    state::{NextAction, Role, RunFacts, RunState},
};
use serde_json::{Value, json};
use uuid::Uuid;

const RUN_ID: &str = "4b230bd8-d870-4ef4-bf20-05a4c61020af";
const PRIMARY_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const REVIEWER_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const INTEGRATION_SHA: &str = "cccccccccccccccccccccccccccccccccccccccc";

#[test]
fn every_prompt_is_self_contained_and_declares_strict_output() {
    let state = RunState::new(facts());
    let payload = json!({"task_context": "derive the primary contract from this task"});

    let prompt = build_turn_prompt(
        Role::Primary,
        NextAction::RequestPrimaryContract,
        &state,
        &payload,
    )
    .unwrap();

    for required in [
        "worktree-merge-consensus/v1",
        RUN_ID,
        "CONTRACT",
        "\"round\": 1",
        PRIMARY_SHA,
        REVIEWER_SHA,
        "derive the primary contract from this task",
        "Worktree Merge Consensus Protocol v1",
        "Text outside that one JSON object is invalid",
    ] {
        assert!(prompt.contains(required), "missing {required:?}");
    }
}

#[test]
fn plan_verdict_prompt_contains_both_contracts_plan_and_coverage() {
    let mut state = plan_state();
    state
        .record_plan(json!({"steps": ["merge", "verify"]}))
        .unwrap();
    let payload = json!({
        "primary_contract": {"goal": "preserve primary API"},
        "reviewer_contract": {"goal": "preserve reviewer retry semantics"},
        "plan": {"steps": ["merge", "verify"]},
        "coverage_matrix": [
            {"contract_item": "primary API", "plan_step": "merge"},
            {"contract_item": "retry semantics", "plan_step": "verify"}
        ]
    });

    let prompt = build_turn_prompt(
        Role::Reviewer,
        NextAction::RequestReviewerPlanVerdict,
        &state,
        &payload,
    )
    .unwrap();

    assert!(prompt.contains("preserve primary API"));
    assert!(prompt.contains("preserve reviewer retry semantics"));
    assert!(prompt.contains("coverage_matrix"));
    assert!(prompt.contains("merge"));
}

#[test]
fn result_prompt_rejects_delta_only_payloads() {
    let state = result_state();

    let error = build_turn_prompt(
        Role::Reviewer,
        NextAction::RequestReviewerResultVerdict,
        &state,
        &json!({"delta": "only the last fix"}),
    )
    .unwrap_err();

    assert_eq!(error.code(), "INCOMPLETE_PAYLOAD");
}

fn plan_state() -> RunState {
    let mut state = RunState::new(facts());
    state
        .apply_message(contract_ready("PRIMARY", "primary contract"))
        .unwrap();
    state
        .apply_message(contract_ready("REVIEWER", "reviewer contract"))
        .unwrap();
    state
}

fn result_state() -> RunState {
    let mut state = plan_state();
    state
        .record_plan(json!({"steps": ["merge", "verify"]}))
        .unwrap();
    state.apply_message(approved_plan()).unwrap();
    state.apply_message(integration_ready()).unwrap();
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

fn contract_ready(role: &str, goal: &str) -> ProtocolMessage {
    message(json!({
        "message_type": "CONTRACT_READY",
        "phase": "CONTRACT",
        "round": 1,
        "plan_revision": null,
        "integration_branch": null,
        "integration_sha": null,
        "reason_code": null,
        "payload": {"role": role, "contract": {"goal": goal}}
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

fn integration_ready() -> ProtocolMessage {
    message(json!({
        "message_type": "INTEGRATION_READY",
        "phase": "VERIFY",
        "round": 1,
        "plan_revision": 1,
        "integration_branch": "consensus/test-run",
        "integration_sha": INTEGRATION_SHA,
        "reason_code": null,
        "payload": {"tests": [{"command": "cargo test", "exit_code": 0}]}
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
