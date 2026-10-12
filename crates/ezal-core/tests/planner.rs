use ezal_core::actuator::{Actuator, ActuatorConfig, ActuatorInput};
use ezal_core::ads1015::FEEDBACK_FULL_SCALE;
use ezal_core::drive::{
    AzimuthDirection, Command, CommandEnvelope, DriveCommand, RelayConfig, MOVEMENT_LEASE_MS,
};
use ezal_core::duty::{DutyConfig, DutyHistory};
use ezal_core::feedback::FeedbackSample;
use ezal_core::interlock::FeedbackInterlock;
use ezal_core::network::NetworkGate;
use ezal_core::orbit::LookAngle;
use ezal_core::passes::Ephemeris;
use ezal_core::position::{AxisCalibration, InstalledCalibration, Pointing, PositionCalibration};
use ezal_core::tracking::{
    CandidateFailure, FollowingController, MotorProfile, Planner, PlanningConfig, PlanningError,
    PlanningReport, PlanningWindow, PreparationTiming, RestingStart, RouteKind, Scenario,
    Simulation, SimulationError, SimulationSettings, MAX_PLANNING_STEPS, PARTIAL_ADMISSION_LEAD_MS,
};
use std::cell::Cell;

#[path = "common/tracking_partial.rs"]
mod partial;

#[path = "common/tracking_plan_acceptance.rs"]
mod acceptance;

const CAL: PositionCalibration = PositionCalibration {
    azimuth: AxisCalibration {
        angle_min_deg: 0.0,
        angle_max_deg: 450.0,
        voltage_min_mv: 200,
        voltage_max_mv: 2900,
    },
    elevation: AxisCalibration {
        angle_min_deg: 0.0,
        angle_max_deg: 180.0,
        voltage_min_mv: 2800,
        voltage_max_mv: 1000,
    },
};

fn snapshot(cal: PositionCalibration, pose: Pointing, used: [u64; 2]) -> RestingStart {
    let installed = InstalledCalibration::new(cal, FEEDBACK_FULL_SCALE).unwrap();
    let mut actuator = Actuator::with_config(
        ActuatorConfig {
            duty: DutyConfig::CONFIRMED_AC,
            ..ActuatorConfig::DEFAULT
        },
        DutyHistory::Known(used),
    );
    let mut network = NetworkGate::new();
    let mut feedback = None;
    for at_ms in [0, 2000] {
        feedback = Some(
            FeedbackSample::from_voltages(
                installed,
                cal.position_to_feedback(pose).unwrap(),
                at_ms,
            )
            .unwrap(),
        );
        let mut interlock = FeedbackInterlock::new(installed);
        interlock.publish(feedback);
        network.observe(at_ms, true);
        actuator.step(
            ActuatorInput {
                interlock,
                network,
                command: None,
            },
            at_ms,
        );
    }
    RestingStart {
        at_ms: 2000,
        feedback: feedback.unwrap(),
        actuator,
        network,
    }
}

fn nominal() -> PlanningConfig {
    let mut config = PlanningConfig::default();
    config.scenarios.clear();
    config.scenarios.push(Scenario::NOMINAL).unwrap();
    config
}

fn window(duration_ms: u32) -> PlanningWindow {
    PlanningWindow {
        aos_unix_s: 0.0,
        aos_at_ms: 122_000,
        duration_ms,
    }
}

fn run<E: Ephemeris>(mut planner: Planner<E>) -> PlanningReport {
    while !planner.is_finished() {
        planner.step().unwrap();
    }
    let report = planner.take_report().unwrap();
    assert!(report.work_steps <= MAX_PLANNING_STEPS);
    assert!((1..=48).contains(&(report.evaluated_candidates + report.geometry_rejections)));
    report
}

fn zenith(time_s: f64) -> Option<LookAngle> {
    let angle = 10.0 + 160.0 * time_s / 600.0;
    Some(LookAngle {
        azimuth_deg: if angle <= 90.0 { 180.0 } else { 0.0 },
        elevation_deg: if angle <= 90.0 { angle } else { 180.0 - angle },
    })
}

#[test]
fn over_top_crosses_zenith_without_a_keyhole_in_all_default_scenarios() {
    let start = snapshot(CAL, Pointing::new(180.0, 10.0), [0, 0]);
    let report = run(Planner::new(
        zenith,
        window(600_000),
        CAL,
        start,
        PlanningConfig::default(),
    )
    .unwrap());
    let plan = report.plan.as_ref().unwrap_or_else(|| panic!("{report:?}"));
    assert!(
        plan.reception().is_complete(),
        "a complete OTT route outranks partial recovery"
    );
    assert!(matches!(
        plan.route.choice().kind,
        RouteKind::OverTop { .. }
    ));
    assert_eq!(plan.assessment.completed_scenarios, 9);
    assert!(plan.assessment.bounded_peak_deg <= 2.0);
    assert_eq!(plan.assessment.gap_time_ms, 0);
    assert!(plan.assessment.gaps.is_empty());
    assert!(plan.assessment.energized_ms[1] < 100_000);
    let mut previous = plan.route.pointing_at(0).unwrap();
    for time in (20..=600_000).step_by(20) {
        let position = plan.route.pointing_at(time).unwrap();
        assert!(position.is_valid_for(CAL));
        assert!((position.azimuth_deg - previous.azimuth_deg).abs() < 0.1);
        assert!((position.elevation_deg - previous.elevation_deg).abs() < 0.1);
        previous = position;
    }
    assert!(plan.route.pointing_at(600_001).is_none());
    println!(
        "zenith: {:?}, points {}, steps {}, planner {} bytes",
        plan.assessment,
        plan.route.point_count(),
        report.work_steps,
        std::mem::size_of::<Planner<fn(f64) -> Option<LookAngle>>>()
    );
}

#[test]
fn conventional_only_travel_reports_keyhole_gaps_instead_of_a_plan() {
    let mut cal = CAL;
    cal.elevation.angle_max_deg = 90.0;
    let report = run(Planner::new(
        zenith,
        window(600_000),
        cal,
        snapshot(cal, Pointing::new(180.0, 10.0), [0, 0]),
        nominal(),
    )
    .unwrap());
    assert!(report.plan.is_none());
    let attempt = report.closest_attempt.unwrap();
    assert_eq!(attempt.failure, Some(CandidateFailure::PointingBudget));
    assert!(attempt.sampled_peak_deg > 5.0);
    assert!(attempt.gap_time_ms > 0);
    assert!(attempt.gap_time_ms <= 600_000);
    assert!(!attempt.gaps.is_empty());
    assert!(attempt
        .gaps
        .iter()
        .all(|gap| gap.start_ms <= gap.end_ms && gap.end_ms <= 600_000));
}

#[test]
fn azimuth_wrap_is_a_continuous_overlap_and_each_step_has_at_most_one_propagation() {
    let calls = Cell::new(0);
    let source = |time: f64| {
        calls.set(calls.get() + 1);
        Some(LookAngle {
            azimuth_deg: (350.0 + time / 6.0).rem_euclid(360.0),
            elevation_deg: 30.0,
        })
    };
    let mut planner = Planner::new(
        source,
        window(120_000),
        CAL,
        snapshot(CAL, Pointing::new(350.0, 30.0), [0, 0]),
        nominal(),
    )
    .unwrap();
    while !planner.is_finished() {
        let before = calls.get();
        planner.step().unwrap();
        assert!(calls.get() - before <= 1);
    }
    let report = planner.take_report().unwrap();
    let plan = report
        .plan
        .unwrap_or_else(|| panic!("{:?}", report.closest_attempt));
    assert_eq!(plan.route.choice().kind, RouteKind::Conventional);
    assert!((plan.route.pointing_at(120_000).unwrap().azimuth_deg - 370.0).abs() < 0.01);
    let middle = plan.route.pointing_at(60_000).unwrap();
    assert!((middle.azimuth_deg - 360.0).abs() < 0.01);
}

#[test]
fn preparation_time_and_duty_include_the_slew_and_do_not_reset_at_aos() {
    let source = |_: f64| {
        Some(LookAngle {
            azimuth_deg: 300.0,
            elevation_deg: 60.0,
        })
    };
    let mut cal = CAL;
    cal.elevation.angle_max_deg = 90.0;
    let report = run(Planner::new(
        source,
        window(10_000),
        cal,
        snapshot(cal, Pointing::new(100.0, 10.0), [20_000, 20_000]),
        nominal(),
    )
    .unwrap());
    let plan = report
        .plan
        .unwrap_or_else(|| panic!("{:?}", report.closest_attempt));
    assert!(plan.assessment.preparation_ms > 40_000);
    assert_eq!(
        plan.assessment.preparation_lead_ms,
        plan.assessment.preparation_ms + 20_000
    );
    assert!(plan.assessment.energized_ms[0] > 30_000);
    assert!(plan.assessment.energized_ms[1] > 20_000);

    let report = run(Planner::new(
        source,
        window(10_000),
        cal,
        snapshot(cal, Pointing::new(100.0, 10.0), [295_000, 295_000]),
        nominal(),
    )
    .unwrap());
    assert!(report.plan.is_none());
    assert!(matches!(
        report.closest_attempt.unwrap().failure,
        Some(CandidateFailure::Simulation(SimulationError::DutyLimit(_)))
    ));
}

#[test]
fn insufficient_lead_time_and_already_started_passes_cannot_claim_full_support() {
    let source = |_: f64| {
        Some(LookAngle {
            azimuth_deg: 300.0,
            elevation_deg: 60.0,
        })
    };
    let start = snapshot(CAL, Pointing::new(100.0, 10.0), [0, 0]);
    let mut time = window(10_000);
    for aos_at_ms in [2500, 1999] {
        time.aos_at_ms = aos_at_ms;
        let report = run(Planner::new(source, time, CAL, start, nominal()).unwrap());
        assert!(report.plan.is_none());
        // Only a late intercept at the fixed entry remains, and none reaches the target.
        let attempt = report.closest_attempt.unwrap();
        assert_eq!(
            attempt.prepare_at_ms,
            start.at_ms + PARTIAL_ADMISSION_LEAD_MS
        );
        assert_eq!(attempt.failure, Some(CandidateFailure::NoRecovery));
    }
}

#[test]
fn future_wait_replenishes_partial_duty_before_preparation_without_simulating_hours() {
    let source = |_: f64| {
        Some(LookAngle {
            azimuth_deg: 300.0,
            elevation_deg: 60.0,
        })
    };
    let mut time = window(10_000);
    for aos in [1_000_000, 86_400_000] {
        time.aos_at_ms = aos;
        let report = run(Planner::new(
            source,
            time,
            CAL,
            snapshot(CAL, Pointing::new(100.0, 10.0), [295_000, 295_000]),
            nominal(),
        )
        .unwrap());
        assert!(report.plan.is_some(), "{report:?}");
        assert!(report.work_steps < 100_000);
    }
}

#[test]
fn recorded_pass_includes_real_sky_error_and_reports_noisy_preparation_failure() {
    use ezal_core::orbit::{Catalog, GroundStation, Observer};
    use ezal_core::passes::Track;
    use ezal_core::tracking::EvaluationPhase;
    let mut catalog = Catalog::new();
    catalog.update(
        include_str!("fixtures/weather-2026-10-07.tle"),
        1_791_385_200,
    );
    let orbit = catalog
        .orbits()
        .find(|orbit| orbit.satellite.norad_id == 39260)
        .unwrap();
    let station = GroundStation::new(Observer::CLEVEDON).unwrap();
    let time = PlanningWindow {
        aos_unix_s: 1_791_385_770.0,
        duration_ms: 777_000,
        ..window(1)
    };
    let start = snapshot(CAL, Pointing::new(180.0, 10.0), [0, 0]);
    let quiet = run(Planner::new(
        Track {
            orbit,
            station: &station,
        },
        time,
        CAL,
        start,
        nominal(),
    )
    .unwrap());
    assert!(quiet.plan.is_none());
    let attempt = quiet.closest_attempt.unwrap();
    assert_eq!(attempt.completed_scenarios, 1);
    assert_eq!(attempt.failure, Some(CandidateFailure::PointingBudget));
    assert!(
        attempt.sampled_peak_deg > 2.0 && attempt.sampled_peak_deg < 3.0,
        "{attempt:?}"
    );
    assert!(attempt.bounded_peak_deg > attempt.sampled_peak_deg);
    assert!(attempt.energized_ms.iter().all(|used| *used > 20_000));
    let noisy = run(Planner::new(
        Track {
            orbit,
            station: &station,
        },
        time,
        CAL,
        start,
        PlanningConfig::default(),
    )
    .unwrap());
    assert!(noisy.plan.is_none());
    let attempt = noisy.closest_attempt.unwrap();
    assert!(matches!(
        attempt.failure,
        Some(CandidateFailure::Simulation(SimulationError::Motion(_)))
    ));
    assert_eq!(attempt.failure_phase, Some(EvaluationPhase::Preparation));
    let scenario = PlanningConfig::default().scenarios[attempt.failure_scenario.unwrap()];
    assert!(scenario.alternate_noise);
    assert!(attempt.energized_ms.iter().any(|used| *used > 0));
}

#[test]
fn prepare_now_does_not_borrow_future_rest_and_can_hold_after_readiness() {
    let source = |_: f64| {
        Some(LookAngle {
            azimuth_deg: 300.0,
            elevation_deg: 60.0,
        })
    };
    let time = PlanningWindow {
        aos_at_ms: 1_000_000,
        ..window(10_000)
    };
    let mut config = nominal();
    config.preparation_timing = PreparationTiming::Immediate;
    let start = snapshot(CAL, Pointing::new(100.0, 10.0), [0, 0]);
    let report = run(Planner::new(source, time, CAL, start, config.clone()).unwrap());
    let plan = report
        .plan
        .unwrap_or_else(|| panic!("{:?}", report.closest_attempt));
    assert_eq!(plan.assessment.prepare_at_ms, start.at_ms);
    assert!(plan.assessment.preparation_ms < 60_000);
    // The same early schedule cannot spend rest that will only finish later.
    let report = run(Planner::new(
        source,
        time,
        CAL,
        snapshot(CAL, Pointing::new(100.0, 10.0), [295_000, 295_000]),
        config,
    )
    .unwrap());
    assert!(report.plan.is_none());
    assert!(matches!(
        report.closest_attempt.unwrap().failure,
        Some(CandidateFailure::Simulation(SimulationError::DutyLimit(_)))
    ));
}

#[test]
fn bad_sources_never_publish_partial_results() {
    let start = snapshot(CAL, Pointing::new(180.0, 10.0), [0, 0]);
    for source in [
        |_: f64| None,
        |_: f64| {
            Some(LookAngle {
                azimuth_deg: f64::NAN,
                elevation_deg: 45.0,
            })
        },
    ] {
        let mut planner = Planner::new(source, window(1000), CAL, start, nominal()).unwrap();
        assert_eq!(planner.step(), Err(PlanningError::Source));
        assert_eq!(planner.take_report(), Err(PlanningError::Source));
    }
    let source = |t: f64| {
        Some(LookAngle {
            azimuth_deg: 180.0 + t * 10.0,
            elevation_deg: 10.0,
        })
    };
    let mut planner = Planner::new(source, window(1000), CAL, start, nominal()).unwrap();
    planner.step().unwrap();
    assert_eq!(planner.step(), Err(PlanningError::SkyRateBound));
}

#[test]
fn route_storage_exhaustion_is_explicit() {
    let source = |_: f64| {
        Some(LookAngle {
            azimuth_deg: 180.0,
            elevation_deg: 89.0,
        })
    };
    let mut planner = Planner::new(
        source,
        window(600_000),
        CAL,
        snapshot(CAL, Pointing::new(180.0, 89.0), [0, 0]),
        nominal(),
    )
    .unwrap();
    let error = loop {
        if let Err(error) = planner.step() {
            break error;
        }
    };
    assert_eq!(error, PlanningError::RouteCapacity);
    assert_eq!(planner.take_report(), Err(PlanningError::RouteCapacity));
    assert!(std::mem::size_of::<Planner<fn(f64) -> Option<LookAngle>>>() < 90_000);
}

#[test]
fn resting_snapshot_requires_matching_clock_and_post_coast_feedback() {
    let start = snapshot(CAL, Pointing::new(100.0, 30.0), [1000, 2000]);
    let follower =
        FollowingController::new(MotorProfile::AC_50HZ, 0.15, RelayConfig::DEFAULT).unwrap();
    let settings = SimulationSettings {
        step_ms: 20,
        scenario: Scenario::NOMINAL,
    };
    let make = |start| Simulation::from_rest(CAL, follower, settings, start);
    assert!(make(start).is_ok());
    assert!(matches!(
        make(RestingStart {
            at_ms: 2001,
            ..start
        }),
        Err(SimulationError::InvalidStart)
    ));
    let installed = InstalledCalibration::new(CAL, FEEDBACK_FULL_SCALE).unwrap();
    let old = FeedbackSample::from_voltages(
        installed,
        CAL.position_to_feedback(Pointing::new(100.0, 30.0))
            .unwrap(),
        1000,
    )
    .unwrap();
    assert!(matches!(
        make(RestingStart {
            feedback: old,
            ..start
        }),
        Err(SimulationError::InvalidStart)
    ));

    let mut sim = make(start).unwrap();
    sim.skip_idle_until(900_000).unwrap();
    assert_eq!(sim.step(None).unwrap().requested, Command::Inhibit);
    assert_eq!(sim.actuator().status().duty.azimuth.remaining_ms, 300_000);
    assert_eq!(sim.summary().energized_ms, [0, 0]);
    let mut actuator = start.actuator.configuration();
    actuator.relays.min_active_ms = 600;
    assert!(matches!(
        Simulation::new(
            CAL,
            start.feedback.position(),
            follower,
            settings,
            actuator,
            [0, 0],
        ),
        Err(SimulationError::InvalidSettings)
    ));
}

#[test]
fn simulation_copies_recent_relay_holds_and_consumed_duty_from_the_output_owner() {
    let mut start = snapshot(CAL, Pointing::new(100.0, 30.0), [0, 0]);
    let installed = InstalledCalibration::new(CAL, FEEDBACK_FULL_SCALE).unwrap();
    let cw = DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: None,
    };
    let follower =
        FollowingController::new(MotorProfile::AC_50HZ, 0.15, RelayConfig::DEFAULT).unwrap();
    let settings = SimulationSettings {
        step_ms: 20,
        scenario: Scenario {
            latency_ms: 0,
            ..Scenario::NOMINAL
        },
    };
    for (now, azimuth, command) in [
        (2000, 100.0, Command::Drive(cw)),
        (2250, 100.0, Command::Drive(cw)),
        (2500, 100.8, Command::Inhibit),
        (3000, 102.0, Command::Inhibit),
    ] {
        start.at_ms = now;
        start.feedback = FeedbackSample::from_voltages(
            installed,
            CAL.position_to_feedback(Pointing::new(azimuth, 30.0))
                .unwrap(),
            now,
        )
        .unwrap();
        let mut interlock = FeedbackInterlock::new(installed);
        interlock.publish(Some(start.feedback));
        start.network.observe(now, true);
        start.actuator.step(
            ActuatorInput {
                interlock,
                network: start.network,
                command: Some(CommandEnvelope {
                    command,
                    valid_until_ms: now + MOVEMENT_LEASE_MS,
                }),
            },
            now,
        );
        if now < 3000 {
            assert!(matches!(
                Simulation::from_rest(CAL, follower, settings, start),
                Err(SimulationError::InvalidStart)
            ));
        }
    }
    let mut sim = Simulation::from_rest(CAL, follower, settings, start).unwrap();
    while sim.now_ms() < 4500 {
        assert_eq!(
            sim.step(Some(Pointing::new(90.0, 30.0))).unwrap().applied,
            DriveCommand::IDLE
        );
    }
    assert_eq!(sim.now_ms(), 4500);
    assert_eq!(
        sim.step(Some(Pointing::new(90.0, 30.0)))
            .unwrap()
            .applied
            .azimuth,
        Some(AzimuthDirection::CounterClockwise)
    );
    assert_eq!(sim.actuator().status().duty.azimuth.remaining_ms, 299_500);
    assert_eq!(start.actuator.actual(), DriveCommand::IDLE);
    assert_eq!(start.actuator.status().duty.azimuth.remaining_ms, 299_500);
}
