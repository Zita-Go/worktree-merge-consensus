use app_server_client::{AppServer, CodexAppServer, transport::JsonRpcTransport};
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
                    "version": "0.1.0"
                },
                "capabilities": null
            })
        );
        respond(
            &mut server_write,
            &initialize,
            json!({"userAgent": "codex-cli/0.144.5"}),
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

        let resume = read_request(&mut lines).await;
        assert_eq!(resume["method"], "thread/resume");
        assert_eq!(resume["params"], json!({"threadId": "t-1"}));
        respond(
            &mut server_write,
            &resume,
            json!({"thread": thread_with_turns()}),
        )
        .await;

        let turn = read_request(&mut lines).await;
        assert_eq!(turn["method"], "turn/start");
        assert_eq!(turn["params"]["threadId"], "t-1");
        assert_eq!(
            turn["params"]["input"],
            json!([{"type": "text", "text": "review this", "text_elements": []}])
        );
        assert_eq!(turn["params"]["outputSchema"]["type"], "object");
        respond(
            &mut server_write,
            &turn,
            json!({"turn": {"id": "turn-2", "status": "inProgress", "items": []}}),
        )
        .await;
    });

    client.initialize().await.unwrap();
    let page = client.list_threads(None, 50).await.unwrap();
    assert_eq!(page.data[0].id, "t-1");
    assert_eq!(page.next_cursor.as_deref(), Some("next-page"));
    let detail = client.read_thread("t-1").await.unwrap();
    assert_eq!(detail.turns.len(), 1);
    client.resume_thread("t-1").await.unwrap();
    let turn = client
        .start_turn("t-1", "review this", json!({"type": "object"}))
        .await
        .unwrap();
    assert_eq!(turn.id, "turn-2");
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
