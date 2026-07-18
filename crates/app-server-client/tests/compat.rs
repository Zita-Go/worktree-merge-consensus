use app_server_client::compat::{REQUIRED_METHODS, check_compatibility};

#[test]
fn first_supported_codex_version_passes() {
    let report = check_compatibility("codex-cli 0.144.5", REQUIRED_METHODS);

    assert!(report.compatible);
    assert_eq!(report.installed_version.as_deref(), Some("0.144.5"));
}

#[test]
fn missing_turn_start_fails_closed() {
    let methods = REQUIRED_METHODS
        .iter()
        .copied()
        .filter(|method| *method != "turn/start")
        .collect::<Vec<_>>();

    let report = check_compatibility("codex-cli 0.144.5", &methods);

    assert!(!report.compatible);
    assert_eq!(report.reason_code.as_deref(), Some("INCOMPATIBLE_CODEX"));
    assert_eq!(report.missing_methods, vec!["turn/start"]);
}

#[test]
fn malformed_version_output_fails_closed() {
    let report = check_compatibility("Codex, probably recent", REQUIRED_METHODS);

    assert!(!report.compatible);
    assert_eq!(report.reason_code.as_deref(), Some("INCOMPATIBLE_CODEX"));
}

#[test]
fn unknown_future_protocol_version_fails_closed() {
    let report = check_compatibility("codex-cli 0.145.0", REQUIRED_METHODS);

    assert!(!report.compatible);
    assert_eq!(report.reason_code.as_deref(), Some("INCOMPATIBLE_CODEX"));
}

#[test]
fn older_patch_release_is_not_silently_accepted() {
    let report = check_compatibility("codex-cli 0.144.1", REQUIRED_METHODS);

    assert!(!report.compatible);
    assert_eq!(report.reason_code.as_deref(), Some("INCOMPATIBLE_CODEX"));
}
