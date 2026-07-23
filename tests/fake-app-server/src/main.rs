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

const CONTROLLED_PATCH_APPROVAL_KEY: &str = "plugins.worktree-merge-consensus.mcp_servers.worktreeMergeConsensus.tools.consensus_apply_patch.approval_mode";
const PARTICIPANT_MCP_SERVER: &str = "worktreeMergeConsensusParticipant";
const PARTICIPANT_PATCH_TOOL: &str = "consensus_apply_patch";

#[derive(Debug, Deserialize)]
struct Config {
    scenario: String,
    primary_thread: String,
    reviewer_thread: String,
    primary_thread_cwd: PathBuf,
    reviewer_thread_cwd: PathBuf,
    primary_worktree: PathBuf,
    reviewer_worktree: PathBuf,
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
            mark_thread_resumed(config, thread_id, params)?;
            Ok(json!({"thread": thread_detail(config, thread_id)?}))
        }
        "mcpServerStatus/list" => participant_mcp_status(config, params),
        "turn/interrupt" => {
            let thread_id = params
                .get("threadId")
                .and_then(Value::as_str)
                .ok_or_else(|| "turn/interrupt threadId is missing".to_owned())?;
            let turn_id = params
                .get("turnId")
                .and_then(Value::as_str)
                .ok_or_else(|| "turn/interrupt turnId is missing".to_owned())?;
            append_event(config, &format!("interrupt {thread_id} {turn_id}"))?;
            Ok(json!({}))
        }
        "config/read" => Ok(controlled_patch_config(config)),
        "config/batchWrite" => configure_controlled_patch(config, params),
        "command/exec" => execute_command(config, params),
        "turn/start" => start_turn(config, params),
        _ => Err(format!("unsupported method {method}")),
    }
}

fn controlled_patch_config(config: &Config) -> Value {
    json!({
        "config": {
            "plugins": {
                "worktree-merge-consensus": {
                    "mcp_servers": {
                        "worktreeMergeConsensus": {
                            "tools": {
                                "consensus_apply_patch": {
                                    "approval_mode": "approve"
                                }
                            }
                        }
                    }
                }
            }
        },
        "origins": {},
        "layers": null,
        "filePath": config.state_directory.join("codex-home/config.toml")
    })
}

fn configure_controlled_patch(config: &Config, params: &Value) -> Result<Value, String> {
    if params.get("reloadUserConfig") != Some(&json!(true))
        || params.get("edits")
            != Some(&json!([{
                "keyPath": CONTROLLED_PATCH_APPROVAL_KEY,
                "value": "approve",
                "mergeStrategy": "upsert"
            }]))
    {
        return Err("config/batchWrite did not contain the exact controlled-patch edit".to_owned());
    }
    let codex_home = config.state_directory.join("codex-home");
    fs::create_dir_all(&codex_home).map_err(|error| format!("create fake Codex home: {error}"))?;
    let file_path = codex_home.join("config.toml");
    fs::write(
        &file_path,
        b"[plugins.worktree-merge-consensus.mcp_servers.worktreeMergeConsensus.tools.consensus_apply_patch]\napproval_mode = \"approve\"\n",
    )
    .map_err(|error| format!("write fake Codex config: {error}"))?;
    append_event(config, "configured controlled patch approval")?;
    Ok(json!({
        "status": "ok",
        "version": "fake-config-v1",
        "filePath": file_path,
        "overriddenMetadata": null
    }))
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
    let resume_mode = consume_thread_resume(config, thread_id)?;
    let expected_resume_mode = if action == "REQUEST_PRIMARY_INTEGRATION" {
        "participant"
    } else {
        "default"
    };
    if resume_mode != expected_resume_mode {
        return Err(format!(
            "{action} used {resume_mode} resume instead of {expected_resume_mode}"
        ));
    }
    if let Err(error) = validate_turn_policy(config, action, thread_id, params, &payload) {
        append_event(
            config,
            &format!("turn-policy-error {error}; params={params}"),
        )?;
        return Err(error);
    }
    append_event(config, &format!("method turn/start {thread_id} {action}"))?;
    append_event(config, &format!("turn {thread_id} {action}"))?;
    let occurrence = action_count(config, action)?;

    let turn_id = format!("turn-{}", turn_count(config)? + 1);
    let reply = scripted_reply(config, action, occurrence, &metadata)?;
    let deferred = deferred_marker(config, action);
    let turn = if deferred.is_some() {
        let pending = PendingTurn {
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.clone(),
            prompt: prompt.to_owned(),
            reply: reply.clone(),
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

fn mark_thread_resumed(config: &Config, thread_id: &str, params: &Value) -> Result<(), String> {
    let mode = match params.get("config") {
        None => "default",
        Some(resume_config) => {
            validate_participant_resume_config(resume_config)?;
            "participant"
        }
    };
    fs::write(resume_marker(config, thread_id), format!("{mode}\n"))
        .map_err(|error| format!("mark resumed task {thread_id}: {error}"))?;
    append_event(config, &format!("method thread/resume {thread_id}"))?;
    append_event(config, &format!("resume {thread_id}"))
}

fn consume_thread_resume(config: &Config, thread_id: &str) -> Result<String, String> {
    let marker = resume_marker(config, thread_id);
    if !marker.exists() {
        return Err(format!(
            "task {thread_id} must be resumed before turn/start"
        ));
    }
    let mode = fs::read_to_string(&marker)
        .map_err(|error| format!("read resumed task {thread_id}: {error}"))?
        .trim()
        .to_owned();
    fs::remove_file(marker)
        .map_err(|error| format!("consume resumed task {thread_id}: {error}"))?;
    Ok(mode)
}

fn validate_participant_resume_config(config: &Value) -> Result<(), String> {
    let server = config
        .pointer(&format!("/mcp_servers/{PARTICIPANT_MCP_SERVER}"))
        .and_then(Value::as_object)
        .ok_or_else(|| "participant resume config is missing the exact MCP server".to_owned())?;
    if server.len() != 7
        || !server
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| Path::new(command).is_absolute())
        || server.get("args") != Some(&json!(["participant-mcp-server"]))
        || server.get("required") != Some(&json!(true))
        || server.get("enabled_tools") != Some(&json!([PARTICIPANT_PATCH_TOOL]))
        || server.get("startup_timeout_sec") != Some(&json!(10))
        || server.get("tool_timeout_sec") != Some(&json!(300))
        || server.get("tools")
            != Some(&json!({
                PARTICIPANT_PATCH_TOOL: {"approval_mode": "approve"}
            }))
    {
        return Err(format!("participant resume config is malformed: {config}"));
    }
    Ok(())
}

fn participant_mcp_status(config: &Config, params: &Value) -> Result<Value, String> {
    let thread_id = params
        .get("threadId")
        .and_then(Value::as_str)
        .ok_or_else(|| "mcpServerStatus/list threadId is missing".to_owned())?;
    if params.get("detail") != Some(&json!("toolsAndAuthOnly")) {
        return Err("mcpServerStatus/list detail is not toolsAndAuthOnly".to_owned());
    }
    let resume_mode = fs::read_to_string(resume_marker(config, thread_id))
        .map_err(|error| format!("MCP status requested before task resume: {error}"))?;
    if resume_mode.trim() != "participant" {
        return Err("MCP status requested without participant resume config".to_owned());
    }
    append_event(config, &format!("method mcpServerStatus/list {thread_id}"))?;
    let data = match config.scenario.as_str() {
        "participant_patch_missing_server" => json!([]),
        "participant_patch_missing_tool" => json!([{
            "name": PARTICIPANT_MCP_SERVER,
            "tools": {}
        }]),
        "participant_patch_extra_tool" => json!([{
            "name": PARTICIPANT_MCP_SERVER,
            "tools": {
                PARTICIPANT_PATCH_TOOL: {"inputSchema": {"type": "object"}},
                "unexpected_patch_tool": {"inputSchema": {"type": "object"}}
            }
        }]),
        "participant_patch_malformed_inventory" => json!([{
            "name": PARTICIPANT_MCP_SERVER,
            "tools": {
                PARTICIPANT_PATCH_TOOL: "not-an-object"
            }
        }]),
        _ => json!([{
            "name": PARTICIPANT_MCP_SERVER,
            "tools": {
                PARTICIPANT_PATCH_TOOL: {"inputSchema": {"type": "object"}}
            }
        }]),
    };
    Ok(json!({"data": data}))
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
    let expected_approval = "never";
    if params.get("approvalPolicy") != Some(&json!(expected_approval)) {
        return Err("turn/start approval policy is not fail-closed".to_owned());
    }
    let expected_sandbox = json!({"type": "dangerFullAccess"});
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
) -> Result<String, String> {
    if config.scenario == "invalid_reply" && occurrence == 1 {
        return Ok("not a protocol response".to_owned());
    }
    if action == "REQUEST_REVIEWER_PLAN_VERDICT" {
        let issue = match config.scenario.as_str() {
            "plan_revision" if occurrence == 1 => Some("missing-reviewer-edge".to_owned()),
            "no_progress" => Some("same-plan-gap".to_owned()),
            "round_limit" => Some(format!("round-gap-{occurrence}")),
            _ => None,
        };
        if let Some(issue) = issue {
            return Ok(changes_required_message(&issue));
        }
    }
    if action == "REQUEST_REVIEWER_RESULT_VERDICT"
        && config.scenario == "result_revision"
        && occurrence == 1
    {
        return Ok(changes_required_message("missing-result-edge"));
    }
    match action {
        "REQUEST_PRIMARY_CONTRACT" => contract_ready_reply(
            json!({
                "items": ["primary-feature"],
                "tests": declared_tests(metadata)
            }),
        ),
        "REQUEST_REVIEWER_CONTRACT" => contract_ready_reply(
            json!({
                "items": ["reviewer-feature"],
                "tests": declared_tests(metadata)
            }),
        ),
        "REQUEST_PRIMARY_PLAN" => {
            let step = match config.scenario.as_str() {
                "no_progress" => "unchanged plan".to_owned(),
                "round_limit" => format!("round plan {occurrence}"),
                "plan_revision" if occurrence > 1 => "preserve reviewer edge".to_owned(),
                _ => "merge both frozen commits".to_owned(),
            };
            Ok(format!(
                "<consensus-result>PLAN_READY</consensus-result>\n\n## Integration plan\n\n{step}. Preserve both contracts and run every frozen test."
            ))
        }
        "REQUEST_REVIEWER_PLAN_VERDICT" => Ok(
            "<consensus-result>APPROVED</consensus-result>\n\nThe current proposal covers both contracts."
                .to_owned(),
        ),
        "REQUEST_PRIMARY_INTEGRATION" => {
            integrate(config, metadata, occurrence)?;
            Ok(
                "<consensus-result>INTEGRATION_READY</consensus-result>\n\nBoth frozen commits were integrated according to the approved plan."
                    .to_owned(),
            )
        }
        "REQUEST_PRIMARY_VERIFICATION" => Ok(
            "<consensus-result>VERIFICATION_READY</consensus-result>\n\nThe integration is ready for coordinator-owned verification."
                .to_owned(),
        ),
        "REQUEST_REVIEWER_RESULT_VERDICT" => Ok(
            "<consensus-result>APPROVED</consensus-result>\n\nThe exact tested integration result preserves both contracts."
                .to_owned(),
        ),
        _ => Err(format!("unsupported action {action}")),
    }
}

fn contract_ready_reply(contract: Value) -> Result<String, String> {
    Ok(format!(
        "<consensus-result>CONTRACT_READY</consensus-result>\n{}",
        serde_json::to_string(&contract).map_err(|error| error.to_string())?
    ))
}

fn changes_required_message(issue_id: &str) -> String {
    format!(
        "<consensus-result>CHANGES_REQUIRED</consensus-result>\n\n## {issue_id}\n\nThe current proposal must preserve this behavior before approval."
    )
}

fn integrate(config: &Config, metadata: &Value, occurrence: usize) -> Result<(), String> {
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
    Ok(())
}

fn execute_command(config: &Config, params: &Value) -> Result<Value, String> {
    let command = params
        .get("command")
        .and_then(Value::as_array)
        .ok_or_else(|| "command/exec command is missing".to_owned())?
        .iter()
        .map(|argument| {
            argument
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "command/exec argument is not a string".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (program, arguments) = command
        .split_first()
        .ok_or_else(|| "command/exec command is empty".to_owned())?;
    let cwd = params
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| "command/exec cwd is missing".to_owned())?;
    let cwd =
        fs::canonicalize(cwd).map_err(|error| format!("canonicalize command/exec cwd: {error}"))?;
    let timeout_ms = params
        .get("timeoutMs")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| "command/exec timeoutMs must be greater than zero".to_owned())?;
    let output_bytes_cap = params
        .get("outputBytesCap")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| "command/exec outputBytesCap must be greater than zero".to_owned())?;
    if params.get("sandboxPolicy") != Some(&json!({"type": "dangerFullAccess"})) {
        return Err("command/exec sandbox policy is not dangerFullAccess".to_owned());
    }

    append_event(
        config,
        &format!(
            "verification-test {} {} timeout={timeout_ms}",
            cwd.display(),
            command.join(" ")
        ),
    )?;
    let output = Command::new(program)
        .args(arguments)
        .current_dir(&cwd)
        .output()
        .map_err(|error| format!("run command/exec {command:?}: {error}"))?;
    Ok(json!({
        "exitCode": output.status.code().unwrap_or(1),
        "stdout": bounded_output(&output.stdout, output_bytes_cap),
        "stderr": bounded_output(&output.stderr, output_bytes_cap)
    }))
}

fn bounded_output(output: &[u8], cap: usize) -> String {
    let start = output.len().saturating_sub(cap);
    String::from_utf8_lossy(&output[start..]).into_owned()
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
    reply: String,
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
        let turn = completed_turn(&pending.turn_id, &pending.prompt, &pending.reply);
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
        let turn = result
            .get("turn")
            .cloned()
            .ok_or_else(|| "turn/start response is missing canonical turn".to_owned())?;
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": {"threadId": config.primary_thread, "turn": turn}
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

fn completed_turn(turn_id: &str, prompt: &str, reply: &str) -> Value {
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
                "text": reply,
                "phase": "final_answer"
            }
        ]
    })
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
