#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

use app_server_client::{CodexAppServer, client::ConnectOptions};

#[tokio::test]
async fn compatible_binary_starts_daemon_then_proxy_and_initializes() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("calls.log");
    let binary = fake_codex(temp.path(), &log, "0.144.1");

    let client = CodexAppServer::connect(ConnectOptions {
        codex_binary: binary,
        control_socket: None,
        start_daemon: true,
    })
    .await
    .unwrap();

    let calls = fs::read_to_string(&log).unwrap();
    assert!(calls.lines().next().unwrap().contains("--version"));
    assert!(calls.contains("app-server daemon start"));
    assert!(calls.contains("app-server proxy"));
    drop(client);
}

#[tokio::test]
async fn incompatible_binary_never_starts_daemon_or_proxy() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("calls.log");
    let binary = fake_codex(temp.path(), &log, "0.144.0");

    let error = CodexAppServer::connect(ConnectOptions {
        codex_binary: binary,
        control_socket: None,
        start_daemon: true,
    })
    .await
    .unwrap_err();

    assert!(error.to_string().contains("INCOMPATIBLE_CODEX"));
    let calls = fs::read_to_string(&log).unwrap();
    assert_eq!(calls.lines().count(), 1);
    assert!(calls.contains("--version"));
}

#[tokio::test]
async fn mismatched_managed_app_server_identity_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("calls.log");
    let binary = fake_codex_with_server_version(temp.path(), &log, "0.144.5", "0.144.6");

    let error = CodexAppServer::connect(ConnectOptions {
        codex_binary: binary,
        control_socket: None,
        start_daemon: true,
    })
    .await
    .unwrap_err();

    assert!(error.to_string().contains("INCOMPATIBLE_CODEX"));
    assert!(error.to_string().contains("userAgent"));
}

#[tokio::test]
async fn desktop_managed_app_server_identity_is_accepted_end_to_end() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("calls.log");
    let binary = fake_codex_with_user_agent(
        temp.path(),
        &log,
        "0.144.1",
        "Codex Desktop/0.144.1 (Mac OS 26.2.0; arm64) dumb (test-client; 0.1.0)",
    );

    CodexAppServer::connect(ConnectOptions {
        codex_binary: binary,
        control_socket: None,
        start_daemon: true,
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cli_version_output_shape_is_rejected_as_a_managed_user_agent() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("calls.log");
    let binary = fake_codex_with_user_agent(temp.path(), &log, "0.144.5", "codex-cli 0.144.5");

    let error = CodexAppServer::connect(ConnectOptions {
        codex_binary: binary,
        control_socket: None,
        start_daemon: true,
    })
    .await
    .unwrap_err();

    assert!(error.to_string().contains("INCOMPATIBLE_CODEX"));
    assert!(error.to_string().contains("userAgent"));
}

#[tokio::test]
async fn silent_initialize_is_bounded_after_a_successful_websocket_upgrade() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("calls.log");
    let binary = fake_codex_with_user_agent_and_mode(
        temp.path(),
        &log,
        "0.144.5",
        "codex-cli/0.144.5",
        "silent",
    );

    let result = tokio::time::timeout(
        Duration::from_secs(11),
        CodexAppServer::connect(ConnectOptions {
            codex_binary: binary,
            control_socket: None,
            start_daemon: true,
        }),
    )
    .await
    .expect("the client must enforce its own initialize timeout");
    let error = result.unwrap_err();

    assert!(error.to_string().contains("initialize"));
    assert!(error.to_string().contains("timed out"));
}

#[tokio::test]
async fn unsupported_websocket_frame_reports_the_protocol_cause() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("calls.log");
    let binary = fake_codex_with_user_agent_and_mode(
        temp.path(),
        &log,
        "0.144.5",
        "codex-cli/0.144.5",
        "binary",
    );

    let error = CodexAppServer::connect(ConnectOptions {
        codex_binary: binary,
        control_socket: None,
        start_daemon: true,
    })
    .await
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("unsupported binary WebSocket frame")
    );
}

fn fake_codex(directory: &Path, log: &Path, version: &str) -> std::path::PathBuf {
    fake_codex_with_server_version(directory, log, version, version)
}

fn fake_codex_with_server_version(
    directory: &Path,
    log: &Path,
    version: &str,
    server_version: &str,
) -> std::path::PathBuf {
    fake_codex_with_user_agent(
        directory,
        log,
        version,
        &format!("codex-cli/{server_version}"),
    )
}

fn fake_codex_with_user_agent(
    directory: &Path,
    log: &Path,
    version: &str,
    user_agent: &str,
) -> std::path::PathBuf {
    fake_codex_with_user_agent_and_mode(directory, log, version, user_agent, "respond")
}

fn fake_codex_with_user_agent_and_mode(
    directory: &Path,
    log: &Path,
    version: &str,
    user_agent: &str,
    mode: &str,
) -> std::path::PathBuf {
    let binary = directory.join("codex");
    let proxy = directory.join("fake_proxy.py");
    fs::write(&proxy, FAKE_WEBSOCKET_PROXY).unwrap();
    let script = format!(
        r#"#!/bin/sh
LOG='{}'
PROXY='{}'
printf '%s\n' "$*" >> "$LOG"
if [ "$1" = "--version" ]; then
  printf 'codex-cli {}\n'
  exit 0
fi
if [ "$1 $2 $3" = "app-server daemon start" ]; then
  exit 0
fi
if [ "$1 $2" = "app-server proxy" ]; then
  exec /usr/bin/env python3 "$PROXY" '{user_agent}' '{mode}'
fi
exit 2
"#,
        log.display(),
        proxy.display(),
        version,
        user_agent = user_agent,
        mode = mode,
    );
    fs::write(&binary, script).unwrap();
    let mut permissions = fs::metadata(&binary).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&binary, permissions).unwrap();
    binary
}

const FAKE_WEBSOCKET_PROXY: &str = r#"import base64
import hashlib
import json
import struct
import sys


def read_exact(length):
    data = bytearray()
    while len(data) < length:
        chunk = sys.stdin.buffer.read(length - len(data))
        if not chunk:
            raise EOFError
        data.extend(chunk)
    return bytes(data)


def send_frame(opcode, payload):
    length = len(payload)
    if length < 126:
        header = bytes([0x80 | opcode, length])
    elif length <= 0xFFFF:
        header = bytes([0x80 | opcode, 126]) + struct.pack('!H', length)
    else:
        header = bytes([0x80 | opcode, 127]) + struct.pack('!Q', length)
    sys.stdout.buffer.write(header + payload)
    sys.stdout.buffer.flush()


headers = bytearray()
while b'\r\n\r\n' not in headers:
    headers.extend(read_exact(1))
key = None
for line in headers.decode('ascii').split('\r\n'):
    name, separator, value = line.partition(':')
    if separator and name.lower() == 'sec-websocket-key':
        key = value.strip()
        break
if key is None:
    raise RuntimeError('missing Sec-WebSocket-Key')
accept = base64.b64encode(
    hashlib.sha1((key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode('ascii')).digest()
).decode('ascii')
sys.stdout.buffer.write(
    ('HTTP/1.1 101 Switching Protocols\r\n'
     'Upgrade: websocket\r\n'
     'Connection: Upgrade\r\n'
     f'Sec-WebSocket-Accept: {accept}\r\n\r\n').encode('ascii')
)
sys.stdout.buffer.flush()

user_agent = sys.argv[1]
mode = sys.argv[2]
while True:
    try:
        first, second = read_exact(2)
    except EOFError:
        break
    opcode = first & 0x0F
    length = second & 0x7F
    if length == 126:
        length = struct.unpack('!H', read_exact(2))[0]
    elif length == 127:
        length = struct.unpack('!Q', read_exact(8))[0]
    mask = read_exact(4) if second & 0x80 else None
    payload = bytearray(read_exact(length))
    if mask is not None:
        for index in range(length):
            payload[index] ^= mask[index % 4]
    if opcode == 8:
        break
    if opcode == 9:
        send_frame(10, bytes(payload))
        continue
    if opcode != 1:
        continue
    request = json.loads(payload.decode('utf-8'))
    if request.get('method') != 'initialize' or 'id' not in request:
        continue
    if mode == 'silent':
        continue
    if mode == 'binary':
        send_frame(2, b'not-json')
        continue
    response = {
        'id': request['id'],
        'result': {
            'codexHome': '/tmp/fake-codex-home',
            'platformFamily': 'unix',
            'platformOs': 'linux',
            'userAgent': user_agent,
        },
    }
    send_frame(1, json.dumps(response, separators=(',', ':')).encode('utf-8'))
"#;
