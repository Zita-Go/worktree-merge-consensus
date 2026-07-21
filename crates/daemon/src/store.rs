use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use consensus_core::state::{RunState, RunStatus};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

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

    pub fn record_pending_send(
        &self,
        run_id: &str,
        role: &str,
        phase: &str,
        round: u32,
        message_hash: &str,
    ) -> Result<(), StoreError> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        ensure_run_exists(&transaction, run_id)?;
        transaction.execute(
            "INSERT INTO turns (
                run_id, role, phase, round, message_hash, delivery_state, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'PENDING', ?6)
             ON CONFLICT(run_id, role, phase, round, message_hash)
             DO NOTHING",
            params![run_id, role, phase, round, message_hash, now_unix()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn pending_send(&self, run_id: &str) -> Result<Option<PendingSend>, StoreError> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT run_id, role, phase, round, message_hash, thread_id, turn_id
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
                        thread_id, turn_id
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
        let changed = self.lock()?.execute(
            "UPDATE turns
             SET delivery_state = 'SENT', thread_id = ?1, turn_id = ?2
             WHERE run_id = ?3 AND message_hash = ?4
               AND delivery_state IN ('PENDING', 'SENT')",
            params![thread_id, turn_id, run_id, message_hash],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(StoreError::PendingSendNotFound(run_id.to_owned()))
        }
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

    pub fn release_lock(&self, run_id: &str) -> Result<(), StoreError> {
        self.lock()?
            .execute("DELETE FROM locks WHERE run_id = ?1", [run_id])?;
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
        self.connection.lock().map_err(|_| StoreError::Poisoned)
    }
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
    let changed = transaction.execute(
        "UPDATE turns
         SET delivery_state = 'PENDING', thread_id = NULL, turn_id = NULL,
             response_hash = NULL, accepted_at = NULL
         WHERE id = ?1 AND delivery_state = ?2
           AND thread_id = ?3 AND turn_id = ?4",
        params![turn_record_id, delivery_state, thread_id, turn_id],
    )?;
    if changed != 1 {
        return Err(StoreError::TerminalTurnNotRetryable(format!(
            "turn {turn_id} changed while preparing its retry"
        )));
    }
    Ok(())
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
    )
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
