use semver::Version;
use serde::Deserialize;
use serde_json::Value;

pub const REQUIRED_METHODS: &[&str] = &[
    "initialize",
    "thread/list",
    "thread/read",
    "thread/resume",
    "turn/start",
    "turn/interrupt",
    "command/exec",
    "config/read",
    "config/batchWrite",
    "mcpServerStatus/list",
];

const METHOD_FIXTURE: &str = include_str!("../../../schemas/app-server/supported-methods.json");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatibilityReport {
    pub compatible: bool,
    pub installed_version: Option<String>,
    pub reason_code: Option<String>,
    pub detail: String,
    pub missing_methods: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MethodFixture {
    minimum_version: String,
    required_methods: Vec<String>,
}

pub fn check_compatibility(version_output: &str) -> CompatibilityReport {
    let fixture = match serde_json::from_str::<MethodFixture>(METHOD_FIXTURE) {
        Ok(fixture) => fixture,
        Err(error) => {
            return incompatible(
                None,
                vec![],
                format!("checked-in compatibility fixture is invalid: {error}"),
            );
        }
    };
    let installed = match parse_codex_version(version_output) {
        Some(version) => version,
        None => {
            return incompatible(
                None,
                vec![],
                "could not parse an exact codex-cli semantic version".to_owned(),
            );
        }
    };
    let minimum = match Version::parse(&fixture.minimum_version) {
        Ok(version) => version,
        Err(error) => {
            return incompatible(
                Some(&installed),
                vec![],
                format!("invalid minimum version in compatibility fixture: {error}"),
            );
        }
    };
    if fixture.required_methods != REQUIRED_METHODS {
        return incompatible(
            Some(&installed),
            vec![],
            "checked-in method fixture does not match the compiled protocol contract".to_owned(),
        );
    }
    if installed < minimum {
        return incompatible(
            Some(&installed),
            vec![],
            format!("supported versions are >= {minimum}"),
        );
    }

    CompatibilityReport {
        compatible: true,
        installed_version: Some(installed.to_string()),
        reason_code: None,
        detail: format!("codex-cli {installed} satisfies the v1 compatibility gate"),
        missing_methods: vec![],
    }
}

pub fn parse_codex_version(output: &str) -> Option<Version> {
    let trimmed = output.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        for key in ["cliVersion", "cli_version", "version"] {
            if let Some(version) = value.get(key).and_then(Value::as_str) {
                if let Ok(version) = Version::parse(version.trim_start_matches('v')) {
                    return Some(version);
                }
            }
        }
    }

    if let Some(version) = trimmed.strip_prefix("codex-cli/") {
        return Version::parse(version.trim_start_matches('v')).ok();
    }
    let mut fields = trimmed.split_whitespace();
    (fields.next()? == "codex-cli")
        .then(|| fields.next())
        .flatten()
        .and_then(|version| Version::parse(version.trim_start_matches('v')).ok())
}

pub fn parse_managed_user_agent(user_agent: &str) -> Option<Version> {
    if user_agent.trim() != user_agent {
        return None;
    }
    if let Some(version) = user_agent.strip_prefix("codex-cli/") {
        return Version::parse(version.trim_start_matches('v')).ok();
    }
    for prefix in ["Codex Desktop/", "worktree-merge-consensus/"] {
        if let Some(identity) = user_agent.strip_prefix(prefix) {
            return parse_versioned_managed_identity(identity);
        }
    }
    None
}

fn parse_versioned_managed_identity(identity: &str) -> Option<Version> {
    let (version, metadata) = identity.split_once(' ')?;
    if !metadata.starts_with('(') || !metadata.ends_with(')') || metadata.contains(['\r', '\n']) {
        return None;
    }
    Version::parse(version.trim_start_matches('v')).ok()
}

fn incompatible(
    installed: Option<&Version>,
    missing_methods: Vec<String>,
    detail: String,
) -> CompatibilityReport {
    CompatibilityReport {
        compatible: false,
        installed_version: installed.map(ToString::to_string),
        reason_code: Some("INCOMPATIBLE_CODEX".to_owned()),
        detail,
        missing_methods,
    }
}
