use std::path::PathBuf;

use app_server_client::ThreadSummary;
use consensus_core::git::{RegisteredWorktree, WorktreeSnapshot};
use dialoguer::{Confirm, FuzzySelect, Input, theme::ColorfulTheme};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct SelectedTasks {
    pub primary: ThreadSummary,
    pub reviewer: ThreadSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedWorktrees {
    pub primary: PathBuf,
    pub reviewer: PathBuf,
}

#[derive(Debug, Clone)]
pub struct SelectedBinding {
    pub tasks: SelectedTasks,
    pub primary_snapshot: WorktreeSnapshot,
    pub reviewer_snapshot: WorktreeSnapshot,
}

#[derive(Debug, Error)]
pub enum SelectionError {
    #[error("no Codex tasks are available")]
    NoTasks,
    #[error("no different Codex task is available for review")]
    NoReviewer,
    #[error("fewer than two available registered worktrees can be selected")]
    NoWorktrees,
    #[error("task selection was cancelled")]
    Cancelled,
    #[error("terminal selection failed: {0}")]
    Terminal(String),
    #[error(transparent)]
    Git(#[from] consensus_core::git::GitSafetyError),
}

pub trait TaskSelector {
    fn select_primary(&mut self, labels: &[String]) -> Result<usize, SelectionError>;
    fn select_reviewer(&mut self, labels: &[String]) -> Result<usize, SelectionError>;
    fn select_primary_worktree(&mut self, labels: &[String]) -> Result<usize, SelectionError>;
    fn select_reviewer_worktree(&mut self, labels: &[String]) -> Result<usize, SelectionError>;
    fn input_repository(&mut self) -> Result<PathBuf, SelectionError>;
    fn confirm(&mut self, summary: &str) -> Result<bool, SelectionError>;
}

#[derive(Default)]
pub struct TerminalTaskSelector {
    theme: ColorfulTheme,
}

impl TaskSelector for TerminalTaskSelector {
    fn select_primary(&mut self, labels: &[String]) -> Result<usize, SelectionError> {
        FuzzySelect::with_theme(&self.theme)
            .with_prompt("Select the primary Codex task")
            .items(labels)
            .interact()
            .map_err(|error| SelectionError::Terminal(error.to_string()))
    }

    fn select_reviewer(&mut self, labels: &[String]) -> Result<usize, SelectionError> {
        FuzzySelect::with_theme(&self.theme)
            .with_prompt("Select the reviewer Codex task")
            .items(labels)
            .interact()
            .map_err(|error| SelectionError::Terminal(error.to_string()))
    }

    fn select_primary_worktree(&mut self, labels: &[String]) -> Result<usize, SelectionError> {
        FuzzySelect::with_theme(&self.theme)
            .with_prompt("Select the primary source worktree")
            .items(labels)
            .interact()
            .map_err(|error| SelectionError::Terminal(error.to_string()))
    }

    fn select_reviewer_worktree(&mut self, labels: &[String]) -> Result<usize, SelectionError> {
        FuzzySelect::with_theme(&self.theme)
            .with_prompt("Select the reviewer source worktree")
            .items(labels)
            .interact()
            .map_err(|error| SelectionError::Terminal(error.to_string()))
    }

    fn input_repository(&mut self) -> Result<PathBuf, SelectionError> {
        Input::<String>::with_theme(&self.theme)
            .with_prompt("Absolute path to any worktree in the source repository")
            .interact_text()
            .map(PathBuf::from)
            .map_err(|error| SelectionError::Terminal(error.to_string()))
    }

    fn confirm(&mut self, summary: &str) -> Result<bool, SelectionError> {
        Confirm::with_theme(&self.theme)
            .with_prompt(summary)
            .default(false)
            .interact()
            .map_err(|error| SelectionError::Terminal(error.to_string()))
    }
}

pub fn select_tasks(
    threads: &[ThreadSummary],
    selector: &mut impl TaskSelector,
) -> Result<SelectedTasks, SelectionError> {
    if threads.is_empty() {
        return Err(SelectionError::NoTasks);
    }
    let labels = threads.iter().map(task_label).collect::<Vec<_>>();
    let primary_index = selector.select_primary(&labels)?;
    let primary = threads
        .get(primary_index)
        .cloned()
        .ok_or_else(|| SelectionError::Terminal("invalid primary selection index".into()))?;

    let reviewer_candidates = threads
        .iter()
        .filter(|thread| thread.id != primary.id)
        .cloned()
        .collect::<Vec<_>>();
    if reviewer_candidates.is_empty() {
        return Err(SelectionError::NoReviewer);
    }
    let reviewer_labels = reviewer_candidates
        .iter()
        .map(task_label)
        .collect::<Vec<_>>();
    let reviewer_index = selector.select_reviewer(&reviewer_labels)?;
    let reviewer = reviewer_candidates
        .get(reviewer_index)
        .cloned()
        .ok_or_else(|| SelectionError::Terminal("invalid reviewer selection index".into()))?;

    Ok(SelectedTasks { primary, reviewer })
}

pub fn select_worktrees(
    entries: &[RegisteredWorktree],
    selector: &mut impl TaskSelector,
) -> Result<SelectedWorktrees, SelectionError> {
    let candidates = entries
        .iter()
        .filter(|entry| !entry.bare && entry.issue.is_none())
        .collect::<Vec<_>>();
    if candidates.len() < 2 {
        return Err(SelectionError::NoWorktrees);
    }
    let labels = candidates
        .iter()
        .map(|entry| worktree_label(entry))
        .collect::<Vec<_>>();
    let primary_index = selector.select_primary_worktree(&labels)?;
    let primary = candidates
        .get(primary_index)
        .ok_or_else(|| SelectionError::Terminal("invalid primary worktree index".into()))?;
    let reviewer_candidates = candidates
        .iter()
        .filter(|entry| entry.worktree != primary.worktree)
        .copied()
        .collect::<Vec<_>>();
    let reviewer_labels = reviewer_candidates
        .iter()
        .map(|entry| worktree_label(entry))
        .collect::<Vec<_>>();
    let reviewer_index = selector.select_reviewer_worktree(&reviewer_labels)?;
    let reviewer = reviewer_candidates
        .get(reviewer_index)
        .ok_or_else(|| SelectionError::Terminal("invalid reviewer worktree index".into()))?;
    Ok(SelectedWorktrees {
        primary: primary.worktree.clone(),
        reviewer: reviewer.worktree.clone(),
    })
}

pub fn confirm_binding(
    binding: &SelectedBinding,
    selector: &mut impl TaskSelector,
) -> Result<(), SelectionError> {
    let summary = format!(
        "Use primary task {} with {} at {} and reviewer task {} with {} at {} in repository {}?",
        short_id(&binding.tasks.primary.id),
        binding.primary_snapshot.worktree.display(),
        short_sha(&binding.primary_snapshot.head_sha),
        short_id(&binding.tasks.reviewer.id),
        binding.reviewer_snapshot.worktree.display(),
        short_sha(&binding.reviewer_snapshot.head_sha),
        binding.primary_snapshot.common_dir.display(),
    );
    if selector.confirm(&summary)? {
        Ok(())
    } else {
        Err(SelectionError::Cancelled)
    }
}

pub fn task_label(thread: &ThreadSummary) -> String {
    let title = thread.name.as_deref().unwrap_or_else(|| {
        if thread.preview.is_empty() {
            "Untitled task"
        } else {
            &thread.preview
        }
    });
    let status = thread
        .status
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    format!(
        "{title} | {status} | updated:{} | {} | {}",
        thread.updated_at,
        thread.cwd.display(),
        short_id(&thread.id)
    )
}

pub fn worktree_label(entry: &RegisteredWorktree) -> String {
    let source = entry
        .source_ref
        .as_ref()
        .map(|source| source.name.as_str())
        .unwrap_or("detached");
    let head = entry
        .head_sha
        .as_deref()
        .map(short_sha)
        .unwrap_or("unknown");
    let state = match (entry.clean, entry.issue.as_ref()) {
        (_, Some(issue)) => issue.code.as_str(),
        (Some(true), None) => "clean",
        (Some(false), None) => "dirty",
        (None, None) if entry.bare => "bare",
        (None, None) => "unknown",
    };
    format!("{} | {source} | {head} | {state}", entry.worktree.display())
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        process::Command,
    };

    use consensus_core::git::GitInspector;
    use serde_json::json;

    use super::*;

    #[test]
    fn task_selection_does_not_infer_sources_from_task_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let shared_non_git_cwd = temp.path().join("task-home");
        fs::create_dir(&shared_non_git_cwd).unwrap();
        let threads = vec![
            thread("primary-thread", &shared_non_git_cwd),
            thread("reviewer-thread", &shared_non_git_cwd),
        ];
        let mut selector = FixedSelector {
            primary_task: 0,
            reviewer_task: 0,
            primary_worktree: 0,
            reviewer_worktree: 0,
            confirmed: true,
        };

        let selected = select_tasks(&threads, &mut selector).unwrap();

        assert_eq!(selected.primary.id, "primary-thread");
        assert_eq!(selected.reviewer.id, "reviewer-thread");
        assert_eq!(selected.primary.cwd, shared_non_git_cwd);
        assert_eq!(selected.reviewer.cwd, shared_non_git_cwd);
    }

    #[test]
    fn worktree_selection_is_independent_from_selected_tasks() {
        let fixture = WorktreeFixture::new();
        let entries = GitInspector::default()
            .list_registered_worktrees(&fixture.repository)
            .unwrap();
        let primary_worktree = entries
            .iter()
            .position(|entry| entry.worktree.ends_with("primary"))
            .unwrap();
        let mut selector = FixedSelector {
            primary_task: 0,
            reviewer_task: 0,
            primary_worktree,
            reviewer_worktree: 0,
            confirmed: true,
        };

        let selected = select_worktrees(&entries, &mut selector).unwrap();

        assert!(selected.primary.ends_with("primary"));
        assert_ne!(selected.primary, selected.reviewer);
    }

    struct FixedSelector {
        primary_task: usize,
        reviewer_task: usize,
        primary_worktree: usize,
        reviewer_worktree: usize,
        confirmed: bool,
    }

    impl TaskSelector for FixedSelector {
        fn select_primary(&mut self, _labels: &[String]) -> Result<usize, SelectionError> {
            Ok(self.primary_task)
        }

        fn select_reviewer(&mut self, labels: &[String]) -> Result<usize, SelectionError> {
            assert_eq!(labels.len(), 1);
            Ok(self.reviewer_task)
        }

        fn select_primary_worktree(&mut self, _labels: &[String]) -> Result<usize, SelectionError> {
            Ok(self.primary_worktree)
        }

        fn select_reviewer_worktree(&mut self, labels: &[String]) -> Result<usize, SelectionError> {
            assert!(!labels.is_empty());
            Ok(self.reviewer_worktree)
        }

        fn input_repository(&mut self) -> Result<PathBuf, SelectionError> {
            unreachable!("repository prompting is not used by these unit tests")
        }

        fn confirm(&mut self, _summary: &str) -> Result<bool, SelectionError> {
            Ok(self.confirmed)
        }
    }

    struct WorktreeFixture {
        _root: tempfile::TempDir,
        repository: PathBuf,
    }

    impl WorktreeFixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let repository = root.path().join("repository");
            let primary = root.path().join("primary");
            let reviewer = root.path().join("reviewer");
            fs::create_dir(&repository).unwrap();
            run_git(&repository, &["init", "--initial-branch=base"]);
            run_git(&repository, &["config", "user.name", "Test User"]);
            run_git(
                &repository,
                &["config", "user.email", "test@example.invalid"],
            );
            fs::write(repository.join("base.txt"), "base\n").unwrap();
            run_git(&repository, &["add", "base.txt"]);
            run_git(&repository, &["commit", "-m", "base"]);
            run_git(&repository, &["branch", "primary"]);
            run_git(&repository, &["branch", "reviewer"]);
            run_git(
                &repository,
                &["worktree", "add", primary.to_str().unwrap(), "primary"],
            );
            run_git(
                &repository,
                &["worktree", "add", reviewer.to_str().unwrap(), "reviewer"],
            );
            Self {
                _root: root,
                repository,
            }
        }
    }

    fn thread(id: &str, cwd: &Path) -> ThreadSummary {
        ThreadSummary {
            id: id.into(),
            cwd: cwd.to_owned(),
            name: Some(id.into()),
            preview: String::new(),
            cli_version: "0.144.5".into(),
            created_at: 0,
            updated_at: 0,
            status: json!({"type": "idle"}),
            source: json!({}),
        }
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
