//! Generate real orbital inputs and native results for the actual WASM tests.
#[path = "../tests/common/mod.rs"]
mod common;
use ezal_core::tracking::offload::*;
use ezal_core::tracking::*;
use std::{fs, path::PathBuf};

fn main() {
    let destination = PathBuf::from(std::env::args().nth(1).expect("corpus directory"));
    fs::create_dir_all(&destination).unwrap();
    for case in [
        "nominal",
        "scenarios",
        "partial",
        "unsupported",
        "invalid",
        "source-error",
    ] {
        let (inputs, original) = common::planning::portable(case);
        let mut bytes = [0; INPUT_CAPACITY];
        let len = original.encode(&mut bytes).unwrap();
        fs::write(destination.join(format!("{case}.input")), &bytes[..len]).unwrap();
        let decoded = PlanningInput::decode(&bytes[..len]).unwrap();
        assert_eq!(decoded.start, original.start);
        assert_eq!(decoded.config, original.config);
        assert_eq!(decoded.window, original.window);
        let job = original.job;
        let outcome = match decoded.planner() {
            Err(error) => format!("{{\"status\":3,\"error\":\"{error:?}\"}}"),
            Ok(mut planner) => {
                let mut local = Planner::new(
                    PlanningTrack::new(inputs),
                    original.window,
                    original.calibration,
                    original.start,
                    original.config,
                )
                .unwrap();
                while !planner.is_finished() {
                    let _ = planner.step();
                }
                while !local.is_finished() {
                    let _ = local.step();
                }
                let report = planner.take_report();
                assert_eq!(
                    report,
                    local.take_report(),
                    "lazy versus exported knots: {case}"
                );
                match report {
                    Err(error) => format!("{{\"status\":3,\"error\":\"{error:?}\"}}"),
                    Ok(report) => match report.plan {
                        Some(plan) => {
                            if case == "partial" {
                                assert!(!plan.reception().is_complete());
                            }
                            let u = UploadedPlan::new(job, &plan);
                            // Rechecking the compact upload must reproduce every field.
                            let mut check = PlanningInput::decode(&bytes[..len])
                                .unwrap()
                                .planner()
                                .unwrap();
                            check.verify_candidate(u.candidate, u.partial).unwrap();
                            while !check.is_finished() {
                                check.step().unwrap();
                            }
                            let checked = check.take_report().unwrap();
                            assert_eq!(checked.plan.as_ref(), Some(&plan), "upload replay: {case}");
                            assert!(u.matches(&plan));
                            format!("{{\"status\":1,\"job\":\"{}\",\"candidate\":{},\"partial\":{},\"digest\":\"{}\",\"work_steps\":{}}}", u.job, u.candidate, u.partial, u.digest, report.work_steps)
                        }
                        None => format!(
                        "{{\"status\":2,\"error\":\"No supported route found\",\"work_steps\":{}}}",
                        report.work_steps
                    ),
                    },
                }
            }
        };
        fs::write(destination.join(format!("{case}.json")), &outcome).unwrap();
        println!("{case}: {len} bytes · {outcome}");
    }
}
