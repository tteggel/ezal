use super::*;
use crate::actuator::{Actuator, ActuatorConfig, ActuatorInput};
use crate::drive::{AzimuthDirection, Command, CommandEnvelope, DriveCommand};
use crate::duty::{DutyConfig, DutyHistory};
use crate::interlock::FeedbackInterlock;
use crate::network::NetworkGate;
use crate::orbit::LookAngle;
use crate::test_support::{full_travel_installed, full_travel_sample, FULL_TRAVEL_CALIBRATION};
use crate::tracking::*;
use std::cell::Cell;
use std::sync::OnceLock;

const AOS: u64 = 120_000;

fn rest(pose: Pointing, now: u64, used: [u64; 2]) -> RestingStart {
    let mut actuator = Actuator::with_config(
        ActuatorConfig {
            duty: DutyConfig::CONFIRMED_AC,
            ..ActuatorConfig::DEFAULT
        },
        DutyHistory::Known(used),
    );
    let mut network = NetworkGate::new();
    for at in [now - 2000, now] {
        network.observe(at, true);
        service(&mut actuator, pose, at, network, Command::Inhibit);
    }
    RestingStart {
        at_ms: now,
        feedback: full_travel_sample(pose, now),
        actuator,
        network,
    }
}

fn service(
    actuator: &mut Actuator,
    pose: Pointing,
    at: u64,
    network: NetworkGate,
    command: Command,
) {
    let mut interlock = FeedbackInterlock::new(full_travel_installed());
    interlock.publish(Some(full_travel_sample(pose, at)));
    actuator.step(
        ActuatorInput {
            interlock,
            network,
            command: Some(CommandEnvelope {
                command,
                valid_until_ms: at + 750,
            }),
        },
        at,
    );
}

fn plan() -> TrackingPlan {
    static PLAN: OnceLock<TrackingPlan> = OnceLock::new();
    PLAN.get_or_init(|| {
        let mut planner = Planner::new(
            |_| {
                Some(LookAngle {
                    azimuth_deg: 180.0,
                    elevation_deg: 45.0,
                })
            },
            PlanningWindow {
                aos_unix_s: 1000.0,
                aos_at_ms: AOS,
                duration_ms: 90_000,
            },
            FULL_TRAVEL_CALIBRATION,
            rest(Pointing::new(180.0, 45.0), 2000, [0, 0]),
            PlanningConfig::default(),
        )
        .unwrap();
        while !planner.is_finished() {
            planner.step().unwrap();
        }
        planner.take_report().unwrap().plan.unwrap()
    })
    .clone()
}

fn search(plan: &TrackingPlan, start: RestingStart, deadline: u64) -> InterceptSearch {
    let mut search = InterceptSearch::new(plan, start, deadline).unwrap();
    let calls = Cell::new(0usize);
    let source = |_: f64| {
        calls.set(calls.get() + 1);
        Some(LookAngle {
            azimuth_deg: 180.0,
            elevation_deg: 45.0,
        })
    };
    while !search.finished {
        let before = calls.get();
        search.step(plan, &source).unwrap();
        assert!(calls.get() - before <= 1);
        assert!(search.work_steps <= MAX_PLANNING_STEPS + 1);
    }
    search
}

#[test]
fn future_intercept_qualifies_all_scenarios_and_the_whole_remaining_pass() {
    let plan = plan();
    assert_eq!(plan.config.scenarios.len(), 9);
    let result = search(
        &plan,
        rest(Pointing::new(180.0, 35.0), AOS + 2000, [1000, 12000]),
        AOS + 32000,
    );
    let intercept = result
        .result
        .expect("static ten-degree lag is recoverable in all configured scenarios");
    assert!(intercept.join_at_ms > intercept.launch_at_ms + 1000);
    assert!(intercept.join_at_ms + 2500 < AOS + 32000);
    // Reaching an angle is insufficient when there is no remaining motor duty.
    let failed = search(
        &plan,
        rest(Pointing::new(180.0, 35.0), AOS + 2000, [300000; 2]),
        AOS + 32000,
    );
    assert!(failed.result.is_none());
    assert_eq!(failed.hold, RecoveryHold::NoIntercept);
}

#[test]
fn far_intercept_and_a_future_source_failure_never_publish_partial_success() {
    let plan = plan();
    let start = rest(Pointing::new(400.0, 150.0), AOS + 2000, [0, 0]);
    assert!(search(&plan, start, AOS + 32000).result.is_none());
    let mut search = InterceptSearch::new(
        &plan,
        rest(Pointing::new(180.0, 35.0), AOS + 2000, [0, 0]),
        AOS + 32000,
    )
    .unwrap();
    // An apparently reachable early join cannot hide failure later in the pass.
    let source = |utc: f64| {
        (utc < 1050.0).then_some(LookAngle {
            azimuth_deg: 180.0,
            elevation_deg: 45.0,
        })
    };
    loop {
        if search.step(&plan, &source).is_err() {
            break;
        }
        assert!(!search.finished);
    }
    assert!(search.result.is_none());
}

#[test]
fn intercept_keeps_the_frozen_overlap_or_over_top_representation() {
    for (mechanical, sky, kind, turns) in [
        (
            Pointing::new(370.0, 45.0),
            Pointing::new(10.0, 45.0),
            RouteKind::Conventional,
            1,
        ),
        (
            Pointing::new(180.0, 100.0),
            Pointing::new(0.0, 80.0),
            RouteKind::Inverted,
            0,
        ),
    ] {
        let mut plan = plan();
        plan.route.choice = RouteChoice {
            kind,
            azimuth_turns: turns,
        };
        for knot in &mut plan.route.points {
            knot.az = mechanical.azimuth_deg;
            knot.el = mechanical.elevation_deg;
        }
        let start = rest(
            Pointing::new(mechanical.azimuth_deg, mechanical.elevation_deg - 10.0),
            AOS + 2000,
            [0, 0],
        );
        let source = |_: f64| {
            Some(LookAngle {
                azimuth_deg: f64::from(sky.azimuth_deg),
                elevation_deg: f64::from(sky.elevation_deg),
            })
        };
        let mut search = InterceptSearch::new(&plan, start, AOS + 32000).unwrap();
        while !search.finished {
            search.step(&plan, &source).unwrap();
        }
        let intercept = search.result.unwrap();
        assert_eq!(intercept.pointing, mechanical);
        assert_eq!(
            intercept.target(&plan, intercept.join_at_ms + 1000),
            Some(mechanical)
        );
    }
}

#[test]
fn start_proof_rejects_changed_pose_and_hidden_relay_history() {
    let pose = Pointing::new(180.0, 35.0);
    let start = rest(pose, AOS + 2000, [1000, 12000]);
    let intercept = search(&plan(), start, AOS + 32000).result.unwrap();
    let now = intercept.launch_at_ms;
    let mut current = start;
    current.at_ms = now;
    current.feedback = full_travel_sample(pose, now);
    for at in (start.at_ms..=now).step_by(100) {
        current.network.observe(at, true);
    }
    service(
        &mut current.actuator,
        pose,
        now,
        current.network,
        Command::Inhibit,
    );
    assert!(intercept.start_unchanged(current));
    current.feedback = full_travel_sample(Pointing::new(180.0, 35.1), now);
    assert!(!intercept.start_unchanged(current));
    // Return to the same pose with a fresh sample, but lose continuous idle.
    current = start;
    let drive = Command::Drive(DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: None,
    });
    current.network.observe(start.at_ms + 100, true);
    service(
        &mut current.actuator,
        pose,
        start.at_ms + 100,
        current.network,
        drive,
    );
    current.network.observe(start.at_ms + 200, true);
    service(
        &mut current.actuator,
        pose,
        start.at_ms + 200,
        current.network,
        Command::Inhibit,
    );
    current.at_ms = now;
    current.feedback = full_travel_sample(pose, now);
    for at in (start.at_ms + 200..=now).step_by(100) {
        current.network.observe(at, true);
    }
    service(
        &mut current.actuator,
        pose,
        now,
        current.network,
        Command::Inhibit,
    );
    assert!(!intercept.start_unchanged(current));
}

#[test]
fn recovery_episode_has_one_deadline_and_old_sources_cannot_resume() {
    let mut session = TrackingLifecycle::new();
    let owner = session.claim(1, 2000).unwrap().token;
    let pass = PassEvent {
        norad_id: 39260,
        aos_unix_s: 1000,
        los_unix_s: 1090,
    };
    session
        .request(
            TrackingRequest {
                operator: owner,
                sequence: 1,
                expected_revision: session.snapshot().revision,
                action: TrackingAction::Select(pass),
            },
            2000,
        )
        .unwrap();
    session
        .refresh(&[pass], PlanRevisions::default(), 2000)
        .unwrap();
    session
        .publish_plan(
            session.planning_ticket().unwrap(),
            Ok(PlanningReport {
                plan: Some(plan()),
                closest_attempt: None,
                evaluated_candidates: 1,
                geometry_rejections: 0,
                work_steps: 1,
            }),
            (),
            119000,
            2000,
        )
        .unwrap();
    session
        .request(
            TrackingRequest {
                operator: owner,
                sequence: 2,
                expected_revision: session.snapshot().revision,
                action: TrackingAction::Arm,
            },
            2000,
        )
        .unwrap();
    session.tick(60000).unwrap();
    let run = session.snapshot().run.unwrap();
    session.preparation_ready(run, 62000).unwrap();
    session.tick(AOS).unwrap();
    session.take_command(AOS);
    let first = session
        .begin_reacquisition(run, AOS + 1000)
        .unwrap()
        .unwrap();
    assert_eq!(first.deadline_ms(), AOS + 31000);
    assert_eq!(
        session.begin_reacquisition(run, AOS + 20000).unwrap(),
        Some(first)
    );
    assert!(!session.tracking_command(run, CommandEnvelope::INHIBIT, AOS + 20000));
    assert_eq!(
        session.take_command(AOS + 20000),
        Some(CommandEnvelope::INHIBIT)
    );
    assert!(session.finish_reacquisition(first, AOS + 20000).unwrap());
    let second = session
        .begin_reacquisition(run, AOS + 21000)
        .unwrap()
        .unwrap();
    assert_ne!(first, second);
    assert!(!session.recovery_command(first, CommandEnvelope::INHIBIT, AOS + 22000));
    assert!(!session.finish_reacquisition(first, AOS + 22000).unwrap());
    session.tick(second.deadline_ms()).unwrap();
    assert_eq!(
        session.snapshot().termination.unwrap().reason,
        TrackingStop::ReacquisitionTimeout
    );
    assert_eq!(session.snapshot().mode, TrackingMode::Manual);
    assert!(!session
        .finish_reacquisition(second, second.deadline_ms())
        .unwrap());
}
