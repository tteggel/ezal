//! Offline preparation and complete-pass planning with explicit uncertainty.
use ezal_core::actuator::{Actuator, ActuatorConfig, ActuatorInput};
use ezal_core::ads1015::FEEDBACK_FULL_SCALE;
use ezal_core::drive::RelayConfig;
use ezal_core::duty::{DutyConfig, DutyHistory};
use ezal_core::feedback::FeedbackSample;
use ezal_core::interlock::FeedbackInterlock;
use ezal_core::network::NetworkGate;
use ezal_core::orbit::{Catalog, GroundStation, LookAngle, Observer};
use ezal_core::passes::{Config, Ephemeris, Search, Track};
use ezal_core::position::{AxisCalibration, InstalledCalibration, Pointing, PositionCalibration};
use ezal_core::tracking::{Planner, PlanningConfig, PlanningWindow, RestingStart};

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

fn plan(name: &str, source: impl Ephemeris, window: PlanningWindow, relays: RelayConfig) {
    let installed = InstalledCalibration::new(CAL, FEEDBACK_FULL_SCALE).unwrap();
    let mut actuator = Actuator::with_config(
        ActuatorConfig {
            relays,
            duty: DutyConfig::CONFIRMED_AC,
            ..ActuatorConfig::DEFAULT
        },
        DutyHistory::Known([0, 0]),
    );
    let mut network = NetworkGate::new();
    let mut feedback = None;
    for at_ms in [0, 2000] {
        feedback = Some(
            FeedbackSample::from_voltages(
                installed,
                CAL.position_to_feedback(Pointing::new(180.0, 10.0))
                    .unwrap(),
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
    let mut planner = Planner::new(
        source,
        window,
        CAL,
        RestingStart {
            at_ms: 2000,
            feedback: feedback.unwrap(),
            actuator,
            network,
        },
        PlanningConfig::default(),
    )
    .unwrap();
    let begin = std::time::Instant::now();
    while !planner.is_finished() {
        if let Err(error) = planner.step() {
            println!("{name}: planning failed: {error:?}");
            return;
        }
    }
    let report = planner.into_report().unwrap();
    println!(
        "{name}: {} ({} candidates, {} geometry rejections, {} steps, {:.2}s host)",
        if report.plan.is_some() {
            "supported under assumptions"
        } else {
            "no supported route found"
        },
        report.evaluated_candidates,
        report.geometry_rejections,
        report.work_steps,
        begin.elapsed().as_secs_f64()
    );
    if let Some(attempt) = report.closest_attempt {
        println!(
            "  {:?}; {}/9 scenarios complete; failure {:?} in {:?}, scenario {:?}",
            attempt.choice,
            attempt.completed_scenarios,
            attempt.failure,
            attempt.failure_phase,
            attempt.failure_scenario
        );
        println!("  peak {:.3}° sampled / {:.3}° bounded; gaps {:.2}s{}; preparation {:.2}s, lead {:.2}s",
            attempt.sampled_peak_deg, attempt.bounded_peak_deg, attempt.gap_time_ms as f64 / 1000.0,
            if attempt.gaps_truncated { " (interval list truncated)" } else { "" },
            attempt.preparation_ms as f64 / 1000.0, attempt.preparation_lead_ms as f64 / 1000.0);
        println!(
            "  on-time {:.2}/{:.2}s az/el; maximum {} reversals, {} transitions",
            attempt.energized_ms[0] as f64 / 1000.0,
            attempt.energized_ms[1] as f64 / 1000.0,
            attempt.reversals,
            attempt.transitions
        );
    }
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let relays = match args.as_slice() {
        [] => RelayConfig::DEFAULT,
        [flag] if flag == "--candidate-timing" => RelayConfig::TRACKING_CANDIDATE,
        _ => panic!("usage: tracking-plan [--candidate-timing]"),
    };
    println!("Offline planner: synthetic full travel, assumed AC profiles, nine motor/feedback scenarios.");
    println!("3° target with 1° uncertainty reserve; assumed true sky rate <=2°/s. Not hardware validation.");
    println!("Relay policy {relays:?}; initial pose 180°/10°, two minutes before AOS.\n");
    let window = PlanningWindow {
        aos_unix_s: 0.0,
        aos_at_ms: 122_000,
        duration_ms: 600_000,
    };
    for (name, miss_deg) in [("Exact zenith", 0.0_f64), ("88° peak", 2.0_f64)] {
        plan(
            name,
            |time: f64| {
                let angle = (10.0 + 160.0 * time / 600.0).to_radians();
                let miss = miss_deg.to_radians();
                Some(LookAngle {
                    azimuth_deg: miss
                        .sin()
                        .atan2(-miss.cos() * angle.cos())
                        .to_degrees()
                        .rem_euclid(360.0),
                    elevation_deg: (miss.cos() * angle.sin()).asin().to_degrees(),
                })
            },
            window,
            relays,
        );
    }
    let epoch = 1_791_385_200;
    let mut catalog = Catalog::new();
    catalog.update(
        include_str!("../tests/fixtures/weather-2026-10-07.tle"),
        epoch,
    );
    let station = GroundStation::new(Observer::CLEVEDON).unwrap();
    let orbit = catalog
        .orbits()
        .find(|orbit| orbit.satellite.norad_id == 39260)
        .unwrap();
    let mut search = Search::new(
        epoch,
        epoch + 3600,
        Config::DEFAULT,
        orbit.satellite,
        Track {
            orbit,
            station: &station,
        },
    )
    .unwrap();
    let pass = loop {
        assert!(!search.is_finished(), "fixture pass");
        if let Some(pass) = search.step().unwrap() {
            break pass;
        }
    };
    plan(
        "Recorded FENGYUN 3C",
        Track {
            orbit,
            station: &station,
        },
        PlanningWindow {
            aos_unix_s: pass.aos_unix_s as f64,
            aos_at_ms: 122_000,
            duration_ms: ((pass.los_unix_s - pass.aos_unix_s) * 1000) as u32,
        },
        relays,
    );
}
