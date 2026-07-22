use thiserror::Error;

pub const PARTICIPANT_PROTOCOL_V2: &str = "worktree-merge-consensus/v2";

const MARKER_OPEN: &str = "<consensus-result>";
const MARKER_CLOSE: &str = "</consensus-result>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParticipantSignal {
    ContractReady,
    PlanReady,
    Approved,
    ChangesRequired,
    IntegrationReady,
    VerificationReady,
    Blocked,
}

impl ParticipantSignal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ContractReady => "CONTRACT_READY",
            Self::PlanReady => "PLAN_READY",
            Self::Approved => "APPROVED",
            Self::ChangesRequired => "CHANGES_REQUIRED",
            Self::IntegrationReady => "INTEGRATION_READY",
            Self::VerificationReady => "VERIFICATION_READY",
            Self::Blocked => "BLOCKED",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "CONTRACT_READY" => Some(Self::ContractReady),
            "PLAN_READY" => Some(Self::PlanReady),
            "APPROVED" => Some(Self::Approved),
            "CHANGES_REQUIRED" => Some(Self::ChangesRequired),
            "INTEGRATION_READY" => Some(Self::IntegrationReady),
            "VERIFICATION_READY" => Some(Self::VerificationReady),
            "BLOCKED" => Some(Self::Blocked),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantResponse {
    pub signal: ParticipantSignal,
    pub blocked_reason: Option<String>,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("participant response marker is invalid: {detail}")]
pub struct ParticipantResponseError {
    detail: String,
}

impl ParticipantResponseError {
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

pub fn parse_participant_response(
    text: &str,
    allowed: &[ParticipantSignal],
) -> Result<ParticipantResponse, ParticipantResponseError> {
    let opens = text.match_indices(MARKER_OPEN).collect::<Vec<_>>();
    let closes = text.match_indices(MARKER_CLOSE).collect::<Vec<_>>();
    if opens.len() != 1 || closes.len() != 1 {
        return Err(invalid(format!(
            "expected exactly one {MARKER_OPEN}...{MARKER_CLOSE} marker"
        )));
    }

    let marker_start = opens[0].0;
    let value_start = marker_start + MARKER_OPEN.len();
    let marker_end = closes[0].0;
    if marker_end < value_start {
        return Err(invalid(
            "the closing marker appears before the opening marker",
        ));
    }
    let marker_value = text[value_start..marker_end].trim();
    let (signal_value, blocked_reason) = match marker_value.split_once(':') {
        Some((signal, reason)) if signal.trim() == "BLOCKED" => {
            let reason = reason.trim();
            if reason.is_empty()
                || reason.len() > 64
                || !reason
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
            {
                return Err(invalid(
                    "a BLOCKED reason must be 1-64 uppercase ASCII letters, digits, or underscores",
                ));
            }
            (signal.trim(), Some(reason.to_owned()))
        }
        Some(_) => {
            return Err(invalid(
                "only BLOCKED may include an optional reason after a colon",
            ));
        }
        None => (marker_value, None),
    };
    let signal = ParticipantSignal::parse(signal_value)
        .ok_or_else(|| invalid(format!("unknown result {signal_value:?}")))?;
    if !allowed.contains(&signal) {
        return Err(invalid(format!(
            "result {} is not allowed for this turn",
            signal.as_str()
        )));
    }

    let after_marker = marker_end + MARKER_CLOSE.len();
    let before = text[..marker_start].trim();
    let after = text[after_marker..].trim();
    let body = match (before.is_empty(), after.is_empty()) {
        (true, true) => String::new(),
        (false, true) => before.to_owned(),
        (true, false) => after.to_owned(),
        (false, false) => format!("{before}\n\n{after}"),
    };

    Ok(ParticipantResponse {
        signal,
        blocked_reason,
        body,
    })
}

fn invalid(detail: impl Into<String>) -> ParticipantResponseError {
    ParticipantResponseError {
        detail: detail.into(),
    }
}
