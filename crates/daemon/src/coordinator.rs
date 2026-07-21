use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use app_server_client::{AppEvent, AppServer, AppServerError, ThreadDetail, TurnExecutionPolicy};
use consensus_core::{
    canonical_json_hash,
    git::{
        GitInspector, GitSafetyError, WorktreeSnapshot, normalize_branch_name,
        verify_frozen_sources, verify_integration_result, verify_reported_changed_files,
        verify_same_repository,
    },
    prompts::{PromptError, build_turn_prompt},
    protocol::{MessageType, ProtocolMessage, validate_message},
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
    is_retry_safe_read_only_integration_command, normalize_app_server_command,
    validate_test_command,
};
use crate::store::{AcceptedTurn, SqliteRunStore, StoreError};

const MAX_DRIVER_STEPS: usize = 128;

struct CompletedTurn {
    response: Value,
    turn: Value,
}

struct RetryableCompletedTurn {
    message_hash: String,
    thread_id: String,
    turn_id: String,
    observed_status: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StartRequest {
    pub integration_branch: Option<String>,
    pub test_commands: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatorOptions {
    pub wait_timeout: Duration,
    pub poll_interval: Duration,
    pub communication_attempts: usize,
}

impl Default for CoordinatorOptions {
    fn default() -> Self {
        Self {
            wait_timeout: Duration::from_secs(300),
            poll_interval: Duration::from_millis(500),
            communication_attempts: 3,
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

    fn verify_integration(
        &self,
        facts: &RunFacts,
        branch: &str,
        sha: &str,
        changed_files: &[PathBuf],
    ) -> Result<(), SafetyError>;

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
}

impl<A, R> Clone for Coordinator<A, R> {
    fn clone(&self) -> Self {
        Self {
            app: Arc::clone(&self.app),
            store: self.store.clone(),
            safety: Arc::clone(&self.safety),
            options: self.options.clone(),
            driver_lock: Arc::clone(&self.driver_lock),
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
        }
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
        let retry_invalid_response_action = invalid_response_retry_action(&state)?;
        let retry_execution_tool_action = execution_tool_unavailable_retry_action(&state)?;
        let retry_forbidden_operation_action = forbidden_operation_retry_action(&state)?;
        let retry_completed_response_action =
            retry_invalid_test_action.or(retry_invalid_response_action);
        let effective_action = retry_execution_tool_action
            .or(retry_forbidden_operation_action)
            .or(retry_completed_response_action)
            .unwrap_or(state.next_action);
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
            self.prepare_terminal_turn_retry(&state).await?;
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
        let resumed = self.resume_thread_with_retry(&thread_id).await?;
        self.verify_thread_identity(state, role, &resumed)?;
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
        let mut prompt = build_turn_prompt(role, action, state, &payload)?;
        prompt.push_str("\nCoordinator delivery identity for crash recovery:\n```json\n");
        prompt.push_str(
            &serde_json::to_string(&json!({"request_hash": request_hash})).map_err(|error| {
                CoordinatorError::operational("SERIALIZATION_FAILURE", error.to_string())
            })?,
        );
        prompt.push_str("\n```\n");

        let run_id = state.facts.run_id.to_string();
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
        let mut message = validate_message(completed.response).map_err(|error| {
            CoordinatorError::operational("INVALID_RESPONSE", error.to_string())
        })?;
        self.verify_message_evidence(state, action, &mut message, &completed.turn, &turn_id)?;
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

    fn verify_message_evidence(
        &self,
        state: &RunState,
        action: NextAction,
        message: &mut ProtocolMessage,
        turn: &Value,
        turn_id: &str,
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
                verify_integration_command_items(state, turn)?;
            }
            NextAction::RequestPrimaryVerification => {
                let authoritative = authoritative_test_evidence(state, turn, turn_id)?;
                verify_reported_test_evidence(&authoritative, &message.payload)?;
                let mut canonical = current_integration_payload(state)?.clone();
                canonical
                    .as_object_mut()
                    .ok_or_else(|| {
                        CoordinatorError::operational(
                            "INVALID_STATE",
                            "canonical integration payload is not an object",
                        )
                    })?
                    .insert(
                        "test_evidence".into(),
                        serde_json::to_value(authoritative).map_err(|error| {
                            CoordinatorError::operational(
                                "SERIALIZATION_FAILURE",
                                error.to_string(),
                            )
                        })?,
                    );
                message.payload = canonical;
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
        let deadline = tokio::time::Instant::now() + self.options.wait_timeout;
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
            if tokio::time::Instant::now() >= deadline {
                return Err(CoordinatorError::operational(
                    "COMMUNICATION_FAILURE",
                    "task remained active beyond the bounded wait",
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
        let deadline = tokio::time::Instant::now() + self.options.wait_timeout;
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
                match turn.get("status").and_then(Value::as_str) {
                    Some("completed") => {
                        return Ok(CompletedTurn {
                            response: final_agent_json(turn)?,
                            turn: turn.clone(),
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
            if tokio::time::Instant::now() >= deadline {
                return Err(CoordinatorError::operational(
                    "COMMUNICATION_FAILURE",
                    "task turn did not complete within the bounded wait",
                ));
            }
            match tokio::time::timeout(self.options.poll_interval, self.app.next_event()).await {
                Ok(Some(event))
                    if event_matches_turn(&event, thread_id, turn_id)
                        && self.handle_execution_request(state, &event).await? =>
                {
                    continue;
                }
                Ok(Some(event)) if user_action_event(&event, thread_id, turn_id) => {
                    state.pause("PERMISSION_REQUIRED")?;
                    self.store.save_state(state)?;
                    return Err(CoordinatorError::operational(
                        "PERMISSION_REQUIRED",
                        "task turn is waiting for user approval or input",
                    ));
                }
                Ok(None) => tokio::time::sleep(self.options.poll_interval).await,
                _ => {}
            }
        }
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
    ) -> Result<ThreadDetail, CoordinatorError> {
        let attempts = self.options.communication_attempts.max(1);
        let mut last_error = None;
        for attempt in 0..attempts {
            match self.app.resume_thread(thread_id).await {
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

fn verify_reported_test_evidence(
    authoritative: &[TestEvidence],
    payload: &Value,
) -> Result<(), CoordinatorError> {
    let evidence = payload
        .get("test_evidence")
        .and_then(Value::as_array)
        .filter(|evidence| !evidence.is_empty())
        .ok_or_else(|| {
            CoordinatorError::operational(
                "TEST_FAILURE",
                "verification response requires nonempty test_evidence",
            )
        })?;
    if evidence.len() != authoritative.len() {
        return Err(CoordinatorError::operational(
            "TEST_FAILURE",
            "reported test evidence count does not match commandExecution items",
        ));
    }
    for item in evidence {
        let command = item
            .get("command")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|command| !command.is_empty());
        if command.is_none() || item.get("exit_code").and_then(Value::as_i64) != Some(0) {
            return Err(CoordinatorError::operational(
                "TEST_FAILURE",
                "each test evidence entry requires a nonempty command and exact exit_code 0",
            ));
        }
    }
    for actual in authoritative {
        let passed = evidence.iter().any(|item| {
            item.get("command").and_then(Value::as_str) == Some(actual.command.as_str())
                && item.get("exit_code").and_then(Value::as_i64) == Some(actual.exit_code)
        });
        if !passed {
            return Err(CoordinatorError::operational(
                "TEST_FAILURE",
                format!(
                    "reported test evidence does not match commandExecution: {}",
                    actual.command
                ),
            ));
        }
    }
    Ok(())
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

fn verify_integration_command_items(
    state: &RunState,
    turn: &Value,
) -> Result<(), CoordinatorError> {
    for item in command_execution_items(turn)? {
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

fn authoritative_test_evidence(
    state: &RunState,
    turn: &Value,
    turn_id: &str,
) -> Result<Vec<TestEvidence>, CoordinatorError> {
    let expected_cwd = state.verification_worktree.as_ref().ok_or_else(|| {
        CoordinatorError::operational("INVALID_STATE", "verification workspace is not persisted")
    })?;
    let items = command_execution_items(turn)?;
    if items.len() != state.required_test_commands.len() {
        return Err(CoordinatorError::operational(
            "TEST_FAILURE",
            "verification must execute each frozen command exactly once and no other command",
        ));
    }
    let mut evidence = Vec::with_capacity(items.len());
    for required in &state.required_test_commands {
        let matches = items
            .iter()
            .filter(|item| {
                item.get("command")
                    .and_then(Value::as_str)
                    .and_then(normalize_app_server_command)
                    .is_some_and(|command| command == *required)
            })
            .copied()
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(CoordinatorError::operational(
                "TEST_FAILURE",
                format!("frozen test was not executed exactly once: {required}"),
            ));
        }
        let item = matches[0];
        let item_id = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| {
                CoordinatorError::operational("INVALID_RESPONSE", "commandExecution item omits id")
            })?;
        if item.get("cwd").and_then(Value::as_str) != expected_cwd.to_str() {
            return Err(CoordinatorError::operational(
                "FORBIDDEN_OPERATION",
                format!("test executed outside the isolated clone: {required}"),
            ));
        }
        if item.get("status").and_then(Value::as_str) != Some("completed")
            || item.get("exitCode").and_then(Value::as_i64) != Some(0)
        {
            return Err(CoordinatorError::operational(
                "TEST_FAILURE",
                format!("frozen test did not complete with exit code 0: {required}"),
            ));
        }
        if item
            .get("source")
            .and_then(Value::as_str)
            .is_some_and(|source| source != "agent")
        {
            return Err(CoordinatorError::operational(
                "INVALID_RESPONSE",
                "test commandExecution source is not the agent turn",
            ));
        }
        evidence.push(TestEvidence {
            command: required.clone(),
            exit_code: 0,
            turn_id: turn_id.to_owned(),
            item_id: item_id.to_owned(),
            cwd: expected_cwd.clone(),
        });
    }
    Ok(evidence)
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
        if !matches!(item_type, "userMessage" | "agentMessage" | "reasoning") {
            return Some(format!(
                "canonical item type {item_type} may have side effects"
            ));
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

fn find_turn<'a>(detail: &'a ThreadDetail, turn_id: &str) -> Option<&'a Value> {
    detail
        .turns
        .iter()
        .find(|turn| turn.get("id").and_then(Value::as_str) == Some(turn_id))
}

fn final_agent_json(turn: &Value) -> Result<Value, CoordinatorError> {
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
    let text = preferred
        .or(fallback)
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CoordinatorError::operational(
                "INVALID_RESPONSE",
                "completed turn has no final assistant JSON",
            )
        })?;
    serde_json::from_str(text.trim())
        .map_err(|error| CoordinatorError::operational("INVALID_RESPONSE", error.to_string()))
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
    let event_turn = event.params.get("turnId").and_then(Value::as_str);
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
}
