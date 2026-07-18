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
    assert_eq!(responses[1]["result"]["tools"].as_array().unwrap().len(), 6);
}
