use app_server_client::transport::{JsonRpcTransport, RpcError};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf, duplex, split};

#[tokio::test]
async fn correlates_out_of_order_responses_and_keeps_notifications() {
    let (client_side, server_side) = duplex(64 * 1024);
    let (client_read, client_write) = split(client_side);
    let client = JsonRpcTransport::new(client_read, client_write);
    let (server_read, mut server_write) = split(server_side);
    let mut server_read = BufReader::new(server_read).lines();

    let first = tokio::spawn({
        let client = client.clone();
        async move { client.request("thread/list", json!({"limit": 50})).await }
    });
    let second = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .request(
                    "thread/read",
                    json!({"threadId": "t-1", "includeTurns": true}),
                )
                .await
        }
    });

    let request_a: Value = serde_json::from_str(&server_read.next_line().await.unwrap().unwrap())
        .expect("first request JSON");
    let request_b: Value = serde_json::from_str(&server_read.next_line().await.unwrap().unwrap())
        .expect("second request JSON");
    let (list, read) = if request_a["method"] == "thread/list" {
        (request_a, request_b)
    } else {
        (request_b, request_a)
    };

    send_json(
        &mut server_write,
        &json!({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": {"threadId": "t-1", "turn": {"id": "turn-1"}}
        }),
    )
    .await;
    send_json(
        &mut server_write,
        &json!({
            "jsonrpc": "2.0",
            "id": read["id"],
            "result": {"thread": {"id": "t-1"}}
        }),
    )
    .await;
    send_json(
        &mut server_write,
        &json!({
            "jsonrpc": "2.0",
            "id": list["id"],
            "result": {"data": []}
        }),
    )
    .await;

    assert_eq!(second.await.unwrap().unwrap()["thread"]["id"], "t-1");
    assert!(first.await.unwrap().unwrap()["data"].is_array());
    assert_eq!(client.next_event().await.unwrap().method, "turn/completed");
}

#[tokio::test]
async fn json_rpc_errors_are_returned_to_the_matching_request() {
    let (client_side, server_side) = duplex(16 * 1024);
    let (client_read, client_write) = split(client_side);
    let client = JsonRpcTransport::new(client_read, client_write);
    let (server_read, mut server_write) = split(server_side);
    let mut server_read = BufReader::new(server_read).lines();

    let request = tokio::spawn({
        let client = client.clone();
        async move { client.request("thread/read", json!({})).await }
    });
    let wire: Value =
        serde_json::from_str(&server_read.next_line().await.unwrap().unwrap()).unwrap();
    send_json(
        &mut server_write,
        &json!({
            "jsonrpc": "2.0",
            "id": wire["id"],
            "error": {"code": -32602, "message": "invalid params"}
        }),
    )
    .await;

    let error = request.await.unwrap().unwrap_err();
    assert!(matches!(error, RpcError::Remote { code: -32602, .. }));
}

async fn send_json(writer: &mut WriteHalf<tokio::io::DuplexStream>, value: &Value) {
    writer
        .write_all(format!("{}\n", serde_json::to_string(value).unwrap()).as_bytes())
        .await
        .unwrap();
    writer.flush().await.unwrap();
}

#[allow(dead_code)]
fn assert_transport_bounds(
    _reader: ReadHalf<tokio::io::DuplexStream>,
    _writer: WriteHalf<tokio::io::DuplexStream>,
) {
}
