use std::{fs, path::PathBuf, process::Command};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use serde_json::{Value, json};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn plugin_manifest_and_mcp_registration_match_the_binary() {
    let root = repository_root();
    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(root.join("plugin/.codex-plugin/plugin.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["name"], "worktree-merge-consensus");
    assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest["license"], "Apache-2.0");
    assert_eq!(manifest["skills"], "./skills/");
    assert_eq!(manifest["mcpServers"], "./.mcp.json");

    let mcp: Value =
        serde_json::from_str(&fs::read_to_string(root.join("plugin/.mcp.json")).unwrap()).unwrap();
    assert_eq!(
        mcp,
        json!({
            "mcpServers": {
                "worktreeMergeConsensus": {
                    "title": "Worktree Merge Consensus",
                    "description": "Coordinate reviewed integration across two existing Codex tasks.",
                    "cwd": ".",
                    "command": "/bin/sh",
                    "args": ["./scripts/start-mcp.sh"]
                }
            }
        })
    );
}

#[cfg(unix)]
#[test]
fn plugin_mcp_launcher_uses_the_explicit_binary_override() {
    let root = repository_root();
    let temp = tempfile::tempdir().unwrap();
    let fake_binary = temp.path().join("codex-consensus");
    fs::write(&fake_binary, "#!/bin/sh\nprintf '%s\\n' \"$1\"\n").unwrap();
    fs::set_permissions(&fake_binary, fs::Permissions::from_mode(0o755)).unwrap();

    let output = Command::new("/bin/sh")
        .arg(root.join("plugin/scripts/start-mcp.sh"))
        .env("CODEX_CONSENSUS_BIN", &fake_binary)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "mcp-server\n");
}

#[test]
fn skill_is_a_launcher_for_the_daemon_not_a_review_relay() {
    let root = repository_root();
    let skill =
        fs::read_to_string(root.join("plugin/skills/worktree-merge-consensus/SKILL.md")).unwrap();
    for required in [
        "consensus_doctor",
        "MCP tool, not a shell command",
        "codex-consensus doctor",
        "codex-consensus mcp-server",
        "Never run `consensus_doctor` as an executable",
        "codex-consensus threads list",
        "codex-consensus worktrees list",
        "codex-consensus run",
        "codex-consensus status",
        "codex-consensus resume",
        "codex-consensus cancel",
        "codex mcp list --json",
        "consensus_list_threads",
        "consensus_list_worktrees",
        "consensus_apply_patch",
        "consensus_start",
        "primary_worktree",
        "reviewer_worktree",
        "repository_path",
        "task cwd",
        "run_id",
        "same host",
        "existing Codex tasks",
        "dangerFullAccess",
        "trusted tasks",
        "coordinator-owned verification",
        "do not run Shell in the verification marker turn",
        "End the launch turn",
    ] {
        assert!(
            skill.contains(required),
            "missing launcher contract: {required}"
        );
    }

    let lowercase = skill.to_lowercase();
    for forbidden in [
        "subagent",
        "create_thread",
        "fork_thread",
        "send_message_to_thread",
        "wait_threads",
        "git push",
        "pull request",
        "source-branch mutation",
    ] {
        assert!(
            !lowercase.contains(forbidden),
            "launcher skill mentions forbidden mechanism: {forbidden}"
        );
    }

    let metadata =
        fs::read_to_string(root.join("plugin/skills/worktree-merge-consensus/agents/openai.yaml"))
            .unwrap();
    assert!(metadata.contains("$worktree-merge-consensus"));
}
