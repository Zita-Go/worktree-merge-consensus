#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::Path};

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

fn fake_codex(directory: &Path, log: &Path, version: &str) -> std::path::PathBuf {
    fake_codex_with_server_version(directory, log, version, version)
}

fn fake_codex_with_server_version(
    directory: &Path,
    log: &Path,
    version: &str,
    server_version: &str,
) -> std::path::PathBuf {
    let binary = directory.join("codex");
    let script = format!(
        r#"#!/bin/sh
LOG='{}'
printf '%s\n' "$*" >> "$LOG"
if [ "$1" = "--version" ]; then
  printf 'codex-cli {}\n'
  exit 0
fi
if [ "$1 $2 $3" = "app-server daemon start" ]; then
  exit 0
fi
if [ "$1 $2" = "app-server proxy" ]; then
  while IFS= read -r line; do
    case "$line" in
      *'"method":"initialize"'*)
        printf '{{"jsonrpc":"2.0","id":1,"result":{{"codexHome":"/tmp/fake-codex-home","platformFamily":"unix","platformOs":"linux","userAgent":"codex-cli/{server_version}"}}}}\n'
        ;;
    esac
  done
  exit 0
fi
exit 2
"#,
        log.display(),
        version,
        server_version = server_version
    );
    fs::write(&binary, script).unwrap();
    let mut permissions = fs::metadata(&binary).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&binary, permissions).unwrap();
    binary
}
