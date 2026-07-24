use consensus_core::state::{NextAction, RunState};
use serde_json::Value;
use std::path::{Component, Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalDecision {
    Accept,
    Cancel,
}

pub(crate) fn decide_command_approval(state: &RunState, params: &Value) -> ApprovalDecision {
    if command_approval_denial(state, params).is_some() {
        ApprovalDecision::Cancel
    } else {
        ApprovalDecision::Accept
    }
}

pub(crate) fn command_approval_denial(state: &RunState, params: &Value) -> Option<&'static str> {
    let expected_cwd = match state.next_action {
        NextAction::RequestPrimaryIntegration => Some(&state.facts.primary_worktree),
        NextAction::RequestPrimaryVerification => state.verification_worktree.as_ref(),
        _ => None,
    };
    if params.get("cwd").and_then(Value::as_str) != expected_cwd.and_then(|path| path.to_str()) {
        return Some("command cwd does not match the bound execution worktree");
    }
    if params
        .get("approvalId")
        .is_some_and(|value| !value.is_null())
    {
        return Some("subcommand approval callbacks are outside the frozen command policy");
    }
    if params
        .get("environmentId")
        .is_some_and(|value| !value.is_null() && value.as_str() != Some("local"))
    {
        return Some("command targets a non-local execution environment");
    }
    // App Server may suggest an execpolicy amendment for an otherwise ordinary
    // one-time approval. Returning `accept` does not apply that suggestion, so
    // it is metadata rather than an additional permission request.
    if [
        "additionalPermissions",
        "networkApprovalContext",
        "proposedNetworkPolicyAmendments",
    ]
    .iter()
    .any(|field| params.get(*field).is_some_and(|value| !value.is_null()))
    {
        return Some("command requests additional filesystem or network permissions");
    }
    if params.get("availableDecisions").is_some_and(|value| {
        !value.is_null()
            && !value.as_array().is_some_and(|decisions| {
                decisions
                    .iter()
                    .any(|decision| decision.as_str() == Some("accept"))
            })
    }) {
        return Some("one-time command acceptance is unavailable");
    }
    let Some(command) = params
        .get("command")
        .and_then(Value::as_str)
        .and_then(normalize_app_server_command)
    else {
        return Some("command is absent or has an unsupported shell representation");
    };
    let command = command.trim();
    if command.is_empty() {
        return Some("command is empty");
    }
    if contains_shell_control(command) {
        return Some("command contains shell control syntax");
    }
    match state.next_action {
        NextAction::RequestPrimaryIntegration
            if is_allowed_git_command(state, command)
                || is_allowed_instruction_discovery(command) =>
        {
            None
        }
        NextAction::RequestPrimaryVerification
            if state
                .required_test_commands
                .iter()
                .any(|required| required.trim() == command && validate_test_command(required)) =>
        {
            None
        }
        _ => Some("command is outside the frozen integration or verification allowlist"),
    }
}

pub(crate) fn normalize_app_server_command(command: &str) -> Option<String> {
    if command.trim().is_empty() {
        return None;
    }
    let tokens = shell_words::split(command).ok()?;
    match tokens.as_slice() {
        [shell, flag, script]
            if matches!(
                executable_name(shell),
                "sh" | "bash" | "dash" | "zsh" | "fish"
            ) && matches!(flag.as_str(), "-c" | "-lc") =>
        {
            (!script.trim().is_empty()).then(|| script.clone())
        }
        _ => Some(command.to_owned()),
    }
}

pub(crate) fn is_retry_safe_read_only_integration_command(
    state: &RunState,
    cwd: &str,
    command: &str,
) -> bool {
    if state.facts.primary_worktree.to_str() != Some(cwd) {
        return false;
    }
    let Some(command) = normalize_app_server_command(command) else {
        return false;
    };
    let command = command.trim();
    !command.is_empty()
        && !contains_shell_control(command)
        && (is_allowed_read_only_git_command(state, command)
            || is_allowed_instruction_discovery(command)
            || is_retry_safe_no_index_diff(command)
            || is_retry_safe_stale_launcher_skill_read(command))
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
    if is_allowed_read_only_git_invocation(state, subcommand, rest) {
        return true;
    }
    let corrective_integration = state.integration_branch.is_some()
        || state.integration_sha.is_some()
        || state.current_integration_payload.is_some();
    match *subcommand {
        "switch" if corrective_integration => false,
        "switch" => {
            matches!(
                rest,
                ["-c", branch, sha]
                    if Some(*branch) == state.target_integration_branch.as_deref()
                        && *sha == state.facts.primary_sha
            )
        }
        "merge" if corrective_integration => false,
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

fn is_allowed_read_only_git_command(state: &RunState, command: &str) -> bool {
    let Ok(tokens) = shell_words::split(command) else {
        return false;
    };
    let tokens = tokens.iter().map(String::as_str).collect::<Vec<_>>();
    let ["git", subcommand, rest @ ..] = tokens.as_slice() else {
        return false;
    };
    is_allowed_read_only_git_invocation(state, subcommand, rest)
}

fn is_allowed_instruction_discovery(command: &str) -> bool {
    let Ok(tokens) = shell_words::split(command) else {
        return false;
    };
    matches!(
        tokens
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["rg", "--files", "-g", "AGENTS.md"]
    )
}

fn is_retry_safe_no_index_diff(command: &str) -> bool {
    let Ok(tokens) = shell_words::split(command) else {
        return false;
    };
    let ["git", "diff", "--no-index", "--", "/dev/null", path] =
        tokens.iter().map(String::as_str).collect::<Vec<_>>()[..]
    else {
        return false;
    };
    let path = Path::new(path);
    if path.as_os_str().is_empty() || path.is_absolute() || path.starts_with(".git") {
        return false;
    }
    path.components()
        .all(|component| matches!(component, Component::Normal(_)))
}

fn is_retry_safe_stale_launcher_skill_read(command: &str) -> bool {
    let Ok(tokens) = shell_words::split(command) else {
        return false;
    };
    let ["sed", "-n", "1,240p", path] = tokens.iter().map(String::as_str).collect::<Vec<_>>()[..]
    else {
        return false;
    };

    let path = Path::new(path);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return false;
    }
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let Some(value) = value.to_str() else {
                    return false;
                };
                components.push(value);
            }
            Component::RootDir | Component::Prefix(_) => {}
            Component::CurDir | Component::ParentDir => return false,
        }
    }
    if components.len() < 8 {
        return false;
    }
    let suffix = &components[components.len() - 8..];
    suffix[0] == "plugins"
        && suffix[1] == "cache"
        && suffix[2] == "worktree-merge-consensus"
        && suffix[3] == "worktree-merge-consensus"
        && semver::Version::parse(suffix[4]).is_ok()
        && suffix[5] == "skills"
        && suffix[6] == "worktree-merge-consensus"
        && suffix[7] == "SKILL.md"
}

fn is_allowed_read_only_git_invocation(
    state: &RunState,
    subcommand: &str,
    arguments: &[&str],
) -> bool {
    match subcommand {
        "status" | "diff" | "show" | "log" | "rev-parse" | "merge-base" | "ls-files" => {
            safe_read_only_git_arguments(arguments)
        }
        "branch" => safe_branch_arguments(state, arguments),
        "show-ref" => safe_show_ref_arguments(state, arguments),
        "symbolic-ref" => matches!(arguments, ["--short", "HEAD"]),
        _ => false,
    }
}

fn safe_branch_arguments(state: &RunState, arguments: &[&str]) -> bool {
    if matches!(arguments, ["--show-current"]) {
        return state.target_integration_branch.is_some();
    }
    let Some(branch) = state.target_integration_branch.as_deref() else {
        return false;
    };
    matches!(arguments, ["--list", candidate] if *candidate == branch)
}

fn safe_show_ref_arguments(state: &RunState, arguments: &[&str]) -> bool {
    let Some(branch) = state.target_integration_branch.as_deref() else {
        return false;
    };
    let target_ref = format!("refs/heads/{branch}");
    matches!(arguments, ["--verify", reference] if *reference == target_ref)
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

    use super::{
        ApprovalDecision, decide_command_approval, is_retry_safe_read_only_integration_command,
        normalize_app_server_command, validate_test_command,
    };

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
                &json!({
                    "cwd": "/repo/primary",
                    "command": "git rev-parse HEAD",
                    "proposedExecpolicyAmendment": ["git", "rev-parse", "HEAD"],
                    "availableDecisions": ["accept", "acceptForSession", "decline", "cancel"]
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
    fn corrective_integration_denies_branch_and_merge_but_allows_patch_completion() {
        let mut state = integration_state();
        state.integration_branch = Some("consensus/test-run".into());
        state.integration_sha = Some("cccccccccccccccccccccccccccccccccccccccc".into());
        state.current_integration_payload = Some(json!({
            "changed_files": ["combined.txt"],
            "integration_evidence": {"summary": "frozen failed result"}
        }));

        for command in [
            format!("git switch -c consensus/test-run {PRIMARY_SHA}"),
            format!("git merge --no-ff --no-edit {REVIEWER_SHA}"),
        ] {
            assert_eq!(
                decide_command_approval(
                    &state,
                    &json!({"cwd": "/repo/primary", "command": command})
                ),
                ApprovalDecision::Cancel,
                "{command} must be denied for a corrective integration"
            );
        }
        for command in [
            "git status --short",
            "git rev-parse HEAD",
            "git add -A",
            "git commit -m consensus-fix",
        ] {
            assert_eq!(
                decide_command_approval(
                    &state,
                    &json!({"cwd": "/repo/primary", "command": command})
                ),
                ApprovalDecision::Accept,
                "{command} must remain available for exact corrective patch completion or inspection"
            );
        }
    }

    #[test]
    fn one_known_app_server_shell_wrapper_is_normalized_before_policy_checks() {
        let state = integration_state();
        let wrapped = "/bin/bash -lc 'git rev-parse HEAD'";

        assert_eq!(
            normalize_app_server_command(wrapped).as_deref(),
            Some("git rev-parse HEAD")
        );
        assert_eq!(
            decide_command_approval(
                &state,
                &json!({
                    "approvalId": null,
                    "environmentId": "local",
                    "cwd": "/repo/primary",
                    "command": wrapped,
                    "proposedExecpolicyAmendment": ["git", "rev-parse", "HEAD"],
                    "availableDecisions": ["accept", "acceptForSession", "decline", "cancel"]
                })
            ),
            ApprovalDecision::Accept
        );

        for command in [
            "/bin/bash -lc 'git rev-parse HEAD && git push origin HEAD'",
            "/bin/bash -lc 'sh -c \"git rev-parse HEAD\"'",
            "/bin/bash -lc '/bin/bash -lc \"git rev-parse HEAD\"'",
            "/bin/ksh -lc 'git rev-parse HEAD'",
        ] {
            assert_eq!(
                decide_command_approval(
                    &state,
                    &json!({
                        "approvalId": null,
                        "environmentId": "local",
                        "cwd": "/repo/primary",
                        "command": command,
                        "availableDecisions": ["accept"]
                    })
                ),
                ApprovalDecision::Cancel,
                "{command} must fail closed"
            );
        }
    }

    #[test]
    fn target_branch_queries_are_limited_to_the_frozen_target_branch() {
        let state = integration_state();
        let target = "refs/heads/consensus/test-run";
        let wrapped = format!("/bin/bash -lc 'git show-ref --verify {target}'");

        assert_eq!(
            decide_command_approval(
                &state,
                &json!({
                    "approvalId": null,
                    "environmentId": "local",
                    "cwd": "/repo/primary",
                    "command": wrapped,
                    "availableDecisions": ["accept"]
                })
            ),
            ApprovalDecision::Accept
        );
        assert!(is_retry_safe_read_only_integration_command(
            &state,
            "/repo/primary",
            &format!("/bin/bash -lc 'git show-ref --verify {target}'")
        ));
        assert_eq!(
            decide_command_approval(
                &state,
                &json!({
                    "approvalId": null,
                    "environmentId": "local",
                    "cwd": "/repo/primary",
                    "command": "/bin/bash -lc 'git branch --list consensus/test-run'",
                    "availableDecisions": ["accept"]
                })
            ),
            ApprovalDecision::Accept
        );
        assert!(is_retry_safe_read_only_integration_command(
            &state,
            "/repo/primary",
            "/bin/bash -lc 'git branch --list consensus/test-run'"
        ));
        assert!(!is_retry_safe_read_only_integration_command(
            &state,
            "/repo/reviewer",
            &format!("/bin/bash -lc 'git show-ref --verify {target}'")
        ));
        for command in [
            "git show-ref --verify refs/heads/primary",
            "git show-ref --verify --quiet refs/heads/consensus/test-run",
            "git show-ref --exclude-existing=refs/heads/consensus/test-run",
            "git branch --list primary",
            "git branch --list",
            "git branch --list consensus/test-run primary",
            "git branch -a --list consensus/test-run",
            "git branch --contains consensus/test-run",
            "git branch --list consensus/*",
            "git branch consensus/test-run aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "/bin/bash -lc \"bash -lc 'git show-ref --verify refs/heads/consensus/test-run'\"",
            "git switch -c consensus/test-run aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(!is_retry_safe_read_only_integration_command(
                &state,
                "/repo/primary",
                command
            ));
        }
    }

    #[test]
    fn current_branch_queries_are_exactly_scoped_and_read_only() {
        let state = integration_state();

        for command in [
            "git symbolic-ref --short HEAD",
            "/bin/bash -lc 'git symbolic-ref --short HEAD'",
            "git branch --show-current",
            "/bin/bash -lc 'git branch --show-current'",
        ] {
            assert_eq!(
                decide_command_approval(
                    &state,
                    &json!({
                        "cwd": "/repo/primary",
                        "command": command,
                        "availableDecisions": ["accept"]
                    })
                ),
                ApprovalDecision::Accept
            );
            assert!(is_retry_safe_read_only_integration_command(
                &state,
                "/repo/primary",
                command
            ));
        }
        assert!(!is_retry_safe_read_only_integration_command(
            &state,
            "/repo/reviewer",
            "git symbolic-ref --short HEAD"
        ));
        assert!(!is_retry_safe_read_only_integration_command(
            &state,
            "/repo/reviewer",
            "git branch --show-current"
        ));
        let mut unbound = state.clone();
        unbound.target_integration_branch = None;
        assert_eq!(
            decide_command_approval(
                &unbound,
                &json!({"cwd": "/repo/primary", "command": "git branch --show-current"})
            ),
            ApprovalDecision::Cancel
        );
        for command in [
            "git symbolic-ref HEAD",
            "git symbolic-ref --quiet --short HEAD",
            "git symbolic-ref --short refs/heads/primary",
            "git symbolic-ref HEAD refs/heads/primary",
            "git symbolic-ref --short HEAD refs/heads/primary",
            "git symbolic-ref -d HEAD",
            "git symbolic-ref --delete HEAD",
            "git branch --show-current HEAD",
            "git branch --show-current=HEAD",
            "git branch -a --show-current",
        ] {
            assert_eq!(
                decide_command_approval(
                    &state,
                    &json!({"cwd": "/repo/primary", "command": command})
                ),
                ApprovalDecision::Cancel,
                "{command} must fail closed"
            );
            assert!(
                !is_retry_safe_read_only_integration_command(&state, "/repo/primary", command),
                "{command} must not be recovery-safe"
            );
        }
    }

    #[test]
    fn instruction_discovery_is_exactly_scoped_and_read_only() {
        let state = integration_state();

        for command in [
            "rg --files -g AGENTS.md",
            "/bin/bash -lc 'rg --files -g AGENTS.md'",
        ] {
            assert_eq!(
                decide_command_approval(
                    &state,
                    &json!({
                        "cwd": "/repo/primary",
                        "command": command,
                        "availableDecisions": ["accept"]
                    })
                ),
                ApprovalDecision::Accept
            );
            assert!(is_retry_safe_read_only_integration_command(
                &state,
                "/repo/primary",
                command
            ));
        }

        assert_eq!(
            decide_command_approval(
                &state,
                &json!({
                    "cwd": "/repo/reviewer",
                    "command": "rg --files -g AGENTS.md"
                })
            ),
            ApprovalDecision::Cancel
        );
        for command in [
            "rg --files",
            "rg --files -g '*.md'",
            "rg --files -g AGENTS.md /tmp",
            "rg --files --hidden -g AGENTS.md",
            "find . -name AGENTS.md",
            "rg --files -g AGENTS.md && git status",
        ] {
            assert_eq!(
                decide_command_approval(
                    &state,
                    &json!({"cwd": "/repo/primary", "command": command})
                ),
                ApprovalDecision::Cancel,
                "{command} must fail closed"
            );
            assert!(!is_retry_safe_read_only_integration_command(
                &state,
                "/repo/primary",
                command
            ));
        }
    }

    #[test]
    fn retry_safe_no_index_diff_is_exactly_scoped_and_never_live_approved() {
        let state = integration_state();
        let command = "/bin/bash -lc 'git diff --no-index -- /dev/null tests/cli.rs'";

        assert!(is_retry_safe_read_only_integration_command(
            &state,
            "/repo/primary",
            command
        ));
        assert_eq!(
            decide_command_approval(&state, &json!({"cwd": "/repo/primary", "command": command})),
            ApprovalDecision::Cancel
        );

        for unsafe_command in [
            "git diff --no-index -- /dev/null /tmp/cli.rs",
            "git diff --no-index -- /dev/null ../cli.rs",
            "git diff --no-index -- /etc/passwd tests/cli.rs",
            "git diff --no-index /dev/null tests/cli.rs",
            "git diff --no-index -- /dev/null tests/cli.rs --output=copy",
            "git diff --no-index -- /dev/null tests/cli.rs && git status",
        ] {
            assert!(
                !is_retry_safe_read_only_integration_command(
                    &state,
                    "/repo/primary",
                    unsafe_command
                ),
                "{unsafe_command} must fail closed"
            );
        }
    }

    #[test]
    fn stale_launcher_skill_reads_are_retry_only_and_exactly_scoped() {
        let state = integration_state();
        let stale_skill = "/opt/codex-home/plugins/cache/worktree-merge-consensus/\
worktree-merge-consensus/0.1.11/skills/worktree-merge-consensus/SKILL.md";
        let command = format!("sed -n '1,240p' {stale_skill}");

        assert!(is_retry_safe_read_only_integration_command(
            &state,
            "/repo/primary",
            &command
        ));
        assert_eq!(
            decide_command_approval(&state, &json!({"cwd": "/repo/primary", "command": command})),
            ApprovalDecision::Cancel,
            "the stale read may be discarded during recovery but must never enter the execution allowlist"
        );

        for command in [
            format!("sed -n '1,241p' {stale_skill}"),
            format!("sed -n '1,240p' {stale_skill}.bak"),
            "sed -n '1,240p' /etc/passwd".into(),
            "sed -n '1,240p' plugins/cache/worktree-merge-consensus/worktree-merge-consensus/0.1.11/skills/worktree-merge-consensus/SKILL.md".into(),
            "sed -n '1,240p' /opt/codex-home/plugins/cache/other/worktree-merge-consensus/0.1.11/skills/worktree-merge-consensus/SKILL.md".into(),
            "sed -n '1,240p' /opt/codex-home/plugins/cache/worktree-merge-consensus/worktree-merge-consensus/not-semver/skills/worktree-merge-consensus/SKILL.md".into(),
            format!("sed -n '1,240p' {stale_skill} && git status"),
        ] {
            assert!(
                !is_retry_safe_read_only_integration_command(
                    &state,
                    "/repo/primary",
                    &command
                ),
                "{command} must fail closed"
            );
        }
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
                    "approvalId": null,
                    "environmentId": "local",
                    "cwd": "/state/verification/run",
                    "command": "/bin/bash -lc 'cargo test --workspace'"
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
        assert_eq!(
            decide_command_approval(
                &state,
                &json!({
                    "approvalId": "subcommand-callback",
                    "environmentId": "local",
                    "cwd": "/repo/primary",
                    "command": "/bin/bash -lc 'git rev-parse HEAD'"
                })
            ),
            ApprovalDecision::Cancel
        );
        assert_eq!(
            decide_command_approval(
                &state,
                &json!({
                    "approvalId": null,
                    "environmentId": "remote-container",
                    "cwd": "/repo/primary",
                    "command": "/bin/bash -lc 'git rev-parse HEAD'"
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
