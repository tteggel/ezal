//! Tests for the rotator drive model and actuator-safety debouncer.

use ezal_core::drive::{
    ActuatorGuard, AzimuthDirection, Command, Debouncer, DriveCommand, ElevationDirection,
    MOVEMENT_LEASE_MS, OUTPUT_MIN_ACTIVE_MS, OUTPUT_MIN_INACTIVE_MS,
};

#[test]
fn debouncer_holds_active_outputs_for_the_minimum_time() {
    let mut debouncer = Debouncer::new();
    let cw = DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: None,
    };

    assert!(debouncer.set_target(Command::Drive(cw), 10));
    assert_eq!(debouncer.actual(), cw);
    assert_eq!(debouncer.target(), cw);

    assert!(!debouncer.set_target(Command::Stop, 100));
    assert_eq!(debouncer.actual(), cw);
    assert_eq!(debouncer.next_transition_ms(100), Some(510));

    assert!(!debouncer.update(509));
    assert_eq!(debouncer.actual(), cw);

    assert!(debouncer.update(510));
    assert_eq!(debouncer.actual(), DriveCommand::IDLE);
    assert_eq!(debouncer.next_transition_ms(510), None);
}

#[test]
fn debouncer_delays_reversals_without_conflicting_outputs() {
    let mut debouncer = Debouncer::new();
    let cw = DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: None,
    };
    let ccw = DriveCommand {
        azimuth: Some(AzimuthDirection::CounterClockwise),
        elevation: None,
    };

    assert!(debouncer.set_target(Command::Drive(cw), 0));
    assert!(!debouncer.set_target(Command::Drive(ccw), 125));
    assert_eq!(debouncer.target(), ccw);
    assert_eq!(debouncer.actual(), cw);
    assert_eq!(
        debouncer.next_transition_ms(125),
        Some(OUTPUT_MIN_ACTIVE_MS)
    );

    assert!(debouncer.update(OUTPUT_MIN_ACTIVE_MS));
    assert_eq!(debouncer.actual(), DriveCommand::IDLE);
    assert_eq!(
        debouncer.next_transition_ms(OUTPUT_MIN_ACTIVE_MS),
        Some(OUTPUT_MIN_ACTIVE_MS + OUTPUT_MIN_INACTIVE_MS)
    );

    assert!(!debouncer.update(OUTPUT_MIN_ACTIVE_MS + OUTPUT_MIN_INACTIVE_MS - 1));
    assert_eq!(debouncer.actual(), DriveCommand::IDLE);

    assert!(debouncer.update(OUTPUT_MIN_ACTIVE_MS + OUTPUT_MIN_INACTIVE_MS));
    assert_eq!(debouncer.actual(), ccw);
    assert_eq!(
        debouncer.next_transition_ms(OUTPUT_MIN_ACTIVE_MS + OUTPUT_MIN_INACTIVE_MS),
        None
    );

    assert!(!debouncer.set_target(
        Command::Stop,
        OUTPUT_MIN_ACTIVE_MS + OUTPUT_MIN_INACTIVE_MS + 1
    ));
    assert_eq!(debouncer.actual(), ccw);
    assert_eq!(
        debouncer.next_transition_ms(OUTPUT_MIN_ACTIVE_MS + OUTPUT_MIN_INACTIVE_MS + 1),
        Some(OUTPUT_MIN_ACTIVE_MS * 2 + OUTPUT_MIN_INACTIVE_MS)
    );
}

#[test]
fn debouncer_holds_inactive_outputs_for_the_minimum_time() {
    let mut debouncer = Debouncer::new();
    let cw = DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: None,
    };

    assert!(debouncer.set_target(Command::Drive(cw), 0));
    assert!(debouncer.set_target(Command::Stop, OUTPUT_MIN_ACTIVE_MS));
    assert_eq!(debouncer.actual(), DriveCommand::IDLE);

    assert!(!debouncer.set_target(Command::Drive(cw), OUTPUT_MIN_ACTIVE_MS + 100));
    assert_eq!(debouncer.actual(), DriveCommand::IDLE);
    assert_eq!(
        debouncer.next_transition_ms(OUTPUT_MIN_ACTIVE_MS + 100),
        Some(OUTPUT_MIN_ACTIVE_MS + OUTPUT_MIN_INACTIVE_MS)
    );

    assert!(!debouncer.update(OUTPUT_MIN_ACTIVE_MS + OUTPUT_MIN_INACTIVE_MS - 1));
    assert_eq!(debouncer.actual(), DriveCommand::IDLE);

    assert!(debouncer.update(OUTPUT_MIN_ACTIVE_MS + OUTPUT_MIN_INACTIVE_MS));
    assert_eq!(debouncer.actual(), cw);
}

#[test]
fn debouncer_keeps_axis_timing_independent() {
    let mut debouncer = Debouncer::new();
    let cw = DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: None,
    };
    let cw_up = DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: Some(ElevationDirection::Up),
    };
    let up = DriveCommand {
        azimuth: None,
        elevation: Some(ElevationDirection::Up),
    };

    assert!(debouncer.set_target(Command::Drive(cw), 0));
    assert!(debouncer.set_target(Command::Drive(cw_up), 100));
    assert_eq!(debouncer.actual(), cw_up);

    assert!(!debouncer.set_target(Command::Drive(up), 200));
    assert_eq!(debouncer.actual(), cw_up);
    assert_eq!(
        debouncer.next_transition_ms(200),
        Some(OUTPUT_MIN_ACTIVE_MS)
    );

    assert!(debouncer.update(OUTPUT_MIN_ACTIVE_MS));
    assert_eq!(debouncer.actual(), up);
    assert_eq!(debouncer.next_transition_ms(OUTPUT_MIN_ACTIVE_MS), None);
}

#[test]
fn actuator_guard_stops_an_unrefreshed_movement_lease() {
    let mut guard = ActuatorGuard::new();
    let drive = DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: Some(ElevationDirection::Up),
    };

    assert!(guard.command(Command::Drive(drive), 100));
    assert_eq!(guard.actual(), drive);
    assert_eq!(guard.lease_deadline_ms(), Some(100 + MOVEMENT_LEASE_MS));
    assert!(!guard.update(100 + MOVEMENT_LEASE_MS - 1));
    assert_eq!(guard.actual(), drive);

    assert!(guard.update(100 + MOVEMENT_LEASE_MS));
    assert_eq!(guard.actual(), DriveCommand::IDLE);
    assert_eq!(guard.target(), DriveCommand::IDLE);
    assert_eq!(guard.lease_deadline_ms(), None);
}

#[test]
fn actuator_guard_refreshes_lease_without_changing_outputs() {
    let mut guard = ActuatorGuard::new();
    let drive = DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: None,
    };

    assert!(guard.command(Command::Drive(drive), 0));
    assert!(!guard.command(Command::Drive(drive), 500));
    assert_eq!(guard.lease_deadline_ms(), Some(500 + MOVEMENT_LEASE_MS));
    assert_eq!(guard.next_transition_ms(600), Some(1_250));
}
