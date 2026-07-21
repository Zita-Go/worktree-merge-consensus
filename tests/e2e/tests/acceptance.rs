#![cfg(unix)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

const PRIMARY_THREAD: &str = "primary-thread";
const REVIEWER_THREAD: &str = "reviewer-thread";
const TERMINAL_STATUSES: [&str; 5] = [
    "ACCEPTED",
    "BLOCKED",
    "CANCELLED",
    "PAUSED_USER_ACTION",
    "INCOMPATIBLE_CODEX",
];

#[test]
fn conflict_free() {
    let fixture = AcceptanceFixture::new("conflict_free", false);
    assert!(!fixture.task_cwd.join(".git").exists());
    assert_ne!(fixture.task_cwd, fixture.repository.primary);
    assert_ne!(fixture.task_cwd, fixture.repository.reviewer);
    let (run_id, _daemon) = fixture.start();
    let accepted = fixture.wait_for_terminal(&run_id);

    assert_eq!(
        accepted["status"],
        "ACCEPTED",
        "state={accepted}\nevents={}",
        fixture.events()
    );
    assert_eq!(accepted["integration_branch"], fixture.branch);
    let integration_sha = git_text(&fixture.repository.primary, &["rev-parse", "HEAD"]);
    assert_eq!(accepted["integration_sha"], integration_sha);
    assert_eq!(
        accepted["accepted_result"]["integration_sha"],
        integration_sha
    );
    assert_eq!(
        accepted["accepted_result"]["tests"][0]["command"],
        "test -f reviewer.txt"
    );
    let verification_worktree = PathBuf::from(
        accepted["verification_worktree"]
            .as_str()
            .expect("accepted run must retain the verification worktree"),
    );
    let accepted_test = &accepted["accepted_result"]["tests"][0];
    assert_eq!(accepted_test["cwd"], json!(verification_worktree));
    assert!(
        accepted_test["turn_id"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(
        accepted_test["item_id"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert_eq!(git_text(&verification_worktree, &["remote"]), "");
    assert_eq!(
        git_text(&verification_worktree, &["branch", "--show-current"]),
        ""
    );
    assert_ne!(
        git_text(
            &verification_worktree,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"]
        ),
        fixture.repository.git_common_dir.to_string_lossy()
    );
    assert_eq!(accepted["accepted_result"]["source_refs_unchanged"], true);
    assert_eq!(
        accepted["accepted_result"]["publication"]["local_only"],
        true
    );
    assert_eq!(accepted["accepted_result"]["publication"]["pushed"], false);
    assert_eq!(
        accepted["facts"]["primary_sha"],
        fixture.repository.primary_sha
    );
    assert_eq!(
        accepted["facts"]["reviewer_sha"],
        fixture.repository.reviewer_sha
    );
    fixture.assert_source_refs_unchanged();
    assert!(git_success(
        &fixture.repository.primary,
        &[
            "merge-base",
            "--is-ancestor",
            &fixture.repository.primary_sha,
            &integration_sha
        ]
    ));
    assert!(git_success(
        &fixture.repository.primary,
        &[
            "merge-base",
            "--is-ancestor",
            &fixture.repository.reviewer_sha,
            &integration_sha
        ]
    ));
    assert_eq!(
        git_text(&fixture.repository.primary, &["status", "--porcelain"]),
        ""
    );
    assert_eq!(git_text(&fixture.repository.primary, &["remote"]), "");
    assert!(!fixture.events().contains("git push"));
}

#[test]
fn conflict_resolution() {
    let fixture = AcceptanceFixture::new("conflict_resolution", true);
    let (run_id, _daemon) = fixture.start();
    let accepted = fixture.wait_for_terminal(&run_id);

    assert_eq!(
        accepted["status"],
        "ACCEPTED",
        "state={accepted}\nevents={}",
        fixture.events()
    );
    assert_eq!(
        fs::read_to_string(fixture.repository.primary.join("shared.txt")).unwrap(),
        "primary decision\nreviewer decision\n"
    );
    assert!(!fixture.events().contains("<<<<<<<"));
    fixture.assert_source_refs_unchanged();
}

#[test]
fn multiple_plan_revisions() {
    let fixture = AcceptanceFixture::new("plan_revision", false);
    let (run_id, _daemon) = fixture.start();
    let accepted = fixture.wait_for_terminal(&run_id);

    assert_eq!(accepted["status"], "ACCEPTED");
    assert_eq!(accepted["plan_revision"], 2);
    assert_eq!(fixture.action_count("REQUEST_REVIEWER_PLAN_VERDICT"), 2);
    assert_eq!(fixture.action_count("REQUEST_PRIMARY_INTEGRATION"), 1);
}

#[test]
fn result_rejection_requires_a_new_sha() {
    let fixture = AcceptanceFixture::new("result_revision", false);
    let (run_id, _daemon) = fixture.start();
    let accepted = fixture.wait_for_terminal(&run_id);

    assert_eq!(accepted["status"], "ACCEPTED");
    assert_eq!(fixture.action_count("REQUEST_PRIMARY_INTEGRATION"), 2);
    let shas = fixture.integration_shas();
    assert_eq!(shas.len(), 2);
    assert_ne!(shas[0], shas[1]);
    assert_eq!(accepted["integration_sha"], shas[1]);
}

#[test]
fn source_drift_blocks_before_integration() {
    let fixture = AcceptanceFixture::new("source_drift", false);
    let (run_id, _daemon) = fixture.start();
    fixture.wait_for_action("REQUEST_PRIMARY_CONTRACT", 1);

    fs::write(fixture.repository.reviewer.join("drift.txt"), "drift\n").unwrap();
    git(&fixture.repository.reviewer, &["add", "drift.txt"]);
    git(
        &fixture.repository.reviewer,
        &["commit", "-m", "source drift"],
    );
    fixture.release_deferred("complete");
    let blocked = fixture.wait_for_terminal(&run_id);

    assert_eq!(blocked["status"], "BLOCKED");
    assert_eq!(blocked["reason_code"], "SOURCE_DRIFT");
    assert_eq!(fixture.action_count("REQUEST_PRIMARY_INTEGRATION"), 0);
    fixture.assert_branch_absent();
}

#[test]
fn dirty_worktree_is_rejected_before_a_run_is_created() {
    let fixture = AcceptanceFixture::new("conflict_free", false);
    fs::write(fixture.repository.reviewer.join("dirty.txt"), "dirty\n").unwrap();

    let error = fixture.start_error();

    assert_eq!(error["error"]["code"], "DIRTY_WORKTREE");
    fixture.assert_source_refs_unchanged();
    fixture.assert_branch_absent();
}

#[test]
fn existing_integration_branch_is_rejected() {
    let fixture = AcceptanceFixture::new("conflict_free", false);
    git(
        &fixture.repository.primary,
        &["branch", &fixture.branch, &fixture.repository.primary_sha],
    );

    let error = fixture.start_error();

    assert_eq!(error["error"]["code"], "INTEGRATION_BRANCH_EXISTS");
    fixture.assert_source_refs_unchanged();
}

#[test]
fn detached_primary_source_can_integrate_on_a_new_attached_branch() {
    let fixture = AcceptanceFixture::new("conflict_free", false);
    git(&fixture.repository.primary, &["switch", "--detach"]);
    let (run_id, _daemon) = fixture.start();
    let accepted = fixture.wait_for_terminal(&run_id);

    assert_eq!(accepted["status"], "ACCEPTED");
    assert_eq!(
        git_text(&fixture.repository.primary, &["branch", "--show-current"]),
        fixture.branch
    );
    fixture.assert_source_refs_unchanged();
}

#[test]
fn invalid_model_reply_blocks_fail_closed() {
    let fixture = AcceptanceFixture::new("invalid_reply", false);
    let (run_id, _daemon) = fixture.start();
    let blocked = fixture.wait_for_terminal(&run_id);

    assert_eq!(blocked["status"], "BLOCKED");
    assert_eq!(blocked["reason_code"], "INVALID_RESPONSE");
    fixture.assert_branch_absent();
}

#[test]
fn repeated_unchanged_plan_feedback_blocks_as_no_progress() {
    let fixture = AcceptanceFixture::new("no_progress", false);
    let (run_id, _daemon) = fixture.start();
    let blocked = fixture.wait_for_terminal(&run_id);

    assert_eq!(blocked["status"], "BLOCKED");
    assert_eq!(blocked["reason_code"], "NO_PROGRESS");
    assert_eq!(fixture.action_count("REQUEST_PRIMARY_INTEGRATION"), 0);
}

#[test]
fn review_round_limit_blocks_without_integration() {
    let fixture = AcceptanceFixture::new("round_limit", false);
    let (run_id, _daemon) = fixture.start();
    let blocked = fixture.wait_for_terminal(&run_id);

    assert_eq!(blocked["status"], "BLOCKED");
    assert_eq!(blocked["reason_code"], "ROUND_LIMIT");
    assert_eq!(fixture.action_count("REQUEST_REVIEWER_PLAN_VERDICT"), 6);
    assert_eq!(fixture.action_count("REQUEST_PRIMARY_INTEGRATION"), 0);
}

#[test]
fn user_input_pause_resumes_the_existing_integration_turn() {
    let fixture = AcceptanceFixture::new("user_input_pause", false);
    let (run_id, _daemon) = fixture.start();
    let paused = fixture.wait_for_terminal(&run_id);
    assert_eq!(paused["status"], "PAUSED_USER_ACTION");
    assert_eq!(paused["reason_code"], "PERMISSION_REQUIRED");

    fixture.release_deferred("approve");
    fixture
        .environment()
        .cli_json(&["resume", &run_id, "--json"]);
    let accepted = fixture.wait_for_terminal(&run_id);

    assert_eq!(
        accepted["status"],
        "ACCEPTED",
        "state={accepted}\nevents={}",
        fixture.events()
    );
    assert_eq!(fixture.action_count("REQUEST_PRIMARY_INTEGRATION"), 1);
    fixture.assert_source_refs_unchanged();
}

#[test]
fn daemon_crash_restart_recovers_without_a_duplicate_turn() {
    let fixture = AcceptanceFixture::new("crash_restart", false);
    let (run_id, daemon) = fixture.start();
    fixture.wait_for_action("REQUEST_PRIMARY_INTEGRATION", 1);
    fixture.wait_for_pending_turn();

    daemon.kill();
    fixture.release_deferred("complete");
    let accepted = fixture.wait_for_terminal(&run_id);

    assert_eq!(
        accepted["status"],
        "ACCEPTED",
        "state={accepted}\nevents={}",
        fixture.events()
    );
    assert_eq!(fixture.action_count("REQUEST_PRIMARY_INTEGRATION"), 1);
}

#[test]
fn daemon_crash_recovers_a_dirty_in_progress_verification_clone() {
    let fixture = AcceptanceFixture::new("crash_verification", false);
    let (run_id, daemon) = fixture.start_with_test("touch verification-artifact.txt");
    fixture.wait_for_action("REQUEST_PRIMARY_VERIFICATION", 1);
    fixture.wait_for_pending_turn();

    daemon.kill();
    fixture.release_deferred("complete");
    let accepted = fixture.wait_for_terminal(&run_id);

    assert_eq!(
        accepted["status"],
        "ACCEPTED",
        "state={accepted}\nevents={}",
        fixture.events()
    );
    assert_eq!(fixture.action_count("REQUEST_PRIMARY_VERIFICATION"), 1);
    assert_eq!(
        accepted["accepted_result"]["tests"][0]["command"],
        "touch verification-artifact.txt"
    );
    let verification_worktree = PathBuf::from(
        accepted["verification_worktree"]
            .as_str()
            .expect("verification worktree must be persisted"),
    );
    assert!(
        verification_worktree
            .join("verification-artifact.txt")
            .exists()
    );
    fixture.assert_source_refs_unchanged();
}

#[test]
fn duplicate_notifications_do_not_duplicate_turns() {
    let fixture = AcceptanceFixture::new("duplicate_notification", false);
    let (run_id, _daemon) = fixture.start();
    let accepted = fixture.wait_for_terminal(&run_id);

    assert_eq!(accepted["status"], "ACCEPTED");
    assert_eq!(fixture.action_count("REQUEST_PRIMARY_CONTRACT"), 1);
    assert_eq!(fixture.action_count("REQUEST_REVIEWER_RESULT_VERDICT"), 1);
    assert_eq!(fixture.event_count("duplicate notification"), 2);
}

#[test]
fn cancellation_stops_before_integration_and_preserves_git_state() {
    let fixture = AcceptanceFixture::new("cancellation", false);
    let (run_id, _daemon) = fixture.start();
    fixture.wait_for_action("REQUEST_PRIMARY_CONTRACT", 1);

    let cancelled = fixture
        .environment()
        .cli_json(&["cancel", &run_id, "--json"]);

    assert_eq!(cancelled["status"], "CANCELLED");
    assert_eq!(fixture.action_count("REQUEST_PRIMARY_INTEGRATION"), 0);
    fixture.assert_source_refs_unchanged();
    fixture.assert_branch_absent();
}

struct AcceptanceFixture {
    _temp: tempfile::TempDir,
    repository: RepositoryFixture,
    state_dir: PathBuf,
    fake_config: PathBuf,
    fake_state: PathBuf,
    task_cwd: PathBuf,
    branch: String,
}

impl AcceptanceFixture {
    fn new(scenario: &str, conflict: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repository = RepositoryFixture::new(temp.path(), conflict);
        let state_dir = temp.path().join("state");
        let fake_config = temp.path().join("fake-config.json");
        let fake_state = temp.path().join("fake-state");
        let task_cwd = temp.path().join("task-home");
        fs::create_dir(&task_cwd).unwrap();
        let branch = format!("consensus/e2e-{}", scenario.replace('_', "-"));
        fs::write(
            &fake_config,
            serde_json::to_vec_pretty(&json!({
                "scenario": scenario,
                "primary_thread": PRIMARY_THREAD,
                "reviewer_thread": REVIEWER_THREAD,
                "primary_thread_cwd": task_cwd,
                "reviewer_thread_cwd": task_cwd,
                "primary_worktree": repository.primary,
                "reviewer_worktree": repository.reviewer,
                "git_common_dir": repository.git_common_dir,
                "integration_branch": branch,
                "state_directory": fake_state
            }))
            .unwrap(),
        )
        .unwrap();
        Self {
            _temp: temp,
            repository,
            state_dir,
            fake_config,
            fake_state,
            task_cwd,
            branch,
        }
    }

    fn environment(&self) -> TestEnvironment<'_> {
        let binaries = test_binaries();
        TestEnvironment {
            cli: &binaries.cli,
            fake_codex: &binaries.fake_codex,
            fake_config: &self.fake_config,
            state_dir: &self.state_dir,
        }
    }

    fn start(&self) -> (String, DaemonGuard) {
        self.start_with_test("test -f reviewer.txt")
    }

    fn start_with_test(&self, test_command: &str) -> (String, DaemonGuard) {
        let started = self.environment().cli_json(&[
            "run",
            "--primary-thread",
            PRIMARY_THREAD,
            "--reviewer-thread",
            REVIEWER_THREAD,
            "--primary-worktree",
            self.repository.primary.to_str().unwrap(),
            "--reviewer-worktree",
            self.repository.reviewer.to_str().unwrap(),
            "--integration-branch",
            &self.branch,
            "--test",
            test_command,
            "--json",
        ]);
        let daemon = DaemonGuard::new(&self.state_dir);
        let run_id = started["run_id"].as_str().unwrap().to_owned();
        (run_id, daemon)
    }

    fn start_error(&self) -> Value {
        let output = self.environment().cli_output(&[
            "run",
            "--primary-thread",
            PRIMARY_THREAD,
            "--reviewer-thread",
            REVIEWER_THREAD,
            "--primary-worktree",
            self.repository.primary.to_str().unwrap(),
            "--reviewer-worktree",
            self.repository.reviewer.to_str().unwrap(),
            "--integration-branch",
            &self.branch,
            "--json",
        ]);
        let _daemon = DaemonGuard::new(&self.state_dir);
        assert!(!output.status.success(), "run unexpectedly succeeded");
        parse_cli_json(&output)
    }

    fn wait_for_terminal(&self, run_id: &str) -> Value {
        self.environment()
            .wait_for_terminal(run_id, Duration::from_secs(20))
    }

    fn events(&self) -> String {
        fs::read_to_string(self.fake_state.join("events.log")).unwrap_or_default()
    }

    fn action_count(&self, action: &str) -> usize {
        self.events()
            .lines()
            .filter(|line| line.starts_with("turn ") && line.ends_with(action))
            .count()
    }

    fn event_count(&self, event: &str) -> usize {
        self.events().lines().filter(|line| *line == event).count()
    }

    fn integration_shas(&self) -> Vec<String> {
        self.events()
            .lines()
            .filter_map(|line| line.strip_prefix("integration-sha "))
            .map(str::to_owned)
            .collect()
    }

    fn wait_for_action(&self, action: &str, count: usize) {
        let started = Instant::now();
        while self.action_count(action) < count {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "fake App Server never observed {action}: {}",
                self.events()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn release_deferred(&self, marker: &str) {
        fs::create_dir_all(&self.fake_state).unwrap();
        fs::write(self.fake_state.join(marker), "released\n").unwrap();
    }

    fn wait_for_pending_turn(&self) {
        let started = Instant::now();
        loop {
            let pending = fs::read_dir(&self.fake_state)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .any(|entry| entry.file_name().to_string_lossy().starts_with("pending-"));
            if pending {
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "fake App Server never persisted a pending turn: {}",
                self.events()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn assert_source_refs_unchanged(&self) {
        assert_eq!(
            git_text(
                &self.repository.primary,
                &["rev-parse", "refs/heads/primary"]
            ),
            self.repository.primary_sha
        );
        assert_eq!(
            git_text(
                &self.repository.primary,
                &["rev-parse", "refs/heads/reviewer"]
            ),
            self.repository.reviewer_sha
        );
    }

    fn assert_branch_absent(&self) {
        assert!(!git_success(
            &self.repository.primary,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{}", self.branch)
            ]
        ));
    }
}

struct TestBinaries {
    cli: PathBuf,
    fake_codex: PathBuf,
}

fn test_binaries() -> &'static TestBinaries {
    static BINARIES: OnceLock<TestBinaries> = OnceLock::new();
    BINARIES.get_or_init(|| {
        let root = repository_root();
        let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .current_dir(&root)
            .args([
                "build",
                "--locked",
                "-p",
                "codex-consensus",
                "-p",
                "consensus-fake-app-server",
            ])
            .output()
            .unwrap();
        assert_success("build test binaries", &output);
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("target"));
        TestBinaries {
            cli: target.join("debug/codex-consensus"),
            fake_codex: target.join("debug/consensus-fake-app-server"),
        }
    })
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct TestEnvironment<'a> {
    cli: &'a Path,
    fake_codex: &'a Path,
    fake_config: &'a Path,
    state_dir: &'a Path,
}

impl TestEnvironment<'_> {
    fn cli_output(&self, arguments: &[&str]) -> Output {
        Command::new(self.cli)
            .arg("--state-dir")
            .arg(self.state_dir)
            .args(arguments)
            .env("CODEX_CONSENSUS_CODEX_BINARY", self.fake_codex)
            .env("CONSENSUS_FAKE_CONFIG", self.fake_config)
            .output()
            .unwrap()
    }

    fn cli_json(&self, arguments: &[&str]) -> Value {
        let output = self.cli_output(arguments);
        assert_success("codex-consensus", &output);
        parse_cli_json(&output)
    }

    fn wait_for_terminal(&self, run_id: &str, timeout: Duration) -> Value {
        let started = Instant::now();
        loop {
            let status = self.cli_json(&["status", run_id, "--json"]);
            let status_name = status["status"].as_str().unwrap_or_default();
            if TERMINAL_STATUSES.contains(&status_name) {
                return status;
            }
            assert!(
                started.elapsed() < timeout,
                "run {run_id} did not finish: {status}"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }
}

struct RepositoryFixture {
    primary: PathBuf,
    reviewer: PathBuf,
    git_common_dir: PathBuf,
    primary_sha: String,
    reviewer_sha: String,
}

impl RepositoryFixture {
    fn new(root: &Path, conflict: bool) -> Self {
        let repository = root.join("repository");
        let primary = root.join("primary");
        let reviewer = root.join("reviewer");
        fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "--initial-branch=base"]);
        git(&repository, &["config", "user.name", "Consensus Test"]);
        git(
            &repository,
            &["config", "user.email", "consensus@example.invalid"],
        );
        fs::write(repository.join("base.txt"), "base\n").unwrap();
        if conflict {
            fs::write(repository.join("shared.txt"), "base\n").unwrap();
        }
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "-m", "base"]);
        git(&repository, &["branch", "primary"]);
        git(&repository, &["branch", "reviewer"]);
        git(
            &repository,
            &["worktree", "add", primary.to_str().unwrap(), "primary"],
        );
        git(
            &repository,
            &["worktree", "add", reviewer.to_str().unwrap(), "reviewer"],
        );

        fs::write(primary.join("primary.txt"), "primary feature\n").unwrap();
        if conflict {
            fs::write(primary.join("shared.txt"), "primary decision\n").unwrap();
        }
        git(&primary, &["add", "."]);
        git(&primary, &["commit", "-m", "primary implementation"]);
        let primary_sha = git_text(&primary, &["rev-parse", "HEAD"]);

        fs::write(reviewer.join("reviewer.txt"), "reviewer feature\n").unwrap();
        if conflict {
            fs::write(reviewer.join("shared.txt"), "reviewer decision\n").unwrap();
        }
        git(&reviewer, &["add", "."]);
        git(&reviewer, &["commit", "-m", "reviewer implementation"]);
        let reviewer_sha = git_text(&reviewer, &["rev-parse", "HEAD"]);
        let git_common_dir = PathBuf::from(git_text(
            &primary,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        ));

        Self {
            primary,
            reviewer,
            git_common_dir,
            primary_sha,
            reviewer_sha,
        }
    }
}

struct DaemonGuard {
    state_dir: PathBuf,
}

impl DaemonGuard {
    fn new(state_dir: &Path) -> Self {
        Self {
            state_dir: state_dir.to_owned(),
        }
    }

    fn kill(&self) {
        let Ok(pid) = fs::read_to_string(self.state_dir.join("daemon.pid")) else {
            return;
        };
        let pid = pid.trim();
        let _ = Command::new("kill").arg(pid).status();
        for _ in 0..100 {
            let alive = Command::new("kill")
                .args(["-0", pid])
                .output()
                .is_ok_and(|output| output.status.success());
            if !alive {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

fn parse_cli_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "CLI returned invalid JSON: {error}; stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn git(cwd: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(arguments)
        .output()
        .unwrap();
    assert_success(&format!("git {arguments:?}"), &output);
}

fn git_text(cwd: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(arguments)
        .output()
        .unwrap();
    assert_success(&format!("git {arguments:?}"), &output);
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn git_success(cwd: &Path, arguments: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(arguments)
        .status()
        .unwrap()
        .success()
}

fn assert_success(action: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{action} failed with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
