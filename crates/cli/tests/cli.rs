use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn help_lists_public_commands_but_not_internal_modes() {
    Command::cargo_bin("codex-consensus")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("doctor"))
        .stdout(predicate::str::contains("threads"))
        .stdout(predicate::str::contains("run"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("resume"))
        .stdout(predicate::str::contains("cancel"))
        .stdout(predicate::str::contains("daemon serve").not())
        .stdout(predicate::str::contains("\n  daemon ").not())
        .stdout(predicate::str::contains("mcp-server").not());
}

#[test]
fn run_requires_both_thread_flags_together() {
    Command::cargo_bin("codex-consensus")
        .unwrap()
        .args(["run", "--primary-thread", "primary-only"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--primary-thread and --reviewer-thread must be provided together",
        ));
}

#[test]
fn json_argument_error_is_one_stdout_object_without_stderr_decoration() {
    let output = Command::cargo_bin("codex-consensus")
        .unwrap()
        .args(["run", "--primary-thread", "primary-only", "--json"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "INVALID_ARGUMENTS");
    assert_eq!(
        output.stdout.iter().filter(|byte| **byte == b'\n').count(),
        1
    );
}

#[test]
fn hidden_daemon_help_is_available_for_lifecycle_startup() {
    Command::cargo_bin("codex-consensus")
        .unwrap()
        .args(["daemon", "serve", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--state-dir"));
}
