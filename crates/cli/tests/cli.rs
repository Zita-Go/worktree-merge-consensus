use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

#[test]
fn help_lists_public_commands_but_not_internal_modes() {
    Command::cargo_bin("codex-consensus")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("doctor"))
        .stdout(predicate::str::contains("configure"))
        .stdout(predicate::str::contains("threads"))
        .stdout(predicate::str::contains("worktrees"))
        .stdout(predicate::str::contains("run"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("watch"))
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
fn run_requires_both_worktree_flags_together() {
    Command::cargo_bin("codex-consensus")
        .unwrap()
        .args(["run", "--primary-worktree", "/repo/primary"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--primary-worktree and --reviewer-worktree must be provided together",
        ));
}

#[test]
fn json_run_requires_all_four_binding_flags() {
    let output = Command::cargo_bin("codex-consensus")
        .unwrap()
        .args([
            "run",
            "--primary-thread",
            "primary",
            "--reviewer-thread",
            "reviewer",
            "--json",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "INVALID_ARGUMENTS");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("all four binding flags")
    );
}

#[test]
fn worktree_discovery_help_requires_repository_anchor() {
    Command::cargo_bin("codex-consensus")
        .unwrap()
        .args(["worktrees", "list", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--repository"));
}

#[test]
fn watch_rejects_a_negative_resume_cursor() {
    Command::cargo_bin("codex-consensus")
        .unwrap()
        .args(["watch", "run-123", "--after-cursor=-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--after-cursor must be at least 0",
        ));
}

#[test]
fn doctor_rejects_legacy_standalone_skill_without_deleting_it() {
    let codex_home = tempfile::tempdir().unwrap();
    let skill = codex_home
        .path()
        .join("skills/worktree-merge-consensus/SKILL.md");
    fs::create_dir_all(skill.parent().unwrap()).unwrap();
    fs::write(&skill, "legacy workflow\n").unwrap();

    let output = Command::cargo_bin("codex-consensus")
        .unwrap()
        .env("CODEX_HOME", codex_home.path())
        .args(["doctor", "--json"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "LEGACY_SKILL_CONFLICT");
    assert_eq!(fs::read_to_string(skill).unwrap(), "legacy workflow\n");
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

#[test]
fn hidden_mcp_server_mode_serves_the_protocol_over_stdio() {
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        "\n",
    );
    let output = Command::cargo_bin("codex-consensus")
        .unwrap()
        .arg("mcp-server")
        .write_stdin(input)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let responses = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(responses[1]["result"]["tools"].as_array().unwrap().len(), 9);
}

#[test]
fn hidden_participant_mcp_server_mode_lists_only_patch_tool() {
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        "\n",
    );
    let output = Command::cargo_bin("codex-consensus")
        .unwrap()
        .arg("participant-mcp-server")
        .write_stdin(input)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let responses = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(responses[1]["result"]["tools"].as_array().unwrap().len(), 1);
    assert_eq!(
        responses[1]["result"]["tools"][0]["name"],
        "consensus_apply_patch"
    );

    Command::cargo_bin("codex-consensus")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("participant-mcp-server").not());
}
