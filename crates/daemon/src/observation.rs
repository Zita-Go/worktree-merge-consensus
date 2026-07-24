use std::path::Path;

use consensus_core::state::{NextAction, Phase, Role, RunDiagnostic, RunState, RunStatus};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const OBSERVATION_SCHEMA_VERSION: u32 = 1;
pub const MAX_EVENT_BATCH: usize = 6;
const MAX_PUBLIC_ARTIFACT_BYTES: usize = 48 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublicSource {
    pub thread_id: String,
    pub worktree: String,
    pub source_ref: Option<String>,
    pub sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressStage {
    pub index: u8,
    pub total: u8,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsensusArtifacts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_contract: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewer_contract: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_plan: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_feedback: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_approval: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integration_summary: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_feedback: Option<Value>,
    pub required_tests: Value,
    pub test_evidence: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_result: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublicRunSnapshot {
    pub schema_version: u32,
    pub cursor: i64,
    pub run_id: String,
    pub status: RunStatus,
    pub phase: Phase,
    pub round: u32,
    pub max_review_rounds: u32,
    pub plan_revision: Option<u32>,
    pub next_action: NextAction,
    pub active_role: Option<Role>,
    pub progress: ProgressStage,
    pub terminal: bool,
    pub primary: PublicSource,
    pub reviewer: PublicSource,
    pub target_integration_branch: Option<String>,
    pub integration_branch: Option<String>,
    pub integration_sha: Option<String>,
    pub reason_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<Value>,
    pub artifacts: ConsensusArtifacts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunProgressEvent {
    pub schema_version: u32,
    pub cursor: i64,
    pub run_id: String,
    pub created_at: i64,
    pub kind: String,
    pub summary: String,
    pub status: RunStatus,
    pub phase: Phase,
    pub round: u32,
    pub plan_revision: Option<u32>,
    pub next_action: NextAction,
    pub active_role: Option<Role>,
    pub progress: ProgressStage,
    pub terminal: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunObservationBatch {
    pub schema_version: u32,
    pub run_id: String,
    pub after_cursor: i64,
    pub next_cursor: i64,
    pub latest_cursor: i64,
    pub timed_out: bool,
    pub has_more: bool,
    pub terminal: bool,
    pub paused: bool,
    pub events: Vec<RunProgressEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<PublicRunSnapshot>,
}

pub fn public_snapshot(state: &RunState, cursor: i64) -> PublicRunSnapshot {
    PublicRunSnapshot {
        schema_version: OBSERVATION_SCHEMA_VERSION,
        cursor,
        run_id: state.facts.run_id.to_string(),
        status: state.status,
        phase: state.phase,
        round: state.round,
        max_review_rounds: state.max_review_rounds,
        plan_revision: state.plan_revision,
        next_action: state.next_action,
        active_role: action_role(state.next_action),
        progress: progress_stage(state),
        terminal: terminal_status(state.status),
        primary: PublicSource {
            thread_id: state.facts.primary_thread_id.clone(),
            worktree: display_path(&state.facts.primary_worktree),
            source_ref: state.facts.primary_ref.clone(),
            sha: state.facts.primary_sha.clone(),
        },
        reviewer: PublicSource {
            thread_id: state.facts.reviewer_thread_id.clone(),
            worktree: display_path(&state.facts.reviewer_worktree),
            source_ref: state.facts.reviewer_ref.clone(),
            sha: state.facts.reviewer_sha.clone(),
        },
        target_integration_branch: state.target_integration_branch.clone(),
        integration_branch: state.integration_branch.clone(),
        integration_sha: state.integration_sha.clone(),
        reason_code: state.reason_code.clone(),
        diagnostic: state.last_error.as_ref().and_then(public_diagnostic),
        artifacts: ConsensusArtifacts {
            primary_contract: bounded_optional("primary_contract", &state.primary_contract),
            reviewer_contract: bounded_optional("reviewer_contract", &state.reviewer_contract),
            current_plan: bounded_optional("current_plan", &state.current_plan_payload),
            plan_feedback: bounded_optional("plan_feedback", &state.last_plan_feedback),
            plan_approval: bounded_optional("plan_approval", &state.plan_approval_payload),
            integration_summary: public_verification_artifact(
                "integration_summary",
                &state.current_integration_payload,
            ),
            result_feedback: public_verification_artifact(
                "result_feedback",
                &state.last_result_feedback,
            ),
            required_tests: bounded_serializable("required_tests", &state.required_test_commands)
                .unwrap_or_else(|| json!([])),
            test_evidence: bounded_serializable("test_evidence", &state.test_evidence)
                .unwrap_or_else(|| json!([])),
            accepted_result: state
                .accepted_result
                .as_ref()
                .and_then(|value| bounded_serializable("accepted_result", value)),
        },
    }
}

pub fn progress_event(
    previous: Option<&RunState>,
    state: &RunState,
    created_at: i64,
) -> RunProgressEvent {
    let (kind, summary, artifact) = classify_event(previous, state);
    let progress = if previous.is_none() {
        ProgressStage {
            index: 1,
            total: 6,
            name: "SOURCE_FREEZE".to_owned(),
        }
    } else {
        progress_stage(state)
    };
    RunProgressEvent {
        schema_version: OBSERVATION_SCHEMA_VERSION,
        cursor: 0,
        run_id: state.facts.run_id.to_string(),
        created_at,
        kind: kind.to_owned(),
        summary,
        status: state.status,
        phase: state.phase,
        round: state.round,
        plan_revision: state.plan_revision,
        next_action: state.next_action,
        active_role: action_role(state.next_action),
        progress,
        terminal: terminal_status(state.status),
        artifact,
    }
}

fn classify_event(
    previous: Option<&RunState>,
    state: &RunState,
) -> (&'static str, String, Option<Value>) {
    if state.status == RunStatus::Accepted {
        return (
            "accepted",
            format!(
                "The reviewer approved the verified integration at {}.",
                state.integration_sha.as_deref().unwrap_or("an unknown SHA")
            ),
            state
                .accepted_result
                .as_ref()
                .and_then(|value| bounded_serializable("accepted_result", value)),
        );
    }
    if state.status == RunStatus::Blocked {
        return (
            "blocked",
            format!(
                "The run stopped fail-closed: {}.",
                state.reason_code.as_deref().unwrap_or("UNKNOWN")
            ),
            state.last_error.as_ref().and_then(public_diagnostic),
        );
    }
    if state.status == RunStatus::PausedUserAction {
        return (
            "paused_user_action",
            format!(
                "The run is paused for user action: {}.",
                state.reason_code.as_deref().unwrap_or("UNKNOWN")
            ),
            state.last_error.as_ref().and_then(public_diagnostic),
        );
    }
    if state.status == RunStatus::Cancelled {
        return (
            "cancelled",
            "The run was cancelled without deleting existing Git state.".to_owned(),
            None,
        );
    }
    if state.status == RunStatus::IncompatibleCodex {
        return (
            "incompatible_codex",
            "The run stopped because the Codex App Server is incompatible.".to_owned(),
            state.last_error.as_ref().and_then(public_diagnostic),
        );
    }
    if previous.is_none() {
        return (
            "run_started",
            "The source tasks, worktrees, refs, and commits were frozen.".to_owned(),
            Some(json!({
                "primary_ref": state.facts.primary_ref,
                "primary_sha": state.facts.primary_sha,
                "reviewer_ref": state.facts.reviewer_ref,
                "reviewer_sha": state.facts.reviewer_sha,
                "target_integration_branch": state.target_integration_branch,
            })),
        );
    }
    let previous = previous.expect("previous state was checked above");

    if previous.primary_contract != state.primary_contract {
        return (
            "primary_contract_ready",
            "Primary declared its goals, constraints, interfaces, edge cases, and tests."
                .to_owned(),
            bounded_optional("primary_contract", &state.primary_contract),
        );
    }
    if previous.reviewer_contract != state.reviewer_contract {
        return (
            "reviewer_contract_ready",
            "Reviewer declared the implementation details and behaviors it requires preserved."
                .to_owned(),
            bounded_optional("reviewer_contract", &state.reviewer_contract),
        );
    }
    if previous.current_plan_payload != state.current_plan_payload {
        return (
            "plan_proposed",
            format!(
                "Primary submitted integration plan revision {} for review.",
                state.plan_revision.unwrap_or(1)
            ),
            bounded_optional("current_plan", &state.current_plan_payload),
        );
    }
    if previous.last_plan_feedback != state.last_plan_feedback {
        return (
            "plan_changes_required",
            format!(
                "Reviewer requested changes to the integration plan; review round {} is next.",
                state.round
            ),
            bounded_optional("plan_feedback", &state.last_plan_feedback),
        );
    }
    if previous.plan_approval_payload != state.plan_approval_payload {
        return (
            "plan_approved",
            format!(
                "Reviewer approved plan revision {}.",
                state.plan_revision.unwrap_or(1)
            ),
            bounded_optional("plan_approval", &state.plan_approval_payload),
        );
    }
    if previous.integration_sha != state.integration_sha {
        return (
            "integration_ready",
            format!(
                "Primary produced integration commit {} on {}.",
                state.integration_sha.as_deref().unwrap_or("an unknown SHA"),
                state
                    .integration_branch
                    .as_deref()
                    .or(state.target_integration_branch.as_deref())
                    .unwrap_or("the integration branch")
            ),
            bounded_optional("integration_summary", &state.current_integration_payload),
        );
    }
    if previous.test_evidence != state.test_evidence {
        let failed = state
            .test_evidence
            .iter()
            .filter(|evidence| evidence.exit_code != 0)
            .count();
        let summary = if failed == 0 {
            format!(
                "All {} frozen verification commands passed.",
                state.test_evidence.len()
            )
        } else {
            format!(
                "Verification completed with {failed} failed command(s); Primary must revise the same integration branch."
            )
        };
        return (
            if failed == 0 {
                "verification_passed"
            } else {
                "verification_failed"
            },
            summary,
            bounded_serializable("test_evidence", &state.test_evidence),
        );
    }
    if previous.last_result_feedback != state.last_result_feedback {
        return (
            "result_changes_required",
            format!(
                "Reviewer requested changes to the integrated result; review round {} is next.",
                state.round
            ),
            bounded_optional("result_feedback", &state.last_result_feedback),
        );
    }
    if previous.next_action != NextAction::RevalidateAndAccept
        && state.next_action == NextAction::RevalidateAndAccept
    {
        return (
            "result_approved",
            "Reviewer approved the exact verified integration SHA; final source checks are running."
                .to_owned(),
            None,
        );
    }
    if previous.status != state.status && state.status == RunStatus::WaitingThread {
        return (
            "waiting_for_task",
            format!(
                "Waiting for {} to complete the current {} action.",
                role_label(action_role(state.next_action)),
                action_label(state.next_action)
            ),
            None,
        );
    }
    if previous.status != state.status && state.status == RunStatus::Running {
        return (
            "task_completed",
            format!(
                "The participant turn completed; the coordinator is validating {} evidence.",
                action_label(state.next_action)
            ),
            None,
        );
    }
    (
        "state_changed",
        format!(
            "The run advanced to {} and will {}.",
            stage_name(state),
            action_label(state.next_action)
        ),
        None,
    )
}

fn bounded_optional(label: &str, value: &Option<Value>) -> Option<Value> {
    value
        .as_ref()
        .and_then(|value| bounded_value(label, value.clone()))
}

fn public_diagnostic(diagnostic: &RunDiagnostic) -> Option<Value> {
    bounded_value(
        "diagnostic",
        json!({
            "code": diagnostic.code,
            "operation": diagnostic.operation,
            "action": diagnostic.action,
            "role": diagnostic.role,
            "thread_id": diagnostic.thread_id,
            "source_thread_id": diagnostic.source_thread_id,
            "effective_thread_id": diagnostic.effective_thread_id,
            "participant_binding_generation": diagnostic.participant_binding_generation,
            "participant_binding_mode": diagnostic.participant_binding_mode,
            "participant_server": diagnostic.participant_server,
            "detail": "Detailed redacted diagnostics remain available through consensus_status."
        }),
    )
}

fn public_verification_artifact(label: &str, value: &Option<Value>) -> Option<Value> {
    let mut value = value.clone()?;
    scrub_verification_outputs(&mut value);
    bounded_value(label, value)
}

fn scrub_verification_outputs(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                scrub_verification_outputs(value);
            }
        }
        Value::Object(object) => {
            for key in ["verification_failures", "failed_tests"] {
                if let Some(Value::Array(failures)) = object.get_mut(key) {
                    for failure in failures {
                        if let Value::Object(failure) = failure {
                            failure.remove("output");
                            failure.remove("stdout");
                            failure.remove("stderr");
                        }
                    }
                }
            }
            for value in object.values_mut() {
                scrub_verification_outputs(value);
            }
        }
        _ => {}
    }
}

fn bounded_serializable(label: &str, value: &impl Serialize) -> Option<Value> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| bounded_value(label, value))
}

fn bounded_value(label: &str, value: Value) -> Option<Value> {
    let encoded = serde_json::to_vec(&value).ok()?;
    if encoded.len() <= MAX_PUBLIC_ARTIFACT_BYTES {
        Some(value)
    } else {
        Some(json!({
            "truncated": true,
            "artifact": label,
            "original_bytes": encoded.len(),
            "message": "The public artifact exceeded the live event limit; inspect consensus_status or the final report for the persisted value."
        }))
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn terminal_status(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Accepted
            | RunStatus::Blocked
            | RunStatus::Cancelled
            | RunStatus::IncompatibleCodex
    )
}

fn action_role(action: NextAction) -> Option<Role> {
    match action {
        NextAction::RequestPrimaryContract
        | NextAction::RequestPrimaryPlan
        | NextAction::RequestPrimaryIntegration
        | NextAction::RequestPrimaryVerification => Some(Role::Primary),
        NextAction::RequestReviewerContract
        | NextAction::RequestReviewerPlanVerdict
        | NextAction::RequestReviewerResultVerdict => Some(Role::Reviewer),
        NextAction::RevalidateAndAccept | NextAction::WaitForUser | NextAction::Stop => None,
    }
}

fn progress_stage(state: &RunState) -> ProgressStage {
    let (index, name) = match observable_phase(state) {
        Phase::Discover | Phase::Freeze => (1, "SOURCE_FREEZE"),
        Phase::Contract => (2, "CONTRACT"),
        Phase::PlanReview => (3, "PLAN_REVIEW"),
        Phase::Integrate => (4, "INTEGRATE"),
        Phase::Verify => (5, "VERIFY"),
        Phase::ResultReview => (6, "RESULT_REVIEW"),
        Phase::Accepted => (6, "ACCEPTED"),
        Phase::PausedUserAction => (stage_for_action(state.next_action), "PAUSED_USER_ACTION"),
        Phase::Blocked => (
            state
                .last_error
                .as_ref()
                .map(|diagnostic| stage_for_action(diagnostic.action))
                .unwrap_or(1),
            "BLOCKED",
        ),
        Phase::Cancelled => (stage_for_action(state.next_action), "CANCELLED"),
    };
    ProgressStage {
        index,
        total: 6,
        name: name.to_owned(),
    }
}

fn observable_phase(state: &RunState) -> Phase {
    if state.status == RunStatus::Accepted {
        Phase::Accepted
    } else {
        state.phase
    }
}

fn stage_for_action(action: NextAction) -> u8 {
    match action {
        NextAction::RequestPrimaryContract | NextAction::RequestReviewerContract => 2,
        NextAction::RequestPrimaryPlan | NextAction::RequestReviewerPlanVerdict => 3,
        NextAction::RequestPrimaryIntegration => 4,
        NextAction::RequestPrimaryVerification => 5,
        NextAction::RequestReviewerResultVerdict | NextAction::RevalidateAndAccept => 6,
        NextAction::WaitForUser | NextAction::Stop => 1,
    }
}

fn stage_name(state: &RunState) -> &'static str {
    match observable_phase(state) {
        Phase::Discover => "discovery",
        Phase::Freeze => "source freeze",
        Phase::Contract => "contract collection",
        Phase::PlanReview => "plan review",
        Phase::Integrate => "integration",
        Phase::Verify => "isolated verification",
        Phase::ResultReview => "result review",
        Phase::Accepted => "acceptance",
        Phase::Blocked => "blocked",
        Phase::PausedUserAction => "user action",
        Phase::Cancelled => "cancelled",
    }
}

fn role_label(role: Option<Role>) -> &'static str {
    match role {
        Some(Role::Primary) => "Primary",
        Some(Role::Reviewer) => "Reviewer",
        None => "the coordinator",
    }
}

fn action_label(action: NextAction) -> &'static str {
    match action {
        NextAction::RequestPrimaryContract => "collect the Primary contract",
        NextAction::RequestReviewerContract => "collect the Reviewer contract",
        NextAction::RequestPrimaryPlan => "request a complete integration plan",
        NextAction::RequestReviewerPlanVerdict => "request the plan verdict",
        NextAction::RequestPrimaryIntegration => "integrate on the new local branch",
        NextAction::RequestPrimaryVerification => "prepare isolated verification",
        NextAction::RequestReviewerResultVerdict => "request the final result verdict",
        NextAction::RevalidateAndAccept => "revalidate sources and accept the exact result",
        NextAction::WaitForUser => "wait for user action",
        NextAction::Stop => "stop",
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use consensus_core::state::{RunFacts, RunState, TestEvidence};
    use uuid::Uuid;

    use super::*;

    fn state() -> RunState {
        RunState::new(RunFacts {
            run_id: Uuid::parse_str("dfe0ff31-6e31-4e88-824a-fc6eeec69cd1").unwrap(),
            primary_thread_id: "primary".into(),
            reviewer_thread_id: "reviewer".into(),
            primary_worktree: PathBuf::from("/repo/primary"),
            reviewer_worktree: PathBuf::from("/repo/reviewer"),
            git_common_dir: PathBuf::from("/repo/.git"),
            primary_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            reviewer_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            primary_ref: Some("refs/heads/main".into()),
            reviewer_ref: Some("refs/heads/feature".into()),
        })
    }

    #[test]
    fn initial_event_exposes_frozen_sources_without_hidden_turn_data() {
        let state = state();
        let event = progress_event(None, &state, 123);

        assert_eq!(event.kind, "run_started");
        assert_eq!(event.progress.index, 1);
        assert_eq!(event.active_role, Some(Role::Primary));
        assert_eq!(event.created_at, 123);
        assert_eq!(
            event.artifact.as_ref().unwrap()["primary_sha"],
            state.facts.primary_sha
        );
    }

    #[test]
    fn snapshot_exposes_declared_artifacts_and_machine_progress() {
        let mut state = state();
        state.primary_contract = Some(json!({"goals": ["preserve behavior"]}));
        let snapshot = public_snapshot(&state, 7);

        assert_eq!(snapshot.cursor, 7);
        assert_eq!(snapshot.progress.name, "CONTRACT");
        assert_eq!(
            snapshot.artifacts.primary_contract.unwrap()["goals"][0],
            "preserve behavior"
        );
        assert!(snapshot.diagnostic.is_none());
    }

    #[test]
    fn oversized_artifacts_are_replaced_with_bounded_metadata() {
        let value = json!({"text": "x".repeat(MAX_PUBLIC_ARTIFACT_BYTES)});
        let bounded = bounded_value("plan", value).unwrap();

        assert_eq!(bounded["truncated"], true);
        assert_eq!(bounded["artifact"], "plan");

        let mut state = state();
        state.required_test_commands = vec!["x".repeat(MAX_PUBLIC_ARTIFACT_BYTES)];
        let snapshot = public_snapshot(&state, 1);
        assert_eq!(snapshot.artifacts.required_tests["truncated"], true);
        assert_eq!(
            snapshot.artifacts.required_tests["artifact"],
            "required_tests"
        );
    }

    #[test]
    fn corrective_sha_advance_is_not_misreported_as_zero_test_success() {
        let mut previous = state();
        previous.integration_sha = Some("c".repeat(40));
        previous.test_evidence = vec![TestEvidence {
            command: "cargo test".into(),
            exit_code: 1,
            turn_id: "turn-1".into(),
            item_id: "item-1".into(),
            cwd: PathBuf::from("/verify"),
        }];
        let mut corrected = previous.clone();
        corrected.integration_sha = Some("d".repeat(40));
        corrected.test_evidence.clear();
        corrected.current_integration_payload = Some(json!({"summary": "fixed"}));

        let event = progress_event(Some(&previous), &corrected, 456);

        assert_eq!(event.kind, "integration_ready");
        assert!(event.summary.contains(&"d".repeat(40)));
        assert!(!event.summary.contains("0 frozen verification"));
    }

    #[test]
    fn verification_with_the_same_sha_reports_test_exit_codes() {
        let mut previous = state();
        previous.integration_sha = Some("c".repeat(40));
        let mut verified = previous.clone();
        verified.test_evidence = vec![TestEvidence {
            command: "cargo test".into(),
            exit_code: 0,
            turn_id: "turn-1".into(),
            item_id: "item-1".into(),
            cwd: PathBuf::from("/verify"),
        }];
        verified.current_integration_payload = Some(json!({"summary": "verified"}));

        let event = progress_event(Some(&previous), &verified, 789);

        assert_eq!(event.kind, "verification_passed");
        assert_eq!(event.artifact.unwrap()[0]["exit_code"], 0);
    }

    #[test]
    fn public_snapshot_removes_coordinator_captured_command_output() {
        let mut state = state();
        state.current_integration_payload = Some(json!({
            "verification_failures": [{
                "command": "cargo test",
                "exit_code": 1,
                "item_id": "item-1",
                "output": "private captured output"
            }]
        }));
        state.last_result_feedback = Some(json!({
            "format": "machine_verification",
            "failed_tests": [{
                "command": "cargo test",
                "exit_code": 1,
                "stderr": "private captured stderr"
            }]
        }));

        let encoded = serde_json::to_string(&public_snapshot(&state, 1)).unwrap();

        assert!(!encoded.contains("private captured output"));
        assert!(!encoded.contains("private captured stderr"));
        assert!(!encoded.contains("\"output\""));
        assert!(!encoded.contains("\"stderr\""));
    }

    #[test]
    fn public_diagnostic_keeps_machine_identity_but_not_opaque_detail() {
        let mut state = state();
        state.record_error(RunDiagnostic {
            code: "COMMUNICATION_FAILURE".into(),
            detail: "private process stderr".into(),
            operation: Some("turn/start".into()),
            action: NextAction::RequestPrimaryContract,
            role: Some(Role::Primary),
            thread_id: Some("primary".into()),
            source_thread_id: None,
            effective_thread_id: None,
            participant_binding_generation: None,
            participant_binding_mode: None,
            participant_server: None,
        });

        let diagnostic = public_snapshot(&state, 1).diagnostic.unwrap();

        assert_eq!(diagnostic["code"], "COMMUNICATION_FAILURE");
        assert_eq!(diagnostic["operation"], "turn/start");
        assert!(!diagnostic.to_string().contains("private process stderr"));
    }
}
