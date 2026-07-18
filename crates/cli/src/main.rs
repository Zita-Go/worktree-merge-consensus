mod args;
mod output;
mod select;

use std::{
    collections::HashSet,
    io::IsTerminal,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use app_server_client::{AppServer, CodexAppServer, ConnectOptions, ThreadDetail, ThreadSummary};
use args::{Cli, Command, DaemonCommand, RunArgs, ThreadsCommand};
use clap::Parser;
use consensus_core::{
    git::{GitInspector, WorktreeSnapshot, verify_frozen_sources, verify_same_repository},
    state::{RunFacts, RunState},
};
use consensus_daemon::{
    coordinator::{Coordinator, CoordinatorOptions, GitRepositorySafety, StartRequest},
    lifecycle::{default_state_dir, ensure_daemon},
    server::{RunController, ServerConfig, run_server_with_controller},
    store::SqliteRunStore,
    wire::{DaemonRequest, DaemonResponse},
};
use output::{emit_error, emit_serializable, emit_value, human_json};
use select::{SelectedTasks, TerminalTaskSelector, select_tasks, task_label};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::oneshot;
use uuid::Uuid;

#[derive(Debug, Error)]
#[error("{code}: {message}")]
pub struct CliError {
    code: String,
    message: String,
}

impl CliError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn code(&self) -> &str {
        &self.code
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let json_output = cli.json_output();
    if let Err(message) = cli.validate() {
        emit_error(&CliError::new("INVALID_ARGUMENTS", message), json_output);
        return ExitCode::FAILURE;
    }
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            emit_error(&error, json_output);
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), CliError> {
    let state_dir = match cli.state_dir {
        Some(state_dir) => state_dir,
        None => default_state_dir()
            .map_err(|error| CliError::new("STATE_HOME_UNAVAILABLE", error.to_string()))?,
    };
    match cli.command {
        Command::Doctor(arguments) => doctor(&state_dir, arguments.json).await,
        Command::Threads(arguments) => match arguments.command {
            ThreadsCommand::List(output) => list_threads(output.json).await,
        },
        Command::Run(arguments) => start_run(&state_dir, arguments).await,
        Command::Status(arguments) => {
            daemon_request(
                &state_dir,
                DaemonRequest::Status {
                    run_id: arguments.run_id,
                },
                arguments.json,
            )
            .await
        }
        Command::Resume(arguments) => {
            daemon_request(
                &state_dir,
                DaemonRequest::Resume {
                    run_id: arguments.run_id,
                },
                arguments.json,
            )
            .await
        }
        Command::Cancel(arguments) => {
            daemon_request(
                &state_dir,
                DaemonRequest::Cancel {
                    run_id: arguments.run_id,
                },
                arguments.json,
            )
            .await
        }
        Command::Daemon(arguments) => match arguments.command {
            DaemonCommand::Serve(arguments) => {
                serve_daemon(state_dir, arguments.codex_binary).await
            }
        },
        Command::McpServer => Err(CliError::new(
            "MCP_NOT_AVAILABLE",
            "MCP server support is not included in this build stage",
        )),
    }
}

async fn doctor(state_dir: &Path, json_output: bool) -> Result<(), CliError> {
    let git = std::process::Command::new("git")
        .arg("--version")
        .output()
        .map_err(|error| CliError::new("GIT_UNAVAILABLE", error.to_string()))?;
    if !git.status.success() {
        return Err(CliError::new(
            "GIT_UNAVAILABLE",
            "git --version did not succeed",
        ));
    }
    let app = connect_app_server().await?;
    let page = app.list_threads(None, 1).await.map_err(app_server_error)?;
    let config = ServerConfig::new(state_dir);
    SqliteRunStore::open(&config.database_path)
        .map_err(|error| CliError::new(error.code(), error.to_string()))?;
    let client = ensure_daemon(&config)
        .await
        .map_err(|error| CliError::new("DAEMON_START_FAILED", error.to_string()))?;
    client
        .ping()
        .await
        .map_err(|error| CliError::new("DAEMON_UNREACHABLE", error.to_string()))?;
    let value = json!({
        "ok": true,
        "git": String::from_utf8_lossy(&git.stdout).trim(),
        "codex_app_server": "compatible",
        "daemon": "reachable",
        "state_dir": state_dir,
        "sampled_threads": page.data.len(),
    });
    emit_value(&value, json_output, || {
        format!(
            "Ready: Git, compatible Codex App Server, private state at {}, and consensus daemon",
            state_dir.display()
        )
    });
    Ok(())
}

async fn list_threads(json_output: bool) -> Result<(), CliError> {
    let app = connect_app_server().await?;
    let threads = all_threads(&app).await?;
    emit_serializable(&threads, json_output, || {
        if threads.is_empty() {
            "No local Codex tasks found.".into()
        } else {
            threads
                .iter()
                .map(task_label)
                .collect::<Vec<_>>()
                .join("\n")
        }
    })
}

async fn start_run(state_dir: &Path, arguments: RunArgs) -> Result<(), CliError> {
    let app = connect_app_server().await?;
    let selected = if let (Some(primary), Some(reviewer)) = (
        arguments.primary_thread.as_deref(),
        arguments.reviewer_thread.as_deref(),
    ) {
        select_explicit(&app, primary, reviewer).await?
    } else {
        if !std::io::stdin().is_terminal() {
            return Err(CliError::new(
                "INTERACTIVE_TTY_REQUIRED",
                "interactive task selection requires a TTY; provide both thread flags for automation",
            ));
        }
        let threads = all_threads(&app).await?;
        select_tasks(
            &threads,
            &GitInspector::default(),
            &mut TerminalTaskSelector::default(),
        )
        .map_err(|error| CliError::new("TASK_SELECTION_FAILED", error.to_string()))?
    };
    let facts = freeze_selected(&selected)?;
    let run_id = facts.run_id.to_string();
    let state = RunState::new(facts);
    let config = ServerConfig::new(state_dir);
    let client = ensure_daemon(&config)
        .await
        .map_err(|error| CliError::new("DAEMON_START_FAILED", error.to_string()))?;
    let response = client
        .request(DaemonRequest::Start {
            state: Box::new(state),
            request: StartRequest {
                integration_branch: arguments.integration_branch,
                test_commands: arguments.test_commands,
            },
        })
        .await
        .map_err(|error| CliError::new("DAEMON_UNREACHABLE", error.to_string()))?;
    let result = response_result(response)?;
    emit_value(&result, arguments.json, || {
        format!(
            "Started consensus run {run_id}. Use `codex-consensus status {run_id}` to follow it."
        )
    });
    Ok(())
}

async fn daemon_request(
    state_dir: &Path,
    request: DaemonRequest,
    json_output: bool,
) -> Result<(), CliError> {
    let config = ServerConfig::new(state_dir);
    let client = ensure_daemon(&config)
        .await
        .map_err(|error| CliError::new("DAEMON_START_FAILED", error.to_string()))?;
    let response = client
        .request(request)
        .await
        .map_err(|error| CliError::new("DAEMON_UNREACHABLE", error.to_string()))?;
    let value = response_result(response)?;
    emit_value(&value, json_output, || human_json(&value));
    Ok(())
}

async fn serve_daemon(state_dir: PathBuf, codex_binary: PathBuf) -> Result<(), CliError> {
    let config = ServerConfig::new(&state_dir);
    let store = SqliteRunStore::open(&config.database_path)
        .map_err(|error| CliError::new(error.code(), error.to_string()))?;
    let codex_binary = if codex_binary == Path::new("codex") {
        std::env::var_os("CODEX_CONSENSUS_CODEX_BINARY")
            .map(PathBuf::from)
            .unwrap_or(codex_binary)
    } else {
        codex_binary
    };
    let app = Arc::new(
        CodexAppServer::connect(ConnectOptions {
            codex_binary,
            ..ConnectOptions::default()
        })
        .await
        .map_err(app_server_error)?,
    );
    let coordinator = Coordinator::new(
        app,
        store.clone(),
        Arc::new(GitRepositorySafety::default()),
        CoordinatorOptions::default(),
    );
    let controller: Arc<dyn RunController> = Arc::new(coordinator);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let result = run_server_with_controller(config, store, controller, shutdown_rx)
        .await
        .map_err(|error| CliError::new("DAEMON_FAILED", error.to_string()));
    drop(shutdown_tx);
    result
}

async fn connect_app_server() -> Result<CodexAppServer, CliError> {
    let codex_binary = std::env::var_os("CODEX_CONSENSUS_CODEX_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("codex"));
    CodexAppServer::connect(ConnectOptions {
        codex_binary,
        ..ConnectOptions::default()
    })
    .await
    .map_err(app_server_error)
}

async fn all_threads(app: &impl AppServer) -> Result<Vec<ThreadSummary>, CliError> {
    let mut threads = Vec::new();
    let mut cursor = None;
    let mut seen = HashSet::new();
    loop {
        let page = app
            .list_threads(cursor.clone(), 100)
            .await
            .map_err(app_server_error)?;
        threads.extend(page.data);
        let Some(next) = page.next_cursor else {
            break;
        };
        if !seen.insert(next.clone()) {
            return Err(CliError::new(
                "INVALID_APP_SERVER_RESPONSE",
                "thread/list repeated a pagination cursor",
            ));
        }
        cursor = Some(next);
    }
    Ok(threads)
}

async fn select_explicit(
    app: &impl AppServer,
    primary_id: &str,
    reviewer_id: &str,
) -> Result<SelectedTasks, CliError> {
    if primary_id == reviewer_id {
        return Err(CliError::new(
            "AMBIGUOUS_THREAD",
            "primary and reviewer task IDs must differ",
        ));
    }
    let primary = app
        .read_thread(primary_id)
        .await
        .map_err(app_server_error)?;
    let reviewer = app
        .read_thread(reviewer_id)
        .await
        .map_err(app_server_error)?;
    selected_from_details(primary, reviewer)
}

fn selected_from_details(
    primary: ThreadDetail,
    reviewer: ThreadDetail,
) -> Result<SelectedTasks, CliError> {
    let inspector = GitInspector::default();
    let primary_snapshot = inspector
        .inspect_worktree(&primary.summary.cwd)
        .map_err(git_error)?;
    let reviewer_snapshot = inspector
        .inspect_worktree(&reviewer.summary.cwd)
        .map_err(git_error)?;
    verify_same_repository(&primary_snapshot, &reviewer_snapshot).map_err(git_error)?;
    Ok(SelectedTasks {
        primary: primary.summary,
        reviewer: reviewer.summary,
        primary_snapshot,
        reviewer_snapshot,
    })
}

fn freeze_selected(selected: &SelectedTasks) -> Result<RunFacts, CliError> {
    let facts = RunFacts {
        run_id: Uuid::new_v4(),
        primary_thread_id: selected.primary.id.clone(),
        reviewer_thread_id: selected.reviewer.id.clone(),
        primary_worktree: selected.primary_snapshot.worktree.clone(),
        reviewer_worktree: selected.reviewer_snapshot.worktree.clone(),
        git_common_dir: selected.primary_snapshot.common_dir.clone(),
        primary_sha: selected.primary_snapshot.head_sha.clone(),
        reviewer_sha: selected.reviewer_snapshot.head_sha.clone(),
        primary_ref: source_ref_name(&selected.primary_snapshot),
        reviewer_ref: source_ref_name(&selected.reviewer_snapshot),
    };
    verify_frozen_sources(
        &facts,
        &selected.primary_snapshot,
        &selected.reviewer_snapshot,
    )
    .map_err(git_error)?;
    Ok(facts)
}

fn source_ref_name(snapshot: &WorktreeSnapshot) -> Option<String> {
    snapshot
        .source_ref
        .as_ref()
        .map(|source| source.name.clone())
}

fn response_result(response: DaemonResponse) -> Result<Value, CliError> {
    if response.ok {
        response.result.ok_or_else(|| {
            CliError::new("INVALID_DAEMON_RESPONSE", "daemon response has no result")
        })
    } else {
        let error = response.error.ok_or_else(|| {
            CliError::new("INVALID_DAEMON_RESPONSE", "daemon response has no error")
        })?;
        Err(CliError::new(error.code, error.message))
    }
}

fn app_server_error(error: app_server_client::AppServerError) -> CliError {
    let code = match &error {
        app_server_client::AppServerError::IncompatibleCodex(_) => "INCOMPATIBLE_CODEX",
        _ => "APP_SERVER_FAILURE",
    };
    CliError::new(code, error.to_string())
}

fn git_error(error: consensus_core::git::GitSafetyError) -> CliError {
    CliError::new(error.code(), error.to_string())
}
