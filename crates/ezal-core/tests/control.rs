//! Tests for the fail-safe position controller.

use ezal_core::control::{ControlState, TimedPointing, TrackingController};
use ezal_core::drive::{AzimuthDirection, Command, ElevationDirection};
use ezal_core::position::Pointing;

fn timed(azimuth_deg: f32, elevation_deg: f32, timestamp_ms: u64) -> TimedPointing {
    TimedPointing::new(Pointing::new(azimuth_deg, elevation_deg), timestamp_ms)
}

#[test]
fn controller_tracks_both_axes_and_releases_inside_deadband() {
    let mut controller = TrackingController::default();
    let decision = controller.update(
        100,
        Some(timed(110.0, 60.0, 100)),
        Some(timed(100.0, 50.0, 100)),
    );
    let Command::Drive(drive) = decision.command else {
        panic!("expected movement");
    };
    assert_eq!(decision.state, ControlState::Tracking);
    assert_eq!(drive.azimuth, Some(AzimuthDirection::Clockwise));
    assert_eq!(drive.elevation, Some(ElevationDirection::Up));

    let decision = controller.update(
        200,
        Some(timed(110.0, 60.0, 200)),
        Some(timed(109.0, 59.0, 200)),
    );
    assert_eq!(decision.command, Command::Stop);
}

#[test]
fn controller_hysteresis_does_not_chatter_between_thresholds() {
    let mut controller = TrackingController::default();
    assert!(matches!(
        controller
            .update(0, Some(timed(105.0, 50.0, 0)), Some(timed(100.0, 50.0, 0)))
            .command,
        Command::Drive(_)
    ));
    assert!(matches!(
        controller
            .update(
                100,
                Some(timed(103.0, 50.0, 100)),
                Some(timed(100.0, 50.0, 100))
            )
            .command,
        Command::Drive(_)
    ));
    assert_eq!(
        controller
            .update(
                200,
                Some(timed(101.0, 50.0, 200)),
                Some(timed(100.0, 50.0, 200))
            )
            .command,
        Command::Stop
    );
    assert_eq!(
        controller
            .update(
                300,
                Some(timed(103.0, 50.0, 300)),
                Some(timed(100.0, 50.0, 300))
            )
            .command,
        Command::Stop
    );
}

#[test]
fn every_missing_stale_or_invalid_input_fails_safe() {
    let mut controller = TrackingController::default();
    let good_target = Some(timed(100.0, 50.0, 1_000));
    let good_feedback = Some(timed(90.0, 40.0, 1_000));

    let cases = [
        (None, good_feedback, 1_000, ControlState::Idle),
        (good_target, None, 1_000, ControlState::FeedbackUnavailable),
        (good_target, good_feedback, 1_501, ControlState::TargetStale),
        (
            Some(timed(100.0, 50.0, 1_600)),
            good_feedback,
            1_600,
            ControlState::FeedbackStale,
        ),
        (
            Some(timed(451.0, 50.0, 1_000)),
            good_feedback,
            1_000,
            ControlState::TargetInvalid,
        ),
        (
            good_target,
            Some(timed(90.0, f32::NAN, 1_000)),
            1_000,
            ControlState::FeedbackInvalid,
        ),
        (
            Some(timed(100.0, 50.0, 1_001)),
            good_feedback,
            1_000,
            ControlState::TimestampInvalid,
        ),
    ];

    for (target, feedback, now_ms, expected) in cases {
        let decision = controller.update(now_ms, target, feedback);
        assert_eq!(decision.command, Command::Stop);
        assert_eq!(decision.state, expected);
        assert!(!decision.state.permits_motion());
    }
}

#[test]
fn controller_never_drives_farther_into_a_mechanical_stop() {
    let mut controller = TrackingController::default();
    let decision = controller.update(0, Some(timed(0.0, 180.0, 0)), Some(timed(0.0, 180.0, 0)));
    assert_eq!(decision.command, Command::Stop);

    let decision = controller.update(
        100,
        Some(timed(20.0, 160.0, 100)),
        Some(timed(0.0, 180.0, 100)),
    );
    let Command::Drive(drive) = decision.command else {
        panic!("expected motion away from the limits");
    };
    assert_eq!(drive.azimuth, Some(AzimuthDirection::Clockwise));
    assert_eq!(drive.elevation, Some(ElevationDirection::Down));
}
