//! Startup-only operator override through the same boundary used by firmware.
mod common;

use ezal_core::actuator::{Actuator, ActuatorConfig};
use ezal_core::coordination::CoordinationState;
use ezal_core::drive::{AzimuthDirection, Command, CommandEnvelope, DriveCommand};
use ezal_core::duty::DutyHistory;
use ezal_core::feedback::{FeedbackHealth, FeedbackSnapshot};
use ezal_core::interlock::FeedbackInterlock;
use ezal_core::position::Pointing;
use ezal_core::protocol::{
    ClientMessage, StartupRestIntent, StartupRestRejection, StartupRestReply,
    STARTUP_REST_REPLY_CAPACITY,
};
use ezal_core::session::CommandFreshness;

fn observe(state: &mut CoordinationState, now: u64) {
    let sample = common::sample(Pointing::new(180.0, 45.0), now);
    let mut interlock = FeedbackInterlock::new(common::installed());
    interlock.publish(Some(sample));
    state.publish_network(true, now);
    state.publish_feedback(
        FeedbackSnapshot {
            raw: None,
            last_valid: Some(sample),
            health: FeedbackHealth::Healthy,
        },
        interlock,
        now,
    );
}

fn service(state: &mut CoordinationState, actuator: &mut Actuator, now: u64) {
    observe(state, now);
    state.service_actuator(actuator, now);
    state.record_actuator(actuator, now);
}

fn boot(
    actuator: Actuator,
) -> (
    CoordinationState,
    Actuator,
    u32,
    CommandFreshness,
    StartupRestIntent,
) {
    let mut state = CoordinationState::new();
    let mut actuator = actuator;
    service(&mut state, &mut actuator, 0);
    service(&mut state, &mut actuator, 2000);
    let owner = state.register_client(2000);
    let mut freshness = CommandFreshness::new(owner);
    let intent = StartupRestIntent {
        proof: freshness.issue(2000).unwrap().token,
        lease: state.operator(owner, 2000).unwrap().token.generation(),
    };
    (state, actuator, owner, freshness, intent)
}

#[test]
fn override_is_applied_only_by_output_owner_discards_buffered_motion_and_invalidates_preview() {
    let (mut state, mut actuator, owner, freshness, intent) = boot(Actuator::new());
    let before = state.telemetry(owner, 2000).control.duty;
    let revision = state.start_revision();
    assert!(before.startup_rest_required);
    let move_request = CommandEnvelope {
        command: Command::Jog(DriveCommand {
            azimuth: Some(AzimuthDirection::Clockwise),
            elevation: None,
        }),
        valid_until_ms: 2750,
    };
    assert!(state.accept_command(owner, move_request, 2000));
    assert_eq!(
        state
            .request_startup_rest(owner, intent, &freshness, 2000)
            .result,
        Ok(())
    );
    assert_eq!(
        state.telemetry(owner, 2000).control.duty,
        before,
        "queuing is not application"
    );
    service(&mut state, &mut actuator, 2010);
    assert!(!actuator.status().duty.startup_rest_required);
    assert_eq!(actuator.status().duty.azimuth.remaining_ms, 180_000);
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
    assert_eq!(actuator.target(), DriveCommand::IDLE);
    assert!(state.start_revision() > revision);
    service(&mut state, &mut actuator, 2110);
    assert_eq!(
        actuator.actual(),
        DriveCommand::IDLE,
        "the old held command cannot restart"
    );
    assert_eq!(
        state
            .request_startup_rest(owner, intent, &freshness, 2110)
            .result,
        Err(StartupRestRejection::Unavailable)
    );
    assert!(state.accept_command(owner, move_request, 2110));
    service(&mut state, &mut actuator, 2110);
    assert!(
        actuator.actual().is_active(),
        "a fresh press can use the allowance"
    );
}

#[test]
fn viewer_wrong_lease_and_expired_proof_cannot_queue_an_override() {
    let (mut state, mut actuator, owner, freshness, intent) = boot(Actuator::new());
    let viewer = state.register_client(2000);
    assert_eq!(
        state
            .request_startup_rest(viewer, intent, &freshness, 2000)
            .result,
        Err(StartupRestRejection::Lease)
    );
    assert_eq!(
        state
            .request_startup_rest(
                owner,
                StartupRestIntent {
                    lease: intent.lease + 1,
                    ..intent
                },
                &freshness,
                2000
            )
            .result,
        Err(StartupRestRejection::Lease)
    );
    for now in (2200..=3000).step_by(200) {
        observe(&mut state, now);
    }
    assert_eq!(
        state
            .request_startup_rest(owner, intent, &freshness, 3000)
            .result,
        Err(StartupRestRejection::StaleObservation)
    );
    service(&mut state, &mut actuator, 3000);
    assert!(actuator.status().duty.startup_rest_required);
}

#[test]
fn queued_request_is_discarded_on_expiry_disconnect_or_network_generation_change() {
    for cause in ["expiry", "disconnect", "network"] {
        let (mut state, mut actuator, owner, freshness, intent) = boot(Actuator::new());
        assert_eq!(
            state
                .request_startup_rest(owner, intent, &freshness, 2000)
                .result,
            Ok(())
        );
        let now = match cause {
            "expiry" => {
                for now in (2200..=3000).step_by(200) {
                    observe(&mut state, now);
                }
                3000
            }
            "disconnect" => {
                assert!(state.release_client(owner, 2001));
                state.register_client(2001);
                2010
            }
            "network" => {
                state.publish_network(false, 2001);
                state.publish_network(true, 2002);
                state.register_client(2002);
                2010
            }
            _ => unreachable!(),
        };
        service(&mut state, &mut actuator, now);
        assert!(actuator.status().duty.startup_rest_required, "{cause}");
        assert_eq!(actuator.actual(), DriveCommand::IDLE);
    }
}

#[test]
fn known_duty_history_and_latched_configuration_fault_cannot_be_overridden() {
    let invalid = ActuatorConfig {
        duty: ezal_core::duty::DutyConfig {
            energized_ms: 1,
            rest_ms: 900_000,
        },
        ..ActuatorConfig::DEFAULT
    };
    for actuator in [
        Actuator::with_config(ActuatorConfig::DEFAULT, DutyHistory::Known([180_000; 2])),
        Actuator::with_config(invalid, DutyHistory::Unknown),
    ] {
        let (mut state, mut actuator, owner, freshness, intent) = boot(actuator);
        let before = actuator.status();
        assert_eq!(
            state
                .request_startup_rest(owner, intent, &freshness, 2000)
                .result,
            Err(StartupRestRejection::Unavailable)
        );
        assert!(!actuator.override_startup_rest(2000));
        assert_eq!(actuator.status(), before);
    }
}

#[test]
fn wire_format_is_strict_full_width_and_reply_is_bounded() {
    let intent = StartupRestIntent {
        proof: u64::MAX,
        lease: u64::MAX,
    };
    assert_eq!(
        ClientMessage::parse("startup-rest:18446744073709551615:18446744073709551615"),
        Some(ClientMessage::StartupRest(intent))
    );
    for bad in [
        "startup-rest",
        "startup-rest:1",
        "startup-rest:1:1:extra",
        "startup-rest:0:1",
        "startup-rest:1:0",
        "startup-rest:+1:1",
        "startup-rest:1:1\n",
        "startup-rest:18446744073709551616:1",
    ] {
        assert_eq!(ClientMessage::parse(bad), None, "{bad}");
    }
    for result in [
        Ok(()),
        Err(StartupRestRejection::Lease),
        Err(StartupRestRejection::StaleObservation),
        Err(StartupRestRejection::Unavailable),
    ] {
        let mut json: heapless::String<STARTUP_REST_REPLY_CAPACITY> = heapless::String::new();
        StartupRestReply {
            proof: u64::MAX,
            result,
        }
        .write_json(&mut json)
        .unwrap();
        assert!(json.contains("\"proof\":\"18446744073709551615\""));
    }
}
