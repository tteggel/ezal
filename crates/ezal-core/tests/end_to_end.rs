//! A recorded METEOR-M N2-3 pass through the code the device runs: its
//! planning worker, coordination, feedback validation and actuator, with the
//! installed calibration and motion settings. A simulated rotator moves as
//! planned, and its readings arrive in 2 mV ADC codes that can flicker by one
//! code, as an idle reading does. Only the output task's service order below
//! is copied from firmware; it touches GPIO there.
//!
//! Unlike the policy tests this uses `HARD_CODED_CALIBRATION` and
//! `installation`, so a failure means the installed settings cannot track a
//! pass that both full and 360°/90° travel can, or that the device path is broken.
mod common;

use common::{download, ntp_sample};
use ezal_core::actuator::Actuator;
use ezal_core::catalog::Catalog;
use ezal_core::coordination::{CoordinationState, PlanningUnit, PlanningWorker, TrackingSource};
use ezal_core::duty::DutyHistory;
use ezal_core::feedback::{FeedbackMonitor, FEEDBACK_PERIOD_MS};
use ezal_core::installation;
use ezal_core::ntp::UtcClock;
use ezal_core::position::{FeedbackVoltages, Pointing, HARD_CODED_CALIBRATION};
use ezal_core::protocol::{TrackingIntent, TrackingRejection};
use ezal_core::session::CommandFreshness;
use ezal_core::tracking::*;

const TLES: &str = include_str!("fixtures/weather-2026-10-07.tle");
/// METEOR-M N2-3 in the recorded elements, as the forecast finds it.
const PASS: PassEvent = PassEvent {
    norad_id: 57166,
    aos_unix_s: 1_791_406_775,
    los_unix_s: 1_791_407_532,
};
/// Uptime when the clock first synchronizes and the operator connects.
const BOOT_MS: u64 = 2000;
/// Enough to plan and then prepare before AOS.
const BOOT_BEFORE_AOS_MS: u64 = 300_000;
const TICK_MS: u64 = 10;
/// Worker units between output services. Planning then takes device
/// seconds, as on the Pico, so feedback keeps arriving while it runs.
const UNITS_PER_TICK: u32 = 200;

struct Device {
    state: CoordinationState,
    actuator: Actuator,
    motor: MotorModel,
    monitor: FeedbackMonitor,
    worker: PlanningWorker,
    catalog: Catalog,
    flicker: bool,
    reads: u64,
    operator: u32,
    sequence: u64,
    now: u64,
}

fn source(catalog: &Catalog, norad_id: u64) -> Option<TrackingSource> {
    Some((
        catalog.snapshot(norad_id)?,
        heapless::Vec::from_slice(&[PASS]).unwrap(),
    ))
}

impl Device {
    /// Boot with rested motors, as after the startup rest or its override,
    /// and a connected operator holding control.
    fn boot(flicker: bool) -> Self {
        let utc0 = PASS.aos_unix_s as u64 * 1000 - BOOT_BEFORE_AOS_MS;
        let mut catalog = Catalog::new();
        catalog
            .commit(download(TLES, (utc0 / 1000) as i64), BOOT_MS)
            .unwrap();
        let config = installation::planning_config();
        let mut device = Self {
            state: CoordinationState::new(),
            actuator: Actuator::with_config(installation::ACTUATOR, DutyHistory::Known([0, 0])),
            motor: MotorModel::new(
                config.reference,
                HARD_CODED_CALIBRATION,
                Pointing::new(180.0, 10.0),
                [0, 0],
            )
            .unwrap(),
            monitor: FeedbackMonitor::new(HARD_CODED_CALIBRATION, None),
            worker: PlanningWorker::new(HARD_CODED_CALIBRATION, config, InputSettings::default()),
            catalog,
            flicker,
            reads: 0,
            operator: 0,
            sequence: 0,
            now: 0,
        };
        device.state.publish_network(true, 0);
        device.until(BOOT_MS);
        let mut clock = UtcClock::new();
        clock
            .synchronize(BOOT_MS, ntp_sample(utc0 + BOOT_MS))
            .unwrap();
        device.state.publish_clock(clock, BOOT_MS);
        device.operator = device.state.register_client(BOOT_MS);
        device
    }

    fn uptime(utc_s: i64) -> u64 {
        utc_s as u64 * 1000 - (PASS.aos_unix_s as u64 * 1000 - BOOT_BEFORE_AOS_MS)
    }

    /// The firmware feedback task's acquisition, through its validation.
    fn read(&mut self) {
        let exact = HARD_CODED_CALIBRATION
            .position_to_feedback(self.motor.position())
            .unwrap();
        self.reads += 1;
        let noise = if self.flicker {
            2 * (self.reads % 2) as i32
        } else {
            0
        };
        let code = |mv: i32| (mv + noise) / 2 * 2;
        let readings = FeedbackVoltages {
            a0_elevation_mv: code(exact.a0_elevation_mv),
            a1_azimuth_mv: code(exact.a1_azimuth_mv),
        };
        let update = self.monitor.update(self.now, self.now - 5, Ok(readings));
        self.state
            .publish_feedback(update.snapshot, update.interlock, self.now);
    }

    /// The firmware output task's service order, applying every transition.
    fn service(&mut self) {
        let update = self.state.service_actuator(&mut self.actuator, self.now);
        self.motor.advance(update.output, 0).unwrap();
        self.state.record_actuator(&self.actuator, self.now);
        self.state.execute_tracking(self.now);
        let update = self.state.service_actuator(&mut self.actuator, self.now);
        self.motor.advance(update.output, 0).unwrap();
        self.state.record_actuator(&self.actuator, self.now);
        assert_eq!(self.actuator.fault(), None, "at {} ms", self.now);
    }

    fn tick(&mut self) {
        self.motor.advance(self.actuator.actual(), TICK_MS).unwrap();
        self.now += TICK_MS;
        self.state.publish_network(true, self.now);
        if self.now.is_multiple_of(FEEDBACK_PERIOD_MS) {
            self.read();
        }
        if self.now.is_multiple_of(1000) {
            if let Some(grant) = self.state.operator(self.operator, self.now) {
                self.state.renew_operator(grant.token, self.now);
            }
        }
        self.service();
        let catalog = &self.catalog;
        if self.now.is_multiple_of(100) {
            self.state
                .refresh_tracking(|norad| source(catalog, norad), self.now);
        }
        let now = self.now;
        for _ in 0..UNITS_PER_TICK {
            let unit = self
                .worker
                .poll(&mut self.state, |norad| source(catalog, norad), || now);
            if unit == PlanningUnit::Idle {
                break;
            }
        }
    }

    fn until(&mut self, at: u64) {
        while self.now < at {
            self.tick();
        }
    }

    /// Tick until `done`, failing after `limit_ms` of device time.
    fn wait_for(&mut self, limit_ms: u64, done: impl Fn(&Self) -> bool) {
        let deadline = self.now + limit_ms;
        while !done(self) {
            assert!(self.now < deadline, "{:?}", self.snapshot());
            self.tick();
        }
    }

    fn snapshot(&self) -> TrackingSnapshot {
        self.state.tracking().snapshot()
    }

    fn supported(&self) -> bool {
        self.snapshot().plan == PlanStatus::Supported
    }

    fn request(&mut self, action: TrackingAction) -> Result<u64, TrackingRejection> {
        let grant = self.state.operator(self.operator, self.now).unwrap();
        let mut freshness = CommandFreshness::new(self.operator);
        self.sequence += 1;
        let intent = TrackingIntent {
            proof: freshness.issue(self.now).unwrap().token,
            lease: grant.token.generation(),
            sequence: self.sequence,
            revision: self.snapshot().revision,
            action,
        };
        self.state
            .request_tracking(self.operator, intent, &freshness, self.now, |p| p == PASS)
            .result
    }

    /// The tracking projection a dashboard receives.
    fn telemetry(&self) -> String {
        let mut json = String::new();
        self.state
            .tracking_telemetry(self.operator, self.now)
            .write_json(&mut json)
            .unwrap();
        json
    }
}

#[test]
fn flickering_feedback_plans_arms_and_follows_the_recorded_pass() {
    let mut device = Device::boot(true);
    device.request(TrackingAction::Select(PASS)).unwrap();
    device.wait_for(60_000, Device::supported);
    assert!(device
        .telemetry()
        .contains(r#""plan":{"status":"supported""#));
    device.request(TrackingAction::Arm).unwrap();
    let mut phases = vec![device.snapshot().phase];
    while device.now < Device::uptime(PASS.los_unix_s) + 5000 {
        device.tick();
        let phase = device.snapshot().phase;
        if phases.last() != Some(&phase) {
            phases.push(phase);
        }
    }
    assert_eq!(
        phases,
        [
            TrackingPhase::Armed,
            TrackingPhase::Preparing,
            TrackingPhase::Ready,
            TrackingPhase::Following,
            TrackingPhase::Complete,
        ]
    );
    let summary = device.state.tracking().last_summary().unwrap();
    assert_eq!(summary.termination.reason, TrackingStop::LossOfSignal);
    assert!(summary.reception.is_complete());
    assert_eq!(summary.deviation.outside_tolerance_ms, 0);
    assert!(summary.deviation.maximum_deg.unwrap() < 2.0, "{summary:?}");
}

#[test]
fn return_to_manual_keeps_the_pass_replanning_and_cancel_clears_it() {
    let mut device = Device::boot(false);
    device.request(TrackingAction::Select(PASS)).unwrap();
    device.wait_for(60_000, Device::supported);
    device.request(TrackingAction::Arm).unwrap();
    device.request(TrackingAction::Manual).unwrap();
    assert_eq!(device.snapshot().mode, TrackingMode::Manual);
    assert!(device.snapshot().selection.is_some());
    device.wait_for(60_000, Device::supported);
    device.request(TrackingAction::Cancel).unwrap();
    device.until(device.now + 5000);
    let state = device.snapshot();
    assert_eq!(
        (state.phase, state.plan),
        (TrackingPhase::Cancelled, PlanStatus::Unplanned)
    );
    assert_eq!(state.selection, None);
    assert_eq!(device.state.tracking().planning_ticket(), None);
    let telemetry = device.telemetry();
    assert!(telemetry.contains(r#""selection":null"#), "{telemetry}");
    assert!(
        telemetry.contains(r#""plan":{"status":"unplanned""#),
        "{telemetry}"
    );
}
