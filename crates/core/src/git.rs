use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::state::RunFacts;

#[derive(Debug, Clone)]
pub struct GitInspector {
    git_binary: OsString,
}

impl Default for GitInspector {
    fn default() -> Self {
        Self {
            git_binary: OsString::from("git"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    pub name: String,
    pub target_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSnapshot {
    pub worktree: PathBuf,
    pub common_dir: PathBuf,
    pub head_sha: String,
    pub source_ref: Option<SourceRef>,
    pub clean: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationSnapshot {
    pub worktree: WorktreeSnapshot,
    pub branch: String,
    pub changed_files: Vec<PathBuf>,
    pub unmerged_entries: Vec<String>,
    pub conflict_marker_files: Vec<PathBuf>,
    pub primary_is_ancestor: bool,
    pub reviewer_is_ancestor: bool,
    pub primary_source_ref_target: Option<String>,
    pub reviewer_source_ref_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{code}: {detail}")]
pub struct GitSafetyError {
    code: &'static str,
    detail: String,
}

impl GitSafetyError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

pub fn inspect_worktree(path: impl AsRef<Path>) -> Result<WorktreeSnapshot, GitSafetyError> {
    GitInspector::default().inspect_worktree(path)
}

pub fn verify_same_repository(
    primary: &WorktreeSnapshot,
    reviewer: &WorktreeSnapshot,
) -> Result<(), GitSafetyError> {
    if primary.common_dir != reviewer.common_dir {
        return Err(git_error(
            "DIFFERENT_REPOSITORY",
            "selected worktrees do not share one canonical Git common directory",
        ));
    }
    if primary.worktree == reviewer.worktree {
        return Err(git_error(
            "SAME_WORKTREE",
            "primary and reviewer must use different canonical worktree paths",
        ));
    }
    Ok(())
}

pub fn verify_frozen_sources(
    facts: &RunFacts,
    primary: &WorktreeSnapshot,
    reviewer: &WorktreeSnapshot,
) -> Result<(), GitSafetyError> {
    verify_same_repository(primary, reviewer)?;
    if primary.common_dir != facts.git_common_dir
        || reviewer.common_dir != facts.git_common_dir
        || primary.worktree != facts.primary_worktree
        || reviewer.worktree != facts.reviewer_worktree
    {
        return Err(git_error(
            "SOURCE_DRIFT",
            "canonical repository or worktree identity changed after freeze",
        ));
    }
    if !primary.clean || !reviewer.clean {
        return Err(git_error(
            "DIRTY_WORKTREE",
            "both frozen source worktrees must be clean",
        ));
    }

    verify_frozen_source(
        "primary",
        &facts.primary_sha,
        facts.primary_ref.as_deref(),
        primary,
    )?;
    verify_frozen_source(
        "reviewer",
        &facts.reviewer_sha,
        facts.reviewer_ref.as_deref(),
        reviewer,
    )?;
    Ok(())
}

pub fn verify_integration_result(
    facts: &RunFacts,
    integration: &IntegrationSnapshot,
    expected_branch: &str,
    expected_sha: &str,
) -> Result<(), GitSafetyError> {
    if integration.worktree.common_dir != facts.git_common_dir
        || integration.worktree.worktree != facts.primary_worktree
    {
        return Err(git_error(
            "DIFFERENT_REPOSITORY",
            "integration result is not in the frozen primary repository and worktree",
        ));
    }
    if !integration.worktree.clean {
        return Err(git_error(
            "DIRTY_WORKTREE",
            "integration worktree must be clean before result review",
        ));
    }
    if integration.branch != normalize_branch_name(expected_branch)? {
        return Err(git_error(
            "UNEXPECTED_INTEGRATION_BRANCH",
            "integration result is on a different branch",
        ));
    }
    if integration.worktree.head_sha != expected_sha {
        return Err(git_error(
            "STALE_INTEGRATION_SHA",
            "integration HEAD does not match the reported SHA",
        ));
    }
    if !integration.unmerged_entries.is_empty() {
        return Err(git_error(
            "UNRESOLVED_CONFLICTS",
            "integration index contains unresolved entries",
        ));
    }
    if !integration.conflict_marker_files.is_empty() {
        return Err(git_error(
            "CONFLICT_MARKERS",
            "changed text files contain unresolved conflict markers",
        ));
    }
    if !integration.primary_is_ancestor || !integration.reviewer_is_ancestor {
        return Err(git_error(
            "MISSING_SOURCE_ANCESTRY",
            "integration HEAD does not contain both frozen source commits",
        ));
    }
    verify_source_ref_target(
        "primary",
        facts.primary_ref.as_deref(),
        &facts.primary_sha,
        integration.primary_source_ref_target.as_deref(),
    )?;
    verify_source_ref_target(
        "reviewer",
        facts.reviewer_ref.as_deref(),
        &facts.reviewer_sha,
        integration.reviewer_source_ref_target.as_deref(),
    )?;
    Ok(())
}

pub fn verify_reported_changed_files(
    integration: &IntegrationSnapshot,
    reported: &[PathBuf],
) -> Result<(), GitSafetyError> {
    let reported = reported
        .iter()
        .map(|path| validate_relative_changed_path(path).map(Path::to_path_buf))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let authoritative = integration
        .changed_files
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if reported != authoritative {
        return Err(git_error(
            "CHANGED_FILES_MISMATCH",
            "reported changed_files does not match the authoritative Git object diff",
        ));
    }
    Ok(())
}

impl GitInspector {
    pub fn materialize_verification_clone(
        &self,
        source_worktree: impl AsRef<Path>,
        destination: impl AsRef<Path>,
        integration_sha: &str,
        source_common_dir: impl AsRef<Path>,
    ) -> Result<PathBuf, GitSafetyError> {
        validate_sha(integration_sha, "verification integration SHA")?;
        let source = fs::canonicalize(source_worktree.as_ref()).map_err(|error| {
            git_error(
                "NOT_A_WORKTREE",
                format!("cannot canonicalize verification source: {error}"),
            )
        })?;
        let source_common = fs::canonicalize(source_common_dir.as_ref()).map_err(|error| {
            git_error(
                "NOT_A_WORKTREE",
                format!("cannot canonicalize source Git common directory: {error}"),
            )
        })?;
        let destination = destination.as_ref();
        if destination.starts_with(&source) || destination.starts_with(&source_common) {
            return Err(git_error(
                "UNSAFE_VERIFICATION_WORKSPACE",
                "verification clone must be outside the source worktree and Git common directory",
            ));
        }
        if destination.exists() {
            return self.verify_verification_clone(
                destination,
                integration_sha,
                &source_common,
                true,
            );
        }
        let parent = destination.parent().ok_or_else(|| {
            git_error(
                "UNSAFE_VERIFICATION_WORKSPACE",
                "verification clone has no parent directory",
            )
        })?;
        fs::create_dir_all(parent).map_err(|error| {
            git_error(
                "VERIFICATION_WORKSPACE_FAILURE",
                format!("cannot create verification parent: {error}"),
            )
        })?;
        let name = destination
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| {
                git_error(
                    "UNSAFE_VERIFICATION_WORKSPACE",
                    "verification clone name is not UTF-8",
                )
            })?;
        let preparing = parent.join(format!(".{name}.preparing"));
        if preparing.exists() {
            fs::remove_dir_all(&preparing).map_err(|error| {
                git_error(
                    "VERIFICATION_WORKSPACE_FAILURE",
                    format!("cannot remove an interrupted verification clone: {error}"),
                )
            })?;
        }

        let clone = Command::new(&self.git_binary)
            .args([
                OsStr::new("-c"),
                OsStr::new("protocol.file.allow=always"),
                OsStr::new("clone"),
                OsStr::new("--no-local"),
                OsStr::new("--no-hardlinks"),
                OsStr::new("--no-checkout"),
                OsStr::new("--quiet"),
                OsStr::new("--config"),
                OsStr::new("core.hooksPath=/dev/null"),
                OsStr::new("--"),
                source.as_os_str(),
                preparing.as_os_str(),
            ])
            .output()
            .map_err(|error| {
                git_error(
                    "VERIFICATION_WORKSPACE_FAILURE",
                    format!("cannot start isolated git clone: {error}"),
                )
            })?;
        ensure_success("clone verification workspace", &clone)?;

        for (label, args) in [
            (
                "remove verification origin",
                vec![
                    OsString::from("-C"),
                    preparing.as_os_str().to_owned(),
                    OsString::from("remote"),
                    OsString::from("remove"),
                    OsString::from("origin"),
                ],
            ),
            (
                "checkout verification SHA",
                vec![
                    OsString::from("-C"),
                    preparing.as_os_str().to_owned(),
                    OsString::from("-c"),
                    OsString::from("core.hooksPath=/dev/null"),
                    OsString::from("checkout"),
                    OsString::from("--detach"),
                    OsString::from("--quiet"),
                    OsString::from(integration_sha),
                ],
            ),
        ] {
            let output = Command::new(&self.git_binary)
                .args(args)
                .output()
                .map_err(|error| {
                    git_error(
                        "VERIFICATION_WORKSPACE_FAILURE",
                        format!("cannot {label}: {error}"),
                    )
                })?;
            ensure_success(label, &output)?;
        }
        fs::rename(&preparing, destination).map_err(|error| {
            git_error(
                "VERIFICATION_WORKSPACE_FAILURE",
                format!("cannot publish verification clone: {error}"),
            )
        })?;
        self.verify_verification_clone(destination, integration_sha, &source_common, true)
    }

    pub fn recover_verification_clone(
        &self,
        destination: impl AsRef<Path>,
        integration_sha: &str,
        source_common_dir: impl AsRef<Path>,
    ) -> Result<PathBuf, GitSafetyError> {
        validate_sha(integration_sha, "verification integration SHA")?;
        let source_common = fs::canonicalize(source_common_dir.as_ref()).map_err(|error| {
            git_error(
                "NOT_A_WORKTREE",
                format!("cannot canonicalize source Git common directory: {error}"),
            )
        })?;
        self.verify_verification_clone(destination.as_ref(), integration_sha, &source_common, false)
    }

    fn verify_verification_clone(
        &self,
        destination: &Path,
        integration_sha: &str,
        source_common: &Path,
        require_clean: bool,
    ) -> Result<PathBuf, GitSafetyError> {
        let snapshot = self.inspect_worktree(destination)?;
        if snapshot.common_dir == source_common
            || snapshot.head_sha != integration_sha
            || snapshot.source_ref.is_some()
            || (require_clean && !snapshot.clean)
        {
            return Err(git_error(
                "UNSAFE_VERIFICATION_WORKSPACE",
                "verification clone is not an isolated detached copy of the exact integration SHA in the required cleanliness state",
            ));
        }
        let remotes = Command::new(&self.git_binary)
            .current_dir(&snapshot.worktree)
            .arg("remote")
            .output()
            .map_err(|error| {
                git_error(
                    "VERIFICATION_WORKSPACE_FAILURE",
                    format!("cannot inspect verification remotes: {error}"),
                )
            })?;
        ensure_success("inspect verification remotes", &remotes)?;
        if !remotes.stdout.iter().all(u8::is_ascii_whitespace) {
            return Err(git_error(
                "UNSAFE_VERIFICATION_WORKSPACE",
                "verification clone must not retain a Git remote",
            ));
        }
        Ok(snapshot.worktree)
    }

    pub fn inspect_worktree(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<WorktreeSnapshot, GitSafetyError> {
        let requested = fs::canonicalize(path.as_ref()).map_err(|error| {
            git_error(
                "NOT_A_WORKTREE",
                format!("cannot canonicalize worktree path: {error}"),
            )
        })?;
        let top_level = self.git_text(&requested, ["rev-parse", "--show-toplevel"])?;
        let worktree = canonical_git_path(&requested, &top_level)?;
        let common_dir_raw = self.git_text(&worktree, ["rev-parse", "--git-common-dir"])?;
        let common_dir = canonical_git_path(&worktree, &common_dir_raw)?;
        let head_sha = self.git_text(&worktree, ["rev-parse", "HEAD"])?;
        validate_sha(&head_sha, "HEAD")?;

        let status = self.git_output(
            &worktree,
            ["status", "--porcelain=v1", "-z"].map(OsString::from),
        )?;
        let source_ref = self.read_source_ref(&worktree)?;

        Ok(WorktreeSnapshot {
            worktree,
            common_dir,
            head_sha,
            source_ref,
            clean: status.stdout.is_empty(),
        })
    }

    pub fn verify_integration_branch_absent(
        &self,
        repository: impl AsRef<Path>,
        branch: &str,
    ) -> Result<(), GitSafetyError> {
        let reference = format!("refs/heads/{}", normalize_branch_name(branch)?);
        let output = self.git_output_allow_status(
            repository.as_ref(),
            [
                OsString::from("show-ref"),
                OsString::from("--verify"),
                OsString::from("--quiet"),
                OsString::from(reference),
            ],
        )?;
        match output.status.code() {
            Some(0) => Err(git_error(
                "INTEGRATION_BRANCH_EXISTS",
                "the requested integration branch already exists",
            )),
            Some(1) => Ok(()),
            _ => Err(command_failure("show-ref", &output)),
        }
    }

    pub fn inspect_integration(
        &self,
        path: impl AsRef<Path>,
        facts: &RunFacts,
    ) -> Result<IntegrationSnapshot, GitSafetyError> {
        let worktree = self.inspect_worktree(path)?;
        let source_ref = worktree.source_ref.as_ref().ok_or_else(|| {
            git_error(
                "DETACHED_INTEGRATION",
                "integration result must be attached to its new branch",
            )
        })?;
        let branch = source_ref
            .name
            .strip_prefix("refs/heads/")
            .ok_or_else(|| git_error("INVALID_REF", "integration ref is not a local branch"))?
            .to_owned();

        let unmerged = self.git_output(
            &worktree.worktree,
            ["ls-files", "-u", "-z"].map(OsString::from),
        )?;
        let unmerged_entries = parse_unmerged_entries(&unmerged.stdout);
        let primary_is_ancestor =
            self.is_ancestor(&worktree.worktree, &facts.primary_sha, &worktree.head_sha)?;
        let reviewer_is_ancestor =
            self.is_ancestor(&worktree.worktree, &facts.reviewer_sha, &worktree.head_sha)?;
        let changed_files =
            self.changed_files(&worktree.worktree, &facts.primary_sha, &worktree.head_sha)?;
        let conflict_marker_files = scan_conflict_markers(&worktree.worktree, &changed_files)?;
        let primary_source_ref_target =
            self.optional_ref_target(&worktree.worktree, facts.primary_ref.as_deref())?;
        let reviewer_source_ref_target =
            self.optional_ref_target(&worktree.worktree, facts.reviewer_ref.as_deref())?;

        Ok(IntegrationSnapshot {
            worktree,
            branch,
            changed_files,
            unmerged_entries,
            conflict_marker_files,
            primary_is_ancestor,
            reviewer_is_ancestor,
            primary_source_ref_target,
            reviewer_source_ref_target,
        })
    }

    fn changed_files(
        &self,
        worktree: &Path,
        baseline: &str,
        integration: &str,
    ) -> Result<Vec<PathBuf>, GitSafetyError> {
        validate_sha(baseline, "changed-file baseline")?;
        validate_sha(integration, "changed-file integration SHA")?;
        let output = self.git_output(
            worktree,
            [
                OsString::from("diff"),
                OsString::from("--name-only"),
                OsString::from("-z"),
                OsString::from(baseline),
                OsString::from(integration),
                OsString::from("--"),
            ],
        )?;
        parse_changed_files(&output.stdout)
    }

    pub fn verify_source_refs_unchanged(
        &self,
        repository: impl AsRef<Path>,
        facts: &RunFacts,
    ) -> Result<(), GitSafetyError> {
        let primary =
            self.optional_ref_target(repository.as_ref(), facts.primary_ref.as_deref())?;
        let reviewer =
            self.optional_ref_target(repository.as_ref(), facts.reviewer_ref.as_deref())?;
        verify_source_ref_target(
            "primary",
            facts.primary_ref.as_deref(),
            &facts.primary_sha,
            primary.as_deref(),
        )?;
        verify_source_ref_target(
            "reviewer",
            facts.reviewer_ref.as_deref(),
            &facts.reviewer_sha,
            reviewer.as_deref(),
        )
    }

    fn read_source_ref(&self, worktree: &Path) -> Result<Option<SourceRef>, GitSafetyError> {
        let output = self.git_output_allow_status(
            worktree,
            ["symbolic-ref", "--quiet", "HEAD"].map(OsString::from),
        )?;
        match output.status.code() {
            Some(0) => {
                let name = output_text("symbolic-ref", &output)?;
                if !name.starts_with("refs/heads/") {
                    return Err(git_error(
                        "INVALID_REF",
                        "source symbolic ref is not a local branch",
                    ));
                }
                let target_sha = self.ref_target(worktree, &name)?;
                Ok(Some(SourceRef { name, target_sha }))
            }
            Some(1) => Ok(None),
            _ => Err(command_failure("symbolic-ref", &output)),
        }
    }

    fn optional_ref_target(
        &self,
        worktree: &Path,
        reference: Option<&str>,
    ) -> Result<Option<String>, GitSafetyError> {
        reference
            .map(|reference| self.ref_target(worktree, reference))
            .transpose()
    }

    fn ref_target(&self, worktree: &Path, reference: &str) -> Result<String, GitSafetyError> {
        if !reference.starts_with("refs/heads/") {
            return Err(git_error(
                "INVALID_REF",
                "only local branch refs are allowed",
            ));
        }
        let output = self.git_output(
            worktree,
            [
                OsString::from("show-ref"),
                OsString::from("--verify"),
                OsString::from("--hash"),
                OsString::from(reference),
            ],
        )?;
        let sha = output_text("show-ref", &output)?;
        validate_sha(&sha, "ref target")?;
        Ok(sha)
    }

    fn is_ancestor(
        &self,
        worktree: &Path,
        ancestor: &str,
        descendant: &str,
    ) -> Result<bool, GitSafetyError> {
        validate_sha(ancestor, "ancestor")?;
        validate_sha(descendant, "descendant")?;
        let output = self.git_output_allow_status(
            worktree,
            [
                OsString::from("merge-base"),
                OsString::from("--is-ancestor"),
                OsString::from(ancestor),
                OsString::from(descendant),
            ],
        )?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(command_failure("merge-base", &output)),
        }
    }

    fn git_text<const N: usize>(
        &self,
        worktree: &Path,
        args: [&str; N],
    ) -> Result<String, GitSafetyError> {
        let command = args.first().copied().unwrap_or("git");
        let output = self.git_output(worktree, args.map(OsString::from))?;
        output_text(command, &output)
    }

    fn git_output<I, S>(&self, worktree: &Path, args: I) -> Result<Output, GitSafetyError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.git_output_allow_status(worktree, args)?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(command_failure("git", &output))
        }
    }

    fn git_output_allow_status<I, S>(
        &self,
        worktree: &Path,
        args: I,
    ) -> Result<Output, GitSafetyError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args = args
            .into_iter()
            .map(|argument| argument.as_ref().to_owned())
            .collect::<Vec<_>>();
        validate_read_only_command(&args)?;
        Command::new(&self.git_binary)
            .arg("-C")
            .arg(worktree)
            .args(&args)
            .output()
            .map_err(|error| {
                git_error(
                    "GIT_COMMAND_FAILED",
                    format!("could not execute Git read command: {error}"),
                )
            })
    }
}

fn verify_frozen_source(
    label: &str,
    expected_sha: &str,
    expected_ref: Option<&str>,
    snapshot: &WorktreeSnapshot,
) -> Result<(), GitSafetyError> {
    if snapshot.head_sha != expected_sha {
        return Err(git_error(
            "SOURCE_DRIFT",
            format!("{label} HEAD changed after freeze"),
        ));
    }
    match (expected_ref, snapshot.source_ref.as_ref()) {
        (Some(expected), Some(observed))
            if observed.name == expected && observed.target_sha == expected_sha =>
        {
            Ok(())
        }
        (None, None) => Ok(()),
        _ => Err(git_error(
            "SOURCE_DRIFT",
            format!("{label} source ref changed after freeze"),
        )),
    }
}

fn verify_source_ref_target(
    label: &str,
    expected_ref: Option<&str>,
    expected_sha: &str,
    observed_target: Option<&str>,
) -> Result<(), GitSafetyError> {
    match (expected_ref, observed_target) {
        (Some(_), Some(observed)) if observed == expected_sha => Ok(()),
        (None, None) => Ok(()),
        _ => Err(git_error(
            "SOURCE_DRIFT",
            format!("{label} source ref moved during integration"),
        )),
    }
}

fn validate_read_only_command(args: &[OsString]) -> Result<(), GitSafetyError> {
    let utf8 = args
        .iter()
        .map(|argument| argument.to_str())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| git_error("UNSAFE_GIT_COMMAND", "Git arguments must be valid UTF-8"))?;
    let allowed = match utf8.as_slice() {
        ["rev-parse", "--show-toplevel" | "--git-common-dir" | "HEAD"] => true,
        ["status", "--porcelain=v1", "-z"] => true,
        ["symbolic-ref", "--quiet", "HEAD"] => true,
        ["show-ref", "--verify", "--hash", reference]
        | ["show-ref", "--verify", "--quiet", reference] => reference.starts_with("refs/heads/"),
        ["ls-files", "-u", "-z"] => true,
        ["merge-base", "--is-ancestor", ancestor, descendant] => {
            is_sha(ancestor) && is_sha(descendant)
        }
        ["diff", "--name-only", "-z", baseline, integration, "--"] => {
            is_sha(baseline) && is_sha(integration)
        }
        _ => false,
    };
    if allowed {
        Ok(())
    } else {
        Err(git_error(
            "UNSAFE_GIT_COMMAND",
            "GitInspector refused a command outside its read-only allowlist",
        ))
    }
}

fn canonical_git_path(base: &Path, raw: &str) -> Result<PathBuf, GitSafetyError> {
    let path = Path::new(raw);
    let candidate = if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    };
    fs::canonicalize(&candidate).map_err(|error| {
        git_error(
            "NOT_A_WORKTREE",
            format!(
                "cannot canonicalize Git path {}: {error}",
                candidate.display()
            ),
        )
    })
}

pub fn normalize_branch_name(branch: &str) -> Result<String, GitSafetyError> {
    let branch = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    let forbidden = [' ', '~', '^', ':', '?', '*', '[', '\\'];
    let valid = !branch.is_empty()
        && !branch.starts_with(['.', '/', '-'])
        && !branch.ends_with(['.', '/'])
        && !branch.ends_with(".lock")
        && !branch.contains("..")
        && !branch.contains("//")
        && !branch.contains("@{")
        && branch != "HEAD"
        && !branch.chars().any(char::is_control)
        && !branch
            .chars()
            .any(|character| forbidden.contains(&character))
        && branch != "@"
        && branch.split('/').all(|component| {
            !component.is_empty()
                && component != "."
                && component != ".."
                && !component.starts_with('.')
                && !component.ends_with('.')
                && !component.ends_with(".lock")
        });
    if valid {
        Ok(branch.to_owned())
    } else {
        Err(git_error(
            "INVALID_BRANCH_NAME",
            "integration branch name is not a safe local branch ref",
        ))
    }
}

fn parse_changed_files(bytes: &[u8]) -> Result<Vec<PathBuf>, GitSafetyError> {
    let mut paths = BTreeSet::new();
    for raw in bytes.split(|byte| *byte == 0).filter(|raw| !raw.is_empty()) {
        let text = std::str::from_utf8(raw).map_err(|error| {
            git_error(
                "INVALID_GIT_OUTPUT",
                format!("Git changed-file path is not UTF-8: {error}"),
            )
        })?;
        let path = PathBuf::from(text);
        validate_relative_changed_path(&path)?;
        paths.insert(path);
    }
    Ok(paths.into_iter().collect())
}

fn validate_relative_changed_path(path: &Path) -> Result<&Path, GitSafetyError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(git_error(
            "UNSAFE_CHANGED_PATH",
            "changed file paths must be normalized repository-relative paths",
        ));
    }
    Ok(path)
}

fn scan_conflict_markers(
    worktree: &Path,
    changed_files: &[PathBuf],
) -> Result<Vec<PathBuf>, GitSafetyError> {
    let mut markers = BTreeSet::new();
    for changed in changed_files {
        let candidate = if changed.is_absolute() {
            changed.clone()
        } else {
            worktree.join(changed)
        };
        if !candidate.exists() {
            continue;
        }
        let canonical = fs::canonicalize(&candidate).map_err(|error| {
            git_error(
                "UNSAFE_CHANGED_PATH",
                format!("cannot canonicalize changed path: {error}"),
            )
        })?;
        if !canonical.starts_with(worktree) {
            return Err(git_error(
                "UNSAFE_CHANGED_PATH",
                "changed path escapes the integration worktree",
            ));
        }
        let metadata = fs::metadata(&canonical).map_err(|error| {
            git_error(
                "UNREADABLE_CHANGED_FILE",
                format!("cannot inspect changed file: {error}"),
            )
        })?;
        if !metadata.is_file() {
            continue;
        }
        let mut file = fs::File::open(&canonical).map_err(|error| {
            git_error(
                "UNREADABLE_CHANGED_FILE",
                format!("cannot open changed file: {error}"),
            )
        })?;
        if stream_contains_conflict_markers(&mut file).map_err(|error| {
            git_error(
                "UNREADABLE_CHANGED_FILE",
                format!("cannot scan changed file: {error}"),
            )
        })? {
            markers.insert(canonical);
        }
    }
    Ok(markers.into_iter().collect())
}

fn stream_contains_conflict_markers(reader: &mut impl Read) -> std::io::Result<bool> {
    #[derive(Clone, Copy)]
    enum Stage {
        Outside,
        Ours,
        Separator,
    }

    fn advance(stage: Stage, first: Option<u8>, leading: usize, only_same: bool) -> (Stage, bool) {
        if first == Some(b'<') && leading >= 7 {
            return (Stage::Ours, false);
        }
        match stage {
            Stage::Ours if first == Some(b'=') && leading >= 7 && only_same => {
                (Stage::Separator, false)
            }
            Stage::Separator if first == Some(b'>') && leading >= 7 => (Stage::Outside, true),
            current => (current, false),
        }
    }

    let mut stage = Stage::Outside;
    let mut first = None;
    let mut leading = 0usize;
    let mut only_same = true;
    let mut binary = false;
    let mut found = false;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        for byte in &buffer[..read] {
            if *byte == 0 {
                binary = true;
            }
            if *byte == b'\n' {
                let (next, matched) = advance(stage, first, leading, only_same);
                stage = next;
                found |= matched;
                first = None;
                leading = 0;
                only_same = true;
                continue;
            }
            if *byte == b'\r' {
                continue;
            }
            match first {
                None => {
                    first = Some(*byte);
                    leading = 1;
                }
                Some(value) if *byte == value && only_same => leading += 1,
                Some(_) => only_same = false,
            }
        }
    }
    if first.is_some() {
        let (_, matched) = advance(stage, first, leading, only_same);
        found |= matched;
    }
    Ok(found && !binary)
}

fn parse_unmerged_entries(bytes: &[u8]) -> Vec<String> {
    let mut paths = BTreeSet::new();
    for entry in bytes
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        if let Some(tab) = entry.iter().position(|byte| *byte == b'\t') {
            paths.insert(String::from_utf8_lossy(&entry[tab + 1..]).into_owned());
        }
    }
    paths.into_iter().collect()
}

fn output_text(command: &str, output: &Output) -> Result<String, GitSafetyError> {
    String::from_utf8(output.stdout.clone())
        .map(|text| text.trim().to_owned())
        .map_err(|error| {
            git_error(
                "INVALID_GIT_OUTPUT",
                format!("git {command} returned non-UTF-8 output: {error}"),
            )
        })
}

fn validate_sha(value: &str, label: &str) -> Result<(), GitSafetyError> {
    if is_sha(value) {
        Ok(())
    } else {
        Err(git_error(
            "INVALID_GIT_OUTPUT",
            format!("{label} is not a 40-character commit SHA"),
        ))
    }
}

fn is_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn command_failure(command: &str, output: &Output) -> GitSafetyError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim().chars().take(2_000).collect::<String>();
    git_error(
        "GIT_COMMAND_FAILED",
        format!(
            "git {command} failed with {:?}: {stderr}",
            output.status.code()
        ),
    )
}

fn ensure_success(command: &str, output: &Output) -> Result<(), GitSafetyError> {
    if output.status.success() {
        Ok(())
    } else {
        Err(command_failure(command, output))
    }
}

fn git_error(code: &'static str, detail: impl Into<String>) -> GitSafetyError {
    GitSafetyError {
        code,
        detail: detail.into(),
    }
}
