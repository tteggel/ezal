//! Real wire parsing and the device mutation boundary, without an HTTP mock.
use ezal_core::coordination::CoordinationState;
use ezal_core::protocol::{ClientMessage, TrackingIntent, TrackingRejection};
use ezal_core::session::CommandFreshness;
use ezal_core::tracking::{LifecycleError, PassEvent, RuntimeError, TrackingAction};

const PASS: PassEvent = PassEvent {
    norad_id: 39260,
    aos_unix_s: 1_791_385_770,
    los_unix_s: 1_791_385_800,
};
fn parse(text: &str) -> TrackingIntent {
    match ClientMessage::parse(text).unwrap() {
        ClientMessage::Tracking(intent) => intent,
        other => panic!("not a tracking intent: {other:?}"),
    }
}
fn setup() -> (CoordinationState, u32, CommandFreshness, TrackingIntent) {
    let mut state = CoordinationState::new();
    state.publish_network(true, 1000);
    let owner = state.register_client(1000);
    let mut freshness = CommandFreshness::new(owner);
    let proof = freshness.issue(1000).unwrap().token;
    let lease = state.operator(owner, 1000).unwrap().token.generation();
    let intent = parse(&format!(
        "track:{proof}:{lease}:1:0:select:{}:{}:{}",
        PASS.norad_id, PASS.aos_unix_s, PASS.los_unix_s
    ));
    (state, owner, freshness, intent)
}

#[test]
fn strict_bounded_wire_preserves_identifiers_and_rejects_extra_intent() {
    let intent = parse("track:18446744073709551615:18446744073709551615:18446744073709551615:18446744073709551615:arm");
    assert_eq!(intent.proof, u64::MAX);
    assert_eq!(intent.lease, u64::MAX);
    assert_eq!(intent.sequence, u64::MAX);
    assert_eq!(intent.revision, u64::MAX);
    for action in ["arm", "prepare", "cancel", "manual"] {
        assert!(matches!(
            ClientMessage::parse(&format!("track:1:2:3:4:{action}")),
            Some(ClientMessage::Tracking(_))
        ));
    }
    for text in [
        "track:1:2:3:4:arm:select",
        "track:1:2:3:4:cancel:",
        "track:1:2:0:4:arm",
        "track:1:0:3:4:arm",
        "track:1:2:3:4:select:1:100",
        "track:1:2:3:4:select:1:100:-1",
        "track:+1:2:3:4:arm",
        "track:1:2: 3:4:arm",
        "track:1:2:18446744073709551616:4:arm",
        "track:1:2:3:4:follow",
        "track:1:2:3:4:select:1:100:200:300",
    ] {
        assert_eq!(ClientMessage::parse(text), None, "{text}");
    }
}

#[test]
fn only_current_owner_can_select_and_forged_forecast_never_replaces_selection() {
    let (mut state, owner, freshness, intent) = setup();
    let viewer = state.register_client(1000);
    assert_eq!(
        state
            .request_tracking(viewer, intent, &freshness, 1000, |_| panic!(
                "viewer must be rejected first"
            ))
            .result,
        Err(TrackingRejection::Lease)
    );
    assert!(!state.take_control(viewer, 1000));
    assert!(state
        .request_tracking(owner, intent, &freshness, 1000, |p| p == PASS)
        .result
        .is_ok());
    let selected = state.tracking().snapshot();
    let replacement = TrackingIntent {
        sequence: 2,
        revision: selected.revision,
        action: TrackingAction::Select(PassEvent {
            norad_id: 1,
            ..PASS
        }),
        ..intent
    };
    assert_eq!(
        state
            .request_tracking(owner, replacement, &freshness, 1000, |_| false)
            .result,
        Err(TrackingRejection::ForecastChanged)
    );
    assert_eq!(state.tracking().snapshot(), selected);
    // Exact retries are idempotent while their observation is valid.
    assert!(state
        .request_tracking(owner, intent, &freshness, 1000, |_| true)
        .result
        .is_ok());
    assert_eq!(state.tracking().snapshot(), selected);
    let reused = TrackingIntent {
        action: TrackingAction::Cancel,
        ..intent
    };
    assert_eq!(
        state
            .request_tracking(owner, reused, &freshness, 1000, |_| true)
            .result,
        Err(TrackingRejection::Rejected(RuntimeError::Lifecycle(
            LifecycleError::RequestOrder
        )))
    );
}

#[test]
fn stale_proof_and_revision_cannot_arm_but_current_owner_can_always_cancel() {
    let (mut state, owner, freshness, intent) = setup();
    state
        .request_tracking(owner, intent, &freshness, 1000, |_| true)
        .result
        .unwrap();
    let arm = TrackingIntent {
        sequence: 2,
        action: TrackingAction::Arm,
        ..intent
    };
    assert_eq!(
        state
            .request_tracking(owner, arm, &freshness, 1000, |_| true)
            .result,
        Err(TrackingRejection::Rejected(RuntimeError::Lifecycle(
            LifecycleError::StateRevision
        )))
    );
    for now in (1100..=2000).step_by(100) {
        state.publish_network(true, now);
    }
    let arm = TrackingIntent {
        sequence: 3,
        revision: state.tracking().snapshot().revision,
        ..arm
    };
    assert_eq!(
        state
            .request_tracking(owner, arm, &freshness, 2000, |_| true)
            .result,
        Err(TrackingRejection::StaleObservation)
    );
    let cancel = TrackingIntent {
        sequence: 4,
        proof: 0,
        revision: u64::MAX,
        action: TrackingAction::Cancel,
        ..intent
    };
    assert!(state
        .request_tracking(owner, cancel, &freshness, 2000, |_| true)
        .result
        .is_ok());
    assert_eq!(
        state.tracking().snapshot().phase,
        ezal_core::tracking::TrackingPhase::Cancelled
    );
}

#[test]
fn reads_do_not_renew_and_old_connection_or_generation_cannot_cancel_successor() {
    let (mut state, owner, mut freshness, intent) = setup();
    let grant = state.operator(owner, 1000).unwrap();
    for now in (1100..6000).step_by(100) {
        state.publish_network(true, now);
        let mut json = String::new();
        state
            .tracking_telemetry(owner, now)
            .write_json(&mut json)
            .unwrap();
        assert!(json.contains("\"operator\":\"1\""));
        assert_eq!(
            state.operator(owner, now).unwrap().expires_at_ms,
            grant.expires_at_ms
        );
    }
    state.publish_network(true, 6000);
    assert!(state.operator(owner, 6000).is_none());
    assert!(state.take_control(owner, 6000));
    let stale = TrackingIntent {
        action: TrackingAction::Cancel,
        ..intent
    };
    assert_eq!(
        state
            .request_tracking(owner, stale, &freshness, 6000, |_| true)
            .result,
        Err(TrackingRejection::Lease)
    );
    let current = TrackingIntent {
        proof: freshness.issue(6000).unwrap().token,
        lease: state.operator(owner, 6000).unwrap().token.generation(),
        ..intent
    };
    state
        .request_tracking(owner, current, &freshness, 6000, |_| true)
        .result
        .unwrap();
    state.release_client(owner, 6000);
    let reconnect = state.register_client(6000);
    assert_ne!(owner, reconnect);
    assert_eq!(
        state
            .request_tracking(owner, current, &freshness, 6000, |_| true)
            .result,
        Err(TrackingRejection::Lease)
    );
    let mut json = String::new();
    state
        .tracking_telemetry(reconnect, 6000)
        .write_json(&mut json)
        .unwrap();
    assert!(
        json.contains("\"norad_id\":\"39260\""),
        "selection survives reconnect"
    );
}
