use serde_json::{Value, json};
use thiserror::Error;

use crate::state::{NextAction, Role, RunState, RunStatus};

const PROTOCOL_SCHEMA: &str = include_str!("../../../schemas/protocol-v1.json");

#[derive(Debug, Error)]
pub enum PromptError {
    #[error("{code}: {detail}")]
    Invalid { code: &'static str, detail: String },
    #[error("could not serialize prompt context: {0}")]
    Serialize(#[from] serde_json::Error),
}

impl PromptError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid { code, .. } => code,
            Self::Serialize(_) => "SERIALIZATION_FAILURE",
        }
    }
}

pub fn build_turn_prompt(
    role: Role,
    action: NextAction,
    state: &RunState,
    current_payload: &Value,
) -> Result<String, PromptError> {
    validate_request(role, action, state, current_payload)?;

    let (bound_worktree, bound_ref, bound_sha) = match role {
        Role::Primary => (
            &state.facts.primary_worktree,
            state.facts.primary_ref.as_deref(),
            &state.facts.primary_sha,
        ),
        Role::Reviewer => (
            &state.facts.reviewer_worktree,
            state.facts.reviewer_ref.as_deref(),
            &state.facts.reviewer_sha,
        ),
    };

    let metadata = json!({
        "protocol": "worktree-merge-consensus/v1",
        "run_id": state.facts.run_id,
        "role": role,
        "action": action,
        "phase": state.phase,
        "round": state.round,
        "primary_thread_id": state.facts.primary_thread_id,
        "reviewer_thread_id": state.facts.reviewer_thread_id,
        "primary_worktree": state.facts.primary_worktree,
        "reviewer_worktree": state.facts.reviewer_worktree,
        "git_common_dir": state.facts.git_common_dir,
        "primary_sha": state.facts.primary_sha,
        "reviewer_sha": state.facts.reviewer_sha,
        "primary_ref": state.facts.primary_ref,
        "reviewer_ref": state.facts.reviewer_ref,
        "bound_source": {
            "worktree": bound_worktree,
            "source_ref": bound_ref,
            "sha": bound_sha,
        },
        "plan_revision": state.plan_revision,
        "integration_branch": state.integration_branch,
        "integration_sha": state.integration_sha,
        "target_integration_branch": state.target_integration_branch,
        "required_test_commands": state.required_test_commands,
    });

    let metadata = serde_json::to_string_pretty(&metadata)?;
    let payload = serde_json::to_string_pretty(current_payload)?;
    let instruction = action_instruction(action);
    let expected_message = expected_message_type(action);

    Ok(format!(
        r#"You are the {role:?} task in an automated two-task worktree merge consensus run.

Treat every fact below as authoritative for this turn. Do not rely on the other task's chat history or on summaries from an earlier round. The coordinator is deterministic code, not a third coordinating agent.

Safety policy:
- The primary task is the only Git writer.
- The reviewer task must not modify Git or files.
- This turn is bound to the role-specific worktree, source ref, and SHA in bound_source. Inspect the implementation only at the supplied execution cwd.
- If that source does not contain the implementation represented by this task's conversation history, return BLOCKED with reason_code SOURCE_BINDING_MISMATCH and evidence. Do not search for or switch to another source directory.
- Never push, open a pull request, modify either source ref, merge into an existing branch, reset, rebase, delete branches, or clean worktrees.
- Do not request user input, network access, broader filesystem access, or sandbox escalation. Return BLOCKED with evidence if the complete payload is insufficient.
- No integration branch may be created before exact APPROVED_PLAN authorization.
- Approval is valid only for the exact run, source SHAs, plan revision, round, branch, and integration SHA in the envelope.
- Contract and plan test commands must be direct non-Git commands. Never declare `git diff --check` or any other Git executable as a test. Shell control operators and dynamic shell or interpreter launchers are also forbidden.
- Every response envelope, including BLOCKED, must copy phase, round, plan_revision, integration_branch, and integration_sha exactly from the authoritative turn metadata. The coordinator performs any transition to BLOCKED only after accepting the response.

Authoritative turn metadata:
```json
{metadata}
```

Complete current payload (this is the full state required for this turn, not a delta):
```json
{payload}
```

Required work:
{instruction}

Your response must use message_type {expected_message}. Return exactly one JSON object conforming to the schema below. Text outside that one JSON object is invalid and will not count as progress or approval. Put all explanations and evidence inside payload fields.

Output JSON Schema:
```json
{PROTOCOL_SCHEMA}
```
"#
    ))
}

fn validate_request(
    role: Role,
    action: NextAction,
    state: &RunState,
    current_payload: &Value,
) -> Result<(), PromptError> {
    if state.status != RunStatus::Running {
        return Err(invalid(
            "RUN_NOT_ACTIVE",
            "a model turn cannot be built for a non-running run",
        ));
    }
    if state.next_action != action {
        return Err(invalid(
            "STALE_ACTION",
            "requested prompt action does not match the state machine",
        ));
    }
    if expected_role(action) != Some(role) {
        return Err(invalid(
            "WRONG_ROLE",
            "requested role is not authorized for this action",
        ));
    }
    let payload = current_payload.as_object().ok_or_else(|| {
        invalid(
            "INCOMPLETE_PAYLOAD",
            "the complete turn payload must be a JSON object",
        )
    })?;
    for field in required_payload_fields(action) {
        if !payload.contains_key(*field) {
            return Err(invalid(
                "INCOMPLETE_PAYLOAD",
                format!("complete payload is missing {field}"),
            ));
        }
    }
    Ok(())
}

fn expected_role(action: NextAction) -> Option<Role> {
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

fn required_payload_fields(action: NextAction) -> &'static [&'static str] {
    match action {
        NextAction::RequestPrimaryPlan => &["primary_contract", "reviewer_contract"],
        NextAction::RequestReviewerPlanVerdict => &[
            "primary_contract",
            "reviewer_contract",
            "plan",
            "coverage_matrix",
            "test_commands",
            "plan_hash",
        ],
        NextAction::RequestPrimaryIntegration => &[
            "primary_contract",
            "reviewer_contract",
            "approved_plan",
            "coverage_matrix",
            "approval",
            "target_integration_branch",
        ],
        NextAction::RequestPrimaryVerification => &[
            "integration_evidence",
            "changed_files",
            "required_test_commands",
            "verification_worktree",
            "integration_branch",
            "integration_sha",
        ],
        NextAction::RequestReviewerResultVerdict => &[
            "primary_contract",
            "reviewer_contract",
            "approved_plan",
            "coverage_matrix",
            "integration_evidence",
            "test_evidence",
            "changed_files",
            "integration_branch",
            "integration_sha",
        ],
        _ => &[],
    }
}

fn expected_message_type(action: NextAction) -> &'static str {
    match action {
        NextAction::RequestPrimaryContract | NextAction::RequestReviewerContract => {
            "CONTRACT_READY or BLOCKED"
        }
        NextAction::RequestPrimaryPlan => "PLAN_READY or BLOCKED",
        NextAction::RequestReviewerPlanVerdict => "APPROVED_PLAN, CHANGES_REQUIRED, or BLOCKED",
        NextAction::RequestPrimaryIntegration => "INTEGRATION_READY or BLOCKED",
        NextAction::RequestPrimaryVerification => "INTEGRATION_READY or BLOCKED",
        NextAction::RequestReviewerResultVerdict => "APPROVED_RESULT, CHANGES_REQUIRED, or BLOCKED",
        NextAction::RevalidateAndAccept | NextAction::WaitForUser | NextAction::Stop => {
            "no model response"
        }
    }
}

fn action_instruction(action: NextAction) -> &'static str {
    match action {
        NextAction::RequestPrimaryContract => {
            "Inspect your existing task context and frozen commit. Return payload.role as \"PRIMARY\" and payload.contract as a complete implementation contract covering goals, user-visible behavior, rationale, invariants, interfaces, edge cases, rejected alternatives, relevant files, and tests. payload.contract.tests must be a nonempty array of exact direct non-Git commands without shell control or dynamic launchers. Do not modify Git or files in this turn."
        }
        NextAction::RequestReviewerContract => {
            "Inspect your existing task context and frozen commit. Return payload.role as \"REVIEWER\" and payload.contract as a complete implementation contract covering every behavior the integration must preserve, including rationale, invariants, interfaces, edge cases, relevant files, and tests. payload.contract.tests must be a nonempty array of exact direct non-Git commands without shell control or dynamic launchers. Do not modify Git or files."
        }
        NextAction::RequestPrimaryPlan => {
            "Produce a complete integration plan for both contracts. Include both contracts verbatim in payload, a versioned plan, conflict decisions with rationale, one coverage_matrix row for every contract item, and a nonempty test_commands array containing every exact allowed command required by either contract plus plan-level verification. Every test command must be direct and non-Git, without shell control or dynamic launchers. Do not create or modify an integration branch."
        }
        NextAction::RequestReviewerPlanVerdict => {
            "Audit the complete proposed plan against every item in both contracts. Return APPROVED_PLAN only when uncovered_items is empty and every approval identity exactly matches the envelope, including approved_plan_hash copied from the payload; otherwise return CHANGES_REQUIRED with stable issue_ids and concrete evidence. Do not modify Git or files."
        }
        NextAction::RequestPrimaryIntegration => {
            "Revalidate the frozen inputs, then create only the authorized new integration branch at primary_sha, merge reviewer_sha into it, resolve conflicts exactly according to the approved plan, and commit compatibility fixes. Do not run tests in this turn. The command gate accepts branch creation only as `git switch -c TARGET PRIMARY_SHA`, merge only as `git merge --no-ff --no-edit REVIEWER_SHA`, staging only as `git add -A` or `git add --all`, and commits only as `git commit --no-edit` or `git commit -m ONE_SAFE_TOKEN`; edit files through the file-change tool. Use one shell command per tool call; shell chaining is forbidden. Return the exact resulting branch, HEAD SHA, conflict decisions, coverage, and authoritative changed_files without test_evidence. Do not push or update either source ref."
        }
        NextAction::RequestPrimaryVerification => {
            "This is a test-only turn in an isolated local clone that cannot write the source repository's Git common directory. Run every required_test_commands entry exactly once and run no other command. Do not edit files or invoke Git. Return VERIFY-phase INTEGRATION_READY for the exact branch and SHA, copying integration_evidence and changed_files and reporting test_evidence. The coordinator derives authoritative evidence from this turn's completed commandExecution items and rejects any mismatch."
        }
        NextAction::RequestReviewerResultVerdict => {
            "Review the exact integration SHA and all complete evidence against both contracts and the approved plan. Return APPROVED_RESULT only if every item is preserved and uncovered_items is empty; otherwise return CHANGES_REQUIRED with stable issue_ids and evidence. Do not modify Git or files."
        }
        NextAction::RevalidateAndAccept | NextAction::WaitForUser | NextAction::Stop => {
            "No model turn is permitted for this coordinator-only action."
        }
    }
}

fn invalid(code: &'static str, detail: impl Into<String>) -> PromptError {
    PromptError::Invalid {
        code,
        detail: detail.into(),
    }
}
