use std::path::PathBuf;

use consensus_core::{
    canonical_json_hash,
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
fn every_prompt_is_self_contained_and_declares_v2_contract_output() {
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
        "worktree-merge-consensus/v2",
        RUN_ID,
        "CONTRACT",
        "\"round\": 1",
        PRIMARY_SHA,
        REVIEWER_SHA,
        "derive the primary contract from this task",
        "<consensus-result>CONTRACT_READY</consensus-result>",
        "place exactly one JSON object after the marker",
        "Do not return the legacy v1 protocol envelope",
        "This is an internal participant turn inside an already-running run",
        "do not select it, read its `SKILL.md`",
        "Do not call `worktreeMergeConsensus`",
        "The coordinator binds every response to this exact task turn",
    ] {
        assert!(prompt.contains(required), "missing {required:?}");
    }
}

#[test]
fn prompt_declares_role_binding_and_binding_mismatch_protocol() {
    let state = RunState::new(facts());

    let primary = build_turn_prompt(
        Role::Primary,
        NextAction::RequestPrimaryContract,
        &state,
        &json!({"task_context": "derive the primary contract from this task"}),
    )
    .unwrap();

    for required in [
        "/repo/primary",
        "refs/heads/primary",
        PRIMARY_SHA,
        "The contract JSON `tests` field",
        "SOURCE_BINDING_MISMATCH",
        "Do not search for or switch to another source directory",
    ] {
        assert!(primary.contains(required), "missing {required:?}");
    }

    let mut reviewer_state = state;
    reviewer_state
        .apply_message(contract_ready("PRIMARY", "primary contract"))
        .unwrap();
    let reviewer = build_turn_prompt(
        Role::Reviewer,
        NextAction::RequestReviewerContract,
        &reviewer_state,
        &json!({"task_context": "derive the reviewer contract from this task"}),
    )
    .unwrap();
    for required in [
        "/repo/reviewer",
        "refs/heads/reviewer",
        REVIEWER_SHA,
        "contract itself, not a protocol envelope",
    ] {
        assert!(reviewer.contains(required), "missing {required:?}");
    }
}

#[test]
fn integration_prompt_declares_the_frozen_repository_read_surface() {
    let mut state = RunState::new(facts());
    state.next_action = NextAction::RequestPrimaryIntegration;

    let prompt = build_turn_prompt(
        Role::Primary,
        NextAction::RequestPrimaryIntegration,
        &state,
        &json!({
            "primary_contract": {"goal": "preserve primary"},
            "reviewer_contract": {"goal": "preserve reviewer"},
            "approved_plan": {"revision": 1},
            "coverage_matrix": [],
            "approval": {"approved_plan_revision": 1},
            "target_integration_branch": "consensus/test-run"
        }),
    )
    .unwrap();

    for required in [
        "`rg --files -g AGENTS.md`",
        "`git show REV:path`",
        "`git ls-files`",
        "`git diff`",
        "do not invoke sed, cat, find, ls, head, tail",
        "consensus_apply_patch",
        "exact request_hash",
        "do not use the built-in file-change tool",
        "do not recreate or re-merge",
        "coordinator independently validates the current branch and HEAD",
        "prefer exactly `git branch --show-current`",
        "`git symbolic-ref --short HEAD` is the only equivalent form",
        "after one successful patch, no second patch is authorized",
        "Never use `git diff --no-index`",
        "stage new files with `git add -A` before inspecting them with `git diff --cached`",
        "If `rg` is unavailable, use `git ls-files` to discover tracked `AGENTS.md` files",
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
        ],
        "test_commands": ["cargo test"],
        "plan_hash": "0000000000000000000000000000000000000000000000000000000000000000"
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
    for required in [
        "<consensus-result>APPROVED</consensus-result>".to_owned(),
        "<consensus-result>CHANGES_REQUIRED</consensus-result>".to_owned(),
        "write concrete free-form Markdown feedback".to_owned(),
        "Do not return JSON or repeat run, revision, hash, or SHA fields".to_owned(),
        "\"plan_hash\": \"0000000000000000000000000000000000000000000000000000000000000000\""
            .to_owned(),
        "binds the verdict to the exact current plan automatically".to_owned(),
    ] {
        assert!(prompt.contains(&required), "missing {required:?}");
    }
}

#[test]
fn result_verdict_prompt_uses_a_minimal_marker_and_code_side_identity() {
    let state = result_state();
    let payload = json!({
        "primary_contract": {"goal": "preserve primary API"},
        "reviewer_contract": {"goal": "preserve reviewer behavior"},
        "approved_plan": {"steps": ["merge", "verify"]},
        "coverage_matrix": [{"contract_item": "all", "plan_step": "verify"}],
        "integration_evidence": {"summary": "integrated"},
        "test_evidence": [{"command": "cargo test", "exit_code": 0}],
        "changed_files": ["combined.txt"],
        "integration_branch": "consensus/test-run",
        "integration_sha": INTEGRATION_SHA
    });

    let prompt = build_turn_prompt(
        Role::Reviewer,
        NextAction::RequestReviewerResultVerdict,
        &state,
        &payload,
    )
    .unwrap();

    for required in [
        "<consensus-result>APPROVED</consensus-result>".to_owned(),
        "<consensus-result>CHANGES_REQUIRED</consensus-result>".to_owned(),
        "Do not return JSON or repeat the integration branch or SHA".to_owned(),
        "binds the verdict to the exact current branch and SHA automatically".to_owned(),
        "\"integration_branch\": \"consensus/test-run\"".to_owned(),
        format!("\"integration_sha\": \"{INTEGRATION_SHA}\""),
    ] {
        assert!(prompt.contains(&required), "missing {required:?}");
    }
}

#[test]
fn verification_prompt_requires_marker_only_coordinator_handoff() {
    let mut state = plan_state();
    let plan = json!({"steps": ["merge", "verify"]});
    let plan_hash = canonical_json_hash(&plan);
    state.record_plan(plan).unwrap();
    state.apply_message(approved_plan(&plan_hash)).unwrap();
    state.apply_message(integration_created()).unwrap();
    state.verification_worktree = Some(PathBuf::from("/state/verification/run"));
    let payload = json!({
        "integration_evidence": {"summary": "integrated"},
        "changed_files": ["combined.txt"],
        "required_test_commands": ["cargo test"],
        "verification_worktree": "/state/verification/run",
        "integration_branch": "consensus/test-run",
        "integration_sha": INTEGRATION_SHA
    });

    let prompt = build_turn_prompt(
        Role::Primary,
        NextAction::RequestPrimaryVerification,
        &state,
        &payload,
    )
    .unwrap();

    for required in [
        "This is a marker-only handoff to coordinator-owned verification",
        "Do not run Shell, Git, file, MCP, or patch tools in this turn",
        "Return VERIFICATION_READY when ready",
        "the coordinator will run every frozen command in the exact isolated clone and derive all evidence",
        "<consensus-result>VERIFICATION_READY</consensus-result>",
    ] {
        assert!(prompt.contains(required), "missing {required:?}");
    }
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
    let plan = json!({"steps": ["merge", "verify"]});
    let plan_hash = canonical_json_hash(&plan);
    state.record_plan(plan).unwrap();
    state.apply_message(approved_plan(&plan_hash)).unwrap();
    state.apply_message(integration_created()).unwrap();
    state.apply_message(integration_verified()).unwrap();
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
        "payload": {
            "role": role,
            "contract": {"goal": goal, "tests": ["cargo test"]}
        }
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

fn integration_created() -> ProtocolMessage {
    message(json!({
        "message_type": "INTEGRATION_READY",
        "phase": "INTEGRATE",
        "round": 1,
        "plan_revision": 1,
        "integration_branch": "consensus/test-run",
        "integration_sha": INTEGRATION_SHA,
        "reason_code": null,
        "payload": {
            "changed_files": ["combined.txt"],
            "integration_evidence": {"summary": "both changes integrated"}
        }
    }))
}

fn integration_verified() -> ProtocolMessage {
    message(json!({
        "message_type": "INTEGRATION_READY",
        "phase": "VERIFY",
        "round": 1,
        "plan_revision": 1,
        "integration_branch": "consensus/test-run",
        "integration_sha": INTEGRATION_SHA,
        "reason_code": null,
        "payload": {
            "changed_files": ["combined.txt"],
            "integration_evidence": {"summary": "both changes integrated"},
            "test_evidence": [{
                "command": "cargo test",
                "exit_code": 0,
                "turn_id": "turn-verify",
                "item_id": "command-1",
                "cwd": "/state/verification/run"
            }]
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
