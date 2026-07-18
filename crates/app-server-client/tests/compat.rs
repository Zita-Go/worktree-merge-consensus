use app_server_client::compat::{REQUIRED_METHODS, check_compatibility};
use serde_json::Value;

#[test]
fn first_supported_codex_version_passes() {
    let report = check_compatibility("codex-cli 0.144.5");

    assert!(report.compatible);
    assert_eq!(report.installed_version.as_deref(), Some("0.144.5"));
}

#[test]
fn required_method_contract_is_explicit_and_pinned() {
    assert_eq!(
        REQUIRED_METHODS,
        [
            "initialize",
            "thread/list",
            "thread/read",
            "thread/resume",
            "turn/start"
        ]
    );
}

#[test]
fn malformed_version_output_fails_closed() {
    let report = check_compatibility("Codex, probably recent");

    assert!(!report.compatible);
    assert_eq!(report.reason_code.as_deref(), Some("INCOMPATIBLE_CODEX"));
}

#[test]
fn unknown_future_protocol_version_fails_closed() {
    let report = check_compatibility("codex-cli 0.145.0");

    assert!(!report.compatible);
    assert_eq!(report.reason_code.as_deref(), Some("INCOMPATIBLE_CODEX"));
}

#[test]
fn older_patch_release_is_not_silently_accepted() {
    let report = check_compatibility("codex-cli 0.144.1");

    assert!(!report.compatible);
    assert_eq!(report.reason_code.as_deref(), Some("INCOMPATIBLE_CODEX"));
}

#[test]
fn pinned_fixture_distinguishes_integration_and_verification_write_roots() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../schemas/app-server/0.144.5-methods.json"
    ))
    .unwrap();
    let profiles = &fixture["turnPolicyProfiles"];

    assert_eq!(
        profiles["primaryIntegrationWorkspaceWrite"]["writableRootRoles"],
        serde_json::json!(["primaryWorktree", "sourceGitCommonDirectory"])
    );
    assert_eq!(
        profiles["primaryVerificationWorkspaceWrite"]["writableRootRoles"],
        serde_json::json!(["isolatedVerificationClone"])
    );
    assert_eq!(
        fixture["commandExecutionEvidenceShape"]["successfulExitCode"],
        0
    );
}
