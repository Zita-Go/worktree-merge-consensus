use consensus_core::{
    canonical_json_hash,
    protocol::{MessageType, validate_message},
};
use serde_json::json;

fn valid_approval() -> serde_json::Value {
    json!({
        "protocol": "worktree-merge-consensus/v1",
        "run_id": "4b230bd8-d870-4ef4-bf20-05a4c61020af",
        "message_type": "APPROVED_PLAN",
        "phase": "PLAN_REVIEW",
        "round": 1,
        "primary_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "reviewer_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "plan_revision": 1,
        "integration_branch": null,
        "integration_sha": null,
        "reason_code": null,
        "payload": {
            "approved_plan_revision": 1,
            "approved_primary_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "approved_reviewer_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "uncovered_items": []
        }
    })
}

#[test]
fn approval_requires_exact_nonempty_source_shas() {
    let parsed = validate_message(valid_approval()).expect("valid approval");
    assert_eq!(parsed.envelope.message_type, MessageType::ApprovedPlan);
}

#[test]
fn natural_language_is_not_a_protocol_message() {
    let error = validate_message(json!("looks good")).unwrap_err();
    assert!(error.to_string().contains("JSON object"));
}

#[test]
fn canonical_hash_ignores_object_key_order() {
    let first = json!({"a": 1, "nested": {"x": true, "y": false}});
    let second = json!({"nested": {"y": false, "x": true}, "a": 1});

    assert_eq!(canonical_json_hash(&first), canonical_json_hash(&second));
}

#[test]
fn invalid_sha_is_rejected() {
    let mut value = valid_approval();
    value["primary_sha"] = json!("not-a-sha");

    assert!(validate_message(value).is_err());
}

#[test]
fn zero_round_is_rejected() {
    let mut value = valid_approval();
    value["round"] = json!(0);

    assert!(validate_message(value).is_err());
}

#[test]
fn plan_approval_requires_plan_revision() {
    let mut value = valid_approval();
    value["plan_revision"] = serde_json::Value::Null;

    assert!(validate_message(value).is_err());
}

#[test]
fn plan_approval_payload_must_match_envelope() {
    let mut value = valid_approval();
    value["payload"]["approved_primary_sha"] = json!("cccccccccccccccccccccccccccccccccccccccc");

    assert!(validate_message(value).is_err());
}

#[test]
fn plan_approval_cannot_leave_uncovered_items() {
    let mut value = valid_approval();
    value["payload"]["uncovered_items"] = json!(["reviewer test coverage"]);

    assert!(validate_message(value).is_err());
}
