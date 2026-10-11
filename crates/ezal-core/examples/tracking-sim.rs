//! Offline development replay. No network access, GPIOs or installed calibration.
//! Run with `cargo run -p ezal-core --target <host> --example tracking-sim`.

use ezal_core::orbit::{Catalog, GroundStation, Observer};
use ezal_core::passes::{Config, Search, Track};
use ezal_core::position::{AxisCalibration, Pointing, PositionCalibration};
use ezal_core::tracking::{
    FollowingController, MotorProfile, Simulation, SimulationError, SimulationSettings,
    TrackingTarget,
};

// Synthetic full-travel calibration. This never replaces the firmware constants.
const CALIBRATION: PositionCalibration = PositionCalibration {
    azimuth: AxisCalibration {
        angle_min_deg: 0.0,
        angle_max_deg: 450.0,
        voltage_min_mv: 200,
        voltage_max_mv: 2_900,
    },
    elevation: AxisCalibration {
        angle_min_deg: 0.0,
        angle_max_deg: 180.0,
        voltage_min_mv: 2_800,
        voltage_max_mv: 1_000,
    },
};
const START: i64 = 1_791_385_200; // Fixture epoch: 2026-10-07T15:00:00Z

fn replay(
    name: &str,
    duration_ms: u64,
    profile: MotorProfile,
    mut target_at: impl FnMut(u64) -> TrackingTarget,
) {
    let initial = target_at(0).mechanical;
    let settings = SimulationSettings::default();
    let mut sim = Simulation::new(
        CALIBRATION,
        initial,
        profile,
        FollowingController::new(MotorProfile::AC_50HZ, 0.5).unwrap(),
        settings,
    )
    .unwrap();
    let result = (|| -> Result<(), SimulationError> {
        while sim.now_ms() < duration_ms {
            let target = target_at(sim.now_ms());
            sim.step_target(Some(target))?;
        }
        // Inhibit even if the relays are currently idle: activation may be pending.
        sim.step(None)?;
        while !sim.is_settled() {
            sim.step(None)?;
        }
        Ok(())
    })();
    let summary = sim.summary();
    let outcome = match result {
        Err(error) => format!("stopped: {error:?}"),
        Ok(()) if summary.max_deviation_deg > settings.tolerance_deg => {
            "complete; exceeds modeled allocation".to_owned()
        }
        Ok(()) => "complete; within sampled allocation".to_owned(),
    };
    println!(
        "{name:<31} {:>6.2}° {:>7.1}s  {:>5.1}/{:<5.1}s  {:>4}/{:<4}  {outcome}",
        summary.max_deviation_deg,
        summary.outside_tolerance_ms as f64 / 1_000.0,
        summary.energized_ms[0] as f64 / 1_000.0,
        summary.energized_ms[1] as f64 / 1_000.0,
        summary.reversals[0],
        summary.reversals[1],
    );
}

// A smooth path with a fixed cross-track offset from zenith. The exact route
// has a rapid azimuth turn near the peak; a fixed-azimuth over-the-top route
// is continuous but deliberately offsets the desired sky direction.
fn synthetic_target(at_ms: u64, miss_deg: f64, over_top: bool) -> TrackingTarget {
    let angle_deg = 10.0 + 160.0 * at_ms as f64 / 600_000.0;
    let angle = angle_deg.to_radians();
    let miss = miss_deg.to_radians();
    let east = miss.sin();
    let north = -miss.cos() * angle.cos();
    let up = miss.cos() * angle.sin();
    let sky = Pointing::new(
        east.atan2(north).to_degrees().rem_euclid(360.0) as f32,
        up.asin().to_degrees() as f32,
    );
    let mechanical = if over_top {
        Pointing::new(180.0, angle_deg as f32)
    } else {
        sky
    };
    TrackingTarget { mechanical, sky }
}

fn main() {
    println!("Tracking development replay — current 500 ms on / 2 s off actuator timing.");
    println!("Unmeasured AC profiles, synthetic calibration, initially at the first target.");
    println!("20 ms maximum sampling; 2° modeled allocation leaves 1° for unmodeled uncertainty.");
    println!("No preparation planner or reacquisition; sampled results are not feasibility certification.\n");
    println!(
        "case                              peak    >2° time    on az/el      reversals   result"
    );
    for (name, profile) in [
        ("slow", MotorProfile::AC_SLOW),
        ("nominal", MotorProfile::AC_50HZ),
        ("fast", MotorProfile::AC_FAST),
    ] {
        for (label, miss, over_top) in [
            ("zenith conventional", 0.0, false),
            ("zenith over-top", 0.0, true),
            ("88deg conventional", 2.0, false),
            ("88deg offset over-top", 2.0, true),
        ] {
            replay(&format!("{label} {name}"), 600_000, profile, |at_ms| {
                synthetic_target(at_ms, miss, over_top)
            });
        }
    }

    let mut catalog = Catalog::new();
    catalog.update(
        include_str!("../tests/fixtures/weather-2026-10-07.tle"),
        START,
    );
    let station = GroundStation::new(Observer::CLEVEDON).unwrap();
    let orbit = catalog
        .orbits()
        .find(|o| o.satellite.norad_id == 39260)
        .unwrap();
    let mut search = Search::new(
        START,
        START + 3_600,
        Config::DEFAULT,
        orbit.satellite,
        Track {
            orbit,
            station: &station,
        },
    )
    .unwrap();
    let pass = loop {
        assert!(
            !search.is_finished(),
            "recorded fixture must contain a pass"
        );
        if let Some(pass) = search.step().unwrap() {
            break pass;
        }
    };
    println!(
        "\nRecorded {} pass, AOS {}, LOS {}, peak {:.2}°:",
        orbit.satellite.name, pass.aos_unix_s, pass.los_unix_s, pass.peak_elevation_deg
    );
    for (name, profile) in [
        ("slow", MotorProfile::AC_SLOW),
        ("nominal", MotorProfile::AC_50HZ),
        ("fast", MotorProfile::AC_FAST),
    ] {
        let mut previous_azimuth: Option<f64> = None;
        replay(
            &format!("recorded conventional {name}"),
            (pass.los_unix_s - pass.aos_unix_s) as u64 * 1_000,
            profile,
            |at_ms| {
                let look = orbit
                    .look_at(&station, pass.aos_unix_s as f64 + at_ms as f64 / 1_000.0)
                    .unwrap();
                let sky = Pointing::new(look.azimuth_deg as f32, look.elevation_deg as f32);
                // Select the locally continuous bearing for this baseline replay.
                // Crossing an installed stop still fails; this is not a route planner.
                let azimuth = previous_azimuth.map_or(look.azimuth_deg, |previous| {
                    previous + (look.azimuth_deg - previous + 180.0).rem_euclid(360.0) - 180.0
                });
                previous_azimuth = Some(azimuth);
                TrackingTarget {
                    mechanical: Pointing::new(azimuth as f32, sky.elevation_deg),
                    sky,
                }
            },
        );
    }
}
