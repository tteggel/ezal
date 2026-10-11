//! Feedback interlock behaviour through the shared actuator sequence.

use super::common;

use common::{installed, sample, TEST_CALIBRATION};
use ezal_core::ads1015::FEEDBACK_FULL_SCALE;
use ezal_core::drive::{
    AzimuthDirection, Command, DriveCommand, ElevationDirection, AZIMUTH_MAX_SPEED_DEG_PER_S,
};
use ezal_core::feedback::{FeedbackMonitor, FEEDBACK_MAX_AGE_MS};
use ezal_core::interlock::{FeedbackInterlock, TravelProfile};
use ezal_core::position::{
    FeedbackVoltages, InstalledCalibration, Pointing, FEEDBACK_ENDPOINT_MARGIN_MV,
};

const BOTH: DriveCommand = DriveCommand {
    azimuth: Some(AzimuthDirection::Clockwise),
    elevation: Some(ElevationDirection::Up),
};

const CLOCKWISE: DriveCommand = DriveCommand {
    azimuth: Some(AzimuthDirection::Clockwise),
    elevation: None,
};

fn gate_at(position: Pointing, timestamp_ms: u64) -> FeedbackInterlock {
    let mut gate = FeedbackInterlock::new(installed());
    gate.publish(Some(sample(position, timestamp_ms)));
    gate
}

#[test]
fn held_command_refreshes_cannot_extend_feedback_lifetime() {
    let gate = gate_at(Pointing::new(180.0, 45.0), 0);
    let mut actuator = common::rested_actuator();
    for now_ms in [0, 150, 300, 450] {
        actuator.service(&gate, now_ms);
        actuator.command(Command::Drive(BOTH), &gate, now_ms);
        assert_eq!(actuator.actual(), BOTH);
    }
    assert_eq!(
        actuator.next_deadline_ms(&gate, 450),
        Some(FEEDBACK_MAX_AGE_MS + 1)
    );
    assert!(gate.motion_permitted(FEEDBACK_MAX_AGE_MS));
    assert!(actuator.service(&gate, FEEDBACK_MAX_AGE_MS + 1));
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
    assert_eq!(actuator.target(), DriveCommand::IDLE);
    assert_eq!(actuator.lease_deadline_ms(), None);
    assert_eq!(
        actuator.next_deadline_ms(&gate, FEEDBACK_MAX_AGE_MS + 1),
        None
    );
    actuator.command(Command::Drive(BOTH), &gate, 600);
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
}

#[test]
fn fault_clears_active_movement_before_minimum_on_time() {
    let mut gate = gate_at(Pointing::new(180.0, 45.0), 0);
    let mut actuator = common::rested_actuator();
    actuator.command(Command::Drive(BOTH), &gate, 0);
    gate.publish(None);
    assert!(actuator.service(&gate, 100));
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
    assert_eq!(actuator.target(), DriveCommand::IDLE);
    for command in [Command::Drive(BOTH), Command::Jog(BOTH)] {
        assert_eq!(gate.guard_command(command, 100), Command::Inhibit);
    }

    // Feedback recovery alone must never replay the discarded movement.
    gate.publish(Some(sample(Pointing::new(180.0, 45.0), 200)));
    actuator.service(&gate, 200);
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
    assert_eq!(actuator.target(), DriveCommand::IDLE);
    actuator.command(Command::Drive(BOTH), &gate, 200);
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
}

#[test]
fn a_pending_activation_is_checked_against_feedback_when_it_falls_due() {
    let mut gate = gate_at(Pointing::new(355.0, 45.0), 0);
    let mut actuator = common::rested_actuator();
    actuator.command(Command::Drive(CLOCKWISE), &gate, 0);
    actuator.command(Command::Inhibit, &gate, 100);

    gate.publish(Some(sample(Pointing::new(355.0, 45.0), 1_900)));
    actuator.command(Command::Drive(CLOCKWISE), &gate, 1_900);
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
    assert_eq!(actuator.target(), CLOCKWISE);
    assert_eq!(actuator.next_deadline_ms(&gate, 1_900), Some(2_100));

    // Updated feedback enters the coast allowance before the inactive hold ends.
    gate.publish(Some(sample(Pointing::new(356.2, 45.0), 2_050)));
    assert!(!actuator.service(&gate, 2_100));
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
    assert_eq!(actuator.target(), DriveCommand::IDLE);
    assert_eq!(actuator.lease_deadline_ms(), None);
}

#[test]
fn endpoint_inhibits_outward_axis_immediately_and_preserves_other_axis() {
    let mut gate = gate_at(Pointing::new(355.0, 45.0), 0);
    let mut actuator = common::rested_actuator();
    actuator.command(Command::Drive(BOTH), &gate, 0);
    assert_eq!(actuator.actual(), BOTH);
    let lease = actuator.lease_deadline_ms();
    gate.publish(Some(sample(Pointing::new(356.2, 45.0), 100)));
    assert!(actuator.service(&gate, 100));
    let up = DriveCommand {
        azimuth: None,
        elevation: BOTH.elevation,
    };
    assert_eq!(actuator.actual(), up);
    assert_eq!(actuator.target(), up);
    assert_eq!(actuator.lease_deadline_ms(), lease);
    assert_eq!(
        gate.guard_command(Command::Drive(BOTH), 100),
        Command::Drive(up)
    );

    let inward = DriveCommand {
        azimuth: Some(AzimuthDirection::CounterClockwise),
        elevation: BOTH.elevation,
    };
    assert_eq!(gate.guard_drive(inward, 100), inward);
    actuator.command(Command::Drive(inward), &gate, 100);
    assert_eq!(actuator.actual(), up);
}

#[test]
fn each_endpoint_allows_only_inward_axis_directions() {
    let lower = gate_at(Pointing::new(0.0, 0.0), 0);
    let upper = gate_at(Pointing::new(360.0, 90.0), 0);
    let decreasing = DriveCommand {
        azimuth: Some(AzimuthDirection::CounterClockwise),
        elevation: Some(ElevationDirection::Down),
    };
    assert_eq!(lower.guard_drive(BOTH, 0), BOTH);
    assert_eq!(lower.guard_drive(decreasing, 0), DriveCommand::IDLE);
    assert_eq!(upper.guard_drive(BOTH, 0), DriveCommand::IDLE);
    assert_eq!(upper.guard_drive(decreasing, 0), decreasing);
}

#[test]
fn the_actuators_coast_policy_overrides_the_feedback_producers_default() {
    let mut travel = TravelProfile::DEFAULT;
    travel.azimuth.coast_ms = 1_000;
    travel.elevation.coast_ms = 1_000;
    let decreasing = DriveCommand {
        azimuth: Some(AzimuthDirection::CounterClockwise),
        elevation: Some(ElevationDirection::Down),
    };
    for (position, outward, inward) in [
        (Pointing::new(354.0, 87.5), BOTH, decreasing),
        (Pointing::new(6.0, 2.5), decreasing, BOTH),
    ] {
        let gate = gate_at(position, 0);
        assert_eq!(gate.guard_drive(outward, 0), outward);
        let mut actuator = ezal_core::actuator::Actuator::with_config(
            ezal_core::actuator::ActuatorConfig {
                travel,
                ..ezal_core::actuator::ActuatorConfig::DEFAULT
            },
            ezal_core::duty::DutyHistory::Known([0, 0]),
        );
        actuator.command(Command::Jog(outward), &gate, 0);
        assert_eq!(actuator.actual(), DriveCommand::IDLE);
        actuator.command(Command::Jog(inward), &gate, 0);
        assert_eq!(actuator.actual(), inward);
        assert_eq!(actuator.fault(), None);
    }
}

#[test]
fn absent_future_and_differently_calibrated_feedback_cannot_admit_motion() {
    let mut narrower = TEST_CALIBRATION;
    narrower.azimuth.angle_max_deg = 350.0;
    narrower.elevation.angle_max_deg = 80.0;
    let mut gate =
        FeedbackInterlock::new(InstalledCalibration::new(narrower, FEEDBACK_FULL_SCALE).unwrap());
    assert_eq!(
        gate.guard_command(Command::Drive(BOTH), 0),
        Command::Inhibit
    );
    for feedback in [
        sample(Pointing::new(180.0, 45.0), 1),
        sample(Pointing::new(355.0, 45.0), 0),
        sample(Pointing::new(180.0, 85.0), 0),
    ] {
        gate.publish(Some(feedback));
        assert_eq!(
            gate.guard_command(Command::Drive(BOTH), 0),
            Command::Inhibit
        );
        assert_eq!(gate.deadline_ms(BOTH, 0), None);
    }

    // Neither an unpublished gate nor an unsupervisable calibration permits motion.
    assert!(!FeedbackInterlock::INHIBITED.motion_permitted(0));
    let mut unsupervised = TEST_CALIBRATION;
    unsupervised.elevation.voltage_max_mv = FEEDBACK_ENDPOINT_MARGIN_MV;
    let mut monitor = FeedbackMonitor::new(unsupervised, None);
    let gate = monitor
        .update(
            0,
            0,
            Ok(FeedbackVoltages {
                a0_elevation_mv: 850,
                a1_azimuth_mv: 1100,
            }),
        )
        .interlock;
    assert_eq!(gate.guard_drive(BOTH, 0), DriveCommand::IDLE);
}

#[test]
fn quantized_endpoint_readings_refuse_outward_motion_and_permit_inward_motion() {
    let code_mv = FEEDBACK_FULL_SCALE.code_mv() as i32;
    let azimuth = TEST_CALIBRATION.azimuth;
    let elevation = TEST_CALIBRATION.elevation;
    let outward = DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: Some(ElevationDirection::Down),
    };
    let inward = DriveCommand {
        azimuth: Some(AzimuthDirection::CounterClockwise),
        elevation: Some(ElevationDirection::Up),
    };
    let mut supervisor = FeedbackMonitor::new(TEST_CALIBRATION, None);

    // A dish at the azimuth maximum and elevation minimum can read a whole
    // ADC code inside either endpoint. Elevation voltage falls as angle rises.
    let update = supervisor.update(
        2,
        0,
        Ok(FeedbackVoltages {
            a0_elevation_mv: elevation.voltage_min_mv - code_mv,
            a1_azimuth_mv: azimuth.voltage_max_mv - code_mv,
        }),
    );
    let gate = update.interlock;
    let position = gate.feedback().unwrap().position();
    assert!(position.azimuth_deg < azimuth.angle_max_deg);
    assert!(position.elevation_deg > elevation.angle_min_deg);
    assert_eq!(gate.guard_drive(outward, 2), DriveCommand::IDLE);
    assert_eq!(
        gate.guard_command(Command::Jog(outward), 2),
        Command::Jog(DriveCommand::IDLE)
    );
    assert_eq!(
        gate.guard_command(Command::Jog(inward), 2),
        Command::Jog(inward)
    );

    // Far enough inside to reserve coast plus quantization, a fresh sample
    // permits outward motion only until travel consumes the remaining room.
    let update = supervisor.update(
        102,
        100,
        Ok(FeedbackVoltages {
            a0_elevation_mv: elevation.voltage_min_mv - 12 * code_mv,
            a1_azimuth_mv: azimuth.voltage_max_mv - 12 * code_mv,
        }),
    );
    let gate = update.interlock;
    assert_eq!(gate.guard_drive(outward, 102), outward);
    let refused_ms = gate.deadline_ms(outward, 102).unwrap();
    assert!(refused_ms < 100 + FEEDBACK_MAX_AGE_MS);
    assert_eq!(gate.guard_drive(outward, refused_ms - 1), outward);
    assert_ne!(gate.guard_drive(outward, refused_ms), outward);
    assert_eq!(gate.guard_drive(inward, refused_ms), inward);
}

#[test]
fn outward_travel_is_cut_before_it_could_reach_an_endpoint_code() {
    let sample_ms = 1_000;
    let start = Pointing::new(354.0, 86.0);
    let gate = gate_at(start, sample_ms);
    let mut actuator = common::rested_actuator();
    actuator.command(Command::Drive(BOTH), &gate, sample_ms + 5);
    assert_eq!(actuator.actual(), BOTH);

    // No further sample arrives: the cut must not wait for one.
    let cut_ms = actuator.next_deadline_ms(&gate, sample_ms + 5).unwrap();
    assert!(cut_ms < sample_ms + FEEDBACK_MAX_AGE_MS);
    assert!(!actuator.service(&gate, cut_ms - 1));
    assert!(actuator.service(&gate, cut_ms));
    assert_eq!(
        actuator.actual(),
        DriveCommand {
            azimuth: None,
            elevation: BOTH.elevation,
        }
    );
    let azimuth = TEST_CALIBRATION.azimuth;
    let code_deg = (azimuth.angle_max_deg - azimuth.angle_min_deg) * FEEDBACK_FULL_SCALE.code_mv()
        / (azimuth.voltage_max_mv - azimuth.voltage_min_mv) as f32;
    let worst_travel_deg = (cut_ms - sample_ms + TravelProfile::DEFAULT.azimuth.coast_ms) as f32
        * AZIMUTH_MAX_SPEED_DEG_PER_S
        / 1_000.0;
    assert!(start.azimuth_deg + worst_travel_deg < azimuth.angle_max_deg - code_deg);

    // Elevation has more room, so it keeps moving until the sample expires.
    assert_eq!(
        actuator.next_deadline_ms(&gate, cut_ms),
        Some(sample_ms + FEEDBACK_MAX_AGE_MS + 1)
    );
}

#[test]
fn a_released_axis_stops_the_same_way_whether_or_not_the_other_axis_is_refused() {
    let up = DriveCommand {
        azimuth: None,
        elevation: BOTH.elevation,
    };
    for elevation_deg in [45.0, 88.5] {
        let released_azimuth = |command: fn(DriveCommand) -> Command| {
            let mut gate = gate_at(Pointing::new(180.0, elevation_deg - 0.4), 0);
            let mut actuator = common::rested_actuator();
            actuator.command(command(BOTH), &gate, 0);
            gate.publish(Some(sample(Pointing::new(180.0, elevation_deg), 100)));
            actuator.command(command(up), &gate, 100);
            actuator.actual().azimuth
        };
        // A controller release keeps the relay's minimum on-time...
        assert_eq!(released_azimuth(Command::Drive), BOTH.azimuth);
        // ...while an operator release stops the axis at once.
        assert_eq!(released_azimuth(Command::Jog), None);
    }
}

#[test]
fn a_command_is_admitted_before_a_transition_due_at_the_same_instant() {
    let mut actuator = common::rested_actuator();
    actuator.command(
        Command::Drive(CLOCKWISE),
        &gate_at(Pointing::new(180.0, 45.0), 0),
        0,
    );
    actuator.command(
        Command::Stop,
        &gate_at(Pointing::new(180.0, 45.0), 500),
        500,
    );
    let gate = gate_at(Pointing::new(180.0, 45.0), 2_000);
    actuator.command(Command::Drive(CLOCKWISE), &gate, 2_000);
    actuator.command(Command::Drive(CLOCKWISE), &gate, 2_300);
    assert_eq!(actuator.next_deadline_ms(&gate, 2_300), Some(2_500));

    let mut serviced = actuator;
    assert!(serviced.service(&gate, 2_500));
    assert!(!actuator.command(Command::Stop, &gate, 2_500));
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
    assert_eq!(actuator.next_deadline_ms(&gate, 2_500), None);
}
