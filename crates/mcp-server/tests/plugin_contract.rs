use std::{fs, path::PathBuf};

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
    assert_eq!(manifest["version"], "0.1.0");
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
                    "command": "codex-consensus",
                    "args": ["mcp-server"]
                }
            }
        })
    );
}

#[test]
fn skill_is_a_launcher_for_the_daemon_not_a_review_relay() {
    let root = repository_root();
    let skill =
        fs::read_to_string(root.join("plugin/skills/worktree-merge-consensus/SKILL.md")).unwrap();
    for required in [
        "consensus_doctor",
        "consensus_list_threads",
        "consensus_start",
        "run_id",
        "same host",
        "existing Codex tasks",
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
