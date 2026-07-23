mod args;
mod installation;
mod output;
mod select;

use std::{
    collections::HashSet,
    io::IsTerminal,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use app_server_client::{
    AppServer, CONTROLLED_PATCH_APPROVAL_KEY, CONTROLLED_PATCH_APPROVAL_MODE, CodexAppServer,
    ConnectOptions, ReconnectingCodexAppServer, ThreadDetail, ThreadSummary,
};
use args::{Cli, Command, DaemonCommand, RunArgs, ThreadsCommand, WorktreesCommand};
use async_trait::async_trait;
use clap::Parser;
use consensus_core::{
    git::{GitInspector, WorktreeSnapshot, verify_frozen_sources},
    state::{RunFacts, RunState},
};
use consensus_daemon::{
    coordinator::{Coordinator, CoordinatorOptions, GitRepositorySafety, StartRequest},
    lifecycle::{default_state_dir, ensure_daemon},
    server::{RunController, ServerConfig, run_server_with_controller},
    store::SqliteRunStore,
    wire::{DaemonRequest, DaemonResponse},
};
use consensus_mcp_server::{
    BackendError, ToolBackend, ToolSurface, serve_stdio, serve_stdio_surface,
};
use installation::{DoctorSurface, inspect_effective_legacy_skill};
use output::{emit_error, emit_serializable, emit_value, human_json};
use select::{
    SelectedBinding, SelectedTasks, TaskSelector, TerminalTaskSelector, confirm_binding,
    select_tasks, select_valid_worktrees, task_label, worktree_label,
};
use serde::Deserialize;
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

    pub fn message(&self) -> &str {
        &self.message
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
        Command::Configure(arguments) => configure_codex(arguments.json).await,
        Command::Threads(arguments) => match arguments.command {
            ThreadsCommand::List(output) => list_threads(output.json).await,
        },
        Command::Worktrees(arguments) => match arguments.command {
            WorktreesCommand::List(arguments) => {
                list_worktrees(&arguments.repository, arguments.json)
            }
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
        Command::McpServer => serve_mcp(state_dir).await,
        Command::ParticipantMcpServer => serve_participant_mcp(state_dir).await,
    }
}

async fn doctor(state_dir: &Path, json_output: bool) -> Result<(), CliError> {
    let value = doctor_value(state_dir, DoctorSurface::DirectCli).await?;
    emit_value(&value, json_output, || {
        format!(
            "Ready: Git, compatible Codex App Server, private state at {}, and consensus daemon",
            state_dir.display()
        )
    });
    Ok(())
}

async fn doctor_value(state_dir: &Path, surface: DoctorSurface) -> Result<Value, CliError> {
    let legacy_skill = inspect_effective_legacy_skill(surface)
        .map_err(|error| CliError::new(error.code(), error.to_string()))?;
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
    let controlled_patch_approval = require_controlled_patch_approval(&app).await?;
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
    let daemon_health = client
        .request(DaemonRequest::Health)
        .await
        .map_err(|error| CliError::new("DAEMON_UNREACHABLE", error.to_string()))?;
    response_result(daemon_health)?;
    let value = json!({
        "ok": true,
        "git": String::from_utf8_lossy(&git.stdout).trim(),
        "codex_app_server": "compatible",
        "daemon": "reachable",
        "daemon_app_server": "reachable",
        "state_dir": state_dir,
        "sampled_threads": page.data.len(),
        "controlled_patch_approval": {
            "key": CONTROLLED_PATCH_APPROVAL_KEY,
            "mode": controlled_patch_approval,
        },
        "plugin_surface": surface == DoctorSurface::PluginMcp,
        "legacy_skill": legacy_skill,
    });
    Ok(value)
}

async fn configure_codex(json_output: bool) -> Result<(), CliError> {
    let app = connect_app_server().await?;
    let response = app
        .configure_controlled_patch_approval()
        .await
        .map_err(app_server_error)?;
    let file_path = response
        .get("filePath")
        .and_then(Value::as_str)
        .unwrap_or("the user Codex config");
    let value = json!({
        "ok": true,
        "key": CONTROLLED_PATCH_APPROVAL_KEY,
        "mode": CONTROLLED_PATCH_APPROVAL_MODE,
        "file_path": file_path,
        "reload_user_config": true,
        "write_status": response.get("status"),
    });
    emit_value(&value, json_output, || {
        format!(
            "Configured only consensus_apply_patch for automatic approval in {file_path}; loaded tasks were hot-reloaded."
        )
    });
    Ok(())
}

async fn require_controlled_patch_approval(app: &impl AppServer) -> Result<String, CliError> {
    let mode = app
        .controlled_patch_approval_mode()
        .await
        .map_err(app_server_error)?;
    if mode.as_deref() != Some(CONTROLLED_PATCH_APPROVAL_MODE) {
        return Err(CliError::new(
            "APPROVAL_CONFIGURATION_REQUIRED",
            format!(
                "{CONTROLLED_PATCH_APPROVAL_KEY} must equal {CONTROLLED_PATCH_APPROVAL_MODE}; run `codex-consensus configure` once, then retry the same run"
            ),
        ));
    }
    Ok(CONTROLLED_PATCH_APPROVAL_MODE.to_owned())
}

async fn list_threads(json_output: bool) -> Result<(), CliError> {
    let threads = local_threads().await?;
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

async fn list_threads_value() -> Result<Value, CliError> {
    let threads = local_threads().await?;
    serde_json::to_value(threads)
        .map_err(|error| CliError::new("SERIALIZATION_FAILURE", error.to_string()))
}

async fn local_threads() -> Result<Vec<ThreadSummary>, CliError> {
    let app = connect_app_server().await?;
    all_threads(&app).await
}

fn list_worktrees(repository: &Path, json_output: bool) -> Result<(), CliError> {
    let value = list_worktrees_value(repository)?;
    emit_value(&value, json_output, || {
        value["worktrees"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| serde_json::from_value(entry.clone()).ok())
            .map(|entry| worktree_label(&entry))
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(())
}

fn list_worktrees_value(repository: &Path) -> Result<Value, CliError> {
    let worktrees = GitInspector::default()
        .list_registered_worktrees(repository)
        .map_err(git_error)?;
    Ok(json!({"worktrees": worktrees}))
}

async fn start_run(state_dir: &Path, arguments: RunArgs) -> Result<(), CliError> {
    let json_output = arguments.json;
    let result = start_run_value(state_dir, &arguments).await?;
    let run_id = result
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    emit_value(&result, json_output, || {
        format!(
            "Started consensus run {run_id}. Use `codex-consensus status {run_id}` to follow it."
        )
    });
    Ok(())
}

async fn start_run_value(state_dir: &Path, arguments: &RunArgs) -> Result<Value, CliError> {
    let has_tasks = arguments.primary_thread.is_some() && arguments.reviewer_thread.is_some();
    let has_worktrees =
        arguments.primary_worktree.is_some() && arguments.reviewer_worktree.is_some();
    let needs_interaction = !has_tasks || !has_worktrees;
    if needs_interaction && (arguments.json || !std::io::stdin().is_terminal()) {
        return Err(CliError::new(
            "INTERACTIVE_TTY_REQUIRED",
            "non-interactive runs require all four binding flags: --primary-thread, --reviewer-thread, --primary-worktree, and --reviewer-worktree",
        ));
    }

    let app = connect_app_server().await?;
    require_controlled_patch_approval(&app).await?;
    let mut selector = TerminalTaskSelector::default();
    let tasks = if let (Some(primary), Some(reviewer)) = (
        arguments.primary_thread.as_deref(),
        arguments.reviewer_thread.as_deref(),
    ) {
        select_explicit_tasks(&app, primary, reviewer).await?
    } else {
        let threads = all_threads(&app).await?;
        select_tasks(&threads, &mut selector)
            .map_err(|error| CliError::new("TASK_SELECTION_FAILED", error.to_string()))?
    };
    let inspector = GitInspector::default();
    let (primary_snapshot, reviewer_snapshot) = if let (Some(primary), Some(reviewer)) = (
        arguments.primary_worktree.as_ref(),
        arguments.reviewer_worktree.as_ref(),
    ) {
        inspector
            .inspect_registered_pair(primary, reviewer)
            .map_err(git_error)?
    } else {
        let entries = interactive_worktrees(arguments, &inspector, &mut selector)?;
        select_valid_worktrees(&entries, &inspector, &mut selector)
            .map_err(|error| CliError::new("WORKTREE_SELECTION_FAILED", error.to_string()))?
    };
    let selected = SelectedBinding {
        tasks,
        primary_snapshot,
        reviewer_snapshot,
    };
    if needs_interaction {
        confirm_binding(&selected, &mut selector)
            .map_err(|error| CliError::new("SELECTION_CANCELLED", error.to_string()))?;
    }
    let facts = freeze_selected(&selected)?;
    let state = RunState::new(facts);
    let config = ServerConfig::new(state_dir);
    let client = ensure_daemon(&config)
        .await
        .map_err(|error| CliError::new("DAEMON_START_FAILED", error.to_string()))?;
    let response = client
        .request(DaemonRequest::Start {
            state: Box::new(state),
            request: StartRequest {
                integration_branch: arguments.integration_branch.clone(),
                test_commands: arguments.test_commands.clone(),
            },
        })
        .await
        .map_err(|error| CliError::new("DAEMON_UNREACHABLE", error.to_string()))?;
    response_result(response)
}

async fn daemon_request(
    state_dir: &Path,
    request: DaemonRequest,
    json_output: bool,
) -> Result<(), CliError> {
    let value = daemon_request_value(state_dir, request).await?;
    emit_value(&value, json_output, || human_json(&value));
    Ok(())
}

async fn daemon_request_value(state_dir: &Path, request: DaemonRequest) -> Result<Value, CliError> {
    let config = ServerConfig::new(state_dir);
    let client = ensure_daemon(&config)
        .await
        .map_err(|error| CliError::new("DAEMON_START_FAILED", error.to_string()))?;
    let response = client
        .request(request)
        .await
        .map_err(|error| CliError::new("DAEMON_UNREACHABLE", error.to_string()))?;
    response_result(response)
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
        ReconnectingCodexAppServer::connect(ConnectOptions {
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

async fn serve_mcp(state_dir: PathBuf) -> Result<(), CliError> {
    serve_stdio(Arc::new(CliMcpBackend { state_dir }))
        .await
        .map_err(|error| CliError::new("MCP_SERVER_FAILED", error.to_string()))
}

async fn serve_participant_mcp(state_dir: PathBuf) -> Result<(), CliError> {
    serve_stdio_surface(
        Arc::new(CliMcpBackend { state_dir }),
        ToolSurface::ParticipantPatch,
    )
    .await
    .map_err(|error| CliError::new("MCP_SERVER_FAILED", error.to_string()))
}

struct CliMcpBackend {
    state_dir: PathBuf,
}

#[async_trait]
impl ToolBackend for CliMcpBackend {
    async fn call(&self, tool: &str, arguments: Value) -> Result<Value, BackendError> {
        let result = match tool {
            "consensus_doctor" => doctor_value(&self.state_dir, DoctorSurface::PluginMcp).await,
            "consensus_list_threads" => list_threads_value()
                .await
                .map(|threads| json!({"threads": threads})),
            "consensus_list_worktrees" => {
                let arguments: McpWorktreeListArguments = decode_mcp_arguments(arguments)?;
                list_worktrees_value(Path::new(&arguments.repository_path))
            }
            "consensus_start" => {
                let arguments: McpStartArguments = decode_mcp_arguments(arguments)?;
                start_run_value(
                    &self.state_dir,
                    &RunArgs {
                        primary_thread: Some(arguments.primary_thread),
                        reviewer_thread: Some(arguments.reviewer_thread),
                        primary_worktree: Some(PathBuf::from(arguments.primary_worktree)),
                        reviewer_worktree: Some(PathBuf::from(arguments.reviewer_worktree)),
                        repository: None,
                        integration_branch: arguments.integration_branch,
                        test_commands: arguments.test_commands,
                        json: true,
                    },
                )
                .await
            }
            "consensus_status" => {
                let arguments: McpStatusArguments = decode_mcp_arguments(arguments)?;
                daemon_request_value(
                    &self.state_dir,
                    DaemonRequest::Status {
                        run_id: arguments.run_id,
                    },
                )
                .await
            }
            "consensus_resume" => {
                let arguments: McpRunIdArguments = decode_mcp_arguments(arguments)?;
                daemon_request_value(
                    &self.state_dir,
                    DaemonRequest::Resume {
                        run_id: arguments.run_id,
                    },
                )
                .await
            }
            "consensus_apply_patch" => {
                let arguments: McpApplyPatchArguments = decode_mcp_arguments(arguments)?;
                daemon_request_value(
                    &self.state_dir,
                    DaemonRequest::ApplyPatch {
                        run_id: arguments.run_id,
                        request_hash: arguments.request_hash,
                        patch: arguments.patch,
                    },
                )
                .await
            }
            "consensus_cancel" => {
                let arguments: McpRunIdArguments = decode_mcp_arguments(arguments)?;
                daemon_request_value(
                    &self.state_dir,
                    DaemonRequest::Cancel {
                        run_id: arguments.run_id,
                    },
                )
                .await
            }
            _ => {
                return Err(BackendError::new(
                    "UNKNOWN_TOOL",
                    format!("unsupported MCP tool {tool}"),
                ));
            }
        };
        result.map_err(cli_backend_error)
    }
}

#[derive(Deserialize)]
struct McpStartArguments {
    primary_thread: String,
    reviewer_thread: String,
    primary_worktree: String,
    reviewer_worktree: String,
    integration_branch: Option<String>,
    #[serde(default)]
    test_commands: Vec<String>,
}

#[derive(Deserialize)]
struct McpWorktreeListArguments {
    repository_path: String,
}

#[derive(Deserialize)]
struct McpApplyPatchArguments {
    run_id: String,
    request_hash: String,
    patch: String,
}

#[derive(Deserialize)]
struct McpStatusArguments {
    run_id: Option<String>,
}

#[derive(Deserialize)]
struct McpRunIdArguments {
    run_id: String,
}

fn decode_mcp_arguments<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, BackendError> {
    serde_json::from_value(value)
        .map_err(|error| BackendError::new("INVALID_ARGUMENTS", error.to_string()))
}

fn cli_backend_error(error: CliError) -> BackendError {
    BackendError::new(error.code(), error.message())
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

fn interactive_worktrees(
    arguments: &RunArgs,
    inspector: &GitInspector,
    selector: &mut impl TaskSelector,
) -> Result<Vec<consensus_core::git::RegisteredWorktree>, CliError> {
    if let Some(repository) = &arguments.repository {
        return inspector
            .list_registered_worktrees(repository)
            .map_err(git_error);
    }
    if let Ok(current) = std::env::current_dir() {
        if let Ok(entries) = inspector.list_registered_worktrees(&current) {
            return Ok(entries);
        }
    }
    let repository = selector
        .input_repository()
        .map_err(|error| CliError::new("WORKTREE_SELECTION_FAILED", error.to_string()))?;
    inspector
        .list_registered_worktrees(repository)
        .map_err(git_error)
}

async fn select_explicit_tasks(
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
    selected_from_details(primary_id, reviewer_id, primary, reviewer)
}

fn selected_from_details(
    primary_id: &str,
    reviewer_id: &str,
    primary: ThreadDetail,
    reviewer: ThreadDetail,
) -> Result<SelectedTasks, CliError> {
    if primary.summary.id != primary_id || reviewer.summary.id != reviewer_id {
        return Err(CliError::new(
            "AMBIGUOUS_THREAD",
            "App Server returned a different task than requested",
        ));
    }
    Ok(SelectedTasks {
        primary: primary.summary,
        reviewer: reviewer.summary,
    })
}

fn freeze_selected(selected: &SelectedBinding) -> Result<RunFacts, CliError> {
    let facts = RunFacts {
        run_id: Uuid::new_v4(),
        primary_thread_id: selected.tasks.primary.id.clone(),
        reviewer_thread_id: selected.tasks.reviewer.id.clone(),
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
