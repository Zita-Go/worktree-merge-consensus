use std::path::PathBuf;

use app_server_client::{
    AppServer, CONTROLLED_PATCH_APPROVAL_KEY, CodexAppServer, CommandExecRequest,
    ParticipantMcpConfig, ThreadForkPolicy, ThreadResumePolicy, ThreadRuntimeStatus,
    TurnExecutionPolicy, transport::JsonRpcTransport,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex, split};

#[tokio::test]
async fn typed_methods_emit_the_pinned_v2_request_shapes() {
    let (client_side, server_side) = duplex(128 * 1024);
    let (client_read, client_write) = split(client_side);
    let client = CodexAppServer::from_transport(JsonRpcTransport::new(client_read, client_write));
    let (server_read, mut server_write) = split(server_side);
    let mut lines = BufReader::new(server_read).lines();

    let server = tokio::spawn(async move {
        let initialize = read_request(&mut lines).await;
        assert_eq!(initialize["method"], "initialize");
        assert_eq!(
            initialize["params"],
            json!({
                "clientInfo": {
                    "name": "worktree-merge-consensus",
                    "title": "Worktree Merge Consensus",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {
                    "experimentalApi": true
                }
            })
        );
        respond(
            &mut server_write,
            &initialize,
            json!({
                "codexHome": "/home/test/.codex",
                "platformFamily": "unix",
                "platformOs": "linux",
                "userAgent": "codex-cli/0.144.5"
            }),
        )
        .await;
        let initialized = read_request(&mut lines).await;
        assert_eq!(initialized["method"], "initialized");
        assert!(initialized.get("id").is_none());

        let list = read_request(&mut lines).await;
        assert_eq!(list["method"], "thread/list");
        assert_eq!(list["params"]["limit"], 50);
        assert_eq!(list["params"]["sortKey"], "updated_at");
        assert_eq!(list["params"]["sortDirection"], "desc");
        respond(
            &mut server_write,
            &list,
            json!({
                "data": [{
                    "id": "t-1",
                    "cwd": "/repo/primary",
                    "name": "Primary task",
                    "preview": "implement primary",
                    "cliVersion": "0.144.5",
                    "createdAt": 10,
                    "updatedAt": 20,
                    "status": {"type": "idle"},
                    "source": "appServer",
                    "turns": []
                }],
                "nextCursor": "next-page"
            }),
        )
        .await;

        let read = read_request(&mut lines).await;
        assert_eq!(read["method"], "thread/read");
        assert_eq!(
            read["params"],
            json!({"threadId": "t-1", "includeTurns": true})
        );
        respond(
            &mut server_write,
            &read,
            json!({"thread": thread_with_turns()}),
        )
        .await;

        let summary_read = read_request(&mut lines).await;
        assert_eq!(summary_read["method"], "thread/read");
        assert_eq!(
            summary_read["params"],
            json!({"threadId": "fork-1", "includeTurns": false})
        );
        respond(
            &mut server_write,
            &summary_read,
            json!({"thread": thread_with_id("fork-1")}),
        )
        .await;

        let resume = read_request(&mut lines).await;
        assert_eq!(resume["method"], "thread/resume");
        assert_eq!(resume["params"], json!({"threadId": "t-1"}));
        respond(
            &mut server_write,
            &resume,
            json!({"thread": thread_with_turns()}),
        )
        .await;

        let participant_resume = read_request(&mut lines).await;
        assert_eq!(participant_resume["method"], "thread/resume");
        assert_eq!(
            participant_resume["params"],
            json!({
                "threadId": "t-1",
                "config": {
                    "mcp_servers": {
                        "worktreeMergeConsensusParticipant": {
                            "command": "/opt/codex-consensus",
                            "args": ["participant-mcp-server"],
                            "required": true,
                            "enabled_tools": ["consensus_apply_patch"],
                            "startup_timeout_sec": 10,
                            "tool_timeout_sec": 300,
                            "tools": {
                                "consensus_apply_patch": {"approval_mode": "approve"}
                            }
                        }
                    }
                }
            })
        );
        respond(
            &mut server_write,
            &participant_resume,
            json!({"thread": thread_with_turns()}),
        )
        .await;

        let fork = read_request(&mut lines).await;
        assert_eq!(fork["method"], "thread/fork");
        assert_eq!(
            fork["params"],
            json!({
                "threadId": "t-1",
                "config": {
                    "mcp_servers": {
                        "worktreeMergeConsensusParticipant": {
                            "command": "/opt/codex-consensus",
                            "args": ["participant-mcp-server"],
                            "required": true,
                            "enabled_tools": ["consensus_apply_patch"],
                            "startup_timeout_sec": 10,
                            "tool_timeout_sec": 300,
                            "tools": {
                                "consensus_apply_patch": {"approval_mode": "approve"}
                            }
                        }
                    }
                },
                "ephemeral": true,
                "excludeTurns": false
            })
        );
        assert!(fork["params"].get("lastTurnId").is_none());
        assert!(fork["params"].get("path").is_none());
        assert!(fork["params"].get("threadSource").is_none());
        assert!(fork["params"].get("deferGoalContinuation").is_none());
        respond(
            &mut server_write,
            &fork,
            json!({"thread": thread_with_id("fork-1")}),
        )
        .await;

        let turn = read_request(&mut lines).await;
        assert_eq!(turn["method"], "turn/start");
        assert_eq!(turn["params"]["threadId"], "t-1");
        assert_eq!(
            turn["params"]["input"],
            json!([{"type": "text", "text": "review this", "text_elements": []}])
        );
        assert!(turn["params"].get("outputSchema").is_none());
        assert_eq!(turn["params"]["approvalPolicy"], "never");
        assert_eq!(turn["params"]["approvalsReviewer"], "user");
        assert_eq!(
            turn["params"]["environments"],
            json!([{"environmentId": "local", "cwd": "/repo/reviewer"}])
        );
        assert_eq!(turn["params"]["cwd"], "/repo/reviewer");
        assert_eq!(
            turn["params"]["runtimeWorkspaceRoots"],
            json!(["/repo/reviewer"])
        );
        assert_eq!(
            turn["params"]["sandboxPolicy"],
            json!({"type": "dangerFullAccess"})
        );
        respond(
            &mut server_write,
            &turn,
            json!({"turn": {"id": "turn-2", "status": "inProgress", "items": []}}),
        )
        .await;

        let integration_turn = read_request(&mut lines).await;
        assert_eq!(integration_turn["method"], "turn/start");
        assert_eq!(integration_turn["params"]["approvalPolicy"], "never");
        assert_eq!(
            integration_turn["params"]["environments"],
            json!([{"environmentId": "local", "cwd": "/repo/primary"}])
        );
        assert_eq!(integration_turn["params"]["cwd"], "/repo/primary");
        assert_eq!(
            integration_turn["params"]["runtimeWorkspaceRoots"],
            json!(["/repo/primary"])
        );
        assert_eq!(
            integration_turn["params"]["sandboxPolicy"],
            json!({"type": "dangerFullAccess"})
        );
        respond(
            &mut server_write,
            &integration_turn,
            json!({"turn": {"id": "turn-3", "status": "inProgress", "items": []}}),
        )
        .await;

        let verification_turn = read_request(&mut lines).await;
        assert_eq!(verification_turn["method"], "turn/start");
        assert_eq!(verification_turn["params"]["approvalPolicy"], "never");
        assert_eq!(
            verification_turn["params"]["environments"],
            json!([{
                "environmentId": "local",
                "cwd": "/state/verification/run"
            }])
        );
        assert_eq!(
            verification_turn["params"]["cwd"],
            "/state/verification/run"
        );
        assert_eq!(
            verification_turn["params"]["runtimeWorkspaceRoots"],
            json!(["/state/verification/run"])
        );
        assert_eq!(
            verification_turn["params"]["sandboxPolicy"],
            json!({"type": "dangerFullAccess"})
        );
        respond(
            &mut server_write,
            &verification_turn,
            json!({"turn": {"id": "turn-4", "status": "inProgress", "items": []}}),
        )
        .await;

        let exec = read_request(&mut lines).await;
        assert_eq!(exec["method"], "command/exec");
        assert_eq!(
            exec["params"],
            json!({
                "command": ["cargo", "test", "--locked"],
                "cwd": "/state/verification/run",
                "timeoutMs": 1_800_000,
                "outputBytesCap": 65_536,
                "sandboxPolicy": {"type": "dangerFullAccess"}
            })
        );
        respond(
            &mut server_write,
            &exec,
            json!({
                "exitCode": 7,
                "stdout": "partial stdout",
                "stderr": "test failed"
            }),
        )
        .await;

        let config_read = read_request(&mut lines).await;
        assert_eq!(config_read["method"], "config/read");
        assert_eq!(config_read["params"], json!({"includeLayers": false}));
        respond(&mut server_write, &config_read, plugin_config("prompt")).await;

        let config_write = read_request(&mut lines).await;
        assert_eq!(config_write["method"], "config/batchWrite");
        assert_eq!(config_write["params"]["reloadUserConfig"], true);
        assert_eq!(
            config_write["params"]["edits"],
            json!([{
                "keyPath": CONTROLLED_PATCH_APPROVAL_KEY,
                "value": "approve",
                "mergeStrategy": "upsert"
            }])
        );
        respond(
            &mut server_write,
            &config_write,
            json!({
                "status": "ok",
                "version": "config-v2",
                "filePath": "/home/test/.codex/config.toml",
                "overriddenMetadata": null
            }),
        )
        .await;

        let verify_config = read_request(&mut lines).await;
        assert_eq!(verify_config["method"], "config/read");
        respond(&mut server_write, &verify_config, plugin_config("approve")).await;

        let interrupt = read_request(&mut lines).await;
        assert_eq!(interrupt["method"], "turn/interrupt");
        assert_eq!(
            interrupt["params"],
            json!({"threadId": "t-1", "turnId": "turn-4"})
        );
        respond(&mut server_write, &interrupt, json!({})).await;
    });

    client.initialize().await.unwrap();
    let page = client.list_threads(None, 50).await.unwrap();
    assert_eq!(page.data[0].id, "t-1");
    assert_eq!(page.next_cursor.as_deref(), Some("next-page"));
    let detail = client.read_thread("t-1").await.unwrap();
    assert_eq!(detail.turns.len(), 1);
    let summary = client.read_thread_summary("fork-1").await.unwrap();
    assert_eq!(summary.id, "fork-1");
    assert_eq!(summary.runtime_status().unwrap(), ThreadRuntimeStatus::Idle);
    client
        .resume_thread("t-1", &ThreadResumePolicy::Default)
        .await
        .unwrap();
    client
        .resume_thread(
            "t-1",
            &ThreadResumePolicy::Participant(ParticipantMcpConfig {
                participant_executable: PathBuf::from("/opt/codex-consensus"),
            }),
        )
        .await
        .unwrap();
    let fork = client
        .fork_thread(
            "t-1",
            &ThreadForkPolicy::EphemeralParticipant(ParticipantMcpConfig {
                participant_executable: PathBuf::from("/opt/codex-consensus"),
            }),
        )
        .await
        .unwrap();
    assert_eq!(fork.summary.id, "fork-1");
    let turn = client
        .start_turn(
            "t-1",
            "review this",
            &TurnExecutionPolicy::ReadOnly {
                cwd: PathBuf::from("/repo/reviewer"),
            },
        )
        .await
        .unwrap();
    assert_eq!(turn.id, "turn-2");
    let turn = client
        .start_turn(
            "t-1",
            "integrate this",
            &TurnExecutionPolicy::PrimaryIntegration {
                cwd: PathBuf::from("/repo/primary"),
                git_common_dir: PathBuf::from("/repo/.git"),
            },
        )
        .await
        .unwrap();
    assert_eq!(turn.id, "turn-3");
    let turn = client
        .start_turn(
            "t-1",
            "verify this",
            &TurnExecutionPolicy::PrimaryVerification {
                cwd: PathBuf::from("/state/verification/run"),
            },
        )
        .await
        .unwrap();
    assert_eq!(turn.id, "turn-4");
    let exec = client
        .execute_command(&CommandExecRequest {
            command: vec!["cargo".into(), "test".into(), "--locked".into()],
            cwd: PathBuf::from("/state/verification/run"),
            timeout_ms: 1_800_000,
            output_bytes_cap: 65_536,
        })
        .await
        .unwrap();
    assert_eq!(exec.exit_code, 7);
    assert_eq!(exec.stdout, "partial stdout");
    assert_eq!(exec.stderr, "test failed");
    assert_eq!(
        client.controlled_patch_approval_mode().await.unwrap(),
        Some("prompt".into())
    );
    let configured = client.configure_controlled_patch_approval().await.unwrap();
    assert_eq!(configured["status"], "ok");
    client.interrupt_turn("t-1", "turn-4").await.unwrap();
    server.await.unwrap();
}

#[test]
fn thread_runtime_status_is_strict() {
    assert_eq!(
        summary_with_status(json!({"type": "notLoaded"}))
            .runtime_status()
            .unwrap(),
        ThreadRuntimeStatus::NotLoaded
    );
    assert_eq!(
        summary_with_status(json!({"type": "idle"}))
            .runtime_status()
            .unwrap(),
        ThreadRuntimeStatus::Idle
    );
    assert_eq!(
        summary_with_status(json!({"type": "active", "activeFlags": []}))
            .runtime_status()
            .unwrap(),
        ThreadRuntimeStatus::Active
    );
    assert_eq!(
        summary_with_status(json!({"type": "systemError"}))
            .runtime_status()
            .unwrap(),
        ThreadRuntimeStatus::SystemError
    );
    assert!(
        summary_with_status(json!({"type": "future"}))
            .runtime_status()
            .unwrap_err()
            .contains("unsupported thread status")
    );
}

#[tokio::test]
async fn thread_goal_response_is_strict() {
    let responses = vec![
        json!({"goal": null}),
        json!({
            "goal": {
                "threadId": "t-1",
                "objective": "preserve both implementations",
                "status": "active",
                "tokenBudget": null,
                "tokensUsed": 0,
                "timeUsedSeconds": 0,
                "createdAt": 1,
                "updatedAt": 1
            }
        }),
        json!({}),
        json!({"goal": "active"}),
    ];
    let (client_side, server_side) = duplex(16 * 1024);
    let (client_read, client_write) = split(client_side);
    let client = CodexAppServer::from_transport(JsonRpcTransport::new(client_read, client_write));
    let (server_read, mut server_write) = split(server_side);
    let mut lines = BufReader::new(server_read).lines();

    let server = tokio::spawn(async move {
        for response in responses {
            let request = read_request(&mut lines).await;
            assert_eq!(request["method"], "thread/goal/get");
            assert_eq!(request["params"], json!({"threadId": "t-1"}));
            respond(&mut server_write, &request, response).await;
        }
    });

    assert_eq!(client.get_thread_goal("t-1").await.unwrap(), None);
    let goal = client.get_thread_goal("t-1").await.unwrap().unwrap();
    assert_eq!(goal["threadId"], "t-1");
    assert_eq!(goal["objective"], "preserve both implementations");

    for expected in [
        "thread/goal/get result is missing goal",
        "thread/goal/get goal must be an object or null",
    ] {
        let error = client.get_thread_goal("t-1").await.unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
    server.await.unwrap();
}

#[tokio::test]
async fn malformed_initialize_handshake_fails_closed() {
    let (client_side, server_side) = duplex(16 * 1024);
    let (client_read, client_write) = split(client_side);
    let client = CodexAppServer::from_transport(JsonRpcTransport::new(client_read, client_write));
    let (server_read, mut server_write) = split(server_side);
    let mut lines = BufReader::new(server_read).lines();

    let server = tokio::spawn(async move {
        let initialize = read_request(&mut lines).await;
        respond(
            &mut server_write,
            &initialize,
            json!({"userAgent": "codex-cli/0.144.5"}),
        )
        .await;
    });

    let error = client.initialize().await.unwrap_err();
    assert!(error.to_string().contains("INCOMPATIBLE_CODEX"));
    server.await.unwrap();
}

#[tokio::test]
async fn missing_required_method_is_reported_as_incompatible_codex() {
    let (client_side, server_side) = duplex(16 * 1024);
    let (client_read, client_write) = split(client_side);
    let client = CodexAppServer::from_transport(JsonRpcTransport::new(client_read, client_write));
    let (server_read, mut server_write) = split(server_side);
    let mut lines = BufReader::new(server_read).lines();

    let server = tokio::spawn(async move {
        let request = read_request(&mut lines).await;
        server_write
            .write_all(
                format!(
                    "{}\n",
                    serde_json::to_string(&json!({
                        "jsonrpc": "2.0",
                        "id": request["id"],
                        "error": {"code": -32601, "message": "Method not found"}
                    }))
                    .unwrap()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        server_write.flush().await.unwrap();
    });

    let error = client
        .start_turn(
            "t-1",
            "review",
            &TurnExecutionPolicy::ReadOnly {
                cwd: PathBuf::from("/repo/reviewer"),
            },
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("INCOMPATIBLE_CODEX"));
    assert!(error.to_string().contains("turn/start"));
    server.await.unwrap();
}

#[tokio::test]
async fn execute_command_rejects_invalid_requests_before_sending() {
    let (client_side, _server_side) = duplex(16 * 1024);
    let (client_read, client_write) = split(client_side);
    let client = CodexAppServer::from_transport(JsonRpcTransport::new(client_read, client_write));

    let error = client
        .execute_command(&CommandExecRequest {
            command: vec![],
            cwd: PathBuf::from("/state/verification/run"),
            timeout_ms: 1_800_000,
            output_bytes_cap: 65_536,
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("command argv must not be empty"));

    let error = client
        .execute_command(&CommandExecRequest {
            command: vec!["cargo".into()],
            cwd: PathBuf::from("relative/path"),
            timeout_ms: 1_800_000,
            output_bytes_cap: 65_536,
        })
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("command cwd must be an absolute path")
    );

    let error = client
        .execute_command(&CommandExecRequest {
            command: vec!["cargo".into()],
            cwd: PathBuf::from("/state/verification/run"),
            timeout_ms: 0,
            output_bytes_cap: 65_536,
        })
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("command timeout must be greater than zero")
    );

    let error = client
        .execute_command(&CommandExecRequest {
            command: vec!["cargo".into()],
            cwd: PathBuf::from("/state/verification/run"),
            timeout_ms: 1_800_000,
            output_bytes_cap: 0,
        })
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("command output_bytes_cap must be greater than zero")
    );
}

#[tokio::test]
async fn mcp_status_consumes_all_pages() {
    let (client_side, server_side) = duplex(16 * 1024);
    let (client_read, client_write) = split(client_side);
    let client = CodexAppServer::from_transport(JsonRpcTransport::new(client_read, client_write));
    let (server_read, mut server_write) = split(server_side);
    let mut lines = BufReader::new(server_read).lines();

    let server = tokio::spawn(async move {
        let first = read_request(&mut lines).await;
        assert_eq!(first["method"], "mcpServerStatus/list");
        assert_eq!(
            first["params"],
            json!({
                "threadId": "t-1",
                "detail": "toolsAndAuthOnly",
                "limit": 100,
                "cursor": null
            })
        );
        respond(
            &mut server_write,
            &first,
            json!({
                "data": [{"name": "unrelated", "tools": {}}],
                "nextCursor": "page-2"
            }),
        )
        .await;

        let second = read_request(&mut lines).await;
        assert_eq!(second["method"], "mcpServerStatus/list");
        assert_eq!(
            second["params"],
            json!({
                "threadId": "t-1",
                "detail": "toolsAndAuthOnly",
                "limit": 100,
                "cursor": "page-2"
            })
        );
        respond(
            &mut server_write,
            &second,
            json!({
                "data": [{
                    "name": "worktreeMergeConsensusParticipant",
                    "tools": {
                        "consensus_apply_patch": {
                            "inputSchema": {"type": "object"}
                        }
                    }
                }],
                "nextCursor": null
            }),
        )
        .await;
    });

    let statuses = client.list_mcp_server_status("t-1").await.unwrap();
    assert_eq!(statuses.len(), 2);
    assert_eq!(statuses[0].name, "unrelated");
    assert_eq!(statuses[1].name, "worktreeMergeConsensusParticipant");
    assert_eq!(
        statuses[1].tools.get("consensus_apply_patch"),
        Some(&json!({"inputSchema": {"type": "object"}}))
    );
    server.await.unwrap();
}

#[tokio::test]
async fn mcp_status_rejects_incomplete_or_unbounded_pagination() {
    assert_mcp_status_error(
        vec![json!({"data": []})],
        "mcpServerStatus/list result is missing nextCursor",
    )
    .await;
    assert_mcp_status_error(
        vec![json!({"data": [], "nextCursor": 7})],
        "nextCursor must be a string or null",
    )
    .await;
    assert_mcp_status_error(
        vec![
            json!({"data": [], "nextCursor": "repeat"}),
            json!({"data": [], "nextCursor": "repeat"}),
        ],
        "repeated MCP status cursor",
    )
    .await;
    assert_mcp_status_error(
        vec![
            json!({
                "data": [{"name": "duplicate", "tools": {}}],
                "nextCursor": "page-2"
            }),
            json!({
                "data": [{"name": "duplicate", "tools": {}}],
                "nextCursor": null
            }),
        ],
        "duplicate MCP server name",
    )
    .await;

    let page_limit = (0..16)
        .map(|index| {
            json!({
                "data": [],
                "nextCursor": format!("page-{}", index + 2)
            })
        })
        .collect();
    assert_mcp_status_error(page_limit, "MCP status page limit exceeded").await;

    let too_many_servers = (0..1_001)
        .map(|index| json!({"name": format!("server-{index}"), "tools": {}}))
        .collect::<Vec<_>>();
    assert_mcp_status_error(
        vec![json!({"data": too_many_servers, "nextCursor": null})],
        "MCP status server limit exceeded",
    )
    .await;

    for (definition, expected) in [
        (
            json!({"description": "missing schema"}),
            "MCP tool inputSchema must be an object",
        ),
        (
            json!({"inputSchema": "not-an-object"}),
            "MCP tool inputSchema must be an object",
        ),
    ] {
        assert_mcp_status_error(
            vec![json!({
                "data": [{
                    "name": "participant",
                    "tools": {"consensus_apply_patch": definition}
                }],
                "nextCursor": null
            })],
            expected,
        )
        .await;
    }
}

async fn assert_mcp_status_error(responses: Vec<Value>, expected: &str) {
    let (client_side, server_side) = duplex(512 * 1024);
    let (client_read, client_write) = split(client_side);
    let client = CodexAppServer::from_transport(JsonRpcTransport::new(client_read, client_write));
    let (server_read, mut server_write) = split(server_side);
    let mut lines = BufReader::new(server_read).lines();

    let server = tokio::spawn(async move {
        for (index, response) in responses.into_iter().enumerate() {
            let request = read_request(&mut lines).await;
            assert_eq!(request["method"], "mcpServerStatus/list");
            assert_eq!(request["params"]["threadId"], "t-1");
            assert_eq!(request["params"]["detail"], "toolsAndAuthOnly");
            assert_eq!(request["params"]["limit"], 100);
            if index == 0 {
                assert!(request["params"]["cursor"].is_null());
            }
            respond(&mut server_write, &request, response).await;
        }
    });

    let error = client.list_mcp_server_status("t-1").await.unwrap_err();
    assert!(error.to_string().contains(expected), "{error}");
    server.await.unwrap();
}

fn thread_with_turns() -> Value {
    json!({
        "id": "t-1",
        "cwd": "/repo/primary",
        "name": "Primary task",
        "preview": "implement primary",
        "cliVersion": "0.144.5",
        "createdAt": 10,
        "updatedAt": 20,
        "status": {"type": "idle"},
        "source": "appServer",
        "turns": [{"id": "turn-1", "status": "completed", "items": []}]
    })
}

fn thread_with_id(id: &str) -> Value {
    let mut thread = thread_with_turns();
    thread["id"] = json!(id);
    thread
}

fn summary_with_status(status: Value) -> app_server_client::ThreadSummary {
    let mut thread = thread_with_turns();
    thread["status"] = status;
    serde_json::from_value(thread).unwrap()
}

fn plugin_config(mode: &str) -> Value {
    json!({
        "config": {
            "plugins": {
                "worktree-merge-consensus": {
                    "mcp_servers": {
                        "worktreeMergeConsensus": {
                            "tools": {
                                "consensus_apply_patch": {
                                    "approval_mode": mode
                                }
                            }
                        }
                    }
                }
            }
        },
        "origins": {},
        "layers": null
    })
}

async fn read_request(
    lines: &mut tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
) -> Value {
    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap()
}

async fn respond(
    writer: &mut tokio::io::WriteHalf<tokio::io::DuplexStream>,
    request: &Value,
    result: Value,
) {
    writer
        .write_all(
            format!(
                "{}\n",
                serde_json::to_string(&json!({
                    "jsonrpc": "2.0",
                    "id": request["id"],
                    "result": result
                }))
                .unwrap()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    writer.flush().await.unwrap();
}
