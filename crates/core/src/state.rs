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
pub struct RunState {
    pub facts: RunFacts,
    pub phase: Phase,
    pub status: RunStatus,
    pub round: u32,
    pub plan_revision: Option<u32>,
    pub integration_branch: Option<String>,
    pub integration_sha: Option<String>,
    pub reason_code: Option<String>,
    pub next_action: NextAction,
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
            facts,
            phase: Phase::Contract,
            status: RunStatus::Running,
            round: 1,
            plan_revision: None,
            integration_branch: None,
            integration_sha: None,
            reason_code: None,
            next_action: NextAction::RequestPrimaryContract,
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

        self.current_plan_hash = Some(canonical_json_hash(&payload));
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
        self.require_running()?;
        self.status = RunStatus::PausedUserAction;
        self.reason_code = Some(reason_code.into());
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
        Ok(self.next_action)
    }

    pub fn cancel(&mut self) -> NextAction {
        self.status = RunStatus::Cancelled;
        self.phase = Phase::Cancelled;
        self.reason_code = None;
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

        self.status = RunStatus::Accepted;
        self.phase = Phase::Accepted;
        self.reason_code = None;
        self.next_action = NextAction::Stop;
        Ok(self.next_action)
    }

    fn apply_contract(&mut self, message: ProtocolMessage) -> Result<NextAction, StateError> {
        self.require_phase(Phase::Contract)?;
        self.require_round(&message, 1)?;
        let role = message
            .payload
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                state_error("INVALID_RESPONSE", "CONTRACT_READY payload requires a role")
            })?;
        let contract = message.payload.get("contract").ok_or_else(|| {
            state_error(
                "INVALID_RESPONSE",
                "CONTRACT_READY payload requires a contract",
            )
        })?;

        match self.next_action {
            NextAction::RequestPrimaryContract if role == "PRIMARY" => {
                self.primary_contract_hash = Some(canonical_json_hash(contract));
                self.next_action = NextAction::RequestReviewerContract;
            }
            NextAction::RequestReviewerContract if role == "REVIEWER" => {
                self.reviewer_contract_hash = Some(canonical_json_hash(contract));
                self.phase = Phase::PlanReview;
                self.round = 1;
                self.plan_revision = Some(1);
                self.next_action = NextAction::RequestPrimaryPlan;
            }
            _ => {
                return Err(state_error(
                    "UNEXPECTED_ROLE",
                    "contract role does not match the pending action",
                ));
            }
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

        self.plan_approved = true;
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
        self.require_phase(Phase::Integrate)?;
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
        let sha = message
            .envelope
            .integration_sha
            .clone()
            .ok_or_else(|| state_error("INVALID_RESPONSE", "integration SHA is missing"))?;
        let is_initial_result_review = self.integration_sha.is_none();
        self.integration_branch = Some(branch);
        self.integration_sha = Some(sha);
        self.result_approved_sha = None;
        self.phase = Phase::ResultReview;
        if is_initial_result_review {
            self.round = 1;
            self.last_review_fingerprint = None;
            self.unchanged_review_streak = 0;
        }
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
            self.plan_revision = self.plan_revision.map(|revision| revision + 1);
            self.plan_approved = false;
            self.next_action = NextAction::RequestPrimaryPlan;
        } else {
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

    fn block(&mut self, reason_code: &str) -> NextAction {
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
