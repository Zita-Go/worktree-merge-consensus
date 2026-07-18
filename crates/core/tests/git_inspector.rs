use std::{fs, path::Path, process::Command};

use consensus_core::{
    git::{
        GitInspector, inspect_worktree, verify_frozen_sources, verify_integration_result,
        verify_same_repository,
    },
    state::RunFacts,
};
use tempfile::TempDir;
use uuid::Uuid;

#[test]
fn two_worktrees_share_objects_without_sharing_paths() {
    let fixture = GitFixture::two_worktrees();
    let primary = inspect_worktree(fixture.primary()).unwrap();
    let reviewer = inspect_worktree(fixture.reviewer()).unwrap();

    verify_same_repository(&primary, &reviewer).unwrap();
    assert_ne!(primary.worktree, reviewer.worktree);
    assert_eq!(primary.common_dir, reviewer.common_dir);
}

#[test]
fn dirty_source_worktree_is_rejected() {
    let fixture = GitFixture::two_worktrees();
    let clean_primary = inspect_worktree(fixture.primary()).unwrap();
    let reviewer = inspect_worktree(fixture.reviewer()).unwrap();
    let facts = facts_from(&clean_primary, &reviewer);
    fs::write(fixture.primary().join("untracked.txt"), "dirty\n").unwrap();
    let dirty_primary = inspect_worktree(fixture.primary()).unwrap();

    let error = verify_frozen_sources(&facts, &dirty_primary, &reviewer).unwrap_err();

    assert_eq!(error.code(), "DIRTY_WORKTREE");
}

#[test]
fn moving_a_frozen_source_ref_is_rejected() {
    let fixture = GitFixture::two_worktrees();
    let frozen_primary = inspect_worktree(fixture.primary()).unwrap();
    let reviewer = inspect_worktree(fixture.reviewer()).unwrap();
    let facts = facts_from(&frozen_primary, &reviewer);

    fs::write(fixture.primary().join("feature.txt"), "new commit\n").unwrap();
    fixture.git(fixture.primary(), &["add", "feature.txt"]);
    fixture.git(fixture.primary(), &["commit", "-m", "move primary"]);
    let moved_primary = inspect_worktree(fixture.primary()).unwrap();

    let error = verify_frozen_sources(&facts, &moved_primary, &reviewer).unwrap_err();
    assert_eq!(error.code(), "SOURCE_DRIFT");
}

#[test]
fn separate_repositories_are_rejected() {
    let first = GitFixture::two_worktrees();
    let second = GitFixture::two_worktrees();
    let primary = inspect_worktree(first.primary()).unwrap();
    let unrelated = inspect_worktree(second.reviewer()).unwrap();

    let error = verify_same_repository(&primary, &unrelated).unwrap_err();

    assert_eq!(error.code(), "DIFFERENT_REPOSITORY");
}

#[test]
fn detached_source_is_frozen_by_sha() {
    let fixture = GitFixture::two_worktrees();
    fixture.git(fixture.primary(), &["switch", "--detach"]);
    let primary = inspect_worktree(fixture.primary()).unwrap();
    let reviewer = inspect_worktree(fixture.reviewer()).unwrap();
    let facts = facts_from(&primary, &reviewer);

    assert!(primary.source_ref.is_none());
    verify_frozen_sources(&facts, &primary, &reviewer).unwrap();
}

#[test]
fn existing_integration_branch_is_rejected() {
    let fixture = GitFixture::two_worktrees();
    let inspector = GitInspector::default();

    let error = inspector
        .verify_integration_branch_absent(fixture.primary(), "reviewer")
        .unwrap_err();

    assert_eq!(error.code(), "INTEGRATION_BRANCH_EXISTS");
}

#[test]
fn integration_result_contains_both_frozen_commits_and_preserves_source_refs() {
    let (fixture, facts) = integrated_fixture();
    let inspector = GitInspector::default();
    let integration = inspector
        .inspect_integration(
            fixture.primary(),
            &facts,
            &["primary.txt".into(), "reviewer.txt".into()],
        )
        .unwrap();

    verify_integration_result(
        &facts,
        &integration,
        "consensus/test-run",
        &integration.worktree.head_sha,
    )
    .unwrap();
    assert!(integration.primary_is_ancestor);
    assert!(integration.reviewer_is_ancestor);
}

#[test]
fn committed_conflict_markers_are_rejected() {
    let (fixture, facts) = integrated_fixture();
    fs::write(
        fixture.primary().join("conflicted.txt"),
        "<<<<<<< HEAD\nleft\n=======\nright\n>>>>>>> reviewer\n",
    )
    .unwrap();
    fixture.git(fixture.primary(), &["add", "conflicted.txt"]);
    fixture.git(
        fixture.primary(),
        &["commit", "-m", "accidental conflict markers"],
    );
    let inspector = GitInspector::default();
    let integration = inspector
        .inspect_integration(fixture.primary(), &facts, &["conflicted.txt".into()])
        .unwrap();

    let error = verify_integration_result(
        &facts,
        &integration,
        "consensus/test-run",
        &integration.worktree.head_sha,
    )
    .unwrap_err();

    assert_eq!(error.code(), "CONFLICT_MARKERS");
}

fn integrated_fixture() -> (GitFixture, RunFacts) {
    let fixture = GitFixture::two_worktrees();
    fs::write(fixture.primary().join("primary.txt"), "primary\n").unwrap();
    fixture.git(fixture.primary(), &["add", "primary.txt"]);
    fixture.git(fixture.primary(), &["commit", "-m", "primary change"]);
    fs::write(fixture.reviewer().join("reviewer.txt"), "reviewer\n").unwrap();
    fixture.git(fixture.reviewer(), &["add", "reviewer.txt"]);
    fixture.git(fixture.reviewer(), &["commit", "-m", "reviewer change"]);
    let primary = inspect_worktree(fixture.primary()).unwrap();
    let reviewer = inspect_worktree(fixture.reviewer()).unwrap();
    let facts = facts_from(&primary, &reviewer);
    fixture.git(fixture.primary(), &["switch", "-c", "consensus/test-run"]);
    fixture.git(
        fixture.primary(),
        &["merge", "--no-ff", "reviewer", "-m", "integrate reviewer"],
    );
    (fixture, facts)
}

fn facts_from(
    primary: &consensus_core::git::WorktreeSnapshot,
    reviewer: &consensus_core::git::WorktreeSnapshot,
) -> RunFacts {
    RunFacts {
        run_id: Uuid::new_v4(),
        primary_thread_id: "primary-thread".into(),
        reviewer_thread_id: "reviewer-thread".into(),
        primary_worktree: primary.worktree.clone(),
        reviewer_worktree: reviewer.worktree.clone(),
        git_common_dir: primary.common_dir.clone(),
        primary_sha: primary.head_sha.clone(),
        reviewer_sha: reviewer.head_sha.clone(),
        primary_ref: primary
            .source_ref
            .as_ref()
            .map(|source| source.name.clone()),
        reviewer_ref: reviewer
            .source_ref
            .as_ref()
            .map(|source| source.name.clone()),
    }
}

struct GitFixture {
    _root: TempDir,
    primary: std::path::PathBuf,
    reviewer: std::path::PathBuf,
}

impl GitFixture {
    fn two_worktrees() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let primary = root.path().join("primary");
        let reviewer = root.path().join("reviewer");
        fs::create_dir(&repository).unwrap();

        run_git(&repository, &["init", "--initial-branch=base"]);
        run_git(&repository, &["config", "user.name", "Consensus Test"]);
        run_git(
            &repository,
            &["config", "user.email", "consensus@example.invalid"],
        );
        fs::write(repository.join("README.md"), "base\n").unwrap();
        run_git(&repository, &["add", "README.md"]);
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
            primary,
            reviewer,
        }
    }

    fn primary(&self) -> &Path {
        &self.primary
    }

    fn reviewer(&self) -> &Path {
        &self.reviewer
    }

    fn git(&self, cwd: &Path, args: &[&str]) {
        run_git(cwd, args);
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
