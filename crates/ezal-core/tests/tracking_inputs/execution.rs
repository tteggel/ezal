//! The production coordination/execution path, driven with a separate motor.
use super::*;
use ezal_core::coordination::CoordinationState;
use ezal_core::feedback::{FeedbackHealth, FeedbackSnapshot};
use ezal_core::passes::{Config, Job};
use std::collections::VecDeque;

mod acceptance;

struct Device {
    state: CoordinationState,
    actuator: Actuator,
    model: MotorModel,
    pending: VecDeque<(u64, FeedbackSample)>,
    owner: u32,
    now: u64,
    ready_seen: bool,
    following_seen: bool,
    applied_transitions: usize,
    event: PassEvent,
}

impl Device {
    fn new() -> Self {
        Self::with_preparation_offset(0.0)
    }

    fn with_preparation_offset(offset_deg: f32) -> Self {
        let (preview, _) = preview();
        let plan = preview.bound_plan().unwrap().plan();
        let mut pose = plan.route.pointing_at(0).unwrap();
        pose.elevation_deg += offset_deg;
        let mut device = Self {
            state: CoordinationState::new(),
            actuator: Actuator::with_config(plan.actuator, DutyHistory::Known([0; 2])),
            model: MotorModel::new(MotorProfile::AC_50HZ, FULL_TRAVEL_CALIBRATION, pose, [0; 2])
                .unwrap(),
            pending: VecDeque::new(),
            owner: 0,
            now: 0,
            ready_seen: false,
            following_seen: false,
            applied_transitions: 0,
            event: EVENT,
        };
        for at in (0..=2000).step_by(100) {
            device.state.publish_network(true, at);
            device.feedback(full_travel_sample(pose, at), at);
            device.apply(at);
        }
        device.now = 2000;
        device.state.publish_clock(clock(UTC), 2000);
        device.owner = device.state.register_client(2000);
        device.request(1, TrackingAction::Select(EVENT));
        let revisions = PlanRevisions {
            start: device.state.start_revision(),
            ..REVISIONS
        };
        device.state.tracking_operation(2000, |runtime, obs| {
            runtime.refresh(&[EVENT], revisions, obs).unwrap()
        });
        let inputs = capture(
            device.state.tracking(),
            &catalog(),
            clock(UTC),
            UTC + 120_000,
            UTC + 150_000,
        )
        .unwrap();
        let report = if offset_deg == 0.0 {
            report(&inputs)
        } else {
            let mut planner = Planner::new(
                inputs.clone(),
                inputs.window(),
                FULL_TRAVEL_CALIBRATION,
                device.state.resting_start().unwrap(),
                nominal(),
            )
            .unwrap();
            while !planner.is_finished() {
                planner.step().unwrap();
            }
            let report = planner.take_report().unwrap();
            assert!(report.plan.is_some(), "{report:?}");
            report
        };
        device.state.tracking_operation(2000, |runtime, obs| {
            runtime
                .publish_plan(inputs.ticket(), Ok(report), inputs, obs)
                .unwrap();
        });
        device.request(2, TrackingAction::Arm);
        device.apply(2000);
        device
    }

    fn request(&mut self, sequence: u64, action: TrackingAction) {
        self.try_request(sequence, action).unwrap();
    }

    fn try_request(
        &mut self,
        sequence: u64,
        action: TrackingAction,
    ) -> Result<u64, ezal_core::protocol::TrackingRejection> {
        let token = self.state.operator(self.owner, self.now).unwrap().token;
        let mut freshness = ezal_core::session::CommandFreshness::new(self.owner);
        let proof = freshness.issue(self.now).unwrap().token;
        let intent = ezal_core::protocol::TrackingIntent {
            proof,
            lease: token.generation(),
            sequence,
            revision: self.state.tracking().snapshot().revision,
            action,
        };
        self.state
            .request_tracking(self.owner, intent, &freshness, self.now, |p| {
                p == self.event
            })
            .result
    }

    fn feedback(&mut self, sample: FeedbackSample, at: u64) {
        let feedback = FeedbackSnapshot {
            raw: None,
            last_valid: Some(sample),
            health: FeedbackHealth::Healthy,
        };
        let mut interlock = FeedbackInterlock::new(full_travel_installed());
        interlock.publish(Some(sample));
        self.state.publish_feedback(feedback, interlock, at);
    }

    fn apply(&mut self, at: u64) {
        let old = self.actuator.actual();
        let input = self.state.take_actuator_input(at);
        let update = self.actuator.step(input, at);
        self.applied_transitions += usize::from(update.output != old);
        // Apply EVERY transition, including the service before command generation.
        self.model.advance(update.output, 0).unwrap();
        self.state.record_actuator(&self.actuator, at);
    }

    fn tick(&mut self, at: u64) {
        self.tick_inner(at, Some(full_travel_sample));
        assert_eq!(
            self.actuator.fault(),
            None,
            "{:?}",
            self.state.tracking().snapshot()
        );
    }

    fn tick_inner(&mut self, at: u64, read: Option<fn(Pointing, u64) -> FeedbackSample>) {
        self.model
            .advance(self.actuator.actual(), at - self.now)
            .unwrap();
        self.now = at;
        self.state.publish_network(true, at);
        // 100 ms paired acquisition followed by the firmware's 100 ms delay.
        if let Some(read) = read.filter(|_| at.is_multiple_of(200)) {
            self.pending
                .push_back((at + 100, read(self.model.position(), at)));
        }
        while self.pending.front().is_some_and(|(ready, _)| *ready <= at) {
            let sample = self.pending.pop_front().unwrap().1;
            self.feedback(sample, at);
        }
        self.apply(at);
        self.state.execute_tracking(at);
        self.apply(at);
        self.ready_seen |= self.state.tracking().snapshot().phase == TrackingPhase::Ready;
        self.following_seen |= self.state.tracking().snapshot().phase == TrackingPhase::Following;
    }

    fn until(&mut self, at: u64) {
        while self.now < at {
            self.tick((self.now + 10).min(at));
        }
    }
}

#[test]
fn device_prepares_follows_and_finishes_after_browser_disconnect_under_background_load() {
    let mut device = Device::new();
    assert!(device.state.release_client(device.owner, device.now));
    let viewers: Vec<_> = (0..8)
        .map(|_| device.state.register_client(device.now))
        .collect();
    // All viewers leave as well; requests and telemetry never own execution.
    for viewer in &viewers {
        device.state.release_client(*viewer, device.now);
    }
    let catalog = catalog();
    let station = ezal_core::orbit::GroundStation::new(Observer::CLEVEDON).unwrap();
    let mut prediction =
        Job::new(&catalog, &station, Config::DEFAULT, (UTC / 1000) as i64).unwrap();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{TLES}",
        TLES.len()
    );
    let mut download = OrbitalDownload::new((UTC / 1000) as i64);
    let mut bytes = response.as_bytes().chunks(17);
    let mut forecast_finished = false;
    while device.now < LOS {
        device.tick(device.now + 10);
        if let Some(chunk) = bytes.next() {
            download.push(chunk).unwrap();
        }
        forecast_finished |= prediction.step();
        for viewer in &viewers {
            device.state.telemetry(*viewer, device.now);
        }
        device.state.tracking_operation(device.now, |runtime, obs| {
            runtime.step_reacquisition(obs).unwrap();
        });
    }
    assert!(forecast_finished);
    assert!(download.is_complete());
    assert!(
        device.ready_seen && device.following_seen,
        "{:?}",
        device.state.tracking().snapshot()
    );
    assert!(device.applied_transitions > 0);
    assert_eq!(
        device.state.tracking().snapshot().phase,
        TrackingPhase::Complete
    );
    assert_eq!(
        device
            .state
            .tracking()
            .last_summary()
            .unwrap()
            .termination
            .reason,
        TrackingStop::LossOfSignal
    );
    assert!(!device.actuator.actual().is_active());
    assert!(device.state.tracking().operator(device.now).is_none());
    device.until(LOS + 2000);
    assert!(!device.actuator.actual().is_active());
    assert!(!device.actuator.target().is_active());
}

#[test]
fn delayed_control_or_delayed_drain_inhibits_before_any_pending_relay_can_start() {
    for delayed_drain in [false, true] {
        let mut device = Device::new();
        device.until(AOS + 1000);
        assert_eq!(
            device.state.tracking().snapshot().phase,
            TrackingPhase::Following
        );
        if delayed_drain {
            device.state.execute_tracking(device.now);
        }
        device.now += 21;
        device.state.publish_network(true, device.now);
        device.apply(device.now);
        let stopped = device.state.tracking().snapshot();
        assert_eq!(
            stopped.termination.unwrap().reason,
            TrackingStop::ExecutionDeadline
        );
        assert!(!device.actuator.actual().is_active());
        assert!(!device.actuator.target().is_active());
        device.until(device.now + 1000);
        assert_eq!(
            device.state.tracking().snapshot().termination,
            stopped.termination
        );
    }
}

#[test]
fn only_the_current_lease_can_cancel_and_old_device_commands_cannot_restore_motion() {
    let mut device = Device::new();
    device.until(AOS + 1000);
    // The original lease has expired; a new browser can claim and cancel.
    let viewer = device.state.register_client(device.now);
    assert!(device.state.operator(viewer, device.now).is_some());
    assert!(!device.state.take_control(device.owner, device.now));
    assert!(!device
        .state
        .accept_command(viewer, CommandEnvelope::INHIBIT, device.now));
    device.owner = viewer;
    device.request(1, TrackingAction::Cancel);
    device.apply(device.now);
    device.state.execute_tracking(device.now);
    assert_eq!(device.state.take_actuator_input(device.now).command, None);
    assert_eq!(
        device.state.tracking().snapshot().phase,
        TrackingPhase::Cancelled
    );
    assert!(!device.actuator.actual().is_active());
    device.until(LOS + 1000);
    assert!(!device.actuator.actual().is_active());
}

#[test]
fn changed_start_invalidates_preview_before_a_simultaneous_preparation_deadline() {
    let mut device = Device::new();
    let prep = device
        .state
        .tracking()
        .bound_plan()
        .unwrap()
        .plan()
        .assessment
        .prepare_at_ms;
    device.until(prep - 10);
    let mut pose = device.model.position();
    pose.azimuth_deg += 1.0;
    let old_ticket = device.state.tracking().bound_plan().unwrap().ticket();
    device.feedback(full_travel_sample(pose, prep - 10), prep);
    device.state.publish_network(true, prep);
    device.apply(prep);
    assert!(device.state.tracking().snapshot().run.is_none());
    assert!(device.state.tracking().bound_plan().is_none());
    assert_ne!(device.state.tracking().planning_ticket(), Some(old_ticket));
    assert!(!device.actuator.actual().is_active());
}

#[test]
fn a_disarmed_clock_publication_blocks_preview_and_stops_a_run() {
    let mut disarmed = clock(UTC);
    assert_eq!(
        disarmed.synchronize(1000, sample(UTC)),
        Err(ntp::ClockDisarmed)
    );
    let unavailable = InputError::Timing(TimingError::Unavailable);
    let mut device = Device::new();
    device.state.publish_clock(disarmed, device.now);
    assert_eq!(
        device.state.tracking().snapshot().plan,
        PlanStatus::Blocked(PlanProblem::Inputs(unavailable))
    );
    let mut device = Device::new();
    device.until(AOS + 1000);
    device.state.publish_clock(disarmed, device.now);
    device.apply(device.now);
    let stopped = device.state.tracking().snapshot().termination.unwrap();
    assert_eq!(stopped.reason, TrackingStop::Inputs(unavailable));
    assert!(!device.actuator.actual().is_active());
}

#[test]
fn device_late_admission_uses_background_recovery_then_hands_off_to_normal_following() {
    let mut device = Device::new();
    device.until(AOS + 1000);
    device.owner = device.state.register_client(device.now);
    device.request(1, TrackingAction::Cancel);
    device.apply(device.now);
    device.until(AOS + 2500); // settle using actual coast and duty history
    device.request(2, TrackingAction::Select(EVENT));
    let revisions = PlanRevisions {
        start: device.state.start_revision(),
        ..REVISIONS
    };
    let now = device.now;
    device.state.tracking_operation(now, |runtime, obs| {
        runtime.refresh(&[EVENT], revisions, obs).unwrap()
    });
    let start = device.state.resting_start().unwrap();
    let inputs = ExecutionInputs::capture(
        device.state.tracking().planning_ticket().unwrap(),
        catalog().snapshot(EVENT.norad_id).unwrap(),
        InputSettings::default(),
        clock(UTC),
        execution_window(clock(UTC).anchor().unwrap(), UTC + 120_000, UTC + 150_000).unwrap(),
        &nominal(),
        now,
    )
    .unwrap();
    let mut planner = Planner::new(
        inputs.clone(),
        inputs.window(),
        FULL_TRAVEL_CALIBRATION,
        start,
        nominal(),
    )
    .unwrap();
    while !planner.is_finished() {
        planner.step().unwrap();
    }
    let result = planner.take_report().unwrap();
    assert_eq!(planner.take_report(), Err(PlanningError::Incomplete));
    let gap = result
        .plan
        .as_ref()
        .expect("late fixture is supported")
        .reception()
        .gap()
        .unwrap();
    assert!(gap.late_acquisition);
    device.state.tracking_operation(now, |runtime, obs| {
        runtime
            .publish_plan(inputs.ticket(), Ok(result), inputs, obs)
            .unwrap()
    });
    device.request(3, TrackingAction::Arm);
    device.until(gap.recovery_at_ms);
    assert_eq!(
        device.state.tracking().snapshot().phase,
        TrackingPhase::Reacquiring
    );
    let mut recovery_seen = false;
    let mut joined = false;
    while device.now < LOS {
        // A bounded worker quantum per live service. No command generation,
        // simulated output or watcher extends the episode/launch deadlines.
        for _ in 0..128 {
            if !device.state.tracking_operation(device.now, |runtime, obs| {
                runtime.step_reacquisition(obs).unwrap()
            }) {
                break;
            }
        }
        recovery_seen |= device
            .state
            .tracking()
            .recovery()
            .is_some_and(|r| r.intercept_at_ms.is_some());
        device.tick(device.now + 10);
        joined |= device.state.tracking().snapshot().phase == TrackingPhase::Following;
    }
    assert!(
        recovery_seen && joined,
        "{:?}",
        device.state.tracking().snapshot()
    );
    assert_eq!(
        device.state.tracking().snapshot().phase,
        TrackingPhase::Complete
    );
    assert!(!device
        .state
        .tracking()
        .last_summary()
        .unwrap()
        .reception
        .is_complete());
    assert!(!device.actuator.actual().is_active());
}

#[test]
fn device_waits_for_scheduled_preparation_then_slews_and_establishes_measured_readiness() {
    let mut device = Device::with_preparation_offset(10.0);
    let plan = device.state.tracking().bound_plan().unwrap().plan();
    let prep = plan.assessment.prepare_at_ms;
    let target = plan.route.pointing_at(0).unwrap();
    device.until(prep - 10);
    assert_eq!(device.applied_transitions, 0);
    assert_eq!(
        device.state.tracking().snapshot().phase,
        TrackingPhase::Armed
    );
    device.until(AOS);
    assert!(device.applied_transitions > 0);
    assert!(device.ready_seen);
    assert!(pointing_deviation_deg(device.model.position(), target).unwrap() < 3.0);
    assert_eq!(
        device.state.tracking().snapshot().phase,
        TrackingPhase::Following
    );
    device.until(LOS);
    assert_eq!(
        device.state.tracking().snapshot().phase,
        TrackingPhase::Complete
    );
}

#[test]
fn tracking_dashboard_contract_uses_real_device_snapshots_and_bounded_payloads() {
    use ezal_core::protocol::{TelemetryMessage, TELEMETRY_CAPACITY};
    use ezal_core::session::CommandToken;
    fn json(device: &Device, client: u32) -> String {
        let before = device.state.tracking().snapshot();
        let message = TelemetryMessage {
            snapshot: device.state.telemetry(client, device.now),
            token: Some(CommandToken {
                token: u64::MAX,
                expires_ms: u64::MAX,
            }),
            tracking: device.state.tracking_telemetry(client, device.now),
        };
        let mut payload: heapless::String<TELEMETRY_CAPACITY> = heapless::String::new();
        message.write_json(&mut payload).unwrap();
        assert_eq!(
            device.state.tracking().snapshot(),
            before,
            "formatting must remain read-only"
        );
        assert!(
            core::mem::size_of_val(&message.tracking) < 1024,
            "never copy route/source data per connection"
        );
        payload.to_string()
    }
    let mut device = Device::new();
    let mut snapshots = vec![json(&device, device.owner)];
    let viewer = device.state.register_client(device.now);
    snapshots.push(json(&device, viewer));
    device.state.release_client(device.owner, device.now);
    let prep = device
        .state
        .tracking()
        .bound_plan()
        .unwrap()
        .plan()
        .assessment
        .prepare_at_ms;
    device.until(prep + 100);
    snapshots.push(json(&device, viewer));
    device.until(AOS + 1000);
    snapshots.push(json(&device, viewer));
    // A new leased browser can observe, but connecting alone cannot interrupt.
    let reconnect = device.state.register_client(device.now);
    snapshots.push(json(&device, reconnect));
    device.until(LOS);
    snapshots.push(json(&device, reconnect));
    assert!(snapshots[0].contains("\"phase\":\"armed\""));
    assert!(snapshots[3].contains("\"phase\":\"following\""));
    assert!(snapshots[5].contains("\"reason\":\"LossOfSignal\""));
    let json = format!("[{}]", snapshots.join(","));
    if std::env::var_os("EZAL_UPDATE_FIXTURES").is_some() {
        std::fs::write(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/tracking-telemetry.json"
            ),
            format!("{json}\n"),
        )
        .unwrap();
        return;
    }
    assert_eq!(
        json,
        include_str!("../fixtures/tracking-telemetry.json").trim_end()
    );
}
