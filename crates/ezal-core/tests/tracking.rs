mod common;

use common::{full_travel_sample, FULL_TRAVEL_CALIBRATION};
use ezal_core::actuator::ActuatorConfig;
use ezal_core::drive::{AzimuthDirection, Command, DriveCommand, ElevationDirection, RelayConfig};
use ezal_core::duty::DutyConfig;
use ezal_core::feedback::FEEDBACK_MAX_AGE_MS;
use ezal_core::motion::MotionFault;
use ezal_core::position::Pointing;
use ezal_core::tracking::{
    pointing_deviation_deg, Axis, FollowingController, MotorModel, MotorProfile, Scenario,
    Simulation, SimulationError, SimulationSample, SimulationSettings, TrackingTarget,
};

const CW: DriveCommand = DriveCommand {
    azimuth: Some(AzimuthDirection::Clockwise),
    elevation: None,
};
const CCW: DriveCommand = DriveCommand {
    azimuth: Some(AzimuthDirection::CounterClockwise),
    elevation: None,
};
// Offline output policy: installed relay timing with confirmed AC duty.
const ACTUATOR: ActuatorConfig = ActuatorConfig {
    duty: DutyConfig::CONFIRMED_AC,
    ..ActuatorConfig::DEFAULT
};

fn close(actual: f32, expected: f32) {
    assert!((actual - expected).abs() < 0.0001, "{actual} != {expected}");
}

fn ideal_profile(rate: f32) -> MotorProfile {
    let mut profile = MotorProfile::AC_50HZ;
    for axis in [&mut profile.azimuth, &mut profile.elevation] {
        axis.rate_deg_per_s = rate;
        axis.startup_ms = 0;
        axis.coast_ms = 0;
        axis.backlash_deg = 0.0;
    }
    profile
}

fn settings(plant: MotorProfile) -> SimulationSettings {
    SimulationSettings {
        step_ms: 20,
        scenario: Scenario {
            motor: plant,
            ..Scenario::NOMINAL
        },
    }
}

/// True sky deviation of the actual pose, the simulation's ground truth.
fn deviation(sample: SimulationSample, sky: Pointing) -> f32 {
    pointing_deviation_deg(sample.position, sky).unwrap()
}

fn model(profile: MotorProfile) -> MotorModel {
    MotorModel::new(
        profile,
        FULL_TRAVEL_CALIBRATION,
        Pointing::new(100.0, 45.0),
        [0, 0],
    )
    .unwrap()
}

fn simulation(initial: Pointing, plant: MotorProfile) -> Simulation {
    Simulation::new(
        FULL_TRAVEL_CALIBRATION,
        initial,
        FollowingController::new(MotorProfile::AC_50HZ, 0.5, RelayConfig::DEFAULT).unwrap(),
        settings(plant),
        ActuatorConfig {
            duty: plant.duty,
            ..ActuatorConfig::DEFAULT
        },
        [0, 0],
    )
    .unwrap()
}

#[test]
fn sky_deviation_handles_wrap_zenith_and_over_the_top_equivalence() {
    for (a, b, expected) in [
        (Pointing::new(359.0, 0.0), Pointing::new(1.0, 0.0), 2.0),
        (Pointing::new(0.0, 0.0), Pointing::new(180.0, 0.0), 180.0),
        (Pointing::new(450.0, 20.0), Pointing::new(90.0, 20.0), 0.0),
        (Pointing::new(12.0, 90.0), Pointing::new(240.0, 90.0), 0.0),
        (Pointing::new(20.0, 30.0), Pointing::new(200.0, 150.0), 0.0),
        // Near zenith, 180° of azimuth is only 2° of actual pointing deviation.
        (Pointing::new(0.0, 89.0), Pointing::new(180.0, 89.0), 2.0),
    ] {
        close(pointing_deviation_deg(a, b).unwrap(), expected);
        close(pointing_deviation_deg(b, a).unwrap(), expected);
    }
    for bad in [f32::NAN, f32::INFINITY, -1.0, 451.0] {
        assert_eq!(
            pointing_deviation_deg(Pointing::new(bad, 0.0), Pointing::new(0.0, 0.0)),
            None
        );
    }
}

#[test]
fn profile_and_installed_travel_are_checked_before_simulation() {
    let mut bad = MotorProfile::AC_50HZ;
    bad.azimuth.rate_deg_per_s = f32::NAN;
    assert_eq!(
        MotorModel::new(
            bad,
            FULL_TRAVEL_CALIBRATION,
            Pointing::new(100.0, 45.0),
            [0, 0]
        ),
        Err(SimulationError::InvalidProfile)
    );
    assert_eq!(
        MotorModel::new(
            MotorProfile::AC_50HZ,
            FULL_TRAVEL_CALIBRATION,
            Pointing::new(100.0, 181.0),
            [0, 0]
        ),
        Err(SimulationError::InvalidPointing)
    );
    let mut narrow = FULL_TRAVEL_CALIBRATION;
    narrow.elevation.angle_max_deg = 90.0;
    assert_eq!(
        MotorModel::new(
            MotorProfile::AC_50HZ,
            narrow,
            Pointing::new(100.0, 120.0),
            [0, 0]
        ),
        Err(SimulationError::InvalidPointing)
    );
    assert!(MotorProfile::AC_SLOW.is_valid());
    assert!(MotorProfile::AC_FAST.is_valid());
}

#[test]
fn startup_and_post_release_coast_are_included_in_actual_position() {
    let mut profile = ideal_profile(10.0);
    profile.azimuth.startup_ms = 300;
    profile.azimuth.coast_ms = 500;
    let mut motor = model(profile);
    motor.advance(CW, 200).unwrap();
    close(motor.position().azimuth_deg, 100.0);
    motor.advance(CW, 300).unwrap();
    close(motor.position().azimuth_deg, 102.0);
    // A zero-time state observation must not erase velocity before release.
    motor.advance(CW, 0).unwrap();
    motor.advance(DriveCommand::IDLE, 0).unwrap();
    assert!(!motor.is_settled());
    motor.advance(DriveCommand::IDLE, 500).unwrap();
    close(motor.position().azimuth_deg, 107.0);
    assert!(motor.is_settled());
    motor.advance(DriveCommand::IDLE, 1_000).unwrap();
    close(motor.position().azimuth_deg, 107.0);
    assert_eq!(motor.used_duty_ms(), [500, 0]);

    let mut too_short = model(profile);
    too_short.advance(CW, 200).unwrap();
    too_short.advance(DriveCommand::IDLE, 1_000).unwrap();
    close(too_short.position().azimuth_deg, 100.0);
}

#[test]
fn integration_is_independent_of_step_partition_and_axes_run_concurrently() {
    let drive = DriveCommand {
        elevation: Some(ElevationDirection::Up),
        ..CW
    };
    let mut whole = model(MotorProfile::AC_50HZ);
    let mut sliced = whole;
    whole.advance(drive, 2_000).unwrap();
    for _ in 0..100 {
        sliced.advance(drive, 20).unwrap();
    }
    whole.advance(DriveCommand::IDLE, 800).unwrap();
    for _ in 0..40 {
        sliced.advance(DriveCommand::IDLE, 20).unwrap();
    }
    close(whole.position().azimuth_deg, sliced.position().azimuth_deg);
    close(
        whole.position().elevation_deg,
        sliced.position().elevation_deg,
    );
    assert_eq!(whole.used_duty_ms(), [2_000, 2_000]);
    assert_eq!(whole.used_duty_ms(), sliced.used_duty_ms());
}

#[test]
fn backlash_take_up_survives_pulse_gaps_and_partial_reversals() {
    let mut profile = ideal_profile(10.0);
    profile.azimuth.backlash_deg = 1.0;
    let mut motor = model(profile);
    motor.advance(CW, 100).unwrap();
    close(motor.position().azimuth_deg, 101.0);
    for _ in 0..2 {
        motor.advance(DriveCommand::IDLE, 200).unwrap();
        motor.advance(CCW, 40).unwrap();
        close(motor.position().azimuth_deg, 101.0);
    }
    motor.advance(DriveCommand::IDLE, 200).unwrap();
    motor.advance(CCW, 40).unwrap();
    close(motor.position().azimuth_deg, 100.8);
    // Return through only part of the slack, then take up that same part again.
    motor.advance(CW, 20).unwrap();
    motor.advance(CCW, 20).unwrap();
    close(motor.position().azimuth_deg, 100.8);
    motor.advance(CCW, 10).unwrap();
    close(motor.position().azimuth_deg, 100.7);
}

#[test]
fn reversal_waits_for_coast_and_a_new_startup() {
    let mut profile = ideal_profile(10.0);
    profile.azimuth.startup_ms = 100;
    profile.azimuth.coast_ms = 200;
    let mut motor = model(profile);
    motor.advance(CW, 200).unwrap();
    motor.advance(CCW, 400).unwrap();
    // +1° powered, +2° old-direction coast, 100 ms startup, -1° powered.
    close(motor.position().azimuth_deg, 102.0);
}

#[test]
fn brief_idle_does_not_replenish_duty_and_full_rest_is_per_axis() {
    let mut profile = ideal_profile(1.0);
    profile.duty.energized_ms = 1_000;
    profile.duty.rest_ms = 2_000;
    let mut motor = MotorModel::new(
        profile,
        FULL_TRAVEL_CALIBRATION,
        Pointing::new(100.0, 45.0),
        [900, 0],
    )
    .unwrap();
    motor.advance(CW, 100).unwrap();
    let mut rested = motor;
    motor.advance(DriveCommand::IDLE, 1_999).unwrap();
    assert_eq!(motor.used_duty_ms(), [1_000, 0]);
    assert_eq!(
        motor.advance(CW, 1),
        Err(SimulationError::DutyLimit(Axis::Azimuth))
    );
    assert_eq!(
        motor.advance(DriveCommand::IDLE, 2_000),
        Err(SimulationError::DutyLimit(Axis::Azimuth))
    );
    rested.advance(DriveCommand::IDLE, 2_000).unwrap();
    assert_eq!(rested.used_duty_ms(), [0, 0]);
    rested.advance(CW, 100).unwrap();
}

#[test]
fn coast_can_cross_a_limit_after_the_relay_has_been_released() {
    let mut profile = ideal_profile(10.0);
    profile.azimuth.coast_ms = 500;
    let mut motor = MotorModel::new(
        profile,
        FULL_TRAVEL_CALIBRATION,
        Pointing::new(449.0, 45.0),
        [0, 0],
    )
    .unwrap();
    motor.advance(CW, 100).unwrap();
    assert_eq!(
        motor.advance(DriveCommand::IDLE, 1),
        Err(SimulationError::TravelLimit(Axis::Azimuth))
    );
    assert_eq!(
        motor.advance(CCW, 100),
        Err(SimulationError::TravelLimit(Axis::Azimuth))
    );
}

#[test]
fn follower_requires_current_feedback_and_preserves_continuous_coordinates() {
    let mut controller =
        FollowingController::new(MotorProfile::AC_50HZ, 0.5, RelayConfig::DEFAULT).unwrap();
    let position = Pointing::new(359.0, 45.0);
    let feedback = full_travel_sample(position, 100);
    assert_eq!(
        controller.command(
            Pointing::new(362.0, 45.0),
            Some(feedback),
            DriveCommand::IDLE,
            100
        ),
        Command::Drive(CW)
    );
    // The planner must unwrap the sky bearing. The follower cannot choose a
    // short route that crosses the installed stop behind the caller's back.
    assert_eq!(
        controller.command(
            Pointing::new(1.0, 45.0),
            Some(feedback),
            DriveCommand::IDLE,
            100
        ),
        Command::Drive(CCW)
    );
    for now in [99, 101 + FEEDBACK_MAX_AGE_MS] {
        assert_eq!(
            controller.command(position, Some(feedback), CW, now),
            Command::Inhibit
        );
    }
    assert_eq!(
        controller.command(position, None, CW, 100),
        Command::Inhibit
    );
}

#[test]
fn simulated_feedback_is_quantized_and_delayed_before_any_activation() {
    let mut sim = simulation(Pointing::new(100.11, 45.07), MotorProfile::AC_50HZ);
    loop {
        let now = sim.now_ms();
        let sample = sim.step(Some(Pointing::new(120.0, 45.0))).unwrap();
        assert_eq!(sample.at_ms, now);
        if now < 100 {
            assert_eq!(sample.feedback, None);
            assert_eq!(sample.applied, DriveCommand::IDLE);
        } else {
            let feedback = sample.feedback.unwrap();
            assert_eq!(feedback.sampled_at_ms(), 0);
            close(feedback.position().azimuth_deg, 100.0);
            close(feedback.position().elevation_deg, 45.0);
            assert_eq!(sample.applied, CW);
            break;
        }
    }
}

#[test]
fn channel_skew_preserves_oldest_timestamp_and_each_channels_capture_position() {
    let mut plant = ideal_profile(2.0);
    plant.azimuth.rate_deg_per_s = 4.0;
    let mut sim = Simulation::new(
        FULL_TRAVEL_CALIBRATION,
        Pointing::new(100.0, 45.0),
        FollowingController::new(MotorProfile::AC_50HZ, 0.15, RelayConfig::DEFAULT).unwrap(),
        SimulationSettings {
            step_ms: 50,
            scenario: Scenario {
                latency_ms: 150,
                channel_skew_ms: 150,
                ..settings(plant).scenario
            },
        },
        ACTUATOR,
        [0, 0],
    )
    .unwrap();
    let mut elevation_at_capture = 0.0;
    loop {
        let now = sim.now_ms();
        let sample = sim.step(Some(Pointing::new(200.0, 90.0))).unwrap();
        if now == 250 {
            elevation_at_capture = sample.position.elevation_deg;
        }
        if now == 400 {
            let feedback = sample.feedback.unwrap();
            assert_eq!(feedback.sampled_at_ms(), 250);
            assert!((feedback.position().azimuth_deg - sample.position.azimuth_deg).abs() < 0.17);
            assert!((feedback.position().elevation_deg - elevation_at_capture).abs() < 0.11);
            assert!(
                (feedback.position().elevation_deg - sample.position.elevation_deg).abs() > 0.19
            );
            break;
        }
        assert!(now < 400);
    }
}

#[test]
fn noisy_feedback_is_applied_before_adc_quantization_and_alternates_per_read() {
    let mut sim = Simulation::new(
        FULL_TRAVEL_CALIBRATION,
        Pointing::new(100.11, 45.07),
        FollowingController::new(MotorProfile::AC_50HZ, 0.15, RelayConfig::DEFAULT).unwrap(),
        SimulationSettings {
            step_ms: 20,
            scenario: Scenario {
                latency_ms: 0,
                noise_codes: [1, -1],
                alternate_noise: true,
                ..Scenario::NOMINAL
            },
        },
        ACTUATOR,
        [0, 0],
    )
    .unwrap();
    let first = sim.step(None).unwrap().feedback.unwrap();
    while sim.now_ms() < 100 {
        sim.step(None).unwrap();
    }
    let second = sim.step(None).unwrap().feedback.unwrap();
    close(
        first.position().azimuth_deg - second.position().azimuth_deg,
        2.0 / 3.0,
    );
    // Elevation voltage decreases as the mechanical angle increases.
    close(
        first.position().elevation_deg - second.position().elevation_deg,
        0.4,
    );
    assert_eq!(first.sampled_at_ms(), 0);
    assert_eq!(second.sampled_at_ms(), 100);
}

#[test]
fn pulse_aware_follower_avoids_reversal_chatter_with_both_relay_policies() {
    for relays in [
        ezal_core::drive::RelayConfig::DEFAULT,
        ezal_core::drive::RelayConfig::TRACKING_CANDIDATE,
    ] {
        for plant in [
            MotorProfile::AC_SLOW,
            MotorProfile::AC_50HZ,
            MotorProfile::AC_FAST,
        ] {
            let mut sim = Simulation::new(
                FULL_TRAVEL_CALIBRATION,
                Pointing::new(180.0, 10.0),
                FollowingController::new(MotorProfile::AC_50HZ, 0.15, relays).unwrap(),
                settings(plant),
                ActuatorConfig { relays, ..ACTUATOR },
                [0, 0],
            )
            .unwrap();
            let mut peak = 0.0_f32;
            while sim.now_ms() < 600_000 {
                let target = Pointing::new(180.0, 10.0 + 160.0 * sim.now_ms() as f32 / 600_000.0);
                peak = peak.max(deviation(sim.step(Some(target)).unwrap(), target));
            }
            sim.step(None).unwrap();
            while !sim.is_settled() {
                sim.step(None).unwrap();
            }
            let summary = sim.summary();
            assert!(peak < 1.5, "{peak} {summary:?}");
            assert_eq!(summary.reversals, [0, 0]);
            assert_eq!(summary.energized_ms[0], 0);
            assert!(summary.energized_ms[1] < 70_000);
        }
    }
}

#[test]
fn stationary_target_needs_no_drive_and_cancellation_holds_after_coast() {
    let target = Pointing::new(100.0, 45.0);
    let mut sim = simulation(target, MotorProfile::AC_50HZ);
    while sim.now_ms() < 1_000 {
        let sample = sim.step(Some(target)).unwrap();
        assert_eq!(sample.applied, DriveCommand::IDLE);
        close(deviation(sample, target), 0.0);
    }
    assert_eq!(sim.summary().energized_ms, [0, 0]);
    while sim.now_ms() < 2_000 {
        sim.step(Some(Pointing::new(200.0, 45.0))).unwrap();
    }
    assert!(sim.actuator().actual().is_active());
    let cancelled = sim.step(None).unwrap();
    assert_eq!(cancelled.applied, DriveCommand::IDLE);
    assert!(!sim.is_settled());
    while sim.now_ms() < 3_000 {
        assert_eq!(sim.step(None).unwrap().applied, DriveCommand::IDLE);
    }
    assert!(sim.is_settled());
    let held = sim.position();
    while sim.now_ms() < 5_000 {
        sim.step(None).unwrap();
    }
    assert_eq!(sim.position(), held);
}

#[test]
fn actual_actuator_catches_stalls_and_implausible_speed() {
    for (rate, expected) in [
        (0.0, MotionFault::AzimuthNoProgress),
        (20.0, MotionFault::AzimuthImplausibleSpeed),
    ] {
        let mut sim = simulation(Pointing::new(100.0, 45.0), ideal_profile(rate));
        let error = loop {
            assert!(sim.now_ms() < 7_000);
            if let Err(error) = sim.step(Some(Pointing::new(300.0, 45.0))) {
                break error;
            }
        };
        assert_eq!(error, SimulationError::Motion(expected));
        assert_eq!(sim.actuator().actual(), DriveCommand::IDLE);
        assert_eq!(sim.step(None), Err(error));
    }
}

#[test]
fn simulator_checks_duty_at_its_exact_deadline_and_latches_failure() {
    let mut profile = ideal_profile(1.0);
    profile.duty.energized_ms = 1_005;
    let mut sim = simulation(Pointing::new(100.0, 45.0), profile);
    loop {
        if let Err(error) = sim.step(Some(Pointing::new(300.0, 45.0))) {
            assert_eq!(error, SimulationError::DutyLimit(Axis::Azimuth));
            assert_eq!(sim.now_ms(), 1_105); // first activation was at 100 ms
            assert_eq!(sim.summary().energized_ms, [1_005, 0]);
            assert_eq!(sim.actuator().actual(), DriveCommand::IDLE);
            break;
        }
        assert!(sim.now_ms() <= 1_105);
    }
}

#[test]
fn complete_over_the_top_pass_is_followed_without_azimuth_reversal() {
    let mut sim = simulation(Pointing::new(180.0, 10.0), MotorProfile::AC_50HZ);
    let mut peak = 0.0_f32;
    while sim.now_ms() < 600_000 {
        let target = Pointing::new(180.0, 10.0 + 160.0 * sim.now_ms() as f32 / 600_000.0);
        peak = peak.max(deviation(sim.step(Some(target)).unwrap(), target));
    }
    sim.step(None).unwrap();
    while !sim.is_settled() {
        sim.step(None).unwrap();
    }
    let summary = sim.summary();
    assert_eq!(summary.energized_ms[0], 0);
    assert_eq!(summary.reversals[0], 0);
    assert!(summary.energized_ms[1] > 0);
    assert!(summary.energized_ms[1] < MotorProfile::AC_50HZ.duty.energized_ms);
    assert!(peak <= 2.0, "{peak}");
    assert!(sim.position().elevation_deg > 165.0);
}

#[test]
fn impossible_target_rate_is_reported_as_deviation_without_a_false_motion_fault() {
    let mut sim = simulation(Pointing::new(100.0, 45.0), MotorProfile::AC_SLOW);
    let mut peak = 0.0_f32;
    while sim.now_ms() < 10_000 {
        let target = Pointing::new(100.0 + sim.now_ms() as f32 / 100.0, 45.0);
        peak = peak.max(deviation(sim.step(Some(target)).unwrap(), target));
    }
    assert!(peak > 30.0, "{peak}");
    sim.step(None).unwrap();
    assert_eq!(sim.actuator().actual(), DriveCommand::IDLE);
}

#[test]
fn intentional_keyhole_offset_is_followed_on_the_route_not_the_sky() {
    let mechanical = Pointing::new(180.0, 90.0);
    let mut sim = simulation(mechanical, MotorProfile::AC_50HZ);
    let target = TrackingTarget {
        mechanical,
        sky: Pointing::new(180.0, 87.5),
    };
    while sim.now_ms() < 1_000 {
        let sample = sim.step_target(Some(target)).unwrap();
        // The motor path is followed exactly; the planned offset remains.
        assert_eq!(sample.applied, DriveCommand::IDLE);
        close(deviation(sample, target.sky), 2.5);
    }
}

#[test]
fn invalid_simulation_inputs_never_create_or_retain_output() {
    let controller =
        FollowingController::new(MotorProfile::AC_50HZ, 0.5, RelayConfig::DEFAULT).unwrap();
    let nominal = settings(MotorProfile::AC_50HZ);
    for settings in [
        SimulationSettings {
            step_ms: 0,
            ..nominal
        },
        SimulationSettings {
            scenario: Scenario {
                latency_ms: 151,
                ..nominal.scenario
            },
            ..nominal
        },
    ] {
        assert!(matches!(
            Simulation::new(
                FULL_TRAVEL_CALIBRATION,
                Pointing::new(100.0, 45.0),
                controller,
                settings,
                ACTUATOR,
                [0, 0],
            ),
            Err(SimulationError::InvalidSettings)
        ));
    }
    let mut sim = simulation(Pointing::new(100.0, 45.0), MotorProfile::AC_50HZ);
    while sim.now_ms() < 1_000 {
        sim.step(Some(Pointing::new(200.0, 45.0))).unwrap();
    }
    assert!(sim.actuator().actual().is_active());
    assert_eq!(
        sim.step(Some(Pointing::new(451.0, 45.0))),
        Err(SimulationError::InvalidPointing)
    );
    assert_eq!(sim.actuator().actual(), DriveCommand::IDLE);
    assert_eq!(
        sim.step(Some(Pointing::new(200.0, 45.0))),
        Err(SimulationError::InvalidPointing)
    );
}

#[test]
fn cancellation_wins_over_a_pending_activation_at_its_deadline() {
    let target = Pointing::new(200.0, 45.0);
    let mut sim = simulation(Pointing::new(100.0, 45.0), MotorProfile::AC_50HZ);
    while sim.now_ms() < 200 {
        sim.step(Some(target)).unwrap();
    }
    assert_eq!(sim.step(None).unwrap().applied, DriveCommand::IDLE);
    // Initial activation at 100 ms was inhibited at 200 ms. Fresh requests
    // cannot energize during the production actuator's off-time until 2200 ms.
    while sim.now_ms() < 2_200 {
        assert_eq!(sim.step(Some(target)).unwrap().applied, DriveCommand::IDLE);
    }
    assert_eq!(sim.now_ms(), 2_200);
    while sim.now_ms() < 3_000 {
        assert_eq!(sim.step(None).unwrap().applied, DriveCommand::IDLE);
    }
    assert_eq!(sim.summary().energized_ms, [100, 0]);
    assert_eq!(sim.summary().transitions, [2, 0]);
}

#[test]
fn ordinary_follower_release_respects_production_minimum_on_and_off_times() {
    let mut sim = simulation(Pointing::new(100.0, 45.0), ideal_profile(1.0));
    while sim.now_ms() <= 100 {
        sim.step(Some(Pointing::new(200.0, 45.0))).unwrap();
    }
    while sim.now_ms() < 600 {
        assert_eq!(
            sim.step(Some(Pointing::new(100.0, 45.0))).unwrap().applied,
            CW
        );
    }
    assert_eq!(
        sim.step(Some(Pointing::new(100.0, 45.0))).unwrap().applied,
        DriveCommand::IDLE
    );
    while sim.now_ms() < 2_600 {
        assert_eq!(
            sim.step(Some(Pointing::new(200.0, 45.0))).unwrap().applied,
            DriveCommand::IDLE
        );
    }
    assert_eq!(
        sim.step(Some(Pointing::new(200.0, 45.0))).unwrap().applied,
        CW
    );
}

#[test]
fn recorded_orbit_replays_through_the_real_actuator_until_los() {
    use ezal_core::orbit::{GroundStation, Observer, Update};
    let mut update = Update::new(1_791_385_200);
    update.push(include_str!("fixtures/weather-2026-10-07.tle").as_bytes());
    let orbits = update.finish();
    let orbit = orbits
        .iter()
        .flatten()
        .find(|orbit| orbit.satellite.norad_id == 39260)
        .unwrap();
    let station = GroundStation::new(Observer::CLEVEDON).unwrap();
    // Window independently checked in passes.rs. Offline geometry, not a claim
    // that this fixture satellite is currently transmitting.
    let aos = 1_791_385_770.0;
    let duration_ms = 777_000;
    let initial = orbit.look_at(&station, aos).unwrap();
    let mut previous_azimuth = initial.azimuth_deg;
    let mut sim = simulation(
        Pointing::new(initial.azimuth_deg as f32, initial.elevation_deg as f32),
        MotorProfile::AC_50HZ,
    );
    let mut peak = 0.0_f32;
    while sim.now_ms() < duration_ms {
        let look = orbit
            .look_at(&station, aos + sim.now_ms() as f64 / 1_000.0)
            .unwrap();
        let azimuth = previous_azimuth
            + (look.azimuth_deg - previous_azimuth + 180.0).rem_euclid(360.0)
            - 180.0;
        previous_azimuth = azimuth;
        let target = TrackingTarget {
            mechanical: Pointing::new(azimuth as f32, look.elevation_deg as f32),
            sky: Pointing::new(look.azimuth_deg as f32, look.elevation_deg as f32),
        };
        peak = peak.max(deviation(
            sim.step_target(Some(target)).unwrap(),
            target.sky,
        ));
    }
    assert_eq!(sim.step(None).unwrap().applied, DriveCommand::IDLE);
    while !sim.is_settled() {
        sim.step(None).unwrap();
    }
    let final_sky = orbit
        .look_at(&station, aos + duration_ms as f64 / 1_000.0)
        .unwrap();
    assert!(
        pointing_deviation_deg(
            sim.position(),
            Pointing::new(final_sky.azimuth_deg as f32, final_sky.elevation_deg as f32)
        )
        .unwrap()
            < 5.0
    );
    assert!(peak < 10.0, "{peak}");
    for used in sim.summary().energized_ms {
        assert!(used > 0 && used < MotorProfile::AC_50HZ.duty.energized_ms);
    }
}

#[test]
fn candidate_timing_preserves_healthy_coast_and_reversal_with_delayed_feedback() {
    use ezal_core::drive::RelayConfig;
    for profile in [
        MotorProfile::AC_SLOW,
        MotorProfile::AC_50HZ,
        MotorProfile::AC_FAST,
    ] {
        let mut sim = Simulation::new(
            FULL_TRAVEL_CALIBRATION,
            Pointing::new(180.0, 60.0),
            FollowingController::new(MotorProfile::AC_50HZ, 0.5, RelayConfig::DEFAULT).unwrap(),
            SimulationSettings {
                step_ms: 20,
                scenario: Scenario {
                    motor: profile,
                    latency_ms: 150,
                    ..Scenario::NOMINAL
                },
            },
            ActuatorConfig {
                relays: RelayConfig::TRACKING_CANDIDATE,
                ..ACTUATOR
            },
            [0, 0],
        )
        .unwrap();
        while sim.now_ms() < 12_000 {
            let target = match sim.now_ms() % 4_000 {
                0..1_000 => Some(Pointing::new(300.0, 120.0)),
                2_000..3_000 => Some(Pointing::new(60.0, 20.0)),
                _ => None,
            };
            sim.step(target).unwrap();
        }
        while !sim.is_settled() {
            sim.step(None).unwrap();
        }
        let summary = sim.summary();
        assert!(summary.reversals.iter().all(|n| *n >= 5), "{summary:?}");
        assert!(summary.energized_ms.iter().all(|ms| *ms <= 6_000));
    }
}
