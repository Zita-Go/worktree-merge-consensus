use app_server_client::compat::{REQUIRED_METHODS, check_compatibility, parse_managed_user_agent};
use serde_json::Value;

#[test]
fn minimum_supported_codex_version_passes() {
    let report = check_compatibility("codex-cli 0.144.1");

    assert!(report.compatible);
    assert_eq!(report.installed_version.as_deref(), Some("0.144.1"));
}

#[test]
fn desktop_app_server_user_agent_exposes_exact_codex_version() {
    let version = parse_managed_user_agent(
        "Codex Desktop/0.144.1 (Mac OS 26.2.0; arm64) dumb (worktree-merge-consensus; 0.1.0)",
    );

    assert_eq!(
        version.map(|version| version.to_string()).as_deref(),
        Some("0.144.1")
    );
}

#[test]
fn cli_app_server_user_agent_exposes_exact_codex_version() {
    let version = parse_managed_user_agent(
        "worktree-merge-consensus/0.144.6 (Debian 12.0.0; x86_64) unknown (worktree-merge-consensus; 0.1.24)",
    );

    assert_eq!(
        version.map(|version| version.to_string()).as_deref(),
        Some("0.144.6")
    );
}

#[test]
fn unrelated_cli_identity_is_rejected() {
    assert!(
        parse_managed_user_agent(
            "unrelated-client/0.144.6 (Debian 12.0.0; x86_64) unknown (unrelated-client; 1.0.0)",
        )
        .is_none()
    );
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
            "turn/start",
            "turn/interrupt",
            "config/read",
            "config/batchWrite"
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
fn future_codex_versions_have_no_version_ceiling() {
    let report = check_compatibility("codex-cli 1.0.0");

    assert!(report.compatible);
    assert_eq!(report.installed_version.as_deref(), Some("1.0.0"));
}

#[test]
fn version_below_minimum_is_rejected() {
    let report = check_compatibility("codex-cli 0.144.0");

    assert!(!report.compatible);
    assert_eq!(report.reason_code.as_deref(), Some("INCOMPATIBLE_CODEX"));
}

#[test]
fn prerelease_of_minimum_version_is_rejected() {
    let report = check_compatibility("codex-cli 0.144.1-beta.1");

    assert!(!report.compatible);
    assert_eq!(report.reason_code.as_deref(), Some("INCOMPATIBLE_CODEX"));
}

#[test]
fn pinned_fixture_distinguishes_integration_and_verification_write_roots() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../schemas/app-server/supported-methods.json"
    ))
    .unwrap();
    let profiles = &fixture["turnPolicyProfiles"];

    assert_eq!(fixture["minimumVersion"], "0.144.1");
    assert!(fixture.get("maximumVersionExclusive").is_none());
    assert_eq!(fixture["initializeCapabilities"]["experimentalApi"], true);
    assert_eq!(
        fixture["turnLifecycle"]["requiredBeforeEveryTurnStart"],
        serde_json::json!(["thread/resume"])
    );
    assert_eq!(
        fixture["responseValidation"],
        serde_json::json!({
            "transportOutputSchema": false,
            "localProtocolSchema": "../protocol-v1.json",
            "failClosed": true
        })
    );
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
