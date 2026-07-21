use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub const MCP_TOOL_NAMES: [&str; 8] = [
    "consensus_doctor",
    "consensus_list_threads",
    "consensus_list_worktrees",
    "consensus_start",
    "consensus_status",
    "consensus_resume",
    "consensus_apply_patch",
    "consensus_cancel",
];

pub fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            MCP_TOOL_NAMES[0],
            "Check Git, Codex App Server, daemon, and local state compatibility.",
            empty_schema(),
        ),
        tool(
            MCP_TOOL_NAMES[1],
            "List existing Codex tasks visible on this host.",
            empty_schema(),
        ),
        tool(
            MCP_TOOL_NAMES[2],
            "List registered Git worktrees for one repository without modifying them.",
            json!({
                "type": "object",
                "properties": {
                    "repository_path": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Absolute path to any worktree in the source repository."
                    }
                },
                "required": ["repository_path"],
                "additionalProperties": false
            }),
        ),
        tool(
            MCP_TOOL_NAMES[3],
            "Start reviewed integration between two existing tasks and return immediately.",
            json!({
                "type": "object",
                "properties": {
                    "primary_thread": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Existing task that owns integration writes."
                    },
                    "reviewer_thread": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Existing task that protects its implementation details."
                    },
                    "primary_worktree": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Absolute registered worktree containing the primary implementation."
                    },
                    "reviewer_worktree": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Absolute registered worktree containing the reviewer implementation."
                    },
                    "integration_branch": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Optional unique name for the new local integration branch."
                    },
                    "test_commands": {
                        "type": "array",
                        "items": {"type": "string", "minLength": 1},
                        "description": "Additional verification commands for the primary task."
                    }
                },
                "required": [
                    "primary_thread",
                    "reviewer_thread",
                    "primary_worktree",
                    "reviewer_worktree"
                ],
                "additionalProperties": false
            }),
        ),
        tool(
            MCP_TOOL_NAMES[4],
            "Show one consensus run, or list all runs when run_id is omitted.",
            json!({
                "type": "object",
                "properties": {
                    "run_id": {"type": "string", "minLength": 1}
                },
                "required": [],
                "additionalProperties": false
            }),
        ),
        tool(
            MCP_TOOL_NAMES[5],
            "Resume a paused consensus run after its blocking condition is resolved.",
            run_id_schema(),
        ),
        tool(
            MCP_TOOL_NAMES[6],
            "Apply one text-only patch during the exact active primary integration turn.",
            json!({
                "type": "object",
                "properties": {
                    "run_id": {"type": "string", "minLength": 1},
                    "request_hash": {"type": "string", "minLength": 1},
                    "patch": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 524288,
                        "description": "One raw unified text patch for the authorized clean integration branch."
                    }
                },
                "required": ["run_id", "request_hash", "patch"],
                "additionalProperties": false
            }),
        ),
        tool(
            MCP_TOOL_NAMES[7],
            "Cancel a consensus run without reverting or deleting Git state.",
            run_id_schema(),
        ),
    ]
}

pub(crate) fn validate_arguments(name: &str, arguments: Value) -> Result<Value, String> {
    match name {
        "consensus_doctor" | "consensus_list_threads" => {
            let parsed: EmptyArguments = parse(arguments)?;
            serde_json::to_value(parsed).map_err(|error| error.to_string())
        }
        "consensus_list_worktrees" => {
            let parsed: WorktreeListArguments = parse(arguments)?;
            parsed.validate()?;
            serde_json::to_value(parsed).map_err(|error| error.to_string())
        }
        "consensus_start" => {
            let parsed: StartArguments = parse(arguments)?;
            parsed.validate()?;
            serde_json::to_value(parsed).map_err(|error| error.to_string())
        }
        "consensus_status" => {
            let parsed: StatusArguments = parse(arguments)?;
            parsed.validate()?;
            serde_json::to_value(parsed).map_err(|error| error.to_string())
        }
        "consensus_resume" | "consensus_cancel" => {
            let parsed: RunIdArguments = parse(arguments)?;
            parsed.validate()?;
            serde_json::to_value(parsed).map_err(|error| error.to_string())
        }
        "consensus_apply_patch" => {
            let parsed: ApplyPatchArguments = parse(arguments)?;
            parsed.validate()?;
            serde_json::to_value(parsed).map_err(|error| error.to_string())
        }
        _ => Err(format!("unknown tool {name}")),
    }
}

fn parse<T: for<'de> Deserialize<'de>>(arguments: Value) -> Result<T, String> {
    if !arguments.is_object() {
        return Err("tool arguments must be an object".into());
    }
    serde_json::from_value(arguments).map_err(|error| format!("invalid tool arguments: {error}"))
}

fn nonempty(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{field} must be a non-empty string"))
    } else {
        Ok(())
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArguments {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorktreeListArguments {
    repository_path: String,
}

impl WorktreeListArguments {
    fn validate(&self) -> Result<(), String> {
        nonempty("repository_path", &self.repository_path)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartArguments {
    primary_thread: String,
    reviewer_thread: String,
    primary_worktree: String,
    reviewer_worktree: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    integration_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    test_commands: Vec<String>,
}

impl StartArguments {
    fn validate(&self) -> Result<(), String> {
        nonempty("primary_thread", &self.primary_thread)?;
        nonempty("reviewer_thread", &self.reviewer_thread)?;
        if self.primary_thread == self.reviewer_thread {
            return Err("primary_thread and reviewer_thread must differ".into());
        }
        nonempty("primary_worktree", &self.primary_worktree)?;
        nonempty("reviewer_worktree", &self.reviewer_worktree)?;
        if self.primary_worktree == self.reviewer_worktree {
            return Err("primary_worktree and reviewer_worktree must differ".into());
        }
        if let Some(branch) = &self.integration_branch {
            nonempty("integration_branch", branch)?;
        }
        for command in &self.test_commands {
            nonempty("test_commands item", command)?;
        }
        Ok(())
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusArguments {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
}

impl StatusArguments {
    fn validate(&self) -> Result<(), String> {
        if let Some(run_id) = &self.run_id {
            nonempty("run_id", run_id)?;
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunIdArguments {
    run_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyPatchArguments {
    run_id: String,
    request_hash: String,
    patch: String,
}

impl ApplyPatchArguments {
    fn validate(&self) -> Result<(), String> {
        nonempty("run_id", &self.run_id)?;
        nonempty("request_hash", &self.request_hash)?;
        nonempty("patch", &self.patch)?;
        if self.patch.len() > 512 * 1024 {
            return Err("patch exceeds the 524288 byte limit".into());
        }
        Ok(())
    }
}

impl RunIdArguments {
    fn validate(&self) -> Result<(), String> {
        nonempty("run_id", &self.run_id)
    }
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema
    })
}

fn empty_schema() -> Value {
    json!({
        "type": "object",
        "properties": Map::<String, Value>::new(),
        "required": [],
        "additionalProperties": false
    })
}

fn run_id_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "run_id": {"type": "string", "minLength": 1}
        },
        "required": ["run_id"],
        "additionalProperties": false
    })
}
