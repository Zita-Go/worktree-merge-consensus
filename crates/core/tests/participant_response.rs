use consensus_core::participant::{ParticipantSignal, parse_participant_response};

#[test]
fn extracts_one_marker_and_preserves_free_markdown() {
    let parsed = parse_participant_response(
        "Intro that is not machine parsed.\n\n<consensus-result>CHANGES_REQUIRED</consensus-result>\n\n- Preserve empty input\n- Keep retry behavior",
        &[ParticipantSignal::Approved, ParticipantSignal::ChangesRequired],
    )
    .unwrap();

    assert_eq!(parsed.signal, ParticipantSignal::ChangesRequired);
    assert_eq!(parsed.blocked_reason, None);
    assert!(parsed.body.contains("Intro that is not machine parsed."));
    assert!(parsed.body.contains("Preserve empty input"));
}

#[test]
fn accepts_an_optional_machine_reason_only_for_blocked() {
    let parsed = parse_participant_response(
        "<consensus-result>BLOCKED:SOURCE_BINDING_MISMATCH</consensus-result>\nThe bound worktree does not match this task.",
        &[ParticipantSignal::ContractReady, ParticipantSignal::Blocked],
    )
    .unwrap();

    assert_eq!(parsed.signal, ParticipantSignal::Blocked);
    assert_eq!(
        parsed.blocked_reason.as_deref(),
        Some("SOURCE_BINDING_MISMATCH")
    );
}

#[test]
fn rejects_missing_duplicate_unknown_and_wrong_action_markers() {
    for text in [
        "APPROVED",
        "<consensus-result>APPROVED</consensus-result><consensus-result>APPROVED</consensus-result>",
        "<consensus-result>MAYBE</consensus-result>",
        "<consensus-result>APPROVED:EXTRA</consensus-result>",
    ] {
        assert!(
            parse_participant_response(text, &[ParticipantSignal::Approved]).is_err(),
            "unexpectedly accepted {text:?}"
        );
    }

    assert!(
        parse_participant_response(
            "<consensus-result>PLAN_READY</consensus-result>\nPlan",
            &[
                ParticipantSignal::Approved,
                ParticipantSignal::ChangesRequired
            ],
        )
        .is_err()
    );
}
