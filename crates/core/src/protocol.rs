use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;
use uuid::Uuid;

const PROTOCOL_V1: &str = "worktree-merge-consensus/v1";

static PROTOCOL_SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    let schema = serde_json::from_str(include_str!("../../../schemas/protocol-v1.json"))
        .expect("the checked-in protocol schema must be valid JSON");
    jsonschema::validator_for(&schema)
        .expect("the checked-in protocol schema must be a valid JSON Schema")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MessageType {
    ContractReady,
    ChangesRequired,
    ApprovedPlan,
    IntegrationReady,
    ApprovedResult,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MessagePhase {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub protocol: String,
    pub run_id: Uuid,
    pub message_type: MessageType,
    pub phase: MessagePhase,
    pub round: u32,
    pub primary_sha: String,
    pub reviewer_sha: String,
    pub plan_revision: Option<u32>,
    pub integration_branch: Option<String>,
    pub integration_sha: Option<String>,
    pub reason_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtocolMessage {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub payload: Value,
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("protocol response must be a JSON object")]
    ExpectedObject,
    #[error("protocol schema validation failed: {0}")]
    Schema(String),
    #[error("protocol message could not be decoded: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("protocol invariant failed: {0}")]
    Invariant(String),
}

pub fn validate_message(value: Value) -> Result<ProtocolMessage, ProtocolError> {
    if !value.is_object() {
        return Err(ProtocolError::ExpectedObject);
    }

    let errors = PROTOCOL_SCHEMA
        .iter_errors(&value)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        return Err(ProtocolError::Schema(errors.join("; ")));
    }

    let message: ProtocolMessage = serde_json::from_value(value)?;
    message.validate_invariants()?;
    Ok(message)
}

impl ProtocolMessage {
    fn validate_invariants(&self) -> Result<(), ProtocolError> {
        if self.envelope.protocol != PROTOCOL_V1 {
            return Err(invariant("unsupported protocol version"));
        }

        match self.envelope.message_type {
            MessageType::ContractReady => {
                self.require_phase(MessagePhase::Contract)?;
                self.require_no_integration()?;
            }
            MessageType::ApprovedPlan => self.validate_plan_approval()?,
            MessageType::IntegrationReady => {
                if !matches!(
                    self.envelope.phase,
                    MessagePhase::Integrate | MessagePhase::Verify
                ) {
                    return Err(invariant(
                        "INTEGRATION_READY must be emitted during INTEGRATE or VERIFY",
                    ));
                }
                self.require_integration_identity()?;
            }
            MessageType::ApprovedResult => self.validate_result_approval()?,
            MessageType::ChangesRequired => self.validate_changes_required()?,
            MessageType::Blocked => {
                if self
                    .envelope
                    .reason_code
                    .as_deref()
                    .is_none_or(str::is_empty)
                {
                    return Err(invariant("BLOCKED requires a non-empty reason_code"));
                }
            }
        }

        Ok(())
    }

    fn validate_plan_approval(&self) -> Result<(), ProtocolError> {
        self.require_phase(MessagePhase::PlanReview)?;
        self.require_no_integration()?;
        let revision = self.require_plan_revision()?;
        let payload = self.payload_object()?;

        require_u64(payload, "approved_plan_revision", u64::from(revision))?;
        require_string(payload, "approved_primary_sha", &self.envelope.primary_sha)?;
        require_string(
            payload,
            "approved_reviewer_sha",
            &self.envelope.reviewer_sha,
        )?;
        require_empty_array(payload, "uncovered_items")?;
        Ok(())
    }

    fn validate_result_approval(&self) -> Result<(), ProtocolError> {
        self.require_phase(MessagePhase::ResultReview)?;
        let revision = self.require_plan_revision()?;
        let (branch, integration_sha) = self.require_integration_identity()?;
        let payload = self.payload_object()?;

        require_u64(payload, "approved_plan_revision", u64::from(revision))?;
        require_string(payload, "approved_primary_sha", &self.envelope.primary_sha)?;
        require_string(
            payload,
            "approved_reviewer_sha",
            &self.envelope.reviewer_sha,
        )?;
        require_string(payload, "approved_integration_branch", branch)?;
        require_string(payload, "approved_integration_sha", integration_sha)?;
        require_empty_array(payload, "uncovered_items")?;
        Ok(())
    }

    fn validate_changes_required(&self) -> Result<(), ProtocolError> {
        if !matches!(
            self.envelope.phase,
            MessagePhase::PlanReview | MessagePhase::ResultReview
        ) {
            return Err(invariant(
                "CHANGES_REQUIRED is only valid during PLAN_REVIEW or RESULT_REVIEW",
            ));
        }
        self.require_plan_revision()?;
        if self
            .envelope
            .reason_code
            .as_deref()
            .is_none_or(str::is_empty)
        {
            return Err(invariant(
                "CHANGES_REQUIRED requires a non-empty reason_code",
            ));
        }
        if self.envelope.phase == MessagePhase::ResultReview {
            self.require_integration_identity()?;
        } else {
            self.require_no_integration()?;
        }
        Ok(())
    }

    fn require_phase(&self, expected: MessagePhase) -> Result<(), ProtocolError> {
        if self.envelope.phase != expected {
            return Err(invariant(format!(
                "{:?} is not valid during {:?}",
                self.envelope.message_type, self.envelope.phase
            )));
        }
        Ok(())
    }

    fn require_plan_revision(&self) -> Result<u32, ProtocolError> {
        self.envelope
            .plan_revision
            .filter(|revision| *revision > 0)
            .ok_or_else(|| invariant("message requires a non-zero plan_revision"))
    }

    fn require_no_integration(&self) -> Result<(), ProtocolError> {
        if self.envelope.integration_branch.is_some() || self.envelope.integration_sha.is_some() {
            return Err(invariant(
                "message must not include an integration branch or SHA",
            ));
        }
        Ok(())
    }

    fn require_integration_identity(&self) -> Result<(&str, &str), ProtocolError> {
        let branch = self
            .envelope
            .integration_branch
            .as_deref()
            .filter(|branch| !branch.is_empty())
            .ok_or_else(|| invariant("message requires an integration_branch"))?;
        let sha = self
            .envelope
            .integration_sha
            .as_deref()
            .filter(|sha| !sha.is_empty())
            .ok_or_else(|| invariant("message requires an integration_sha"))?;
        Ok((branch, sha))
    }

    fn payload_object(&self) -> Result<&Map<String, Value>, ProtocolError> {
        self.payload
            .as_object()
            .ok_or_else(|| invariant("payload must be a JSON object"))
    }
}

fn require_u64(
    payload: &Map<String, Value>,
    key: &str,
    expected: u64,
) -> Result<(), ProtocolError> {
    if payload.get(key).and_then(Value::as_u64) != Some(expected) {
        return Err(invariant(format!("payload.{key} must match the envelope")));
    }
    Ok(())
}

fn require_string(
    payload: &Map<String, Value>,
    key: &str,
    expected: &str,
) -> Result<(), ProtocolError> {
    if payload.get(key).and_then(Value::as_str) != Some(expected) {
        return Err(invariant(format!("payload.{key} must match the envelope")));
    }
    Ok(())
}

fn require_empty_array(payload: &Map<String, Value>, key: &str) -> Result<(), ProtocolError> {
    match payload.get(key).and_then(Value::as_array) {
        Some(values) if values.is_empty() => Ok(()),
        _ => Err(invariant(format!(
            "payload.{key} must be present and empty for approval"
        ))),
    }
}

fn invariant(message: impl Into<String>) -> ProtocolError {
    ProtocolError::Invariant(message.into())
}
