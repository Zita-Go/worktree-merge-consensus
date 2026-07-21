use std::{
    collections::HashMap,
    env,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Output},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tungstenite::{Message, accept};

#[derive(Debug, Deserialize)]
struct Config {
    scenario: String,
    primary_thread: String,
    reviewer_thread: String,
    primary_thread_cwd: PathBuf,
    reviewer_thread_cwd: PathBuf,
    primary_worktree: PathBuf,
    reviewer_worktree: PathBuf,
    git_common_dir: PathBuf,
    integration_branch: String,
    state_directory: PathBuf,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fake App Server failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    match arguments.as_slice() {
        [flag] if flag == "--version" => {
            println!("codex-cli 0.144.5");
            Ok(())
        }
        [app, daemon, start] if app == "app-server" && daemon == "daemon" && start == "start" => {
            Ok(())
        }
        [app, proxy, ..] if app == "app-server" && proxy == "proxy" => serve_proxy(),
        _ => Err(format!("unsupported arguments: {arguments:?}")),
    }
}

fn serve_proxy() -> Result<(), String> {
    let config_path = env::var_os("CONSENSUS_FAKE_CONFIG")
        .map(PathBuf::from)
        .ok_or_else(|| "CONSENSUS_FAKE_CONFIG is not set".to_owned())?;
    let config: Config = serde_json::from_slice(
        &fs::read(&config_path).map_err(|error| format!("read config: {error}"))?,
    )
    .map_err(|error| format!("parse config: {error}"))?;
    fs::create_dir_all(&config.state_directory)
        .map_err(|error| format!("create fake state: {error}"))?;
    append_event(&config, "proxy started")?;

    let mut websocket =
        accept(StdioStream).map_err(|error| format!("accept websocket upgrade: {error}"))?;
    loop {
        let message = match websocket.read() {
            Ok(message) => message,
            Err(tungstenite::Error::ConnectionClosed) => break,
            Err(error) => return Err(format!("read websocket request: {error}")),
        };
        let Message::Text(line) = message else {
            continue;
        };
        let request: Value =
            serde_json::from_str(&line).map_err(|error| format!("parse request: {error}"))?;
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| "request method is missing".to_owned())?;
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        let result = handle_request(&config, method, &params)?;
        let response = json!({"jsonrpc": "2.0", "id": id, "result": result});
        websocket
            .send(Message::Text(
                serde_json::to_string(&response).map_err(|error| error.to_string())?,
            ))
            .map_err(|error| format!("write response: {error}"))?;
        for notification in emit_post_response(&config, method, &result)? {
            websocket
                .send(Message::Text(
                    serde_json::to_string(&notification).map_err(|error| error.to_string())?,
                ))
                .map_err(|error| format!("write notification: {error}"))?;
        }
    }
    Ok(())
}

struct StdioStream;

impl Read for StdioStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        io::stdin().read(buffer)
    }
}

impl Write for StdioStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        io::stdout().write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stdout().flush()
    }
}

fn handle_request(config: &Config, method: &str, params: &Value) -> Result<Value, String> {
    match method {
        "initialize" => {
            if params
                .pointer("/capabilities/experimentalApi")
                .and_then(Value::as_bool)
                != Some(true)
            {
                return Err(
                    "initialize must opt into experimentalApi before using turn/start.environments"
                        .to_owned(),
                );
            }
            Ok(json!({
                "codexHome": config.state_directory.join("codex-home"),
                "platformFamily": "unix",
                "platformOs": "linux",
                "userAgent": "codex-cli/0.144.5"
            }))
        }
        "thread/list" => Ok(json!({
            "data": [thread_summary(config, &config.primary_thread), thread_summary(config, &config.reviewer_thread)],
            "nextCursor": null,
            "backwardsCursor": null
        })),
        "thread/read" => {
            let thread_id = params
                .get("threadId")
                .and_then(Value::as_str)
                .ok_or_else(|| "threadId is missing".to_owned())?;
            Ok(json!({"thread": thread_detail(config, thread_id)?}))
        }
        "thread/resume" => {
            let thread_id = params
                .get("threadId")
                .and_then(Value::as_str)
                .ok_or_else(|| "threadId is missing".to_owned())?;
            mark_thread_resumed(config, thread_id)?;
            Ok(json!({"thread": thread_detail(config, thread_id)?}))
        }
        "turn/start" => start_turn(config, params),
        _ => Err(format!("unsupported method {method}")),
    }
}

fn thread_summary(config: &Config, thread_id: &str) -> Value {
    let cwd = if thread_id == config.primary_thread {
        &config.primary_thread_cwd
    } else {
        &config.reviewer_thread_cwd
    };
    json!({
        "id": thread_id,
        "cwd": cwd,
        "name": thread_id,
        "preview": "process-level fixture",
        "cliVersion": "0.144.5",
        "createdAt": 1,
        "updatedAt": 1,
        "status": {"type": "idle"},
        "source": "fakeAppServer"
    })
}

fn thread_detail(config: &Config, thread_id: &str) -> Result<Value, String> {
    complete_deferred_turns(config)?;
    let turns = load_turns(config, thread_id)?;
    let mut summary = thread_summary(config, thread_id);
    if turns
        .iter()
        .any(|turn| turn.get("status").and_then(Value::as_str) == Some("inProgress"))
    {
        summary["status"] = json!({"type": "active", "activeFlags": []});
    }
    summary["turns"] = Value::Array(turns);
    Ok(summary)
}

fn start_turn(config: &Config, params: &Value) -> Result<Value, String> {
    let thread_id = params
        .get("threadId")
        .and_then(Value::as_str)
        .ok_or_else(|| "turn/start threadId is missing".to_owned())?;
    consume_thread_resume(config, thread_id)?;
    let prompt = params
        .get("input")
        .and_then(Value::as_array)
        .and_then(|input| input.first())
        .and_then(|input| input.get("text"))
        .and_then(Value::as_str)
        .ok_or_else(|| "turn/start prompt is missing".to_owned())?;
    let metadata = prompt_json_block(prompt, "Authoritative turn metadata:")?;
    let payload = prompt_json_block(prompt, "Complete current payload")?;
    let action = metadata
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| "prompt action is missing".to_owned())?;
    if let Err(error) = validate_turn_policy(config, action, thread_id, params, &payload) {
        append_event(
            config,
            &format!("turn-policy-error {error}; params={params}"),
        )?;
        return Err(error);
    }
    append_event(config, &format!("turn {thread_id} {action}"))?;
    let occurrence = action_count(config, action)?;

    let turn_id = format!("turn-{}", turn_count(config)? + 1);
    let verification = if action == "REQUEST_PRIMARY_VERIFICATION" {
        Some(run_verification(config, &payload, &turn_id)?)
    } else {
        None
    };
    let reply = scripted_reply(
        config,
        action,
        occurrence,
        &metadata,
        &payload,
        verification.as_ref(),
    )?;
    let deferred = deferred_marker(config, action);
    let mut turn = if deferred.is_some() {
        let pending = PendingTurn {
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.clone(),
            prompt: prompt.to_owned(),
            reply: reply.clone(),
            command_items: verification
                .as_ref()
                .map(|result| result.command_items.clone())
                .unwrap_or_default(),
        };
        fs::write(
            pending_path(config, &turn_id),
            serde_json::to_vec(&pending).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("write pending turn: {error}"))?;
        in_progress_turn(&turn_id, prompt)
    } else {
        completed_turn(&turn_id, prompt, &reply)
    };
    if deferred.is_none() {
        if let Some(verification) = verification {
            append_command_items(&mut turn, verification.command_items);
        }
    }
    append_turn(config, thread_id, &turn)?;
    Ok(json!({
        "turn": {
            "id": turn_id,
            "status": if deferred.is_some() { "inProgress" } else { "completed" },
            "items": []
        }
    }))
}

fn resume_marker(config: &Config, thread_id: &str) -> PathBuf {
    config.state_directory.join(format!("resumed-{thread_id}"))
}

fn mark_thread_resumed(config: &Config, thread_id: &str) -> Result<(), String> {
    fs::write(resume_marker(config, thread_id), b"ready\n")
        .map_err(|error| format!("mark resumed task {thread_id}: {error}"))?;
    append_event(config, &format!("resume {thread_id}"))
}

fn consume_thread_resume(config: &Config, thread_id: &str) -> Result<(), String> {
    let marker = resume_marker(config, thread_id);
    if !marker.exists() {
        return Err(format!(
            "task {thread_id} must be resumed before turn/start"
        ));
    }
    fs::remove_file(marker).map_err(|error| format!("consume resumed task {thread_id}: {error}"))
}

fn declared_tests(metadata: &Value) -> Vec<String> {
    let mut commands = metadata
        .get("required_test_commands")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if commands.is_empty() {
        commands.push("test -d .".to_owned());
    }
    commands
}

fn validate_turn_policy(
    config: &Config,
    action: &str,
    thread_id: &str,
    params: &Value,
    current: &Value,
) -> Result<(), String> {
    let integration = action == "REQUEST_PRIMARY_INTEGRATION";
    let verification = action == "REQUEST_PRIMARY_VERIFICATION";
    let primary_action = matches!(
        action,
        "REQUEST_PRIMARY_CONTRACT"
            | "REQUEST_PRIMARY_PLAN"
            | "REQUEST_PRIMARY_INTEGRATION"
            | "REQUEST_PRIMARY_VERIFICATION"
    );
    let expected_thread = if primary_action {
        &config.primary_thread
    } else {
        &config.reviewer_thread
    };
    let expected_cwd = if verification {
        PathBuf::from(
            current
                .get("verification_worktree")
                .and_then(Value::as_str)
                .ok_or_else(|| "verification_worktree is missing".to_owned())?,
        )
    } else if primary_action {
        config.primary_worktree.clone()
    } else {
        config.reviewer_worktree.clone()
    };
    let expected_cwd = fs::canonicalize(&expected_cwd)
        .map_err(|error| format!("canonicalize expected task cwd: {error}"))?;
    if thread_id != expected_thread {
        return Err(format!(
            "turn/start thread mismatch: {thread_id} != {expected_thread}"
        ));
    }
    if params.get("cwd") != Some(&json!(expected_cwd)) {
        return Err(format!(
            "turn/start cwd mismatch: {:?} != {}",
            params.get("cwd"),
            expected_cwd.display()
        ));
    }
    if params.get("runtimeWorkspaceRoots") != Some(&json!([expected_cwd])) {
        return Err(format!(
            "turn/start workspace roots mismatch: {:?} != {}",
            params.get("runtimeWorkspaceRoots"),
            expected_cwd.display()
        ));
    }
    if params.get("approvalsReviewer") != Some(&json!("user")) {
        return Err("turn/start approvalsReviewer is not user".to_owned());
    }
    if params.get("environments")
        != Some(&json!([{
            "environmentId": "local",
            "cwd": expected_cwd
        }]))
    {
        return Err("turn/start local execution environment is not pinned".to_owned());
    }
    let expected_approval = if integration || verification {
        "untrusted"
    } else {
        "never"
    };
    if params.get("approvalPolicy") != Some(&json!(expected_approval)) {
        return Err("turn/start approval policy is not fail-closed".to_owned());
    }
    let expected_sandbox = if integration {
        json!({
            "type": "workspaceWrite",
            "writableRoots": [expected_cwd, config.git_common_dir],
            "networkAccess": false,
            "excludeSlashTmp": true,
            "excludeTmpdirEnvVar": true
        })
    } else if verification {
        json!({
            "type": "workspaceWrite",
            "writableRoots": [expected_cwd],
            "networkAccess": false,
            "excludeSlashTmp": false,
            "excludeTmpdirEnvVar": false
        })
    } else {
        json!({"type": "readOnly", "networkAccess": false})
    };
    if params.get("sandboxPolicy") != Some(&expected_sandbox) {
        return Err("turn/start sandbox policy is not pinned".to_owned());
    }
    Ok(())
}

fn scripted_reply(
    config: &Config,
    action: &str,
    occurrence: usize,
    metadata: &Value,
    current: &Value,
    verification: Option<&VerificationResult>,
) -> Result<Value, String> {
    if config.scenario == "invalid_reply" && occurrence == 1 {
        return Ok(json!("not a protocol envelope"));
    }
    if action == "REQUEST_REVIEWER_PLAN_VERDICT" {
        let issue = match config.scenario.as_str() {
            "plan_revision" if occurrence == 1 => Some("missing-reviewer-edge".to_owned()),
            "no_progress" => Some("same-plan-gap".to_owned()),
            "round_limit" => Some(format!("round-gap-{occurrence}")),
            _ => None,
        };
        if let Some(issue) = issue {
            return Ok(changes_required_message(metadata, &issue));
        }
    }
    if action == "REQUEST_REVIEWER_RESULT_VERDICT"
        && config.scenario == "result_revision"
        && occurrence == 1
    {
        return Ok(changes_required_message(metadata, "missing-result-edge"));
    }
    let (message_type, payload, integration_branch, integration_sha) = match action {
        "REQUEST_PRIMARY_CONTRACT" => (
            "CONTRACT_READY",
            json!({
                "role": "PRIMARY",
                "contract": {
                    "items": ["primary-feature"],
                    "tests": declared_tests(metadata)
                }
            }),
            None,
            None,
        ),
        "REQUEST_REVIEWER_CONTRACT" => (
            "CONTRACT_READY",
            json!({
                "role": "REVIEWER",
                "contract": {
                    "items": ["reviewer-feature"],
                    "tests": declared_tests(metadata)
                }
            }),
            None,
            None,
        ),
        "REQUEST_PRIMARY_PLAN" => {
            let step = match config.scenario.as_str() {
                "no_progress" => "unchanged plan".to_owned(),
                "round_limit" => format!("round plan {occurrence}"),
                "plan_revision" if occurrence > 1 => "preserve reviewer edge".to_owned(),
                _ => "merge both frozen commits".to_owned(),
            };
            (
                "PLAN_READY",
                json!({
                    "primary_contract": current["primary_contract"],
                    "reviewer_contract": current["reviewer_contract"],
                    "plan": {"steps": [step]},
                    "coverage_matrix": [
                        {"item": "primary-feature", "covered_by": step},
                        {"item": "reviewer-feature", "covered_by": step}
                    ],
                    "test_commands": declared_tests(metadata)
                }),
                None,
                None,
            )
        }
        "REQUEST_REVIEWER_PLAN_VERDICT" => (
            "APPROVED_PLAN",
            json!({
                "approved_plan_revision": metadata["plan_revision"],
                "approved_primary_sha": metadata["primary_sha"],
                "approved_reviewer_sha": metadata["reviewer_sha"],
                "approved_plan_hash": current["plan_hash"],
                "uncovered_items": []
            }),
            None,
            None,
        ),
        "REQUEST_PRIMARY_INTEGRATION" => {
            let integration = integrate(config, metadata, occurrence)?;
            (
                "INTEGRATION_READY",
                json!({
                    "changed_files": integration.changed_files,
                    "integration_evidence": {"summary": "both frozen commits integrated"}
                }),
                Some(config.integration_branch.clone()),
                Some(integration.sha),
            )
        }
        "REQUEST_PRIMARY_VERIFICATION" => {
            let verification =
                verification.ok_or_else(|| "verification execution is missing".to_owned())?;
            (
                "INTEGRATION_READY",
                json!({
                    "changed_files": current["changed_files"],
                    "integration_evidence": current["integration_evidence"],
                    "test_evidence": verification.reported_evidence
                }),
                metadata["integration_branch"].as_str().map(str::to_owned),
                metadata["integration_sha"].as_str().map(str::to_owned),
            )
        }
        "REQUEST_REVIEWER_RESULT_VERDICT" => (
            "APPROVED_RESULT",
            json!({
                "approved_plan_revision": metadata["plan_revision"],
                "approved_primary_sha": metadata["primary_sha"],
                "approved_reviewer_sha": metadata["reviewer_sha"],
                "approved_integration_branch": metadata["integration_branch"],
                "approved_integration_sha": metadata["integration_sha"],
                "uncovered_items": []
            }),
            metadata["integration_branch"].as_str().map(str::to_owned),
            metadata["integration_sha"].as_str().map(str::to_owned),
        ),
        _ => return Err(format!("unsupported action {action}")),
    };
    Ok(protocol_message(
        metadata,
        message_type,
        payload,
        integration_branch,
        integration_sha,
        None,
    ))
}

fn changes_required_message(metadata: &Value, issue_id: &str) -> Value {
    protocol_message(
        metadata,
        "CHANGES_REQUIRED",
        json!({
            "issue_ids": [issue_id],
            "evidence": [{"issue_id": issue_id, "detail": "fixture requires another revision"}]
        }),
        metadata["integration_branch"].as_str().map(str::to_owned),
        metadata["integration_sha"].as_str().map(str::to_owned),
        Some("COVERAGE_GAP"),
    )
}

fn protocol_message(
    metadata: &Value,
    message_type: &str,
    payload: Value,
    integration_branch: Option<String>,
    integration_sha: Option<String>,
    reason_code: Option<&str>,
) -> Value {
    json!({
        "protocol": "worktree-merge-consensus/v1",
        "run_id": metadata["run_id"],
        "message_type": message_type,
        "phase": metadata["phase"],
        "round": metadata["round"],
        "primary_sha": metadata["primary_sha"],
        "reviewer_sha": metadata["reviewer_sha"],
        "plan_revision": metadata["plan_revision"],
        "integration_branch": integration_branch,
        "integration_sha": integration_sha,
        "reason_code": reason_code,
        "payload": payload
    })
}

struct IntegrationResult {
    sha: String,
    changed_files: Vec<String>,
}

fn integrate(
    config: &Config,
    metadata: &Value,
    occurrence: usize,
) -> Result<IntegrationResult, String> {
    let primary_sha = required_string(metadata, "primary_sha")?;
    let reviewer_sha = required_string(metadata, "reviewer_sha")?;
    if config.scenario == "result_revision" && occurrence > 1 {
        fs::write(
            config.primary_worktree.join("reviewer-edge.txt"),
            "reviewer edge restored\n",
        )
        .map_err(|error| format!("write result revision: {error}"))?;
        run_git(config, &["add", "reviewer-edge.txt"])?;
        run_git(config, &["commit", "-m", "Address result review"])?;
    } else {
        run_git(
            config,
            &["switch", "-c", &config.integration_branch, primary_sha],
        )?;
        let merge = run_git_allow_status(
            config,
            &[
                "merge",
                "--no-ff",
                reviewer_sha,
                "-m",
                "Integrate reviewer worktree",
            ],
        )?;
        if !merge.status.success() {
            if config.scenario != "conflict_resolution" {
                return Err(format!(
                    "git merge failed: {}",
                    String::from_utf8_lossy(&merge.stderr)
                ));
            }
            fs::write(
                config.primary_worktree.join("shared.txt"),
                "primary decision\nreviewer decision\n",
            )
            .map_err(|error| format!("resolve shared.txt: {error}"))?;
            run_git(config, &["add", "."])?;
            run_git(config, &["commit", "-m", "Resolve integration conflict"])?;
        }
    }

    let sha = git_text(&config.primary_worktree, &["rev-parse", "HEAD"])?;
    append_event(config, &format!("integration-sha {sha}"))?;
    let changed_files = git_text(
        &config.primary_worktree,
        &["diff", "--name-only", primary_sha, &sha, "--"],
    )?
    .lines()
    .map(str::to_owned)
    .collect();
    Ok(IntegrationResult { sha, changed_files })
}

struct VerificationResult {
    reported_evidence: Vec<Value>,
    command_items: Vec<Value>,
}

fn run_verification(
    config: &Config,
    current: &Value,
    turn_id: &str,
) -> Result<VerificationResult, String> {
    let cwd = current
        .get("verification_worktree")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| "verification_worktree is missing".to_owned())?;
    let cwd = fs::canonicalize(cwd)
        .map_err(|error| format!("canonicalize verification worktree: {error}"))?;
    let commands = current
        .get("required_test_commands")
        .and_then(Value::as_array)
        .ok_or_else(|| "required_test_commands is missing".to_owned())?;
    let mut reported_evidence = Vec::with_capacity(commands.len());
    let mut command_items = Vec::with_capacity(commands.len());
    for (index, command) in commands.iter().enumerate() {
        let command = command
            .as_str()
            .ok_or_else(|| "test command is not a string".to_owned())?;
        append_event(
            config,
            &format!("verification-test {} {command}", cwd.display()),
        )?;
        let status = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&cwd)
            .status()
            .map_err(|error| format!("run test {command}: {error}"))?;
        let exit_code = status.code().unwrap_or(1);
        reported_evidence.push(json!({
            "command": command,
            "exit_code": exit_code
        }));
        command_items.push(json!({
            "id": format!("{turn_id}-command-{}", index + 1),
            "type": "commandExecution",
            "command": command,
            "commandActions": [],
            "cwd": cwd,
            "status": "completed",
            "exitCode": exit_code,
            "source": "agent"
        }));
    }
    Ok(VerificationResult {
        reported_evidence,
        command_items,
    })
}

fn prompt_json_block(prompt: &str, marker: &str) -> Result<Value, String> {
    let marker_start = prompt
        .find(marker)
        .ok_or_else(|| format!("prompt marker {marker:?} is missing"))?;
    let after_marker = &prompt[marker_start + marker.len()..];
    let fence_start = after_marker
        .find("```json\n")
        .ok_or_else(|| format!("JSON fence after {marker:?} is missing"))?;
    let json_start = fence_start + "```json\n".len();
    let fenced = &after_marker[json_start..];
    let json_end = fenced
        .find("\n```")
        .ok_or_else(|| format!("JSON fence after {marker:?} is unterminated"))?;
    serde_json::from_str(&fenced[..json_end]).map_err(|error| format!("parse prompt JSON: {error}"))
}

#[derive(Debug, Serialize, Deserialize)]
struct PendingTurn {
    thread_id: String,
    turn_id: String,
    prompt: String,
    reply: Value,
    #[serde(default)]
    command_items: Vec<Value>,
}

fn deferred_marker(config: &Config, action: &str) -> Option<&'static str> {
    match (config.scenario.as_str(), action) {
        ("source_drift" | "cancellation", "REQUEST_PRIMARY_CONTRACT") => Some("complete"),
        ("user_input_pause", "REQUEST_PRIMARY_INTEGRATION") => Some("approve"),
        ("crash_restart", "REQUEST_PRIMARY_INTEGRATION") => Some("complete"),
        ("crash_verification", "REQUEST_PRIMARY_VERIFICATION") => Some("complete"),
        _ => None,
    }
}

fn complete_deferred_turns(config: &Config) -> Result<(), String> {
    let marker = match config.scenario.as_str() {
        "user_input_pause" => "approve",
        "source_drift" | "crash_restart" | "crash_verification" => "complete",
        _ => return Ok(()),
    };
    if !config.state_directory.join(marker).exists() {
        return Ok(());
    }
    let entries = fs::read_dir(&config.state_directory)
        .map_err(|error| format!("read fake state: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("read pending entry: {error}"))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("pending-") || !name.ends_with(".json") {
            continue;
        }
        let pending: PendingTurn = serde_json::from_slice(
            &fs::read(entry.path()).map_err(|error| format!("read pending turn: {error}"))?,
        )
        .map_err(|error| format!("parse pending turn: {error}"))?;
        let mut turn = completed_turn(&pending.turn_id, &pending.prompt, &pending.reply);
        if !pending.command_items.is_empty() {
            append_command_items(&mut turn, pending.command_items);
        }
        append_turn(config, &pending.thread_id, &turn)?;
        fs::remove_file(entry.path()).map_err(|error| format!("remove pending turn: {error}"))?;
        append_event(config, &format!("completed deferred {}", pending.turn_id))?;
    }
    Ok(())
}

fn pending_path(config: &Config, turn_id: &str) -> PathBuf {
    config
        .state_directory
        .join(format!("pending-{turn_id}.json"))
}

fn emit_post_response(config: &Config, method: &str, result: &Value) -> Result<Vec<Value>, String> {
    let mut notifications = Vec::new();
    if method != "turn/start" {
        return Ok(notifications);
    }
    let turn_id = result
        .get("turn")
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| "turn/start response is missing turn id".to_owned())?;
    let status = result["turn"]["status"].as_str().unwrap_or_default();
    if config.scenario == "user_input_pause" && status == "inProgress" {
        notifications.push(json!({
            "jsonrpc": "2.0",
            "id": 900,
            "method": "item/tool/requestUserInput",
            "params": {"threadId": config.primary_thread, "turnId": turn_id}
        }));
        append_event(config, "user input notification")?;
    }
    if config.scenario == "duplicate_notification" && turn_id == "turn-1" {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": {"threadId": config.primary_thread, "turnId": turn_id}
        });
        for _ in 0..2 {
            notifications.push(notification.clone());
            append_event(config, "duplicate notification")?;
        }
    }
    Ok(notifications)
}

fn in_progress_turn(turn_id: &str, prompt: &str) -> Value {
    json!({
        "id": turn_id,
        "status": "inProgress",
        "items": [{
            "id": format!("user-{turn_id}"),
            "type": "userMessage",
            "content": [{"type": "text", "text": prompt, "text_elements": []}]
        }]
    })
}

fn completed_turn(turn_id: &str, prompt: &str, reply: &Value) -> Value {
    json!({
        "id": turn_id,
        "status": "completed",
        "items": [
            {
                "id": format!("user-{turn_id}"),
                "type": "userMessage",
                "content": [{"type": "text", "text": prompt, "text_elements": []}]
            },
            {
                "id": format!("assistant-{turn_id}"),
                "type": "agentMessage",
                "text": serde_json::to_string(reply).expect("reply is serializable"),
                "phase": "final_answer"
            }
        ]
    })
}

fn append_command_items(turn: &mut Value, command_items: Vec<Value>) {
    let items = turn["items"]
        .as_array_mut()
        .expect("completed turn items must be an array");
    let assistant = items
        .pop()
        .expect("completed turn must end with an assistant item");
    items.extend(command_items);
    items.push(assistant);
}

fn append_turn(config: &Config, thread_id: &str, turn: &Value) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(config.state_directory.join("turns.jsonl"))
        .map_err(|error| format!("open turns: {error}"))?;
    writeln!(
        file,
        "{}",
        serde_json::to_string(&json!({"thread_id": thread_id, "turn": turn}))
            .map_err(|error| error.to_string())?
    )
    .map_err(|error| format!("append turn: {error}"))
}

fn load_turns(config: &Config, thread_id: &str) -> Result<Vec<Value>, String> {
    let path = config.state_directory.join("turns.jsonl");
    let Ok(contents) = fs::read_to_string(path) else {
        return Ok(Vec::new());
    };
    let mut order = Vec::new();
    let mut turns = HashMap::new();
    for line in contents.lines() {
        let entry: Value =
            serde_json::from_str(line).map_err(|error| format!("parse saved turn: {error}"))?;
        if entry.get("thread_id").and_then(Value::as_str) != Some(thread_id) {
            continue;
        }
        let turn = entry
            .get("turn")
            .cloned()
            .ok_or_else(|| "saved turn is missing turn".to_owned())?;
        let id = turn
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "saved turn is missing id".to_owned())?
            .to_owned();
        if !turns.contains_key(&id) {
            order.push(id.clone());
        }
        turns.insert(id, turn);
    }
    Ok(order
        .into_iter()
        .filter_map(|id| turns.remove(&id))
        .collect())
}

fn turn_count(config: &Config) -> Result<usize, String> {
    Ok(load_turns(config, &config.primary_thread)?.len()
        + load_turns(config, &config.reviewer_thread)?.len())
}

fn action_count(config: &Config, action: &str) -> Result<usize, String> {
    let events = fs::read_to_string(config.state_directory.join("events.log"))
        .map_err(|error| format!("read events: {error}"))?;
    Ok(events
        .lines()
        .filter(|line| line.starts_with("turn ") && line.ends_with(action))
        .count())
}

fn append_event(config: &Config, event: &str) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(config.state_directory.join("events.log"))
        .map_err(|error| format!("open events: {error}"))?;
    writeln!(file, "{event}").map_err(|error| format!("append event: {error}"))
}

fn run_git_allow_status(config: &Config, arguments: &[&str]) -> Result<Output, String> {
    append_event(config, &format!("git {}", arguments.join(" ")))?;
    Command::new("git")
        .arg("-C")
        .arg(&config.primary_worktree)
        .args(arguments)
        .output()
        .map_err(|error| format!("execute git {arguments:?}: {error}"))
}

fn run_git(config: &Config, arguments: &[&str]) -> Result<Output, String> {
    let output = run_git_allow_status(config, arguments)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(format!(
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn git_text(cwd: &Path, arguments: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(arguments)
        .output()
        .map_err(|error| format!("execute git {arguments:?}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{field} is missing"))
}
