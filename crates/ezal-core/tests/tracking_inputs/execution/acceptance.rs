//! Full recorded-pass acceptance through the production coordination/runtime.
use super::*;
use ezal_core::orbit::{GroundStation, LookAngle};
use ezal_core::passes::{Search, Track};
use ezal_core::position::FeedbackVoltages;
use ezal_core::tracking::pointing_deviation_deg;

const FULL_EVENT: PassEvent = PassEvent {
    norad_id: 38771,
    aos_unix_s: 1_791_402_086,
    los_unix_s: 1_791_402_837,
};
const FULL_AOS: u64 = 602_000;
const FULL_LOS: u64 = FULL_AOS + 751_000;
// Explicit virtual throughput, not a measurement of the Pico. Each planning
// unit still yields to execution; T7 must measure achievable work on the board.
const WORK_PER_TICK: usize = 2048;

fn quantized(pose: Pointing, at: u64) -> FeedbackSample {
    let code = f64::from(FEEDBACK_FULL_SCALE.code_mv());
    let voltage = |axis: AxisCalibration, angle: f32| {
        let fraction = f64::from(angle - axis.angle_min_deg)
            / f64::from(axis.angle_max_deg - axis.angle_min_deg);
        let mv = f64::from(axis.voltage_min_mv)
            + fraction * f64::from(axis.voltage_max_mv - axis.voltage_min_mv);
        ((mv / code).round() * code) as i32
    };
    FeedbackSample::from_voltages(
        InstalledCalibration::new(FULL_TRAVEL_CALIBRATION, FEEDBACK_FULL_SCALE).unwrap(),
        FeedbackVoltages {
            a0_elevation_mv: voltage(FULL_TRAVEL_CALIBRATION.elevation, pose.elevation_deg),
            a1_azimuth_mv: voltage(FULL_TRAVEL_CALIBRATION.azimuth, pose.azimuth_deg),
        },
        at,
    )
    .unwrap()
}

fn planned(
    event: PassEvent,
    planning: PlanningConfig,
    used: [u64; 2],
) -> (Device, PlanningReport, ExecutionInputs) {
    let utc = event.aos_unix_s as u64 * 1000 - 600_000;
    let pose = Pointing::new(180.0, 10.0);
    let mut device = Device {
        state: CoordinationState::new(),
        actuator: Actuator::with_config(
            ActuatorConfig {
                duty: DutyConfig::CONFIRMED_AC,
                ..ActuatorConfig::DEFAULT
            },
            DutyHistory::Known(used),
        ),
        model: MotorModel::new(MotorProfile::AC_50HZ, FULL_TRAVEL_CALIBRATION, pose, used).unwrap(),
        pending: VecDeque::new(),
        owner: 0,
        now: 2000,
        ready_seen: false,
        following_seen: false,
        applied_transitions: 0,
        event,
    };
    for at in (0..=2000).step_by(100) {
        device.state.publish_network(true, at);
        device.feedback(quantized(pose, at), at);
        device.apply(at);
    }
    device.state.publish_clock(clock(utc), 2000);
    device.owner = device.state.register_client(2000);
    device.request(1, TrackingAction::Select(event));
    let revisions = PlanRevisions {
        start: device.state.start_revision(),
        ..REVISIONS
    };
    device.state.tracking_operation(2000, |runtime, obs| {
        runtime.refresh(&[event], revisions, obs).unwrap()
    });
    let window = execution_window(
        clock(utc).anchor().unwrap(),
        event.aos_unix_s as u64 * 1000,
        event.los_unix_s as u64 * 1000,
    )
    .unwrap();
    let inputs = ExecutionInputs::capture(
        device.state.tracking().planning_ticket().unwrap(),
        catalog().snapshot(event.norad_id).unwrap(),
        InputSettings::default(),
        clock(utc),
        window,
        &planning,
        2000,
    )
    .unwrap();
    let mut planner = Planner::new(
        inputs.clone(),
        window,
        FULL_TRAVEL_CALIBRATION,
        device.state.resting_start().unwrap(),
        planning,
    )
    .unwrap();
    while !planner.is_finished() {
        planner.step().unwrap();
    }
    let report = planner.take_report().unwrap();
    (device, report, inputs)
}

fn publish(device: &mut Device, report: PlanningReport, inputs: ExecutionInputs) {
    device.state.tracking_operation(device.now, |runtime, obs| {
        runtime
            .publish_plan(inputs.ticket(), Ok(report), inputs, obs)
            .unwrap();
    });
}

fn arm_recorded() -> Device {
    // Only the quiet nominal assumption admits this fixture. The separate
    // production-profile test must reject it; this does not relax firmware.
    let (mut device, report, inputs) = planned(FULL_EVENT, nominal(), [0; 2]);
    assert!(report.plan.is_some(), "{report:?}");
    publish(&mut device, report, inputs);
    device.request(2, TrackingAction::Arm);
    device.apply(device.now);
    device
}

#[test]
fn recorded_complete_window_executes_its_admitted_reception_then_holds_without_a_browser() {
    // Check the fixture is a complete real 5°-mask window, not a convenient
    // short fragment of a longer pass.
    let catalog = catalog();
    let orbit = catalog
        .catalog()
        .orbits()
        .find(|o| o.satellite.norad_id == FULL_EVENT.norad_id)
        .unwrap();
    let station = GroundStation::new(Observer::CLEVEDON).unwrap();
    let mut search = Search::new(
        FULL_EVENT.aos_unix_s - 60,
        FULL_EVENT.los_unix_s + 60,
        Config::DEFAULT,
        orbit.satellite,
        Track {
            orbit,
            station: &station,
        },
    )
    .unwrap();
    let pass = loop {
        assert!(!search.is_finished());
        if let Some(pass) = search.step().unwrap() {
            break pass;
        }
    };
    assert_eq!(
        (pass.aos_unix_s, pass.los_unix_s),
        (FULL_EVENT.aos_unix_s, FULL_EVENT.los_unix_s)
    );
    let mut device = arm_recorded();
    let bound = device.state.tracking().bound_plan().unwrap();
    let prediction = bound.plan().assessment.clone();
    let uncertainty_deg = bound.plan().config.uncertainty_deg;
    let pulse_ms = bound.plan().actuator.relays.min_active_ms;
    let reception = bound.plan().reception();
    let source = bound.inputs().clone();
    assert!(device.state.release_client(device.owner, device.now));
    let mut peak = 0.0_f32;
    let mut recoveries = 0;
    let mut recovered_at = None;
    let mut most_work = 0;
    let mut previous_phase = TrackingPhase::Armed;
    while device.now < FULL_LOS {
        device.tick_inner(device.now + 10, Some(quantized));
        let phase = device.state.tracking().snapshot().phase;
        assert!(
            !matches!(phase, TrackingPhase::Stopped | TrackingPhase::Cancelled),
            "{:?}",
            device.state.tracking().snapshot()
        );
        if phase == TrackingPhase::Reacquiring && previous_phase != phase {
            recoveries += 1;
        }
        if previous_phase == TrackingPhase::Reacquiring && phase == TrackingPhase::Following {
            recovered_at = Some(device.now);
        }
        previous_phase = phase;
        // Bounded worker slices; execution remains in the 10 ms device loop.
        for _ in 0..WORK_PER_TICK {
            if phase != TrackingPhase::Reacquiring {
                break;
            }
            device.state.tracking_operation(device.now, |runtime, obs| {
                runtime.step_reacquisition(obs).unwrap();
            });
            if device
                .state
                .tracking()
                .recovery()
                .is_none_or(|r| !r.planning)
            {
                break;
            }
        }
        if let Some(recovery) = device.state.tracking().recovery() {
            most_work = most_work.max(recovery.work_steps);
        }
        if (FULL_AOS..FULL_LOS).contains(&device.now) {
            let look: LookAngle = source.look_at_uptime(device.now).unwrap();
            peak = peak.max(
                pointing_deviation_deg(
                    device.model.position(),
                    Pointing::new(look.azimuth_deg as f32, look.elevation_deg as f32),
                )
                .unwrap(),
            );
        }
    }
    println!("recorded: predicted {prediction:?}; reception {reception:?}; actual peak {peak:.3}°, recoveries {recoveries}, recovered {recovered_at:?}, work {most_work}, duty {:?}, summary {:?}", device.model.used_duty_ms(), device.state.tracking().last_summary());
    assert_eq!(recoveries, 1);
    let gap = reception.gap().unwrap();
    // Recovery timing is a model prediction. Independent ADC/control phase
    // changes this fixture's join; execution must still establish measured
    // recovery within the original episode, without extending that deadline.
    assert!(recovered_at.unwrap() <= gap.deadline_ms);
    assert!(peak <= prediction.bounded_peak_deg + uncertainty_deg);
    assert!(peak < 3.0);
    for (actual, predicted) in device
        .model
        .used_duty_ms()
        .into_iter()
        .zip(prediction.energized_ms)
    {
        assert!(actual.abs_diff(predicted) <= pulse_ms);
    }
    let summary = device.state.tracking().last_summary().unwrap();
    assert_eq!(summary.reception, reception);
    assert_eq!(summary.deviation.outside_tolerance_ms, 0);
    assert!(device.ready_seen && device.following_seen);
    assert_eq!(
        device.state.tracking().snapshot().phase,
        TrackingPhase::Complete
    );
    assert_eq!(
        device
            .state
            .tracking()
            .snapshot()
            .termination
            .unwrap()
            .reason,
        TrackingStop::LossOfSignal
    );
    assert!(!device.actuator.actual().is_active());
    device.until(FULL_LOS + 1000);
    assert!(device.model.is_settled());
    let held = device.model.position();
    device.until(FULL_LOS + 60_000);
    assert_eq!(device.model.position(), held);
    assert!(!device.actuator.actual().is_active());
    assert_eq!(
        device.state.tracking().snapshot().phase,
        TrackingPhase::Complete
    );
}

#[test]
fn production_assumptions_and_exhausted_duty_refuse_the_recorded_pass_without_motion() {
    for (name, planning, used) in [
        ("nine scenarios", PlanningConfig::default(), [0; 2]),
        ("exhausted duty", nominal(), [300_000; 2]),
    ] {
        let (mut device, report, inputs) = planned(FULL_EVENT, planning, used);
        assert!(report.plan.is_none(), "{name}: {report:?}");
        println!("recorded rejected ({name}): {:?}", report.closest_attempt);
        publish(&mut device, report, inputs);
        assert!(matches!(
            device.state.tracking().snapshot().plan,
            PlanStatus::Blocked(_)
        ));
        assert_eq!(
            device.try_request(2, TrackingAction::Arm),
            Err(ezal_core::protocol::TrackingRejection::Rejected(
                RuntimeError::Lifecycle(LifecycleError::NotReady)
            ))
        );
        let held = device.model.position();
        device.until(FULL_LOS + 1000);
        assert_eq!(device.model.position(), held);
        assert_eq!(device.applied_transitions, 0);
        assert!(device.state.tracking().snapshot().run.is_none());
    }
}

#[test]
fn leased_cancellation_in_each_active_phase_holds_through_the_original_los() {
    for phase in [
        TrackingPhase::Armed,
        TrackingPhase::Preparing,
        TrackingPhase::Following,
        TrackingPhase::Reacquiring,
    ] {
        let mut device = arm_recorded();
        let plan = device.state.tracking().bound_plan().unwrap().plan();
        let cancel_at = match phase {
            TrackingPhase::Armed => device.now,
            TrackingPhase::Preparing => plan.assessment.prepare_at_ms + 1000,
            TrackingPhase::Following => FULL_AOS + 5000,
            TrackingPhase::Reacquiring => plan.reception().gap().unwrap().recovery_at_ms + 200,
            _ => unreachable!(),
        };
        device.until(cancel_at);
        assert_eq!(device.state.tracking().snapshot().phase, phase);
        // A new browser claims the expired operator lease; execution itself
        // continued while the original browser was absent.
        if device.now > 2000 {
            device.owner = device.state.register_client(device.now);
        }
        let used = device.model.used_duty_ms();
        device.request(3, TrackingAction::Cancel);
        device.apply(device.now);
        assert!(!device.actuator.actual().is_active());
        assert_eq!(device.model.used_duty_ms(), used);
        let termination = device.state.tracking().snapshot().termination.unwrap();
        assert_eq!(termination.reason, TrackingStop::Cancelled);
        let coast_end = device.now + 1000;
        device.until(coast_end);
        let held = device.model.position();
        device.until(FULL_LOS + 60_000);
        assert_eq!(device.model.position(), held);
        assert_eq!(
            device.state.tracking().snapshot().termination,
            Some(termination)
        );
        assert!(device.state.tracking().snapshot().run.is_none());
    }
}

#[test]
fn stalled_hardware_and_missing_feedback_stop_the_full_pass_and_do_not_resume() {
    for stall in [true, false] {
        let mut device = arm_recorded();
        if stall {
            let mut profile = MotorProfile::AC_50HZ;
            profile.azimuth.rate_deg_per_s = 0.0;
            profile.elevation.rate_deg_per_s = 0.0;
            device.model = MotorModel::new(
                profile,
                FULL_TRAVEL_CALIBRATION,
                device.model.position(),
                [0; 2],
            )
            .unwrap();
        } else {
            device.until(FULL_AOS + 5000);
        }
        let stop_by = if stall { FULL_AOS } else { device.now + 1000 };
        while device.now < stop_by
            && device.state.tracking().snapshot().phase != TrackingPhase::Stopped
        {
            device.tick_inner(device.now + 10, if stall { Some(quantized) } else { None });
        }
        assert_eq!(
            device.state.tracking().snapshot().phase,
            TrackingPhase::Stopped
        );
        let termination = device.state.tracking().snapshot().termination.unwrap();
        assert!(
            if stall {
                matches!(termination.reason, TrackingStop::Motion(_))
            } else {
                matches!(termination.reason, TrackingStop::Feedback(_))
            },
            "{termination:?}"
        );
        assert!(!device.actuator.actual().is_active());
        while device.now < FULL_LOS + 1000 {
            device.tick_inner(device.now + 10, Some(quantized));
        }
        assert_eq!(
            device.state.tracking().snapshot().termination,
            Some(termination)
        );
        assert!(!device.actuator.actual().is_active());
        assert!(device.model.is_settled());
    }
}

#[test]
fn insufficient_recovery_throughput_holds_and_stops_at_the_original_deadline() {
    let mut device = arm_recorded();
    let gap = device
        .state
        .tracking()
        .bound_plan()
        .unwrap()
        .plan()
        .reception()
        .gap()
        .unwrap();
    let mut missed_launch = false;
    let mut warning = false;
    let mut most_attempts = 0;
    while device.now < gap.deadline_ms {
        device.tick_inner(device.now + 10, Some(quantized));
        for _ in 0..128 {
            device.state.tracking_operation(device.now, |runtime, obs| {
                runtime.step_reacquisition(obs).unwrap();
            });
            if device
                .state
                .tracking()
                .recovery()
                .is_none_or(|r| !r.planning)
            {
                break;
            }
        }
        if let Some(recovery) = device.state.tracking().recovery() {
            assert_eq!(recovery.episode.deadline_ms(), gap.deadline_ms);
            missed_launch |= recovery.hold == Some(RecoveryHold::MissedLaunch);
            most_attempts = most_attempts.max(recovery.attempts);
            assert!(!device.actuator.actual().is_active());
        }
        warning |= device.state.tracking().deviation(device.now).warning;
    }
    let summary = device.state.tracking().last_summary().unwrap();
    println!("slow worker: {most_attempts} attempts; {summary:?}");
    assert!(missed_launch && most_attempts > 1 && warning);
    assert_eq!(
        summary.termination.reason,
        TrackingStop::ReacquisitionTimeout
    );
    assert_eq!(summary.termination.at_ms, gap.deadline_ms);
    assert!(summary.deviation.maximum_deg.unwrap() > 3.0);
    assert!(summary.deviation.outside_tolerance_ms > 0);
    // The same summary/deviation projection sent to the dashboard retains
    // the failed run, even though its current pointing is no longer live.
    let mut json = String::new();
    device
        .state
        .tracking_telemetry(device.owner, device.now)
        .write_json(&mut json)
        .unwrap();
    assert!(json.contains("ReacquisitionTimeout"));
    assert!(json.contains("\"maximum_deg\":"));
    device.until(FULL_LOS + 60_000);
    assert_eq!(device.state.tracking().last_summary(), Some(summary));
    assert!(!device.actuator.actual().is_active());
}
