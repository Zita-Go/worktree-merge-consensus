use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use consensus_core::{
    git::{
        GitInspector, inspect_worktree, normalize_branch_name, verify_frozen_sources,
        verify_integration_result, verify_reported_changed_files, verify_same_repository,
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
        .inspect_integration(fixture.primary(), &facts)
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
    assert_eq!(
        integration.changed_files,
        vec![PathBuf::from("reviewer.txt")]
    );
}

#[test]
fn verification_clone_is_detached_remote_free_and_git_isolated() {
    let (fixture, facts) = integrated_fixture();
    let inspector = GitInspector::default();
    let integration = inspector
        .inspect_integration(fixture.primary(), &facts)
        .unwrap();
    let state = tempfile::tempdir().unwrap();
    let destination = state.path().join("verification");

    let clone = inspector
        .materialize_verification_clone(
            fixture.primary(),
            &destination,
            &integration.worktree.head_sha,
            &facts.git_common_dir,
        )
        .unwrap();

    let snapshot = inspector.inspect_worktree(&clone).unwrap();
    assert_eq!(snapshot.head_sha, integration.worktree.head_sha);
    assert!(snapshot.source_ref.is_none());
    assert_ne!(snapshot.common_dir, facts.git_common_dir);
    let remotes = Command::new("git")
        .args(["-C", clone.to_str().unwrap(), "remote"])
        .output()
        .unwrap();
    assert!(remotes.status.success());
    assert!(remotes.stdout.is_empty());
    inspector
        .verify_source_refs_unchanged(fixture.primary(), &facts)
        .unwrap();
}

#[test]
fn pending_verification_can_recover_a_dirty_clone_without_losing_git_isolation() {
    let (fixture, facts) = integrated_fixture();
    let inspector = GitInspector::default();
    let integration = inspector
        .inspect_integration(fixture.primary(), &facts)
        .unwrap();
    let state = tempfile::tempdir().unwrap();
    let destination = state.path().join("verification");
    let clone = inspector
        .materialize_verification_clone(
            fixture.primary(),
            &destination,
            &integration.worktree.head_sha,
            &facts.git_common_dir,
        )
        .unwrap();
    fs::write(
        clone.join("test-artifact.txt"),
        "created by a frozen test\n",
    )
    .unwrap();

    let normal_error = inspector
        .materialize_verification_clone(
            fixture.primary(),
            &destination,
            &integration.worktree.head_sha,
            &facts.git_common_dir,
        )
        .unwrap_err();
    assert_eq!(normal_error.code(), "UNSAFE_VERIFICATION_WORKSPACE");

    let recovered = inspector
        .recover_verification_clone(
            &destination,
            &integration.worktree.head_sha,
            &facts.git_common_dir,
        )
        .unwrap();
    let snapshot = inspector.inspect_worktree(&recovered).unwrap();
    assert_eq!(snapshot.head_sha, integration.worktree.head_sha);
    assert!(snapshot.source_ref.is_none());
    assert_ne!(snapshot.common_dir, facts.git_common_dir);
    assert!(recovered.join("test-artifact.txt").exists());
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
        .inspect_integration(fixture.primary(), &facts)
        .unwrap();

    let error = verify_integration_result(
        &facts,
        &integration,
        "consensus/test-run",
        &integration.worktree.head_sha,
    )
    .unwrap_err();

    assert_eq!(error.code(), "CONFLICT_MARKERS");
    assert!(
        integration
            .conflict_marker_files
            .iter()
            .any(|path| path.ends_with("conflicted.txt"))
    );
}

#[test]
fn conflict_markers_in_large_files_are_scanned_fail_closed() {
    let (fixture, facts) = integrated_fixture();
    let path = fixture.primary().join("large-conflict.txt");
    let mut file = fs::File::create(&path).unwrap();
    writeln!(file, "<<<<<<<<<< ours").unwrap();
    file.write_all(&vec![b'a'; 8 * 1024 * 1024]).unwrap();
    writeln!(file, "\n==========\nright\n>>>>>>>>>> theirs").unwrap();
    drop(file);
    fixture.git(fixture.primary(), &["add", "large-conflict.txt"]);
    fixture.git(
        fixture.primary(),
        &["commit", "-m", "large unresolved conflict"],
    );
    let integration = GitInspector::default()
        .inspect_integration(fixture.primary(), &facts)
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

#[test]
fn reported_changed_files_must_match_git_objects_exactly() {
    let (fixture, facts) = integrated_fixture();
    let integration = GitInspector::default()
        .inspect_integration(fixture.primary(), &facts)
        .unwrap();

    let error = verify_reported_changed_files(&integration, &[]).unwrap_err();

    assert_eq!(error.code(), "CHANGED_FILES_MISMATCH");
    verify_reported_changed_files(&integration, &[PathBuf::from("reviewer.txt")]).unwrap();
}

#[test]
fn branch_components_follow_git_ref_format_rules() {
    for branch in [
        "consensus/.hidden/result",
        "consensus/result.lock/final",
        "consensus/trailing./final",
        "@",
        "HEAD",
    ] {
        let error = normalize_branch_name(branch).unwrap_err();
        assert_eq!(error.code(), "INVALID_BRANCH_NAME", "{branch}");
    }
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
