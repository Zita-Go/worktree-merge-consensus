use consensus_core::state::{NextAction, RunState};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalDecision {
    Accept,
    Cancel,
}

pub(crate) fn decide_command_approval(state: &RunState, params: &Value) -> ApprovalDecision {
    let expected_cwd = match state.next_action {
        NextAction::RequestPrimaryIntegration => Some(&state.facts.primary_worktree),
        NextAction::RequestPrimaryVerification => state.verification_worktree.as_ref(),
        _ => None,
    };
    if params.get("cwd").and_then(Value::as_str) != expected_cwd.and_then(|path| path.to_str()) {
        return ApprovalDecision::Cancel;
    }
    if [
        "additionalPermissions",
        "networkApprovalContext",
        "proposedExecpolicyAmendment",
        "proposedNetworkPolicyAmendments",
    ]
    .iter()
    .any(|field| params.get(*field).is_some_and(|value| !value.is_null()))
    {
        return ApprovalDecision::Cancel;
    }
    if params.get("availableDecisions").is_some_and(|value| {
        !value.is_null()
            && !value.as_array().is_some_and(|decisions| {
                decisions
                    .iter()
                    .any(|decision| decision.as_str() == Some("accept"))
            })
    }) {
        return ApprovalDecision::Cancel;
    }
    let Some(command) = params
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty())
    else {
        return ApprovalDecision::Cancel;
    };
    if contains_shell_control(command) {
        return ApprovalDecision::Cancel;
    }
    match state.next_action {
        NextAction::RequestPrimaryIntegration if is_allowed_git_command(state, command) => {
            ApprovalDecision::Accept
        }
        NextAction::RequestPrimaryVerification
            if state
                .required_test_commands
                .iter()
                .any(|required| required.trim() == command && validate_test_command(required)) =>
        {
            ApprovalDecision::Accept
        }
        _ => ApprovalDecision::Cancel,
    }
}

pub(crate) fn validate_test_command(command: &str) -> bool {
    let command = command.trim();
    !command.is_empty()
        && !contains_shell_control(command)
        && !contains_forbidden_git_operation(command)
        && !uses_dynamic_command_launcher(command)
}

fn contains_shell_control(command: &str) -> bool {
    let Ok(tokens) = shell_words::split(command) else {
        return true;
    };
    tokens.iter().any(|token| {
        token.chars().any(|character| {
            matches!(
                character,
                ';' | '&' | '|' | '>' | '<' | '`' | '$' | '\n' | '\r' | '\0'
            )
        })
    })
}

fn contains_forbidden_git_operation(command: &str) -> bool {
    let Ok(tokens) = shell_words::split(command) else {
        return true;
    };
    tokens.iter().any(|token| executable_name(token) == "git")
}

fn uses_dynamic_command_launcher(command: &str) -> bool {
    let Ok(tokens) = shell_words::split(command) else {
        return true;
    };
    tokens.iter().enumerate().any(|(index, token)| {
        matches!(
            executable_name(token),
            "sh" | "bash"
                | "dash"
                | "zsh"
                | "fish"
                | "cmd"
                | "powershell"
                | "pwsh"
                | "python"
                | "python3"
                | "perl"
                | "ruby"
                | "node"
        ) && tokens.iter().skip(index + 1).any(|argument| {
            matches!(
                argument.as_str(),
                "-c" | "--command" | "-Command" | "-EncodedCommand" | "-e"
            )
        })
    })
}

fn executable_name(token: &str) -> &str {
    token.rsplit(['/', '\\']).next().unwrap_or(token)
}

fn is_allowed_git_command(state: &RunState, command: &str) -> bool {
    let Ok(tokens) = shell_words::split(command) else {
        return false;
    };
    let tokens = tokens.iter().map(String::as_str).collect::<Vec<_>>();
    let ["git", subcommand, rest @ ..] = tokens.as_slice() else {
        return false;
    };
    match *subcommand {
        "status" | "diff" | "show" | "log" | "rev-parse" | "merge-base" | "ls-files" => {
            safe_read_only_git_arguments(rest)
        }
        "switch" => {
            matches!(
                rest,
                ["-c", branch, sha]
                    if Some(*branch) == state.target_integration_branch.as_deref()
                        && *sha == state.facts.primary_sha
            )
        }
        "merge" => {
            matches!(
                rest,
                ["--no-ff", "--no-edit", sha] if *sha == state.facts.reviewer_sha
            )
        }
        "add" => matches!(rest, ["-A"] | ["--all"]),
        "commit" => {
            matches!(rest, ["--no-edit"])
                || matches!(rest, ["-m", message] if safe_commit_message(message))
        }
        _ => false,
    }
}

fn safe_read_only_git_arguments(arguments: &[&str]) -> bool {
    !arguments.iter().any(|argument| {
        matches!(
            *argument,
            "--no-index" | "--ext-diff" | "--textconv" | "--output"
        ) || argument.starts_with("--output=")
            || argument.starts_with('/')
            || *argument == ".."
            || argument.starts_with("../")
            || argument.ends_with("/..")
            || argument.contains("/../")
    })
}

fn safe_commit_message(message: &str) -> bool {
    !message.is_empty()
        && message.len() <= 120
        && message
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use consensus_core::state::{NextAction, RunFacts, RunState};
    use serde_json::json;
    use uuid::Uuid;

    use super::{ApprovalDecision, decide_command_approval, validate_test_command};

    const PRIMARY_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const REVIEWER_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn integration_approves_only_narrow_git_writes() {
        let state = integration_state();

        assert_eq!(
            decide_command_approval(
                &state,
                &json!({
                    "cwd": "/repo/primary",
                    "command": "cargo test --workspace",
                    "additionalPermissions": null,
                    "networkApprovalContext": null,
                    "proposedNetworkPolicyAmendments": null
                })
            ),
            ApprovalDecision::Cancel
        );
        assert_eq!(
            decide_command_approval(
                &state,
                &json!({
                    "cwd": "/repo/primary",
                    "command": format!("git switch -c consensus/test-run {PRIMARY_SHA}")
                })
            ),
            ApprovalDecision::Accept
        );
        assert_eq!(
            decide_command_approval(
                &state,
                &json!({
                    "cwd": "/repo/primary",
                    "command": format!("git merge --no-ff --no-edit {REVIEWER_SHA}")
                })
            ),
            ApprovalDecision::Accept
        );
        assert_eq!(
            decide_command_approval(
                &state,
                &json!({"cwd": "/repo/primary", "command": "git add -A"})
            ),
            ApprovalDecision::Accept
        );
        assert_eq!(
            decide_command_approval(
                &state,
                &json!({"cwd": "/repo/primary", "command": "git commit -m consensus-fix"})
            ),
            ApprovalDecision::Accept
        );
    }

    #[test]
    fn verification_approves_only_exact_frozen_tests_in_the_isolated_clone() {
        let mut state = integration_state();
        state.next_action = NextAction::RequestPrimaryVerification;
        state.verification_worktree = Some(PathBuf::from("/state/verification/run"));

        assert_eq!(
            decide_command_approval(
                &state,
                &json!({
                    "cwd": "/state/verification/run",
                    "command": "cargo test --workspace"
                })
            ),
            ApprovalDecision::Accept
        );
        for command in ["git status", "cargo test -p other"] {
            assert_eq!(
                decide_command_approval(
                    &state,
                    &json!({"cwd": "/state/verification/run", "command": command})
                ),
                ApprovalDecision::Cancel
            );
        }
    }

    #[test]
    fn publication_destructive_git_and_shell_chaining_are_cancelled() {
        let state = integration_state();
        for command in [
            "git push origin HEAD",
            "git reset --hard HEAD^",
            "git rebase main",
            "git clean -fdx",
            "git branch -D primary",
            "cargo test --workspace && git push origin HEAD",
            "sh -c git-status",
            "git diff '--output=/repo/primary/unexpected' HEAD",
            "git show --textconv HEAD",
            "git diff --no-index /etc/passwd README.md",
        ] {
            assert_eq!(
                decide_command_approval(
                    &state,
                    &json!({"cwd": "/repo/primary", "command": command})
                ),
                ApprovalDecision::Cancel,
                "{command} must fail closed"
            );
        }
    }

    #[test]
    fn wrong_directory_or_permission_escalation_is_cancelled() {
        let state = integration_state();
        assert_eq!(
            decide_command_approval(
                &state,
                &json!({"cwd": "/repo/reviewer", "command": "git add -A"})
            ),
            ApprovalDecision::Cancel
        );
        assert_eq!(
            decide_command_approval(
                &state,
                &json!({
                    "cwd": "/repo/primary",
                    "command": "cargo test --workspace",
                    "additionalPermissions": {"network": {"enabled": true}}
                })
            ),
            ApprovalDecision::Cancel
        );
        assert_eq!(
            decide_command_approval(
                &state,
                &json!({
                    "cwd": "/repo/primary",
                    "command": "cargo test --workspace",
                    "availableDecisions": ["decline", "cancel"]
                })
            ),
            ApprovalDecision::Cancel
        );
    }

    #[test]
    fn test_commands_cannot_hide_git_or_shell_control_operations() {
        for command in [
            "git -C . update-ref -d refs/heads/primary",
            "/usr/bin/git reset --hard HEAD^",
            "sh -c 'git reset --hard HEAD^'",
            "env MODE=test sh -c 'git reset --hard HEAD^'",
            "command sh -c 'git reset --hard HEAD^'",
        ] {
            assert!(!validate_test_command(command), "{command}");
        }

        let mut state = integration_state();
        state.required_test_commands = vec!["git -C . update-ref -d refs/heads/primary".into()];
        assert_eq!(
            decide_command_approval(
                &state,
                &json!({
                    "cwd": "/repo/primary",
                    "command": "git -C . update-ref -d refs/heads/primary"
                })
            ),
            ApprovalDecision::Cancel
        );
    }

    fn integration_state() -> RunState {
        let mut state = RunState::new(RunFacts {
            run_id: Uuid::nil(),
            primary_thread_id: "primary".into(),
            reviewer_thread_id: "reviewer".into(),
            primary_worktree: PathBuf::from("/repo/primary"),
            reviewer_worktree: PathBuf::from("/repo/reviewer"),
            git_common_dir: PathBuf::from("/repo/.git"),
            primary_sha: PRIMARY_SHA.into(),
            reviewer_sha: REVIEWER_SHA.into(),
            primary_ref: Some("refs/heads/primary".into()),
            reviewer_ref: Some("refs/heads/reviewer".into()),
        });
        state.target_integration_branch = Some("consensus/test-run".into());
        state.required_test_commands = vec!["cargo test --workspace".into()];
        state.next_action = NextAction::RequestPrimaryIntegration;
        state
    }
}
