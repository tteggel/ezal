//! Execute whole synthetic routes independently of the planner's Simulation.
//! The plant assumptions are shared; the scheduling, ADC acquisition and
//! production actuator/follower integration below are a separate execution path.
use super::*;
use ezal_core::position::FeedbackVoltages;
use ezal_core::tracking::{pointing_deviation_deg, MotorModel, TrackingPlan};

#[derive(Debug, Default)]
struct Observed {
    peak_deg: f32,
    outside_ms: u64,
    energized_ms: [u64; 2],
    final_position: Option<Pointing>,
}

fn adc(axis: AxisCalibration, angle: f32) -> i32 {
    let fraction =
        f64::from(angle - axis.angle_min_deg) / f64::from(axis.angle_max_deg - axis.angle_min_deg);
    let mv = f64::from(axis.voltage_min_mv)
        + fraction * f64::from(axis.voltage_max_mv - axis.voltage_min_mv);
    let step = f64::from(FEEDBACK_FULL_SCALE.code_mv());
    ((mv / step).round() * step) as i32
}

fn execute(
    source: &impl Ephemeris,
    plan: &TrackingPlan,
    start: RestingStart,
    scenario: Scenario,
) -> Observed {
    let mut actuator = start.actuator;
    let mut motor = MotorModel::new(
        scenario.motor,
        plan.calibration,
        start.feedback.position(),
        [0; 2],
    )
    .unwrap();
    let mut controller = FollowingController::new(
        plan.config.reference,
        plan.config.deadband_deg,
        plan.actuator.relays,
    )
    .unwrap();
    let installed = InstalledCalibration::new(plan.calibration, FEEDBACK_FULL_SCALE).unwrap();
    let mut interlock = FeedbackInterlock::new(installed);
    let mut feedback = start.feedback;
    let mut network = start.network;
    let mut now = start.at_ms;
    let mut previous = now;
    let mut next_read = now;
    // The two channels are sampled separately, then delivered together.
    let mut pending: Option<(u64, i32, Option<i32>)> = None;
    let mut observed = Observed::default();
    let aos = plan.window.aos_at_ms;
    let los = aos + u64::from(plan.window.duration_ms);
    let prepare = plan.assessment.prepare_at_ms;
    let mut held = None;
    while now <= los + 3000 {
        let dt = now - previous;
        let output = actuator.actual();
        for (i, active) in [output.azimuth.is_some(), output.elevation.is_some()]
            .into_iter()
            .enumerate()
        {
            if active {
                observed.energized_ms[i] += dt;
            }
        }
        motor.advance(output, dt).unwrap();
        let pose = motor.position();
        if pending.is_none() && now >= next_read {
            pending = Some((
                now,
                adc(plan.calibration.elevation, pose.elevation_deg),
                None,
            ));
        }
        if let Some((sampled, _, az)) = &mut pending {
            if az.is_none() && now >= *sampled + scenario.channel_skew_ms {
                *az = Some(adc(plan.calibration.azimuth, pose.azimuth_deg));
            }
        }
        if let Some((sampled, el, Some(az))) = pending {
            if now >= sampled + scenario.latency_ms {
                feedback = FeedbackSample::from_voltages(
                    installed,
                    FeedbackVoltages {
                        a0_elevation_mv: el,
                        a1_azimuth_mv: az,
                    },
                    sampled,
                )
                .unwrap();
                pending = None;
                next_read = now + 100;
            }
        }
        interlock.publish(Some(feedback));
        network.observe(now, true);
        // Firmware services the actuator before generation and again after
        // draining the command. Observe both actual physical transitions.
        let serviced = actuator.step(
            ActuatorInput {
                interlock,
                network,
                command: None,
            },
            now,
        );
        motor.advance(serviced.output, 0).unwrap();
        controller.observe_output(serviced.output, now);
        let target = if (prepare..los).contains(&now) {
            plan.route.pointing_at(now.saturating_sub(aos) as u32)
        } else {
            None
        };
        let command = match target {
            Some(target) => controller.command(target, Some(feedback), actuator.actual(), now),
            None => {
                controller.clear_target();
                Command::Inhibit
            }
        };
        let update = actuator.step(
            ActuatorInput {
                interlock,
                network,
                command: Some(CommandEnvelope {
                    command,
                    valid_until_ms: now + MOVEMENT_LEASE_MS,
                }),
            },
            now,
        );
        motor.advance(update.output, 0).unwrap();
        controller.observe_output(update.output, now);
        assert_eq!(actuator.fault(), None, "at {now}, {observed:?}");
        if now < prepare || now >= los {
            assert!(!actuator.actual().is_active());
        }
        if (aos..los).contains(&now) {
            let sky = source
                .look_at(plan.window.aos_unix_s + (now - aos) as f64 / 1000.0)
                .unwrap();
            let error = pointing_deviation_deg(
                pose,
                Pointing::new(sky.azimuth_deg as f32, sky.elevation_deg as f32),
            )
            .unwrap();
            observed.peak_deg = observed.peak_deg.max(error);
            if error > plan.config.allocation_deg() {
                observed.outside_ms += dt;
            }
        }
        if now >= los + 1000 {
            assert!(motor.is_settled());
            assert_eq!(
                *held.get_or_insert(pose),
                pose,
                "no parking or next-pass motion after coast"
            );
        }
        previous = now;
        // Normal firmware cadence, shortened by physical and phase deadlines.
        now += 10;
        for deadline in [Some(prepare), Some(aos), Some(los), update.next_deadline_ms]
            .into_iter()
            .flatten()
        {
            if deadline > previous {
                now = now.min(deadline);
            }
        }
    }
    observed.final_position = held;
    observed
}

fn accept(
    name: &str,
    source: impl Ephemeris,
    duration_ms: u32,
    pose: Pointing,
    config: PlanningConfig,
) {
    let start = resting_start(FULL_TRAVEL_CALIBRATION, pose, [0; 2]);
    let scenarios = config.scenarios.len();
    let report = run(Planner::new(
        |t| source.look_at(t),
        window(duration_ms),
        FULL_TRAVEL_CALIBRATION,
        start,
        config,
    )
    .unwrap());
    let plan = report
        .plan
        .as_ref()
        .unwrap_or_else(|| panic!("{name}: {:?}", report.closest_attempt));
    assert!(plan.reception().is_complete());
    assert_eq!(plan.assessment.completed_scenarios, scenarios);
    // Independently replay the nominal, slow and fast cases at 10 ms, versus
    // the planner's 20 ms samples. Keep noisy/gear-engagement corner cases in
    // the planner's nine-scenario qualification rather than claiming coverage
    // this independent plant initialization does not provide.
    for scenario in plan.config.scenarios.iter().take(3) {
        let observed = execute(&source, plan, start, *scenario);
        println!(
            "{name} rates {:.3}/{:.3}°/s: predicted {:.3}°, observed {observed:?}",
            scenario.motor.azimuth.rate_deg_per_s,
            scenario.motor.elevation.rate_deg_per_s,
            plan.assessment.bounded_peak_deg
        );
        // The reported bound covers the configured 20 ms simulations. The
        // separately scheduled 10 ms execution has different ADC/pulse phase;
        // compare that spread explicitly within the reserved uncertainty too.
        assert!(
            observed.peak_deg <= plan.assessment.bounded_peak_deg + plan.config.uncertainty_deg,
            "{name}: {observed:?}, prediction {:?}",
            plan.assessment
        );
        assert_eq!(observed.outside_ms, 0);
        assert!(observed
            .energized_ms
            .iter()
            .all(|used| *used < scenario.motor.duty.energized_ms));
        for (actual, predicted) in observed
            .energized_ms
            .iter()
            .zip(plan.assessment.energized_ms)
        {
            assert!(*actual <= predicted + plan.actuator.relays.min_active_ms);
        }
    }
}

#[test]
fn whole_zenith_and_wrap_routes_meet_their_prediction_at_execution_cadence() {
    accept(
        "exact zenith",
        zenith,
        600_000,
        Pointing::new(180.0, 10.0),
        PlanningConfig::default(),
    );
    accept(
        "azimuth wrap",
        |t: f64| {
            Some(LookAngle {
                azimuth_deg: (350.0 + t / 6.0) % 360.0,
                elevation_deg: 30.0,
            })
        },
        120_000,
        Pointing::new(350.0, 30.0),
        nominal(),
    );
}

#[test]
fn extreme_preparation_positions_include_slew_and_coast_in_the_whole_run() {
    for pose in [Pointing::new(1.0, 1.0), Pointing::new(449.0, 179.0)] {
        accept(
            "extreme start",
            |_| {
                Some(LookAngle {
                    azimuth_deg: 180.0,
                    elevation_deg: 45.0,
                })
            },
            120_000,
            pose,
            nominal(),
        );
    }
}

#[test]
fn near_zenith_is_not_assumed_trackable_just_because_exact_zenith_is() {
    let source = |t: f64| {
        let angle = (10.0 + 160.0 * t / 600.0).to_radians();
        let miss = 2.0_f64.to_radians();
        Some(LookAngle {
            azimuth_deg: miss
                .sin()
                .atan2(-miss.cos() * angle.cos())
                .to_degrees()
                .rem_euclid(360.0),
            elevation_deg: (miss.cos() * angle.sin()).asin().to_degrees(),
        })
    };
    let report = run(Planner::new(
        source,
        window(600_000),
        FULL_TRAVEL_CALIBRATION,
        resting_start(FULL_TRAVEL_CALIBRATION, Pointing::new(180.0, 10.0), [0; 2]),
        PlanningConfig::default(),
    )
    .unwrap());
    assert!(report.plan.is_none());
    let diagnostic = report.closest_attempt.unwrap();
    println!("88° peak rejected: {diagnostic:?}");
    assert!(diagnostic.failure.is_some());
}
