//! Portable planning fixtures shared by integration tests and the WASM corpus.
use super::*;
use ezal_core::catalog::Catalog;
use ezal_core::passes::Ephemeris;
use ezal_core::tracking::offload::PlanningInput;
use ezal_core::tracking::*;

pub fn portable(case: &str) -> (ExecutionInputs, PlanningInput) {
    let late = case == "partial";
    let duration_s = if case == "source-error" {
        1800
    } else if late {
        90
    } else {
        30
    };
    let event = PassEvent {
        norad_id: 39260,
        aos_unix_s: 1_791_385_770,
        los_unix_s: 1_791_385_770 + duration_s,
    };
    let aos = if late { 1000 } else { 122_000 };
    let utc = event.aos_unix_s as u64 * 1000 + 2000 - aos;
    let mut clock = ntp::UtcClock::new();
    clock.synchronize(2000, ntp_sample(utc)).unwrap();
    let mut catalog = Catalog::new();
    catalog
        .commit(
            download(
                include_str!("../fixtures/weather-2026-10-07.tle"),
                (utc / 1000) as i64,
            ),
            2000,
        )
        .unwrap();
    let mut runtime = TrackingRuntime::new();
    let token = runtime
        .claim(
            1,
            RuntimeObservation {
                now_ms: 2000,
                clock,
                network: NetworkGate::new(),
            },
        )
        .unwrap()
        .token;
    let observation = RuntimeObservation {
        now_ms: 2000,
        clock,
        network: NetworkGate::new(),
    };
    runtime
        .request(
            TrackingRequest {
                operator: token,
                sequence: 1,
                expected_revision: runtime.snapshot().revision,
                action: TrackingAction::Select(event),
            },
            observation,
        )
        .unwrap();
    runtime
        .refresh(&[event], PlanRevisions { orbit: 1, start: 1 }, observation)
        .unwrap();
    let config = if case == "nominal" || case == "partial" {
        nominal()
    } else {
        PlanningConfig::default()
    };
    let window = PlanningWindow {
        aos_unix_s: event.aos_unix_s as f64,
        aos_at_ms: aos,
        duration_ms: duration_s as u32 * 1000,
    };
    let inputs = ExecutionInputs::capture(
        runtime.planning_ticket().unwrap(),
        catalog.snapshot(event.norad_id).unwrap(),
        InputSettings::default(),
        clock,
        window,
        &config,
        2000,
    )
    .unwrap();
    let look = inputs.look_at(window.aos_unix_s).unwrap();
    let pose = Pointing::new(look.azimuth_deg as f32, look.elevation_deg as f32);
    let used = if case == "unsupported" {
        [300_000; 2]
    } else {
        [1000, 2000]
    };
    let start = resting_start(FULL_TRAVEL_CALIBRATION, pose, used);
    let track = PlanningTrack::new(inputs.clone()).prepare().unwrap();
    let mut portable = PlanningInput {
        job: inputs.ticket().serial(),
        track,
        window,
        calibration: FULL_TRAVEL_CALIBRATION,
        start,
        config,
    };
    if case == "invalid" {
        portable.config.step_ms = 0;
    }
    (inputs, portable)
}
