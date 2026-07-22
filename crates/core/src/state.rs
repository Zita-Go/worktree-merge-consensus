use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    canonical_json_hash,
    protocol::{MessagePhase, MessageType, ProtocolMessage, validate_message},
};

pub const DEFAULT_MAX_REVIEW_ROUNDS: u32 = 6;
pub const DEFAULT_NO_PROGRESS_ROUNDS: u8 = 2;
pub const RUN_STATE_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Phase {
    Discover,
    Freeze,
    Contract,
    PlanReview,
    Integrate,
    Verify,
    ResultReview,
    Accepted,
    Blocked,
    PausedUserAction,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RunStatus {
    Running,
    WaitingThread,
    PausedUserAction,
    Accepted,
    Blocked,
    Cancelled,
    IncompatibleCodex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Role {
    Primary,
    Reviewer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NextAction {
    RequestPrimaryContract,
    RequestReviewerContract,
    RequestPrimaryPlan,
    RequestReviewerPlanVerdict,
    RequestPrimaryIntegration,
    RequestPrimaryVerification,
    RequestReviewerResultVerdict,
    RevalidateAndAccept,
    WaitForUser,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFacts {
    pub run_id: Uuid,
    pub primary_thread_id: String,
    pub reviewer_thread_id: String,
    pub primary_worktree: PathBuf,
    pub reviewer_worktree: PathBuf,
    pub git_common_dir: PathBuf,
    pub primary_sha: String,
    pub reviewer_sha: String,
    pub primary_ref: Option<String>,
    pub reviewer_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestEvidence {
    pub command: String,
    pub exit_code: i64,
    pub turn_id: String,
    pub item_id: String,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationBoundary {
    pub local_only: bool,
    pub pushed: bool,
    pub pull_request_created: bool,
    pub merged_into_existing_branch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedResult {
    pub integration_branch: String,
    pub integration_sha: String,
    pub primary_sha: String,
    pub reviewer_sha: String,
    pub tests: Vec<TestEvidence>,
    pub source_refs_unchanged: bool,
    pub publication: PublicationBoundary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunDiagnostic {
    pub code: String,
    pub detail: String,
    pub operation: Option<String>,
    pub action: NextAction,
    pub role: Option<Role>,
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunState {
    pub schema_version: u32,
    pub facts: RunFacts,
    pub phase: Phase,
    pub status: RunStatus,
    pub round: u32,
    pub plan_revision: Option<u32>,
    pub integration_branch: Option<String>,
    pub integration_sha: Option<String>,
    pub reason_code: Option<String>,
    #[serde(default)]
    pub last_error: Option<RunDiagnostic>,
    pub next_action: NextAction,
    pub target_integration_branch: Option<String>,
    pub required_test_commands: Vec<String>,
    pub test_evidence: Vec<TestEvidence>,
    pub accepted_result: Option<AcceptedResult>,
    pub primary_contract: Option<Value>,
    pub reviewer_contract: Option<Value>,
    pub current_plan_payload: Option<Value>,
    pub plan_approval_payload: Option<Value>,
    pub current_integration_payload: Option<Value>,
    pub last_plan_feedback: Option<Value>,
    pub last_result_feedback: Option<Value>,
    pub verification_worktree: Option<PathBuf>,
    pub max_review_rounds: u32,
    pub no_progress_rounds: u8,
    primary_contract_hash: Option<String>,
    reviewer_contract_hash: Option<String>,
    current_plan_hash: Option<String>,
    last_review_fingerprint: Option<String>,
    unchanged_review_streak: u8,
    plan_approved: bool,
    result_approved_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{code}: {detail}")]
pub struct StateError {
    code: &'static str,
    detail: String,
}

impl StateError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl RunState {
    pub fn new(facts: RunFacts) -> Self {
        Self {
            schema_version: RUN_STATE_SCHEMA_VERSION,
            facts,
            phase: Phase::Contract,
            status: RunStatus::Running,
            round: 1,
            plan_revision: None,
            integration_branch: None,
            integration_sha: None,
            reason_code: None,
            last_error: None,
            next_action: NextAction::RequestPrimaryContract,
            target_integration_branch: None,
            required_test_commands: Vec::new(),
            test_evidence: Vec::new(),
            accepted_result: None,
            primary_contract: None,
            reviewer_contract: None,
            current_plan_payload: None,
            plan_approval_payload: None,
            current_integration_payload: None,
            last_plan_feedback: None,
            last_result_feedback: None,
            verification_worktree: None,
            max_review_rounds: DEFAULT_MAX_REVIEW_ROUNDS,
            no_progress_rounds: DEFAULT_NO_PROGRESS_ROUNDS,
            primary_contract_hash: None,
            reviewer_contract_hash: None,
            current_plan_hash: None,
            last_review_fingerprint: None,
            unchanged_review_streak: 0,
            plan_approved: false,
            result_approved_sha: None,
        }
    }

    pub fn configure_integration(
        &mut self,
        target_branch: impl Into<String>,
        test_commands: Vec<String>,
    ) -> Result<(), StateError> {
        self.require_running()?;
        if self.phase != Phase::Contract
            || self.next_action != NextAction::RequestPrimaryContract
            || self.target_integration_branch.is_some()
        {
            return Err(state_error(
                "POLICY_ALREADY_FROZEN",
                "integration policy can only be configured once before the first turn",
            ));
        }
        let target_branch = target_branch.into();
        if target_branch.trim().is_empty() {
            return Err(state_error(
                "INVALID_BRANCH_NAME",
                "target integration branch cannot be empty",
            ));
        }
        if test_commands
            .iter()
            .any(|command| command.trim().is_empty())
        {
            return Err(state_error(
                "INVALID_TEST_COMMAND",
                "required test commands cannot be empty",
            ));
        }
        let mut normalized_tests = Vec::new();
        for command in test_commands {
            let command = command.trim().to_owned();
            if !normalized_tests.contains(&command) {
                normalized_tests.push(command);
            }
        }
        self.target_integration_branch = Some(target_branch);
        self.required_test_commands = normalized_tests;
        Ok(())
    }

    pub fn apply_message(&mut self, message: ProtocolMessage) -> Result<NextAction, StateError> {
        self.require_running()?;
        let message = revalidate_message(message)?;
        self.verify_immutable_envelope(&message)?;

        match message.envelope.message_type {
            MessageType::ContractReady => self.apply_contract(message),
            MessageType::PlanReady => self.apply_plan(message),
            MessageType::ChangesRequired => self.apply_changes_required(message),
            MessageType::ApprovedPlan => self.apply_plan_approval(message),
            MessageType::IntegrationReady => self.apply_integration(message),
            MessageType::ApprovedResult => self.apply_result_approval(message),
            MessageType::Blocked => {
                self.verify_blocked_envelope(&message)?;
                let reason = message
                    .envelope
                    .reason_code
                    .as_deref()
                    .unwrap_or("INVALID_RESPONSE");
                Ok(self.block(reason))
            }
        }
    }

    pub fn record_plan(&mut self, payload: Value) -> Result<NextAction, StateError> {
        self.require_running()?;
        self.require_phase(Phase::PlanReview)?;
        if !payload.is_object() {
            return Err(state_error(
                "INVALID_RESPONSE",
                "the primary plan payload must be a JSON object",
            ));
        }

        if payload.get("test_commands").is_some() {
            let commands = string_array_field(&payload, "test_commands")?;
            self.freeze_test_commands(commands);
        }

        self.current_plan_hash = Some(canonical_json_hash(&payload));
        self.current_plan_payload = Some(payload);
        self.next_action = NextAction::RequestReviewerPlanVerdict;
        Ok(self.next_action)
    }

    pub fn request_integration(&mut self) -> Result<NextAction, StateError> {
        self.require_running()?;
        self.require_phase(Phase::PlanReview)?;
        if !self.plan_approved {
            return Err(state_error(
                "PLAN_NOT_APPROVED",
                "an exact APPROVED_PLAN is required before integration",
            ));
        }

        self.phase = Phase::Integrate;
        self.next_action = NextAction::RequestPrimaryIntegration;
        Ok(self.next_action)
    }

    pub fn pause(&mut self, reason_code: impl Into<String>) -> Result<(), StateError> {
        if !matches!(self.status, RunStatus::Running | RunStatus::WaitingThread) {
            return Err(state_error(
                "RUN_NOT_ACTIVE",
                "only a running or waiting run can be paused",
            ));
        }
        self.status = RunStatus::PausedUserAction;
        self.reason_code = Some(reason_code.into());
        Ok(())
    }

    pub fn record_error(&mut self, diagnostic: RunDiagnostic) {
        self.last_error = Some(diagnostic);
    }

    pub fn wait_for_thread(&mut self) -> Result<(), StateError> {
        self.require_running()?;
        self.status = RunStatus::WaitingThread;
        Ok(())
    }

    pub fn thread_became_idle(&mut self) -> Result<(), StateError> {
        if self.status != RunStatus::WaitingThread {
            return Err(state_error(
                "NOT_WAITING_THREAD",
                "run is not waiting for a task turn",
            ));
        }
        self.status = RunStatus::Running;
        Ok(())
    }

    pub fn resume(&mut self) -> Result<NextAction, StateError> {
        if self.status != RunStatus::PausedUserAction {
            return Err(state_error(
                "NOT_PAUSED",
                "only a PAUSED_USER_ACTION run can be resumed",
            ));
        }
        self.status = RunStatus::Running;
        self.reason_code = None;
        self.last_error = None;
        Ok(self.next_action)
    }

    pub fn retry_blocked_invalid_test_command(&mut self) -> Result<NextAction, StateError> {
        if self.status != RunStatus::Blocked
            || self.reason_code.as_deref() != Some("INVALID_TEST_COMMAND")
        {
            return Err(state_error(
                "NOT_RETRYABLE",
                "only a legacy INVALID_TEST_COMMAND blocked run can be retried",
            ));
        }
        let diagnostic = self.last_error.as_ref().ok_or_else(|| {
            state_error(
                "INCOMPATIBLE_STATE",
                "blocked invalid-test state has no originating diagnostic",
            )
        })?;
        if diagnostic.code != "INVALID_TEST_COMMAND" {
            return Err(state_error(
                "INCOMPATIBLE_STATE",
                "blocked invalid-test reason does not match its diagnostic",
            ));
        }
        let phase = match diagnostic.action {
            NextAction::RequestPrimaryContract | NextAction::RequestReviewerContract => {
                Phase::Contract
            }
            NextAction::RequestPrimaryPlan => Phase::PlanReview,
            _ => {
                return Err(state_error(
                    "NOT_RETRYABLE",
                    "invalid test-command recovery is limited to pre-integration declaration turns",
                ));
            }
        };
        if self.integration_branch.is_some()
            || self.integration_sha.is_some()
            || self.current_integration_payload.is_some()
        {
            return Err(state_error(
                "NOT_RETRYABLE",
                "invalid test-command recovery cannot rewrite a run after integration begins",
            ));
        }

        self.status = RunStatus::Running;
        self.phase = phase;
        self.next_action = diagnostic.action;
        self.reason_code = None;
        self.last_error = None;
        self.validate_persisted()?;
        Ok(self.next_action)
    }

    pub fn retry_blocked_preintegration_invalid_response(
        &mut self,
    ) -> Result<NextAction, StateError> {
        if self.status != RunStatus::Blocked
            || self.reason_code.as_deref() != Some("INVALID_RESPONSE")
        {
            return Err(state_error(
                "NOT_RETRYABLE",
                "only a pre-integration INVALID_RESPONSE blocked run can be retried",
            ));
        }
        let diagnostic = self.last_error.as_ref().ok_or_else(|| {
            state_error(
                "INCOMPATIBLE_STATE",
                "blocked invalid-response state has no originating diagnostic",
            )
        })?;
        if diagnostic.code != "INVALID_RESPONSE" {
            return Err(state_error(
                "INCOMPATIBLE_STATE",
                "blocked invalid-response reason does not match its diagnostic",
            ));
        }
        let phase = match diagnostic.action {
            NextAction::RequestPrimaryContract | NextAction::RequestReviewerContract => {
                Phase::Contract
            }
            NextAction::RequestPrimaryPlan | NextAction::RequestReviewerPlanVerdict => {
                Phase::PlanReview
            }
            _ => {
                return Err(state_error(
                    "NOT_RETRYABLE",
                    "invalid-response recovery is limited to pre-integration read-only turns",
                ));
            }
        };
        if self.integration_branch.is_some()
            || self.integration_sha.is_some()
            || self.current_integration_payload.is_some()
        {
            return Err(state_error(
                "NOT_RETRYABLE",
                "invalid-response recovery cannot rewrite a run after integration begins",
            ));
        }

        self.status = RunStatus::Running;
        self.phase = phase;
        self.next_action = diagnostic.action;
        self.reason_code = None;
        self.last_error = None;
        self.validate_persisted()?;
        Ok(self.next_action)
    }

    pub fn retry_blocked_integration_invalid_response(&mut self) -> Result<NextAction, StateError> {
        if self.status != RunStatus::Blocked
            || self.phase != Phase::Blocked
            || self.next_action != NextAction::Stop
            || self.reason_code.as_deref() != Some("INVALID_RESPONSE")
        {
            return Err(state_error(
                "NOT_RETRYABLE",
                "only a terminal INVALID_RESPONSE integration result can be retried",
            ));
        }
        let diagnostic = self.last_error.as_ref().ok_or_else(|| {
            state_error(
                "INCOMPATIBLE_STATE",
                "integration invalid-response recovery requires its originating diagnostic",
            )
        })?;
        if diagnostic.code != "INVALID_RESPONSE"
            || diagnostic.action != NextAction::RequestPrimaryIntegration
            || diagnostic.role != Some(Role::Primary)
            || diagnostic.thread_id.as_deref() != Some(self.facts.primary_thread_id.as_str())
        {
            return Err(state_error(
                "NOT_RETRYABLE",
                "invalid-response recovery is limited to the bound primary integration turn",
            ));
        }
        if !self.plan_approved
            || self.current_plan_payload.is_none()
            || self.plan_approval_payload.is_none()
            || self.target_integration_branch.is_none()
        {
            return Err(state_error(
                "INCOMPATIBLE_STATE",
                "integration invalid-response recovery requires an approved frozen plan",
            ));
        }
        if self.integration_branch.is_some()
            || self.integration_sha.is_some()
            || self.current_integration_payload.is_some()
            || self.verification_worktree.is_some()
            || !self.test_evidence.is_empty()
        {
            return Err(state_error(
                "NOT_RETRYABLE",
                "integration invalid-response recovery cannot replace an accepted result",
            ));
        }

        self.status = RunStatus::Running;
        self.phase = Phase::Integrate;
        self.next_action = NextAction::RequestPrimaryIntegration;
        self.reason_code = None;
        self.last_error = None;
        self.validate_persisted()?;
        Ok(self.next_action)
    }

    pub fn retry_blocked_integration_execution_tool_unavailable(
        &mut self,
    ) -> Result<NextAction, StateError> {
        if self.status != RunStatus::Blocked
            || self.phase != Phase::Blocked
            || self.next_action != NextAction::Stop
            || self.reason_code.as_deref() != Some("EXECUTION_TOOL_UNAVAILABLE")
        {
            return Err(state_error(
                "NOT_RETRYABLE",
                "only a terminal EXECUTION_TOOL_UNAVAILABLE integration blocker can be retried",
            ));
        }
        if !self.plan_approved
            || self.current_plan_payload.is_none()
            || self.plan_approval_payload.is_none()
            || self.target_integration_branch.is_none()
        {
            return Err(state_error(
                "INCOMPATIBLE_STATE",
                "execution-tool recovery requires an approved frozen integration plan",
            ));
        }
        if self.integration_branch.is_some()
            || self.integration_sha.is_some()
            || self.current_integration_payload.is_some()
            || self.verification_worktree.is_some()
            || !self.test_evidence.is_empty()
        {
            return Err(state_error(
                "NOT_RETRYABLE",
                "execution-tool recovery is limited to a side-effect-free pre-integration blocker",
            ));
        }

        self.status = RunStatus::Running;
        self.phase = Phase::Integrate;
        self.next_action = NextAction::RequestPrimaryIntegration;
        self.reason_code = None;
        self.last_error = None;
        self.validate_persisted()?;
        Ok(self.next_action)
    }

    pub fn retry_blocked_preintegration_forbidden_operation(
        &mut self,
    ) -> Result<NextAction, StateError> {
        if self.status != RunStatus::Blocked
            || self.phase != Phase::Blocked
            || self.next_action != NextAction::Stop
            || self.reason_code.as_deref() != Some("FORBIDDEN_OPERATION")
        {
            return Err(state_error(
                "NOT_RETRYABLE",
                "only a terminal pre-integration FORBIDDEN_OPERATION can be retried",
            ));
        }
        let diagnostic = self.last_error.as_ref().ok_or_else(|| {
            state_error(
                "INCOMPATIBLE_STATE",
                "forbidden-operation recovery requires its originating diagnostic",
            )
        })?;
        if diagnostic.code != "FORBIDDEN_OPERATION"
            || diagnostic.action != NextAction::RequestPrimaryIntegration
            || diagnostic.role != Some(Role::Primary)
            || diagnostic.thread_id.as_deref() != Some(self.facts.primary_thread_id.as_str())
        {
            return Err(state_error(
                "NOT_RETRYABLE",
                "forbidden-operation recovery is limited to the bound primary integration turn",
            ));
        }
        if !self.plan_approved
            || self.current_plan_payload.is_none()
            || self.plan_approval_payload.is_none()
            || self.target_integration_branch.is_none()
        {
            return Err(state_error(
                "INCOMPATIBLE_STATE",
                "forbidden-operation recovery requires an approved frozen integration plan",
            ));
        }
        if self.integration_branch.is_some()
            || self.integration_sha.is_some()
            || self.current_integration_payload.is_some()
            || self.verification_worktree.is_some()
            || !self.test_evidence.is_empty()
        {
            return Err(state_error(
                "NOT_RETRYABLE",
                "forbidden-operation recovery is limited to a side-effect-free first integration turn",
            ));
        }

        self.status = RunStatus::Running;
        self.phase = Phase::Integrate;
        self.next_action = NextAction::RequestPrimaryIntegration;
        self.reason_code = None;
        self.last_error = None;
        self.validate_persisted()?;
        Ok(self.next_action)
    }

    pub fn cancel(&mut self) -> NextAction {
        self.status = RunStatus::Cancelled;
        self.phase = Phase::Cancelled;
        self.reason_code = None;
        self.next_action = NextAction::Stop;
        self.next_action
    }

    pub fn mark_incompatible(&mut self, reason_code: &str) -> NextAction {
        self.status = RunStatus::IncompatibleCodex;
        self.phase = Phase::Blocked;
        self.reason_code = Some(reason_code.to_owned());
        self.next_action = NextAction::Stop;
        self.next_action
    }

    pub fn accept_after_revalidation(&mut self) -> Result<NextAction, StateError> {
        self.require_running()?;
        if self.next_action != NextAction::RevalidateAndAccept
            || self.result_approved_sha.as_deref() != self.integration_sha.as_deref()
        {
            return Err(state_error(
                "RESULT_NOT_APPROVED",
                "the current integration SHA has not received an exact approval",
            ));
        }
        self.validate_test_evidence()?;

        let accepted_result = AcceptedResult {
            integration_branch: self.integration_branch.clone().ok_or_else(|| {
                state_error("INVALID_STATE", "accepted integration branch is missing")
            })?,
            integration_sha: self.integration_sha.clone().ok_or_else(|| {
                state_error("INVALID_STATE", "accepted integration SHA is missing")
            })?,
            primary_sha: self.facts.primary_sha.clone(),
            reviewer_sha: self.facts.reviewer_sha.clone(),
            tests: self.test_evidence.clone(),
            source_refs_unchanged: true,
            publication: PublicationBoundary {
                local_only: true,
                pushed: false,
                pull_request_created: false,
                merged_into_existing_branch: false,
            },
        };
        self.status = RunStatus::Accepted;
        self.phase = Phase::Accepted;
        self.reason_code = None;
        self.next_action = NextAction::Stop;
        self.accepted_result = Some(accepted_result);
        Ok(self.next_action)
    }

    pub fn validate_persisted(&self) -> Result<(), StateError> {
        if self.schema_version != RUN_STATE_SCHEMA_VERSION {
            return Err(state_error(
                "INCOMPATIBLE_STATE",
                format!(
                    "persisted state schema {} is not supported; expected {}",
                    self.schema_version, RUN_STATE_SCHEMA_VERSION
                ),
            ));
        }
        if matches!(
            self.next_action,
            NextAction::RequestPrimaryVerification
                | NextAction::RequestReviewerResultVerdict
                | NextAction::RevalidateAndAccept
        ) && (self.current_integration_payload.is_none()
            || self.integration_branch.is_none()
            || self.integration_sha.is_none())
        {
            return Err(state_error(
                "INCOMPATIBLE_STATE",
                "persisted active result state lacks canonical integration evidence",
            ));
        }
        if matches!(
            self.next_action,
            NextAction::RequestReviewerResultVerdict | NextAction::RevalidateAndAccept
        ) {
            self.validate_test_evidence().map_err(|_| {
                state_error(
                    "INCOMPATIBLE_STATE",
                    "persisted result state lacks authoritative successful tests",
                )
            })?;
        }
        if self.status == RunStatus::Accepted {
            if self.phase != Phase::Accepted
                || self.next_action != NextAction::Stop
                || self.result_approved_sha.as_deref() != self.integration_sha.as_deref()
            {
                return Err(state_error(
                    "INCOMPATIBLE_STATE",
                    "persisted accepted state has inconsistent terminal metadata",
                ));
            }
            self.validate_test_evidence().map_err(|_| {
                state_error(
                    "INCOMPATIBLE_STATE",
                    "persisted accepted state lacks authoritative successful tests",
                )
            })?;
            let expected_result = AcceptedResult {
                integration_branch: self.integration_branch.clone().ok_or_else(|| {
                    state_error(
                        "INCOMPATIBLE_STATE",
                        "persisted accepted state lacks an integration branch",
                    )
                })?,
                integration_sha: self.integration_sha.clone().ok_or_else(|| {
                    state_error(
                        "INCOMPATIBLE_STATE",
                        "persisted accepted state lacks an integration SHA",
                    )
                })?,
                primary_sha: self.facts.primary_sha.clone(),
                reviewer_sha: self.facts.reviewer_sha.clone(),
                tests: self.test_evidence.clone(),
                source_refs_unchanged: true,
                publication: PublicationBoundary {
                    local_only: true,
                    pushed: false,
                    pull_request_created: false,
                    merged_into_existing_branch: false,
                },
            };
            if self.accepted_result.as_ref() != Some(&expected_result) {
                return Err(state_error(
                    "INCOMPATIBLE_STATE",
                    "persisted accepted_result does not match authoritative state",
                ));
            }
        } else if self.accepted_result.is_some() {
            return Err(state_error(
                "INCOMPATIBLE_STATE",
                "persisted non-accepted state contains an accepted_result",
            ));
        }
        Ok(())
    }

    fn apply_contract(&mut self, message: ProtocolMessage) -> Result<NextAction, StateError> {
        self.require_phase(Phase::Contract)?;
        self.require_round(&message, 1)?;
        let expected_role = match self.next_action {
            NextAction::RequestPrimaryContract => "PRIMARY",
            NextAction::RequestReviewerContract => "REVIEWER",
            _ => {
                return Err(state_error(
                    "UNEXPECTED_MESSAGE",
                    "a contract was not the pending action",
                ));
            }
        };
        if let Some(role) = message.payload.get("role") {
            let role = role.as_str().ok_or_else(|| {
                state_error(
                    "INVALID_RESPONSE",
                    "CONTRACT_READY payload.role must be a string when present",
                )
            })?;
            if role != expected_role {
                return Err(state_error(
                    "UNEXPECTED_ROLE",
                    "contract role does not match the bound task for the pending action",
                ));
            }
        }
        let contract = message.payload.get("contract").ok_or_else(|| {
            state_error(
                "INVALID_RESPONSE",
                "CONTRACT_READY payload requires a contract",
            )
        })?;
        let tests = string_array_field(contract, "tests")?;

        match self.next_action {
            NextAction::RequestPrimaryContract => {
                self.primary_contract_hash = Some(canonical_json_hash(contract));
                self.primary_contract = Some(contract.clone());
                self.freeze_test_commands(tests);
                self.next_action = NextAction::RequestReviewerContract;
            }
            NextAction::RequestReviewerContract => {
                self.reviewer_contract_hash = Some(canonical_json_hash(contract));
                self.reviewer_contract = Some(contract.clone());
                self.freeze_test_commands(tests);
                self.phase = Phase::PlanReview;
                self.round = 1;
                self.plan_revision = Some(1);
                self.next_action = NextAction::RequestPrimaryPlan;
            }
            _ => unreachable!("pending contract action was validated above"),
        }
        Ok(self.next_action)
    }

    fn apply_plan_approval(&mut self, message: ProtocolMessage) -> Result<NextAction, StateError> {
        self.require_phase(Phase::PlanReview)?;
        self.require_round(&message, self.round)?;
        self.require_plan_revision(&message)?;
        if self.current_plan_hash.is_none() {
            return Err(state_error(
                "PLAN_MISSING",
                "a primary plan must be recorded before reviewer approval",
            ));
        }
        let approved_hash = message
            .payload
            .get("approved_plan_hash")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                state_error(
                    "INVALID_RESPONSE",
                    "APPROVED_PLAN requires approved_plan_hash",
                )
            })?;
        if self.current_plan_hash.as_deref() != Some(approved_hash) {
            return Err(state_error(
                "STALE_PLAN_HASH",
                "plan approval does not bind the current canonical plan payload",
            ));
        }

        self.plan_approved = true;
        self.plan_approval_payload = Some(message.payload.clone());
        self.request_integration()
    }

    fn apply_plan(&mut self, message: ProtocolMessage) -> Result<NextAction, StateError> {
        self.require_phase(Phase::PlanReview)?;
        self.require_round(&message, self.round)?;
        self.require_plan_revision(&message)?;
        if self.next_action != NextAction::RequestPrimaryPlan {
            return Err(state_error(
                "UNEXPECTED_MESSAGE",
                "a primary plan was not the pending action",
            ));
        }
        self.record_plan(message.payload)
    }

    fn apply_integration(&mut self, message: ProtocolMessage) -> Result<NextAction, StateError> {
        if !matches!(self.phase, Phase::Integrate | Phase::Verify) {
            return Err(state_error(
                "WRONG_PHASE",
                "INTEGRATION_READY requires INTEGRATE or VERIFY state",
            ));
        }
        self.require_round(&message, self.round)?;
        self.require_plan_revision(&message)?;
        if !self.plan_approved {
            return Err(state_error(
                "PLAN_NOT_APPROVED",
                "integration evidence arrived without an approved plan",
            ));
        }

        let branch = message
            .envelope
            .integration_branch
            .clone()
            .ok_or_else(|| state_error("INVALID_RESPONSE", "integration branch is missing"))?;
        if self
            .target_integration_branch
            .as_deref()
            .is_some_and(|target| target != branch)
        {
            return Err(state_error(
                "UNEXPECTED_INTEGRATION_BRANCH",
                "reported integration branch does not match the authorized new branch",
            ));
        }
        let sha = message
            .envelope
            .integration_sha
            .clone()
            .ok_or_else(|| state_error("INVALID_RESPONSE", "integration SHA is missing"))?;
        if self.phase == Phase::Integrate {
            if message.envelope.phase != MessagePhase::Integrate {
                return Err(state_error(
                    "WRONG_PHASE",
                    "integration creation evidence must use INTEGRATE phase",
                ));
            }
            if message
                .payload
                .get("test_evidence")
                .and_then(Value::as_array)
                .is_some_and(|evidence| !evidence.is_empty())
            {
                return Err(state_error(
                    "INVALID_RESPONSE",
                    "integration creation cannot self-report test evidence",
                ));
            }
            let is_initial_result_review = self.integration_sha.is_none();
            self.integration_branch = Some(branch);
            self.integration_sha = Some(sha);
            self.current_integration_payload = Some(message.payload);
            self.test_evidence.clear();
            self.verification_worktree = None;
            self.result_approved_sha = None;
            self.phase = Phase::Verify;
            if is_initial_result_review {
                self.round = 1;
                self.last_review_fingerprint = None;
                self.unchanged_review_streak = 0;
            }
            self.next_action = NextAction::RequestPrimaryVerification;
            return Ok(self.next_action);
        }

        if message.envelope.phase != MessagePhase::Verify {
            return Err(state_error(
                "WRONG_PHASE",
                "test verification evidence must use VERIFY phase",
            ));
        }
        self.require_current_integration(&message)?;
        let evidence = test_evidence(&message.payload)?;
        self.test_evidence = evidence;
        self.current_integration_payload = Some(message.payload);
        self.result_approved_sha = None;
        self.phase = Phase::ResultReview;
        self.next_action = NextAction::RequestReviewerResultVerdict;
        Ok(self.next_action)
    }

    fn apply_result_approval(
        &mut self,
        message: ProtocolMessage,
    ) -> Result<NextAction, StateError> {
        self.require_phase(Phase::ResultReview)?;
        self.require_round(&message, self.round)?;
        self.require_plan_revision(&message)?;
        self.require_current_integration(&message)?;

        self.result_approved_sha = message.envelope.integration_sha;
        self.next_action = NextAction::RevalidateAndAccept;
        Ok(self.next_action)
    }

    fn apply_changes_required(
        &mut self,
        message: ProtocolMessage,
    ) -> Result<NextAction, StateError> {
        if !matches!(self.phase, Phase::PlanReview | Phase::ResultReview) {
            return Err(state_error(
                "WRONG_PHASE",
                "CHANGES_REQUIRED is not valid in the current state phase",
            ));
        }
        self.require_round(&message, self.round)?;
        self.require_plan_revision(&message)?;
        if self.phase == Phase::ResultReview {
            self.require_current_integration(&message)?;
        }

        let issue_hash = normalized_issue_hash(&message.payload);
        let fingerprint = canonical_json_hash(&json!({
            "plan": self.current_plan_hash,
            "issues": issue_hash,
        }));
        if self.last_review_fingerprint.as_deref() == Some(&fingerprint) {
            self.unchanged_review_streak = self.unchanged_review_streak.saturating_add(1);
        } else {
            self.last_review_fingerprint = Some(fingerprint);
            self.unchanged_review_streak = 1;
        }

        if self.unchanged_review_streak >= self.no_progress_rounds {
            return Ok(self.block("NO_PROGRESS"));
        }
        if self.round >= self.max_review_rounds {
            return Ok(self.block("ROUND_LIMIT"));
        }

        self.round += 1;
        self.result_approved_sha = None;
        if self.phase == Phase::PlanReview {
            self.last_plan_feedback = Some(message.payload.clone());
            self.plan_revision = self.plan_revision.map(|revision| revision + 1);
            self.plan_approved = false;
            self.plan_approval_payload = None;
            self.next_action = NextAction::RequestPrimaryPlan;
        } else {
            self.last_result_feedback = Some(message.payload.clone());
            self.phase = Phase::Integrate;
            self.next_action = NextAction::RequestPrimaryIntegration;
        }
        Ok(self.next_action)
    }

    fn verify_immutable_envelope(&self, message: &ProtocolMessage) -> Result<(), StateError> {
        if message.envelope.run_id != self.facts.run_id {
            return Err(state_error(
                "STALE_RUN_ID",
                "message run_id does not match the active run",
            ));
        }
        if message.envelope.primary_sha != self.facts.primary_sha {
            return Err(state_error(
                "STALE_PRIMARY_SHA",
                "message primary_sha does not match the frozen source",
            ));
        }
        if message.envelope.reviewer_sha != self.facts.reviewer_sha {
            return Err(state_error(
                "STALE_REVIEWER_SHA",
                "message reviewer_sha does not match the frozen source",
            ));
        }
        Ok(())
    }

    fn verify_blocked_envelope(&self, message: &ProtocolMessage) -> Result<(), StateError> {
        let expected_phase = MessagePhase::from(self.phase);
        if message.envelope.phase != expected_phase {
            return Err(state_error(
                "WRONG_PHASE",
                "BLOCKED phase does not match the pending action",
            ));
        }
        self.require_round(message, self.round)?;
        if message.envelope.plan_revision != self.plan_revision {
            return Err(state_error(
                "STALE_PLAN_REVISION",
                "BLOCKED plan revision does not match the current revision",
            ));
        }
        match self.integration_sha.as_deref() {
            Some(_) => self.require_current_integration(message),
            None if message.envelope.integration_branch.is_none()
                && message.envelope.integration_sha.is_none() =>
            {
                Ok(())
            }
            None => Err(state_error(
                "STALE_INTEGRATION_SHA",
                "BLOCKED carries an integration identity before one exists",
            )),
        }
    }

    fn freeze_test_commands(&mut self, commands: Vec<String>) {
        for command in commands {
            if !self.required_test_commands.contains(&command) {
                self.required_test_commands.push(command);
            }
        }
    }

    fn validate_test_evidence(&self) -> Result<(), StateError> {
        if self.required_test_commands.is_empty()
            || self.test_evidence.len() != self.required_test_commands.len()
            || self
                .required_test_commands
                .iter()
                .enumerate()
                .any(|(index, command)| {
                    command.trim().is_empty()
                        || self.required_test_commands[..index].contains(command)
                })
            || self.test_evidence.iter().any(|evidence| {
                evidence.command.trim().is_empty()
                    || evidence.exit_code != 0
                    || evidence.turn_id.trim().is_empty()
                    || evidence.item_id.trim().is_empty()
                    || !evidence.cwd.is_absolute()
            })
            || self.required_test_commands.iter().any(|required| {
                self.test_evidence
                    .iter()
                    .filter(|evidence| evidence.command == *required && evidence.exit_code == 0)
                    .count()
                    != 1
            })
        {
            return Err(state_error(
                "TEST_FAILURE",
                "acceptance requires exact successful evidence for every frozen test command",
            ));
        }
        Ok(())
    }

    fn require_current_integration(&self, message: &ProtocolMessage) -> Result<(), StateError> {
        if message.envelope.integration_branch.as_deref() != self.integration_branch.as_deref() {
            return Err(state_error(
                "STALE_INTEGRATION_BRANCH",
                "verdict targets a different integration branch",
            ));
        }
        if message.envelope.integration_sha.as_deref() != self.integration_sha.as_deref() {
            return Err(state_error(
                "STALE_INTEGRATION_SHA",
                "verdict targets a different integration SHA",
            ));
        }
        Ok(())
    }

    fn require_plan_revision(&self, message: &ProtocolMessage) -> Result<(), StateError> {
        if message.envelope.plan_revision != self.plan_revision {
            return Err(state_error(
                "STALE_PLAN_REVISION",
                "message plan_revision does not match the current revision",
            ));
        }
        Ok(())
    }

    fn require_round(&self, message: &ProtocolMessage, expected: u32) -> Result<(), StateError> {
        if message.envelope.round != expected {
            return Err(state_error(
                "STALE_ROUND",
                format!(
                    "message round {} does not match current round {expected}",
                    message.envelope.round
                ),
            ));
        }
        Ok(())
    }

    fn require_phase(&self, expected: Phase) -> Result<(), StateError> {
        if self.phase != expected {
            return Err(state_error(
                "WRONG_PHASE",
                format!("expected {expected:?}, found {:?}", self.phase),
            ));
        }
        Ok(())
    }

    fn require_running(&self) -> Result<(), StateError> {
        if self.status != RunStatus::Running {
            return Err(state_error(
                "RUN_NOT_ACTIVE",
                format!("run status is {:?}", self.status),
            ));
        }
        Ok(())
    }

    pub fn block(&mut self, reason_code: &str) -> NextAction {
        self.status = RunStatus::Blocked;
        self.phase = Phase::Blocked;
        self.reason_code = Some(reason_code.to_owned());
        self.next_action = NextAction::Stop;
        self.next_action
    }
}

fn revalidate_message(message: ProtocolMessage) -> Result<ProtocolMessage, StateError> {
    let value = serde_json::to_value(message).map_err(|error| {
        state_error(
            "INVALID_RESPONSE",
            format!("could not serialize protocol message: {error}"),
        )
    })?;
    validate_message(value).map_err(|error| state_error("INVALID_RESPONSE", error.to_string()))
}

fn string_array_field(value: &Value, field: &str) -> Result<Vec<String>, StateError> {
    let values = value
        .get(field)
        .and_then(Value::as_array)
        .filter(|values| !values.is_empty())
        .ok_or_else(|| {
            state_error(
                "INVALID_RESPONSE",
                format!("{field} must be a nonempty array of commands"),
            )
        })?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|command| !command.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| {
                    state_error(
                        "INVALID_RESPONSE",
                        format!("{field} entries must be nonempty strings"),
                    )
                })
        })
        .collect()
}

fn test_evidence(payload: &Value) -> Result<Vec<TestEvidence>, StateError> {
    let values = payload
        .get("test_evidence")
        .and_then(Value::as_array)
        .filter(|values| !values.is_empty())
        .ok_or_else(|| state_error("INVALID_RESPONSE", "test_evidence must be a nonempty array"))?;
    values
        .iter()
        .map(|value| {
            serde_json::from_value::<TestEvidence>(value.clone()).map_err(|error| {
                state_error(
                    "INVALID_RESPONSE",
                    format!("invalid test_evidence entry: {error}"),
                )
            })
        })
        .collect()
}

fn normalized_issue_hash(payload: &Value) -> String {
    if let Some(issue_ids) = payload.get("issue_ids").and_then(Value::as_array) {
        let mut normalized = issue_ids
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|issue| !issue.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        normalized.sort_unstable();
        normalized.dedup();
        canonical_json_hash(&json!(normalized))
    } else {
        canonical_json_hash(payload)
    }
}

fn state_error(code: &'static str, detail: impl Into<String>) -> StateError {
    StateError {
        code,
        detail: detail.into(),
    }
}

impl From<Phase> for MessagePhase {
    fn from(value: Phase) -> Self {
        match value {
            Phase::Discover => Self::Discover,
            Phase::Freeze => Self::Freeze,
            Phase::Contract => Self::Contract,
            Phase::PlanReview => Self::PlanReview,
            Phase::Integrate => Self::Integrate,
            Phase::Verify => Self::Verify,
            Phase::ResultReview => Self::ResultReview,
            Phase::Accepted => Self::Accepted,
            Phase::Blocked => Self::Blocked,
            Phase::PausedUserAction => Self::PausedUserAction,
            Phase::Cancelled => Self::Cancelled,
        }
    }
}
