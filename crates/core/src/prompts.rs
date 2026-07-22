use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    participant::PARTICIPANT_PROTOCOL_V2,
    state::{NextAction, Role, RunState, RunStatus},
};

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
        "participant_response_protocol": PARTICIPANT_PROTOCOL_V2,
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
    let output_contract = action_output_contract(action);

    Ok(format!(
        r#"You are the {role:?} task in an automated two-task worktree merge consensus run.

This is an internal participant turn inside an already-running run, not a request to launch, select, start, monitor, resume, or control a consensus run. The `worktree-merge-consensus` launcher skill is out of scope for this turn: do not select it, read its `SKILL.md`, or invoke its MCP or CLI surface. Follow only the complete coordinator payload below.

Treat every fact below as authoritative for this turn. Do not rely on the other task's chat history or on summaries from an earlier round. The coordinator is deterministic code, not a third coordinating agent.

Safety policy:
- The primary task is the only Git writer.
- The reviewer task must not modify Git or files.
- This turn is bound to the role-specific worktree, source ref, and SHA in bound_source. Inspect the implementation only at the supplied execution cwd.
- If that source does not contain the implementation represented by this task's conversation history, return BLOCKED with reason_code SOURCE_BINDING_MISMATCH and evidence. Do not search for or switch to another source directory.
- Never push, open a pull request, modify either source ref, merge into an existing branch, reset, rebase, delete branches, or clean worktrees.
- Do not request user input, network access, broader filesystem access, or sandbox escalation. Return BLOCKED with evidence if the complete payload is insufficient.
- Do not call `worktreeMergeConsensus` or any `consensus_*` tool from this task turn, except for the single `consensus_apply_patch` call explicitly authorized by the primary integration instruction. The coordinator already supplied the frozen identity and complete payload.
- No integration branch may be created before the reviewer returns the exact APPROVED marker for the current plan turn.
- The coordinator binds every response to this exact task turn and supplies run identity, source SHAs, plan revision, plan hash, integration branch, and integration SHA itself. Do not repeat or invent those machine fields.
- Contract and plan test commands must be direct non-Git commands. Never declare `git diff --check` or any other Git executable as a test. Shell control operators and dynamic shell or interpreter launchers are also forbidden.

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

{output_contract}
Do not return the legacy v1 protocol envelope. The coordinator parses only the one result marker described above. Except for the CONTRACT_READY JSON body, all remaining response text is free-form Markdown: it is preserved and relayed verbatim but is not parsed into fields.
"#
    ))
}

fn action_output_contract(action: NextAction) -> &'static str {
    match action {
        NextAction::RequestPrimaryContract | NextAction::RequestReviewerContract => {
            r#"Response format:
- Include exactly one `<consensus-result>CONTRACT_READY</consensus-result>` marker, or `<consensus-result>BLOCKED:REASON_CODE</consensus-result>` when blocked.
- For CONTRACT_READY, place exactly one JSON object after the marker. That object is the contract itself, not a protocol envelope. It must contain a nonempty `tests` array of exact direct commands; use ordinary JSON fields for goals, behavior, rationale, invariants, interfaces, edge cases, rejected alternatives, and relevant files.
- For BLOCKED, write ordinary Markdown evidence after the marker; no JSON is required.
"#
        }
        NextAction::RequestPrimaryPlan => {
            r#"Response format:
- Include exactly one `<consensus-result>PLAN_READY</consensus-result>` marker, or `<consensus-result>BLOCKED:REASON_CODE</consensus-result>` when blocked.
- After PLAN_READY, write the complete proposed integration plan as free-form Markdown. Do not return JSON. Include all decisions and verification reasoning needed by the reviewer; never send only a delta.
"#
        }
        NextAction::RequestReviewerPlanVerdict => {
            r#"Response format:
- Include exactly one of `<consensus-result>APPROVED</consensus-result>`, `<consensus-result>CHANGES_REQUIRED</consensus-result>`, or `<consensus-result>BLOCKED:REASON_CODE</consensus-result>`.
- For CHANGES_REQUIRED, write concrete free-form Markdown feedback after the marker. For APPROVED, explanation is optional. Do not return JSON or repeat run, revision, hash, or SHA fields.
"#
        }
        NextAction::RequestPrimaryIntegration => {
            r#"Response format:
- Include exactly one `<consensus-result>INTEGRATION_READY</consensus-result>` marker, or `<consensus-result>BLOCKED:REASON_CODE</consensus-result>` when blocked.
- After INTEGRATION_READY, summarize decisions as free-form Markdown. Do not report branch, SHA, changed-file, or test fields: the coordinator derives those facts directly from Git.
"#
        }
        NextAction::RequestPrimaryVerification => {
            r#"Response format:
- Include exactly one `<consensus-result>VERIFICATION_READY</consensus-result>` marker, or `<consensus-result>BLOCKED:REASON_CODE</consensus-result>` when blocked.
- Any explanation after the marker is free-form Markdown. Do not report test result fields: the coordinator derives authoritative evidence from command items.
"#
        }
        NextAction::RequestReviewerResultVerdict => {
            r#"Response format:
- Include exactly one of `<consensus-result>APPROVED</consensus-result>`, `<consensus-result>CHANGES_REQUIRED</consensus-result>`, or `<consensus-result>BLOCKED:REASON_CODE</consensus-result>`.
- For CHANGES_REQUIRED, write concrete free-form Markdown feedback after the marker. For APPROVED, explanation is optional. Do not return JSON or repeat the integration branch or SHA; this turn is already bound to the exact result.
"#
        }
        NextAction::RevalidateAndAccept | NextAction::WaitForUser | NextAction::Stop => "",
    }
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

fn action_instruction(action: NextAction) -> &'static str {
    match action {
        NextAction::RequestPrimaryContract => {
            "Inspect your existing task context and frozen commit. Produce a complete implementation contract covering goals, user-visible behavior, rationale, invariants, interfaces, edge cases, rejected alternatives, relevant files, and tests. The contract JSON `tests` field must be a nonempty array of exact direct non-Git commands without shell control or dynamic launchers. Do not modify Git or files in this turn."
        }
        NextAction::RequestReviewerContract => {
            "Inspect your existing task context and frozen commit. Produce a complete implementation contract covering every behavior the integration must preserve, including rationale, invariants, interfaces, edge cases, relevant files, and tests. The contract JSON `tests` field must be a nonempty array of exact direct non-Git commands without shell control or dynamic launchers. Do not modify Git or files."
        }
        NextAction::RequestPrimaryPlan => {
            "Produce a complete integration plan for both contracts in free-form Markdown. Explain how every behavior is preserved, how conflicts will be resolved, and how the frozen contract and user test commands verify the result. The coordinator already owns the structured contracts and test list, so do not repeat them as JSON. Do not create or modify an integration branch."
        }
        NextAction::RequestReviewerPlanVerdict => {
            "Audit the complete proposed plan against every item in both contracts. Return APPROVED only when nothing is uncovered; otherwise return CHANGES_REQUIRED and explain every concrete gap in free-form Markdown. The coordinator binds the verdict to the exact current plan automatically. Do not modify Git or files."
        }
        NextAction::RequestPrimaryIntegration => {
            "Revalidate the frozen inputs, then create only the authorized new integration branch at primary_sha, merge reviewer_sha into it, resolve conflicts exactly according to the approved plan, and commit compatibility fixes. If the exact target branch already exists because this is a coordinator-authorized retry, do not recreate or re-merge it: verify that HEAD is attached to that target, both frozen SHAs are ancestors, the worktree is clean, and continue from that exact HEAD. Do not run tests in this turn. Required repository-instruction discovery is allowed only as `rg --files -g AGENTS.md`. Perform every other repository inspection through the command gate's read-only Git queries: use `git show REV:path` to read tracked content, `git ls-files` to list tracked paths, and `git diff` to inspect changes; do not invoke sed, cat, find, ls, head, tail, or another shell file reader. The command gate accepts branch creation only as `git switch -c TARGET PRIMARY_SHA`, merge only as `git merge --no-ff --no-edit REVIEWER_SHA`, staging only as `git add -A` or `git add --all`, and commits only as `git commit --no-edit` or `git commit -m ONE_SAFE_TOKEN`. Make all compatibility edits in one successful call to `consensus_apply_patch`, passing the authoritative run_id, the exact request_hash from Coordinator delivery identity, and one raw unified text patch. A failed patch preflight leaves the worktree clean and may be corrected; after one successful patch, no second patch is authorized. This is the only authorized `consensus_*` call; do not use the built-in file-change tool, and do not call start, status, resume, cancel, discovery, or another coordinator tool. The controlled patch is accepted only after the clean authorized merge. Use one shell command per tool call; shell chaining is forbidden. Return only the INTEGRATION_READY marker plus optional free-form summary; the coordinator reads the final branch, HEAD, and changed files directly from Git. Do not push or update either source ref."
        }
        NextAction::RequestPrimaryVerification => {
            "This is a test-only turn in an isolated local clone that cannot write the source repository's Git common directory. Invoke the command-execution tool once for each required_test_commands entry, in the listed order, using each exact command as one standalone tool call. Wait for all command calls to finish. Run every entry exactly once and run no other command. Do not edit files or invoke Git. Do not return VERIFICATION_READY before all required commandExecution items exist and completed successfully; a final answer without those tool items is invalid evidence. Then return only the VERIFICATION_READY marker plus optional free-form summary. The coordinator derives authoritative evidence from this turn's completed commandExecution items."
        }
        NextAction::RequestReviewerResultVerdict => {
            "Review the exact integration SHA and all complete evidence against both contracts and the approved plan. Return APPROVED only if every item is preserved; otherwise return CHANGES_REQUIRED with concrete free-form Markdown feedback. The coordinator binds the verdict to the exact current branch and SHA automatically. Do not modify Git or files."
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
