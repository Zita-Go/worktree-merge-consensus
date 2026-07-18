use std::{path::PathBuf, sync::Arc, time::Duration};

use app_server_client::{AppEvent, AppServer, AppServerError, ThreadDetail};
use consensus_core::{
    canonical_json_hash,
    git::{
        GitInspector, GitSafetyError, normalize_branch_name, verify_frozen_sources,
        verify_integration_result, verify_same_repository,
    },
    prompts::{PromptError, build_turn_prompt},
    protocol::{MessagePhase, MessageType, ProtocolMessage, output_schema, validate_message},
    state::{NextAction, Phase, Role, RunFacts, RunState, RunStatus, StateError},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::store::{SqliteRunStore, StoreError};

const MAX_DRIVER_STEPS: usize = 128;

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
    fn verify_thread_worktree(
        &self,
        _facts: &RunFacts,
        _role: Role,
        _cwd: &std::path::Path,
    ) -> Result<(), SafetyError> {
        Ok(())
    }

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
}

#[derive(Debug, Clone, Default)]
pub struct GitRepositorySafety {
    inspector: GitInspector,
}

impl RepositorySafety for GitRepositorySafety {
    fn verify_thread_worktree(
        &self,
        facts: &RunFacts,
        role: Role,
        cwd: &std::path::Path,
    ) -> Result<(), SafetyError> {
        let snapshot = self.inspector.inspect_worktree(cwd)?;
        let expected = match role {
            Role::Primary => &facts.primary_worktree,
            Role::Reviewer => &facts.reviewer_worktree,
        };
        if snapshot.worktree != *expected || snapshot.common_dir != facts.git_common_dir {
            return Err(SafetyError::new(
                "SOURCE_DRIFT",
                "task working directory no longer resolves to its frozen worktree",
            ));
        }
        Ok(())
    }

    fn verify_frozen(&self, facts: &RunFacts) -> Result<(), SafetyError> {
        let primary = self.inspector.inspect_worktree(&facts.primary_worktree)?;
        let reviewer = self.inspector.inspect_worktree(&facts.reviewer_worktree)?;
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
        let primary = self.inspector.inspect_worktree(&facts.primary_worktree)?;
        let reviewer = self.inspector.inspect_worktree(&facts.reviewer_worktree)?;
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
        let reviewer = self.inspector.inspect_worktree(&facts.reviewer_worktree)?;
        verify_reviewer_frozen(facts, &reviewer)?;
        let integration =
            self.inspector
                .inspect_integration(&facts.primary_worktree, facts, changed_files)?;
        verify_integration_result(facts, &integration, branch, sha).map_err(Into::into)
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
        let role = action_role(action).ok_or_else(|| {
            CoordinatorError::operational("INVALID_STATE", "action has no task role")
        })?;
        let thread_id = role_thread_id(state, role).to_owned();
        let detail = self.wait_until_idle(state, role, &thread_id).await?;
        self.verify_thread_identity(state, role, &detail)?;

        let history = self.load_history(state).await?;
        self.revalidate_before_action(state, action, &history)?;
        let payload = build_action_payload(state, action, &history)?;
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
                .start_turn(&thread_id, &prompt, output_schema())
                .await
                .map_err(communication_error)?;
            self.store
                .record_turn_started(&run_id, &request_hash, &thread_id, &turn.id)?;
            turn.id
        };

        let response = self
            .wait_for_turn_response(state, &thread_id, &turn_id)
            .await?;
        let message = validate_message(response.clone()).map_err(|error| {
            CoordinatorError::operational("INVALID_RESPONSE", error.to_string())
        })?;
        self.verify_message_evidence(state, &message)?;
        let mut next = state.clone();
        next.apply_message(message)?;
        let response_hash = canonical_json_hash(&response);
        self.store
            .accept_response_and_advance(&run_id, &response_hash, &next)?;
        *state = next;
        Ok(())
    }

    async fn revalidate_and_accept(&self, state: &mut RunState) -> Result<(), CoordinatorError> {
        let history = self.load_history(state).await?;
        let integration = history.latest_integration(state).ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "current integration evidence is absent from task history",
            )
        })?;
        let changed_files = changed_files(&integration.message.payload)?;
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
            let history = self.load_history(state).await?;
            let integration = history.latest_integration(state).ok_or_else(|| {
                CoordinatorError::operational(
                    "HISTORY_UNAVAILABLE",
                    "current integration evidence is absent from task history",
                )
            })?;
            self.safety.verify_integration(
                &state.facts,
                branch,
                sha,
                &changed_files(&integration.message.payload)?,
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
        history: &RunHistory,
    ) -> Result<(), CoordinatorError> {
        if let (Some(branch), Some(sha)) = (
            state.integration_branch.as_deref(),
            state.integration_sha.as_deref(),
        ) {
            let integration = history.latest_integration(state).ok_or_else(|| {
                CoordinatorError::operational(
                    "HISTORY_UNAVAILABLE",
                    "current integration evidence is absent from task history",
                )
            })?;
            self.safety.verify_integration(
                &state.facts,
                branch,
                sha,
                &changed_files(&integration.message.payload)?,
            )?;
        } else {
            self.safety.verify_frozen(&state.facts)?;
            if action == NextAction::RequestPrimaryIntegration {
                let branch = state.target_integration_branch.as_deref().ok_or_else(|| {
                    CoordinatorError::operational(
                        "INVALID_STATE",
                        "target integration branch is missing",
                    )
                })?;
                self.safety.verify_branch_absent(&state.facts, branch)?;
            }
        }
        Ok(())
    }

    fn verify_message_evidence(
        &self,
        state: &RunState,
        message: &ProtocolMessage,
    ) -> Result<(), CoordinatorError> {
        if message.envelope.message_type != MessageType::IntegrationReady {
            return Ok(());
        }
        verify_test_evidence(&state.required_test_commands, &message.payload)?;
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
    ) -> Result<Value, CoordinatorError> {
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
                    Some("completed") => return final_agent_json(turn),
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

    async fn load_history(&self, state: &RunState) -> Result<RunHistory, CoordinatorError> {
        let primary = self
            .read_thread_with_retry(&state.facts.primary_thread_id)
            .await?;
        let reviewer = self
            .read_thread_with_retry(&state.facts.reviewer_thread_id)
            .await?;
        self.verify_thread_identity(state, Role::Primary, &primary)?;
        self.verify_thread_identity(state, Role::Reviewer, &reviewer)?;
        Ok(RunHistory::from_threads(state, [&primary, &reviewer]))
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
        self.safety
            .verify_thread_worktree(&state.facts, role, detail.summary.cwd.as_path())?;
        Ok(())
    }

    fn required_run(&self, run_id: &str) -> Result<RunState, CoordinatorError> {
        self.store
            .load_run(run_id)?
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_owned()).into())
    }
}

#[derive(Clone)]
struct HistoricalMessage {
    message: ProtocolMessage,
}

#[derive(Default)]
struct RunHistory {
    messages: Vec<HistoricalMessage>,
}

impl RunHistory {
    fn from_threads(state: &RunState, threads: [&ThreadDetail; 2]) -> Self {
        let mut messages = Vec::new();
        for thread in threads {
            for turn in &thread.turns {
                for item in turn
                    .get("items")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if item.get("type").and_then(Value::as_str) != Some("agentMessage") {
                        continue;
                    }
                    let Some(text) = item.get("text").and_then(Value::as_str) else {
                        continue;
                    };
                    let Ok(value) = serde_json::from_str::<Value>(text.trim()) else {
                        continue;
                    };
                    let Ok(message) = validate_message(value) else {
                        continue;
                    };
                    if message.envelope.run_id == state.facts.run_id
                        && message.envelope.primary_sha == state.facts.primary_sha
                        && message.envelope.reviewer_sha == state.facts.reviewer_sha
                    {
                        messages.push(HistoricalMessage { message });
                    }
                }
            }
        }
        Self { messages }
    }

    fn latest(&self, message_type: MessageType) -> Option<&HistoricalMessage> {
        self.messages
            .iter()
            .rev()
            .find(|entry| entry.message.envelope.message_type == message_type)
    }

    fn latest_contract(&self, role: &str) -> Option<&Value> {
        self.messages.iter().rev().find_map(|entry| {
            (entry.message.envelope.message_type == MessageType::ContractReady
                && entry.message.payload.get("role").and_then(Value::as_str) == Some(role))
            .then(|| entry.message.payload.get("contract"))
            .flatten()
        })
    }

    fn latest_plan(&self, state: &RunState) -> Option<&HistoricalMessage> {
        self.messages.iter().rev().find(|entry| {
            entry.message.envelope.message_type == MessageType::PlanReady
                && entry.message.envelope.plan_revision == state.plan_revision
        })
    }

    fn latest_integration(&self, state: &RunState) -> Option<&HistoricalMessage> {
        self.messages.iter().rev().find(|entry| {
            entry.message.envelope.message_type == MessageType::IntegrationReady
                && entry.message.envelope.integration_sha.as_deref()
                    == state.integration_sha.as_deref()
        })
    }

    fn latest_changes(&self, phase: MessagePhase) -> Option<&HistoricalMessage> {
        self.messages.iter().rev().find(|entry| {
            entry.message.envelope.message_type == MessageType::ChangesRequired
                && entry.message.envelope.phase == phase
        })
    }
}

fn build_action_payload(
    state: &RunState,
    action: NextAction,
    history: &RunHistory,
) -> Result<Value, CoordinatorError> {
    let primary_contract = || {
        history.latest_contract("PRIMARY").cloned().ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "primary contract is absent from canonical task history",
            )
        })
    };
    let reviewer_contract = || {
        history.latest_contract("REVIEWER").cloned().ok_or_else(|| {
            CoordinatorError::operational(
                "HISTORY_UNAVAILABLE",
                "reviewer contract is absent from canonical task history",
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
            "previous_plan": history.latest(MessageType::PlanReady).map(|entry| entry.message.payload.clone()),
            "review_feedback": history.latest_changes(MessagePhase::PlanReview).map(|entry| entry.message.payload.clone()),
            "target_integration_branch": state.target_integration_branch,
            "required_test_commands": state.required_test_commands,
        })),
        NextAction::RequestReviewerPlanVerdict => {
            let plan = history.latest_plan(state).ok_or_else(|| {
                CoordinatorError::operational(
                    "HISTORY_UNAVAILABLE",
                    "current primary plan is absent from canonical task history",
                )
            })?;
            Ok(plan.message.payload.clone())
        }
        NextAction::RequestPrimaryIntegration => {
            let plan = history.latest_plan(state).ok_or_else(|| {
                CoordinatorError::operational(
                    "HISTORY_UNAVAILABLE",
                    "approved primary plan is absent from canonical task history",
                )
            })?;
            let approval = history.latest(MessageType::ApprovedPlan).ok_or_else(|| {
                CoordinatorError::operational(
                    "HISTORY_UNAVAILABLE",
                    "plan approval is absent from canonical task history",
                )
            })?;
            Ok(json!({
                "primary_contract": primary_contract()?,
                "reviewer_contract": reviewer_contract()?,
                "approved_plan": plan.message.payload.get("plan").cloned().unwrap_or(Value::Null),
                "coverage_matrix": plan.message.payload.get("coverage_matrix").cloned().unwrap_or(Value::Null),
                "approval": approval.message.payload,
                "target_integration_branch": state.target_integration_branch,
                "required_test_commands": state.required_test_commands,
                "previous_integration_sha": state.integration_sha,
                "result_feedback": history.latest_changes(MessagePhase::ResultReview).map(|entry| entry.message.payload.clone()),
            }))
        }
        NextAction::RequestReviewerResultVerdict => {
            let plan = history.latest_plan(state).ok_or_else(|| {
                CoordinatorError::operational(
                    "HISTORY_UNAVAILABLE",
                    "approved primary plan is absent from canonical task history",
                )
            })?;
            let integration = history.latest_integration(state).ok_or_else(|| {
                CoordinatorError::operational(
                    "HISTORY_UNAVAILABLE",
                    "current integration evidence is absent from canonical task history",
                )
            })?;
            Ok(json!({
                "primary_contract": primary_contract()?,
                "reviewer_contract": reviewer_contract()?,
                "approved_plan": plan.message.payload.get("plan").cloned().unwrap_or(Value::Null),
                "coverage_matrix": plan.message.payload.get("coverage_matrix").cloned().unwrap_or(Value::Null),
                "integration_evidence": integration.message.payload.get("integration_evidence").cloned().unwrap_or(Value::Null),
                "test_evidence": integration.message.payload.get("test_evidence").cloned().unwrap_or(Value::Null),
                "changed_files": integration.message.payload.get("changed_files").cloned().unwrap_or(Value::Null),
                "integration_branch": state.integration_branch,
                "integration_sha": state.integration_sha,
            }))
        }
        NextAction::RevalidateAndAccept | NextAction::WaitForUser | NextAction::Stop => Err(
            CoordinatorError::operational("INVALID_STATE", "action does not start a task turn"),
        ),
    }
}

fn verify_test_evidence(
    required_commands: &[String],
    payload: &Value,
) -> Result<(), CoordinatorError> {
    let evidence = payload
        .get("test_evidence")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CoordinatorError::operational(
                "INVALID_RESPONSE",
                "INTEGRATION_READY payload requires test_evidence",
            )
        })?;
    for item in evidence {
        let passed = item.get("exit_code").and_then(Value::as_i64) == Some(0)
            || item.get("passed").and_then(Value::as_bool) == Some(true)
            || item.get("status").and_then(Value::as_str) == Some("passed");
        if !passed {
            return Err(CoordinatorError::operational(
                "TEST_FAILURE",
                "integration test evidence contains a failed or indeterminate command",
            ));
        }
    }
    for required in required_commands {
        let passed = evidence.iter().any(|item| {
            item.get("command").and_then(Value::as_str) == Some(required.as_str())
                && (item.get("exit_code").and_then(Value::as_i64) == Some(0)
                    || item.get("passed").and_then(Value::as_bool) == Some(true)
                    || item.get("status").and_then(Value::as_str) == Some("passed"))
        });
        if !passed {
            return Err(CoordinatorError::operational(
                "TEST_FAILURE",
                format!("required test command lacks passing evidence: {required}"),
            ));
        }
    }
    Ok(())
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
    let event_thread = event.params.get("threadId").and_then(Value::as_str);
    let event_turn = event.params.get("turnId").and_then(Value::as_str);
    event_thread.is_none_or(|value| value == thread_id)
        && event_turn.is_none_or(|value| value == turn_id)
}

fn role_thread_id(state: &RunState, role: Role) -> &str {
    match role {
        Role::Primary => &state.facts.primary_thread_id,
        Role::Reviewer => &state.facts.reviewer_thread_id,
    }
}

fn action_role(action: NextAction) -> Option<Role> {
    match action {
        NextAction::RequestPrimaryContract
        | NextAction::RequestPrimaryPlan
        | NextAction::RequestPrimaryIntegration => Some(Role::Primary),
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
    CoordinatorError::operational("COMMUNICATION_FAILURE", error.to_string())
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
