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
        }
        Ok(())
    }

    pub fn json_output(&self) -> bool {
        match &self.command {
            Command::Doctor(arguments) => arguments.json,
            Command::Threads(arguments) => match &arguments.command {
                ThreadsCommand::List(output) => output.json,
            },
            Command::Run(arguments) => arguments.json,
            Command::Status(arguments) => arguments.json,
            Command::Resume(arguments) | Command::Cancel(arguments) => arguments.json,
            Command::Daemon(_) | Command::McpServer => false,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Verify local Codex, Git, state, and daemon compatibility.
    Doctor(OutputArgs),
    /// List locally available Codex tasks.
    Threads(ThreadsArgs),
    /// Start a reviewed integration run.
    Run(RunArgs),
    /// Show one run or all runs.
    Status(StatusArgs),
    /// Resume a paused run after user action.
    Resume(RunIdArgs),
    /// Cancel a run without reverting or deleting Git state.
    Cancel(RunIdArgs),
    #[command(hide = true)]
    Daemon(DaemonArgs),
    #[command(name = "mcp-server", hide = true)]
    McpServer,
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
pub struct RunArgs {
    #[arg(long, value_name = "THREAD_ID")]
    pub primary_thread: Option<String>,
    #[arg(long, value_name = "THREAD_ID")]
    pub reviewer_thread: Option<String>,
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
