use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use app_server_client::{
    AppEvent, AppServer, AppServerError, CONTROLLED_PATCH_APPROVAL_KEY,
    CONTROLLED_PATCH_APPROVAL_MODE, CommandExecRequest, McpServerStatus, PARTICIPANT_MCP_SERVER,
    PARTICIPANT_PATCH_TOOL, ThreadDetail, ThreadResumePolicy, TurnExecutionPolicy,
};
use consensus_core::{
    canonical_json_hash,
    git::{
        GitInspector, GitSafetyError, WorktreeSnapshot, normalize_branch_name,
        verify_frozen_sources, verify_integration_result, verify_reported_changed_files,
        verify_same_repository,
    },
    participant::{ParticipantResponse, ParticipantSignal, parse_participant_response},
    prompts::{PromptError, build_turn_prompt},
    protocol::{Envelope, MessagePhase, MessageType, ProtocolMessage, validate_message},
    state::{
        NextAction, Phase, Role, RunDiagnostic, RunFacts, RunState, RunStatus, StateError,
        TestEvidence,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::policy::{
    ApprovalDecision, command_approval_denial, decide_command_approval,
    is_retry_safe_read_only_integration_command, validate_test_command,
};
use crate::store::{
    AcceptedTurn, SqliteRunStore, StoreError, VerificationCommandClaim, VerificationCommandRecord,
};

const MAX_DRIVER_STEPS: usize = 128;
const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(1_800);
const TURN_INTERRUPT_TIMEOUT: Duration = Duration::from_secs(10);
const VERIFICATION_COMMAND_OUTPUT_CAP_BYTES: usize = 65_536;
const MAX_VERIFICATION_FAILURE_OUTPUT_BYTES: usize = 16_384;
const VERIFICATION_OUTPUT_TRUNCATION_MARKER: &str = "[earlier output truncated]\n";

struct CompletedTurn {
    response: String,
    turn: Value,
}

struct RetryableCompletedTurn {
    message_hash: String,
    thread_id: String,
    turn_id: String,
    observed_status: String,
}

enum VerificationRetryKind {
    EmptyTurn,
    EventEvidenceCompatibility,
    UnattendedVerificationMigration,
}

struct RetryableVerificationTurn {
    turn: RetryableCompletedTurn,
    kind: VerificationRetryKind,
}

struct RetryableTerminalTurn {
    message_hash: String,
    thread_id: String,
    turn_id: String,
    observed_status: String,
}

struct RetryableAcceptedExecutionToolTurn {
    accepted: AcceptedTurn,
    observed_status: String,
}

struct RetryableAcceptedVerificationEnvironmentTurn {
    accepted: AcceptedTurn,
    observed_status: String,
}

struct AuthoritativeVerification {
    evidence: Vec<TestEvidence>,
    failures: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StartRequest {
    pub integration_branch: Option<String>,
    pub test_commands: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationPatchResult {
    pub run_id: String,
    pub integration_branch: String,
    pub base_sha: String,
    pub changed_files: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatorOptions {
    pub wait_timeout: Duration,
    pub poll_interval: Duration,
    pub communication_attempts: usize,
    pub participant_mcp_executable: PathBuf,
}

impl Default for CoordinatorOptions {
    fn default() -> Self {
        Self {
            wait_timeout: DEFAULT_WAIT_TIMEOUT,
            poll_interval: Duration::from_millis(500),
            communication_attempts: 3,
            participant_mcp_executable: std::env::current_exe()
                .expect("current daemon executable is required for participant MCP injection"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{code}: {detail}")]
pub struct SafetyError {
    code: String,
    detail: String,
}

impl SafetyError {
    pub fn new(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            detail: detail.into(),
        }
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl From<GitSafetyError> for SafetyError {
    fn from(error: GitSafetyError) -> Self {
        Self::new(error.code(), error.detail())
    }
}

pub trait RepositorySafety: Send + Sync {
    fn verify_frozen(&self, facts: &RunFacts) -> Result<(), SafetyError>;

    fn verify_branch_absent(&self, facts: &RunFacts, branch: &str) -> Result<(), SafetyError>;

    fn verify_integration_in_progress(
        &self,
        facts: &RunFacts,
        _target_branch: &str,
    ) -> Result<(), SafetyError> {
        self.verify_frozen(facts)
    }

    fn verify_integration_patch_ready(
        &self,
        _facts: &RunFacts,
        _target_branch: &str,
    ) -> Result<String, SafetyError> {
        Err(SafetyError::new(
            "PATCH_UNAVAILABLE",
            "repository safety provider does not support controlled integration patches",
        ))
    }

    fn apply_integration_patch(
        &self,
        _facts: &RunFacts,
        _target_branch: &str,
        _patch: &str,
    ) -> Result<(String, Vec<PathBuf>), SafetyError> {
        Err(SafetyError::new(
            "PATCH_UNAVAILABLE",
            "repository safety provider does not support controlled integration patches",
        ))
    }

    fn verify_integration(
        &self,
        facts: &RunFacts,
        branch: &str,
        sha: &str,
        changed_files: &[PathBuf],
    ) -> Result<(), SafetyError>;

    fn authoritative_integration_result(
        &self,
        _facts: &RunFacts,
        _target_branch: &str,
    ) -> Result<(String, Vec<PathBuf>), SafetyError> {
        Err(SafetyError::new(
            "INTEGRATION_INSPECTION_UNAVAILABLE",
            "repository safety provider cannot derive an authoritative integration result",
        ))
    }

    fn prepare_verification_workspace(
        &self,
        facts: &RunFacts,
        integration_sha: &str,
        destination: &std::path::Path,
    ) -> Result<PathBuf, SafetyError>;

    fn recover_verification_workspace(
        &self,
        facts: &RunFacts,
        integration_sha: &str,
        destination: &std::path::Path,
    ) -> Result<PathBuf, SafetyError> {
        self.prepare_verification_workspace(facts, integration_sha, destination)
    }
}

#[derive(Debug, Clone, Default)]
pub struct GitRepositorySafety {
    inspector: GitInspector,
}

impl GitRepositorySafety {
    fn inspect_frozen_worktree(
        &self,
        path: &Path,
        role: &str,
    ) -> Result<WorktreeSnapshot, SafetyError> {
        let canonical = fs::canonicalize(path).map_err(|error| {
            SafetyError::new(
                "WORKTREE_UNAVAILABLE",
                format!(
                    "{role} frozen worktree {} is unavailable: {error}",
                    path.display()
                ),
            )
        })?;
        match fs::symlink_metadata(canonical.join(".git")) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(SafetyError::new(
                    "SOURCE_DRIFT",
                    format!(
                        "{role} frozen path {} no longer identifies a Git worktree",
                        path.display()
                    ),
                ));
            }
            Err(error) => {
                return Err(SafetyError::new(
                    "WORKTREE_UNAVAILABLE",
                    format!(
                        "cannot inspect {role} frozen worktree {}: {error}",
                        path.display()
                    ),
                ));
            }
        }
        self.inspector.inspect_worktree(path).map_err(Into::into)
    }
}

impl RepositorySafety for GitRepositorySafety {
    fn verify_frozen(&self, facts: &RunFacts) -> Result<(), SafetyError> {
        let primary = self.inspect_frozen_worktree(&facts.primary_worktree, "primary")?;
        let reviewer = self.inspect_frozen_worktree(&facts.reviewer_worktree, "reviewer")?;
        verify_same_repository(&primary, &reviewer)?;
        verify_frozen_sources(facts, &primary, &reviewer).map_err(Into::into)
    }

    fn verify_branch_absent(&self, facts: &RunFacts, branch: &str) -> Result<(), SafetyError> {
        self.inspect_frozen_worktree(&facts.primary_worktree, "primary")?;
        self.inspector
            .verify_integration_branch_absent(&facts.primary_worktree, branch)
            .map_err(Into::into)
    }

    fn verify_integration_in_progress(
        &self,
        facts: &RunFacts,
        target_branch: &str,
    ) -> Result<(), SafetyError> {
        let primary = self.inspect_frozen_worktree(&facts.primary_worktree, "primary")?;
        let reviewer = self.inspect_frozen_worktree(&facts.reviewer_worktree, "reviewer")?;
        verify_reviewer_frozen(facts, &reviewer)?;
        if primary.worktree != facts.primary_worktree || primary.common_dir != facts.git_common_dir
        {
            return Err(SafetyError::new(
                "SOURCE_DRIFT",
                "primary task left its frozen repository during integration",
            ));
        }
        let target_ref = format!("refs/heads/{target_branch}");
        match (&primary.source_ref, facts.primary_ref.as_deref()) {
            (Some(current), Some(original))
                if current.name == original && current.target_sha == facts.primary_sha => {}
            (Some(current), _) if current.name == target_ref => {}
            (None, None) => {}
            _ => {
                return Err(SafetyError::new(
                    "SOURCE_DRIFT",
                    "primary task is neither on its frozen source nor the authorized integration branch",
                ));
            }
        }
        self.inspector
            .verify_source_refs_unchanged(&facts.primary_worktree, facts)?;
        Ok(())
    }

    fn verify_integration_patch_ready(
        &self,
        facts: &RunFacts,
        target_branch: &str,
    ) -> Result<String, SafetyError> {
        let primary = self.inspect_frozen_worktree(&facts.primary_worktree, "primary")?;
        let reviewer = self.inspect_frozen_worktree(&facts.reviewer_worktree, "reviewer")?;
        verify_reviewer_frozen(facts, &reviewer)?;
        if primary.worktree != facts.primary_worktree
            || primary.common_dir != facts.git_common_dir
            || !primary.clean
        {
            return Err(SafetyError::new(
                "DIRTY_WORKTREE",
                "controlled patch requires the exact clean frozen primary worktree",
            ));
        }
        let target_ref = format!("refs/heads/{target_branch}");
        if primary
            .source_ref
            .as_ref()
            .is_none_or(|source| source.name != target_ref || source.target_sha != primary.head_sha)
        {
            return Err(SafetyError::new(
                "UNEXPECTED_INTEGRATION_BRANCH",
                "controlled patch is not on the exact authorized integration branch",
            ));
        }
        let integration = self
            .inspector
            .inspect_integration(&facts.primary_worktree, facts)?;
        verify_integration_result(facts, &integration, target_branch, &primary.head_sha)?;
        Ok(primary.head_sha)
    }

    fn apply_integration_patch(
        &self,
        facts: &RunFacts,
        target_branch: &str,
        patch: &str,
    ) -> Result<(String, Vec<PathBuf>), SafetyError> {
        let base_sha = self.verify_integration_patch_ready(facts, target_branch)?;
        let changed_files =
            self.inspector
                .apply_checked_text_patch(&facts.primary_worktree, &base_sha, patch)?;
        self.inspector
            .verify_source_refs_unchanged(&facts.primary_worktree, facts)?;
        Ok((base_sha, changed_files))
    }

    fn verify_integration(
        &self,
        facts: &RunFacts,
        branch: &str,
        sha: &str,
        changed_files: &[PathBuf],
    ) -> Result<(), SafetyError> {
        self.inspect_frozen_worktree(&facts.primary_worktree, "primary")?;
        let reviewer = self.inspect_frozen_worktree(&facts.reviewer_worktree, "reviewer")?;
        verify_reviewer_frozen(facts, &reviewer)?;
        let integration = self
            .inspector
            .inspect_integration(&facts.primary_worktree, facts)?;
        verify_reported_changed_files(&integration, changed_files)?;
        verify_integration_result(facts, &integration, branch, sha).map_err(Into::into)
    }

    fn authoritative_integration_result(
        &self,
        facts: &RunFacts,
        target_branch: &str,
    ) -> Result<(String, Vec<PathBuf>), SafetyError> {
        self.inspect_frozen_worktree(&facts.primary_worktree, "primary")?;
        let reviewer = self.inspect_frozen_worktree(&facts.reviewer_worktree, "reviewer")?;
        verify_reviewer_frozen(facts, &reviewer)?;
        let integration = self
            .inspector
            .inspect_integration(&facts.primary_worktree, facts)?;
        let sha = integration.worktree.head_sha.clone();
        verify_integration_result(facts, &integration, target_branch, &sha)?;
        Ok((sha, integration.changed_files))
    }

    fn prepare_verification_workspace(
        &self,
        facts: &RunFacts,
        integration_sha: &str,
        destination: &std::path::Path,
    ) -> Result<PathBuf, SafetyError> {
        self.inspect_frozen_worktree(&facts.primary_worktree, "primary")?;
        self.inspector
            .materialize_verification_clone(
                &facts.primary_worktree,
                destination,
                integration_sha,
                &facts.git_common_dir,
            )
            .map_err(Into::into)
    }

    fn recover_verification_workspace(
        &self,
        facts: &RunFacts,
        integration_sha: &str,
        destination: &std::path::Path,
    ) -> Result<PathBuf, SafetyError> {
        self.inspector
            .recover_verification_clone(destination, integration_sha, &facts.git_common_dir)
            .map_err(Into::into)
    }
}

#[derive(Debug, Error)]
pub enum CoordinatorError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{code}: {detail}")]
    Operational {
        code: String,
        detail: String,
        operation: Option<String>,
        thread_id: Option<String>,
    },
}

impl CoordinatorError {
    pub fn code(&self) -> &str {
        match self {
            Self::Store(error) => error.code(),
            Self::Operational { code, .. } => code,
        }
    }

    fn operational(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::Operational {
            code: code.into(),
            detail: detail.into(),
            operation: None,
            thread_id: None,
        }
    }

    fn app_server(
        code: impl Into<String>,
        detail: impl Into<String>,
        operation: impl Into<String>,
        thread_id: Option<&str>,
    ) -> Self {
        Self::Operational {
            code: code.into(),
            detail: detail.into(),
            operation: Some(operation.into()),
            thread_id: thread_id.map(str::to_owned),
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Self::Store(error) => error.to_string(),
            Self::Operational { detail, .. } => detail.clone(),
        }
    }

    pub fn operation(&self) -> Option<&str> {
        match self {
            Self::Store(_) => None,
            Self::Operational { operation, .. } => operation.as_deref(),
        }
    }

    pub fn thread_id(&self) -> Option<&str> {
        match self {
            Self::Store(_) => None,
            Self::Operational { thread_id, .. } => thread_id.as_deref(),
        }
    }
}

impl From<SafetyError> for CoordinatorError {
    fn from(error: SafetyError) -> Self {
        Self::operational(error.code, error.detail)
    }
}

impl From<StateError> for CoordinatorError {
    fn from(error: StateError) -> Self {
        Self::operational(error.code(), error.detail())
    }
}

impl From<PromptError> for CoordinatorError {
    fn from(error: PromptError) -> Self {
        Self::operational(error.code(), error.to_string())
    }
}

pub struct Coordinator<A, R> {
    app: Arc<A>,
    store: SqliteRunStore,
    safety: Arc<R>,
    options: CoordinatorOptions,
    driver_lock: Arc<Mutex<()>>,
    patch_lock: Arc<Mutex<()>>,
}

impl<A, R> Clone for Coordinator<A, R> {
    fn clone(&self) -> Self {
        Self {
            app: Arc::clone(&self.app),
            store: self.store.clone(),
            safety: Arc::clone(&self.safety),
            options: self.options.clone(),
            driver_lock: Arc::clone(&self.driver_lock),
            patch_lock: Arc::clone(&self.patch_lock),
        }
    }
}

impl<A, R> Coordinator<A, R>
where
    A: AppServer + 'static,
    R: RepositorySafety + 'static,
{
    pub fn new(
        app: Arc<A>,
        store: SqliteRunStore,
        safety: Arc<R>,
        options: CoordinatorOptions,
    ) -> Self {
        Self {
            app,
            store,
            safety,
            options,
            driver_lock: Arc::new(Mutex::new(())),
            patch_lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn apply_patch(
        &self,
        run_id: &str,
        request_hash: &str,
        patch: &str,
    ) -> Result<IntegrationPatchResult, CoordinatorError> {
        let _guard = self.patch_lock.lock().await;
        let state = self.required_run(run_id)?;
        if state.status != RunStatus::Running
            || state.phase != Phase::Integrate
            || state.next_action != NextAction::RequestPrimaryIntegration
            || state.integration_branch.is_some()
            || state.integration_sha.is_some()
            || state.current_integration_payload.is_some()
        {
            return Err(CoordinatorError::operational(
                "PATCH_NOT_AUTHORIZED",
                "controlled patch is limited to the active primary integration turn before a result is reported",
            ));
        }
        let pending = self.store.pending_send(run_id)?.ok_or_else(|| {
            CoordinatorError::operational(
                "PATCH_NOT_AUTHORIZED",
                "controlled patch requires an exact persisted primary integration turn",
            )
        })?;
        if request_hash.trim().is_empty()
            || pending.message_hash != request_hash
            || pending.role != "PRIMARY"
            || pending.phase != "INTEGRATE"
            || pending.round != state.round
            || pending.thread_id.as_deref() != Some(state.facts.primary_thread_id.as_str())
            || pending.turn_id.as_deref().is_none_or(str::is_empty)
        {
            return Err(CoordinatorError::operational(
                "PATCH_NOT_AUTHORIZED",
                "controlled patch identity does not match the exact active primary integration request",
            ));
        }
        if self.store.successful_patch_recorded(run_id, request_hash)? {
            return Err(CoordinatorError::operational(
                "PATCH_ALREADY_APPLIED",
                "the active primary integration request already used its one successful controlled patch",
            ));
        }
        let target = state.target_integration_branch.as_deref().ok_or_else(|| {
            CoordinatorError::operational("INVALID_STATE", "target integration branch is missing")
        })?;
        let (base_sha, changed_files) =
            self.safety
                .apply_integration_patch(&state.facts, target, patch)?;
        let patch_hash = canonical_json_hash(&json!({"patch": patch}));
        self.store
            .record_successful_patch(run_id, request_hash, &patch_hash)?;
        Ok(IntegrationPatchResult {
            run_id: run_id.to_owned(),
            integration_branch: target.to_owned(),
            base_sha,
            changed_files,
        })
    }

    pub async fn start(
        &self,
        mut state: RunState,
        request: StartRequest,
    ) -> Result<RunState, CoordinatorError> {
        if state.facts.primary_thread_id == state.facts.reviewer_thread_id {
            return Err(CoordinatorError::operational(
                "AMBIGUOUS_THREAD",
                "primary and reviewer task IDs must differ",
            ));
        }
        self.ensure_controlled_patch_approval().await?;
        let requested_branch = request
            .integration_branch
            .unwrap_or_else(|| format!("consensus/{}", state.facts.run_id));
        let branch = normalize_branch_name(&requested_branch).map_err(SafetyError::from)?;
        if request
            .test_commands
            .iter()
            .any(|command| !validate_test_command(command))
        {
            return Err(CoordinatorError::operational(
                "INVALID_TEST_COMMAND",
                "test commands must be nonempty, single commands without publication or destructive Git operations",
            ));
        }
        state.configure_integration(branch.clone(), request.test_commands)?;

        let primary = self
            .read_thread_with_retry(&state.facts.primary_thread_id)
            .await?;
        let reviewer = self
            .read_thread_with_retry(&state.facts.reviewer_thread_id)
            .await?;
        self.verify_thread_identity(&state, Role::Primary, &primary)?;
        self.verify_thread_identity(&state, Role::Reviewer, &reviewer)?;
        self.safety.verify_frozen(&state.facts)?;
        self.safety.verify_branch_absent(&state.facts, &branch)?;
        self.store.insert_run(&state)?;
        Ok(state)
    }

    pub async fn recover_startup_runs(&self) -> Result<Vec<RunState>, CoordinatorError> {
        let _guard = self.driver_lock.lock().await;
        let mut recovered = Vec::new();
        for summary in self.store.list_runs()?.into_iter().filter(|summary| {
            summary.status == "BLOCKED" && summary.reason_code.as_deref() == Some("DATABASE_ERROR")
        }) {
            let Some(candidate) = self
                .store
                .v025_verification_completion_collision_candidate(&summary.run_id)?
            else {
                continue;
            };
            let state = &candidate.blocked_state;
            self.revalidate_current_repository(state).await?;
            let thread_id = candidate.pending.thread_id.as_deref().ok_or_else(|| {
                CoordinatorError::operational(
                    "HISTORY_UNAVAILABLE",
                    "v0.2.5 completion collision has no bound primary task",
                )
            })?;
            let turn_id = candidate.pending.turn_id.as_deref().ok_or_else(|| {
                CoordinatorError::operational(
                    "HISTORY_UNAVAILABLE",
                    "v0.2.5 completion collision has no bound verification turn",
                )
            })?;
            let detail = self.read_thread_with_retry(thread_id).await?;
            self.verify_thread_identity(state, Role::Primary, &detail)?;
            let turn = find_turn(&detail, turn_id).ok_or_else(|| {
                CoordinatorError::operational(
                    "HISTORY_UNAVAILABLE",
                    "v0.2.5 completion-collision turn is absent from canonical task history",
                )
            })?;
            if turn.get("status").and_then(Value::as_str) != Some("completed")
                || !turn_contains_request_hash(turn, &candidate.pending.message_hash)
            {
                return Err(CoordinatorError::operational(
                    "HISTORY_UNAVAILABLE",
                    "v0.2.5 completion recovery requires the exact completed request turn",
                ));
            }
            if let Some(blocker) = verification_without_execution_retry_blocker(turn) {
                return Err(CoordinatorError::operational(
                    "MODEL_RESPONSE_RETRY_UNSAFE",
                    format!("v0.2.5 completion-collision turn cannot be recovered: {blocker}"),
                ));
            }
            let response = parse_participant_response(
                final_agent_text(turn)?.trim(),
                allowed_participant_signals(NextAction::RequestPrimaryVerification),
            )
            .map_err(|error| {
                CoordinatorError::operational("HISTORY_UNAVAILABLE", error.to_string())
            })?;
            if response.signal != ParticipantSignal::VerificationReady {
                return Err(CoordinatorError::operational(
                    "MODEL_RESPONSE_RETRY_UNSAFE",
                    "v0.2.5 completion recovery requires the final VERIFICATION_READY marker",
                ));
            }

            self.revalidate_current_repository(state).await?;
            let mut recovered_state = state.clone();
            recovered_state.recover_v025_verification_completion_collision()?;
            self.store
                .recover_v025_verification_completion_collision(&candidate, &recovered_state)?;
            recovered.push(recovered_state);
        }
        Ok(recovered)
    }

    pub async fn drive(&self, run_id: &str) -> Result<RunState, CoordinatorError> {
        let _guard = self.driver_lock.lock().await;
        for _ in 0..MAX_DRIVER_STEPS {
            let mut state = self.required_run(run_id)?;
            match state.status {
                RunStatus::Accepted
                | RunStatus::Blocked
                | RunStatus::Cancelled
                | RunStatus::IncompatibleCodex
                | RunStatus::PausedUserAction => return Ok(state),
                RunStatus::Running | RunStatus::WaitingThread => {}
            }

            let action = state.next_action;
            let step = match action {
                NextAction::RevalidateAndAccept => self.revalidate_and_accept(&mut state).await,
                NextAction::WaitForUser => return Ok(state),
                NextAction::Stop => return Ok(state),
                _ => self.drive_model_action(&mut state, action).await,
            };
            if let Err(error) = step {
                let persisted = self.required_run(run_id)?;
                if matches!(
                    persisted.status,
                    RunStatus::PausedUserAction | RunStatus::Cancelled
                ) {
                    return Ok(persisted);
                }
                state = persisted;
                state.record_error(run_diagnostic(&state, action, &error));
                if error.code() == "COMMUNICATION_FAILURE" {
                    state.pause("COMMUNICATION_FAILURE")?;
                    self.store.save_state(&state)?;
                    return Ok(state);
                }
                if error.code() == "INVALID_TEST_COMMAND" && is_test_declaration_action(action) {
                    state.pause("INVALID_TEST_COMMAND")?;
                    self.store.save_state(&state)?;
                    return Ok(state);
                }
                if error.code() == "INCOMPATIBLE_CODEX" {
                    state.mark_incompatible("INCOMPATIBLE_CODEX");
                    self.store.save_state(&state)?;
                    return Ok(state);
                }
                let reason = error.code().to_owned();
                state.block(&reason);
                self.store.save_state(&state)?;
                return Ok(state);
            }
        }

        let mut state = self.required_run(run_id)?;
        state.block("NO_PROGRESS");
        self.store.save_state(&state)?;
        Ok(state)
    }

    pub async fn resume(&self, run_id: &str) -> Result<RunState, CoordinatorError> {
        self.prepare_resume(run_id).await?;
        self.drive(run_id).await
    }

    pub async fn prepare_resume(&self, run_id: &str) -> Result<RunState, CoordinatorError> {
        let mut state = self.required_run(run_id)?;
        let retry_terminal_turn = state.reason_code.as_deref() == Some("COMMUNICATION_FAILURE");
        let retry_invalid_test_action = invalid_test_retry_action(&state)?;
        let retry_integration_invalid_response_action =
            integration_invalid_response_retry_action(&state)?;
        let retry_verification_without_execution_action =
            verification_without_execution_retry_action(&state)?;
        let retry_verification_environment_action =
            verification_environment_unavailable_retry_action(&state)?;
        let retry_invalid_response_action = if retry_integration_invalid_response_action.is_none() {
            invalid_response_retry_action(&state)?
        } else {
            None
        };
        let retry_execution_tool_action = execution_tool_unavailable_retry_action(&state)?;
        let retry_forbidden_operation_action = forbidden_operation_retry_action(&state)?;
        let retry_completed_response_action =
            retry_invalid_test_action.or(retry_invalid_response_action);
        let effective_action = retry_execution_tool_action
            .or(retry_forbidden_operation_action)
            .or(retry_integration_invalid_response_action)
            .or(retry_verification_without_execution_action)
            .or(retry_verification_environment_action)
            .or(retry_completed_response_action)
            .unwrap_or(state.next_action);
        if effective_action == NextAction::RequestPrimaryIntegration {
            self.ensure_controlled_patch_approval().await?;
        }
        if retry_execution_tool_action.is_some() || retry_forbidden_operation_action.is_some() {
            let target = state.target_integration_branch.as_deref().ok_or_else(|| {
                CoordinatorError::operational(
                    "INVALID_STATE",
                    "target integration branch is missing",
                )
            })?;
            self.safety.verify_frozen(&state.facts)?;
            self.safety.verify_branch_absent(&state.facts, target)?;
        } else if effective_action == NextAction::RequestPrimaryIntegration {
            let target = state.target_integration_branch.as_deref().ok_or_else(|| {
                CoordinatorError::operational(
                    "INVALID_STATE",
                    "target integration branch is missing",
                )
            })?;
            self.safety
                .verify_integration_in_progress(&state.facts, target)?;
        } else {
            self.revalidate_current_repository(&state).await?;
        }
        if retry_terminal_turn {
            let completed_tool_failure_retry = if let Some(retry) = self
                .inspect_completed_patch_not_authorized_retry(&state)
                .await?
            {
                Some(retry)
            } else {
                self.inspect_completed_file_change_tool_unavailable_retry(&state)
                    .await?
            };
            if let Some(retry) = completed_tool_failure_retry {
                self.store
                    .reset_completed_integration_tool_failure_turn_for_retry(
                        run_id,
                        &retry.message_hash,
                        &retry.thread_id,
                        &retry.turn_id,
                        &retry.observed_status,
                    )?;
            } else {
                self.prepare_terminal_turn_retry(&state).await?;
            }
        }
        if let Some(action) = retry_forbidden_operation_action {
            let retry = self
                .inspect_interrupted_forbidden_operation_retry(&state, action)
                .await?;
            let blocked_state = state.clone();
            let restored_action = state.retry_blocked_preintegration_forbidden_operation()?;
            if restored_action != action {
                return Err(CoordinatorError::operational(
                    "INCOMPATIBLE_STATE",
                    "restored forbidden-operation action does not match its interrupted turn",
                ));
            }
            self.store
                .reactivate_blocked_run_with_interrupted_forbidden_operation_retry(
                    &blocked_state,
                    &state,
                    &retry.message_hash,
                    &retry.thread_id,
                    &retry.turn_id,
                    &retry.observed_status,
                )?;
            return Ok(state);
        }
        if let Some(action) = retry_execution_tool_action {
            let retry = self
                .inspect_completed_execution_tool_unavailable_retry(&state, action)
                .await?;
            let blocked_state = state.clone();
            let restored_action = state.retry_blocked_integration_execution_tool_unavailable()?;
            if restored_action != action {
                return Err(CoordinatorError::operational(
                    "INCOMPATIBLE_STATE",
                    "restored execution-tool action does not match its accepted blocker",
                ));
            }
            self.store
                .reactivate_blocked_run_with_accepted_execution_tool_retry(
                    &blocked_state,
                    &state,
                    &retry.accepted,
                    &retry.observed_status,
                )?;
            return Ok(state);
        }
        if let Some(action) = retry_integration_invalid_response_action {
            let retry = self
                .inspect_completed_integration_invalid_response_retry(&state, action)
                .await?;
            let blocked_state = state.clone();
            let restored_action = state.retry_blocked_integration_invalid_response()?;
            if restored_action != action {
                return Err(CoordinatorError::operational(
                    "INCOMPATIBLE_STATE",
                    "restored integration invalid-response action does not match its diagnostic",
                ));
            }
            self.store
                .reactivate_blocked_run_with_completed_turn_retry(
                    &blocked_state,
                    &state,
                    &retry.message_hash,
                    &retry.thread_id,
                    &retry.turn_id,
                    &retry.observed_status,
                )?;
            return Ok(state);
        }
        if let Some(action) = retry_verification_without_execution_action {
            let retry = self
                .inspect_completed_verification_without_execution_retry(&state, action)
                .await?;
            let blocked_state = state.clone();
            let restored_action = state.retry_blocked_verification_without_execution()?;
            if restored_action != action {
                return Err(CoordinatorError::operational(
                    "INCOMPATIBLE_STATE",
                    "restored verification action does not match its failed turn",
                ));
            }
            let retry_turn = &retry.turn;
            match retry.kind {
                VerificationRetryKind::EmptyTurn => {
                    self.store
                        .reactivate_blocked_run_with_completed_turn_retry(
                            &blocked_state,
                            &state,
                            &retry_turn.message_hash,
                            &retry_turn.thread_id,
                            &retry_turn.turn_id,
                            &retry_turn.observed_status,
                        )?;
                }
                VerificationRetryKind::EventEvidenceCompatibility => {
                    self.store
                        .reactivate_blocked_run_with_verification_evidence_retry(
                            &blocked_state,
                            &state,
                            &retry_turn.message_hash,
                            &retry_turn.thread_id,
                            &retry_turn.turn_id,
                            &retry_turn.observed_status,
                        )?;
                }
                VerificationRetryKind::UnattendedVerificationMigration => {
                    self.store
                        .reactivate_blocked_run_with_unattended_verification_retry(
                            &blocked_state,
                            &state,
                            &retry_turn.message_hash,
                            &retry_turn.thread_id,
                            &retry_turn.turn_id,
                            &retry_turn.observed_status,
                        )?;
                }
            }
            return Ok(state);
        }
        if let Some(action) = retry_verification_environment_action {
            let retry = self
                .inspect_completed_verification_environment_unavailable_retry(&state, action)
                .await?;
            let blocked_state = state.clone();
            let restored_action = state.retry_blocked_verification_environment_unavailable()?;
            if restored_action != action {
                return Err(CoordinatorError::operational(
                    "INCOMPATIBLE_STATE",
                    "restored verification action does not match its environment-blocked turn",
                ));
            }
            self.store
                .reactivate_blocked_run_with_accepted_verification_environment_retry(
                    &blocked_state,
                    &state,
                    &retry.accepted,
                    &retry.observed_status,
                )?;
            return Ok(state);
        }
        if let Some(action) = retry_completed_response_action {
            let retry = self
                .inspect_completed_read_only_model_response_retry(&state, action)
                .await?;
            if state.status == RunStatus::Blocked {
                let blocked_state = state.clone();
                let restored_action = match state.reason_code.as_deref() {
                    Some("INVALID_TEST_COMMAND") => state.retry_blocked_invalid_test_command()?,
                    Some("INVALID_RESPONSE") => {
                        state.retry_blocked_preintegration_invalid_response()?
                    }
                    _ => {
                        return Err(CoordinatorError::operational(
                            "INCOMPATIBLE_STATE",
                            "completed model-response retry has an unsupported blocked reason",
                        ));
                    }
                };
                if restored_action != action {
                    return Err(CoordinatorError::operational(
                        "INCOMPATIBLE_STATE",
                        "restored model-response action does not match its diagnostic",
                    ));
                }
                self.store
                    .reactivate_blocked_run_with_completed_turn_retry(
                        &blocked_state,
                        &state,
                        &retry.message_hash,
                        &retry.thread_id,
                        &retry.turn_id,
                        &retry.observed_status,
                    )?;
                return Ok(state);
            }
            self.store.reset_completed_read_only_turn_for_retry(
                run_id,
                &retry.message_hash,
                &retry.thread_id,
                &retry.turn_id,
                &retry.observed_status,
            )?;
        }
        state.resume()?;
        self.store.save_state(&state)?;
        Ok(state)
    }

    pub async fn cancel(&self, run_id: &str) -> Result<RunState, CoordinatorError> {
        let mut state = self.required_run(run_id)?;
        state.cancel();
        self.store.save_state(&state)?;
        Ok(state)
    }

    pub async fn check_app_server(&self) -> Result<(), CoordinatorError> {
        self.app
            .list_threads(None, 1)
            .await
            .map_err(|error| communication_error("thread/list", None, error))?;
        Ok(())
    }

    async fn ensure_controlled_patch_approval(&self) -> Result<(), CoordinatorError> {
        let mode = self
            .app
            .controlled_patch_approval_mode()
            .await
            .map_err(|error| communication_error("config/read", None, error))?;
        if mode.as_deref() != Some(CONTROLLED_PATCH_APPROVAL_MODE) {
            return Err(CoordinatorError::operational(
                "APPROVAL_CONFIGURATION_REQUIRED",
                format!(
                    "{CONTROLLED_PATCH_APPROVAL_KEY} must equal {CONTROLLED_PATCH_APPROVAL_MODE}; run `codex-consensus configure` once, then retry the same run"
                ),
            ));
        }
        Ok(())
    }

    async fn prepare_terminal_turn_retry(&self, state: &RunState) -> Result<(), CoordinatorError> {
        let run_id = state.facts.run_id.to_string();
        let Some(pending) = self.store.pending_send(&run_id)? else {
            return Ok(());
        };
        let (Some(thread_id), Some(turn_id)) =
            (pending.thread_id.as_deref(), pending.turn_id.as_deref())
        else {
            if pending.thread_id.is_none() && pending.turn_id.is_none() {
                return Ok(());
            }
            return Err(CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "pending turn has only one of thread_id and turn_id",
            ));
        };
        let role = action_role(state.next_action).ok_or_else(|| {
            CoordinatorError::operational(
                "INVALID_STATE",
                "paused communication action has no task role",
            )
        })?;
        let expected_thread_id = role_thread_id(state, role);
        if pending.role != role_name(role) || thread_id != expected_thread_id {
            return Err(CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "pending turn identity does not match the deterministic current action",
            ));
        }

        let detail = self.read_thread_with_retry(thread_id).await?;
        self.verify_thread_identity(state, role, &detail)?;
        let turn = find_turn(&detail, turn_id).ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "persisted pending turn is absent from canonical task history",
            )
        })?;
        let status = turn.get("status").and_then(Value::as_str).ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "persisted pending turn has no canonical status",
            )
        })?;
        if !turn_contains_request_hash(turn, &pending.message_hash) {
            return Err(CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "persisted pending turn lacks its deterministic request marker",
            ));
        }
        if status == "inProgress"
            && self
                .prepare_pending_controlled_patch_approval_retry(
                    state,
                    &pending.message_hash,
                    thread_id,
                    turn_id,
                    &detail,
                    turn,
                )
                .await?
        {
            return Ok(());
        }
        if !matches!(status, "failed" | "interrupted") {
            return Ok(());
        }
        if let Some(blocker) = terminal_turn_retry_blocker(turn) {
            return Err(CoordinatorError::operational(
                "TERMINAL_TURN_RETRY_UNSAFE",
                format!("terminal turn {turn_id} cannot be retried automatically: {blocker}"),
            ));
        }
        self.store.reset_terminal_turn_for_retry(
            &run_id,
            &pending.message_hash,
            thread_id,
            turn_id,
            status,
        )?;
        Ok(())
    }

    async fn prepare_pending_controlled_patch_approval_retry(
        &self,
        state: &RunState,
        message_hash: &str,
        thread_id: &str,
        turn_id: &str,
        detail: &ThreadDetail,
        turn: &Value,
    ) -> Result<bool, CoordinatorError> {
        if state.status != RunStatus::PausedUserAction
            || state.reason_code.as_deref() != Some("COMMUNICATION_FAILURE")
            || state.phase != Phase::Integrate
            || state.next_action != NextAction::RequestPrimaryIntegration
            || state.integration_branch.is_some()
            || state.integration_sha.is_some()
            || state.current_integration_payload.is_some()
            || thread_id != state.facts.primary_thread_id
            || !turn
                .get("items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|item| item.get("type").and_then(Value::as_str) == Some("mcpToolCall"))
        {
            return Ok(false);
        }
        if self
            .store
            .successful_patch_recorded(&state.facts.run_id.to_string(), message_hash)?
        {
            return Err(CoordinatorError::operational(
                "TERMINAL_TURN_RETRY_UNSAFE",
                "an in-progress approval turn already has a successful controlled patch record",
            ));
        }
        let retry_failed_patch_sha = if let Some(blocker) =
            pending_controlled_patch_approval_blocker(
                state,
                detail,
                turn,
                message_hash,
                &["inProgress"],
            ) {
            if pending_controlled_patch_approval_blocker(
                state,
                detail,
                turn,
                message_hash,
                &["failed"],
            )
            .is_some()
            {
                return Err(CoordinatorError::operational(
                    "TERMINAL_TURN_RETRY_UNSAFE",
                    blocker,
                ));
            }
            Some(
                self.verify_failed_controlled_patch_retry_turn(state, detail, turn, message_hash)
                    .await?,
            )
        } else {
            None
        };

        let interrupt_error = self.app.interrupt_turn(thread_id, turn_id).await.err();
        let deadline = tokio::time::Instant::now()
            + std::cmp::min(self.options.wait_timeout, TURN_INTERRUPT_TIMEOUT);
        loop {
            let current = self.read_thread_with_retry(thread_id).await?;
            self.verify_thread_identity(state, Role::Primary, &current)?;
            let current_turn = find_turn(&current, turn_id).ok_or_else(|| {
                CoordinatorError::operational(
                    "HISTORY_UNAVAILABLE",
                    "interrupted controlled patch turn disappeared from canonical history",
                )
            })?;
            let status = current_turn
                .get("status")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    CoordinatorError::operational(
                        "HISTORY_UNAVAILABLE",
                        "interrupted controlled patch turn has no canonical status",
                    )
                })?;
            match status {
                "completed" => {
                    if let Some(expected_sha) = retry_failed_patch_sha.as_deref() {
                        let current_sha = self
                            .verify_failed_controlled_patch_retry_turn(
                                state,
                                &current,
                                current_turn,
                                message_hash,
                            )
                            .await?;
                        if current_sha != expected_sha {
                            return Err(CoordinatorError::operational(
                                "TERMINAL_TURN_RETRY_UNSAFE",
                                "authorized integration HEAD changed while the failed controlled-patch turn was interrupted",
                            ));
                        }
                        self.store
                            .reset_completed_integration_tool_failure_turn_for_retry(
                                &state.facts.run_id.to_string(),
                                message_hash,
                                thread_id,
                                turn_id,
                                status,
                            )?;
                    }
                    return Ok(true);
                }
                "failed" | "interrupted" => {
                    if let Some(expected_sha) = retry_failed_patch_sha.as_deref() {
                        let current_sha = self
                            .verify_failed_controlled_patch_retry_turn(
                                state,
                                &current,
                                current_turn,
                                message_hash,
                            )
                            .await?;
                        if current_sha != expected_sha {
                            return Err(CoordinatorError::operational(
                                "TERMINAL_TURN_RETRY_UNSAFE",
                                "authorized integration HEAD changed while the failed controlled-patch turn was interrupted",
                            ));
                        }
                    } else if let Some(blocker) = pending_controlled_patch_approval_blocker(
                        state,
                        &current,
                        current_turn,
                        message_hash,
                        &["inProgress", "failed", "declined", "interrupted"],
                    ) {
                        return Err(CoordinatorError::operational(
                            "TERMINAL_TURN_RETRY_UNSAFE",
                            blocker,
                        ));
                    }
                    if self
                        .store
                        .successful_patch_recorded(&state.facts.run_id.to_string(), message_hash)?
                    {
                        return Err(CoordinatorError::operational(
                            "TERMINAL_TURN_RETRY_UNSAFE",
                            "controlled patch completed while its approval turn was interrupted",
                        ));
                    }
                    let target = state.target_integration_branch.as_deref().ok_or_else(|| {
                        CoordinatorError::operational(
                            "INVALID_STATE",
                            "target integration branch is missing",
                        )
                    })?;
                    self.safety
                        .verify_integration_in_progress(&state.facts, target)?;
                    self.store.reset_terminal_turn_for_retry(
                        &state.facts.run_id.to_string(),
                        message_hash,
                        thread_id,
                        turn_id,
                        status,
                    )?;
                    return Ok(true);
                }
                "inProgress" => {}
                other => {
                    return Err(CoordinatorError::operational(
                        "TERMINAL_TURN_RETRY_UNSAFE",
                        format!(
                            "controlled patch approval turn entered unsupported status {other}"
                        ),
                    ));
                }
            }
            if tokio::time::Instant::now() >= deadline {
                let detail = interrupt_error
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "turn remained in progress after interruption".into());
                return Err(CoordinatorError::operational(
                    "COMMUNICATION_FAILURE",
                    detail,
                ));
            }
            tokio::time::sleep(self.options.poll_interval).await;
        }
    }

    async fn verify_failed_controlled_patch_retry_turn(
        &self,
        state: &RunState,
        detail: &ThreadDetail,
        turn: &Value,
        message_hash: &str,
    ) -> Result<String, CoordinatorError> {
        let run_id = state.facts.run_id.to_string();
        if self
            .store
            .successful_patch_recorded(&run_id, message_hash)?
        {
            return Err(CoordinatorError::operational(
                "TERMINAL_TURN_RETRY_UNSAFE",
                "controlled patch was recorded as successful before its failed retry",
            ));
        }
        if let Some(blocker) = pending_controlled_patch_approval_blocker(
            state,
            detail,
            turn,
            message_hash,
            &["failed"],
        ) {
            return Err(CoordinatorError::operational(
                "TERMINAL_TURN_RETRY_UNSAFE",
                format!("failed controlled-patch turn cannot be retried: {blocker}"),
            ));
        }

        let has_agent_message = turn
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|item| item.get("type").and_then(Value::as_str) == Some("agentMessage"));
        if has_agent_message {
            return self
                .verify_patch_not_authorized_retry_turn(state, detail, turn, message_hash)
                .await;
        }

        let target = state.target_integration_branch.as_deref().ok_or_else(|| {
            CoordinatorError::operational("INVALID_STATE", "target integration branch is missing")
        })?;
        self.safety
            .verify_integration_patch_ready(&state.facts, target)
            .map_err(Into::into)
    }

    async fn inspect_completed_patch_not_authorized_retry(
        &self,
        state: &RunState,
    ) -> Result<Option<RetryableCompletedTurn>, CoordinatorError> {
        if state.status != RunStatus::PausedUserAction
            || state.reason_code.as_deref() != Some("COMMUNICATION_FAILURE")
            || state.phase != Phase::Integrate
            || state.next_action != NextAction::RequestPrimaryIntegration
            || state.integration_branch.is_some()
            || state.integration_sha.is_some()
            || state.current_integration_payload.is_some()
        {
            return Ok(None);
        }
        let run_id = state.facts.run_id.to_string();
        let Some(pending) = self.store.pending_send(&run_id)? else {
            return Ok(None);
        };
        let (Some(thread_id), Some(turn_id)) =
            (pending.thread_id.as_deref(), pending.turn_id.as_deref())
        else {
            return Ok(None);
        };
        if pending.role != "PRIMARY"
            || pending.phase != "INTEGRATE"
            || pending.round != state.round
            || thread_id != state.facts.primary_thread_id
        {
            return Ok(None);
        }

        let detail = self.read_thread_with_retry(thread_id).await?;
        self.verify_thread_identity(state, Role::Primary, &detail)?;
        let turn = find_turn(&detail, turn_id).ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "completed controlled-patch blocker is absent from canonical task history",
            )
        })?;
        let status = turn.get("status").and_then(Value::as_str).ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "completed controlled-patch blocker has no canonical status",
            )
        })?;
        if status != "completed" {
            return Ok(None);
        }
        if !turn_contains_request_hash(turn, &pending.message_hash) {
            return Err(CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "completed controlled-patch blocker lacks its deterministic request marker",
            ));
        }

        let raw_response = final_agent_json(turn)?;
        let message = validate_message(raw_response).map_err(|error| {
            CoordinatorError::operational("INVALID_RESPONSE", error.to_string())
        })?;
        if message.envelope.reason_code.as_deref() != Some("PATCH_NOT_AUTHORIZED") {
            return Ok(None);
        }
        self.verify_patch_not_authorized_retry_turn(state, &detail, turn, &pending.message_hash)
            .await?;

        Ok(Some(RetryableCompletedTurn {
            message_hash: pending.message_hash,
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
            observed_status: status.to_owned(),
        }))
    }

    async fn verify_patch_not_authorized_retry_turn(
        &self,
        state: &RunState,
        detail: &ThreadDetail,
        turn: &Value,
        message_hash: &str,
    ) -> Result<String, CoordinatorError> {
        let run_id = state.facts.run_id.to_string();
        if self
            .store
            .successful_patch_recorded(&run_id, message_hash)?
        {
            return Err(CoordinatorError::operational(
                "TERMINAL_TURN_RETRY_UNSAFE",
                "controlled patch was recorded as successful before PATCH_NOT_AUTHORIZED",
            ));
        }
        if let Some(blocker) = pending_controlled_patch_approval_blocker(
            state,
            detail,
            turn,
            message_hash,
            &["failed"],
        ) {
            return Err(CoordinatorError::operational(
                "TERMINAL_TURN_RETRY_UNSAFE",
                format!("controlled-patch blocker cannot be retried: {blocker}"),
            ));
        }
        let raw_response = final_agent_json(turn)?;
        let message = validate_message(raw_response).map_err(|error| {
            CoordinatorError::operational("INVALID_RESPONSE", error.to_string())
        })?;
        let reported_sha = validate_patch_not_authorized_blocker(state, message_hash, &message)?;
        let target = state.target_integration_branch.as_deref().ok_or_else(|| {
            CoordinatorError::operational("INVALID_STATE", "target integration branch is missing")
        })?;
        let authoritative_sha = self
            .safety
            .verify_integration_patch_ready(&state.facts, target)?;
        if authoritative_sha != reported_sha {
            return Err(CoordinatorError::operational(
                "TERMINAL_TURN_RETRY_UNSAFE",
                "reported clean integration HEAD does not match the authoritative target branch",
            ));
        }
        Ok(authoritative_sha)
    }

    async fn inspect_completed_file_change_tool_unavailable_retry(
        &self,
        state: &RunState,
    ) -> Result<Option<RetryableCompletedTurn>, CoordinatorError> {
        if state.status != RunStatus::PausedUserAction
            || state.reason_code.as_deref() != Some("COMMUNICATION_FAILURE")
            || state.phase != Phase::Integrate
            || state.next_action != NextAction::RequestPrimaryIntegration
            || state.integration_branch.is_some()
            || state.integration_sha.is_some()
            || state.current_integration_payload.is_some()
        {
            return Ok(None);
        }
        let run_id = state.facts.run_id.to_string();
        let Some(pending) = self.store.pending_send(&run_id)? else {
            return Ok(None);
        };
        let (Some(thread_id), Some(turn_id)) =
            (pending.thread_id.as_deref(), pending.turn_id.as_deref())
        else {
            return Ok(None);
        };
        if pending.role != "PRIMARY"
            || pending.phase != "INTEGRATE"
            || pending.round != state.round
            || thread_id != state.facts.primary_thread_id
        {
            return Ok(None);
        }

        let detail = self.read_thread_with_retry(thread_id).await?;
        self.verify_thread_identity(state, Role::Primary, &detail)?;
        let turn = find_turn(&detail, turn_id).ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "completed file-change blocker is absent from canonical task history",
            )
        })?;
        let status = turn.get("status").and_then(Value::as_str).ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "completed file-change blocker has no canonical status",
            )
        })?;
        if status != "completed" {
            return Ok(None);
        }
        if !turn_contains_request_hash(turn, &pending.message_hash) {
            return Err(CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "completed file-change blocker lacks its deterministic request marker",
            ));
        }

        let raw_response = final_agent_json(turn)?;
        let message = validate_message(raw_response).map_err(|error| {
            CoordinatorError::operational("INVALID_RESPONSE", error.to_string())
        })?;
        if message.envelope.reason_code.as_deref() != Some("FILE_CHANGE_TOOL_UNAVAILABLE") {
            return Ok(None);
        }
        if let Some(blocker) = terminal_turn_retry_blocker(turn) {
            return Err(CoordinatorError::operational(
                "TERMINAL_TURN_RETRY_UNSAFE",
                format!("completed file-change blocker cannot be retried: {blocker}"),
            ));
        }
        let reported_sha =
            validate_file_change_tool_unavailable_blocker(state, &pending.message_hash, &message)?;
        let target = state.target_integration_branch.as_deref().ok_or_else(|| {
            CoordinatorError::operational("INVALID_STATE", "target integration branch is missing")
        })?;
        let authoritative_sha = self
            .safety
            .verify_integration_patch_ready(&state.facts, target)?;
        if authoritative_sha != reported_sha {
            return Err(CoordinatorError::operational(
                "TERMINAL_TURN_RETRY_UNSAFE",
                "reported clean integration HEAD does not match the authoritative target branch",
            ));
        }

        Ok(Some(RetryableCompletedTurn {
            message_hash: pending.message_hash,
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
            observed_status: status.to_owned(),
        }))
    }

    async fn inspect_completed_integration_invalid_response_retry(
        &self,
        state: &RunState,
        action: NextAction,
    ) -> Result<RetryableCompletedTurn, CoordinatorError> {
        if action != NextAction::RequestPrimaryIntegration
            || state.integration_branch.is_some()
            || state.integration_sha.is_some()
            || state.current_integration_payload.is_some()
            || state.verification_worktree.is_some()
            || !state.test_evidence.is_empty()
        {
            return Err(CoordinatorError::operational(
                "MODEL_RESPONSE_RETRY_UNSAFE",
                "integration invalid-response recovery requires an unaccepted first result",
            ));
        }
        let run_id = state.facts.run_id.to_string();
        let pending = self.store.pending_send(&run_id)?.ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "integration invalid response has no persisted pending turn",
            )
        })?;
        let (Some(thread_id), Some(turn_id)) =
            (pending.thread_id.as_deref(), pending.turn_id.as_deref())
        else {
            return Err(CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "integration invalid response has no exact persisted turn identity",
            ));
        };
        if pending.role != "PRIMARY"
            || pending.phase != "INTEGRATE"
            || pending.round != state.round
            || thread_id != state.facts.primary_thread_id
        {
            return Err(CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "integration invalid-response turn does not match the bound primary request",
            ));
        }

        let detail = self.read_thread_with_retry(thread_id).await?;
        self.verify_thread_identity(state, Role::Primary, &detail)?;
        let turn = find_turn(&detail, turn_id).ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "integration invalid-response turn is absent from canonical task history",
            )
        })?;
        let status = turn.get("status").and_then(Value::as_str).ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "integration invalid-response turn has no canonical status",
            )
        })?;
        if status != "completed" || !turn_contains_request_hash(turn, &pending.message_hash) {
            return Err(CoordinatorError::operational(
                "MODEL_RESPONSE_RETRY_UNSAFE",
                "integration invalid-response recovery requires the exact completed request turn",
            ));
        }
        let successful_patch_hash = self
            .store
            .successful_patch_hash(&run_id, &pending.message_hash)?
            .ok_or_else(|| {
                CoordinatorError::operational(
                    "MODEL_RESPONSE_RETRY_UNSAFE",
                    "integration invalid-response turn has no successful controlled patch record",
                )
            })?;
        if let Some(blocker) = recoverable_integration_turn_blocker(
            state,
            turn,
            &pending.message_hash,
            &successful_patch_hash,
            true,
        ) {
            return Err(CoordinatorError::operational(
                "MODEL_RESPONSE_RETRY_UNSAFE",
                blocker,
            ));
        }
        let target = state.target_integration_branch.as_deref().ok_or_else(|| {
            CoordinatorError::operational(
                "INVALID_STATE",
                "integration invalid-response recovery has no authorized target branch",
            )
        })?;
        let (sha, changed_files) = self
            .safety
            .authoritative_integration_result(&state.facts, target)?;
        self.safety
            .verify_integration(&state.facts, target, &sha, &changed_files)?;

        Ok(RetryableCompletedTurn {
            message_hash: pending.message_hash,
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
            observed_status: status.to_owned(),
        })
    }

    async fn inspect_completed_read_only_model_response_retry(
        &self,
        state: &RunState,
        action: NextAction,
    ) -> Result<RetryableCompletedTurn, CoordinatorError> {
        if preintegration_read_only_phase(action).is_none()
            || state.integration_branch.is_some()
            || state.integration_sha.is_some()
            || state.current_integration_payload.is_some()
        {
            return Err(CoordinatorError::operational(
                "MODEL_RESPONSE_RETRY_UNSAFE",
                "model-response retry is limited to pre-integration read-only turns",
            ));
        }
        let run_id = state.facts.run_id.to_string();
        let pending = self.store.pending_send(&run_id)?.ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "invalid model response has no persisted pending turn",
            )
        })?;
        let (Some(thread_id), Some(turn_id)) =
            (pending.thread_id.as_deref(), pending.turn_id.as_deref())
        else {
            return Err(CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "invalid model response has no exact persisted turn identity",
            ));
        };
        let role = action_role(action).ok_or_else(|| {
            CoordinatorError::operational(
                "INCOMPATIBLE_STATE",
                "invalid model-response diagnostic has no task role",
            )
        })?;
        let expected_phase = preintegration_read_only_phase(action).ok_or_else(|| {
            CoordinatorError::operational(
                "INCOMPATIBLE_STATE",
                "invalid model-response diagnostic is not a pre-integration read-only action",
            )
        })?;
        let expected_thread_id = role_thread_id(state, role);
        if pending.role != role_name(role)
            || pending.phase != phase_name(expected_phase)
            || pending.round != state.round
            || thread_id != expected_thread_id
        {
            return Err(CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "invalid model response does not match the deterministic pending action",
            ));
        }

        let detail = self.read_thread_with_retry(thread_id).await?;
        self.verify_thread_identity(state, role, &detail)?;
        let turn = find_turn(&detail, turn_id).ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "persisted invalid model-response turn is absent from canonical task history",
            )
        })?;
        let status = turn.get("status").and_then(Value::as_str).ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "persisted invalid model-response turn has no canonical status",
            )
        })?;
        if status != "completed" {
            return Err(CoordinatorError::operational(
                "MODEL_RESPONSE_RETRY_UNSAFE",
                format!("invalid model-response turn has unexpected status {status}"),
            ));
        }
        if !turn_contains_request_hash(turn, &pending.message_hash) {
            return Err(CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "persisted invalid model-response turn lacks its deterministic request marker",
            ));
        }
        if let Some(blocker) = completed_read_only_turn_retry_blocker(turn) {
            return Err(CoordinatorError::operational(
                "MODEL_RESPONSE_RETRY_UNSAFE",
                format!("completed pre-integration turn {turn_id} cannot be retried: {blocker}"),
            ));
        }
        Ok(RetryableCompletedTurn {
            message_hash: pending.message_hash,
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
            observed_status: status.to_owned(),
        })
    }

    async fn inspect_completed_verification_without_execution_retry(
        &self,
        state: &RunState,
        action: NextAction,
    ) -> Result<RetryableVerificationTurn, CoordinatorError> {
        if action != NextAction::RequestPrimaryVerification
            || state.integration_branch.is_none()
            || state.integration_sha.is_none()
            || state.current_integration_payload.is_none()
            || state.verification_worktree.is_none()
            || state.required_test_commands.is_empty()
            || !state.test_evidence.is_empty()
            || state.accepted_result.is_some()
        {
            return Err(CoordinatorError::operational(
                "MODEL_RESPONSE_RETRY_UNSAFE",
                "verification retry requires an unchanged unaccepted integration result",
            ));
        }
        let run_id = state.facts.run_id.to_string();
        let pending = self.store.pending_send(&run_id)?.ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "verification failure has no persisted pending turn",
            )
        })?;
        let (Some(thread_id), Some(turn_id)) =
            (pending.thread_id.as_deref(), pending.turn_id.as_deref())
        else {
            return Err(CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "verification failure has no exact persisted turn identity",
            ));
        };
        if pending.role != "PRIMARY"
            || pending.phase != "VERIFY"
            || pending.round != state.round
            || thread_id != state.facts.primary_thread_id
        {
            return Err(CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "verification failure does not match the bound primary request",
            ));
        }
        let detail = self.read_thread_with_retry(thread_id).await?;
        self.verify_thread_identity(state, Role::Primary, &detail)?;
        let archived_attempts = self
            .store
            .archived_turn_attempts(&run_id, &pending.message_hash)?;
        if archived_attempts.len() == 1 {
            return Err(CoordinatorError::operational(
                "MODEL_RESPONSE_RETRY_UNSAFE",
                "verification-without-execution recovery is limited to one retry",
            ));
        }
        let mut archived_ready = false;
        let mut archived_cargo_unavailable = false;
        let mut archived_sequence = Vec::with_capacity(archived_attempts.len());
        for archived_attempt in &archived_attempts {
            let archived_turn_id = &archived_attempt.turn_id;
            let archived = find_turn(&detail, archived_turn_id).ok_or_else(|| {
                CoordinatorError::operational(
                    "HISTORY_UNAVAILABLE",
                    format!("archived verification turn {archived_turn_id} is absent"),
                )
            })?;
            if !turn_contains_request_hash(archived, &pending.message_hash) {
                return Err(CoordinatorError::operational(
                    "HISTORY_UNAVAILABLE",
                    format!(
                        "archived verification turn {archived_turn_id} lacks its deterministic request marker"
                    ),
                ));
            }
            if let Some(blocker) = terminal_turn_retry_blocker(archived) {
                return Err(CoordinatorError::operational(
                    "MODEL_RESPONSE_RETRY_UNSAFE",
                    format!(
                        "archived verification attempt {archived_turn_id} cannot precede an evidence retry: {blocker}"
                    ),
                ));
            }
            let response = parse_participant_response(
                final_agent_text(archived)?.trim(),
                allowed_participant_signals(NextAction::RequestPrimaryVerification),
            )
            .map_err(|error| {
                CoordinatorError::operational("HISTORY_UNAVAILABLE", error.to_string())
            })?;
            match response.signal {
                ParticipantSignal::VerificationReady => {
                    archived_ready = true;
                    archived_sequence.push(("ready", archived_attempt.terminal_status.as_str()));
                }
                ParticipantSignal::Blocked
                    if response.blocked_reason.as_deref() == Some("CARGO_UNAVAILABLE") =>
                {
                    archived_cargo_unavailable = true;
                    archived_sequence.push((
                        "cargo-unavailable",
                        archived_attempt.terminal_status.as_str(),
                    ));
                }
                _ => {
                    return Err(CoordinatorError::operational(
                        "MODEL_RESPONSE_RETRY_UNSAFE",
                        "archived verification history is outside the bounded evidence-recovery sequence",
                    ));
                }
            }
        }
        let kind = if archived_attempts.is_empty() {
            VerificationRetryKind::EmptyTurn
        } else if archived_sequence
            == [
                ("ready", "completed"),
                ("cargo-unavailable", "completed"),
                ("ready", "completed-evidence-unavailable"),
            ]
        {
            VerificationRetryKind::UnattendedVerificationMigration
        } else if archived_ready
            && archived_cargo_unavailable
            && archived_sequence
                .iter()
                .all(|(_, terminal_status)| *terminal_status == "completed")
        {
            VerificationRetryKind::EventEvidenceCompatibility
        } else {
            return Err(CoordinatorError::operational(
                "MODEL_RESPONSE_RETRY_UNSAFE",
                "verification evidence compatibility recovery requires exact archived verification history and is limited to one retry",
            ));
        };
        let turn = find_turn(&detail, turn_id).ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "persisted verification turn is absent from canonical task history",
            )
        })?;
        let status = turn.get("status").and_then(Value::as_str).ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "persisted verification turn has no canonical status",
            )
        })?;
        if status != "completed" || !turn_contains_request_hash(turn, &pending.message_hash) {
            return Err(CoordinatorError::operational(
                "MODEL_RESPONSE_RETRY_UNSAFE",
                "verification recovery requires the exact completed request turn",
            ));
        }
        if let Some(blocker) = verification_without_execution_retry_blocker(turn) {
            return Err(CoordinatorError::operational(
                "MODEL_RESPONSE_RETRY_UNSAFE",
                blocker,
            ));
        }
        let final_response = parse_participant_response(
            final_agent_text(turn)?.trim(),
            allowed_participant_signals(NextAction::RequestPrimaryVerification),
        )
        .map_err(|error| CoordinatorError::operational("HISTORY_UNAVAILABLE", error.to_string()))?;
        if final_response.signal != ParticipantSignal::VerificationReady {
            return Err(CoordinatorError::operational(
                "MODEL_RESPONSE_RETRY_UNSAFE",
                "verification migration requires the final VERIFICATION_READY marker",
            ));
        }

        Ok(RetryableVerificationTurn {
            turn: RetryableCompletedTurn {
                message_hash: pending.message_hash,
                thread_id: thread_id.to_owned(),
                turn_id: turn_id.to_owned(),
                observed_status: status.to_owned(),
            },
            kind,
        })
    }

    async fn inspect_completed_verification_environment_unavailable_retry(
        &self,
        state: &RunState,
        action: NextAction,
    ) -> Result<RetryableAcceptedVerificationEnvironmentTurn, CoordinatorError> {
        if action != NextAction::RequestPrimaryVerification
            || state.reason_code.as_deref() != Some("CARGO_UNAVAILABLE")
            || state.integration_branch.is_none()
            || state.integration_sha.is_none()
            || state.current_integration_payload.is_none()
            || state.verification_worktree.is_none()
            || state.required_test_commands.is_empty()
            || !state.test_evidence.is_empty()
            || state.accepted_result.is_some()
        {
            return Err(CoordinatorError::operational(
                "MODEL_RESPONSE_RETRY_UNSAFE",
                "verification environment retry requires an unchanged CARGO_UNAVAILABLE result",
            ));
        }
        let run_id = state.facts.run_id.to_string();
        let accepted = self.store.latest_accepted_turn(&run_id)?.ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "verification environment blocker has no accepted turn record",
            )
        })?;
        if accepted.role != "PRIMARY"
            || accepted.phase != "VERIFY"
            || accepted.round != state.round
            || accepted.thread_id != state.facts.primary_thread_id
        {
            return Err(CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "verification environment blocker does not match the frozen primary request",
            ));
        }

        let detail = self.read_thread_with_retry(&accepted.thread_id).await?;
        self.verify_thread_identity(state, Role::Primary, &detail)?;
        for archived_turn_id in self
            .store
            .archived_turn_ids(&run_id, &accepted.message_hash)?
        {
            let archived = find_turn(&detail, &archived_turn_id).ok_or_else(|| {
                CoordinatorError::operational(
                    "HISTORY_UNAVAILABLE",
                    format!("archived verification turn {archived_turn_id} is absent"),
                )
            })?;
            let archived_response = parse_participant_response(
                final_agent_text(archived)?.trim(),
                allowed_participant_signals(NextAction::RequestPrimaryVerification),
            )
            .map_err(|error| {
                CoordinatorError::operational("HISTORY_UNAVAILABLE", error.to_string())
            })?;
            if archived_response.signal == ParticipantSignal::Blocked
                && archived_response.blocked_reason.as_deref() == Some("CARGO_UNAVAILABLE")
            {
                return Err(CoordinatorError::operational(
                    "MODEL_RESPONSE_RETRY_UNSAFE",
                    "verification CARGO_UNAVAILABLE recovery is limited to one retry",
                ));
            }
        }

        let turn = find_turn(&detail, &accepted.turn_id).ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "accepted verification environment blocker is absent from canonical task history",
            )
        })?;
        let status = turn.get("status").and_then(Value::as_str).ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "verification environment blocker has no canonical status",
            )
        })?;
        if status != "completed" || !turn_contains_request_hash(turn, &accepted.message_hash) {
            return Err(CoordinatorError::operational(
                "MODEL_RESPONSE_RETRY_UNSAFE",
                "verification environment recovery requires the exact completed request turn",
            ));
        }
        if let Some(blocker) = terminal_turn_retry_blocker(turn) {
            return Err(CoordinatorError::operational(
                "MODEL_RESPONSE_RETRY_UNSAFE",
                format!("verification environment blocker cannot be retried: {blocker}"),
            ));
        }

        let parsed = parse_participant_response(
            final_agent_text(turn)?.trim(),
            allowed_participant_signals(NextAction::RequestPrimaryVerification),
        )
        .map_err(|error| CoordinatorError::operational("INVALID_RESPONSE", error.to_string()))?;
        if parsed.signal != ParticipantSignal::Blocked
            || parsed.blocked_reason.as_deref() != Some("CARGO_UNAVAILABLE")
        {
            return Err(CoordinatorError::operational(
                "MODEL_RESPONSE_RETRY_UNSAFE",
                "accepted verification blocker is not exactly CARGO_UNAVAILABLE",
            ));
        }
        let mut response_state = state.clone();
        response_state.retry_blocked_verification_environment_unavailable()?;
        let normalized = self.normalized_marker_message(
            &response_state,
            NextAction::RequestPrimaryVerification,
            parsed,
        )?;
        let normalized = serde_json::to_value(normalized).map_err(|error| {
            CoordinatorError::operational("SERIALIZATION_FAILURE", error.to_string())
        })?;
        if canonical_json_hash(&normalized) != accepted.response_hash {
            return Err(CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "verification environment response hash does not match canonical task history",
            ));
        }

        Ok(RetryableAcceptedVerificationEnvironmentTurn {
            accepted,
            observed_status: status.to_owned(),
        })
    }

    async fn inspect_interrupted_forbidden_operation_retry(
        &self,
        state: &RunState,
        action: NextAction,
    ) -> Result<RetryableTerminalTurn, CoordinatorError> {
        if action != NextAction::RequestPrimaryIntegration
            || state.integration_branch.is_some()
            || state.integration_sha.is_some()
            || state.current_integration_payload.is_some()
            || state.verification_worktree.is_some()
            || !state.test_evidence.is_empty()
        {
            return Err(CoordinatorError::operational(
                "TERMINAL_TURN_RETRY_UNSAFE",
                "forbidden-operation recovery is limited to the first integration turn",
            ));
        }
        let run_id = state.facts.run_id.to_string();
        let pending = self.store.pending_send(&run_id)?.ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "forbidden-operation blocker has no persisted pending turn",
            )
        })?;
        let (Some(thread_id), Some(turn_id)) =
            (pending.thread_id.as_deref(), pending.turn_id.as_deref())
        else {
            return Err(CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "forbidden-operation blocker has no exact persisted turn identity",
            ));
        };
        if pending.role != "PRIMARY"
            || pending.phase != "INTEGRATE"
            || pending.round != state.round
            || thread_id != state.facts.primary_thread_id
        {
            return Err(CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "forbidden-operation blocker does not match the frozen integration action",
            ));
        }

        let detail = self.read_thread_with_retry(thread_id).await?;
        self.verify_thread_identity(state, Role::Primary, &detail)?;
        let turn = find_turn(&detail, turn_id).ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "forbidden-operation turn is absent from canonical task history",
            )
        })?;
        let status = turn.get("status").and_then(Value::as_str).ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "forbidden-operation turn has no canonical status",
            )
        })?;
        if !matches!(status, "failed" | "interrupted") {
            return Err(CoordinatorError::operational(
                "TERMINAL_TURN_RETRY_UNSAFE",
                format!("forbidden-operation turn has unexpected status {status}"),
            ));
        }
        if !turn_contains_request_hash(turn, &pending.message_hash) {
            return Err(CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "forbidden-operation turn lacks its deterministic request marker",
            ));
        }
        if let Some(blocker) = interrupted_forbidden_operation_retry_blocker(state, turn) {
            return Err(CoordinatorError::operational(
                "TERMINAL_TURN_RETRY_UNSAFE",
                format!("forbidden-operation turn cannot be retried: {blocker}"),
            ));
        }
        Ok(RetryableTerminalTurn {
            message_hash: pending.message_hash,
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
            observed_status: status.to_owned(),
        })
    }

    async fn inspect_completed_execution_tool_unavailable_retry(
        &self,
        state: &RunState,
        action: NextAction,
    ) -> Result<RetryableAcceptedExecutionToolTurn, CoordinatorError> {
        if action != NextAction::RequestPrimaryIntegration
            || state.integration_branch.is_some()
            || state.integration_sha.is_some()
            || state.current_integration_payload.is_some()
        {
            return Err(CoordinatorError::operational(
                "MODEL_RESPONSE_RETRY_UNSAFE",
                "execution-tool recovery is limited to the first integration turn",
            ));
        }
        let run_id = state.facts.run_id.to_string();
        let accepted = self.store.latest_accepted_turn(&run_id)?.ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "execution-tool blocker has no accepted turn record",
            )
        })?;
        if accepted.role != "PRIMARY"
            || accepted.phase != "INTEGRATE"
            || accepted.round != state.round
            || accepted.thread_id != state.facts.primary_thread_id
        {
            return Err(CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "accepted execution-tool blocker does not match the frozen integration action",
            ));
        }

        let detail = self.read_thread_with_retry(&accepted.thread_id).await?;
        self.verify_thread_identity(state, Role::Primary, &detail)?;
        let turn = find_turn(&detail, &accepted.turn_id).ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "accepted execution-tool blocker is absent from canonical task history",
            )
        })?;
        let status = turn.get("status").and_then(Value::as_str).ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "accepted execution-tool blocker has no canonical status",
            )
        })?;
        if status != "completed" {
            return Err(CoordinatorError::operational(
                "MODEL_RESPONSE_RETRY_UNSAFE",
                format!("execution-tool blocker has unexpected status {status}"),
            ));
        }
        if !turn_contains_request_hash(turn, &accepted.message_hash) {
            return Err(CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "accepted execution-tool blocker lacks its deterministic request marker",
            ));
        }
        if let Some(blocker) = terminal_turn_retry_blocker(turn) {
            return Err(CoordinatorError::operational(
                "MODEL_RESPONSE_RETRY_UNSAFE",
                format!("accepted execution-tool blocker cannot be retried: {blocker}"),
            ));
        }

        let raw_response = final_agent_json(turn)?;
        if canonical_json_hash(&raw_response) != accepted.response_hash {
            return Err(CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "accepted execution-tool response hash does not match canonical task history",
            ));
        }
        let message = validate_message(raw_response).map_err(|error| {
            CoordinatorError::operational("INVALID_RESPONSE", error.to_string())
        })?;
        validate_execution_tool_unavailable_blocker(state, &accepted, &message)?;

        Ok(RetryableAcceptedExecutionToolTurn {
            accepted,
            observed_status: status.to_owned(),
        })
    }

    async fn drive_model_action(
        &self,
        state: &mut RunState,
        action: NextAction,
    ) -> Result<(), CoordinatorError> {
        if action == NextAction::RequestPrimaryVerification {
            self.prepare_verification_workspace(state)?;
        }
        let role = action_role(action).ok_or_else(|| {
            CoordinatorError::operational("INVALID_STATE", "action has no task role")
        })?;
        let thread_id = role_thread_id(state, role).to_owned();
        let detail = self.wait_until_idle(state, role, &thread_id).await?;
        self.verify_thread_identity(state, role, &detail)?;
        let resume_policy = if action == NextAction::RequestPrimaryIntegration {
            ThreadResumePolicy::PrimaryIntegration {
                participant_executable: self.options.participant_mcp_executable.clone(),
            }
        } else {
            ThreadResumePolicy::Default
        };
        let resumed = self
            .resume_thread_with_retry(&thread_id, &resume_policy)
            .await?;
        self.verify_thread_identity(state, role, &resumed)?;
        if action == NextAction::RequestPrimaryIntegration {
            let statuses = self
                .list_mcp_server_status_for_preflight(&thread_id)
                .await?;
            verify_participant_patch_capability(&thread_id, &statuses)?;
        }
        if resumed.summary.is_active() {
            self.wait_until_idle(state, role, &thread_id).await?;
        }

        self.load_history(state).await?;
        self.revalidate_before_action(state, action)?;
        let payload = build_action_payload(state, action)?;
        let request_hash = canonical_json_hash(&json!({
            "run_id": state.facts.run_id,
            "action": action,
            "phase": state.phase,
            "round": state.round,
            "plan_revision": state.plan_revision,
            "integration_sha": state.integration_sha,
            "payload": payload,
        }));
        let run_id = state.facts.run_id.to_string();
        let mut prompt = build_turn_prompt(role, action, state, &payload)?;
        if action == NextAction::RequestPrimaryIntegration
            && self
                .store
                .successful_patch_recorded(&run_id, &request_hash)?
        {
            prompt.push_str(
                "\nCoordinator recovery override:\n- The exact request-bound controlled patch and final commit already succeeded in an archived attempt.\n- Do not call consensus_apply_patch and do not edit, stage, commit, create, or merge anything.\n- Use only read-only Git queries in the authorized primary worktree to inspect the existing clean target branch.\n- Return `<consensus-result>INTEGRATION_READY</consensus-result>` plus optional free-form Markdown. The coordinator derives branch, SHA, and changed files directly from Git.\n",
            );
        }
        prompt.push_str("\nCoordinator delivery identity for crash recovery:\n```json\n");
        prompt.push_str(
            &serde_json::to_string(&json!({"request_hash": request_hash})).map_err(|error| {
                CoordinatorError::operational("SERIALIZATION_FAILURE", error.to_string())
            })?,
        );
        prompt.push_str("\n```\n");

        let mut pending = self.store.pending_send(&run_id)?;
        if let Some(existing) = &pending {
            if existing.message_hash != request_hash {
                return Err(CoordinatorError::operational(
                    "HISTORY_UNAVAILABLE",
                    "pending turn does not match the deterministic current request",
                ));
            }
        } else {
            self.store.record_pending_send(
                &run_id,
                role_name(role),
                phase_name(state.phase),
                state.round,
                &request_hash,
            )?;
            pending = self.store.pending_send(&run_id)?;
        }
        let pending = pending.ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "pending turn could not be reloaded",
            )
        })?;

        let current_detail = self.read_thread_with_retry(&thread_id).await?;
        let archived_turn_ids = self.store.archived_turn_ids(&run_id, &request_hash)?;
        let recovered_turn = pending.turn_id.clone().or_else(|| {
            find_turn_by_request_hash(&current_detail, &request_hash, &archived_turn_ids)
        });
        let turn_id = if let Some(turn_id) = recovered_turn {
            self.store
                .record_turn_started(&run_id, &request_hash, &thread_id, &turn_id)?;
            turn_id
        } else {
            if current_detail.summary.is_active() {
                return Err(CoordinatorError::operational(
                    "HISTORY_UNAVAILABLE",
                    "task became active after pending-send without a recoverable request marker",
                ));
            }
            let turn = self
                .app
                .start_turn(
                    &thread_id,
                    &prompt,
                    &turn_execution_policy(state, action, role),
                )
                .await
                .map_err(|error| communication_error("turn/start", Some(&thread_id), error))?;
            self.store
                .record_turn_started(&run_id, &request_hash, &thread_id, &turn.id)?;
            turn.id
        };

        let completed = self
            .wait_for_turn_response(state, &thread_id, &turn_id)
            .await?;
        if action == NextAction::RequestPrimaryVerification {
            verify_marker_only_verification_turn(&completed.turn)?;
        }
        let mut message = self.normalize_model_response(state, action, &completed.response)?;
        let authoritative_verification = if action == NextAction::RequestPrimaryVerification
            && message.envelope.message_type == MessageType::IntegrationReady
        {
            Some(
                self.execute_frozen_verification(state, &request_hash, &turn_id)
                    .await?,
            )
        } else {
            None
        };
        self.verify_message_evidence(
            state,
            action,
            &mut message,
            &completed.turn,
            authoritative_verification,
        )?;
        let response = serde_json::to_value(&message).map_err(|error| {
            CoordinatorError::operational("SERIALIZATION_FAILURE", error.to_string())
        })?;
        let mut next = state.clone();
        next.apply_message(message)?;
        let response_hash = canonical_json_hash(&response);
        self.store
            .accept_response_and_advance(&run_id, &response_hash, &next)?;
        *state = next;
        Ok(())
    }

    async fn execute_frozen_verification(
        &self,
        state: &RunState,
        request_hash: &str,
        turn_id: &str,
    ) -> Result<AuthoritativeVerification, CoordinatorError> {
        let run_id = state.facts.run_id.to_string();
        let verification_cwd = state.verification_worktree.as_ref().ok_or_else(|| {
            CoordinatorError::operational(
                "INVALID_STATE",
                "verification workspace is not persisted",
            )
        })?;
        let timeout_ms = u64::try_from(self.options.wait_timeout.as_millis()).map_err(|_| {
            CoordinatorError::operational("INVALID_STATE", "verification timeout exceeds u64")
        })?;
        let mut evidence = Vec::with_capacity(state.required_test_commands.len());
        let mut failures = Vec::new();

        for (index, command) in state.required_test_commands.iter().enumerate() {
            if !validate_test_command(command) {
                return Err(CoordinatorError::operational(
                    "INVALID_TEST_COMMAND",
                    format!("frozen test command violates the execution policy: {command}"),
                ));
            }
            let argv = shell_words::split(command).map_err(|_| {
                CoordinatorError::operational(
                    "INVALID_TEST_COMMAND",
                    "frozen test command is not parseable",
                )
            })?;
            if argv.is_empty() {
                return Err(CoordinatorError::operational(
                    "INVALID_TEST_COMMAND",
                    "frozen test command has no executable",
                ));
            }
            let command_index = u32::try_from(index).map_err(|_| {
                CoordinatorError::operational(
                    "INVALID_STATE",
                    "verification command index exceeds u32",
                )
            })?;
            let claim = self.store.begin_verification_command(
                &run_id,
                request_hash,
                turn_id,
                command_index,
                command,
                verification_cwd,
            )?;
            let record = match claim {
                VerificationCommandClaim::Reuse(record) => record,
                VerificationCommandClaim::Execute(_) => {
                    let result = self
                        .app
                        .execute_command(&CommandExecRequest {
                            command: argv,
                            cwd: verification_cwd.to_owned(),
                            timeout_ms,
                            output_bytes_cap: VERIFICATION_COMMAND_OUTPUT_CAP_BYTES,
                        })
                        .await
                        .map_err(|error| communication_error("command/exec", None, error))?;
                    self.store.complete_verification_command(
                        &run_id,
                        request_hash,
                        command_index,
                        result.exit_code,
                        &result.stdout,
                        &result.stderr,
                    )?
                }
            };
            append_verification_record(&record, &mut evidence, &mut failures)?;
        }

        Ok(AuthoritativeVerification { evidence, failures })
    }

    async fn revalidate_and_accept(&self, state: &mut RunState) -> Result<(), CoordinatorError> {
        self.load_history(state).await?;
        let changed_files = changed_files(current_integration_payload(state)?)?;
        let branch = state.integration_branch.as_deref().ok_or_else(|| {
            CoordinatorError::operational("INVALID_STATE", "integration branch is missing")
        })?;
        let sha = state.integration_sha.as_deref().ok_or_else(|| {
            CoordinatorError::operational("INVALID_STATE", "integration SHA is missing")
        })?;
        self.safety
            .verify_integration(&state.facts, branch, sha, &changed_files)?;
        state.accept_after_revalidation()?;
        self.store.save_state(state)?;
        Ok(())
    }

    async fn revalidate_current_repository(
        &self,
        state: &RunState,
    ) -> Result<(), CoordinatorError> {
        if let (Some(branch), Some(sha)) = (
            state.integration_branch.as_deref(),
            state.integration_sha.as_deref(),
        ) {
            self.load_history(state).await?;
            self.safety.verify_integration(
                &state.facts,
                branch,
                sha,
                &changed_files(current_integration_payload(state)?)?,
            )?;
        } else {
            self.safety.verify_frozen(&state.facts)?;
        }
        Ok(())
    }

    fn revalidate_before_action(
        &self,
        state: &RunState,
        action: NextAction,
    ) -> Result<(), CoordinatorError> {
        if action == NextAction::RequestPrimaryIntegration {
            let branch = state.target_integration_branch.as_deref().ok_or_else(|| {
                CoordinatorError::operational(
                    "INVALID_STATE",
                    "target integration branch is missing",
                )
            })?;
            let run_id = state.facts.run_id.to_string();
            if self.store.pending_send(&run_id)?.is_some() {
                self.safety
                    .verify_integration_in_progress(&state.facts, branch)?;
                return Ok(());
            }
        }
        if let (Some(branch), Some(sha)) = (
            state.integration_branch.as_deref(),
            state.integration_sha.as_deref(),
        ) {
            self.safety.verify_integration(
                &state.facts,
                branch,
                sha,
                &changed_files(current_integration_payload(state)?)?,
            )?;
        } else {
            if action == NextAction::RequestPrimaryIntegration {
                let branch = state.target_integration_branch.as_deref().ok_or_else(|| {
                    CoordinatorError::operational(
                        "INVALID_STATE",
                        "target integration branch is missing",
                    )
                })?;
                self.safety.verify_frozen(&state.facts)?;
                self.safety.verify_branch_absent(&state.facts, branch)?;
            } else {
                self.safety.verify_frozen(&state.facts)?;
            }
        }
        Ok(())
    }

    fn prepare_verification_workspace(&self, state: &mut RunState) -> Result<(), CoordinatorError> {
        let integration_sha = state.integration_sha.as_deref().ok_or_else(|| {
            CoordinatorError::operational(
                "INVALID_STATE",
                "verification requires an integration SHA",
            )
        })?;
        let destination = self
            .store
            .verification_path(&state.facts.run_id.to_string(), integration_sha);
        if state
            .verification_worktree
            .as_ref()
            .is_some_and(|existing| existing != &destination)
        {
            return Err(CoordinatorError::operational(
                "INVALID_STATE",
                "persisted verification workspace does not match the exact integration SHA",
            ));
        }
        let pending_turn = self
            .store
            .pending_send(&state.facts.run_id.to_string())?
            .is_some();
        let prepared = if pending_turn && state.verification_worktree.as_ref() == Some(&destination)
        {
            self.safety.recover_verification_workspace(
                &state.facts,
                integration_sha,
                &destination,
            )?
        } else {
            self.safety.prepare_verification_workspace(
                &state.facts,
                integration_sha,
                &destination,
            )?
        };
        if prepared != destination {
            return Err(CoordinatorError::operational(
                "UNSAFE_VERIFICATION_WORKSPACE",
                "verification workspace provider returned a different path",
            ));
        }
        state.verification_worktree = Some(prepared);
        self.store.save_state(state)?;
        Ok(())
    }

    fn normalize_model_response(
        &self,
        state: &RunState,
        action: NextAction,
        response_text: &str,
    ) -> Result<ProtocolMessage, CoordinatorError> {
        let trimmed = response_text.trim();
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            if value.get("protocol").and_then(Value::as_str) == Some("worktree-merge-consensus/v1")
            {
                return validate_message(value).map_err(|error| {
                    CoordinatorError::operational("INVALID_RESPONSE", error.to_string())
                });
            }
        }

        let response = parse_participant_response(trimmed, allowed_participant_signals(action))
            .map_err(|error| {
                CoordinatorError::operational("INVALID_RESPONSE", error.to_string())
            })?;
        self.normalized_marker_message(state, action, response)
    }

    fn normalized_marker_message(
        &self,
        state: &RunState,
        action: NextAction,
        response: ParticipantResponse,
    ) -> Result<ProtocolMessage, CoordinatorError> {
        if response.signal == ParticipantSignal::Blocked {
            return Ok(ProtocolMessage {
                envelope: authoritative_envelope(
                    state,
                    MessageType::Blocked,
                    Some(
                        response
                            .blocked_reason
                            .unwrap_or_else(|| "PARTICIPANT_BLOCKED".to_owned()),
                    ),
                    state.integration_branch.clone(),
                    state.integration_sha.clone(),
                ),
                payload: json!({
                    "format": "markdown",
                    "feedback": response.body,
                }),
            });
        }

        let message = match (action, response.signal) {
            (
                NextAction::RequestPrimaryContract | NextAction::RequestReviewerContract,
                ParticipantSignal::ContractReady,
            ) => {
                let contract = parse_contract_json(&response.body)?;
                let role = if action == NextAction::RequestPrimaryContract {
                    "PRIMARY"
                } else {
                    "REVIEWER"
                };
                ProtocolMessage {
                    envelope: authoritative_envelope(
                        state,
                        MessageType::ContractReady,
                        None,
                        None,
                        None,
                    ),
                    payload: json!({"role": role, "contract": contract}),
                }
            }
            (NextAction::RequestPrimaryPlan, ParticipantSignal::PlanReady) => {
                require_free_markdown(&response.body, "PLAN_READY requires a complete plan")?;
                let primary_contract = state.primary_contract.clone().ok_or_else(|| {
                    CoordinatorError::operational(
                        "HISTORY_UNAVAILABLE",
                        "primary contract is unavailable while normalizing the plan",
                    )
                })?;
                let reviewer_contract = state.reviewer_contract.clone().ok_or_else(|| {
                    CoordinatorError::operational(
                        "HISTORY_UNAVAILABLE",
                        "reviewer contract is unavailable while normalizing the plan",
                    )
                })?;
                ProtocolMessage {
                    envelope: authoritative_envelope(
                        state,
                        MessageType::PlanReady,
                        None,
                        None,
                        None,
                    ),
                    payload: json!({
                        "primary_contract": primary_contract,
                        "reviewer_contract": reviewer_contract,
                        "plan": {
                            "format": "markdown",
                            "content": response.body,
                        },
                        "coverage_matrix": [],
                        "test_commands": state.required_test_commands,
                    }),
                }
            }
            (NextAction::RequestReviewerPlanVerdict, ParticipantSignal::Approved) => {
                let plan_hash = state
                    .current_plan_payload
                    .as_ref()
                    .map(canonical_json_hash)
                    .ok_or_else(|| {
                        CoordinatorError::operational(
                            "HISTORY_UNAVAILABLE",
                            "current plan hash is unavailable for approval",
                        )
                    })?;
                ProtocolMessage {
                    envelope: authoritative_envelope(
                        state,
                        MessageType::ApprovedPlan,
                        None,
                        None,
                        None,
                    ),
                    payload: json!({
                        "approved_plan_revision": state.plan_revision,
                        "approved_primary_sha": state.facts.primary_sha,
                        "approved_reviewer_sha": state.facts.reviewer_sha,
                        "approved_plan_hash": plan_hash,
                        "uncovered_items": [],
                        "review_markdown": response.body,
                    }),
                }
            }
            (NextAction::RequestReviewerPlanVerdict, ParticipantSignal::ChangesRequired) => {
                changes_required_message(state, response.body, false)?
            }
            (NextAction::RequestPrimaryIntegration, ParticipantSignal::IntegrationReady) => {
                let branch = state.target_integration_branch.clone().ok_or_else(|| {
                    CoordinatorError::operational(
                        "INVALID_STATE",
                        "authorized integration branch is unavailable",
                    )
                })?;
                let (sha, changed_files) = self
                    .safety
                    .authoritative_integration_result(&state.facts, &branch)?;
                ProtocolMessage {
                    envelope: authoritative_envelope(
                        state,
                        MessageType::IntegrationReady,
                        None,
                        Some(branch),
                        Some(sha),
                    ),
                    payload: json!({
                        "changed_files": changed_files,
                        "integration_evidence": {
                            "format": "markdown",
                            "summary": response.body,
                        },
                    }),
                }
            }
            (NextAction::RequestPrimaryVerification, ParticipantSignal::VerificationReady) => {
                let mut payload = current_integration_payload(state)?.clone();
                payload
                    .as_object_mut()
                    .ok_or_else(|| {
                        CoordinatorError::operational(
                            "INVALID_STATE",
                            "canonical integration payload is not an object",
                        )
                    })?
                    .insert("verification_summary".into(), json!(response.body));
                ProtocolMessage {
                    envelope: authoritative_envelope(
                        state,
                        MessageType::IntegrationReady,
                        None,
                        state.integration_branch.clone(),
                        state.integration_sha.clone(),
                    ),
                    payload,
                }
            }
            (NextAction::RequestReviewerResultVerdict, ParticipantSignal::Approved) => {
                let branch = state.integration_branch.clone().ok_or_else(|| {
                    CoordinatorError::operational(
                        "INVALID_STATE",
                        "integration branch is unavailable for result approval",
                    )
                })?;
                let sha = state.integration_sha.clone().ok_or_else(|| {
                    CoordinatorError::operational(
                        "INVALID_STATE",
                        "integration SHA is unavailable for result approval",
                    )
                })?;
                ProtocolMessage {
                    envelope: authoritative_envelope(
                        state,
                        MessageType::ApprovedResult,
                        None,
                        Some(branch.clone()),
                        Some(sha.clone()),
                    ),
                    payload: json!({
                        "approved_plan_revision": state.plan_revision,
                        "approved_primary_sha": state.facts.primary_sha,
                        "approved_reviewer_sha": state.facts.reviewer_sha,
                        "approved_integration_branch": branch,
                        "approved_integration_sha": sha,
                        "uncovered_items": [],
                        "review_markdown": response.body,
                    }),
                }
            }
            (NextAction::RequestReviewerResultVerdict, ParticipantSignal::ChangesRequired) => {
                changes_required_message(state, response.body, true)?
            }
            _ => {
                return Err(CoordinatorError::operational(
                    "INVALID_RESPONSE",
                    "participant result marker does not match the pending action",
                ));
            }
        };

        let value = serde_json::to_value(message).map_err(|error| {
            CoordinatorError::operational("SERIALIZATION_FAILURE", error.to_string())
        })?;
        validate_message(value)
            .map_err(|error| CoordinatorError::operational("INVALID_RESPONSE", error.to_string()))
    }

    fn verify_message_evidence(
        &self,
        state: &RunState,
        action: NextAction,
        message: &mut ProtocolMessage,
        turn: &Value,
        authoritative_verification: Option<AuthoritativeVerification>,
    ) -> Result<(), CoordinatorError> {
        if matches!(
            message.envelope.message_type,
            MessageType::ContractReady | MessageType::PlanReady
        ) {
            for command in declared_test_commands(message)? {
                if !validate_test_command(&command) {
                    return Err(CoordinatorError::operational(
                        "INVALID_TEST_COMMAND",
                        format!(
                            "model-declared test command violates the execution policy: {command}"
                        ),
                    ));
                }
            }
            return Ok(());
        }
        if message.envelope.message_type != MessageType::IntegrationReady {
            return Ok(());
        }
        match action {
            NextAction::RequestPrimaryIntegration => {
                let pending = self
                    .store
                    .pending_send(&state.facts.run_id.to_string())?
                    .ok_or_else(|| {
                        CoordinatorError::operational(
                            "HISTORY_UNAVAILABLE",
                            "integration response has no exact pending request identity",
                        )
                    })?;
                let run_id = state.facts.run_id.to_string();
                let successful_patch_hash = self
                    .store
                    .successful_patch_hash(&run_id, &pending.message_hash)?;
                let has_archived_attempt = !self
                    .store
                    .archived_turn_ids(&run_id, &pending.message_hash)?
                    .is_empty();
                verify_integration_execution_items(
                    state,
                    turn,
                    &pending.message_hash,
                    successful_patch_hash.as_deref(),
                    has_archived_attempt,
                )?;
            }
            NextAction::RequestPrimaryVerification => {
                let authoritative = authoritative_verification.ok_or_else(|| {
                    CoordinatorError::operational(
                        "INVALID_STATE",
                        "coordinator verification evidence is absent",
                    )
                })?;
                let mut canonical = current_integration_payload(state)?.clone();
                let canonical = canonical.as_object_mut().ok_or_else(|| {
                    CoordinatorError::operational(
                        "INVALID_STATE",
                        "canonical integration payload is not an object",
                    )
                })?;
                canonical.insert(
                    "test_evidence".into(),
                    serde_json::to_value(authoritative.evidence).map_err(|error| {
                        CoordinatorError::operational("SERIALIZATION_FAILURE", error.to_string())
                    })?,
                );
                if authoritative.failures.is_empty() {
                    canonical.remove("verification_failures");
                } else {
                    canonical.insert(
                        "verification_failures".into(),
                        Value::Array(authoritative.failures),
                    );
                }
                message.payload = Value::Object(canonical.clone());
            }
            _ => {
                return Err(CoordinatorError::operational(
                    "INVALID_RESPONSE",
                    "INTEGRATION_READY arrived for a non-integration action",
                ));
            }
        }
        let branch = message
            .envelope
            .integration_branch
            .as_deref()
            .ok_or_else(|| {
                CoordinatorError::operational("INVALID_RESPONSE", "integration branch is missing")
            })?;
        let sha = message.envelope.integration_sha.as_deref().ok_or_else(|| {
            CoordinatorError::operational("INVALID_RESPONSE", "integration SHA is missing")
        })?;
        self.safety.verify_integration(
            &state.facts,
            branch,
            sha,
            &changed_files(&message.payload)?,
        )?;
        Ok(())
    }

    async fn wait_until_idle(
        &self,
        state: &mut RunState,
        role: Role,
        thread_id: &str,
    ) -> Result<ThreadDetail, CoordinatorError> {
        let mut idle_deadline = tokio::time::Instant::now() + self.options.wait_timeout;
        let mut last_progress = None;
        loop {
            let persisted = self.required_run(&state.facts.run_id.to_string())?;
            if persisted.status == RunStatus::Cancelled {
                *state = persisted;
                return Err(CoordinatorError::operational(
                    "CANCELLED",
                    "run was cancelled while waiting for a task to become idle",
                ));
            }
            let detail = self.read_thread_with_retry(thread_id).await?;
            self.verify_thread_identity(state, role, &detail)?;
            let progress = thread_progress_fingerprint(&detail);
            if last_progress.as_deref() != Some(progress.as_str()) {
                last_progress = Some(progress);
                idle_deadline = tokio::time::Instant::now() + self.options.wait_timeout;
            }
            if !detail.summary.is_active() {
                if state.status == RunStatus::WaitingThread {
                    state.thread_became_idle()?;
                    self.store.save_state(state)?;
                }
                return Ok(detail);
            }
            if state.status == RunStatus::Running {
                state.wait_for_thread()?;
                self.store.save_state(state)?;
            }
            if tokio::time::Instant::now() >= idle_deadline {
                return Err(CoordinatorError::operational(
                    "COMMUNICATION_FAILURE",
                    "task remained active without canonical progress beyond the bounded idle wait",
                ));
            }
            tokio::time::sleep(self.options.poll_interval).await;
        }
    }

    async fn wait_for_turn_response(
        &self,
        state: &mut RunState,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<CompletedTurn, CoordinatorError> {
        let mut idle_deadline = tokio::time::Instant::now() + self.options.wait_timeout;
        let mut last_progress = None;
        loop {
            let persisted = self.required_run(&state.facts.run_id.to_string())?;
            if persisted.status == RunStatus::Cancelled {
                *state = persisted;
                return Err(CoordinatorError::operational(
                    "CANCELLED",
                    "run was cancelled while its task turn was active",
                ));
            }
            let detail = self.read_thread_with_retry(thread_id).await?;
            if let Some(turn) = find_turn(&detail, turn_id) {
                let progress = canonical_json_hash(turn);
                if last_progress.as_deref() != Some(progress.as_str()) {
                    last_progress = Some(progress);
                    idle_deadline = tokio::time::Instant::now() + self.options.wait_timeout;
                }
                match turn.get("status").and_then(Value::as_str) {
                    Some("completed") => {
                        self.drain_completed_turn_events(state, thread_id, turn_id)
                            .await?;
                        let canonical_turn = self
                            .completed_turn_with_event_evidence(state, thread_id, turn_id, turn)?;
                        return Ok(CompletedTurn {
                            response: final_agent_text(&canonical_turn)?.to_owned(),
                            turn: canonical_turn,
                        });
                    }
                    Some("failed" | "interrupted") => {
                        return Err(CoordinatorError::operational(
                            "COMMUNICATION_FAILURE",
                            "task turn did not complete successfully",
                        ));
                    }
                    _ => {}
                }
            }
            if tokio::time::Instant::now() >= idle_deadline {
                return Err(CoordinatorError::operational(
                    "COMMUNICATION_FAILURE",
                    "task turn made no canonical progress within the bounded idle wait",
                ));
            }
            match tokio::time::timeout(self.options.poll_interval, self.app.next_event()).await {
                Ok(Some(event)) if event_matches_turn(&event, thread_id, turn_id) => {
                    self.consume_turn_event(state, thread_id, turn_id, &event)
                        .await?;
                    idle_deadline = tokio::time::Instant::now() + self.options.wait_timeout;
                }
                Ok(None) => tokio::time::sleep(self.options.poll_interval).await,
                _ => {}
            }
        }
    }

    async fn drain_completed_turn_events(
        &self,
        state: &mut RunState,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<(), CoordinatorError> {
        if self
            .store
            .turn_event_evidence(&state.facts.run_id.to_string(), thread_id, turn_id)?
            .is_some()
        {
            return Ok(());
        }
        let drain_timeout = self.options.poll_interval.min(Duration::from_millis(100));
        for _ in 0..256 {
            let event = match tokio::time::timeout(drain_timeout, self.app.next_event()).await {
                Ok(Some(event)) => event,
                _ => break,
            };
            let is_completion =
                event.method == "turn/completed" && event_matches_turn(&event, thread_id, turn_id);
            if event_matches_turn(&event, thread_id, turn_id) {
                self.consume_turn_event(state, thread_id, turn_id, &event)
                    .await?;
            }
            if is_completion {
                break;
            }
        }
        Ok(())
    }

    async fn consume_turn_event(
        &self,
        state: &mut RunState,
        thread_id: &str,
        turn_id: &str,
        event: &AppEvent,
    ) -> Result<(), CoordinatorError> {
        debug_assert!(event_matches_turn(event, thread_id, turn_id));
        let run_id = state.facts.run_id.to_string();
        match event.method.as_str() {
            "item/started" | "item/completed" => {
                let item = event.params.get("item").ok_or_else(|| {
                    CoordinatorError::operational(
                        "HISTORY_UNAVAILABLE",
                        format!("{} event omits its canonical item", event.method),
                    )
                })?;
                self.store.record_turn_item_event(
                    &run_id,
                    thread_id,
                    turn_id,
                    &event.method,
                    item,
                )?;
            }
            "turn/completed" => {
                let turn = event.params.get("turn").ok_or_else(|| {
                    CoordinatorError::operational(
                        "HISTORY_UNAVAILABLE",
                        "turn/completed event omits its canonical turn",
                    )
                })?;
                self.store
                    .record_turn_completed_event(&run_id, thread_id, turn_id, turn)?;
            }
            _ => {}
        }
        if self.handle_execution_request(state, event).await? {
            return Ok(());
        }
        if user_action_event(event, thread_id, turn_id) {
            state.pause("PERMISSION_REQUIRED")?;
            self.store.save_state(state)?;
            return Err(CoordinatorError::operational(
                "PERMISSION_REQUIRED",
                "task turn is waiting for user approval or input",
            ));
        }
        Ok(())
    }

    fn completed_turn_with_event_evidence(
        &self,
        state: &RunState,
        thread_id: &str,
        turn_id: &str,
        persisted_turn: &Value,
    ) -> Result<Value, CoordinatorError> {
        let Some(evidence) =
            self.store
                .turn_event_evidence(&state.facts.run_id.to_string(), thread_id, turn_id)?
        else {
            return Ok(persisted_turn.clone());
        };
        merge_completed_turn_evidence(
            persisted_turn,
            &evidence.completed_turn,
            evidence.completed_items,
        )
    }

    async fn handle_execution_request(
        &self,
        state: &RunState,
        event: &AppEvent,
    ) -> Result<bool, CoordinatorError> {
        let Some(id) = event.id.clone() else {
            return Ok(false);
        };
        match event.method.as_str() {
            "item/commandExecution/requestApproval" => {
                let decision = decide_command_approval(state, &event.params);
                let response = match decision {
                    ApprovalDecision::Accept => json!({"decision": "accept"}),
                    ApprovalDecision::Cancel => json!({"decision": "cancel"}),
                };
                self.app
                    .respond_to_request(id, response)
                    .await
                    .map_err(|error| communication_error("server/request-response", None, error))?;
                if decision == ApprovalDecision::Cancel {
                    let denial = command_approval_denial(state, &event.params)
                        .unwrap_or("command approval failed closed");
                    return Err(CoordinatorError::operational(
                        "FORBIDDEN_OPERATION",
                        format!(
                            "the task requested a command outside the frozen integration execution policy: {denial}"
                        ),
                    ));
                }
                Ok(true)
            }
            "item/fileChange/requestApproval" => {
                let requests_grant_root = event
                    .params
                    .get("grantRoot")
                    .is_some_and(|value| !value.is_null());
                let integration_write = state.next_action == NextAction::RequestPrimaryIntegration
                    && !requests_grant_root;
                self.app
                    .respond_to_request(
                        id,
                        json!({"decision": if integration_write { "accept" } else { "cancel" }}),
                    )
                    .await
                    .map_err(|error| communication_error("server/request-response", None, error))?;
                if !integration_write {
                    return Err(CoordinatorError::operational(
                        "FORBIDDEN_OPERATION",
                        "a task requested a file change outside the fixed integration write roots",
                    ));
                }
                Ok(true)
            }
            "item/permissions/requestApproval" => {
                self.app
                    .respond_to_request(id, json!({"permissions": {}, "scope": "turn"}))
                    .await
                    .map_err(|error| communication_error("server/request-response", None, error))?;
                Err(CoordinatorError::operational(
                    "FORBIDDEN_OPERATION",
                    "additional filesystem or network permissions are forbidden",
                ))
            }
            _ => Ok(false),
        }
    }

    async fn load_history(&self, state: &RunState) -> Result<(), CoordinatorError> {
        let primary = self
            .read_thread_with_retry(&state.facts.primary_thread_id)
            .await?;
        let reviewer = self
            .read_thread_with_retry(&state.facts.reviewer_thread_id)
            .await?;
        self.verify_thread_identity(state, Role::Primary, &primary)?;
        self.verify_thread_identity(state, Role::Reviewer, &reviewer)?;
        Ok(())
    }

    async fn read_thread_with_retry(
        &self,
        thread_id: &str,
    ) -> Result<ThreadDetail, CoordinatorError> {
        let attempts = self.options.communication_attempts.max(1);
        let mut last_error = None;
        for attempt in 0..attempts {
            match self.app.read_thread(thread_id).await {
                Ok(detail) => return Ok(detail),
                Err(error) => last_error = Some(error),
            }
            if attempt + 1 < attempts {
                tokio::time::sleep(self.options.poll_interval).await;
            }
        }
        Err(communication_error(
            "thread/read",
            Some(thread_id),
            last_error.unwrap_or_else(|| {
                AppServerError::InvalidResponse("thread read failed without an error".into())
            }),
        ))
    }

    async fn resume_thread_with_retry(
        &self,
        thread_id: &str,
        policy: &ThreadResumePolicy,
    ) -> Result<ThreadDetail, CoordinatorError> {
        let attempts = self.options.communication_attempts.max(1);
        let mut last_error = None;
        for attempt in 0..attempts {
            match self.app.resume_thread(thread_id, policy).await {
                Ok(detail) => return Ok(detail),
                Err(error) => last_error = Some(error),
            }
            if attempt + 1 < attempts {
                tokio::time::sleep(self.options.poll_interval).await;
            }
        }
        Err(communication_error(
            "thread/resume",
            Some(thread_id),
            last_error.unwrap_or_else(|| {
                AppServerError::InvalidResponse("thread resume failed without an error".into())
            }),
        ))
    }

    async fn list_mcp_server_status_for_preflight(
        &self,
        thread_id: &str,
    ) -> Result<Vec<McpServerStatus>, CoordinatorError> {
        let attempts = self.options.communication_attempts.max(1);
        let mut last_error = None;
        for attempt in 0..attempts {
            match self.app.list_mcp_server_status(thread_id).await {
                Ok(statuses) => return Ok(statuses),
                Err(AppServerError::IncompatibleCodex(detail)) => {
                    return Err(CoordinatorError::app_server(
                        "INCOMPATIBLE_CODEX",
                        detail,
                        "mcpServerStatus/list",
                        Some(thread_id),
                    ));
                }
                Err(AppServerError::InvalidResponse(detail)) => {
                    return Err(CoordinatorError::app_server(
                        "PATCH_TOOL_UNAVAILABLE",
                        detail,
                        "mcpServerStatus/list",
                        Some(thread_id),
                    ));
                }
                Err(error) => last_error = Some(error),
            }
            if attempt + 1 < attempts {
                tokio::time::sleep(self.options.poll_interval).await;
            }
        }
        Err(communication_error(
            "mcpServerStatus/list",
            Some(thread_id),
            last_error.unwrap_or_else(|| {
                AppServerError::InvalidResponse(
                    "MCP server status failed without an error".to_owned(),
                )
            }),
        ))
    }

    fn verify_thread_identity(
        &self,
        state: &RunState,
        role: Role,
        detail: &ThreadDetail,
    ) -> Result<(), CoordinatorError> {
        let expected_id = role_thread_id(state, role);
        if detail.summary.id != expected_id {
            return Err(CoordinatorError::operational(
                "AMBIGUOUS_THREAD",
                "App Server returned a different task than requested",
            ));
        }
        Ok(())
    }

    fn required_run(&self, run_id: &str) -> Result<RunState, CoordinatorError> {
        self.store
            .load_run(run_id)?
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_owned()).into())
    }
}

fn verify_participant_patch_capability(
    thread_id: &str,
    statuses: &[McpServerStatus],
) -> Result<(), CoordinatorError> {
    let participant_servers = statuses
        .iter()
        .filter(|status| status.name == PARTICIPANT_MCP_SERVER)
        .collect::<Vec<_>>();
    let valid = match participant_servers.as_slice() {
        [server] => {
            server.tools.len() == 1
                && server
                    .tools
                    .get(PARTICIPANT_PATCH_TOOL)
                    .is_some_and(Value::is_object)
        }
        _ => false,
    };
    if !valid {
        return Err(CoordinatorError::app_server(
            "PATCH_TOOL_UNAVAILABLE",
            format!(
                "task MCP inventory must expose exactly {PARTICIPANT_MCP_SERVER}.{PARTICIPANT_PATCH_TOOL}"
            ),
            "mcpServerStatus/list",
            Some(thread_id),
        ));
    }
    Ok(())
}

fn allowed_participant_signals(action: NextAction) -> &'static [ParticipantSignal] {
    match action {
        NextAction::RequestPrimaryContract | NextAction::RequestReviewerContract => {
            &[ParticipantSignal::ContractReady, ParticipantSignal::Blocked]
        }
        NextAction::RequestPrimaryPlan => {
            &[ParticipantSignal::PlanReady, ParticipantSignal::Blocked]
        }
        NextAction::RequestReviewerPlanVerdict | NextAction::RequestReviewerResultVerdict => &[
            ParticipantSignal::Approved,
            ParticipantSignal::ChangesRequired,
            ParticipantSignal::Blocked,
        ],
        NextAction::RequestPrimaryIntegration => &[
            ParticipantSignal::IntegrationReady,
            ParticipantSignal::Blocked,
        ],
        NextAction::RequestPrimaryVerification => &[
            ParticipantSignal::VerificationReady,
            ParticipantSignal::Blocked,
        ],
        NextAction::RevalidateAndAccept | NextAction::WaitForUser | NextAction::Stop => &[],
    }
}

fn authoritative_envelope(
    state: &RunState,
    message_type: MessageType,
    reason_code: Option<String>,
    integration_branch: Option<String>,
    integration_sha: Option<String>,
) -> Envelope {
    Envelope {
        protocol: "worktree-merge-consensus/v1".to_owned(),
        run_id: state.facts.run_id,
        message_type,
        phase: MessagePhase::from(state.phase),
        round: state.round,
        primary_sha: state.facts.primary_sha.clone(),
        reviewer_sha: state.facts.reviewer_sha.clone(),
        plan_revision: state.plan_revision,
        integration_branch,
        integration_sha,
        reason_code,
    }
}

fn parse_contract_json(body: &str) -> Result<Value, CoordinatorError> {
    let trimmed = body.trim();
    let candidate = if let Some(inner) = trimmed
        .strip_prefix("```json")
        .and_then(|value| value.strip_suffix("```"))
    {
        inner.trim()
    } else if let Some(inner) = trimmed
        .strip_prefix("```")
        .and_then(|value| value.strip_suffix("```"))
    {
        inner.trim()
    } else {
        trimmed
    };
    let contract = serde_json::from_str::<Value>(candidate).map_err(|error| {
        CoordinatorError::operational(
            "INVALID_RESPONSE",
            format!("CONTRACT_READY body is not one JSON object: {error}"),
        )
    })?;
    if !contract.is_object() {
        return Err(CoordinatorError::operational(
            "INVALID_RESPONSE",
            "CONTRACT_READY body must be a JSON object",
        ));
    }
    Ok(contract)
}

fn require_free_markdown(body: &str, detail: &str) -> Result<(), CoordinatorError> {
    if body.trim().is_empty() {
        return Err(CoordinatorError::operational("INVALID_RESPONSE", detail));
    }
    Ok(())
}

fn changes_required_message(
    state: &RunState,
    feedback: String,
    result_review: bool,
) -> Result<ProtocolMessage, CoordinatorError> {
    require_free_markdown(
        &feedback,
        "CHANGES_REQUIRED requires nonempty free-form feedback",
    )?;
    let (branch, sha) = if result_review {
        (
            Some(state.integration_branch.clone().ok_or_else(|| {
                CoordinatorError::operational(
                    "INVALID_STATE",
                    "result feedback has no current integration branch",
                )
            })?),
            Some(state.integration_sha.clone().ok_or_else(|| {
                CoordinatorError::operational(
                    "INVALID_STATE",
                    "result feedback has no current integration SHA",
                )
            })?),
        )
    } else {
        (None, None)
    };
    Ok(ProtocolMessage {
        envelope: authoritative_envelope(
            state,
            MessageType::ChangesRequired,
            Some("REVIEW_CHANGES_REQUIRED".to_owned()),
            branch,
            sha,
        ),
        payload: json!({
            "format": "markdown",
            "feedback": feedback,
        }),
    })
}

fn build_action_payload(state: &RunState, action: NextAction) -> Result<Value, CoordinatorError> {
    let primary_contract = || {
        state.primary_contract.clone().ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "primary contract is absent from canonical persisted state",
            )
        })
    };
    let reviewer_contract = || {
        state.reviewer_contract.clone().ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "reviewer contract is absent from canonical persisted state",
            )
        })
    };
    let current_plan = || {
        state.current_plan_payload.clone().ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "current plan is absent from canonical persisted state",
            )
        })
    };

    match action {
        NextAction::RequestPrimaryContract => Ok(json!({
            "task_context": "derive the complete primary contract from this task and frozen SHA"
        })),
        NextAction::RequestReviewerContract => Ok(json!({
            "task_context": "derive the complete reviewer contract from this task and frozen SHA"
        })),
        NextAction::RequestPrimaryPlan => Ok(json!({
            "primary_contract": primary_contract()?,
            "reviewer_contract": reviewer_contract()?,
            "previous_plan": state.current_plan_payload,
            "review_feedback": state.last_plan_feedback,
            "target_integration_branch": state.target_integration_branch,
            "required_test_commands": state.required_test_commands,
        })),
        NextAction::RequestReviewerPlanVerdict => {
            let mut plan = current_plan()?;
            let plan_hash = canonical_json_hash(&plan);
            plan.as_object_mut()
                .ok_or_else(|| {
                    CoordinatorError::operational(
                        "INVALID_STATE",
                        "canonical plan payload is not an object",
                    )
                })?
                .insert("plan_hash".into(), json!(plan_hash));
            Ok(plan)
        }
        NextAction::RequestPrimaryIntegration => {
            let plan = current_plan()?;
            let approval = state.plan_approval_payload.clone().ok_or_else(|| {
                CoordinatorError::operational(
                    "HISTORY_UNAVAILABLE",
                    "plan approval is absent from canonical persisted state",
                )
            })?;
            Ok(json!({
                "primary_contract": primary_contract()?,
                "reviewer_contract": reviewer_contract()?,
                "approved_plan": plan.get("plan").cloned().unwrap_or(Value::Null),
                "coverage_matrix": plan.get("coverage_matrix").cloned().unwrap_or(Value::Null),
                "approval": approval,
                "target_integration_branch": state.target_integration_branch,
                "previous_integration_sha": state.integration_sha,
                "result_feedback": state.last_result_feedback,
            }))
        }
        NextAction::RequestPrimaryVerification => {
            let integration = current_integration_payload(state)?;
            Ok(json!({
                "integration_evidence": integration.get("integration_evidence").cloned().unwrap_or(Value::Null),
                "changed_files": integration.get("changed_files").cloned().unwrap_or(Value::Null),
                "required_test_commands": state.required_test_commands,
                "verification_worktree": state.verification_worktree,
                "integration_branch": state.integration_branch,
                "integration_sha": state.integration_sha,
            }))
        }
        NextAction::RequestReviewerResultVerdict => {
            let plan = current_plan()?;
            let integration = current_integration_payload(state)?;
            Ok(json!({
                "primary_contract": primary_contract()?,
                "reviewer_contract": reviewer_contract()?,
                "approved_plan": plan.get("plan").cloned().unwrap_or(Value::Null),
                "coverage_matrix": plan.get("coverage_matrix").cloned().unwrap_or(Value::Null),
                "integration_evidence": integration.get("integration_evidence").cloned().unwrap_or(Value::Null),
                "test_evidence": integration.get("test_evidence").cloned().unwrap_or(Value::Null),
                "changed_files": integration.get("changed_files").cloned().unwrap_or(Value::Null),
                "integration_branch": state.integration_branch,
                "integration_sha": state.integration_sha,
            }))
        }
        NextAction::RevalidateAndAccept | NextAction::WaitForUser | NextAction::Stop => Err(
            CoordinatorError::operational("INVALID_STATE", "action does not start a task turn"),
        ),
    }
}

fn current_integration_payload(state: &RunState) -> Result<&Value, CoordinatorError> {
    state.current_integration_payload.as_ref().ok_or_else(|| {
        CoordinatorError::operational(
            "HISTORY_UNAVAILABLE",
            "current integration evidence is absent from canonical persisted state",
        )
    })
}

fn command_execution_items(turn: &Value) -> Result<Vec<&Value>, CoordinatorError> {
    let items = turn.get("items").and_then(Value::as_array).ok_or_else(|| {
        CoordinatorError::operational(
            "HISTORY_UNAVAILABLE",
            "completed turn has no canonical items",
        )
    })?;
    Ok(items
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("commandExecution"))
        .collect())
}

fn merge_completed_turn_evidence(
    persisted_turn: &Value,
    event_turn: &Value,
    completed_items: Vec<Value>,
) -> Result<Value, CoordinatorError> {
    let persisted_id = persisted_turn.get("id").and_then(Value::as_str);
    let event_id = event_turn.get("id").and_then(Value::as_str);
    if persisted_id.is_none() || persisted_id != event_id {
        return Err(CoordinatorError::operational(
            "HISTORY_UNAVAILABLE",
            "persisted turn and turn/completed event identities differ",
        ));
    }
    let mut ordered = Vec::new();
    let mut item_ids = HashSet::new();
    let mut append = |item: &Value| -> Result<(), CoordinatorError> {
        let item_id = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                CoordinatorError::operational(
                    "HISTORY_UNAVAILABLE",
                    "completed turn evidence contains an item without a nonempty id",
                )
            })?;
        if item_ids.insert(item_id.to_owned()) {
            ordered.push(item.clone());
        }
        Ok(())
    };
    for item in &completed_items {
        append(item)?;
    }
    for source in [event_turn, persisted_turn] {
        let items = source
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                CoordinatorError::operational(
                    "HISTORY_UNAVAILABLE",
                    "completed turn evidence has no canonical items",
                )
            })?;
        for item in items {
            append(item)?;
        }
    }
    let mut merged_items = Vec::with_capacity(ordered.len());
    for item_type in ["userMessage", "__non_message__", "agentMessage"] {
        merged_items.extend(
            ordered
                .iter()
                .filter(|item| {
                    let current = item.get("type").and_then(Value::as_str);
                    match item_type {
                        "__non_message__" => {
                            !matches!(current, Some("userMessage" | "agentMessage"))
                        }
                        expected => current == Some(expected),
                    }
                })
                .cloned(),
        );
    }
    let mut merged = persisted_turn.clone();
    let object = merged.as_object_mut().ok_or_else(|| {
        CoordinatorError::operational("HISTORY_UNAVAILABLE", "completed turn is not an object")
    })?;
    object.insert("items".into(), Value::Array(merged_items));
    Ok(merged)
}

fn verify_integration_execution_items(
    state: &RunState,
    turn: &Value,
    request_hash: &str,
    successful_patch_hash: Option<&str>,
    has_archived_attempt: bool,
) -> Result<(), CoordinatorError> {
    let command_items = command_execution_items(turn)?;
    let patch_items = turn
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("mcpToolCall"))
        .collect::<Vec<_>>();

    if let Some(successful_patch_hash) = successful_patch_hash {
        if !patch_items.is_empty() {
            if let Some(blocker) = recoverable_integration_turn_blocker(
                state,
                turn,
                request_hash,
                successful_patch_hash,
                false,
            ) {
                return Err(CoordinatorError::operational(
                    "FORBIDDEN_OPERATION",
                    blocker,
                ));
            }
            return Ok(());
        }
        if !has_archived_attempt {
            return Err(CoordinatorError::operational(
                "FORBIDDEN_OPERATION",
                "a patch-success confirmation turn has no archived originating attempt",
            ));
        }
        for item in command_items {
            let command = item.get("command").and_then(Value::as_str).ok_or_else(|| {
                CoordinatorError::operational(
                    "INVALID_RESPONSE",
                    "commandExecution item omits command",
                )
            })?;
            let cwd = item.get("cwd").and_then(Value::as_str).ok_or_else(|| {
                CoordinatorError::operational("INVALID_RESPONSE", "commandExecution item omits cwd")
            })?;
            if item.get("status").and_then(Value::as_str) != Some("completed")
                || item.get("exitCode").and_then(Value::as_i64) != Some(0)
                || !is_retry_safe_read_only_integration_command(state, cwd, command)
            {
                return Err(CoordinatorError::operational(
                    "FORBIDDEN_OPERATION",
                    format!(
                        "patch-success confirmation executed a non-read-only command: {command}"
                    ),
                ));
            }
        }
        return Ok(());
    }

    if !patch_items.is_empty() {
        return Err(CoordinatorError::operational(
            "FORBIDDEN_OPERATION",
            "integration turn contains a controlled patch call without a SQLite success record",
        ));
    }

    for item in command_items {
        let command = item.get("command").and_then(Value::as_str).ok_or_else(|| {
            CoordinatorError::operational("INVALID_RESPONSE", "commandExecution item omits command")
        })?;
        let cwd = item.get("cwd").and_then(Value::as_str).ok_or_else(|| {
            CoordinatorError::operational("INVALID_RESPONSE", "commandExecution item omits cwd")
        })?;
        if decide_command_approval(
            state,
            &json!({"cwd": cwd, "command": command, "availableDecisions": ["accept"]}),
        ) != ApprovalDecision::Accept
        {
            return Err(CoordinatorError::operational(
                "FORBIDDEN_OPERATION",
                format!("integration turn executed a command outside policy: {command}"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
fn integration_patch_mcp_blocker(
    state: &RunState,
    item: &Value,
    request_hash: &str,
) -> Option<String> {
    if item.get("status").and_then(Value::as_str) != Some("completed") {
        return Some("controlled patch MCP call is not canonically completed".into());
    }
    controlled_patch_mcp_identity_blocker(state, item, request_hash)
}

fn controlled_patch_mcp_identity_blocker(
    state: &RunState,
    item: &Value,
    request_hash: &str,
) -> Option<String> {
    controlled_patch_mcp_identity_blocker_for_recovery(state, item, request_hash, false)
}

fn recoverable_controlled_patch_mcp_identity_blocker(
    state: &RunState,
    item: &Value,
    request_hash: &str,
) -> Option<String> {
    controlled_patch_mcp_identity_blocker_for_recovery(state, item, request_hash, true)
}

fn controlled_patch_mcp_identity_blocker_for_recovery(
    state: &RunState,
    item: &Value,
    request_hash: &str,
    allow_legacy_server: bool,
) -> Option<String> {
    let server = item.get("server").and_then(Value::as_str);
    let server_matches = server == Some(PARTICIPANT_MCP_SERVER)
        || (allow_legacy_server && server == Some("worktreeMergeConsensus"));
    if item.get("pluginId").and_then(Value::as_str)
        != Some("worktree-merge-consensus@worktree-merge-consensus")
        || !server_matches
        || item.get("tool").and_then(Value::as_str) != Some(PARTICIPANT_PATCH_TOOL)
    {
        return Some(
            "integration turn invoked an MCP tool outside the exact controlled patch capability"
                .into(),
        );
    }
    if item.get("appContext").is_some_and(|value| !value.is_null()) {
        return Some("controlled patch MCP call carries external app context".into());
    }
    let arguments = item.get("arguments").and_then(Value::as_object);
    let run_id = state.facts.run_id.to_string();
    if arguments
        .and_then(|arguments| arguments.get("run_id"))
        .and_then(Value::as_str)
        != Some(run_id.as_str())
        || arguments
            .and_then(|arguments| arguments.get("request_hash"))
            .and_then(Value::as_str)
            != Some(request_hash)
        || !arguments
            .and_then(|arguments| arguments.get("patch"))
            .and_then(Value::as_str)
            .is_some_and(|patch| !patch.trim().is_empty())
    {
        return Some("controlled patch MCP arguments do not match the active run request".into());
    }
    None
}

fn pending_controlled_patch_approval_blocker(
    state: &RunState,
    detail: &ThreadDetail,
    turn: &Value,
    request_hash: &str,
    allowed_patch_statuses: &[&str],
) -> Option<String> {
    let waiting_on_approval = detail
        .summary
        .status
        .get("activeFlags")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|flag| flag.as_str() == Some("waitingOnApproval"));
    if turn.get("status").and_then(Value::as_str) == Some("inProgress") && !waiting_on_approval {
        return Some("active controlled patch turn is not canonically waiting on approval".into());
    }
    let Some(items) = turn.get("items").and_then(Value::as_array) else {
        return Some("canonical items are unavailable".into());
    };
    if items.is_empty() {
        return Some("canonical items are empty".into());
    }
    let mut patch_calls = 0;
    for item in items {
        let Some(item_type) = item.get("type").and_then(Value::as_str) else {
            return Some("canonical item has no type".into());
        };
        match item_type {
            "userMessage" | "agentMessage" | "reasoning" => {}
            "contextCompaction" => {
                if let Some(blocker) = context_compaction_retry_blocker(item) {
                    return Some(blocker);
                }
            }
            "commandExecution" => {
                if item.get("status").and_then(Value::as_str) != Some("completed")
                    || item.get("exitCode").and_then(Value::as_i64) != Some(0)
                {
                    return Some(
                        "integration command is not canonically completed with exit code zero"
                            .into(),
                    );
                }
                let Some(command) = item.get("command").and_then(Value::as_str) else {
                    return Some("integration command omits its canonical command".into());
                };
                let Some(cwd) = item.get("cwd").and_then(Value::as_str) else {
                    return Some("integration command omits its canonical cwd".into());
                };
                if item
                    .get("source")
                    .is_some_and(|source| source.as_str() != Some("agent"))
                    || decide_command_approval(
                        state,
                        &json!({
                            "cwd": cwd,
                            "command": command,
                            "availableDecisions": ["accept"]
                        }),
                    ) != ApprovalDecision::Accept
                {
                    return Some(
                        "integration command is outside the frozen execution policy".into(),
                    );
                }
            }
            "mcpToolCall" => {
                patch_calls += 1;
                if !item
                    .get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|status| allowed_patch_statuses.contains(&status))
                {
                    return Some("controlled patch MCP call has an unexpected status".into());
                }
                if let Some(blocker) =
                    controlled_patch_mcp_identity_blocker(state, item, request_hash)
                {
                    return Some(blocker);
                }
            }
            _ => {
                return Some(format!(
                    "canonical item type {item_type} is not allowed in a controlled patch approval retry"
                ));
            }
        }
    }
    if patch_calls != 1 {
        return Some("controlled patch approval retry requires exactly one MCP call".into());
    }
    None
}

fn recoverable_integration_turn_blocker(
    state: &RunState,
    turn: &Value,
    request_hash: &str,
    successful_patch_hash: &str,
    allow_legacy_server: bool,
) -> Option<String> {
    let Some(items) = turn.get("items").and_then(Value::as_array) else {
        return Some("canonical items are unavailable".into());
    };
    if items.is_empty() {
        return Some("canonical items are empty".into());
    }

    let mut completed_patch_calls = 0usize;
    let mut completed_patch_seen = false;
    let mut final_agent_seen = false;
    for item in items {
        let Some(item_type) = item.get("type").and_then(Value::as_str) else {
            return Some("canonical item has no type".into());
        };
        match item_type {
            "userMessage" | "reasoning" => {}
            "agentMessage" => final_agent_seen = true,
            "contextCompaction" => {
                if let Some(blocker) = context_compaction_retry_blocker(item) {
                    return Some(blocker);
                }
            }
            "commandExecution" => {
                if final_agent_seen {
                    return Some(
                        "integration command appears after the final agent response".into(),
                    );
                }
                if item.get("status").and_then(Value::as_str) != Some("completed")
                    || item.get("exitCode").and_then(Value::as_i64) != Some(0)
                {
                    return Some(
                        "integration command is not canonically completed with exit code zero"
                            .into(),
                    );
                }
                let Some(command) = item.get("command").and_then(Value::as_str) else {
                    return Some("integration command omits its canonical command".into());
                };
                let Some(cwd) = item.get("cwd").and_then(Value::as_str) else {
                    return Some("integration command omits its canonical cwd".into());
                };
                if item
                    .get("source")
                    .is_some_and(|source| source.as_str() != Some("agent"))
                    || decide_command_approval(
                        state,
                        &json!({
                            "cwd": cwd,
                            "command": command,
                            "availableDecisions": ["accept"]
                        }),
                    ) != ApprovalDecision::Accept
                {
                    return Some(
                        "integration command is outside the frozen execution policy".into(),
                    );
                }
            }
            "mcpToolCall" => {
                if final_agent_seen {
                    return Some(
                        "controlled patch call appears after the final agent response".into(),
                    );
                }
                let identity_blocker = if allow_legacy_server {
                    recoverable_controlled_patch_mcp_identity_blocker(state, item, request_hash)
                } else {
                    controlled_patch_mcp_identity_blocker(state, item, request_hash)
                };
                if let Some(blocker) = identity_blocker {
                    return Some(blocker);
                }
                let patch = item
                    .get("arguments")
                    .and_then(|arguments| arguments.get("patch"))
                    .and_then(Value::as_str)
                    .expect("controlled patch identity validation requires a patch string");
                let patch_hash = canonical_json_hash(&json!({"patch": patch}));
                match item.get("status").and_then(Value::as_str) {
                    Some("failed") if !completed_patch_seen => {
                        if patch_hash == successful_patch_hash {
                            return Some(
                                "failed patch preflight has the recorded successful patch hash"
                                    .into(),
                            );
                        }
                    }
                    Some("completed") if !completed_patch_seen => {
                        if patch_hash != successful_patch_hash {
                            return Some(
                                "completed controlled patch does not match the SQLite success record"
                                    .into(),
                            );
                        }
                        completed_patch_seen = true;
                        completed_patch_calls += 1;
                    }
                    Some("completed") => {
                        return Some(
                            "integration turn contains more than one completed patch".into(),
                        );
                    }
                    Some("failed") => {
                        return Some(
                            "failed patch preflight appears after the successful patch".into(),
                        );
                    }
                    Some(status) => {
                        return Some(format!(
                            "controlled patch MCP call has unexpected status {status}"
                        ));
                    }
                    None => return Some("controlled patch MCP call omits status".into()),
                }
            }
            _ => {
                return Some(format!(
                    "canonical item type {item_type} may have side effects"
                ));
            }
        }
    }
    if completed_patch_calls != 1 {
        return Some(
            "integration invalid-response recovery requires exactly one recorded successful patch"
                .into(),
        );
    }
    None
}

fn verify_marker_only_verification_turn(turn: &Value) -> Result<(), CoordinatorError> {
    let items = turn.get("items").and_then(Value::as_array).ok_or_else(|| {
        CoordinatorError::operational(
            "HISTORY_UNAVAILABLE",
            "completed verification turn has no canonical items",
        )
    })?;
    for item in items {
        let item_type = item.get("type").and_then(Value::as_str).ok_or_else(|| {
            CoordinatorError::operational(
                "INVALID_RESPONSE",
                "canonical verification item has no type",
            )
        })?;
        match item_type {
            "userMessage" | "reasoning" | "agentMessage" => {}
            "contextCompaction" => {
                if let Some(blocker) = context_compaction_retry_blocker(item) {
                    return Err(CoordinatorError::operational("INVALID_RESPONSE", blocker));
                }
            }
            "commandExecution" | "fileChange" | "mcpToolCall" | "dynamicToolCall" => {
                return Err(CoordinatorError::operational(
                    "FORBIDDEN_OPERATION",
                    format!(
                        "marker-only verification turn contains side-effect-capable item {item_type}"
                    ),
                ));
            }
            _ => {
                return Err(CoordinatorError::operational(
                    "INVALID_RESPONSE",
                    format!("canonical verification item type {item_type} is not allowed"),
                ));
            }
        }
    }
    Ok(())
}

fn append_verification_record(
    record: &VerificationCommandRecord,
    evidence: &mut Vec<TestEvidence>,
    failures: &mut Vec<Value>,
) -> Result<(), CoordinatorError> {
    let exit_code = record.exit_code.ok_or_else(|| {
        CoordinatorError::operational(
            "INVALID_STATE",
            format!(
                "completed verification command {} has no exit code",
                record.item_id
            ),
        )
    })?;
    let stdout = record.stdout.as_deref().ok_or_else(|| {
        CoordinatorError::operational(
            "INVALID_STATE",
            format!(
                "completed verification command {} has no stdout",
                record.item_id
            ),
        )
    })?;
    let stderr = record.stderr.as_deref().ok_or_else(|| {
        CoordinatorError::operational(
            "INVALID_STATE",
            format!(
                "completed verification command {} has no stderr",
                record.item_id
            ),
        )
    })?;
    if exit_code != 0 {
        let separator = if stdout.is_empty() || stderr.is_empty() {
            ""
        } else {
            "\n"
        };
        let combined = format!("{stdout}{separator}{stderr}");
        failures.push(json!({
            "command": record.command,
            "exit_code": exit_code,
            "item_id": record.item_id,
            "output": bounded_verification_output(&combined),
        }));
    }
    evidence.push(TestEvidence {
        command: record.command.clone(),
        exit_code: i64::from(exit_code),
        turn_id: record.turn_id.clone(),
        item_id: record.item_id.clone(),
        cwd: record.cwd.clone(),
    });
    Ok(())
}

fn bounded_verification_output(output: &str) -> String {
    if output.len() <= MAX_VERIFICATION_FAILURE_OUTPUT_BYTES {
        return output.to_owned();
    }
    let retained_bytes = MAX_VERIFICATION_FAILURE_OUTPUT_BYTES
        .saturating_sub(VERIFICATION_OUTPUT_TRUNCATION_MARKER.len());
    let mut start = output.len() - retained_bytes;
    while !output.is_char_boundary(start) {
        start += 1;
    }
    format!(
        "{VERIFICATION_OUTPUT_TRUNCATION_MARKER}{}",
        &output[start..]
    )
}

fn declared_test_commands(message: &ProtocolMessage) -> Result<Vec<String>, CoordinatorError> {
    let values = match message.envelope.message_type {
        MessageType::ContractReady => message
            .payload
            .get("contract")
            .and_then(|contract| contract.get("tests")),
        MessageType::PlanReady => message.payload.get("test_commands"),
        _ => return Ok(Vec::new()),
    }
    .and_then(Value::as_array)
    .ok_or_else(|| {
        CoordinatorError::operational("INVALID_RESPONSE", "declared tests must be a command array")
    })?;
    values
        .iter()
        .map(|value| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                CoordinatorError::operational(
                    "INVALID_RESPONSE",
                    "declared test commands must be strings",
                )
            })
        })
        .collect()
}

fn changed_files(payload: &Value) -> Result<Vec<PathBuf>, CoordinatorError> {
    payload
        .get("changed_files")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CoordinatorError::operational(
                "INVALID_RESPONSE",
                "integration evidence requires a changed_files array",
            )
        })?
        .iter()
        .map(|value| {
            value.as_str().map(PathBuf::from).ok_or_else(|| {
                CoordinatorError::operational(
                    "INVALID_RESPONSE",
                    "changed_files entries must be strings",
                )
            })
        })
        .collect()
}

fn find_turn_by_request_hash(
    detail: &ThreadDetail,
    request_hash: &str,
    archived_turn_ids: &[String],
) -> Option<String> {
    detail.turns.iter().rev().find_map(|turn| {
        let turn_id = turn.get("id").and_then(Value::as_str)?;
        if archived_turn_ids.iter().any(|archived| archived == turn_id) {
            return None;
        }
        let matched = turn_contains_request_hash(turn, request_hash);
        matched.then(|| turn_id.to_owned())
    })
}

fn turn_contains_request_hash(turn: &Value, request_hash: &str) -> bool {
    let marker = format!("\"request_hash\":\"{request_hash}\"");
    turn.get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("userMessage"))
        .flat_map(|item| {
            item.get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|input| input.get("text").and_then(Value::as_str))
        .any(|text| text.contains(&marker))
}

fn terminal_turn_retry_blocker(turn: &Value) -> Option<String> {
    let Some(items) = turn.get("items").and_then(Value::as_array) else {
        return Some("canonical items are unavailable".into());
    };
    if items.is_empty() {
        return Some("canonical items are empty".into());
    }
    for item in items {
        let Some(item_type) = item.get("type").and_then(Value::as_str) else {
            return Some("canonical item has no type".into());
        };
        match item_type {
            "userMessage" | "agentMessage" | "reasoning" => {}
            "contextCompaction" => {
                if let Some(blocker) = context_compaction_retry_blocker(item) {
                    return Some(blocker);
                }
            }
            _ => {
                return Some(format!(
                    "canonical item type {item_type} may have side effects"
                ));
            }
        }
    }
    None
}

fn interrupted_forbidden_operation_retry_blocker(state: &RunState, turn: &Value) -> Option<String> {
    let Some(items) = turn.get("items").and_then(Value::as_array) else {
        return Some("canonical items are unavailable".into());
    };
    if items.is_empty() {
        return Some("canonical items are empty".into());
    }
    for item in items {
        let Some(item_type) = item.get("type").and_then(Value::as_str) else {
            return Some("canonical item has no type".into());
        };
        match item_type {
            "userMessage" | "agentMessage" | "reasoning" => {}
            "contextCompaction" => {
                if let Some(blocker) = context_compaction_retry_blocker(item) {
                    return Some(blocker);
                }
            }
            "commandExecution" => {
                let status = item.get("status").and_then(Value::as_str);
                let exit_code = item.get("exitCode");
                let terminal_shape_is_valid = match status {
                    Some("completed") => exit_code.and_then(Value::as_i64).is_some(),
                    Some("failed" | "declined") => {
                        exit_code.is_none_or(|value| value.is_null() || value.as_i64().is_some())
                    }
                    _ => false,
                };
                if !terminal_shape_is_valid {
                    return Some(
                        "read-only command execution is not in a canonical terminal state".into(),
                    );
                }
                let Some(command) = item.get("command").and_then(Value::as_str) else {
                    return Some("read-only command execution omits its canonical command".into());
                };
                let Some(cwd) = item.get("cwd").and_then(Value::as_str) else {
                    return Some("read-only command execution omits its canonical cwd".into());
                };
                if item
                    .get("source")
                    .is_some_and(|source| source.as_str() != Some("agent"))
                    || !is_retry_safe_read_only_integration_command(state, cwd, command)
                {
                    return Some(
                        "command execution is not an approved retry-safe read-only integration query".into(),
                    );
                }
            }
            _ => {
                return Some(format!(
                    "canonical item type {item_type} may have side effects"
                ));
            }
        }
    }
    None
}

fn completed_read_only_turn_retry_blocker(turn: &Value) -> Option<String> {
    let Some(items) = turn.get("items").and_then(Value::as_array) else {
        return Some("canonical items are unavailable".into());
    };
    if items.is_empty() {
        return Some("canonical items are empty".into());
    }
    let mut has_agent_message = false;
    for item in items {
        let Some(item_type) = item.get("type").and_then(Value::as_str) else {
            return Some("canonical item has no type".into());
        };
        match item_type {
            "userMessage" | "reasoning" => {}
            "agentMessage" => has_agent_message = true,
            "contextCompaction" => {
                if let Some(blocker) = context_compaction_retry_blocker(item) {
                    return Some(blocker);
                }
            }
            "commandExecution" => {
                if item.get("status").and_then(Value::as_str) != Some("completed") {
                    return Some("command execution is not canonically completed".into());
                }
                if item.get("command").and_then(Value::as_str).is_none() {
                    return Some("command execution omits its canonical command".into());
                }
            }
            "mcpToolCall" => {
                if let Some(blocker) = read_only_consensus_mcp_retry_blocker(item) {
                    return Some(blocker);
                }
            }
            _ => {
                return Some(format!(
                    "canonical item type {item_type} is not allowed in a read-only response retry"
                ));
            }
        }
    }
    (!has_agent_message).then(|| "canonical turn has no agent response".into())
}

fn verification_without_execution_retry_blocker(turn: &Value) -> Option<String> {
    let Some(items) = turn.get("items").and_then(Value::as_array) else {
        return Some("canonical verification items are unavailable".into());
    };
    if items.is_empty() {
        return Some("canonical verification items are empty".into());
    }
    let mut has_agent_message = false;
    for item in items {
        let Some(item_type) = item.get("type").and_then(Value::as_str) else {
            return Some("canonical verification item has no type".into());
        };
        match item_type {
            "userMessage" | "reasoning" => {}
            "agentMessage" => has_agent_message = true,
            "contextCompaction" => {
                if let Some(blocker) = context_compaction_retry_blocker(item) {
                    return Some(blocker);
                }
            }
            "commandExecution" => {
                return Some(
                    "verification retry is forbidden after any test command was executed".into(),
                );
            }
            _ => {
                return Some(format!(
                    "canonical verification item type {item_type} may have side effects"
                ));
            }
        }
    }
    (!has_agent_message).then(|| "canonical verification turn has no agent response".into())
}

fn context_compaction_retry_blocker(item: &Value) -> Option<String> {
    let Some(object) = item.as_object() else {
        return Some("context compaction item is not an object".into());
    };
    if object.len() != 2 || !object.contains_key("id") || !object.contains_key("type") {
        return Some("context compaction item has fields outside its frozen schema".into());
    }
    if object.get("type").and_then(Value::as_str) != Some("contextCompaction") {
        return Some("context compaction item has an unexpected type".into());
    }
    if !object
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty())
    {
        return Some("context compaction item has no nonempty id".into());
    }
    None
}

fn read_only_consensus_mcp_retry_blocker(item: &Value) -> Option<String> {
    if item.get("status").and_then(Value::as_str) != Some("completed") {
        return Some("MCP tool call is not canonically completed".into());
    }
    if item.get("pluginId").and_then(Value::as_str)
        != Some("worktree-merge-consensus@worktree-merge-consensus")
        || item.get("server").and_then(Value::as_str) != Some("worktreeMergeConsensus")
    {
        return Some("MCP tool call is not owned by the consensus plugin".into());
    }
    if item.get("appContext").is_some_and(|value| !value.is_null()) {
        return Some("MCP tool call carries external app context".into());
    }
    if !item.get("arguments").is_some_and(Value::is_object) {
        return Some("MCP tool call omits canonical object arguments".into());
    }
    match item.get("tool").and_then(Value::as_str) {
        Some("consensus_list_threads" | "consensus_list_worktrees" | "consensus_status") => None,
        Some(tool) => Some(format!(
            "consensus MCP tool {tool} is not a read-only retry-safe query"
        )),
        None => Some("MCP tool call omits its canonical tool name".into()),
    }
}

fn is_test_declaration_action(action: NextAction) -> bool {
    matches!(
        action,
        NextAction::RequestPrimaryContract
            | NextAction::RequestReviewerContract
            | NextAction::RequestPrimaryPlan
    )
}

fn declaration_phase(action: NextAction) -> Option<Phase> {
    match action {
        NextAction::RequestPrimaryContract | NextAction::RequestReviewerContract => {
            Some(Phase::Contract)
        }
        NextAction::RequestPrimaryPlan => Some(Phase::PlanReview),
        _ => None,
    }
}

fn preintegration_read_only_phase(action: NextAction) -> Option<Phase> {
    match action {
        NextAction::RequestPrimaryContract | NextAction::RequestReviewerContract => {
            Some(Phase::Contract)
        }
        NextAction::RequestPrimaryPlan | NextAction::RequestReviewerPlanVerdict => {
            Some(Phase::PlanReview)
        }
        _ => None,
    }
}

fn invalid_test_retry_action(state: &RunState) -> Result<Option<NextAction>, CoordinatorError> {
    if state.reason_code.as_deref() != Some("INVALID_TEST_COMMAND") {
        return Ok(None);
    }
    if !matches!(
        state.status,
        RunStatus::PausedUserAction | RunStatus::Blocked
    ) {
        return Err(CoordinatorError::operational(
            "INCOMPATIBLE_STATE",
            "invalid test-command reason is attached to a non-retryable run status",
        ));
    }
    let diagnostic = state.last_error.as_ref().ok_or_else(|| {
        CoordinatorError::operational(
            "INCOMPATIBLE_STATE",
            "invalid test-command state has no originating diagnostic",
        )
    })?;
    if diagnostic.code != "INVALID_TEST_COMMAND" || !is_test_declaration_action(diagnostic.action) {
        return Err(CoordinatorError::operational(
            "INCOMPATIBLE_STATE",
            "invalid test-command state has a mismatched originating action",
        ));
    }
    if state.status == RunStatus::PausedUserAction
        && (state.next_action != diagnostic.action
            || declaration_phase(diagnostic.action) != Some(state.phase))
    {
        return Err(CoordinatorError::operational(
            "INCOMPATIBLE_STATE",
            "paused invalid test-command state does not preserve its pending declaration",
        ));
    }
    if state.status == RunStatus::Blocked
        && (state.next_action != NextAction::Stop || state.phase != Phase::Blocked)
    {
        return Err(CoordinatorError::operational(
            "INCOMPATIBLE_STATE",
            "legacy blocked invalid test-command state has inconsistent terminal metadata",
        ));
    }
    Ok(Some(diagnostic.action))
}

fn integration_invalid_response_retry_action(
    state: &RunState,
) -> Result<Option<NextAction>, CoordinatorError> {
    if state.reason_code.as_deref() != Some("INVALID_RESPONSE") {
        return Ok(None);
    }
    if state.status != RunStatus::Blocked
        || state.next_action != NextAction::Stop
        || state.phase != Phase::Blocked
    {
        return Err(CoordinatorError::operational(
            "INCOMPATIBLE_STATE",
            "invalid-response reason is attached to inconsistent terminal metadata",
        ));
    }
    let diagnostic = state.last_error.as_ref().ok_or_else(|| {
        CoordinatorError::operational(
            "INCOMPATIBLE_STATE",
            "invalid-response state has no originating diagnostic",
        )
    })?;
    if diagnostic.code != "INVALID_RESPONSE"
        || diagnostic.action != NextAction::RequestPrimaryIntegration
        || diagnostic.role != Some(Role::Primary)
        || diagnostic.thread_id.as_deref() != Some(state.facts.primary_thread_id.as_str())
    {
        return Ok(None);
    }
    if !diagnostic
        .detail
        .contains("message requires an integration_branch")
        && !diagnostic
            .detail
            .contains("message requires an integration_sha")
    {
        return Err(CoordinatorError::operational(
            "MODEL_RESPONSE_RETRY_UNSAFE",
            "integration invalid-response recovery is limited to omitted top-level result identity",
        ));
    }
    if state.integration_branch.is_some()
        || state.integration_sha.is_some()
        || state.current_integration_payload.is_some()
        || state.verification_worktree.is_some()
        || !state.test_evidence.is_empty()
    {
        return Err(CoordinatorError::operational(
            "MODEL_RESPONSE_RETRY_UNSAFE",
            "integration invalid-response recovery cannot replace accepted result state",
        ));
    }
    Ok(Some(NextAction::RequestPrimaryIntegration))
}

fn verification_without_execution_retry_action(
    state: &RunState,
) -> Result<Option<NextAction>, CoordinatorError> {
    if state.reason_code.as_deref() != Some("TEST_FAILURE") {
        return Ok(None);
    }
    if state.status != RunStatus::Blocked
        || state.next_action != NextAction::Stop
        || state.phase != Phase::Blocked
    {
        return Err(CoordinatorError::operational(
            "INCOMPATIBLE_STATE",
            "test-failure reason is attached to inconsistent terminal metadata",
        ));
    }
    let diagnostic = state.last_error.as_ref().ok_or_else(|| {
        CoordinatorError::operational(
            "INCOMPATIBLE_STATE",
            "test-failure state has no originating diagnostic",
        )
    })?;
    if diagnostic.code != "TEST_FAILURE"
        || diagnostic.action != NextAction::RequestPrimaryVerification
        || diagnostic.role != Some(Role::Primary)
        || diagnostic.thread_id.as_deref() != Some(state.facts.primary_thread_id.as_str())
        || diagnostic.detail
            != "verification must execute each frozen command exactly once and no other command"
    {
        return Ok(None);
    }
    Ok(Some(NextAction::RequestPrimaryVerification))
}

fn verification_environment_unavailable_retry_action(
    state: &RunState,
) -> Result<Option<NextAction>, CoordinatorError> {
    if state.reason_code.as_deref() != Some("CARGO_UNAVAILABLE") {
        return Ok(None);
    }
    if state.status != RunStatus::Blocked
        || state.next_action != NextAction::Stop
        || state.phase != Phase::Blocked
    {
        return Err(CoordinatorError::operational(
            "INCOMPATIBLE_STATE",
            "verification environment blocker is attached to inconsistent terminal metadata",
        ));
    }
    if !state.test_evidence.is_empty()
        || state.integration_branch.is_none()
        || state.integration_sha.is_none()
        || state.current_integration_payload.is_none()
        || state.verification_worktree.is_none()
        || state.required_test_commands.is_empty()
        || state.accepted_result.is_some()
    {
        return Err(CoordinatorError::operational(
            "MODEL_RESPONSE_RETRY_UNSAFE",
            "verification environment recovery requires an unchanged unaccepted integration result",
        ));
    }
    Ok(Some(NextAction::RequestPrimaryVerification))
}

fn invalid_response_retry_action(state: &RunState) -> Result<Option<NextAction>, CoordinatorError> {
    if state.reason_code.as_deref() != Some("INVALID_RESPONSE") {
        return Ok(None);
    }
    if state.status != RunStatus::Blocked
        || state.next_action != NextAction::Stop
        || state.phase != Phase::Blocked
    {
        return Err(CoordinatorError::operational(
            "INCOMPATIBLE_STATE",
            "invalid-response reason is attached to inconsistent terminal metadata",
        ));
    }
    let diagnostic = state.last_error.as_ref().ok_or_else(|| {
        CoordinatorError::operational(
            "INCOMPATIBLE_STATE",
            "invalid-response state has no originating diagnostic",
        )
    })?;
    if diagnostic.code != "INVALID_RESPONSE"
        || preintegration_read_only_phase(diagnostic.action).is_none()
    {
        return Err(CoordinatorError::operational(
            "MODEL_RESPONSE_RETRY_UNSAFE",
            "invalid-response recovery is limited to a matching pre-integration read-only action",
        ));
    }
    if state.integration_branch.is_some()
        || state.integration_sha.is_some()
        || state.current_integration_payload.is_some()
    {
        return Err(CoordinatorError::operational(
            "MODEL_RESPONSE_RETRY_UNSAFE",
            "invalid-response recovery cannot run after integration begins",
        ));
    }
    Ok(Some(diagnostic.action))
}

fn execution_tool_unavailable_retry_action(
    state: &RunState,
) -> Result<Option<NextAction>, CoordinatorError> {
    if state.reason_code.as_deref() != Some("EXECUTION_TOOL_UNAVAILABLE") {
        return Ok(None);
    }
    if state.status != RunStatus::Blocked
        || state.phase != Phase::Blocked
        || state.next_action != NextAction::Stop
        || state.last_error.is_some()
    {
        return Err(CoordinatorError::operational(
            "INCOMPATIBLE_STATE",
            "execution-tool blocker has inconsistent terminal metadata",
        ));
    }
    if state.integration_branch.is_some()
        || state.integration_sha.is_some()
        || state.current_integration_payload.is_some()
        || state.verification_worktree.is_some()
        || !state.test_evidence.is_empty()
    {
        return Err(CoordinatorError::operational(
            "MODEL_RESPONSE_RETRY_UNSAFE",
            "execution-tool recovery cannot run after integration side effects exist",
        ));
    }
    Ok(Some(NextAction::RequestPrimaryIntegration))
}

fn forbidden_operation_retry_action(
    state: &RunState,
) -> Result<Option<NextAction>, CoordinatorError> {
    if state.reason_code.as_deref() != Some("FORBIDDEN_OPERATION") {
        return Ok(None);
    }
    if state.status != RunStatus::Blocked
        || state.phase != Phase::Blocked
        || state.next_action != NextAction::Stop
    {
        return Err(CoordinatorError::operational(
            "INCOMPATIBLE_STATE",
            "forbidden-operation reason is attached to inconsistent terminal metadata",
        ));
    }
    let diagnostic = state.last_error.as_ref().ok_or_else(|| {
        CoordinatorError::operational(
            "INCOMPATIBLE_STATE",
            "forbidden-operation blocker has no originating diagnostic",
        )
    })?;
    if diagnostic.code != "FORBIDDEN_OPERATION"
        || diagnostic.action != NextAction::RequestPrimaryIntegration
        || diagnostic.role != Some(Role::Primary)
        || diagnostic.thread_id.as_deref() != Some(state.facts.primary_thread_id.as_str())
    {
        return Err(CoordinatorError::operational(
            "TERMINAL_TURN_RETRY_UNSAFE",
            "forbidden-operation recovery is limited to the bound primary integration turn",
        ));
    }
    if state.integration_branch.is_some()
        || state.integration_sha.is_some()
        || state.current_integration_payload.is_some()
        || state.verification_worktree.is_some()
        || !state.test_evidence.is_empty()
    {
        return Err(CoordinatorError::operational(
            "TERMINAL_TURN_RETRY_UNSAFE",
            "forbidden-operation recovery cannot run after integration side effects exist",
        ));
    }
    Ok(Some(NextAction::RequestPrimaryIntegration))
}

fn validate_execution_tool_unavailable_blocker(
    state: &RunState,
    accepted: &AcceptedTurn,
    message: &ProtocolMessage,
) -> Result<(), CoordinatorError> {
    let envelope = &message.envelope;
    if envelope.message_type != MessageType::Blocked
        || envelope.phase != consensus_core::protocol::MessagePhase::Integrate
        || envelope.run_id != state.facts.run_id
        || envelope.round != state.round
        || envelope.plan_revision != state.plan_revision
        || envelope.primary_sha != state.facts.primary_sha
        || envelope.reviewer_sha != state.facts.reviewer_sha
        || envelope.integration_branch.is_some()
        || envelope.integration_sha.is_some()
        || envelope.reason_code.as_deref() != Some("EXECUTION_TOOL_UNAVAILABLE")
    {
        return Err(CoordinatorError::operational(
            "MODEL_RESPONSE_RETRY_UNSAFE",
            "accepted execution-tool blocker envelope does not match the frozen integration action",
        ));
    }
    let payload = message.payload.as_object().ok_or_else(|| {
        CoordinatorError::operational(
            "MODEL_RESPONSE_RETRY_UNSAFE",
            "accepted execution-tool blocker payload is not an object",
        )
    })?;
    let exact_string =
        |key: &str, expected: &str| payload.get(key).and_then(Value::as_str) == Some(expected);
    let false_value = |key: &str| payload.get(key).and_then(Value::as_bool) == Some(false);
    let empty_array = |key: &str| {
        payload
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    };
    let nonempty_array = |key: &str| {
        payload
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(|values| !values.is_empty())
    };
    let target = state
        .target_integration_branch
        .as_deref()
        .unwrap_or_default();
    let approved_plan_hash = state
        .plan_approval_payload
        .as_ref()
        .and_then(|value| value.get("approved_plan_hash"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !exact_string("role", "PRIMARY")
        || !exact_string("request_hash", &accepted.message_hash)
        || payload
            .get("approved_plan_revision")
            .and_then(Value::as_u64)
            != state.plan_revision.map(u64::from)
        || !exact_string("approved_primary_sha", &state.facts.primary_sha)
        || !exact_string("approved_reviewer_sha", &state.facts.reviewer_sha)
        || !exact_string("approved_plan_hash", approved_plan_hash)
        || !exact_string("target_integration_branch", target)
        || !false_value("writes_performed")
        || !false_value("branch_created")
        || !false_value("merge_performed")
        || !empty_array("files_modified")
        || !empty_array("tests_run")
        || !nonempty_array("evidence")
        || !nonempty_array("safety_state")
    {
        return Err(CoordinatorError::operational(
            "MODEL_RESPONSE_RETRY_UNSAFE",
            "accepted execution-tool blocker does not prove a side-effect-free failed integration attempt",
        ));
    }
    Ok(())
}

fn validate_file_change_tool_unavailable_blocker(
    state: &RunState,
    request_hash: &str,
    message: &ProtocolMessage,
) -> Result<String, CoordinatorError> {
    let envelope = &message.envelope;
    if envelope.message_type != MessageType::Blocked
        || envelope.phase != consensus_core::protocol::MessagePhase::Integrate
        || envelope.run_id != state.facts.run_id
        || envelope.round != state.round
        || envelope.plan_revision != state.plan_revision
        || envelope.primary_sha != state.facts.primary_sha
        || envelope.reviewer_sha != state.facts.reviewer_sha
        || envelope.integration_branch.is_some()
        || envelope.integration_sha.is_some()
        || envelope.reason_code.as_deref() != Some("FILE_CHANGE_TOOL_UNAVAILABLE")
    {
        return Err(CoordinatorError::operational(
            "TERMINAL_TURN_RETRY_UNSAFE",
            "file-change blocker envelope does not match the frozen integration action",
        ));
    }
    let payload = message.payload.as_object().ok_or_else(|| {
        CoordinatorError::operational(
            "TERMINAL_TURN_RETRY_UNSAFE",
            "file-change blocker payload is not an object",
        )
    })?;
    let exact_string =
        |key: &str, expected: &str| payload.get(key).and_then(Value::as_str) == Some(expected);
    let target = state
        .target_integration_branch
        .as_deref()
        .unwrap_or_default();
    let approved_plan_hash = state
        .plan_approval_payload
        .as_ref()
        .and_then(|value| value.get("approved_plan_hash"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !exact_string("role", "PRIMARY")
        || !exact_string("request_hash", request_hash)
        || payload
            .get("approved_plan_revision")
            .and_then(Value::as_u64)
            != state.plan_revision.map(u64::from)
        || !exact_string("approved_primary_sha", &state.facts.primary_sha)
        || !exact_string("approved_reviewer_sha", &state.facts.reviewer_sha)
        || !exact_string("approved_plan_hash", approved_plan_hash)
        || !exact_string("resulting_integration_branch", target)
        || !payload
            .get("blocking_condition")
            .and_then(Value::as_str)
            .is_some_and(|condition| {
                condition.contains("bwrap")
                    && condition.contains("Permission denied")
                    && condition.contains("file-change")
            })
    {
        return Err(CoordinatorError::operational(
            "TERMINAL_TURN_RETRY_UNSAFE",
            "file-change blocker does not carry the exact approved identity and bwrap failure evidence",
        ));
    }
    let reported_sha = payload
        .get("resulting_integration_sha")
        .and_then(Value::as_str)
        .filter(|sha| sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| {
            CoordinatorError::operational(
                "TERMINAL_TURN_RETRY_UNSAFE",
                "file-change blocker omits a valid resulting integration SHA",
            )
        })?;
    Ok(reported_sha.to_owned())
}

fn validate_patch_not_authorized_blocker(
    state: &RunState,
    request_hash: &str,
    message: &ProtocolMessage,
) -> Result<String, CoordinatorError> {
    let envelope = &message.envelope;
    if envelope.message_type != MessageType::Blocked
        || envelope.phase != consensus_core::protocol::MessagePhase::Integrate
        || envelope.run_id != state.facts.run_id
        || envelope.round != state.round
        || envelope.plan_revision != state.plan_revision
        || envelope.primary_sha != state.facts.primary_sha
        || envelope.reviewer_sha != state.facts.reviewer_sha
        || envelope.integration_branch.is_some()
        || envelope.integration_sha.is_some()
        || envelope.reason_code.as_deref() != Some("PATCH_NOT_AUTHORIZED")
    {
        return Err(CoordinatorError::operational(
            "TERMINAL_TURN_RETRY_UNSAFE",
            "controlled-patch rejection envelope does not match the frozen integration action",
        ));
    }
    let payload = message.payload.as_object().ok_or_else(|| {
        CoordinatorError::operational(
            "TERMINAL_TURN_RETRY_UNSAFE",
            "controlled-patch rejection payload is not an object",
        )
    })?;
    let exact_string =
        |key: &str, expected: &str| payload.get(key).and_then(Value::as_str) == Some(expected);
    let target = state
        .target_integration_branch
        .as_deref()
        .unwrap_or_default();
    let approved_plan_hash = state
        .plan_approval_payload
        .as_ref()
        .and_then(|value| value.get("approved_plan_hash"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !exact_string("request_hash", request_hash)
        || payload
            .get("approved_plan_revision")
            .and_then(Value::as_u64)
            != state.plan_revision.map(u64::from)
        || !exact_string("approved_primary_sha", &state.facts.primary_sha)
        || !exact_string("approved_reviewer_sha", &state.facts.reviewer_sha)
        || !exact_string("approved_plan_hash", approved_plan_hash)
        || !exact_string("resulting_integration_branch", target)
    {
        return Err(CoordinatorError::operational(
            "TERMINAL_TURN_RETRY_UNSAFE",
            "controlled-patch rejection does not carry the exact machine-checkable approved identity",
        ));
    }
    let reported_sha = payload
        .get("resulting_integration_sha")
        .and_then(Value::as_str)
        .filter(|sha| sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| {
            CoordinatorError::operational(
                "TERMINAL_TURN_RETRY_UNSAFE",
                "controlled-patch rejection omits a valid resulting integration SHA",
            )
        })?;
    Ok(reported_sha.to_owned())
}

fn find_turn<'a>(detail: &'a ThreadDetail, turn_id: &str) -> Option<&'a Value> {
    detail
        .turns
        .iter()
        .find(|turn| turn.get("id").and_then(Value::as_str) == Some(turn_id))
}

fn thread_progress_fingerprint(detail: &ThreadDetail) -> String {
    canonical_json_hash(&json!({
        "status": &detail.summary.status,
        "turns": &detail.turns,
    }))
}

fn final_agent_json(turn: &Value) -> Result<Value, CoordinatorError> {
    let text = final_agent_text(turn)?;
    serde_json::from_str(text.trim())
        .map_err(|error| CoordinatorError::operational("INVALID_RESPONSE", error.to_string()))
}

fn final_agent_text(turn: &Value) -> Result<&str, CoordinatorError> {
    let items = turn.get("items").and_then(Value::as_array).ok_or_else(|| {
        CoordinatorError::operational(
            "HISTORY_UNAVAILABLE",
            "completed turn has no canonical items",
        )
    })?;
    let preferred = items.iter().rev().find(|item| {
        item.get("type").and_then(Value::as_str) == Some("agentMessage")
            && item.get("phase").and_then(Value::as_str) == Some("final_answer")
    });
    let fallback = items
        .iter()
        .rev()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("agentMessage"));
    preferred
        .or(fallback)
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CoordinatorError::operational(
                "INVALID_RESPONSE",
                "completed turn has no final assistant response",
            )
        })
}

fn user_action_event(event: &AppEvent, thread_id: &str, turn_id: &str) -> bool {
    let method = event.method.as_str();
    let is_request = event.id.is_some()
        && (method.ends_with("requestApproval")
            || method == "item/tool/requestUserInput"
            || method == "item/permissions/requestApproval");
    if !is_request {
        return false;
    }
    event_matches_turn(event, thread_id, turn_id)
}

fn event_matches_turn(event: &AppEvent, thread_id: &str, turn_id: &str) -> bool {
    let event_thread = event.params.get("threadId").and_then(Value::as_str);
    let event_turn = event
        .params
        .get("turnId")
        .and_then(Value::as_str)
        .or_else(|| {
            event
                .params
                .get("turn")
                .and_then(|turn| turn.get("id"))
                .and_then(Value::as_str)
        });
    event_thread == Some(thread_id) && event_turn == Some(turn_id)
}

fn role_thread_id(state: &RunState, role: Role) -> &str {
    match role {
        Role::Primary => &state.facts.primary_thread_id,
        Role::Reviewer => &state.facts.reviewer_thread_id,
    }
}

fn turn_execution_policy(state: &RunState, action: NextAction, role: Role) -> TurnExecutionPolicy {
    match action {
        NextAction::RequestPrimaryIntegration => TurnExecutionPolicy::PrimaryIntegration {
            cwd: state.facts.primary_worktree.clone(),
            git_common_dir: state.facts.git_common_dir.clone(),
        },
        NextAction::RequestPrimaryVerification => TurnExecutionPolicy::PrimaryVerification {
            cwd: state
                .verification_worktree
                .clone()
                .expect("verification workspace is prepared before turn creation"),
        },
        _ => {
            let cwd = match role {
                Role::Primary => state.facts.primary_worktree.clone(),
                Role::Reviewer => state.facts.reviewer_worktree.clone(),
            };
            TurnExecutionPolicy::ReadOnly { cwd }
        }
    }
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

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Primary => "PRIMARY",
        Role::Reviewer => "REVIEWER",
    }
}

fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Discover => "DISCOVER",
        Phase::Freeze => "FREEZE",
        Phase::Contract => "CONTRACT",
        Phase::PlanReview => "PLAN_REVIEW",
        Phase::Integrate => "INTEGRATE",
        Phase::Verify => "VERIFY",
        Phase::ResultReview => "RESULT_REVIEW",
        Phase::Accepted => "ACCEPTED",
        Phase::Blocked => "BLOCKED",
        Phase::PausedUserAction => "PAUSED_USER_ACTION",
        Phase::Cancelled => "CANCELLED",
    }
}

fn communication_error(
    operation: &str,
    thread_id: Option<&str>,
    error: AppServerError,
) -> CoordinatorError {
    match error {
        AppServerError::IncompatibleCodex(detail) => {
            CoordinatorError::app_server("INCOMPATIBLE_CODEX", detail, operation, thread_id)
        }
        error => CoordinatorError::app_server(
            "COMMUNICATION_FAILURE",
            error.to_string(),
            operation,
            thread_id,
        ),
    }
}

fn run_diagnostic(state: &RunState, action: NextAction, error: &CoordinatorError) -> RunDiagnostic {
    let role = action_role(action);
    let inferred_thread_id = role.map(|role| role_thread_id(state, role).to_owned());
    RunDiagnostic {
        code: error.code().to_owned(),
        detail: redact_diagnostic(&error.detail()),
        operation: error.operation().map(str::to_owned),
        action,
        role,
        thread_id: error.thread_id().map(str::to_owned).or(inferred_thread_id),
    }
}

fn redact_diagnostic(value: &str) -> String {
    let lowercase = value.to_ascii_lowercase();
    if [
        "authorization:",
        "api_key=",
        "api-key=",
        "secret=",
        "token=",
        "bearer ",
    ]
    .iter()
    .any(|marker| lowercase.contains(marker))
    {
        return "[redacted sensitive coordinator diagnostic]".to_owned();
    }
    let mut redacted = value.to_owned();
    if let Some(home) = std::env::var_os("HOME").and_then(|home| home.into_string().ok()) {
        redacted = redacted.replace(&home, "~");
    }
    redacted.chars().take(2_000).collect()
}

fn verify_reviewer_frozen(
    facts: &RunFacts,
    reviewer: &consensus_core::git::WorktreeSnapshot,
) -> Result<(), SafetyError> {
    if reviewer.worktree != facts.reviewer_worktree
        || reviewer.common_dir != facts.git_common_dir
        || reviewer.head_sha != facts.reviewer_sha
        || !reviewer.clean
    {
        return Err(SafetyError::new(
            "SOURCE_DRIFT",
            "reviewer worktree changed after the run was frozen",
        ));
    }
    match (facts.reviewer_ref.as_deref(), reviewer.source_ref.as_ref()) {
        (Some(expected), Some(actual))
            if actual.name == expected && actual.target_sha == facts.reviewer_sha =>
        {
            Ok(())
        }
        (None, None) => Ok(()),
        _ => Err(SafetyError::new(
            "SOURCE_DRIFT",
            "reviewer source ref identity changed after freeze",
        )),
    }
}

#[cfg(test)]
mod retry_safety_tests {
    use super::*;

    fn consensus_call(tool: &str) -> Value {
        json!({
            "type": "mcpToolCall",
            "pluginId": "worktree-merge-consensus@worktree-merge-consensus",
            "server": "worktreeMergeConsensus",
            "tool": tool,
            "arguments": {},
            "status": "completed",
            "appContext": null
        })
    }

    fn integration_state() -> RunState {
        RunState::new(RunFacts {
            run_id: uuid::Uuid::parse_str("4b230bd8-d870-4ef4-bf20-05a4c61020af").unwrap(),
            primary_thread_id: "primary".into(),
            reviewer_thread_id: "reviewer".into(),
            primary_worktree: PathBuf::from("/repo/primary"),
            reviewer_worktree: PathBuf::from("/repo/reviewer"),
            git_common_dir: PathBuf::from("/repo/.git"),
            primary_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            reviewer_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            primary_ref: Some("refs/heads/primary".into()),
            reviewer_ref: Some("refs/heads/reviewer".into()),
        })
    }

    #[test]
    fn only_exact_local_read_only_consensus_queries_are_retry_safe() {
        for tool in [
            "consensus_list_threads",
            "consensus_list_worktrees",
            "consensus_status",
        ] {
            assert_eq!(
                read_only_consensus_mcp_retry_blocker(&consensus_call(tool)),
                None
            );
        }

        let mut mutating = consensus_call("consensus_resume");
        assert!(
            read_only_consensus_mcp_retry_blocker(&mutating)
                .unwrap()
                .contains("not a read-only")
        );

        mutating["tool"] = json!("consensus_list_worktrees");
        mutating["appContext"] = json!({"connectorId": "external"});
        assert_eq!(
            read_only_consensus_mcp_retry_blocker(&mutating).as_deref(),
            Some("MCP tool call carries external app context")
        );

        let mut foreign = consensus_call("consensus_list_worktrees");
        foreign["pluginId"] = json!("other-plugin@marketplace");
        assert_eq!(
            read_only_consensus_mcp_retry_blocker(&foreign).as_deref(),
            Some("MCP tool call is not owned by the consensus plugin")
        );
    }

    #[test]
    fn participant_patch_integration_history_accepts_only_the_exact_request_bound_tool() {
        let state = integration_state();
        let request_hash = "request-hash";
        let mut call = json!({
            "type": "mcpToolCall",
            "pluginId": "worktree-merge-consensus@worktree-merge-consensus",
            "server": PARTICIPANT_MCP_SERVER,
            "tool": "consensus_apply_patch",
            "arguments": {
                "run_id": state.facts.run_id.to_string(),
                "request_hash": request_hash,
                "patch": "diff --git a/src/lib.rs b/src/lib.rs"
            },
            "status": "completed",
            "appContext": null
        });
        assert_eq!(
            integration_patch_mcp_blocker(&state, &call, request_hash),
            None
        );

        call["server"] = json!("worktreeMergeConsensus");
        assert!(
            integration_patch_mcp_blocker(&state, &call, request_hash)
                .unwrap()
                .contains("outside")
        );
        call["server"] = json!(PARTICIPANT_MCP_SERVER);
        call["tool"] = json!("consensus_resume");
        assert!(
            integration_patch_mcp_blocker(&state, &call, request_hash)
                .unwrap()
                .contains("outside")
        );
        call["tool"] = json!("consensus_apply_patch");
        call["arguments"]["request_hash"] = json!("other-request");
        assert!(
            integration_patch_mcp_blocker(&state, &call, request_hash)
                .unwrap()
                .contains("arguments")
        );
    }

    #[test]
    fn completed_integration_recovery_binds_the_exact_successful_patch_hash() {
        let state = integration_state();
        let request_hash = "request-hash";
        let call = |id: &str, status: &str, patch: &str| {
            json!({
                "id": id,
                "type": "mcpToolCall",
                "pluginId": "worktree-merge-consensus@worktree-merge-consensus",
                "server": "worktreeMergeConsensus",
                "tool": "consensus_apply_patch",
                "arguments": {
                    "run_id": state.facts.run_id.to_string(),
                    "request_hash": request_hash,
                    "patch": patch,
                },
                "status": status,
                "appContext": null,
            })
        };
        let successful_patch = "diff --git a/a b/a\n--- a/a\n+++ b/a\n";
        let turn = json!({
            "items": [
                {"id": "user", "type": "userMessage"},
                call("failed", "failed", "*** Begin Patch\n*** End Patch"),
                call("completed", "completed", successful_patch),
                {"id": "agent", "type": "agentMessage", "text": "invalid legacy response"}
            ]
        });
        let successful_hash = canonical_json_hash(&json!({"patch": successful_patch}));

        assert_eq!(
            recoverable_integration_turn_blocker(
                &state,
                &turn,
                request_hash,
                &successful_hash,
                true,
            ),
            None
        );
        assert!(
            recoverable_integration_turn_blocker(
                &state,
                &turn,
                request_hash,
                &canonical_json_hash(&json!({"patch": "different"})),
                true,
            )
            .unwrap()
            .contains("SQLite success record")
        );
    }

    #[test]
    fn only_the_exact_context_compaction_marker_is_retry_safe() {
        assert_eq!(
            context_compaction_retry_blocker(&json!({
                "id": "compact-1",
                "type": "contextCompaction"
            })),
            None
        );

        for malformed in [
            json!({"id": "", "type": "contextCompaction"}),
            json!({"type": "contextCompaction"}),
            json!({"id": "compact-1", "type": "contextCompaction", "status": "completed"}),
            json!({"id": "compact-1", "type": "compaction"}),
        ] {
            assert!(context_compaction_retry_blocker(&malformed).is_some());
        }
    }

    #[test]
    fn verification_failure_output_is_utf8_safe_and_strictly_bounded() {
        assert_eq!(bounded_verification_output("short output"), "short output");

        let output = "界".repeat(MAX_VERIFICATION_FAILURE_OUTPUT_BYTES);
        let bounded = bounded_verification_output(&output);

        assert!(bounded.len() <= MAX_VERIFICATION_FAILURE_OUTPUT_BYTES);
        assert!(bounded.starts_with(VERIFICATION_OUTPUT_TRUNCATION_MARKER));
        assert!(bounded.ends_with('界'));
    }
}
