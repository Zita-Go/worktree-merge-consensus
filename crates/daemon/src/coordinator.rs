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
    protocol::{MessageType, ProtocolMessage, output_schema, validate_message},
    state::{NextAction, Phase, Role, RunFacts, RunState, RunStatus, StateError, TestEvidence},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::policy::{ApprovalDecision, decide_command_approval, validate_test_command};
use crate::store::{SqliteRunStore, StoreError};

const MAX_DRIVER_STEPS: usize = 128;

struct CompletedTurn {
    response: Value,
    turn: Value,
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
        fs::canonicalize(path).map_err(|error| {
            SafetyError::new(
                "WORKTREE_UNAVAILABLE",
                format!(
                    "{role} frozen worktree {} is unavailable: {error}",
                    path.display()
                ),
            )
        })?;
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
    Operational { code: String, detail: String },
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
                if error.code() == "COMMUNICATION_FAILURE" {
                    state.pause("COMMUNICATION_FAILURE")?;
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
        if state.next_action == NextAction::RequestPrimaryIntegration {
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
        let recovered_turn = pending
            .turn_id
            .clone()
            .or_else(|| find_turn_by_request_hash(&current_detail, &request_hash));
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
                    output_schema(),
                    &turn_execution_policy(state, action, role),
                )
                .await
                .map_err(communication_error)?;
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
                    .map_err(communication_error)?;
                if decision == ApprovalDecision::Cancel {
                    return Err(CoordinatorError::operational(
                        "FORBIDDEN_OPERATION",
                        "the task requested a command outside the frozen integration execution policy",
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
                    .map_err(communication_error)?;
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
                    .map_err(communication_error)?;
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
        Err(communication_error(last_error.unwrap_or_else(|| {
            AppServerError::InvalidResponse("thread read failed without an error".into())
        })))
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
            .filter(|item| item.get("command").and_then(Value::as_str) == Some(required.as_str()))
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

fn find_turn_by_request_hash(detail: &ThreadDetail, request_hash: &str) -> Option<String> {
    let marker = format!("\"request_hash\":\"{request_hash}\"");
    detail.turns.iter().rev().find_map(|turn| {
        let matched = turn
            .get("items")
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
            .any(|text| text.contains(&marker));
        matched
            .then(|| turn.get("id").and_then(Value::as_str).map(str::to_owned))
            .flatten()
    })
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

fn communication_error(error: AppServerError) -> CoordinatorError {
    match error {
        AppServerError::IncompatibleCodex(detail) => {
            CoordinatorError::operational("INCOMPATIBLE_CODEX", detail)
        }
        error => CoordinatorError::operational("COMMUNICATION_FAILURE", error.to_string()),
    }
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
