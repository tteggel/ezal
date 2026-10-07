//! Tests for the rotator drive model and actuator-safety debouncer.

use ezal_core::drive::{
    ActuatorGuard, AzimuthDirection, Command, Debouncer, DriveCommand, ElevationDirection,
    MOVEMENT_LEASE_MS, OUTPUT_MIN_ACTIVE_MS, OUTPUT_MIN_INACTIVE_MS,
};

#[test]
fn delivery_delay_does_not_renew_an_absolute_command_expiry() {
    let drive = DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: None,
    };
    let mut guard = ActuatorGuard::new();
    guard.command_until(Command::Jog(drive), 100, 700);
    assert_eq!(guard.actual(), drive);
    assert_eq!(guard.lease_deadline_ms(), Some(700));
    // A later refresh bearing the same expiry may preserve the output, but
    // never turn its remaining 200 ms into a fresh receipt-time lease.
    guard.command_until(Command::Jog(drive), 500, 700);
    assert_eq!(guard.lease_deadline_ms(), Some(700));
    assert_eq!(guard.next_transition_ms(500), Some(700));
    guard.update(700);
    assert_eq!(guard.actual(), DriveCommand::IDLE);
    assert_eq!(guard.target(), DriveCommand::IDLE);
}

#[test]
fn late_movement_is_refused_even_if_it_was_valid_when_queued() {
    let drive = DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: None,
    };
    for now_ms in [700, 800] {
        let mut guard = ActuatorGuard::new();
        guard.command_until(Command::Jog(drive), now_ms, 700);
        assert_eq!(guard.actual(), DriveCommand::IDLE);
        assert_eq!(guard.target(), DriveCommand::IDLE);
        assert_eq!(guard.lease_deadline_ms(), None);
    }
}

#[test]
fn remaining_absolute_lifetime_must_cover_the_minimum_on_time() {
    let drive = DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: None,
    };
    let mut guard = ActuatorGuard::new();
    guard.command_until(Command::Drive(drive), 300, 700);
    assert_eq!(guard.actual(), DriveCommand::IDLE);
    assert_eq!(guard.target(), drive);
    assert_eq!(guard.next_transition_ms(300), Some(700));
    guard.update(700);
    assert_eq!(guard.actual(), DriveCommand::IDLE);
    assert_eq!(guard.target(), DriveCommand::IDLE);
}

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

fn clockwise() -> Command {
    Command::Drive(DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: None,
    })
}

#[test]
fn a_delayed_activation_waits_until_its_lease_covers_the_minimum_on_time() {
    let mut guard = ActuatorGuard::new();
    guard.command(clockwise(), 0);
    guard.command(Command::Stop, 500);
    // The off-hold ends at 2500, but this lease ends at 2750: starting then
    // would leave only 250 ms before expiry cut the relay.
    guard.command(clockwise(), 2_000);
    assert_eq!(guard.next_transition_ms(2_000), Some(2_750));
    assert!(!guard.update(2_500));
    assert_eq!(guard.actual(), DriveCommand::IDLE);

    // A refresh that covers the minimum on-time starts the output at once.
    assert!(guard.command(clockwise(), 2_600));
    assert!(guard.actual().is_active());
    assert_eq!(guard.lease_deadline_ms(), Some(3_350));
    assert!(guard.update(3_350));
    assert_eq!(guard.actual(), DriveCommand::IDLE);
    assert_eq!(guard.target(), DriveCommand::IDLE);
    assert_eq!(guard.next_transition_ms(3_350), None);

    // Safety stops still protect the relay against immediate reactivation.
    guard.command(clockwise(), 5_000);
    guard.command(clockwise(), 5_300);
    guard.update(5_349);
    assert_eq!(guard.actual(), DriveCommand::IDLE);
    guard.update(5_350);
    assert!(guard.actual().is_active());
}

#[test]
fn a_stalled_refresh_stream_never_pulses_the_relay_briefly() {
    let ccw = Command::Jog(DriveCommand {
        azimuth: Some(AzimuthDirection::CounterClockwise),
        elevation: None,
    });
    let mut guard = ActuatorGuard::new();
    guard.command(ccw, 0);
    guard.command(Command::Inhibit, 0);
    // The last refresh lands during the off-hold, then the network stalls.
    guard.command(ccw, 1_400);
    assert_eq!(guard.next_transition_ms(1_400), Some(2_150));
    assert!(!guard.update(2_000));
    assert!(!guard.update(2_150));
    assert_eq!(guard.actual(), DriveCommand::IDLE);
    assert_eq!(guard.target(), DriveCommand::IDLE);
    // No output ran, so no fresh off-hold delays the next held command.
    assert!(guard.command(ccw, 2_200));
    assert_eq!(
        guard.actual().azimuth,
        Some(AzimuthDirection::CounterClockwise)
    );
}

#[test]
fn safety_inhibition_bypasses_minimum_on_and_cancels_pending_reversal() {
    let mut guard = ActuatorGuard::new();
    guard.command(clockwise(), 0);
    let reverse = Command::Drive(DriveCommand {
        azimuth: Some(AzimuthDirection::CounterClockwise),
        elevation: None,
    });
    guard.command(reverse, 50);
    assert!(guard.command(Command::Inhibit, 100));
    assert_eq!(guard.actual(), DriveCommand::IDLE);
    assert_eq!(guard.target(), DriveCommand::IDLE);
    assert_eq!(guard.lease_deadline_ms(), None);
    guard.update(2_100);
    assert_eq!(guard.actual(), DriveCommand::IDLE);
}

#[test]
fn a_late_refresh_cannot_extend_an_already_expired_lease() {
    let mut guard = ActuatorGuard::new();
    guard.command(clockwise(), 0);
    assert!(guard.command(clockwise(), MOVEMENT_LEASE_MS));
    assert_eq!(guard.actual(), DriveCommand::IDLE);
    assert!(guard.target().is_active());
    assert_eq!(
        guard.next_transition_ms(MOVEMENT_LEASE_MS),
        Some(2 * MOVEMENT_LEASE_MS)
    );
}

#[test]
fn ordinary_release_at_a_pending_activation_deadline_never_energises_the_output() {
    let mut guard = ActuatorGuard::new();
    guard.command(clockwise(), 0);
    guard.command(Command::Stop, 500);
    guard.command(clockwise(), 2_000);
    guard.command(clockwise(), 2_300);
    assert_eq!(guard.actual(), DriveCommand::IDLE);
    assert_eq!(guard.next_transition_ms(2_300), Some(2_500));
    assert!(!guard.command(Command::Stop, 2_500));
    assert_eq!(guard.actual(), DriveCommand::IDLE);
    assert_eq!(guard.target(), DriveCommand::IDLE);
    assert_eq!(guard.next_transition_ms(2_500), None);
}

#[test]
fn ordinary_release_of_a_leased_activation_ends_at_its_minimum_on_time() {
    let mut guard = ActuatorGuard::new();
    guard.command(clockwise(), 0);
    guard.command(Command::Stop, 500);
    guard.command(clockwise(), 2_000);
    guard.command(clockwise(), 2_300);
    assert!(guard.update(2_500));
    guard.command(Command::Stop, 2_600);
    // Even a no-op interlock check must retain the lease while actual is on.
    guard.inhibit_axes(false, false, 2_600);
    assert!(guard.actual().is_active());
    assert_eq!(guard.lease_deadline_ms(), Some(3_050));
    // An output only starts when its lease outlasts the minimum on-time.
    assert_eq!(guard.next_transition_ms(2_600), Some(3_000));
    assert!(guard.update(3_000));
    assert_eq!(guard.actual(), DriveCommand::IDLE);
    assert_eq!(guard.lease_deadline_ms(), None);
}

#[test]
fn jog_release_or_reversal_stops_that_axis_immediately() {
    let cw_up = DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: Some(ElevationDirection::Up),
    };
    let up = DriveCommand {
        azimuth: None,
        elevation: Some(ElevationDirection::Up),
    };
    let ccw_up = DriveCommand {
        azimuth: Some(AzimuthDirection::CounterClockwise),
        elevation: Some(ElevationDirection::Up),
    };
    let down = DriveCommand {
        azimuth: None,
        elevation: Some(ElevationDirection::Down),
    };

    // A controller release keeps the relay's minimum on-time.
    let mut controller = ActuatorGuard::new();
    controller.command(Command::Drive(cw_up), 0);
    assert!(!controller.command(Command::Drive(up), 100));
    assert_eq!(controller.actual(), cw_up);

    // Releasing one held browser control cuts only that axis, at once.
    let mut operator = ActuatorGuard::new();
    operator.command(Command::Jog(cw_up), 0);
    assert!(operator.command(Command::Jog(up), 100));
    assert_eq!(operator.actual(), up);
    assert_eq!(operator.lease_deadline_ms(), Some(100 + MOVEMENT_LEASE_MS));

    // Reversal also cuts at once, and the new direction waits out the hold.
    assert!(!operator.command(Command::Jog(ccw_up), 200));
    assert_eq!(operator.actual(), up);
    assert_eq!(operator.target(), ccw_up);
    assert!(operator.command(Command::Jog(down), 300));
    assert_eq!(operator.actual(), DriveCommand::IDLE);
    assert_eq!(operator.target(), down);
    for now_ms in [800, 1_300, 1_800, 300 + OUTPUT_MIN_INACTIVE_MS - 1] {
        operator.command(Command::Jog(down), now_ms);
        assert_eq!(operator.actual(), DriveCommand::IDLE);
    }
    assert!(operator.update(300 + OUTPUT_MIN_INACTIVE_MS));
    assert_eq!(operator.actual(), down);
}

#[test]
fn inhibiting_one_axis_keeps_the_other_axis_and_does_not_extend_the_off_hold() {
    let mut guard = ActuatorGuard::new();
    let both = Command::Drive(DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: Some(ElevationDirection::Up),
    });
    guard.command(both, 0);
    assert!(guard.inhibit_axes(true, false, 100));
    assert_eq!(guard.actual().azimuth, None);
    assert_eq!(guard.actual().elevation, Some(ElevationDirection::Up));
    assert_eq!(guard.lease_deadline_ms(), Some(MOVEMENT_LEASE_MS));
    guard.inhibit_axes(true, false, 200);
    guard.command(both, 500);
    guard.command(both, 1_000);
    guard.command(both, 1_500);
    guard.command(both, 2_000);
    assert_eq!(guard.actual().azimuth, None);
    guard.update(2_100);
    assert_eq!(guard.actual().azimuth, Some(AzimuthDirection::Clockwise));
}
