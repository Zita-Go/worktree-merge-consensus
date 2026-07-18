use serde::Serialize;
use serde_json::{Value, json};

use crate::CliError;

pub fn emit_value(value: &Value, json_output: bool, human: impl FnOnce() -> String) {
    if json_output {
        println!(
            "{}",
            serde_json::to_string(value).expect("CLI JSON values are serializable")
        );
    } else {
        println!("{}", human());
    }
}

pub fn emit_serializable(
    value: &impl Serialize,
    json_output: bool,
    human: impl FnOnce() -> String,
) -> Result<(), CliError> {
    let value = serde_json::to_value(value)
        .map_err(|error| CliError::new("SERIALIZATION_FAILURE", error.to_string()))?;
    emit_value(&value, json_output, human);
    Ok(())
}

pub fn emit_error(error: &CliError, json_output: bool) {
    if json_output {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "ok": false,
                "error": {"code": error.code(), "message": error.to_string()}
            }))
            .expect("CLI errors are serializable")
        );
    } else {
        eprintln!("{}", error);
    }
}

pub fn human_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}
