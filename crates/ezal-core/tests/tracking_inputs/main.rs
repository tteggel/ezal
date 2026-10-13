// A directory binary resolves `mod common;` inside its own directory.
#[path = "../common/mod.rs"]
mod common;

use common::{
    download, full_travel_installed, full_travel_sample, nominal, resting_start,
    FULL_TRAVEL_CALIBRATION,
};
use ezal_core::actuator::{Actuator, ActuatorConfig, ActuatorInput};
use ezal_core::ads1015::FEEDBACK_FULL_SCALE;
use ezal_core::catalog::{Catalog, OrbitalDownload, SourceOrder};
use ezal_core::drive::{Command, CommandEnvelope};
use ezal_core::duty::{DutyConfig, DutyHistory};
use ezal_core::feedback::{FeedbackHealth, FeedbackSample, FeedbackSnapshot};
use ezal_core::http;
use ezal_core::interlock::FeedbackInterlock;
use ezal_core::network::NetworkGate;
use ezal_core::ntp::{self, UtcClock};
use ezal_core::operator::OperatorToken;
use ezal_core::orbit::{Observer, MAX_TLE_AGE_S};
use ezal_core::passes::Ephemeris;
use ezal_core::position::{AxisCalibration, Pointing};
use ezal_core::tracking::*;
use std::sync::OnceLock;

mod execution;
mod recovery;

const TLES: &str = include_str!("../fixtures/weather-2026-10-07.tle");
const EVENT: PassEvent = PassEvent {
    norad_id: 39260,
    aos_unix_s: 1_791_385_770,
    los_unix_s: 1_791_385_800,
};
const UTC: u64 = EVENT.aos_unix_s as u64 * 1000 + 125 - 120_000;
const AOS: u64 = 122_000;
const LOS: u64 = 152_000;
const REVISIONS: PlanRevisions = PlanRevisions {
    orbit: 1,
    clock: 1,
    start: 1,
};
fn sample(utc: u64) -> ntp::TimeSample {
    let mut stamp = [0; 8];
    stamp[..4].copy_from_slice(&((utc / 1000 + 2_208_988_800) as u32).to_be_bytes());
    stamp[4..].copy_from_slice(&((((utc % 1000) << 32).div_ceil(1000)) as u32).to_be_bytes());
    let mut packet = [0; 48];
    packet[0] = 0x24;
    packet[1] = 2;
    packet[3] = (-20i8) as u8;
    packet[24..32].fill(1);
    packet[32..40].copy_from_slice(&stamp);
    packet[40..48].copy_from_slice(&stamp);
    ntp::reply_sample(&packet, [1; 8], 0).unwrap()
}
fn clock(utc: u64) -> UtcClock {
    let mut clock = UtcClock::new();
    clock.synchronize(2000, sample(utc)).unwrap();
    clock
}
fn observation(at: u64) -> RuntimeObservation {
    let mut network = NetworkGate::new();
    network.observe(at, true);
    RuntimeObservation {
        now_ms: at,
        clock: clock(UTC),
        network,
    }
}
fn catalog() -> Catalog {
    let mut catalog = Catalog::new();
    assert_eq!(
        catalog
            .commit(download(TLES, (UTC / 1000) as i64), 2000)
            .unwrap(),
        7
    );
    catalog
}
fn action(
    runtime: &mut TrackingRuntime,
    token: OperatorToken,
    sequence: u64,
    action: TrackingAction,
    observation: RuntimeObservation,
) -> Result<u64, RuntimeError> {
    runtime.request(
        TrackingRequest {
            operator: token,
            sequence,
            expected_revision: runtime.snapshot().revision,
            action,
        },
        observation,
    )
}
fn selected(event: PassEvent, observation: RuntimeObservation) -> (TrackingRuntime, OperatorToken) {
    let mut runtime = TrackingRuntime::new();
    let token = runtime.claim(1, observation).unwrap().token;
    action(
        &mut runtime,
        token,
        1,
        TrackingAction::Select(event),
        observation,
    )
    .unwrap();
    runtime.refresh(&[event], REVISIONS, observation).unwrap();
    (runtime, token)
}
fn capture(
    runtime: &TrackingRuntime,
    catalog: &Catalog,
    current: UtcClock,
    aos_utc: u64,
    los_utc: u64,
) -> Result<ExecutionInputs, InputError> {
    ExecutionInputs::capture(
        runtime.planning_ticket().unwrap(),
        catalog.snapshot(EVENT.norad_id).unwrap(),
        InputSettings::default(),
        current,
        execution_window(current.anchor().unwrap(), aos_utc, los_utc).unwrap(),
        &nominal(),
        2000,
    )
}
fn report(inputs: &ExecutionInputs) -> PlanningReport {
    static REPORT: OnceLock<PlanningReport> = OnceLock::new();
    REPORT
        .get_or_init(|| {
            let look = inputs.look_at(inputs.window().aos_unix_s).unwrap();
            let pose = Pointing::new(look.azimuth_deg as f32, look.elevation_deg as f32);
            let mut planner = Planner::new(
                inputs.clone(),
                inputs.window(),
                FULL_TRAVEL_CALIBRATION,
                resting_start(FULL_TRAVEL_CALIBRATION, pose, [0, 0]),
                nominal(),
            )
            .unwrap();
            while !planner.is_finished() {
                planner.step().unwrap();
            }
            let report = planner.take_report().unwrap();
            assert!(
                report.plan.is_some(),
                "fixture must be feasible: {:?}",
                report.closest_attempt
            );
            report
        })
        .clone()
}
fn preview() -> (TrackingRuntime, OperatorToken) {
    let (mut runtime, token) = selected(EVENT, observation(2000));
    let inputs = capture(
        &runtime,
        &catalog(),
        clock(UTC),
        UTC + 120_000,
        UTC + 150_000,
    )
    .unwrap();
    runtime
        .publish_plan(
            inputs.ticket(),
            Ok(report(&inputs)),
            inputs,
            observation(2000),
        )
        .unwrap();
    assert_eq!(runtime.snapshot().plan, PlanStatus::Supported);
    assert_eq!(
        runtime.take_command(observation(2000)),
        Some(CommandEnvelope::INHIBIT)
    );
    (runtime, token)
}
fn armed() -> (TrackingRuntime, OperatorToken, u64) {
    let (mut runtime, token) = preview();
    action(
        &mut runtime,
        token,
        2,
        TrackingAction::Arm,
        observation(2000),
    )
    .unwrap();
    let prep = runtime
        .bound_plan()
        .unwrap()
        .plan()
        .assessment
        .prepare_at_ms;
    assert!(prep > 2000);
    runtime.take_command(observation(2000));
    (runtime, token, prep)
}
fn following() -> (TrackingRuntime, RunToken) {
    let (mut runtime, _, prep) = armed();
    let run = follow(&mut runtime, prep);
    (runtime, run)
}
/// Freeze at `prep`, then run the device's execution turn every control step
/// for the last four seconds before AOS, with a rested actuator and fresh
/// feedback at the route's first point. Measured readiness hands over to
/// Following at AOS; the command generated there stays queued.
fn follow(runtime: &mut TrackingRuntime, prep: u64) -> RunToken {
    runtime.service(observation(prep)).unwrap();
    let run = runtime.snapshot().run.unwrap();
    let plan = runtime.bound_plan().unwrap().plan();
    let pose = plan.route.pointing_at(0).unwrap();
    let mut actuator = Actuator::with_config(plan.actuator, DutyHistory::Known([0, 0]));
    let step = plan.config.step_ms as usize;
    assert!(prep <= AOS - 4000);
    for at in (AOS - 4000..=AOS).step_by(step) {
        let sample = full_travel_sample(pose, at);
        let mut interlock = FeedbackInterlock::new(full_travel_installed());
        interlock.publish(Some(sample));
        let obs = observation(at);
        actuator.step(
            ActuatorInput {
                interlock,
                network: obs.network,
                command: None,
            },
            at,
        );
        runtime.take_command(obs);
        runtime.execution_command(healthy(sample), &actuator, obs);
    }
    assert_eq!(runtime.snapshot().phase, TrackingPhase::Following);
    run
}
fn healthy(sample: FeedbackSample) -> FeedbackSnapshot {
    FeedbackSnapshot {
        raw: None,
        last_valid: Some(sample),
        health: FeedbackHealth::Healthy,
    }
}
fn is_drive(command: Option<CommandEnvelope>) -> bool {
    matches!(
        command,
        Some(CommandEnvelope {
            command: Command::Drive(_),
            ..
        })
    )
}

#[test]
fn framed_downloads_are_atomic_and_failed_older_or_malformed_updates_retain_metadata() {
    let mut catalog = catalog();
    let before = catalog.snapshot(EVENT.norad_id).unwrap();
    let mut truncated = OrbitalDownload::new((UTC / 1000) as i64);
    truncated
        .push(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{TLES}",
                TLES.len() + 1
            )
            .as_bytes(),
        )
        .unwrap();
    assert!(matches!(truncated.finish(), Err(http::Error::Truncated)));
    let mut failed = OrbitalDownload::new((UTC / 1000) as i64);
    assert_eq!(
        failed.push(b"HTTP/1.1 500 Error\r\n\r\n"),
        Err(http::Error::Status)
    );
    assert!(failed.push(TLES.as_bytes()).is_err());
    assert!(failed.finish().is_err());
    assert_eq!(
        catalog
            .commit(download("bad data", (UTC / 1000) as i64), 3000)
            .unwrap(),
        0
    );
    assert_eq!(
        catalog.snapshot(EVENT.norad_id).unwrap().publication(),
        before.publication()
    );
    assert_eq!(
        catalog.commit(download(TLES, (UTC / 1000) as i64), 1999),
        Err(SourceOrder)
    );
    assert_eq!(
        catalog.snapshot(EVENT.norad_id).unwrap().publication(),
        before.publication()
    );
    assert_eq!(
        catalog
            .commit(download(TLES, (UTC / 1000) as i64), 4000)
            .unwrap(),
        7
    );
    assert_eq!(
        catalog
            .snapshot(EVENT.norad_id)
            .unwrap()
            .publication()
            .revision,
        2
    );
    assert_eq!(
        before.publication().revision,
        1,
        "owned old snapshots never mutate"
    );
    // An older epoch is valid data but cannot replace a newer accepted one.
    let lines: Vec<_> = TLES.lines().collect();
    let i = lines
        .iter()
        .position(|line| line.starts_with("1 39260"))
        .unwrap();
    let mut line = lines[i][..68].to_string();
    line.replace_range(18..32, "26279.27264657");
    let checksum: u32 = line
        .bytes()
        .map(|byte| {
            if byte.is_ascii_digit() {
                u32::from(byte - b'0')
            } else {
                u32::from(byte == b'-')
            }
        })
        .sum();
    let older = format!("{line}{}\n{}\n", checksum % 10, lines[i + 1]);
    assert_eq!(
        catalog
            .commit(download(&older, (UTC / 1000) as i64), 5000)
            .unwrap(),
        0
    );
    assert_eq!(
        catalog
            .snapshot(EVENT.norad_id)
            .unwrap()
            .publication()
            .received_at_ms,
        4000
    );
}

#[test]
fn input_snapshot_propagates_fractional_time_and_binds_clock_satellite_and_plan() {
    let (runtime, _) = selected(EVENT, observation(2000));
    let catalog = catalog();
    let inputs = capture(&runtime, &catalog, clock(UTC), UTC + 120_000, UTC + 150_000).unwrap();
    assert_ne!(inputs.look_at_uptime(AOS), inputs.look_at_uptime(AOS + 100));
    assert_eq!(
        inputs.look_at_uptime(AOS + 125),
        inputs.look_at((UTC + 120_125) as f64 / 1000.0)
    );
    assert!(inputs.look_at_uptime(AOS - 1).is_none());
    assert!(inputs.look_at_uptime(LOS + 1).is_none());
    let mut plan = report(&inputs).plan.unwrap();
    assert!(inputs.matches(&plan, inputs.ticket()));
    plan.window.aos_at_ms += 1;
    assert!(!inputs.matches(&plan, inputs.ticket()));
    let mut changed = clock(UTC);
    changed.synchronize(2000, sample(UTC)).unwrap();
    assert!(matches!(
        capture(&runtime, &catalog, changed, UTC + 120_000, UTC + 150_000),
        Err(InputError::Binding)
    ));
    let wrong = ExecutionInputs::capture(
        inputs.ticket(),
        catalog.snapshot(38771).unwrap(),
        InputSettings::default(),
        clock(UTC),
        inputs.window(),
        &nominal(),
        2000,
    );
    assert!(matches!(wrong, Err(InputError::Binding)));
    let wrong = ExecutionInputs::capture(
        inputs.ticket(),
        catalog.snapshot(EVENT.norad_id).unwrap(),
        InputSettings {
            observer: Observer {
                latitude_deg: f64::NAN,
                ..Observer::CLEVEDON
            },
            ..InputSettings::default()
        },
        clock(UTC),
        inputs.window(),
        &nominal(),
        2000,
    );
    assert!(matches!(wrong, Err(InputError::Observer)));
}

#[test]
fn freshly_downloaded_elements_must_remain_fresh_through_los_with_time_allowance() {
    let catalog = catalog();
    let epoch = catalog.snapshot(EVENT.norad_id).unwrap().epoch_unix_s();
    let end_of_validity = ((epoch + MAX_TLE_AGE_S as f64) * 1000.0) as u64;
    let utc = end_of_validity - 40_000;
    let event = PassEvent {
        aos_unix_s: (utc / 1000 + 10) as i64,
        los_unix_s: (utc / 1000 + 50) as i64,
        ..EVENT
    };
    let obs = RuntimeObservation {
        clock: clock(utc),
        ..observation(2000)
    };
    let (runtime, _) = selected(event, obs);
    assert!(matches!(
        capture(
            &runtime,
            &catalog,
            obs.clock,
            event.aos_unix_s as u64 * 1000,
            event.los_unix_s as u64 * 1000
        ),
        Err(InputError::StaleOrbit)
    ));
}

#[test]
fn early_selection_allows_refreshes_but_old_clock_cannot_freeze_hours_later() {
    let event = PassEvent {
        aos_unix_s: EVENT.aos_unix_s + 4 * 3600,
        los_unix_s: EVENT.los_unix_s + 4 * 3600,
        ..EVENT
    };
    let (runtime, _) = selected(event, observation(2000));
    let inputs = capture(
        &runtime,
        &catalog(),
        clock(UTC),
        UTC + 120_000 + 14_400_000,
        UTC + 150_000 + 14_400_000,
    )
    .unwrap();
    assert_eq!(inputs.validate_preview(clock(UTC), 2000), Ok(()));
    assert_eq!(
        inputs.validate_execution(clock(UTC), 2000),
        Err(InputError::Timing(TimingError::Uncertainty))
    );
}

#[test]
fn browser_loss_and_failed_internet_refreshes_leave_frozen_inputs_and_clock_unchanged() {
    let (mut runtime, token, prep) = armed();
    assert!(runtime.release(token, observation(2001)).unwrap());
    follow(&mut runtime, prep);
    let anchor = runtime.bound_plan().unwrap().inputs().clock().anchor();
    let pointing = runtime
        .bound_plan()
        .unwrap()
        .inputs()
        .look_at_uptime(AOS + 10);
    let mut obs = observation(AOS + 10);
    obs.clock
        .synchronize(AOS + 10, sample(UTC + AOS + 10 - 2000 + 100))
        .unwrap();
    runtime
        .refresh(
            &[EVENT],
            PlanRevisions {
                orbit: 2,
                clock: 2,
                ..REVISIONS
            },
            obs,
        )
        .unwrap();
    assert_eq!(
        runtime.bound_plan().unwrap().inputs().clock().anchor(),
        anchor
    );
    assert_eq!(
        runtime
            .bound_plan()
            .unwrap()
            .inputs()
            .look_at_uptime(AOS + 10),
        pointing
    );
    assert_eq!(
        runtime
            .bound_plan()
            .unwrap()
            .inputs()
            .ticket()
            .revisions()
            .orbit,
        1
    );
    assert!(is_drive(runtime.take_command(obs)));
    assert_eq!(runtime.snapshot().phase, TrackingPhase::Following);
    // The network supervisor continues its 100 ms observations while the
    // tracking test jumps to LOS; a gap in that supervision would be an outage.
    obs = RuntimeObservation {
        clock: obs.clock,
        ..observation(LOS)
    };
    runtime.service(obs).unwrap();
    assert_eq!(runtime.snapshot().phase, TrackingPhase::Complete);
    assert_eq!(
        runtime.snapshot().termination.unwrap().reason,
        TrackingStop::LossOfSignal
    );
    assert_eq!(runtime.take_command(obs), Some(CommandEnvelope::INHIBIT));
}

#[test]
fn new_clock_at_preparation_invalidates_old_job_then_freezes_only_its_replacement() {
    let (mut runtime, _, prep) = armed();
    let old_inputs = runtime.bound_plan().unwrap().inputs().clone();
    let mut obs = observation(prep);
    obs.clock
        .synchronize(prep, sample(UTC + prep - 2000))
        .unwrap();
    runtime
        .refresh(
            &[EVENT],
            PlanRevisions {
                clock: 2,
                ..REVISIONS
            },
            obs,
        )
        .unwrap();
    assert!(runtime.snapshot().run.is_none());
    assert_eq!(runtime.snapshot().plan, PlanStatus::Planning);
    assert_eq!(
        runtime.publish_plan(
            old_inputs.ticket(),
            Ok(report(&old_inputs)),
            old_inputs,
            obs
        ),
        Err(RuntimeError::Lifecycle(LifecycleError::StaleJob))
    );
    let ticket = runtime.planning_ticket().unwrap();
    let inputs = ExecutionInputs::capture(
        ticket,
        catalog().snapshot(EVENT.norad_id).unwrap(),
        InputSettings::default(),
        obs.clock,
        execution_window(obs.clock.anchor().unwrap(), UTC + 120_000, UTC + 150_000).unwrap(),
        &nominal(),
        prep,
    )
    .unwrap();
    runtime
        .publish_plan(ticket, Ok(report(&inputs)), inputs, obs)
        .unwrap();
    assert_eq!(runtime.snapshot().phase, TrackingPhase::Preparing);
    assert_eq!(
        runtime
            .bound_plan()
            .unwrap()
            .inputs()
            .clock()
            .anchor()
            .revision(),
        2
    );
}

#[test]
fn excessive_clock_correction_inhibits_queued_output_and_cannot_resume() {
    for correction in [-1000i64, 1000] {
        let (mut runtime, _) = following();
        let now = AOS + 10;
        let mut obs = observation(now);
        obs.clock
            .synchronize(
                now,
                sample((UTC as i64 + now as i64 - 2000 + correction) as u64),
            )
            .unwrap();
        assert_eq!(runtime.take_command(obs), Some(CommandEnvelope::INHIBIT));
        assert_eq!(
            runtime.snapshot().termination.unwrap().reason,
            TrackingStop::Inputs(InputError::Timing(TimingError::Correction))
        );
        assert_eq!(runtime.take_command(observation(now + 1)), None);
        assert_eq!(runtime.snapshot().mode, TrackingMode::Manual);
    }
}

#[test]
fn device_outage_expired_network_observation_and_recovered_generation_stop_the_run() {
    for recovered in [false, true] {
        let (mut runtime, _) = following();
        let mut obs = observation(AOS + 1);
        obs.network.observe(AOS + 2, false);
        if recovered {
            obs.network.observe(AOS + 3, true);
        }
        obs.now_ms = AOS + 3;
        assert_eq!(runtime.take_command(obs), Some(CommandEnvelope::INHIBIT));
        assert_eq!(
            runtime.snapshot().termination.unwrap().reason,
            TrackingStop::NetworkUnavailable
        );
        assert_eq!(runtime.take_command(observation(AOS + 4)), None);
    }
    let (mut runtime, _) = following();
    let mut expired = observation(AOS);
    expired.now_ms = AOS + 300;
    runtime.service(expired).unwrap();
    assert_eq!(
        runtime.snapshot().termination.unwrap().reason,
        TrackingStop::NetworkUnavailable
    );
    let (mut runtime, token) = preview();
    let mut offline = observation(2000);
    offline.network.observe(2000, false);
    assert_eq!(
        action(&mut runtime, token, 2, TrackingAction::Arm, offline),
        Err(RuntimeError::NetworkUnavailable)
    );
    assert_eq!(runtime.snapshot().mode, TrackingMode::Manual);
}

#[test]
fn missing_clock_invalid_input_reports_and_reboot_remain_disarmed() {
    assert!(Catalog::new().snapshot(EVENT.norad_id).is_none());
    let mut boot = TrackingRuntime::new();
    boot.service(observation(AOS)).unwrap();
    assert_eq!(boot.snapshot().mode, TrackingMode::Manual);
    assert!(boot.snapshot().selection.is_none());
    let (mut runtime, _) = following();
    let mut lost = observation(AOS + 1);
    lost.clock = UtcClock::new();
    runtime.service(lost).unwrap();
    assert_eq!(
        runtime.snapshot().termination.unwrap().reason,
        TrackingStop::Inputs(InputError::Timing(TimingError::Unavailable))
    );
    let (mut runtime, _) = selected(EVENT, observation(2000));
    let inputs = capture(
        &runtime,
        &catalog(),
        clock(UTC),
        UTC + 120_000,
        UTC + 150_000,
    )
    .unwrap();
    let mut mismatched = report(&inputs);
    mismatched.plan.as_mut().unwrap().config.uncertainty_deg += 0.25;
    assert_eq!(
        runtime.publish_plan(inputs.ticket(), Ok(mismatched), inputs, observation(2000)),
        Err(RuntimeError::Inputs(InputError::Binding))
    );
    assert_eq!(
        runtime.snapshot().plan,
        PlanStatus::Blocked(PlanProblem::Inputs(InputError::Binding))
    );
}

#[test]
fn installation_changes_and_backwards_uptime_inhibit_frozen_work() {
    let (mut runtime, run) = following();
    let frozen = runtime.bound_plan().unwrap().plan().actuator;
    let mut travel = frozen.travel;
    travel.azimuth.coast_ms += 100;
    assert!(travel.is_valid());
    let installed = Actuator::with_config(
        ActuatorConfig { travel, ..frozen },
        DutyHistory::Known([0, 0]),
    );
    // Unavailable feedback would stop the run too; the reason shows the
    // configuration check ran first.
    runtime
        .observe_tracking(
            run,
            FeedbackSnapshot::INITIAL,
            &installed,
            observation(AOS + 1),
        )
        .unwrap();
    assert_eq!(
        runtime.snapshot().termination.unwrap().reason,
        TrackingStop::InstallationChanged
    );
    assert_eq!(
        runtime.take_command(observation(AOS + 1)),
        Some(CommandEnvelope::INHIBIT)
    );
    let (mut runtime, _) = following();
    assert!(is_drive(runtime.take_command(observation(AOS + 10))));
    assert_eq!(
        runtime.take_command(observation(AOS + 9)),
        Some(CommandEnvelope::INHIBIT)
    );
    assert_eq!(
        runtime.snapshot().termination.unwrap().reason,
        TrackingStop::ClockOrCounter
    );
    assert_eq!(
        runtime.take_command(observation(AOS + 20)),
        Some(CommandEnvelope::INHIBIT)
    );
}

#[test]
fn clock_that_is_valid_now_but_exceeds_the_los_budget_blocks_preparation() {
    let (mut runtime, token) = selected(EVENT, observation(2000));
    let inputs = ExecutionInputs::capture(
        runtime.planning_ticket().unwrap(),
        catalog().snapshot(EVENT.norad_id).unwrap(),
        InputSettings {
            timing: TimingPolicy {
                drift_ppm: 2000,
                ..TimingPolicy::default()
            },
            ..InputSettings::default()
        },
        clock(UTC),
        execution_window(clock(UTC).anchor().unwrap(), UTC + 120_000, UTC + 150_000).unwrap(),
        &nominal(),
        2000,
    )
    .unwrap();
    assert_eq!(inputs.validate_preview(clock(UTC), 2000), Ok(()));
    let planned = report(&inputs);
    let prep = planned.plan.as_ref().unwrap().assessment.prepare_at_ms;
    runtime
        .publish_plan(inputs.ticket(), Ok(planned), inputs, observation(2000))
        .unwrap();
    action(
        &mut runtime,
        token,
        2,
        TrackingAction::Arm,
        observation(2000),
    )
    .unwrap();
    runtime.service(observation(prep)).unwrap();
    assert_eq!(runtime.snapshot().phase, TrackingPhase::Armed);
    assert_eq!(
        runtime.snapshot().plan,
        PlanStatus::Blocked(PlanProblem::Inputs(InputError::Timing(
            TimingError::Uncertainty
        )))
    );
    assert!(runtime.snapshot().run.is_none());
    assert_eq!(
        runtime.take_command(observation(prep)),
        Some(CommandEnvelope::INHIBIT)
    );
    runtime.service(observation(AOS)).unwrap();
    assert_eq!(
        runtime.snapshot().termination.unwrap().reason,
        TrackingStop::LatePreparation
    );
}

#[test]
fn runtime_cancel_at_preparation_prevents_a_freeze_and_stale_clock_publications_stop_execution() {
    let (mut runtime, _, prep) = armed();
    let token = runtime.claim(2, observation(prep - 1)).unwrap().token;
    action(
        &mut runtime,
        token,
        1,
        TrackingAction::Cancel,
        observation(prep),
    )
    .unwrap();
    assert_eq!(runtime.snapshot().phase, TrackingPhase::Cancelled);
    assert!(runtime.snapshot().termination.unwrap().run.is_none());
    assert_eq!(
        runtime.take_command(observation(prep)),
        Some(CommandEnvelope::INHIBIT)
    );
    let (mut runtime, _) = following();
    let mut updated = observation(AOS + 10);
    updated
        .clock
        .synchronize(AOS + 10, sample(UTC + AOS + 10 - 2000))
        .unwrap();
    runtime.service(updated).unwrap();
    assert!(is_drive(runtime.take_command(updated)));
    assert_eq!(
        runtime.take_command(observation(AOS + 11)),
        Some(CommandEnvelope::INHIBIT)
    );
    assert_eq!(
        runtime.snapshot().termination.unwrap().reason,
        TrackingStop::Inputs(InputError::Timing(TimingError::ClockOrder))
    );
}
