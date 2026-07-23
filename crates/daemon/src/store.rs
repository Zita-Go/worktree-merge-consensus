use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use consensus_core::state::{NextAction, Phase, Role, RunState, RunStatus};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{PrimaryBindingMode, PrimaryParticipantBinding};

pub const LEGACY_PARTICIPANT_CAPABILITY_GENERATION: &str = "participant-mcp-v1";
pub const PARTICIPANT_CAPABILITY_GENERATION: &str = "participant-mcp-v2";

#[derive(Clone)]
pub struct SqliteRunStore {
    connection: Arc<Mutex<Connection>>,
    state_root: Arc<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingSend {
    pub run_id: String,
    pub role: String,
    pub phase: String,
    pub round: u32,
    pub message_hash: String,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub full_prompt: Option<String>,
    pub capability_generation: Option<String>,
    pub participant_binding_generation: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedTurn {
    pub run_id: String,
    pub role: String,
    pub phase: String,
    pub round: u32,
    pub message_hash: String,
    pub response_hash: String,
    pub thread_id: String,
    pub turn_id: String,
    pub capability_generation: Option<String>,
    pub participant_binding_generation: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuccessfulPatchRecord {
    pub patch_hash: String,
    pub source_primary_thread_id: Option<String>,
    pub effective_primary_thread_id: Option<String>,
    pub participant_binding_generation: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedTurnAttempt {
    pub turn_id: String,
    pub terminal_status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationCommandRecord {
    pub run_id: String,
    pub message_hash: String,
    pub turn_id: String,
    pub item_id: String,
    pub command_index: u32,
    pub command: String,
    pub cwd: PathBuf,
    pub exit_code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationCommandClaim {
    Execute(VerificationCommandRecord),
    Reuse(VerificationCommandRecord),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TurnEventEvidence {
    pub completed_turn: Value,
    pub completed_items: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V025VerificationCompletionCollision {
    pub blocked_state: RunState,
    pub pending: PendingSend,
    pub stale_turn_id: String,
    turn_record_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSummary {
    pub run_id: String,
    pub status: String,
    pub phase: String,
    pub round: u32,
    pub integration_branch: Option<String>,
    pub integration_sha: Option<String>,
    pub reason_code: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("ACTIVE_RUN_EXISTS: {0}")]
    ActiveRunExists(String),
    #[error("RUN_NOT_FOUND: {0}")]
    RunNotFound(String),
    #[error("PENDING_SEND_NOT_FOUND: {0}")]
    PendingSendNotFound(String),
    #[error("TERMINAL_TURN_NOT_RETRYABLE: {0}")]
    TerminalTurnNotRetryable(String),
    #[error("VERIFICATION_EXECUTION_UNCERTAIN: {0}")]
    VerificationExecutionUncertain(String),
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("state serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("INCOMPATIBLE_STATE: {0}")]
    IncompatibleState(String),
    #[error("state storage I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("state store lock is poisoned")]
    Poisoned,
}

impl StoreError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ActiveRunExists(_) => "ACTIVE_RUN_EXISTS",
            Self::RunNotFound(_) => "RUN_NOT_FOUND",
            Self::PendingSendNotFound(_) => "PENDING_SEND_NOT_FOUND",
            Self::TerminalTurnNotRetryable(_) => "TERMINAL_TURN_NOT_RETRYABLE",
            Self::VerificationExecutionUncertain(_) => "VERIFICATION_EXECUTION_UNCERTAIN",
            Self::Database(_) => "DATABASE_ERROR",
            Self::Serialization(_) => "SERIALIZATION_ERROR",
            Self::IncompatibleState(_) => "INCOMPATIBLE_STATE",
            Self::Io(_) => "IO_ERROR",
            Self::Poisoned => "LOCK_POISONED",
        }
    }
}

impl SqliteRunStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        set_private_directory_permissions(parent)?;
        let state_root = fs::canonicalize(parent)?;
        let connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;",
        )?;
        migrate(&connection)?;
        set_private_file_permissions(path)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            state_root: Arc::new(state_root),
        })
    }

    pub fn verification_path(&self, run_id: &str, integration_sha: &str) -> PathBuf {
        self.state_root
            .join("verification")
            .join(format!("{run_id}-{integration_sha}"))
    }

    pub fn begin_verification_command(
        &self,
        run_id: &str,
        message_hash: &str,
        turn_id: &str,
        command_index: u32,
        command: &str,
        cwd: &Path,
    ) -> Result<VerificationCommandClaim, StoreError> {
        let cwd_utf8 = cwd.to_str().ok_or_else(|| {
            StoreError::IncompatibleState(format!(
                "verification command cwd for run {run_id}, turn {turn_id}, index {command_index} is not valid UTF-8"
            ))
        })?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        ensure_run_exists(&transaction, run_id)?;
        let item_id = verification_command_item_id(message_hash, command_index);
        let matches = verification_command_claim_matches(
            &transaction,
            run_id,
            message_hash,
            turn_id,
            command_index,
        )?;

        if matches.len() > 1 {
            return Err(StoreError::IncompatibleState(format!(
                "run {run_id} has conflicting persisted verification command identity for turn {turn_id} item {item_id}"
            )));
        }

        if let Some(existing) = matches.into_iter().next() {
            let VerificationCommandRow { record, status } = existing;
            if record.message_hash != message_hash
                || record.turn_id != turn_id
                || record.item_id != item_id
                || record.command_index != command_index
                || record.command != command
                || record.cwd != cwd
            {
                return Err(StoreError::IncompatibleState(format!(
                    "verification command identity mismatch for run {run_id}, turn {turn_id}, item {item_id}"
                )));
            }
            return match status.as_str() {
                "STARTED" => Err(StoreError::VerificationExecutionUncertain(format!(
                    "verification command {item_id} for run {run_id} was started but not completed"
                ))),
                "COMPLETED" => Ok(VerificationCommandClaim::Reuse(record)),
                status => Err(StoreError::IncompatibleState(format!(
                    "verification command {item_id} for run {run_id} has unsupported status {status}"
                ))),
            };
        }

        transaction.execute(
            "INSERT INTO verification_command_executions (
                run_id, message_hash, turn_id, item_id, command_index, command,
                cwd, status, exit_code, stdout, stderr, started_at, completed_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'STARTED', NULL, NULL, NULL, ?8, NULL)",
            params![
                run_id,
                message_hash,
                turn_id,
                item_id,
                command_index,
                command,
                cwd_utf8,
                now_unix(),
            ],
        )?;
        transaction.commit()?;
        Ok(VerificationCommandClaim::Execute(
            VerificationCommandRecord {
                run_id: run_id.to_owned(),
                message_hash: message_hash.to_owned(),
                turn_id: turn_id.to_owned(),
                item_id,
                command_index,
                command: command.to_owned(),
                cwd: cwd.to_path_buf(),
                exit_code: None,
                stdout: None,
                stderr: None,
            },
        ))
    }

    pub fn complete_verification_command(
        &self,
        run_id: &str,
        message_hash: &str,
        command_index: u32,
        exit_code: i32,
        stdout: &str,
        stderr: &str,
    ) -> Result<VerificationCommandRecord, StoreError> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        ensure_run_exists(&transaction, run_id)?;
        let existing = verification_command_by_run_message_and_index(
            &transaction,
            run_id,
            message_hash,
            command_index,
        )?
        .ok_or_else(|| {
            StoreError::IncompatibleState(format!(
                "run {run_id} has no started verification command for request {message_hash} index {command_index}"
            ))
        })?;

        let VerificationCommandRow { record, status } = existing;
        match status.as_str() {
            "STARTED" => {
                let changed = transaction.execute(
                    "UPDATE verification_command_executions
                     SET status = 'COMPLETED', exit_code = ?1, stdout = ?2, stderr = ?3, completed_at = ?4
                     WHERE run_id = ?5 AND message_hash = ?6 AND command_index = ?7 AND status = 'STARTED'",
                    params![
                        exit_code,
                        stdout,
                        stderr,
                        now_unix(),
                        run_id,
                        message_hash,
                        command_index,
                    ],
                )?;
                if changed != 1 {
                    return Err(StoreError::IncompatibleState(format!(
                        "verification command {} for run {run_id} changed while completing",
                        record.item_id
                    )));
                }
                transaction.commit()?;
                Ok(VerificationCommandRecord {
                    exit_code: Some(exit_code),
                    stdout: Some(stdout.to_owned()),
                    stderr: Some(stderr.to_owned()),
                    ..record
                })
            }
            "COMPLETED" => {
                if record.exit_code == Some(exit_code)
                    && record.stdout.as_deref() == Some(stdout)
                    && record.stderr.as_deref() == Some(stderr)
                {
                    Ok(record)
                } else {
                    Err(StoreError::IncompatibleState(format!(
                        "verification command {} for run {run_id} already completed with different output",
                        record.item_id
                    )))
                }
            }
            status => Err(StoreError::IncompatibleState(format!(
                "verification command {} for run {run_id} has unsupported status {status}",
                record.item_id
            ))),
        }
    }

    pub fn insert_run(&self, state: &RunState) -> Result<(), StoreError> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let run_id = state.facts.run_id.to_string();
        let common_dir = state.facts.git_common_dir.to_string_lossy();
        let state_json = serialize_state(state)?;
        transaction.execute(
            "INSERT INTO runs (
                run_id, state_json, status, phase, round, plan_revision,
                integration_branch, integration_sha, reason_code, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
            params![
                run_id,
                state_json,
                enum_name(&state.status)?,
                enum_name(&state.phase)?,
                state.round,
                state.plan_revision,
                state.integration_branch,
                state.integration_sha,
                state.reason_code,
                now_unix(),
            ],
        )?;
        transaction.execute(
            "INSERT INTO source_facts (
                run_id, primary_thread_id, reviewer_thread_id,
                primary_worktree, reviewer_worktree, git_common_dir,
                primary_sha, reviewer_sha, primary_ref, reviewer_ref
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                run_id,
                state.facts.primary_thread_id,
                state.facts.reviewer_thread_id,
                state.facts.primary_worktree.to_string_lossy().as_ref(),
                state.facts.reviewer_worktree.to_string_lossy().as_ref(),
                common_dir.as_ref(),
                state.facts.primary_sha,
                state.facts.reviewer_sha,
                state.facts.primary_ref,
                state.facts.reviewer_ref,
            ],
        )?;
        match transaction.execute(
            "INSERT INTO locks (repository_id, run_id, primary_worktree, acquired_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                common_dir.as_ref(),
                run_id,
                state.facts.primary_worktree.to_string_lossy().as_ref(),
                now_unix(),
            ],
        ) {
            Ok(_) => {}
            Err(error) if is_constraint(&error) => {
                return Err(StoreError::ActiveRunExists(format!(
                    "repository {} already has an active run",
                    state.facts.git_common_dir.display()
                )));
            }
            Err(error) => return Err(error.into()),
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn load_run(&self, run_id: &str) -> Result<Option<RunState>, StoreError> {
        let connection = self.lock()?;
        let state_json = connection
            .query_row(
                "SELECT state_json FROM runs WHERE run_id = ?1",
                [run_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        state_json
            .map(|state| deserialize_state(&state))
            .transpose()
    }

    pub fn list_runs(&self) -> Result<Vec<RunSummary>, StoreError> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT run_id, status, phase, round, integration_branch,
                    integration_sha, reason_code, updated_at
             FROM runs ORDER BY updated_at DESC, run_id ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(RunSummary {
                run_id: row.get(0)?,
                status: row.get(1)?,
                phase: row.get(2)?,
                round: row.get(3)?,
                integration_branch: row.get(4)?,
                integration_sha: row.get(5)?,
                reason_code: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn activate_primary_binding(
        &self,
        run_id: &str,
        source_primary_thread_id: &str,
        effective_primary_thread_id: &str,
        mode: PrimaryBindingMode,
        participant_server: &str,
    ) -> Result<PrimaryParticipantBinding, StoreError> {
        if source_primary_thread_id.trim().is_empty()
            || effective_primary_thread_id.trim().is_empty()
            || participant_server.trim().is_empty()
        {
            return Err(StoreError::IncompatibleState(
                "primary participant binding identities must be nonempty".to_owned(),
            ));
        }

        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        ensure_run_exists(&transaction, run_id)?;
        let (frozen_primary, frozen_reviewer) = transaction.query_row(
            "SELECT primary_thread_id, reviewer_thread_id
             FROM source_facts WHERE run_id = ?1",
            [run_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        if source_primary_thread_id != frozen_primary {
            return Err(StoreError::IncompatibleState(format!(
                "run {run_id} primary binding source does not match frozen Primary task"
            )));
        }
        match mode {
            PrimaryBindingMode::Direct
                if effective_primary_thread_id != source_primary_thread_id =>
            {
                return Err(StoreError::IncompatibleState(format!(
                    "run {run_id} direct Primary binding must use the Source Primary task"
                )));
            }
            PrimaryBindingMode::EphemeralFork
                if effective_primary_thread_id == source_primary_thread_id
                    || effective_primary_thread_id == frozen_reviewer =>
            {
                return Err(StoreError::IncompatibleState(format!(
                    "run {run_id} ephemeral Primary binding must differ from both frozen source tasks"
                )));
            }
            _ => {}
        }

        let pending_count = transaction.query_row(
            "SELECT COUNT(*) FROM turns
             WHERE run_id = ?1 AND delivery_state IN ('PENDING', 'SENT')",
            [run_id],
            |row| row.get::<_, u64>(0),
        )?;
        if pending_count != 0 {
            return Err(StoreError::IncompatibleState(format!(
                "run {run_id} cannot change Primary binding while a turn is pending or sent"
            )));
        }

        if let Some(active) = query_active_primary_binding(&transaction, run_id)? {
            if active.source_primary_thread_id == source_primary_thread_id
                && active.effective_primary_thread_id == effective_primary_thread_id
                && active.mode == mode
                && active.participant_server == participant_server
            {
                transaction.commit()?;
                return Ok(active);
            }
        }

        let generation = transaction.query_row(
            "SELECT COALESCE(MAX(generation), 0) + 1
             FROM primary_participant_bindings WHERE run_id = ?1",
            [run_id],
            |row| row.get::<_, u32>(0),
        )?;
        transaction.execute(
            "UPDATE primary_participant_bindings
             SET active = 0 WHERE run_id = ?1 AND active = 1",
            [run_id],
        )?;
        let timestamp = now_unix();
        transaction.execute(
            "INSERT INTO primary_participant_bindings (
                run_id, generation, source_primary_thread_id,
                effective_primary_thread_id, mode, participant_server,
                active, created_at, verified_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?7)",
            params![
                run_id,
                generation,
                source_primary_thread_id,
                effective_primary_thread_id,
                mode.as_database_value(),
                participant_server,
                timestamp,
            ],
        )?;
        transaction.commit()?;
        Ok(PrimaryParticipantBinding {
            run_id: run_id.to_owned(),
            source_primary_thread_id: source_primary_thread_id.to_owned(),
            effective_primary_thread_id: effective_primary_thread_id.to_owned(),
            mode,
            generation,
            participant_server: participant_server.to_owned(),
            created_at: timestamp,
            verified_at: timestamp,
        })
    }

    pub fn active_primary_binding(
        &self,
        run_id: &str,
    ) -> Result<Option<PrimaryParticipantBinding>, StoreError> {
        let connection = self.lock()?;
        query_active_primary_binding(&connection, run_id)
    }

    pub fn activate_initial_direct_binding_for_pending_send(
        &self,
        run_id: &str,
        source_primary_thread_id: &str,
        participant_server: &str,
    ) -> Result<PrimaryParticipantBinding, StoreError> {
        if source_primary_thread_id.trim().is_empty() || participant_server.trim().is_empty() {
            return Err(StoreError::IncompatibleState(
                "initial direct binding identities must be nonempty".to_owned(),
            ));
        }

        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        ensure_run_exists(&transaction, run_id)?;
        let frozen_primary = transaction.query_row(
            "SELECT primary_thread_id FROM source_facts WHERE run_id = ?1",
            [run_id],
            |row| row.get::<_, String>(0),
        )?;
        if source_primary_thread_id != frozen_primary {
            return Err(StoreError::IncompatibleState(format!(
                "run {run_id} initial direct binding does not match frozen Primary task"
            )));
        }
        if query_active_primary_binding(&transaction, run_id)?.is_some() {
            return Err(StoreError::IncompatibleState(format!(
                "run {run_id} already has an active Primary binding"
            )));
        }
        let pending = transaction
            .query_row(
                "SELECT role, thread_id, participant_binding_generation
                 FROM turns
                 WHERE run_id = ?1 AND delivery_state IN ('PENDING', 'SENT')
                 ORDER BY id DESC LIMIT 1",
                [run_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<u32>>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::IncompatibleState(format!(
                    "run {run_id} has no pending send for initial direct binding migration"
                ))
            })?;
        let pending_count = transaction.query_row(
            "SELECT COUNT(*) FROM turns
             WHERE run_id = ?1 AND delivery_state IN ('PENDING', 'SENT')",
            [run_id],
            |row| row.get::<_, u64>(0),
        )?;
        if pending_count != 1
            || pending.0 != "PRIMARY"
            || pending
                .1
                .as_deref()
                .is_some_and(|id| id != source_primary_thread_id)
            || pending.2.is_some()
        {
            return Err(StoreError::IncompatibleState(format!(
                "run {run_id} pending send is not eligible for initial direct binding migration"
            )));
        }

        let generation = transaction.query_row(
            "SELECT COALESCE(MAX(generation), 0) + 1
             FROM primary_participant_bindings WHERE run_id = ?1",
            [run_id],
            |row| row.get::<_, u32>(0),
        )?;
        let timestamp = now_unix();
        transaction.execute(
            "INSERT INTO primary_participant_bindings (
                run_id, generation, source_primary_thread_id,
                effective_primary_thread_id, mode, participant_server,
                active, created_at, verified_at
             ) VALUES (?1, ?2, ?3, ?3, 'DIRECT', ?4, 1, ?5, ?5)",
            params![
                run_id,
                generation,
                source_primary_thread_id,
                participant_server,
                timestamp,
            ],
        )?;
        let changed = transaction.execute(
            "UPDATE turns
             SET participant_binding_generation = ?1
             WHERE run_id = ?2 AND delivery_state IN ('PENDING', 'SENT')
               AND participant_binding_generation IS NULL",
            params![generation, run_id],
        )?;
        if changed != 1 {
            return Err(StoreError::IncompatibleState(format!(
                "run {run_id} pending send changed during initial direct binding migration"
            )));
        }
        transaction.commit()?;
        Ok(PrimaryParticipantBinding {
            run_id: run_id.to_owned(),
            source_primary_thread_id: source_primary_thread_id.to_owned(),
            effective_primary_thread_id: source_primary_thread_id.to_owned(),
            mode: PrimaryBindingMode::Direct,
            generation,
            participant_server: participant_server.to_owned(),
            created_at: timestamp,
            verified_at: timestamp,
        })
    }

    pub fn bind_unsent_primary_pending_to_active_binding(
        &self,
        run_id: &str,
    ) -> Result<(), StoreError> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let Some(binding) = query_active_primary_binding(&transaction, run_id)? else {
            return Err(StoreError::IncompatibleState(format!(
                "run {run_id} has no active Primary binding"
            )));
        };
        let pending = transaction
            .query_row(
                "SELECT role, thread_id, turn_id, participant_binding_generation
                 FROM turns
                 WHERE run_id = ?1 AND delivery_state IN ('PENDING', 'SENT')
                 ORDER BY id DESC LIMIT 1",
                [run_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<u32>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((role, thread_id, turn_id, generation)) = pending else {
            transaction.commit()?;
            return Ok(());
        };
        if role != "PRIMARY" {
            return Err(StoreError::IncompatibleState(format!(
                "run {run_id} pending send belongs to Reviewer, not Primary"
            )));
        }
        if let Some(generation) = generation {
            if generation != binding.generation {
                return Err(StoreError::IncompatibleState(format!(
                    "run {run_id} pending send does not match the active Primary binding"
                )));
            }
            transaction.commit()?;
            return Ok(());
        }
        if thread_id.is_some() || turn_id.is_some() {
            return Err(StoreError::IncompatibleState(format!(
                "run {run_id} cannot bind a sent or uncertain legacy turn to a new Primary binding"
            )));
        }
        let changed = transaction.execute(
            "UPDATE turns
             SET participant_binding_generation = ?1
             WHERE run_id = ?2 AND delivery_state = 'PENDING'
               AND thread_id IS NULL AND turn_id IS NULL
               AND participant_binding_generation IS NULL",
            params![binding.generation, run_id],
        )?;
        if changed != 1 {
            return Err(StoreError::IncompatibleState(format!(
                "run {run_id} unsent Primary pending changed while binding it"
            )));
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn primary_binding(
        &self,
        run_id: &str,
        generation: u32,
    ) -> Result<Option<PrimaryParticipantBinding>, StoreError> {
        let connection = self.lock()?;
        query_primary_binding(&connection, run_id, generation)
    }

    pub fn record_pending_send(
        &self,
        run_id: &str,
        role: &str,
        phase: &str,
        round: u32,
        message_hash: &str,
    ) -> Result<(), StoreError> {
        self.record_pending_send_with_binding(run_id, role, phase, round, message_hash, None)
    }

    pub fn record_pending_send_with_binding(
        &self,
        run_id: &str,
        role: &str,
        phase: &str,
        round: u32,
        message_hash: &str,
        participant_binding_generation: Option<u32>,
    ) -> Result<(), StoreError> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        ensure_run_exists(&transaction, run_id)?;
        transaction.execute(
            "INSERT INTO turns (
                run_id, role, phase, round, message_hash, delivery_state,
                capability_generation, participant_binding_generation, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'PENDING', ?6, ?7, ?8)
             ON CONFLICT(run_id, role, phase, round, message_hash)
             DO NOTHING",
            params![
                run_id,
                role,
                phase,
                round,
                message_hash,
                PARTICIPANT_CAPABILITY_GENERATION,
                participant_binding_generation,
                now_unix()
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn pending_send(&self, run_id: &str) -> Result<Option<PendingSend>, StoreError> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT run_id, role, phase, round, message_hash, thread_id, turn_id,
                        capability_generation, participant_binding_generation
                 FROM turns
                 WHERE run_id = ?1 AND delivery_state IN ('PENDING', 'SENT')
                 ORDER BY id DESC LIMIT 1",
                [run_id],
                |row| {
                    Ok(PendingSend {
                        run_id: row.get(0)?,
                        role: row.get(1)?,
                        phase: row.get(2)?,
                        round: row.get(3)?,
                        message_hash: row.get(4)?,
                        thread_id: row.get(5)?,
                        turn_id: row.get(6)?,
                        full_prompt: None,
                        capability_generation: row.get(7)?,
                        participant_binding_generation: row.get(8)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn latest_accepted_turn(&self, run_id: &str) -> Result<Option<AcceptedTurn>, StoreError> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT run_id, role, phase, round, message_hash, response_hash,
                        thread_id, turn_id, capability_generation,
                        participant_binding_generation
                 FROM turns
                 WHERE run_id = ?1 AND delivery_state = 'ACCEPTED'
                 ORDER BY id DESC LIMIT 1",
                [run_id],
                |row| {
                    Ok(AcceptedTurn {
                        run_id: row.get(0)?,
                        role: row.get(1)?,
                        phase: row.get(2)?,
                        round: row.get(3)?,
                        message_hash: row.get(4)?,
                        response_hash: row.get(5)?,
                        thread_id: row.get(6)?,
                        turn_id: row.get(7)?,
                        capability_generation: row.get(8)?,
                        participant_binding_generation: row.get(9)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn record_turn_started(
        &self,
        run_id: &str,
        message_hash: &str,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<(), StoreError> {
        self.record_turn_started_with_generation(run_id, message_hash, thread_id, turn_id, true)
    }

    pub fn record_turn_start_intent(
        &self,
        run_id: &str,
        message_hash: &str,
    ) -> Result<(), StoreError> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE turns
             SET capability_generation = ?1
             WHERE run_id = ?2 AND message_hash = ?3
               AND delivery_state IN ('PENDING', 'SENT')
               AND thread_id IS NULL AND turn_id IS NULL",
            params![PARTICIPANT_CAPABILITY_GENERATION, run_id, message_hash],
        )?;
        if changed != 1 {
            return Err(StoreError::PendingSendNotFound(run_id.to_owned()));
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn record_recovered_turn_started(
        &self,
        run_id: &str,
        message_hash: &str,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<(), StoreError> {
        self.record_turn_started_with_generation(run_id, message_hash, thread_id, turn_id, false)
    }

    fn record_turn_started_with_generation(
        &self,
        run_id: &str,
        message_hash: &str,
        thread_id: &str,
        turn_id: &str,
        record_participant_generation: bool,
    ) -> Result<(), StoreError> {
        let capability_generation =
            record_participant_generation.then_some(PARTICIPANT_CAPABILITY_GENERATION);
        let changed = self.lock()?.execute(
            "UPDATE turns
             SET delivery_state = 'SENT', thread_id = ?1, turn_id = ?2,
                 capability_generation = COALESCE(?3, capability_generation)
             WHERE run_id = ?4 AND message_hash = ?5
               AND delivery_state IN ('PENDING', 'SENT')",
            params![
                thread_id,
                turn_id,
                capability_generation,
                run_id,
                message_hash
            ],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(StoreError::PendingSendNotFound(run_id.to_owned()))
        }
    }

    pub fn record_turn_item_event(
        &self,
        run_id: &str,
        thread_id: &str,
        turn_id: &str,
        event_method: &str,
        item: &Value,
    ) -> Result<(), StoreError> {
        let lifecycle_state = match event_method {
            "item/started" => "STARTED",
            "item/completed" => "COMPLETED",
            other => {
                return Err(StoreError::IncompatibleState(format!(
                    "unsupported turn item lifecycle event {other}"
                )));
            }
        };
        let item_id = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                StoreError::IncompatibleState("turn item event has no nonempty id".into())
            })?;
        let item_type = item
            .get("type")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                StoreError::IncompatibleState("turn item event has no nonempty type".into())
            })?;
        let item_json = serde_json::to_string(item)?;

        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let turn_record_id = active_turn_record_id(&transaction, run_id, thread_id, turn_id)?;
        let existing = transaction
            .query_row(
                "SELECT item_type, lifecycle_state, item_json
                 FROM turn_event_items
                 WHERE turn_record_id = ?1 AND turn_id = ?2 AND item_id = ?3",
                params![turn_record_id, turn_id, item_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        match existing {
            None => {
                transaction.execute(
                    "INSERT INTO turn_event_items (
                        turn_record_id, run_id, thread_id, turn_id, item_id,
                        item_type, lifecycle_state, item_json, created_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                    params![
                        turn_record_id,
                        run_id,
                        thread_id,
                        turn_id,
                        item_id,
                        item_type,
                        lifecycle_state,
                        item_json,
                        now_unix(),
                    ],
                )?;
            }
            Some((existing_type, existing_lifecycle, existing_json)) => {
                if existing_type != item_type {
                    return Err(StoreError::IncompatibleState(format!(
                        "turn item {item_id} changed type from {existing_type} to {item_type}"
                    )));
                }
                if existing_lifecycle == "COMPLETED" {
                    if lifecycle_state == "COMPLETED"
                        && serde_json::from_str::<Value>(&existing_json)? != *item
                    {
                        return Err(StoreError::IncompatibleState(format!(
                            "completed turn item {item_id} changed after persistence"
                        )));
                    }
                } else {
                    transaction.execute(
                        "UPDATE turn_event_items
                         SET lifecycle_state = ?1, item_json = ?2, updated_at = ?3
                         WHERE turn_record_id = ?4 AND turn_id = ?5 AND item_id = ?6",
                        params![
                            lifecycle_state,
                            item_json,
                            now_unix(),
                            turn_record_id,
                            turn_id,
                            item_id,
                        ],
                    )?;
                }
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn record_turn_completed_event(
        &self,
        run_id: &str,
        thread_id: &str,
        turn_id: &str,
        turn: &Value,
    ) -> Result<(), StoreError> {
        if turn.get("id").and_then(Value::as_str) != Some(turn_id) {
            return Err(StoreError::IncompatibleState(
                "turn/completed payload does not match its bound turn id".into(),
            ));
        }
        let completed_turn_json = serde_json::to_string(turn)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let turn_record_id = active_turn_record_id(&transaction, run_id, thread_id, turn_id)?;
        let existing = transaction
            .query_row(
                "SELECT completed_turn_json
                 FROM turn_event_completions
                 WHERE turn_record_id = ?1 AND turn_id = ?2",
                params![turn_record_id, turn_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if serde_json::from_str::<Value>(&existing)? != *turn {
                return Err(StoreError::IncompatibleState(format!(
                    "completed turn event {turn_id} changed after persistence"
                )));
            }
        } else {
            transaction.execute(
                "INSERT INTO turn_event_completions (
                    turn_record_id, run_id, thread_id, turn_id,
                    completed_turn_json, recorded_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    turn_record_id,
                    run_id,
                    thread_id,
                    turn_id,
                    completed_turn_json,
                    now_unix(),
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn turn_event_evidence(
        &self,
        run_id: &str,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<Option<TurnEventEvidence>, StoreError> {
        let connection = self.lock()?;
        let completed_turn_json = connection
            .query_row(
                "SELECT completion.completed_turn_json
                 FROM turn_event_completions completion
                 JOIN turns turn_record ON turn_record.id = completion.turn_record_id
                 WHERE completion.run_id = ?1
                   AND completion.thread_id = ?2
                   AND completion.turn_id = ?3
                   AND turn_record.thread_id = ?2
                   AND turn_record.turn_id = ?3",
                params![run_id, thread_id, turn_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(completed_turn_json) = completed_turn_json else {
            return Ok(None);
        };
        let mut statement = connection.prepare(
            "SELECT lifecycle_state, item_json
             FROM turn_event_items
             WHERE run_id = ?1 AND thread_id = ?2 AND turn_id = ?3
             ORDER BY id ASC",
        )?;
        let rows = statement.query_map(params![run_id, thread_id, turn_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut completed_items = Vec::new();
        for row in rows {
            let (lifecycle_state, item_json) = row?;
            if lifecycle_state != "COMPLETED" {
                return Err(StoreError::IncompatibleState(format!(
                    "turn {turn_id} completed before all item lifecycle events were persisted"
                )));
            }
            completed_items.push(serde_json::from_str(&item_json)?);
        }
        Ok(Some(TurnEventEvidence {
            completed_turn: serde_json::from_str(&completed_turn_json)?,
            completed_items,
        }))
    }

    pub fn reset_terminal_turn_for_retry(
        &self,
        run_id: &str,
        message_hash: &str,
        thread_id: &str,
        turn_id: &str,
        terminal_status: &str,
    ) -> Result<(), StoreError> {
        if !matches!(terminal_status, "failed" | "interrupted") {
            return Err(StoreError::TerminalTurnNotRetryable(format!(
                "turn {turn_id} has non-terminal retry status {terminal_status}"
            )));
        }

        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        archive_and_reset_turn(
            &transaction,
            run_id,
            message_hash,
            thread_id,
            turn_id,
            "SENT",
            terminal_status,
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn reset_completed_read_only_turn_for_retry(
        &self,
        run_id: &str,
        message_hash: &str,
        thread_id: &str,
        turn_id: &str,
        observed_status: &str,
    ) -> Result<(), StoreError> {
        if observed_status != "completed" {
            return Err(StoreError::TerminalTurnNotRetryable(format!(
                "turn {turn_id} has non-completed model-response retry status {observed_status}"
            )));
        }

        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        archive_and_reset_turn(
            &transaction,
            run_id,
            message_hash,
            thread_id,
            turn_id,
            "SENT",
            observed_status,
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn reset_completed_integration_tool_failure_turn_for_retry(
        &self,
        run_id: &str,
        message_hash: &str,
        thread_id: &str,
        turn_id: &str,
        observed_status: &str,
    ) -> Result<(), StoreError> {
        if observed_status != "completed" {
            return Err(StoreError::TerminalTurnNotRetryable(format!(
                "turn {turn_id} has non-completed integration-tool retry status {observed_status}"
            )));
        }

        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        archive_and_reset_turn(
            &transaction,
            run_id,
            message_hash,
            thread_id,
            turn_id,
            "SENT",
            observed_status,
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn reactivate_blocked_run_with_completed_turn_retry(
        &self,
        blocked_state: &RunState,
        resumed_state: &RunState,
        message_hash: &str,
        thread_id: &str,
        turn_id: &str,
        observed_status: &str,
    ) -> Result<(), StoreError> {
        if blocked_state.status != RunStatus::Blocked
            || resumed_state.status != RunStatus::Running
            || blocked_state.facts != resumed_state.facts
            || observed_status != "completed"
        {
            return Err(StoreError::TerminalTurnNotRetryable(
                "blocked-run reactivation identity or status is invalid".into(),
            ));
        }

        let run_id = blocked_state.facts.run_id.to_string();
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let current_json = transaction
            .query_row(
                "SELECT state_json FROM runs WHERE run_id = ?1",
                [&run_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::RunNotFound(run_id.clone()))?;
        let current_state = deserialize_state(&current_json)?;
        if current_state != *blocked_state {
            return Err(StoreError::TerminalTurnNotRetryable(format!(
                "run {run_id} changed while preparing blocked model-response retry"
            )));
        }

        let common_dir = resumed_state.facts.git_common_dir.to_string_lossy();
        match transaction.execute(
            "INSERT INTO locks (repository_id, run_id, primary_worktree, acquired_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                common_dir.as_ref(),
                run_id,
                resumed_state
                    .facts
                    .primary_worktree
                    .to_string_lossy()
                    .as_ref(),
                now_unix(),
            ],
        ) {
            Ok(_) => {}
            Err(error) if is_constraint(&error) => {
                return Err(StoreError::ActiveRunExists(format!(
                    "repository {} already has an active run",
                    resumed_state.facts.git_common_dir.display()
                )));
            }
            Err(error) => return Err(error.into()),
        }
        archive_and_reset_turn(
            &transaction,
            &run_id,
            message_hash,
            thread_id,
            turn_id,
            "SENT",
            observed_status,
        )?;
        update_run_row(&transaction, &run_id, resumed_state)?;
        transaction.commit()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn reactivate_blocked_run_with_verification_evidence_retry(
        &self,
        blocked_state: &RunState,
        resumed_state: &RunState,
        message_hash: &str,
        thread_id: &str,
        turn_id: &str,
        observed_status: &str,
    ) -> Result<(), StoreError> {
        let diagnostic = blocked_state.last_error.as_ref();
        if blocked_state.status != RunStatus::Blocked
            || blocked_state.reason_code.as_deref() != Some("TEST_FAILURE")
            || diagnostic.map(|value| value.code.as_str()) != Some("TEST_FAILURE")
            || diagnostic.map(|value| value.action) != Some(NextAction::RequestPrimaryVerification)
            || diagnostic.and_then(|value| value.role) != Some(Role::Primary)
            || resumed_state.status != RunStatus::Running
            || resumed_state.phase != Phase::Verify
            || resumed_state.next_action != NextAction::RequestPrimaryVerification
            || blocked_state.facts != resumed_state.facts
            || observed_status != "completed"
        {
            return Err(StoreError::TerminalTurnNotRetryable(
                "verification evidence retry identity or status is invalid".into(),
            ));
        }

        let run_id = blocked_state.facts.run_id.to_string();
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let current_json = transaction
            .query_row(
                "SELECT state_json FROM runs WHERE run_id = ?1",
                [&run_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::RunNotFound(run_id.clone()))?;
        let current_state = deserialize_state(&current_json)?;
        if current_state != *blocked_state {
            return Err(StoreError::TerminalTurnNotRetryable(format!(
                "run {run_id} changed while preparing verification evidence recovery"
            )));
        }
        let prior_compatibility_retry = transaction
            .query_row(
                "SELECT 1 FROM turn_attempts
                 WHERE run_id = ?1 AND message_hash = ?2
                   AND terminal_status = 'completed-evidence-unavailable'
                 LIMIT 1",
                params![run_id, message_hash],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if prior_compatibility_retry {
            return Err(StoreError::TerminalTurnNotRetryable(
                "verification evidence compatibility recovery is limited to one retry".into(),
            ));
        }

        let common_dir = resumed_state.facts.git_common_dir.to_string_lossy();
        match transaction.execute(
            "INSERT INTO locks (repository_id, run_id, primary_worktree, acquired_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                common_dir.as_ref(),
                run_id,
                resumed_state
                    .facts
                    .primary_worktree
                    .to_string_lossy()
                    .as_ref(),
                now_unix(),
            ],
        ) {
            Ok(_) => {}
            Err(error) if is_constraint(&error) => {
                return Err(StoreError::ActiveRunExists(format!(
                    "repository {} already has an active run",
                    resumed_state.facts.git_common_dir.display()
                )));
            }
            Err(error) => return Err(error.into()),
        }
        archive_and_reset_turn(
            &transaction,
            &run_id,
            message_hash,
            thread_id,
            turn_id,
            "SENT",
            "completed-evidence-unavailable",
        )?;
        update_run_row(&transaction, &run_id, resumed_state)?;
        transaction.commit()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn reactivate_blocked_run_with_unattended_verification_retry(
        &self,
        blocked_state: &RunState,
        resumed_state: &RunState,
        message_hash: &str,
        thread_id: &str,
        turn_id: &str,
        observed_status: &str,
    ) -> Result<(), StoreError> {
        let mut expected_resumed_state = blocked_state.clone();
        if expected_resumed_state
            .retry_blocked_verification_without_execution()
            .is_err()
            || expected_resumed_state != *resumed_state
            || thread_id != blocked_state.facts.primary_thread_id
            || observed_status != "completed"
        {
            return Err(StoreError::TerminalTurnNotRetryable(
                "unattended verification migration identity or status is invalid".into(),
            ));
        }

        let run_id = blocked_state.facts.run_id.to_string();
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let current_json = transaction
            .query_row(
                "SELECT state_json FROM runs WHERE run_id = ?1",
                [&run_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::RunNotFound(run_id.clone()))?;
        let current_state = deserialize_state(&current_json)?;
        if current_state != *blocked_state {
            return Err(StoreError::TerminalTurnNotRetryable(format!(
                "run {run_id} changed while preparing unattended verification migration"
            )));
        }
        let (compatibility_retry_count, migration_retry_count) = transaction.query_row(
            "SELECT
                COALESCE(SUM(CASE
                    WHEN message_hash = ?2
                     AND terminal_status = 'completed-evidence-unavailable'
                    THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN terminal_status = 'completed-unattended-verification-migration' THEN 1 ELSE 0 END), 0)
             FROM turn_attempts
             WHERE run_id = ?1",
            params![run_id, message_hash],
            |row| Ok((row.get::<_, u64>(0)?, row.get::<_, u64>(1)?)),
        )?;
        if compatibility_retry_count != 1 || migration_retry_count != 0 {
            return Err(StoreError::TerminalTurnNotRetryable(
                "unattended verification migration requires one prior evidence compatibility retry and no prior migration"
                    .into(),
            ));
        }

        let common_dir = resumed_state.facts.git_common_dir.to_string_lossy();
        match transaction.execute(
            "INSERT INTO locks (repository_id, run_id, primary_worktree, acquired_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                common_dir.as_ref(),
                run_id,
                resumed_state
                    .facts
                    .primary_worktree
                    .to_string_lossy()
                    .as_ref(),
                now_unix(),
            ],
        ) {
            Ok(_) => {}
            Err(error) if is_constraint(&error) => {
                return Err(StoreError::ActiveRunExists(format!(
                    "repository {} already has an active run",
                    resumed_state.facts.git_common_dir.display()
                )));
            }
            Err(error) => return Err(error.into()),
        }
        archive_and_reset_turn(
            &transaction,
            &run_id,
            message_hash,
            thread_id,
            turn_id,
            "SENT",
            "completed-unattended-verification-migration",
        )?;
        update_run_row(&transaction, &run_id, resumed_state)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn v025_verification_completion_collision_candidate(
        &self,
        run_id: &str,
    ) -> Result<Option<V025VerificationCompletionCollision>, StoreError> {
        let connection = self.lock()?;
        v025_verification_completion_collision_candidate(&connection, run_id)
    }

    pub fn recover_v025_verification_completion_collision(
        &self,
        candidate: &V025VerificationCompletionCollision,
        recovered_state: &RunState,
    ) -> Result<(), StoreError> {
        let mut expected_recovered_state = candidate.blocked_state.clone();
        if expected_recovered_state
            .recover_v025_verification_completion_collision()
            .is_err()
            || expected_recovered_state != *recovered_state
        {
            return Err(StoreError::TerminalTurnNotRetryable(
                "v0.2.5 completion collision recovery state is invalid".into(),
            ));
        }

        let run_id = candidate.blocked_state.facts.run_id.to_string();
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let current = v025_verification_completion_collision_candidate(&transaction, &run_id)?;
        if current.as_ref() != Some(candidate) {
            return Err(StoreError::TerminalTurnNotRetryable(format!(
                "run {run_id} changed while preparing v0.2.5 completion collision recovery"
            )));
        }

        let common_dir = recovered_state.facts.git_common_dir.to_string_lossy();
        match transaction.execute(
            "INSERT INTO locks (repository_id, run_id, primary_worktree, acquired_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                common_dir.as_ref(),
                run_id,
                recovered_state
                    .facts
                    .primary_worktree
                    .to_string_lossy()
                    .as_ref(),
                now_unix(),
            ],
        ) {
            Ok(_) => {}
            Err(error) if is_constraint(&error) => {
                return Err(StoreError::ActiveRunExists(format!(
                    "repository {} already has an active run",
                    recovered_state.facts.git_common_dir.display()
                )));
            }
            Err(error) => return Err(error.into()),
        }
        transaction.execute(
            "DELETE FROM turn_event_items
             WHERE turn_record_id = ?1 AND turn_id = ?2",
            params![candidate.turn_record_id, candidate.stale_turn_id],
        )?;
        let deleted_completion = transaction.execute(
            "DELETE FROM turn_event_completions
             WHERE turn_record_id = ?1 AND turn_id = ?2",
            params![candidate.turn_record_id, candidate.stale_turn_id],
        )?;
        if deleted_completion != 1 {
            return Err(StoreError::TerminalTurnNotRetryable(
                "v0.2.5 completion collision evidence changed during recovery".into(),
            ));
        }
        update_run_row(&transaction, &run_id, recovered_state)?;
        transaction.commit()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn reactivate_blocked_run_with_interrupted_forbidden_operation_retry(
        &self,
        blocked_state: &RunState,
        resumed_state: &RunState,
        message_hash: &str,
        thread_id: &str,
        turn_id: &str,
        observed_status: &str,
    ) -> Result<(), StoreError> {
        let diagnostic = blocked_state.last_error.as_ref();
        if blocked_state.status != RunStatus::Blocked
            || blocked_state.reason_code.as_deref() != Some("FORBIDDEN_OPERATION")
            || diagnostic.map(|value| value.code.as_str()) != Some("FORBIDDEN_OPERATION")
            || diagnostic.map(|value| value.action) != Some(NextAction::RequestPrimaryIntegration)
            || diagnostic.and_then(|value| value.role) != Some(Role::Primary)
            || resumed_state.status != RunStatus::Running
            || blocked_state.facts != resumed_state.facts
            || !matches!(observed_status, "failed" | "interrupted")
        {
            return Err(StoreError::TerminalTurnNotRetryable(
                "blocked forbidden-operation retry identity or status is invalid".into(),
            ));
        }

        let run_id = blocked_state.facts.run_id.to_string();
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let current_json = transaction
            .query_row(
                "SELECT state_json FROM runs WHERE run_id = ?1",
                [&run_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::RunNotFound(run_id.clone()))?;
        let current_state = deserialize_state(&current_json)?;
        if current_state != *blocked_state {
            return Err(StoreError::TerminalTurnNotRetryable(format!(
                "run {run_id} changed while preparing forbidden-operation recovery"
            )));
        }

        let common_dir = resumed_state.facts.git_common_dir.to_string_lossy();
        match transaction.execute(
            "INSERT INTO locks (repository_id, run_id, primary_worktree, acquired_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                common_dir.as_ref(),
                run_id,
                resumed_state
                    .facts
                    .primary_worktree
                    .to_string_lossy()
                    .as_ref(),
                now_unix(),
            ],
        ) {
            Ok(_) => {}
            Err(error) if is_constraint(&error) => {
                return Err(StoreError::ActiveRunExists(format!(
                    "repository {} already has an active run",
                    resumed_state.facts.git_common_dir.display()
                )));
            }
            Err(error) => return Err(error.into()),
        }
        archive_and_reset_turn(
            &transaction,
            &run_id,
            message_hash,
            thread_id,
            turn_id,
            "SENT",
            observed_status,
        )?;
        update_run_row(&transaction, &run_id, resumed_state)?;
        transaction.execute(
            "INSERT INTO transitions (
                run_id, from_phase, to_phase, status, reason_code,
                response_hash, created_at
             ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5)",
            params![
                run_id,
                enum_name(&blocked_state.phase)?,
                enum_name(&resumed_state.phase)?,
                enum_name(&resumed_state.status)?,
                now_unix(),
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn reactivate_blocked_run_with_accepted_execution_tool_retry(
        &self,
        blocked_state: &RunState,
        resumed_state: &RunState,
        accepted: &AcceptedTurn,
        observed_status: &str,
    ) -> Result<(), StoreError> {
        if blocked_state.status != RunStatus::Blocked
            || blocked_state.reason_code.as_deref() != Some("EXECUTION_TOOL_UNAVAILABLE")
            || resumed_state.status != RunStatus::Running
            || blocked_state.facts != resumed_state.facts
            || observed_status != "completed"
        {
            return Err(StoreError::TerminalTurnNotRetryable(
                "blocked execution-tool retry identity or status is invalid".into(),
            ));
        }

        let run_id = blocked_state.facts.run_id.to_string();
        if accepted.run_id != run_id
            || accepted.role != "PRIMARY"
            || accepted.phase != "INTEGRATE"
            || accepted.round != blocked_state.round
            || accepted.thread_id != blocked_state.facts.primary_thread_id
        {
            return Err(StoreError::TerminalTurnNotRetryable(
                "accepted execution-tool blocker does not match the frozen integration action"
                    .into(),
            ));
        }

        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let current_json = transaction
            .query_row(
                "SELECT state_json FROM runs WHERE run_id = ?1",
                [&run_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::RunNotFound(run_id.clone()))?;
        let current_state = deserialize_state(&current_json)?;
        if current_state != *blocked_state {
            return Err(StoreError::TerminalTurnNotRetryable(format!(
                "run {run_id} changed while preparing execution-tool recovery"
            )));
        }

        let common_dir = resumed_state.facts.git_common_dir.to_string_lossy();
        match transaction.execute(
            "INSERT INTO locks (repository_id, run_id, primary_worktree, acquired_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                common_dir.as_ref(),
                run_id,
                resumed_state
                    .facts
                    .primary_worktree
                    .to_string_lossy()
                    .as_ref(),
                now_unix(),
            ],
        ) {
            Ok(_) => {}
            Err(error) if is_constraint(&error) => {
                return Err(StoreError::ActiveRunExists(format!(
                    "repository {} already has an active run",
                    resumed_state.facts.git_common_dir.display()
                )));
            }
            Err(error) => return Err(error.into()),
        }
        archive_and_reset_turn(
            &transaction,
            &run_id,
            &accepted.message_hash,
            &accepted.thread_id,
            &accepted.turn_id,
            "ACCEPTED",
            observed_status,
        )?;
        update_run_row(&transaction, &run_id, resumed_state)?;
        transaction.execute(
            "INSERT INTO transitions (
                run_id, from_phase, to_phase, status, reason_code,
                response_hash, created_at
             ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5)",
            params![
                run_id,
                enum_name(&blocked_state.phase)?,
                enum_name(&resumed_state.phase)?,
                enum_name(&resumed_state.status)?,
                now_unix(),
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn reactivate_blocked_run_with_corrective_patch_tool_retry(
        &self,
        blocked_state: &RunState,
        resumed_state: &RunState,
        accepted: &AcceptedTurn,
        observed_status: &str,
    ) -> Result<(), StoreError> {
        let mut expected_resumed = blocked_state.clone();
        expected_resumed
            .retry_blocked_corrective_patch_tool_unavailable()
            .map_err(|error| {
                StoreError::TerminalTurnNotRetryable(format!(
                    "corrective patch-tool state is not retryable: {error}"
                ))
            })?;
        if *resumed_state != expected_resumed || observed_status != "completed" {
            return Err(StoreError::TerminalTurnNotRetryable(
                "corrective patch-tool retry state or terminal status is invalid".into(),
            ));
        }

        let run_id = blocked_state.facts.run_id.to_string();
        if accepted.run_id != run_id
            || accepted.role != "PRIMARY"
            || accepted.phase != "INTEGRATE"
            || accepted.round != blocked_state.round
            || accepted.thread_id != blocked_state.facts.primary_thread_id
            || accepted.message_hash.is_empty()
            || accepted.response_hash.is_empty()
            || accepted.turn_id.is_empty()
            || accepted.capability_generation.as_deref()
                != Some(LEGACY_PARTICIPANT_CAPABILITY_GENERATION)
        {
            return Err(StoreError::TerminalTurnNotRetryable(
                "accepted corrective patch-tool blocker does not match the frozen correction request"
                    .into(),
            ));
        }

        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let current_json = transaction
            .query_row(
                "SELECT state_json FROM runs WHERE run_id = ?1",
                [&run_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::RunNotFound(run_id.clone()))?;
        let current_state = deserialize_state(&current_json)?;
        if current_state != *blocked_state {
            return Err(StoreError::TerminalTurnNotRetryable(format!(
                "run {run_id} changed while preparing corrective patch-tool recovery"
            )));
        }
        let latest_accepted = transaction
            .query_row(
                "SELECT run_id, role, phase, round, message_hash, response_hash,
                        thread_id, turn_id, capability_generation,
                        participant_binding_generation
                 FROM turns
                 WHERE run_id = ?1 AND delivery_state = 'ACCEPTED'
                 ORDER BY id DESC LIMIT 1",
                [&run_id],
                |row| {
                    Ok(AcceptedTurn {
                        run_id: row.get(0)?,
                        role: row.get(1)?,
                        phase: row.get(2)?,
                        round: row.get(3)?,
                        message_hash: row.get(4)?,
                        response_hash: row.get(5)?,
                        thread_id: row.get(6)?,
                        turn_id: row.get(7)?,
                        capability_generation: row.get(8)?,
                        participant_binding_generation: row.get(9)?,
                    })
                },
            )
            .optional()?;
        if latest_accepted.as_ref() != Some(accepted) {
            return Err(StoreError::TerminalTurnNotRetryable(
                "corrective patch-tool blocker is not the transaction-local latest accepted turn"
                    .into(),
            ));
        }
        let prior_attempt_exists = transaction
            .query_row(
                "SELECT 1 FROM turn_attempts
                 WHERE run_id = ?1 AND message_hash = ?2
                 LIMIT 1",
                params![run_id, accepted.message_hash],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if prior_attempt_exists {
            return Err(StoreError::TerminalTurnNotRetryable(
                "corrective patch-tool recovery is limited to one empty blocker attempt".into(),
            ));
        }
        let successful_patch_exists = transaction
            .query_row(
                "SELECT 1 FROM patch_applications
                 WHERE run_id = ?1 AND message_hash = ?2
                 LIMIT 1",
                params![run_id, accepted.message_hash],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if successful_patch_exists {
            return Err(StoreError::TerminalTurnNotRetryable(
                "corrective patch-tool blocker request already has a successful patch record"
                    .into(),
            ));
        }

        let common_dir = resumed_state.facts.git_common_dir.to_string_lossy();
        match transaction.execute(
            "INSERT INTO locks (repository_id, run_id, primary_worktree, acquired_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                common_dir.as_ref(),
                run_id,
                resumed_state
                    .facts
                    .primary_worktree
                    .to_string_lossy()
                    .as_ref(),
                now_unix(),
            ],
        ) {
            Ok(_) => {}
            Err(error) if is_constraint(&error) => {
                return Err(StoreError::ActiveRunExists(format!(
                    "repository {} already has an active run",
                    resumed_state.facts.git_common_dir.display()
                )));
            }
            Err(error) => return Err(error.into()),
        }
        archive_and_reset_turn(
            &transaction,
            &run_id,
            &accepted.message_hash,
            &accepted.thread_id,
            &accepted.turn_id,
            "ACCEPTED",
            observed_status,
        )?;
        update_run_row(&transaction, &run_id, resumed_state)?;
        transaction.execute(
            "INSERT INTO transitions (
                run_id, from_phase, to_phase, status, reason_code,
                response_hash, created_at
             ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5)",
            params![
                run_id,
                enum_name(&blocked_state.phase)?,
                enum_name(&resumed_state.phase)?,
                enum_name(&resumed_state.status)?,
                now_unix(),
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn reactivate_blocked_run_with_accepted_verification_environment_retry(
        &self,
        blocked_state: &RunState,
        resumed_state: &RunState,
        accepted: &AcceptedTurn,
        observed_status: &str,
    ) -> Result<(), StoreError> {
        if blocked_state.status != RunStatus::Blocked
            || blocked_state.reason_code.as_deref() != Some("CARGO_UNAVAILABLE")
            || resumed_state.status != RunStatus::Running
            || resumed_state.phase != Phase::Verify
            || resumed_state.next_action != NextAction::RequestPrimaryVerification
            || blocked_state.facts != resumed_state.facts
            || observed_status != "completed"
        {
            return Err(StoreError::TerminalTurnNotRetryable(
                "blocked verification environment retry identity or status is invalid".into(),
            ));
        }

        let run_id = blocked_state.facts.run_id.to_string();
        if accepted.run_id != run_id
            || accepted.role != "PRIMARY"
            || accepted.phase != "VERIFY"
            || accepted.round != blocked_state.round
            || accepted.thread_id != blocked_state.facts.primary_thread_id
        {
            return Err(StoreError::TerminalTurnNotRetryable(
                "accepted verification environment blocker does not match the frozen request"
                    .into(),
            ));
        }

        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let current_json = transaction
            .query_row(
                "SELECT state_json FROM runs WHERE run_id = ?1",
                [&run_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::RunNotFound(run_id.clone()))?;
        let current_state = deserialize_state(&current_json)?;
        if current_state != *blocked_state {
            return Err(StoreError::TerminalTurnNotRetryable(format!(
                "run {run_id} changed while preparing verification environment recovery"
            )));
        }

        let common_dir = resumed_state.facts.git_common_dir.to_string_lossy();
        match transaction.execute(
            "INSERT INTO locks (repository_id, run_id, primary_worktree, acquired_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                common_dir.as_ref(),
                run_id,
                resumed_state
                    .facts
                    .primary_worktree
                    .to_string_lossy()
                    .as_ref(),
                now_unix(),
            ],
        ) {
            Ok(_) => {}
            Err(error) if is_constraint(&error) => {
                return Err(StoreError::ActiveRunExists(format!(
                    "repository {} already has an active run",
                    resumed_state.facts.git_common_dir.display()
                )));
            }
            Err(error) => return Err(error.into()),
        }
        archive_and_reset_turn(
            &transaction,
            &run_id,
            &accepted.message_hash,
            &accepted.thread_id,
            &accepted.turn_id,
            "ACCEPTED",
            observed_status,
        )?;
        update_run_row(&transaction, &run_id, resumed_state)?;
        transaction.execute(
            "INSERT INTO transitions (
                run_id, from_phase, to_phase, status, reason_code,
                response_hash, created_at
             ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5)",
            params![
                run_id,
                enum_name(&blocked_state.phase)?,
                enum_name(&resumed_state.phase)?,
                enum_name(&resumed_state.status)?,
                now_unix(),
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn accept_response_and_advance(
        &self,
        run_id: &str,
        response_hash: &str,
        next_state: &RunState,
    ) -> Result<(), StoreError> {
        if next_state.facts.run_id.to_string() != run_id {
            return Err(StoreError::RunNotFound(run_id.to_owned()));
        }
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let current_json = transaction
            .query_row(
                "SELECT state_json FROM runs WHERE run_id = ?1",
                [run_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_owned()))?;
        let current = deserialize_state(&current_json)?;
        let pending_id = transaction
            .query_row(
                "SELECT id FROM turns
                 WHERE run_id = ?1 AND delivery_state IN ('PENDING', 'SENT')
                 ORDER BY id DESC LIMIT 1",
                [run_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::PendingSendNotFound(run_id.to_owned()))?;
        let changed = transaction.execute(
            "UPDATE turns
             SET delivery_state = 'ACCEPTED', response_hash = ?1, accepted_at = ?2
             WHERE id = ?3 AND delivery_state IN ('PENDING', 'SENT')",
            params![response_hash, now_unix(), pending_id],
        )?;
        if changed != 1 {
            return Err(StoreError::PendingSendNotFound(run_id.to_owned()));
        }

        update_run_row(&transaction, run_id, next_state)?;
        transaction.execute(
            "INSERT INTO transitions (
                run_id, from_phase, to_phase, status, reason_code,
                response_hash, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                run_id,
                enum_name(&current.phase)?,
                enum_name(&next_state.phase)?,
                enum_name(&next_state.status)?,
                next_state.reason_code,
                response_hash,
                now_unix(),
            ],
        )?;
        if is_terminal(next_state.status) {
            transaction.execute("DELETE FROM locks WHERE run_id = ?1", [run_id])?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn save_state(&self, state: &RunState) -> Result<(), StoreError> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let run_id = state.facts.run_id.to_string();
        ensure_run_exists(&transaction, &run_id)?;
        update_run_row(&transaction, &run_id, state)?;
        if is_terminal(state.status) {
            transaction.execute("DELETE FROM locks WHERE run_id = ?1", [&run_id])?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn transition_count(&self, run_id: &str) -> Result<u64, StoreError> {
        let connection = self.lock()?;
        let count = connection.query_row(
            "SELECT COUNT(*) FROM transitions WHERE run_id = ?1",
            [run_id],
            |row| row.get::<_, u64>(0),
        )?;
        Ok(count)
    }

    pub fn turn_attempt_count(&self, run_id: &str) -> Result<u64, StoreError> {
        let connection = self.lock()?;
        let count = connection.query_row(
            "SELECT COUNT(*) FROM turn_attempts WHERE run_id = ?1",
            [run_id],
            |row| row.get::<_, u64>(0),
        )?;
        Ok(count)
    }

    pub fn archived_turn_ids(
        &self,
        run_id: &str,
        message_hash: &str,
    ) -> Result<Vec<String>, StoreError> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT turn_id FROM turn_attempts
             WHERE run_id = ?1 AND message_hash = ?2
             ORDER BY id ASC",
        )?;
        let rows = statement.query_map(params![run_id, message_hash], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn archived_turn_attempts(
        &self,
        run_id: &str,
        message_hash: &str,
    ) -> Result<Vec<ArchivedTurnAttempt>, StoreError> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT turn_id, terminal_status FROM turn_attempts
             WHERE run_id = ?1 AND message_hash = ?2
             ORDER BY id ASC",
        )?;
        let rows = statement.query_map(params![run_id, message_hash], |row| {
            Ok(ArchivedTurnAttempt {
                turn_id: row.get(0)?,
                terminal_status: row.get(1)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn verification_evidence_retry_recorded(
        &self,
        run_id: &str,
        message_hash: &str,
    ) -> Result<bool, StoreError> {
        let connection = self.lock()?;
        Ok(connection
            .query_row(
                "SELECT 1 FROM turn_attempts
                 WHERE run_id = ?1 AND message_hash = ?2
                   AND terminal_status = 'completed-evidence-unavailable'
                 LIMIT 1",
                params![run_id, message_hash],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn successful_patch_recorded(
        &self,
        run_id: &str,
        message_hash: &str,
    ) -> Result<bool, StoreError> {
        let connection = self.lock()?;
        let exists = connection
            .query_row(
                "SELECT 1 FROM patch_applications
                 WHERE run_id = ?1 AND message_hash = ?2
                 LIMIT 1",
                params![run_id, message_hash],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        Ok(exists)
    }

    pub fn successful_patch_hash(
        &self,
        run_id: &str,
        message_hash: &str,
    ) -> Result<Option<String>, StoreError> {
        Ok(self
            .successful_patch_record(run_id, message_hash)?
            .map(|record| record.patch_hash))
    }

    pub fn successful_patch_record(
        &self,
        run_id: &str,
        message_hash: &str,
    ) -> Result<Option<SuccessfulPatchRecord>, StoreError> {
        self.lock()?
            .query_row(
                "SELECT patch_hash, source_primary_thread_id,
                        effective_primary_thread_id,
                        participant_binding_generation
                 FROM patch_applications
                 WHERE run_id = ?1 AND message_hash = ?2
                 LIMIT 1",
                params![run_id, message_hash],
                |row| {
                    Ok(SuccessfulPatchRecord {
                        patch_hash: row.get(0)?,
                        source_primary_thread_id: row.get(1)?,
                        effective_primary_thread_id: row.get(2)?,
                        participant_binding_generation: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn record_successful_patch(
        &self,
        run_id: &str,
        message_hash: &str,
        patch_hash: &str,
    ) -> Result<(), StoreError> {
        self.record_successful_patch_with_provenance(
            run_id,
            message_hash,
            patch_hash,
            None,
            None,
            None,
        )
        .map(|_| ())
    }

    pub fn record_successful_patch_with_provenance(
        &self,
        run_id: &str,
        message_hash: &str,
        patch_hash: &str,
        source_primary_thread_id: Option<&str>,
        effective_primary_thread_id: Option<&str>,
        participant_binding_generation: Option<u32>,
    ) -> Result<SuccessfulPatchRecord, StoreError> {
        let provenance_count = [
            source_primary_thread_id.is_some(),
            effective_primary_thread_id.is_some(),
            participant_binding_generation.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if provenance_count != 0 && provenance_count != 3 {
            return Err(StoreError::IncompatibleState(
                "successful patch binding provenance must be entirely present or absent".to_owned(),
            ));
        }

        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        ensure_run_exists(&transaction, run_id)?;
        if let (Some(source), Some(effective), Some(generation)) = (
            source_primary_thread_id,
            effective_primary_thread_id,
            participant_binding_generation,
        ) {
            let binding =
                query_primary_binding(&transaction, run_id, generation)?.ok_or_else(|| {
                    StoreError::IncompatibleState(format!(
                        "run {run_id} has no Primary binding generation {generation}"
                    ))
                })?;
            if binding.source_primary_thread_id != source
                || binding.effective_primary_thread_id != effective
            {
                return Err(StoreError::IncompatibleState(format!(
                    "successful patch provenance does not match run {run_id} Primary binding generation {generation}"
                )));
            }
        }
        let record = SuccessfulPatchRecord {
            patch_hash: patch_hash.to_owned(),
            source_primary_thread_id: source_primary_thread_id.map(str::to_owned),
            effective_primary_thread_id: effective_primary_thread_id.map(str::to_owned),
            participant_binding_generation,
        };
        let changed = transaction.execute(
            "INSERT INTO patch_applications (
                run_id, message_hash, patch_hash, source_primary_thread_id,
                effective_primary_thread_id, participant_binding_generation,
                applied_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(run_id, message_hash) DO NOTHING",
            params![
                run_id,
                message_hash,
                patch_hash,
                source_primary_thread_id,
                effective_primary_thread_id,
                participant_binding_generation,
                now_unix(),
            ],
        )?;
        if changed == 1 {
            transaction.commit()?;
            Ok(record)
        } else {
            Err(StoreError::IncompatibleState(format!(
                "run {run_id} already recorded a successful controlled patch for request {message_hash}"
            )))
        }
    }

    pub fn release_lock(&self, run_id: &str) -> Result<(), StoreError> {
        self.lock()?
            .execute("DELETE FROM locks WHERE run_id = ?1", [run_id])?;
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
        self.connection.lock().map_err(|_| StoreError::Poisoned)
    }
}

type PrimaryBindingRow = (String, u32, String, String, String, String, i64, i64);

fn query_active_primary_binding(
    connection: &Connection,
    run_id: &str,
) -> Result<Option<PrimaryParticipantBinding>, StoreError> {
    let row = connection
        .query_row(
            "SELECT run_id, generation, source_primary_thread_id,
                    effective_primary_thread_id, mode, participant_server,
                    created_at, verified_at
             FROM primary_participant_bindings
             WHERE run_id = ?1 AND active = 1",
            [run_id],
            primary_binding_row,
        )
        .optional()?;
    row.map(decode_primary_binding).transpose()
}

fn query_primary_binding(
    connection: &Connection,
    run_id: &str,
    generation: u32,
) -> Result<Option<PrimaryParticipantBinding>, StoreError> {
    let row = connection
        .query_row(
            "SELECT run_id, generation, source_primary_thread_id,
                    effective_primary_thread_id, mode, participant_server,
                    created_at, verified_at
             FROM primary_participant_bindings
             WHERE run_id = ?1 AND generation = ?2",
            params![run_id, generation],
            primary_binding_row,
        )
        .optional()?;
    row.map(decode_primary_binding).transpose()
}

fn primary_binding_row(row: &rusqlite::Row<'_>) -> Result<PrimaryBindingRow, rusqlite::Error> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
    ))
}

fn decode_primary_binding(row: PrimaryBindingRow) -> Result<PrimaryParticipantBinding, StoreError> {
    let (
        run_id,
        generation,
        source_primary_thread_id,
        effective_primary_thread_id,
        mode,
        participant_server,
        created_at,
        verified_at,
    ) = row;
    let mode = PrimaryBindingMode::from_database_value(&mode).ok_or_else(|| {
        StoreError::IncompatibleState(format!(
            "run {run_id} has unsupported Primary binding mode {mode}"
        ))
    })?;
    Ok(PrimaryParticipantBinding {
        run_id,
        source_primary_thread_id,
        effective_primary_thread_id,
        mode,
        generation,
        participant_server,
        created_at,
        verified_at,
    })
}

fn active_turn_record_id(
    transaction: &Transaction<'_>,
    run_id: &str,
    thread_id: &str,
    turn_id: &str,
) -> Result<i64, StoreError> {
    transaction
        .query_row(
            "SELECT id FROM turns
             WHERE run_id = ?1 AND thread_id = ?2 AND turn_id = ?3
               AND delivery_state = 'SENT'",
            params![run_id, thread_id, turn_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or_else(|| {
            StoreError::PendingSendNotFound(format!(
                "run {run_id} has no active turn {turn_id} on task {thread_id}"
            ))
        })
}

fn v025_verification_completion_collision_candidate(
    connection: &Connection,
    run_id: &str,
) -> Result<Option<V025VerificationCompletionCollision>, StoreError> {
    let state_json = connection
        .query_row(
            "SELECT state_json FROM runs WHERE run_id = ?1",
            [run_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(state_json) = state_json else {
        return Ok(None);
    };
    let blocked_state = deserialize_state(&state_json)?;
    let mut recovered_state = blocked_state.clone();
    if recovered_state
        .recover_v025_verification_completion_collision()
        .is_err()
    {
        return Ok(None);
    }

    let mut statement = connection.prepare(
        "SELECT id, role, phase, round, message_hash, thread_id, turn_id,
                capability_generation, participant_binding_generation
         FROM turns
         WHERE run_id = ?1 AND delivery_state = 'SENT'
         ORDER BY id ASC",
    )?;
    let sent_turns = statement
        .query_map([run_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, u32>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<u32>>(8)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let [
        (
            turn_record_id,
            role,
            phase,
            round,
            message_hash,
            Some(thread_id),
            Some(turn_id),
            capability_generation,
            participant_binding_generation,
        ),
    ] = sent_turns.as_slice()
    else {
        return Ok(None);
    };
    if role != "PRIMARY"
        || phase != "VERIFY"
        || *round != blocked_state.round
        || thread_id != &blocked_state.facts.primary_thread_id
    {
        return Ok(None);
    }

    let mut statement = connection.prepare(
        "SELECT turn_id, terminal_status
         FROM turn_attempts
         WHERE turn_record_id = ?1
         ORDER BY id ASC",
    )?;
    let attempts = statement
        .query_map([turn_record_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let expected_statuses = [
        "completed",
        "completed",
        "completed-evidence-unavailable",
        "completed-unattended-verification-migration",
    ];
    if attempts.len() != expected_statuses.len()
        || attempts
            .iter()
            .zip(expected_statuses)
            .any(|((_, status), expected)| status != expected)
    {
        return Ok(None);
    }
    let stale_turn_id = attempts.last().map(|attempt| attempt.0.clone()).unwrap();
    if stale_turn_id == *turn_id {
        return Ok(None);
    }
    let migration_count = connection.query_row(
        "SELECT COUNT(*) FROM turn_attempts
         WHERE run_id = ?1
           AND terminal_status = 'completed-unattended-verification-migration'",
        [run_id],
        |row| row.get::<_, u64>(0),
    )?;
    if migration_count != 1 {
        return Ok(None);
    }

    let stale_completion = connection
        .query_row(
            "SELECT run_id, thread_id, turn_id, completed_turn_json
             FROM turn_event_completions
             WHERE turn_record_id = ?1",
            [turn_record_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((completion_run_id, completion_thread_id, completion_turn_id, completion_json)) =
        stale_completion
    else {
        return Ok(None);
    };
    let completion: Value = serde_json::from_str(&completion_json)?;
    if completion_run_id != run_id
        || completion_thread_id != blocked_state.facts.primary_thread_id
        || completion_turn_id != stale_turn_id
        || completion.get("id").and_then(Value::as_str) != Some(stale_turn_id.as_str())
        || completion.get("status").and_then(Value::as_str) != Some("completed")
    {
        return Ok(None);
    }

    let (active_agent_items, active_side_effect_items) = connection.query_row(
        "SELECT
            COALESCE(SUM(CASE
                WHEN turn_id = ?2 AND item_type = 'agentMessage'
                 AND lifecycle_state = 'COMPLETED' THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE
                WHEN turn_id = ?2 AND item_type IN (
                    'commandExecution', 'fileChange', 'mcpToolCall', 'dynamicToolCall'
                ) THEN 1 ELSE 0 END), 0)
         FROM turn_event_items
         WHERE turn_record_id = ?1",
        params![turn_record_id, turn_id],
        |row| Ok((row.get::<_, u64>(0)?, row.get::<_, u64>(1)?)),
    )?;
    if active_agent_items == 0 || active_side_effect_items != 0 {
        return Ok(None);
    }
    let command_count = connection.query_row(
        "SELECT COUNT(*) FROM verification_command_executions
         WHERE run_id = ?1",
        [run_id],
        |row| row.get::<_, u64>(0),
    )?;
    let patch_count = connection.query_row(
        "SELECT COUNT(*) FROM patch_applications WHERE run_id = ?1",
        [run_id],
        |row| row.get::<_, u64>(0),
    )?;
    let lock_count = connection.query_row(
        "SELECT COUNT(*) FROM locks WHERE run_id = ?1",
        [run_id],
        |row| row.get::<_, u64>(0),
    )?;
    if command_count != 0 || patch_count != 1 || lock_count != 0 {
        return Ok(None);
    }

    Ok(Some(V025VerificationCompletionCollision {
        blocked_state,
        pending: PendingSend {
            run_id: run_id.to_owned(),
            role: role.clone(),
            phase: phase.clone(),
            round: *round,
            message_hash: message_hash.clone(),
            thread_id: Some(thread_id.clone()),
            turn_id: Some(turn_id.clone()),
            full_prompt: None,
            capability_generation: capability_generation.clone(),
            participant_binding_generation: *participant_binding_generation,
        },
        stale_turn_id,
        turn_record_id: *turn_record_id,
    }))
}

fn archive_and_reset_turn(
    transaction: &Transaction<'_>,
    run_id: &str,
    message_hash: &str,
    thread_id: &str,
    turn_id: &str,
    delivery_state: &str,
    observed_status: &str,
) -> Result<(), StoreError> {
    let turn_record_id = transaction
        .query_row(
            "SELECT id FROM turns
             WHERE run_id = ?1 AND message_hash = ?2
               AND delivery_state = ?3
               AND thread_id = ?4 AND turn_id = ?5",
            params![run_id, message_hash, delivery_state, thread_id, turn_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or_else(|| {
            StoreError::TerminalTurnNotRetryable(format!(
                "turn {turn_id} is not the persisted pending attempt for run {run_id}"
            ))
        })?;
    transaction.execute(
        "INSERT INTO turn_attempts (
            turn_record_id, run_id, message_hash, thread_id, turn_id,
            terminal_status, recorded_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            turn_record_id,
            run_id,
            message_hash,
            thread_id,
            turn_id,
            observed_status,
            now_unix(),
        ],
    )?;
    transaction.execute(
        "DELETE FROM turn_event_items
         WHERE turn_record_id = ?1 AND turn_id = ?2",
        params![turn_record_id, turn_id],
    )?;
    transaction.execute(
        "DELETE FROM turn_event_completions
         WHERE turn_record_id = ?1 AND turn_id = ?2",
        params![turn_record_id, turn_id],
    )?;
    let changed = transaction.execute(
        "UPDATE turns
         SET delivery_state = 'PENDING', thread_id = NULL, turn_id = NULL,
             response_hash = NULL, accepted_at = NULL,
             capability_generation = ?1
         WHERE id = ?2 AND delivery_state = ?3
           AND thread_id = ?4 AND turn_id = ?5",
        params![
            PARTICIPANT_CAPABILITY_GENERATION,
            turn_record_id,
            delivery_state,
            thread_id,
            turn_id
        ],
    )?;
    if changed != 1 {
        return Err(StoreError::TerminalTurnNotRetryable(format!(
            "turn {turn_id} changed while preparing its retry"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VerificationCommandRow {
    record: VerificationCommandRecord,
    status: String,
}

fn migrate(connection: &Connection) -> Result<(), rusqlite::Error> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS runs (
            run_id TEXT PRIMARY KEY,
            state_json TEXT NOT NULL,
            status TEXT NOT NULL,
            phase TEXT NOT NULL,
            round INTEGER NOT NULL,
            plan_revision INTEGER,
            integration_branch TEXT,
            integration_sha TEXT,
            reason_code TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS source_facts (
            run_id TEXT PRIMARY KEY REFERENCES runs(run_id) ON DELETE CASCADE,
            primary_thread_id TEXT NOT NULL,
            reviewer_thread_id TEXT NOT NULL,
            primary_worktree TEXT NOT NULL,
            reviewer_worktree TEXT NOT NULL,
            git_common_dir TEXT NOT NULL,
            primary_sha TEXT NOT NULL,
            reviewer_sha TEXT NOT NULL,
            primary_ref TEXT,
            reviewer_ref TEXT
         );
         CREATE TABLE IF NOT EXISTS primary_participant_bindings (
            run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
            generation INTEGER NOT NULL,
            source_primary_thread_id TEXT NOT NULL,
            effective_primary_thread_id TEXT NOT NULL,
            mode TEXT NOT NULL CHECK(mode IN ('DIRECT', 'EPHEMERAL_FORK')),
            participant_server TEXT NOT NULL,
            active INTEGER NOT NULL CHECK(active IN (0, 1)),
            created_at INTEGER NOT NULL,
            verified_at INTEGER NOT NULL,
            PRIMARY KEY(run_id, generation)
         );
         CREATE UNIQUE INDEX IF NOT EXISTS one_active_primary_binding
            ON primary_participant_bindings(run_id) WHERE active = 1;
         CREATE TABLE IF NOT EXISTS turns (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
            role TEXT NOT NULL,
            phase TEXT NOT NULL,
            round INTEGER NOT NULL,
            message_hash TEXT NOT NULL,
            response_hash TEXT,
            delivery_state TEXT NOT NULL,
            thread_id TEXT,
            turn_id TEXT,
            capability_generation TEXT,
            participant_binding_generation INTEGER,
            created_at INTEGER NOT NULL,
            accepted_at INTEGER,
            UNIQUE(run_id, role, phase, round, message_hash)
         );
         CREATE INDEX IF NOT EXISTS turns_pending
            ON turns(run_id, delivery_state, id DESC);
             CREATE TABLE IF NOT EXISTS turn_attempts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            turn_record_id INTEGER NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
            run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
            message_hash TEXT NOT NULL,
            thread_id TEXT NOT NULL,
            turn_id TEXT NOT NULL,
            terminal_status TEXT NOT NULL,
            recorded_at INTEGER NOT NULL,
             UNIQUE(turn_record_id, turn_id)
             );
         CREATE TABLE IF NOT EXISTS turn_event_items (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            turn_record_id INTEGER NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
            run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
            thread_id TEXT NOT NULL,
            turn_id TEXT NOT NULL,
            item_id TEXT NOT NULL,
            item_type TEXT NOT NULL,
            lifecycle_state TEXT NOT NULL,
            item_json TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(turn_record_id, turn_id, item_id)
         );
         CREATE INDEX IF NOT EXISTS turn_event_items_lookup
            ON turn_event_items(run_id, thread_id, turn_id, id ASC);
         CREATE TABLE IF NOT EXISTS turn_event_completions (
            turn_record_id INTEGER PRIMARY KEY REFERENCES turns(id) ON DELETE CASCADE,
            run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
            thread_id TEXT NOT NULL,
            turn_id TEXT NOT NULL,
            completed_turn_json TEXT NOT NULL,
            recorded_at INTEGER NOT NULL,
            UNIQUE(run_id, turn_id)
         );
             CREATE TABLE IF NOT EXISTS patch_applications (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
                message_hash TEXT NOT NULL,
                patch_hash TEXT NOT NULL,
                source_primary_thread_id TEXT,
                effective_primary_thread_id TEXT,
                participant_binding_generation INTEGER,
                applied_at INTEGER NOT NULL,
                UNIQUE(run_id, message_hash)
             );
         CREATE TABLE IF NOT EXISTS verification_command_executions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
            message_hash TEXT NOT NULL,
            turn_id TEXT NOT NULL,
            item_id TEXT NOT NULL,
            command_index INTEGER NOT NULL,
            command TEXT NOT NULL,
            cwd TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('STARTED', 'COMPLETED')),
            exit_code INTEGER,
            stdout TEXT,
            stderr TEXT,
            started_at INTEGER NOT NULL,
            completed_at INTEGER,
            UNIQUE(run_id, message_hash, command_index),
            UNIQUE(run_id, item_id)
         );
         CREATE INDEX IF NOT EXISTS turn_attempts_run
            ON turn_attempts(run_id, id DESC);
         CREATE TABLE IF NOT EXISTS transitions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
            from_phase TEXT NOT NULL,
            to_phase TEXT NOT NULL,
            status TEXT NOT NULL,
            reason_code TEXT,
            response_hash TEXT,
            created_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS locks (
            repository_id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL UNIQUE REFERENCES runs(run_id) ON DELETE CASCADE,
            primary_worktree TEXT NOT NULL UNIQUE,
            acquired_at INTEGER NOT NULL
         );",
    )?;
    if !table_has_column(connection, "turns", "capability_generation")? {
        connection.execute(
            "ALTER TABLE turns ADD COLUMN capability_generation TEXT",
            [],
        )?;
    }
    if !table_has_column(connection, "turns", "participant_binding_generation")? {
        connection.execute(
            "ALTER TABLE turns ADD COLUMN participant_binding_generation INTEGER",
            [],
        )?;
    }
    for (column, column_type) in [
        ("source_primary_thread_id", "TEXT"),
        ("effective_primary_thread_id", "TEXT"),
        ("participant_binding_generation", "INTEGER"),
    ] {
        if !table_has_column(connection, "patch_applications", column)? {
            connection.execute(
                &format!("ALTER TABLE patch_applications ADD COLUMN {column} {column_type}"),
                [],
            )?;
        }
    }
    Ok(())
}

fn table_has_column(
    connection: &Connection,
    table: &str,
    expected_column: &str,
) -> Result<bool, rusqlite::Error> {
    let mut columns = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = columns
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(names.iter().any(|column| column == expected_column))
}

fn update_run_row(
    transaction: &Transaction<'_>,
    run_id: &str,
    state: &RunState,
) -> Result<(), StoreError> {
    let state_json = serialize_state(state)?;
    let changed = transaction.execute(
        "UPDATE runs SET
            state_json = ?1, status = ?2, phase = ?3, round = ?4,
            plan_revision = ?5, integration_branch = ?6,
            integration_sha = ?7, reason_code = ?8, updated_at = ?9
         WHERE run_id = ?10",
        params![
            state_json,
            enum_name(&state.status)?,
            enum_name(&state.phase)?,
            state.round,
            state.plan_revision,
            state.integration_branch,
            state.integration_sha,
            state.reason_code,
            now_unix(),
            run_id,
        ],
    )?;
    if changed == 1 {
        Ok(())
    } else {
        Err(StoreError::RunNotFound(run_id.to_owned()))
    }
}

fn serialize_state(state: &RunState) -> Result<String, StoreError> {
    state
        .validate_persisted()
        .map_err(|error| StoreError::IncompatibleState(error.to_string()))?;
    serde_json::to_string(state).map_err(Into::into)
}

fn deserialize_state(encoded: &str) -> Result<RunState, StoreError> {
    let value = serde_json::from_str::<serde_json::Value>(encoded)?;
    let version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64);
    if version != Some(u64::from(consensus_core::state::RUN_STATE_SCHEMA_VERSION)) {
        return Err(StoreError::IncompatibleState(format!(
            "persisted state schema {:?} is unsupported",
            version
        )));
    }
    let state = serde_json::from_value::<RunState>(value)?;
    state
        .validate_persisted()
        .map_err(|error| StoreError::IncompatibleState(error.to_string()))?;
    Ok(state)
}

fn verification_command_claim_matches(
    transaction: &Transaction<'_>,
    run_id: &str,
    message_hash: &str,
    turn_id: &str,
    command_index: u32,
) -> Result<Vec<VerificationCommandRow>, StoreError> {
    let mut statement = transaction.prepare(
        "SELECT run_id, message_hash, turn_id, item_id, command_index, command,
                cwd, status, exit_code, stdout, stderr
         FROM verification_command_executions
         WHERE run_id = ?1
           AND command_index = ?2
           AND (message_hash = ?3 OR turn_id = ?4)
         ORDER BY id ASC",
    )?;
    let rows = statement.query_map(
        params![run_id, command_index, message_hash, turn_id],
        verification_command_row_from_sql,
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn verification_command_by_run_message_and_index(
    transaction: &Transaction<'_>,
    run_id: &str,
    message_hash: &str,
    command_index: u32,
) -> Result<Option<VerificationCommandRow>, StoreError> {
    transaction
        .query_row(
            "SELECT run_id, message_hash, turn_id, item_id, command_index, command,
                    cwd, status, exit_code, stdout, stderr
             FROM verification_command_executions
             WHERE run_id = ?1 AND message_hash = ?2 AND command_index = ?3",
            params![run_id, message_hash, command_index],
            verification_command_row_from_sql,
        )
        .optional()
        .map_err(Into::into)
}

fn verification_command_row_from_sql(
    row: &rusqlite::Row<'_>,
) -> Result<VerificationCommandRow, rusqlite::Error> {
    let status = row.get::<_, String>(7)?;
    let exit_code = row.get::<_, Option<i32>>(8)?;
    let stdout = row.get::<_, Option<String>>(9)?;
    let stderr = row.get::<_, Option<String>>(10)?;
    let outputs_present = exit_code.is_some() || stdout.is_some() || stderr.is_some();
    if status == "STARTED" && outputs_present {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            7,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "started verification command row has completion output",
            )),
        ));
    }
    if status == "COMPLETED" && !outputs_present {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            7,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "completed verification command row has no persisted output",
            )),
        ));
    }
    Ok(VerificationCommandRow {
        record: VerificationCommandRecord {
            run_id: row.get(0)?,
            message_hash: row.get(1)?,
            turn_id: row.get(2)?,
            item_id: row.get(3)?,
            command_index: row.get(4)?,
            command: row.get(5)?,
            cwd: PathBuf::from(row.get::<_, String>(6)?),
            exit_code,
            stdout,
            stderr,
        },
        status,
    })
}

fn verification_command_item_id(message_hash: &str, command_index: u32) -> String {
    format!("coordinator-command/{message_hash}/{command_index}")
}

fn ensure_run_exists(transaction: &Transaction<'_>, run_id: &str) -> Result<(), StoreError> {
    let exists = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM runs WHERE run_id = ?1)",
        [run_id],
        |row| row.get::<_, bool>(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(StoreError::RunNotFound(run_id.to_owned()))
    }
}

fn enum_name<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    serde_json::to_value(value).map(|value| {
        value
            .as_str()
            .expect("state enums serialize as strings")
            .to_owned()
    })
}

fn is_terminal(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Accepted
            | RunStatus::Blocked
            | RunStatus::Cancelled
            | RunStatus::IncompatibleCodex
    )
}

fn is_constraint(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}
