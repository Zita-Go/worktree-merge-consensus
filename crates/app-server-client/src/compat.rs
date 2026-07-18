use std::collections::BTreeSet;

use semver::Version;
use serde::Deserialize;
use serde_json::Value;

pub const REQUIRED_METHODS: &[&str] = &[
    "initialize",
    "thread/list",
    "thread/read",
    "thread/resume",
    "turn/start",
];

const METHOD_FIXTURE: &str = include_str!("../../../schemas/app-server/0.144.5-methods.json");

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
    maximum_version_exclusive: String,
    required_methods: Vec<String>,
}

pub fn check_compatibility(
    version_output: &str,
    available_methods: &[&str],
) -> CompatibilityReport {
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
    let maximum = match Version::parse(&fixture.maximum_version_exclusive) {
        Ok(version) => version,
        Err(error) => {
            return incompatible(
                Some(&installed),
                vec![],
                format!("invalid maximum version in compatibility fixture: {error}"),
            );
        }
    };

    let advertised = available_methods.iter().copied().collect::<BTreeSet<_>>();
    let missing_methods = fixture
        .required_methods
        .iter()
        .filter(|method| !advertised.contains(method.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !missing_methods.is_empty() {
        return incompatible(
            Some(&installed),
            missing_methods,
            "one or more required App Server methods are unavailable".to_owned(),
        );
    }
    if installed < minimum || installed >= maximum {
        return incompatible(
            Some(&installed),
            vec![],
            format!(
                "supported range is >= {minimum} and < {maximum}; unknown protocols fail closed"
            ),
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

    let mut fields = trimmed.split_whitespace();
    if fields.next()? != "codex-cli" {
        return None;
    }
    Version::parse(fields.next()?.trim_start_matches('v')).ok()
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
