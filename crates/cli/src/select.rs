use std::path::PathBuf;

use app_server_client::ThreadSummary;
use consensus_core::git::{GitInspector, RegisteredWorktree, WorktreeSnapshot};
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
    #[error("fewer than two registered worktree entries can be selected")]
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
    fn report_worktree_validation_error(&mut self, _error: &consensus_core::git::GitSafetyError) {}
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

    fn report_worktree_validation_error(&mut self, error: &consensus_core::git::GitSafetyError) {
        eprintln!(
            "Cannot use that worktree pair ({}): {}. Choose again.",
            error.code(),
            error.detail()
        );
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
    let candidates = entries.iter().collect::<Vec<_>>();
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

pub fn select_valid_worktrees(
    entries: &[RegisteredWorktree],
    inspector: &GitInspector,
    selector: &mut impl TaskSelector,
) -> Result<(WorktreeSnapshot, WorktreeSnapshot), SelectionError> {
    loop {
        let selected = select_worktrees(entries, selector)?;
        match inspector.inspect_registered_pair(&selected.primary, &selected.reviewer) {
            Ok(pair) => return Ok(pair),
            Err(error)
                if matches!(
                    error.code(),
                    "UNREGISTERED_WORKTREE"
                        | "DUPLICATE_WORKTREE"
                        | "REPOSITORY_MISMATCH"
                        | "DIRTY_WORKTREE"
                        | "WORKTREE_UNAVAILABLE"
                ) =>
            {
                selector.report_worktree_validation_error(&error);
            }
            Err(error) => return Err(SelectionError::Git(error)),
        }
    }
}

pub fn confirm_binding(
    binding: &SelectedBinding,
    selector: &mut impl TaskSelector,
) -> Result<(), SelectionError> {
    let summary = format!(
        "Confirm exact binding?\nPRIMARY task: {}\n  worktree: {}\n  source ref: {}\n  HEAD SHA: {}\nREVIEWER task: {}\n  worktree: {}\n  source ref: {}\n  HEAD SHA: {}\nGit common directory: {}",
        binding.tasks.primary.id,
        binding.primary_snapshot.worktree.display(),
        snapshot_source_label(&binding.primary_snapshot),
        binding.primary_snapshot.head_sha,
        binding.tasks.reviewer.id,
        binding.reviewer_snapshot.worktree.display(),
        snapshot_source_label(&binding.reviewer_snapshot),
        binding.reviewer_snapshot.head_sha,
        binding.primary_snapshot.common_dir.display(),
    );
    if selector.confirm(&summary)? {
        Ok(())
    } else {
        Err(SelectionError::Cancelled)
    }
}

fn snapshot_source_label(snapshot: &WorktreeSnapshot) -> &str {
    snapshot
        .source_ref
        .as_ref()
        .map(|source| source.name.as_str())
        .unwrap_or("detached")
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
        collections::VecDeque,
        fs,
        path::{Path, PathBuf},
        process::Command,
    };

    use consensus_core::git::{GitInspector, RegisteredWorktree, WorktreeIssue};
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

    #[test]
    fn worktree_selection_displays_unavailable_registered_entries() {
        let fixture = WorktreeFixture::new();
        let mut entries = GitInspector::default()
            .list_registered_worktrees(&fixture.repository)
            .unwrap();
        let missing = fixture._root.path().join("missing-worktree");
        entries.push(RegisteredWorktree {
            worktree: missing.clone(),
            common_dir: entries[0].common_dir.clone(),
            head_sha: None,
            source_ref: None,
            clean: None,
            bare: false,
            issue: Some(WorktreeIssue {
                code: "WORKTREE_UNAVAILABLE".into(),
                detail: "fixture is unavailable".into(),
            }),
        });
        let mut selector =
            PathSequenceSelector::new([fixture.repository.clone()], [fixture.reviewer.clone()]);

        select_worktrees(&entries, &mut selector).unwrap();

        assert!(selector.primary_labels[0].iter().any(|label| {
            label.contains(&missing.display().to_string()) && label.contains("WORKTREE_UNAVAILABLE")
        }));
    }

    #[test]
    fn invalid_interactive_pair_returns_to_worktree_selection() {
        let fixture = WorktreeFixture::new();
        fs::write(fixture.primary.join("uncommitted.txt"), "dirty\n").unwrap();
        let entries = GitInspector::default()
            .list_registered_worktrees(&fixture.repository)
            .unwrap();
        let mut selector = PathSequenceSelector::new(
            [fixture.primary.clone(), fixture.repository.clone()],
            [fixture.reviewer.clone(), fixture.reviewer.clone()],
        );

        let (primary, reviewer) =
            select_valid_worktrees(&entries, &GitInspector::default(), &mut selector).unwrap();

        assert_eq!(primary.worktree, fixture.repository.canonicalize().unwrap());
        assert_eq!(reviewer.worktree, fixture.reviewer.canonicalize().unwrap());
        assert_eq!(selector.primary_labels.len(), 2);
    }

    #[test]
    fn binding_confirmation_shows_the_complete_exact_mapping() {
        let fixture = WorktreeFixture::new();
        let (primary_snapshot, reviewer_snapshot) = GitInspector::default()
            .inspect_registered_pair(&fixture.primary, &fixture.reviewer)
            .unwrap();
        let primary_id = "primary-thread-111111111111111111111111";
        let reviewer_id = "reviewer-thread-2222222222222222222222";
        let binding = SelectedBinding {
            tasks: SelectedTasks {
                primary: thread(primary_id, &fixture.repository),
                reviewer: thread(reviewer_id, &fixture.repository),
            },
            primary_snapshot,
            reviewer_snapshot,
        };
        let mut selector = ConfirmationSelector::default();

        confirm_binding(&binding, &mut selector).unwrap();

        let summary = selector.summary.unwrap();
        for required in [
            primary_id,
            reviewer_id,
            binding.primary_snapshot.worktree.to_str().unwrap(),
            binding.reviewer_snapshot.worktree.to_str().unwrap(),
            binding
                .primary_snapshot
                .source_ref
                .as_ref()
                .unwrap()
                .name
                .as_str(),
            binding
                .reviewer_snapshot
                .source_ref
                .as_ref()
                .unwrap()
                .name
                .as_str(),
            binding.primary_snapshot.head_sha.as_str(),
            binding.reviewer_snapshot.head_sha.as_str(),
        ] {
            assert!(
                summary.contains(required),
                "missing {required:?} in {summary}"
            );
        }
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

    struct PathSequenceSelector {
        primary_choices: VecDeque<PathBuf>,
        reviewer_choices: VecDeque<PathBuf>,
        primary_labels: Vec<Vec<String>>,
    }

    impl PathSequenceSelector {
        fn new(
            primary_choices: impl IntoIterator<Item = PathBuf>,
            reviewer_choices: impl IntoIterator<Item = PathBuf>,
        ) -> Self {
            Self {
                primary_choices: primary_choices.into_iter().collect(),
                reviewer_choices: reviewer_choices.into_iter().collect(),
                primary_labels: Vec::new(),
            }
        }

        fn choose(labels: &[String], path: &Path) -> Result<usize, SelectionError> {
            let path = path.canonicalize().unwrap_or_else(|_| path.to_owned());
            labels
                .iter()
                .position(|label| label.starts_with(&path.display().to_string()))
                .ok_or_else(|| {
                    SelectionError::Terminal(format!(
                        "expected {} in labels {labels:?}",
                        path.display()
                    ))
                })
        }
    }

    impl TaskSelector for PathSequenceSelector {
        fn select_primary(&mut self, _labels: &[String]) -> Result<usize, SelectionError> {
            unreachable!("task selection is not used")
        }

        fn select_reviewer(&mut self, _labels: &[String]) -> Result<usize, SelectionError> {
            unreachable!("task selection is not used")
        }

        fn select_primary_worktree(&mut self, labels: &[String]) -> Result<usize, SelectionError> {
            self.primary_labels.push(labels.to_vec());
            let path = self
                .primary_choices
                .pop_front()
                .expect("missing scripted primary choice");
            Self::choose(labels, &path)
        }

        fn select_reviewer_worktree(&mut self, labels: &[String]) -> Result<usize, SelectionError> {
            let path = self
                .reviewer_choices
                .pop_front()
                .expect("missing scripted reviewer choice");
            Self::choose(labels, &path)
        }

        fn input_repository(&mut self) -> Result<PathBuf, SelectionError> {
            unreachable!("repository prompting is not used")
        }

        fn confirm(&mut self, _summary: &str) -> Result<bool, SelectionError> {
            unreachable!("binding confirmation is not used")
        }
    }

    #[derive(Default)]
    struct ConfirmationSelector {
        summary: Option<String>,
    }

    impl TaskSelector for ConfirmationSelector {
        fn select_primary(&mut self, _labels: &[String]) -> Result<usize, SelectionError> {
            unreachable!("selection is not used")
        }

        fn select_reviewer(&mut self, _labels: &[String]) -> Result<usize, SelectionError> {
            unreachable!("selection is not used")
        }

        fn select_primary_worktree(&mut self, _labels: &[String]) -> Result<usize, SelectionError> {
            unreachable!("selection is not used")
        }

        fn select_reviewer_worktree(
            &mut self,
            _labels: &[String],
        ) -> Result<usize, SelectionError> {
            unreachable!("selection is not used")
        }

        fn input_repository(&mut self) -> Result<PathBuf, SelectionError> {
            unreachable!("selection is not used")
        }

        fn confirm(&mut self, summary: &str) -> Result<bool, SelectionError> {
            self.summary = Some(summary.to_owned());
            Ok(true)
        }
    }

    struct WorktreeFixture {
        _root: tempfile::TempDir,
        repository: PathBuf,
        primary: PathBuf,
        reviewer: PathBuf,
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
                primary,
                reviewer,
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
