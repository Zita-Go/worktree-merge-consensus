use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "codex-consensus",
    version,
    about = "Coordinate reviewed integration across two existing Codex tasks"
)]
pub struct Cli {
    #[arg(long, global = true, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    pub fn validate(&self) -> Result<(), String> {
        if let Command::Run(arguments) = &self.command {
            let primary = arguments.primary_thread.is_some();
            let reviewer = arguments.reviewer_thread.is_some();
            if primary != reviewer {
                return Err(
                    "--primary-thread and --reviewer-thread must be provided together".into(),
                );
            }
            let primary_worktree = arguments.primary_worktree.is_some();
            let reviewer_worktree = arguments.reviewer_worktree.is_some();
            if primary_worktree != reviewer_worktree {
                return Err(
                    "--primary-worktree and --reviewer-worktree must be provided together".into(),
                );
            }
            if arguments.json && !(primary && reviewer && primary_worktree && reviewer_worktree) {
                return Err(
                    "JSON runs require all four binding flags: --primary-thread, --reviewer-thread, --primary-worktree, and --reviewer-worktree"
                        .into(),
                );
            }
        }
        if let Command::Watch(arguments) = &self.command {
            if arguments.after_cursor < 0 {
                return Err("--after-cursor must be at least 0".into());
            }
        }
        Ok(())
    }

    pub fn json_output(&self) -> bool {
        match &self.command {
            Command::Doctor(arguments) => arguments.json,
            Command::Configure(arguments) => arguments.json,
            Command::Threads(arguments) => match &arguments.command {
                ThreadsCommand::List(output) => output.json,
            },
            Command::Worktrees(arguments) => match &arguments.command {
                WorktreesCommand::List(output) => output.json,
            },
            Command::Run(arguments) => arguments.json,
            Command::Status(arguments) => arguments.json,
            Command::Watch(arguments) => arguments.json,
            Command::Resume(arguments) | Command::Cancel(arguments) => arguments.json,
            Command::Daemon(_) | Command::McpServer | Command::ParticipantMcpServer => false,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Verify local Codex, Git, state, and daemon compatibility.
    Doctor(OutputArgs),
    /// Configure the single controlled patch tool for unattended participant turns.
    Configure(OutputArgs),
    /// List locally available Codex tasks.
    Threads(ThreadsArgs),
    /// List registered Git worktrees in one repository.
    Worktrees(WorktreesArgs),
    /// Start a reviewed integration run.
    Run(RunArgs),
    /// Show one run or all runs.
    Status(StatusArgs),
    /// Follow the public consensus event stream for one run.
    Watch(WatchArgs),
    /// Resume a paused run after user action.
    Resume(RunIdArgs),
    /// Cancel a run without reverting or deleting Git state.
    Cancel(RunIdArgs),
    #[command(hide = true)]
    Daemon(DaemonArgs),
    #[command(name = "mcp-server", hide = true)]
    McpServer,
    #[command(name = "participant-mcp-server", hide = true)]
    ParticipantMcpServer,
}

#[derive(Debug, Args)]
pub struct OutputArgs {
    /// Emit exactly one JSON value.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ThreadsArgs {
    #[command(subcommand)]
    pub command: ThreadsCommand,
}

#[derive(Debug, Subcommand)]
pub enum ThreadsCommand {
    /// List tasks visible to the local Codex App Server.
    List(OutputArgs),
}

#[derive(Debug, Args)]
pub struct WorktreesArgs {
    #[command(subcommand)]
    pub command: WorktreesCommand,
}

#[derive(Debug, Subcommand)]
pub enum WorktreesCommand {
    /// List registered worktrees without pruning or repairing them.
    List(WorktreeListArgs),
}

#[derive(Debug, Args)]
pub struct WorktreeListArgs {
    #[arg(long, value_name = "PATH")]
    pub repository: PathBuf,
    /// Emit exactly one JSON value.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[arg(long, value_name = "THREAD_ID")]
    pub primary_thread: Option<String>,
    #[arg(long, value_name = "THREAD_ID")]
    pub reviewer_thread: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub primary_worktree: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub reviewer_worktree: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub repository: Option<PathBuf>,
    #[arg(long, value_name = "NEW_BRANCH")]
    pub integration_branch: Option<String>,
    #[arg(long = "test", value_name = "COMMAND")]
    pub test_commands: Vec<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    pub run_id: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct WatchArgs {
    pub run_id: String,
    /// Resume after a previously returned event cursor.
    #[arg(long, default_value_t = 0)]
    pub after_cursor: i64,
    /// Long-poll timeout for each request, in milliseconds.
    #[arg(long, default_value_t = 25_000, value_parser = clap::value_parser!(u64).range(0..=30_000))]
    pub timeout_ms: u64,
    /// Return after one event batch or timeout instead of following the run.
    #[arg(long)]
    pub once: bool,
    /// Emit each public event batch as one JSON line.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RunIdArgs {
    pub run_id: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct DaemonArgs {
    #[command(subcommand)]
    pub command: DaemonCommand,
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Serve the private Unix socket and coordinate runs.
    Serve(DaemonServeArgs),
}

#[derive(Debug, Args)]
pub struct DaemonServeArgs {
    #[arg(long, value_name = "PATH", default_value = "codex")]
    pub codex_binary: PathBuf,
}
