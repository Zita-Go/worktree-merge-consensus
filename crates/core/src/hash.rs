use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Hash a JSON value after recursively sorting every object by key.
pub fn canonical_json_hash(value: &Value) -> String {
    let canonical = canonicalize(value);
    let encoded = serde_json::to_vec(&canonical)
        .expect("serializing an in-memory serde_json::Value cannot fail");
    hex::encode(Sha256::digest(encoded))
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries = object.iter().collect::<Vec<_>>();
            entries.sort_unstable_by_key(|(key, _)| *key);

            let mut canonical = Map::new();
            for (key, value) in entries {
                canonical.insert(key.clone(), canonicalize(value));
            }
            Value::Object(canonical)
        }
        Value::Array(values) => Value::Array(values.iter().map(canonicalize).collect()),
        scalar => scalar.clone(),
    }
}
