use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use consensus_mcp_server::{
    BackendError, MCP_TOOL_NAMES, ToolBackend, ToolSurface, serve, serve_surface,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};

#[derive(Default)]
struct FakeBackend {
    calls: Mutex<Vec<(String, Value)>>,
}

#[async_trait]
impl ToolBackend for FakeBackend {
    async fn call(&self, tool: &str, arguments: Value) -> Result<Value, BackendError> {
        self.calls
            .lock()
            .unwrap()
            .push((tool.to_owned(), arguments));
        Ok(json!({"run_id": "run-123", "status": "CONTRACTS_PENDING"}))
    }
}

async fn exchange(input: &str, backend: Arc<dyn ToolBackend>) -> Vec<Value> {
    let (client, server) = tokio::io::duplex(128 * 1024);
    let (server_read, server_write) = tokio::io::split(server);
    let task = tokio::spawn(async move {
        serve(BufReader::new(server_read), server_write, backend)
            .await
            .unwrap();
    });

    let (mut client_read, mut client_write) = tokio::io::split(client);
    client_write.write_all(input.as_bytes()).await.unwrap();
    client_write.shutdown().await.unwrap();

    let mut output = String::new();
    client_read.read_to_string(&mut output).await.unwrap();
    task.await.unwrap();
    output
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

async fn exchange_surface(
    input: &str,
    backend: Arc<dyn ToolBackend>,
    surface: ToolSurface,
) -> Vec<Value> {
    let (client, server) = tokio::io::duplex(128 * 1024);
    let (server_read, server_write) = tokio::io::split(server);
    let task = tokio::spawn(async move {
        serve_surface(BufReader::new(server_read), server_write, backend, surface)
            .await
            .unwrap();
    });

    let (mut client_read, mut client_write) = tokio::io::split(client);
    client_write.write_all(input.as_bytes()).await.unwrap();
    client_write.shutdown().await.unwrap();

    let mut output = String::new();
    client_read.read_to_string(&mut output).await.unwrap();
    task.await.unwrap();
    output
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test]
async fn participant_patch_surface_lists_only_patch_and_rejects_other_tools_before_dispatch() {
    let list_input = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}
"#;
    let operator_responses = exchange_surface(
        list_input,
        Arc::new(FakeBackend::default()),
        ToolSurface::Operator,
    )
    .await;
    let operator_tool_names = operator_responses[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    let participant_responses = exchange_surface(
        list_input,
        Arc::new(FakeBackend::default()),
        ToolSurface::ParticipantPatch,
    )
    .await;
    let participant_tool_names = participant_responses[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(operator_tool_names, MCP_TOOL_NAMES);
    assert_eq!(participant_tool_names, ["consensus_apply_patch"]);

    let backend = Arc::new(FakeBackend::default());
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"consensus_status","arguments":{"unexpected":true}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"consensus_apply_patch","arguments":{"run_id":"run-123","request_hash":"hash-123","patch":"diff --git a/file b/file"}}}"#,
        "\n",
    );
    let responses = exchange_surface(input, backend.clone(), ToolSurface::ParticipantPatch).await;
    assert_eq!(responses[0]["error"]["code"], -32602);
    assert_eq!(responses[1]["result"]["isError"], false);
    assert_eq!(
        backend.calls.lock().unwrap().as_slice(),
        [(
            "consensus_apply_patch".to_owned(),
            json!({
                "run_id": "run-123",
                "request_hash": "hash-123",
                "patch": "diff --git a/file b/file"
            })
        )]
    );
}

#[tokio::test]
async fn initializes_and_lists_exactly_the_nine_public_tools() {
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        "\n",
    );

    let responses = exchange(input, Arc::new(FakeBackend::default())).await;
    assert_eq!(
        responses.len(),
        2,
        "notifications must not receive responses"
    );
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(
        responses[0]["result"]["serverInfo"]["name"],
        "worktree-merge-consensus"
    );

    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    let names = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "consensus_doctor",
            "consensus_list_threads",
            "consensus_list_worktrees",
            "consensus_start",
            "consensus_status",
            "consensus_wait",
            "consensus_resume",
            "consensus_apply_patch",
            "consensus_cancel",
        ]
    );
    for tool in tools {
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
    }
    assert_eq!(
        tools[2]["inputSchema"]["required"],
        json!(["repository_path"])
    );
    assert_eq!(
        tools[3]["inputSchema"]["required"],
        json!([
            "primary_thread",
            "reviewer_thread",
            "primary_worktree",
            "reviewer_worktree"
        ])
    );
    assert_eq!(tools[4]["inputSchema"]["required"], json!([]));
    assert_eq!(tools[5]["inputSchema"]["required"], json!(["run_id"]));
    assert_eq!(tools[6]["inputSchema"]["required"], json!(["run_id"]));
    assert_eq!(
        tools[7]["inputSchema"]["required"],
        json!(["run_id", "request_hash", "patch"])
    );
    assert_eq!(tools[8]["inputSchema"]["required"], json!(["run_id"]));
}

#[tokio::test]
async fn accepts_codex_paginated_list_and_request_metadata_params() {
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"cursor":null,"_meta":{"progressToken":"startup"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"cursor":"next-page"}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":3,"method":"ping","params":null}"#,
        "\n",
    );

    let responses = exchange(input, Arc::new(FakeBackend::default())).await;
    assert_eq!(responses.len(), 3);
    assert_eq!(responses[0]["result"]["tools"].as_array().unwrap().len(), 9);
    assert_eq!(responses[1]["result"]["tools"].as_array().unwrap().len(), 9);
    assert_eq!(responses[2]["result"], json!({}));
}

#[tokio::test]
async fn wait_tool_supplies_bounded_long_poll_defaults() {
    let backend = Arc::new(FakeBackend::default());
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"consensus_wait","arguments":{"run_id":"run-123"}}}"#,
        "\n",
    );

    let responses = exchange(input, backend.clone()).await;
    assert_eq!(responses[0]["result"]["isError"], false);
    assert_eq!(
        backend.calls.lock().unwrap().as_slice(),
        [(
            "consensus_wait".to_owned(),
            json!({
                "run_id": "run-123",
                "after_cursor": 0,
                "timeout_ms": 25_000
            })
        )]
    );
}

#[tokio::test]
async fn tool_calls_accept_standard_request_metadata() {
    let backend = Arc::new(FakeBackend::default());
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":"start","method":"tools/call","params":{"name":"consensus_status","arguments":{},"_meta":{"progressToken":7}}}"#,
        "\n",
    );

    let responses = exchange(input, backend.clone()).await;
    assert_eq!(responses[0]["result"]["isError"], false);
    assert_eq!(backend.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn tool_calls_return_text_and_structured_content() {
    let backend = Arc::new(FakeBackend::default());
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":"start","method":"tools/call","params":{"name":"consensus_start","arguments":{"primary_thread":"p","reviewer_thread":"r","primary_worktree":"/repo/p","reviewer_worktree":"/repo/r","test_commands":["cargo test"]}}}"#,
        "\n",
    );

    let responses = exchange(input, backend.clone()).await;
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["id"], "start");
    assert_eq!(responses[0]["result"]["isError"], false);
    assert_eq!(
        responses[0]["result"]["structuredContent"],
        json!({"run_id": "run-123", "status": "CONTRACTS_PENDING"})
    );
    assert!(
        responses[0]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("run-123")
    );
    assert_eq!(
        backend.calls.lock().unwrap().as_slice(),
        [(
            "consensus_start".to_owned(),
            json!({
                "primary_thread": "p",
                "reviewer_thread": "r",
                "primary_worktree": "/repo/p",
                "reviewer_worktree": "/repo/r",
                "test_commands": ["cargo test"]
            })
        )]
    );
}

#[tokio::test]
async fn unsupported_methods_and_invalid_tool_arguments_use_json_rpc_errors() {
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"unknown","params":{}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"consensus_resume","arguments":{}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"consensus_list_worktrees","arguments":{}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"consensus_wait","arguments":{"run_id":"run-123","after_cursor":-1}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"consensus_wait","arguments":{"run_id":"run-123","timeout_ms":30001}}}"#,
        "\n",
    );
    let responses = exchange(input, Arc::new(FakeBackend::default())).await;
    assert_eq!(responses[0]["error"]["code"], -32601);
    assert_eq!(responses[1]["error"]["code"], -32602);
    assert_eq!(responses[2]["error"]["code"], -32602);
    assert_eq!(responses[3]["error"]["code"], -32602);
    assert_eq!(responses[4]["error"]["code"], -32602);
}
