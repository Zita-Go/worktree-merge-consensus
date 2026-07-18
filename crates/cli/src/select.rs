use app_server_client::ThreadSummary;
use consensus_core::git::{GitInspector, WorktreeSnapshot};
use dialoguer::{Confirm, FuzzySelect, theme::ColorfulTheme};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct SelectedTasks {
    pub primary: ThreadSummary,
    pub reviewer: ThreadSummary,
    pub primary_snapshot: WorktreeSnapshot,
    pub reviewer_snapshot: WorktreeSnapshot,
}

#[derive(Debug, Error)]
pub enum SelectionError {
    #[error("no Codex tasks are available")]
    NoTasks,
    #[error("no different worktree in the same repository is available for review")]
    NoReviewer,
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
    inspector: &GitInspector,
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
    let primary_snapshot = inspector.inspect_worktree(&primary.cwd)?;

    let reviewer_candidates = threads
        .iter()
        .filter(|thread| thread.id != primary.id)
        .filter_map(|thread| {
            inspector
                .inspect_worktree(&thread.cwd)
                .ok()
                .filter(|snapshot| {
                    snapshot.common_dir == primary_snapshot.common_dir
                        && snapshot.worktree != primary_snapshot.worktree
                })
                .map(|snapshot| (thread.clone(), snapshot))
        })
        .collect::<Vec<_>>();
    if reviewer_candidates.is_empty() {
        return Err(SelectionError::NoReviewer);
    }
    let reviewer_labels = reviewer_candidates
        .iter()
        .map(|(thread, _)| task_label(thread))
        .collect::<Vec<_>>();
    let reviewer_index = selector.select_reviewer(&reviewer_labels)?;
    let (reviewer, reviewer_snapshot) = reviewer_candidates
        .get(reviewer_index)
        .cloned()
        .ok_or_else(|| SelectionError::Terminal("invalid reviewer selection index".into()))?;

    let summary = format!(
        "Use primary {} at {} ({}) and reviewer {} at {} ({}) in repository {}?",
        short_id(&primary.id),
        primary_snapshot.worktree.display(),
        short_sha(&primary_snapshot.head_sha),
        short_id(&reviewer.id),
        reviewer_snapshot.worktree.display(),
        short_sha(&reviewer_snapshot.head_sha),
        primary_snapshot.common_dir.display(),
    );
    if !selector.confirm(&summary)? {
        return Err(SelectionError::Cancelled);
    }

    Ok(SelectedTasks {
        primary,
        reviewer,
        primary_snapshot,
        reviewer_snapshot,
    })
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

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, process::Command};

    use serde_json::json;

    use super::*;

    #[test]
    fn reviewer_candidates_are_same_repository_but_different_worktrees() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repository");
        let primary = temp.path().join("primary");
        let reviewer = temp.path().join("reviewer");
        fs::create_dir_all(&repository).unwrap();
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

        let separate = temp.path().join("separate");
        fs::create_dir_all(&separate).unwrap();
        run_git(&separate, &["init", "--initial-branch=base"]);
        run_git(&separate, &["config", "user.name", "Test User"]);
        run_git(&separate, &["config", "user.email", "test@example.invalid"]);
        fs::write(separate.join("other.txt"), "other\n").unwrap();
        run_git(&separate, &["add", "other.txt"]);
        run_git(&separate, &["commit", "-m", "other"]);
        let threads = vec![
            thread("primary-thread", &primary),
            thread("other-repository", &separate),
            thread("reviewer-thread", &reviewer),
        ];
        let mut selector = FixedSelector {
            primary: 0,
            reviewer: 0,
            confirmed: true,
        };

        let selected = select_tasks(&threads, &GitInspector::default(), &mut selector).unwrap();

        assert_eq!(selected.primary.id, "primary-thread");
        assert_eq!(selected.reviewer.id, "reviewer-thread");
        assert_eq!(
            selected.primary_snapshot.common_dir,
            selected.reviewer_snapshot.common_dir
        );
        assert_ne!(
            selected.primary_snapshot.worktree,
            selected.reviewer_snapshot.worktree
        );
    }

    struct FixedSelector {
        primary: usize,
        reviewer: usize,
        confirmed: bool,
    }

    impl TaskSelector for FixedSelector {
        fn select_primary(&mut self, _labels: &[String]) -> Result<usize, SelectionError> {
            Ok(self.primary)
        }

        fn select_reviewer(&mut self, labels: &[String]) -> Result<usize, SelectionError> {
            assert_eq!(labels.len(), 1);
            Ok(self.reviewer)
        }

        fn confirm(&mut self, _summary: &str) -> Result<bool, SelectionError> {
            Ok(self.confirmed)
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
