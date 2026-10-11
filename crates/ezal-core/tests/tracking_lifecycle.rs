use ezal_core::actuator::{Actuator, ActuatorConfig, ActuatorInput};
use ezal_core::ads1015::FEEDBACK_FULL_SCALE;
use ezal_core::drive::{AzimuthDirection, Command, DriveCommand};
use ezal_core::duty::{DutyConfig, DutyHistory};
use ezal_core::feedback::FeedbackSample;
use ezal_core::interlock::FeedbackInterlock;
use ezal_core::mailbox::CommandEnvelope;
use ezal_core::network::NetworkGate;
use ezal_core::operator::{LeaseError, OperatorToken};
use ezal_core::orbit::LookAngle;
use ezal_core::position::{AxisCalibration, InstalledCalibration, Pointing, PositionCalibration};
use ezal_core::tracking::*;
use std::sync::OnceLock;

const EVENT: PassEvent = PassEvent {
    norad_id: 39260,
    aos_unix_s: 1000,
    los_unix_s: 1030,
};
const REVISIONS: PlanRevisions = PlanRevisions {
    orbit: 1,
    observer: 1,
    calibration: 1,
    motion: 1,
    clock: 1,
    start: 1,
};
const AOS: u64 = 122_000;
const PREP: u64 = 62_000;
const LOS: u64 = 152_000;
const CW: DriveCommand = DriveCommand {
    azimuth: Some(AzimuthDirection::Clockwise),
    elevation: None,
};

fn report(timing: PreparationTiming) -> PlanningReport {
    static SCHEDULED: OnceLock<PlanningReport> = OnceLock::new();
    static IMMEDIATE: OnceLock<PlanningReport> = OnceLock::new();
    let cache = if timing == PreparationTiming::Scheduled {
        &SCHEDULED
    } else {
        &IMMEDIATE
    };
    cache
        .get_or_init(|| {
            let calibration = PositionCalibration {
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
            let installed = InstalledCalibration::new(calibration, FEEDBACK_FULL_SCALE).unwrap();
            let mut actuator = Actuator::with_config(
                ActuatorConfig {
                    duty: DutyConfig::CONFIRMED_AC,
                    ..ActuatorConfig::DEFAULT
                },
                DutyHistory::Known([0, 0]),
            );
            let mut network = NetworkGate::new();
            let mut feedback = None;
            for at in [0, 2000] {
                feedback = Some(
                    FeedbackSample::from_voltages(
                        installed,
                        calibration
                            .position_to_feedback(Pointing::new(180.0, 45.0))
                            .unwrap(),
                        at,
                    )
                    .unwrap(),
                );
                let mut interlock = FeedbackInterlock::new(installed);
                interlock.publish(feedback);
                network.observe(at, true);
                actuator.step(
                    ActuatorInput {
                        interlock,
                        network,
                        command: None,
                    },
                    at,
                );
            }
            let mut config = PlanningConfig {
                preparation_timing: timing,
                ..PlanningConfig::default()
            };
            config.scenarios.clear();
            config.scenarios.push(Scenario::NOMINAL).unwrap();
            let mut planner = Planner::new(
                |_: f64| {
                    Some(LookAngle {
                        azimuth_deg: 180.0,
                        elevation_deg: 45.0,
                    })
                },
                PlanningWindow {
                    aos_unix_s: 1000.0,
                    aos_at_ms: AOS,
                    duration_ms: 30_000,
                },
                calibration,
                RestingStart {
                    at_ms: 2000,
                    feedback: feedback.unwrap(),
                    actuator,
                    network,
                },
                config,
            )
            .unwrap();
            while !planner.is_finished() {
                planner.step().unwrap();
            }
            let report = planner.into_report().unwrap();
            assert!(report.plan.is_some());
            report
        })
        .clone()
}

fn request(
    session: &TrackingSession<&'static str>,
    owner: OperatorToken,
    sequence: u64,
    action: TrackingAction,
) -> TrackingRequest {
    TrackingRequest {
        operator: owner,
        sequence,
        expected_revision: session.snapshot().revision,
        action,
    }
}

fn selected() -> (TrackingSession<&'static str>, OperatorToken) {
    let mut session = TrackingSession::new();
    let token = session.claim(1, 2000).unwrap().token;
    session
        .request(
            request(&session, token, 1, TrackingAction::Select(EVENT)),
            2000,
        )
        .unwrap();
    session.refresh(&[EVENT], REVISIONS, 2000).unwrap();
    let ticket = session.planning_ticket().unwrap();
    session
        .publish_plan(
            ticket,
            Ok(report(PreparationTiming::Scheduled)),
            "original inputs",
            AOS,
            2000,
        )
        .unwrap();
    assert_eq!(session.take_command(2000), Some(CommandEnvelope::INHIBIT));
    (session, token)
}

fn armed() -> (TrackingSession<&'static str>, OperatorToken) {
    let (mut session, token) = selected();
    session
        .request(request(&session, token, 2, TrackingAction::Arm), 2000)
        .unwrap();
    assert_eq!(session.take_command(2000), Some(CommandEnvelope::INHIBIT));
    (session, token)
}

fn output(now: u64, command: Command) -> CommandEnvelope {
    CommandEnvelope {
        command,
        valid_until_ms: now + 750,
    }
}

#[test]
fn boot_and_selection_are_manual_and_do_not_begin_preparation() {
    let empty = TrackingSession::<()>::new();
    assert_eq!(empty.snapshot().selection, None);
    assert_eq!(empty.snapshot().mode, TrackingMode::Manual);
    assert_eq!(empty.snapshot().run, None);
    let (mut session, _) = selected();
    session.tick(PREP).unwrap();
    assert_eq!(session.snapshot().phase, TrackingPhase::Idle);
    assert_eq!(session.snapshot().plan, PlanStatus::Supported);
    // Expiry still inhibits the manual source, even though selection never drove it.
    assert_eq!(session.take_command(PREP), Some(CommandEnvelope::INHIBIT));
    assert_eq!(session.take_command(PREP), None);
}

#[test]
fn device_progresses_without_browser_and_holds_final_position_without_next_pass() {
    let (mut session, owner) = armed();
    assert!(session.release(owner, 2100).unwrap());
    session.tick(PREP - 1).unwrap();
    assert_eq!(session.snapshot().phase, TrackingPhase::Armed);
    session.tick(PREP).unwrap();
    let run = session.snapshot().run.unwrap();
    assert_eq!(session.snapshot().phase, TrackingPhase::Preparing);
    assert!(session.tracking_command(run, output(PREP, Command::Drive(CW)), PREP));
    assert_eq!(
        session.take_command(PREP),
        Some(output(PREP, Command::Drive(CW)))
    );
    assert!(session.preparation_ready(run, PREP + 2000).unwrap());
    session.tick(AOS).unwrap();
    assert_eq!(session.snapshot().phase, TrackingPhase::Following);
    assert!(session.tracking_command(run, output(LOS - 1, Command::Drive(CW)), LOS - 1));
    assert_eq!(session.take_command(LOS), Some(CommandEnvelope::INHIBIT));
    assert_eq!(session.snapshot().phase, TrackingPhase::Complete);
    assert_eq!(session.snapshot().mode, TrackingMode::Manual);
    assert_eq!(
        session.snapshot().termination.unwrap().reason,
        TrackingStop::LossOfSignal
    );
    assert!(!session.tracking_command(run, output(LOS, Command::Drive(CW)), LOS));
    assert_eq!(session.take_command(LOS + 10_000), None);
    assert_eq!(session.snapshot().selection.unwrap().original, EVENT);
}

#[test]
fn operator_expiry_reacquisition_and_old_disconnect_never_interrupt_tracking() {
    let (mut session, old) = armed();
    assert_eq!(
        session.claim(2, 2100),
        Err(LifecycleError::Lease(LeaseError::Occupied))
    );
    session.tick(PREP).unwrap(); // original lease expired long before preparation
    let run = session.snapshot().run.unwrap();
    assert!(session.tracking_command(run, output(PREP, Command::Drive(CW)), PREP));
    let new = session.claim(2, PREP + 100).unwrap().token;
    assert!(!session.release(old, PREP + 100).unwrap());
    assert_eq!(
        session.renew(old, PREP + 100),
        Err(LifecycleError::Lease(LeaseError::Invalid))
    );
    assert_eq!(
        session.take_command(PREP + 100),
        Some(output(PREP, Command::Drive(CW)))
    );
    assert!(session.release(new, PREP + 100).unwrap());
    assert_eq!(session.snapshot().run, Some(run));
    assert_eq!(session.take_command(PREP + 101), None);
}

#[test]
fn only_current_lease_can_mutate_and_stale_generations_cannot_cancel() {
    let (mut session, old) = armed();
    let new = session.claim(1, 7000).unwrap().token;
    let before = session.snapshot();
    for action in [
        TrackingAction::Select(EVENT),
        TrackingAction::Arm,
        TrackingAction::PrepareNow,
        TrackingAction::Cancel,
        TrackingAction::Manual,
    ] {
        let stale = request(&session, old, 100, action);
        assert_eq!(
            session.request(stale, 7000),
            Err(LifecycleError::Lease(LeaseError::Invalid))
        );
        assert_eq!(session.snapshot(), before);
    }
    let mut cancel = request(&session, new, 1, TrackingAction::Cancel);
    cancel.expected_revision = 0; // cancel survives arbitrary automatic plan revisions
    session.request(cancel, 7000).unwrap();
    assert_eq!(session.snapshot().phase, TrackingPhase::Cancelled);
    assert_eq!(session.take_command(7000), Some(CommandEnvelope::INHIBIT));
}

#[test]
fn exact_retries_are_idempotent_and_old_requests_cannot_revive_or_cancel_a_replacement() {
    let (mut session, owner) = selected();
    let arm = request(&session, owner, 2, TrackingAction::Arm);
    let receipt = session.request(arm, 2000).unwrap();
    session
        .refresh(
            &[EVENT],
            PlanRevisions {
                start: 2,
                ..REVISIONS
            },
            2100,
        )
        .unwrap();
    let snapshot = session.snapshot();
    assert_eq!(session.request(arm, 2100), Ok(receipt));
    assert_eq!(session.snapshot(), snapshot);
    let cancel = request(&session, owner, 3, TrackingAction::Cancel);
    let receipt = session.request(cancel, 2100).unwrap();
    assert_eq!(session.request(cancel, 2100), Ok(receipt));
    let replace = request(
        &session,
        owner,
        4,
        TrackingAction::Select(PassEvent {
            norad_id: 12345,
            ..EVENT
        }),
    );
    session.request(replace, 2100).unwrap();
    let snapshot = session.snapshot();
    assert_eq!(
        session.request(cancel, 2100),
        Err(LifecycleError::RequestOrder)
    );
    assert_eq!(
        session.request(arm, 2100),
        Err(LifecycleError::RequestOrder)
    );
    assert_eq!(session.snapshot(), snapshot);
    let reused = TrackingRequest {
        action: TrackingAction::Cancel,
        ..replace
    };
    assert_eq!(
        session.request(reused, 2100),
        Err(LifecycleError::RequestOrder)
    );
}

#[test]
fn auto_refresh_preserves_identity_and_discards_old_or_ambiguous_work() {
    let (mut session, owner) = armed();
    let event = session.snapshot().selection.unwrap().id;
    let shifted = PassEvent {
        aos_unix_s: 1010,
        los_unix_s: 1040,
        ..EVENT
    };
    session
        .refresh(
            &[shifted],
            PlanRevisions {
                orbit: 2,
                ..REVISIONS
            },
            2100,
        )
        .unwrap();
    let old = session.planning_ticket().unwrap();
    session
        .refresh(
            &[shifted],
            PlanRevisions {
                orbit: 3,
                ..REVISIONS
            },
            2200,
        )
        .unwrap();
    let latest = session.planning_ticket().unwrap();
    assert_ne!(old, latest);
    assert_eq!(session.snapshot().selection.unwrap().id, event);
    assert_eq!(
        session.publish_plan(
            old,
            Ok(report(PreparationTiming::Scheduled)),
            "old",
            AOS,
            2200
        ),
        Err(LifecycleError::StaleJob)
    );
    let mut result = report(PreparationTiming::Scheduled);
    result.plan.as_mut().unwrap().window.aos_unix_s += 10.0;
    session
        .publish_plan(latest, Ok(result), "new", AOS, 2200)
        .unwrap();
    assert_eq!(*session.bound_plan().unwrap().inputs(), "new");
    let stale = request(&session, owner, 3, TrackingAction::PrepareNow);
    session
        .refresh(
            &[shifted, shifted],
            PlanRevisions {
                orbit: 4,
                ..REVISIONS
            },
            2300,
        )
        .unwrap();
    assert_eq!(
        session.snapshot().plan,
        PlanStatus::Blocked(PlanProblem::Event(EventError::Ambiguous))
    );
    assert_eq!(
        session.request(stale, 2300),
        Err(LifecycleError::StateRevision)
    );
    session.tick(PREP).unwrap();
    assert_eq!(session.snapshot().run, None);
    assert_eq!(session.snapshot().phase, TrackingPhase::Armed);
    session.tick(AOS).unwrap();
    assert_eq!(
        session.snapshot().termination.unwrap().reason,
        TrackingStop::LatePreparation
    );
}

#[test]
fn freeze_is_atomic_and_only_installation_changes_replace_it_with_a_stop() {
    let (mut session, _) = armed();
    session.tick(PREP).unwrap();
    let run = session.snapshot().run.unwrap();
    let frozen = session.bound_plan().unwrap().ticket();
    session
        .refresh(
            &[],
            PlanRevisions {
                orbit: 20,
                clock: 20,
                observer: 20,
                start: 20,
                ..REVISIONS
            },
            PREP,
        )
        .unwrap();
    assert_eq!(session.snapshot().run, Some(run));
    assert_eq!(session.bound_plan().unwrap().ticket(), frozen);
    assert_eq!(*session.bound_plan().unwrap().inputs(), "original inputs");
    assert_eq!(
        session.publish_plan(
            frozen,
            Ok(report(PreparationTiming::Scheduled)),
            "late",
            AOS,
            PREP
        ),
        Err(LifecycleError::StaleJob)
    );
    session
        .refresh(
            &[],
            PlanRevisions {
                orbit: 20,
                clock: 20,
                observer: 20,
                start: 20,
                calibration: 2,
                ..REVISIONS
            },
            PREP + 1,
        )
        .unwrap();
    assert_eq!(
        session.snapshot().termination.unwrap().reason,
        TrackingStop::InstallationChanged
    );
    assert_eq!(
        session.take_command(PREP + 1),
        Some(CommandEnvelope::INHIBIT)
    );
    assert!(!session.tracking_command(run, output(PREP + 1, Command::Drive(CW)), PREP + 1));
}

#[test]
fn update_or_cancel_at_preparation_boundary_prevents_stale_activation() {
    for cancel in [false, true] {
        let (mut session, _) = armed();
        let owner = session.claim(2, PREP).unwrap().token;
        if cancel {
            session
                .request(request(&session, owner, 1, TrackingAction::Cancel), PREP)
                .unwrap();
        } else {
            session
                .refresh(
                    &[EVENT],
                    PlanRevisions {
                        start: 2,
                        ..REVISIONS
                    },
                    PREP,
                )
                .unwrap();
        }
        session.tick(PREP).unwrap();
        assert_eq!(session.snapshot().run, None);
        if cancel {
            assert_eq!(session.take_command(PREP), Some(CommandEnvelope::INHIBIT));
        } else {
            assert_eq!(session.take_command(PREP), None);
        }
    }
    let (mut session, _) = armed();
    session.tick(PREP).unwrap();
    let old = session.snapshot().run.unwrap();
    let owner = session.claim(2, PREP).unwrap().token;
    assert!(session.tracking_command(old, output(PREP, Command::Drive(CW)), PREP));
    session
        .request(
            request(&session, owner, 1, TrackingAction::Select(EVENT)),
            PREP,
        )
        .unwrap();
    assert!(!session.preparation_ready(old, PREP).unwrap());
    assert!(!session.tracking_command(old, output(PREP, Command::Drive(CW)), PREP));
    assert_eq!(session.take_command(PREP), Some(CommandEnvelope::INHIBIT));
    assert_eq!(session.snapshot().mode, TrackingMode::Manual);
}

#[test]
fn prepare_now_needs_a_new_plan_that_explicitly_evaluated_early_preparation() {
    let (mut session, owner) = armed();
    session
        .request(
            request(&session, owner, 3, TrackingAction::PrepareNow),
            2100,
        )
        .unwrap();
    let ticket = session.planning_ticket().unwrap();
    assert_eq!(ticket.preparation_timing(), PreparationTiming::Immediate);
    session.tick(2100).unwrap();
    assert_eq!(session.snapshot().run, None);
    session
        .publish_plan(
            ticket,
            Ok(report(PreparationTiming::Scheduled)),
            "wrong schedule",
            AOS,
            2100,
        )
        .unwrap();
    assert_eq!(
        session.snapshot().plan,
        PlanStatus::Blocked(PlanProblem::InvalidResult)
    );
    session
        .refresh(
            &[EVENT],
            PlanRevisions {
                start: 2,
                ..REVISIONS
            },
            2200,
        )
        .unwrap();
    let ticket = session.planning_ticket().unwrap();
    session
        .publish_plan(
            ticket,
            Ok(report(PreparationTiming::Immediate)),
            "early plan",
            AOS,
            2200,
        )
        .unwrap();
    session.tick(2200).unwrap();
    assert_eq!(session.snapshot().phase, TrackingPhase::Preparing);
    assert_eq!(*session.bound_plan().unwrap().inputs(), "early plan");
}

#[test]
fn manual_admission_cannot_cross_tracking_mode_or_reuse_a_moving_starting_pose() {
    let (mut session, owner) = selected();
    assert!(session.manual_command(owner, output(2100, Command::Jog(CW)), 2100));
    assert_eq!(session.snapshot().plan, PlanStatus::Unplanned);
    assert_eq!(
        session.request(request(&session, owner, 2, TrackingAction::Arm), 2100),
        Err(LifecycleError::NotReady)
    );
    assert_eq!(
        session.take_command(2100),
        Some(output(2100, Command::Jog(CW)))
    );
    session
        .refresh(
            &[EVENT],
            PlanRevisions {
                start: 2,
                ..REVISIONS
            },
            2100,
        )
        .unwrap();
    let ticket = session.planning_ticket().unwrap();
    session
        .publish_plan(
            ticket,
            Ok(report(PreparationTiming::Scheduled)),
            "new pose",
            AOS,
            2100,
        )
        .unwrap();
    session
        .request(request(&session, owner, 3, TrackingAction::Arm), 2100)
        .unwrap();
    assert!(!session.manual_command(owner, output(2100, Command::Jog(CW)), 2100));
    assert!(!session.manual_command(owner, CommandEnvelope::INHIBIT, 2100));
    assert_eq!(session.take_command(2100), Some(CommandEnvelope::INHIBIT));
}

#[test]
fn manual_owner_expiry_and_source_deadlines_still_stop_manual_output() {
    let mut session = TrackingSession::<()>::new();
    let owner = session.claim(1, 0).unwrap().token;
    session.take_command(0);
    assert!(session.manual_command(owner, output(4900, Command::Jog(CW)), 4900));
    assert_eq!(session.take_command(5000), Some(CommandEnvelope::INHIBIT));
    assert!(!session.manual_command(owner, output(5000, Command::Jog(CW)), 5000));
    let owner = session.claim(2, 5000).unwrap().token;
    session.take_command(5000);
    assert!(session.manual_command(owner, output(5000, Command::Jog(CW)), 5000));
    assert_eq!(session.take_command(5750), Some(CommandEnvelope::INHIBIT));
    assert!(!session.manual_command(
        owner,
        CommandEnvelope {
            command: Command::Jog(CW),
            valid_until_ms: 8000
        },
        6000
    ));
}

#[test]
fn expired_failed_or_invalid_plans_cannot_be_used_as_an_older_success() {
    for corrupt in 0..4 {
        let (mut session, _) = armed();
        session
            .refresh(
                &[EVENT],
                PlanRevisions {
                    orbit: 2,
                    ..REVISIONS
                },
                2100,
            )
            .unwrap();
        let ticket = session.planning_ticket().unwrap();
        let mut invalid = report(PreparationTiming::Scheduled);
        let plan = invalid.plan.as_mut().unwrap();
        match corrupt {
            0 => plan.assessment.completed_scenarios = 0,
            1 => plan.assessment.gap_time_ms = 1,
            2 => plan.window.aos_unix_s += 20.0,
            _ => plan.calibration.elevation.angle_max_deg = 30.0,
        }
        session
            .publish_plan(ticket, Ok(invalid), "invalid", AOS, 2200)
            .unwrap();
        assert_eq!(
            session.snapshot().plan,
            PlanStatus::Blocked(PlanProblem::InvalidResult)
        );
        session.tick(PREP).unwrap();
        assert_eq!(session.snapshot().run, None);
    }
    for result in [
        Err(PlanningError::Source),
        Ok(PlanningReport {
            plan: None,
            closest_attempt: None,
            evaluated_candidates: 0,
            geometry_rejections: 24,
            work_steps: 1,
        }),
    ] {
        let (mut session, _) = armed();
        session
            .refresh(
                &[EVENT],
                PlanRevisions {
                    orbit: 2,
                    ..REVISIONS
                },
                2100,
            )
            .unwrap();
        let ticket = session.planning_ticket().unwrap();
        session
            .publish_plan(ticket, result, "failed", AOS, 2200)
            .unwrap();
        session.tick(PREP).unwrap();
        assert!(matches!(session.snapshot().plan, PlanStatus::Blocked(_)));
        assert_eq!(session.snapshot().run, None);
        assert!(session.bound_plan().is_none());
    }
    let (mut session, _) = armed();
    session
        .refresh(
            &[EVENT],
            PlanRevisions {
                orbit: 2,
                ..REVISIONS
            },
            2100,
        )
        .unwrap();
    let ticket = session.planning_ticket().unwrap();
    session
        .publish_plan(
            ticket,
            Ok(report(PreparationTiming::Scheduled)),
            "expires",
            PREP,
            2200,
        )
        .unwrap();
    session.tick(PREP).unwrap();
    assert_eq!(
        session.snapshot().plan,
        PlanStatus::Blocked(PlanProblem::Expired)
    );
    assert_eq!(session.snapshot().run, None);
}

#[test]
fn cancellation_and_safety_interruptions_invalidate_pending_worker_and_executor_results() {
    let (mut session, owner) = armed();
    session
        .refresh(
            &[EVENT],
            PlanRevisions {
                start: 2,
                ..REVISIONS
            },
            2100,
        )
        .unwrap();
    let ticket = session.planning_ticket().unwrap();
    session
        .request(request(&session, owner, 3, TrackingAction::Cancel), 2100)
        .unwrap();
    assert_eq!(
        session.publish_plan(
            ticket,
            Ok(report(PreparationTiming::Scheduled)),
            "cancelled",
            AOS,
            2100
        ),
        Err(LifecycleError::StaleJob)
    );
    let (mut session, _) = armed();
    session.tick(PREP).unwrap();
    let run = session.snapshot().run.unwrap();
    session
        .interrupt(TrackingStop::NetworkUnavailable, PREP)
        .unwrap();
    assert_eq!(session.take_command(PREP), Some(CommandEnvelope::INHIBIT));
    assert!(!session.preparation_ready(run, PREP).unwrap());
    assert!(!session.tracking_command(run, output(PREP, Command::Drive(CW)), PREP));
    session.tick(AOS).unwrap();
    assert_eq!(session.snapshot().phase, TrackingPhase::Stopped);
    assert_eq!(session.tick(AOS - 1), Err(LifecycleError::ClockOrCounter));
    assert_eq!(session.take_command(AOS), Some(CommandEnvelope::INHIBIT));
    assert_eq!(session.claim(3, AOS), Err(LifecycleError::ClockOrCounter));
}

#[test]
fn readiness_must_arrive_before_aos_and_stale_input_publications_cannot_roll_back() {
    let (mut session, _) = armed();
    session.tick(PREP).unwrap();
    let run = session.snapshot().run.unwrap();
    assert!(!session.preparation_ready(run, AOS).unwrap());
    assert_eq!(
        session.snapshot().termination.unwrap().reason,
        TrackingStop::LatePreparation
    );
    let (mut session, _) = armed();
    session
        .refresh(
            &[EVENT],
            PlanRevisions {
                orbit: 2,
                ..REVISIONS
            },
            2100,
        )
        .unwrap();
    let before = session.snapshot();
    assert_eq!(
        session.refresh(&[], REVISIONS, 2200),
        Err(LifecycleError::StaleInputs)
    );
    assert_eq!(session.snapshot(), before);
}

#[test]
fn event_matching_uses_the_original_window_and_rejects_neighbor_orbit_and_duplicates() {
    let shifted = PassEvent {
        aos_unix_s: 1020,
        los_unix_s: 1050,
        ..EVENT
    };
    assert_eq!(reconcile_event(EVENT, &[shifted]), Ok(shifted));
    assert_eq!(
        reconcile_event(EVENT, &[shifted, shifted]),
        Err(EventError::Ambiguous)
    );
    assert_eq!(
        reconcile_event(
            EVENT,
            &[PassEvent {
                norad_id: 12345,
                ..EVENT
            }]
        ),
        Err(EventError::Missing)
    );
    assert_eq!(
        reconcile_event(
            EVENT,
            &[PassEvent {
                aos_unix_s: 7000,
                los_unix_s: 7030,
                ..EVENT
            }]
        ),
        Err(EventError::Missing)
    );
    let broad = PassEvent {
        los_unix_s: 1600,
        ..EVENT
    };
    let drifted = PassEvent {
        aos_unix_s: 1250,
        los_unix_s: 1850,
        ..EVENT
    };
    assert_eq!(reconcile_event(broad, &[drifted]), Ok(drifted));
    let another = PassEvent {
        aos_unix_s: 1400,
        los_unix_s: 2000,
        ..EVENT
    };
    assert_eq!(reconcile_event(drifted, &[another]), Ok(another));
    assert_eq!(reconcile_event(broad, &[another]), Err(EventError::Missing));
}

#[test]
fn cancellation_reaches_real_actuator_before_a_pending_activation_at_the_same_deadline() {
    let (mut session, _) = armed();
    session.tick(PREP).unwrap();
    let run = session.snapshot().run.unwrap();
    let plan = session.bound_plan().unwrap().plan();
    let calibration = plan.calibration;
    let installed = InstalledCalibration::new(calibration, FEEDBACK_FULL_SCALE).unwrap();
    let mut actuator = Actuator::with_config(plan.actuator, DutyHistory::Known([0, 0]));
    let service = |session: &mut TrackingSession<&'static str>, actuator: &mut Actuator, now| {
        let mut interlock = FeedbackInterlock::new(installed);
        interlock.publish(Some(
            FeedbackSample::from_voltages(
                installed,
                calibration
                    .position_to_feedback(Pointing::new(180.0, 45.0))
                    .unwrap(),
                now,
            )
            .unwrap(),
        ));
        let mut network = NetworkGate::new();
        network.observe(now, true);
        actuator.step(
            ActuatorInput {
                interlock,
                network,
                command: session.take_command(now),
            },
            now,
        )
    };
    assert!(session.tracking_command(run, output(PREP, Command::Drive(CW)), PREP));
    assert_eq!(service(&mut session, &mut actuator, PREP).output, CW);
    assert!(session.tracking_command(run, CommandEnvelope::INHIBIT, PREP + 50));
    assert_eq!(
        service(&mut session, &mut actuator, PREP + 50).output,
        DriveCommand::IDLE
    );
    // Queue an ordinary activation whose source lease covers the remaining hold.
    assert!(session.tracking_command(run, output(PREP + 1800, Command::Drive(CW)), PREP + 1800));
    assert_eq!(
        service(&mut session, &mut actuator, PREP + 1800).output,
        DriveCommand::IDLE
    );
    assert_eq!(actuator.target(), CW);
    let deadline = PREP + 2050;
    let owner = session.claim(2, deadline).unwrap().token;
    session
        .request(
            request(&session, owner, 1, TrackingAction::Cancel),
            deadline,
        )
        .unwrap();
    assert!(!session.tracking_command(run, output(deadline, Command::Drive(CW)), deadline));
    let stopped = service(&mut session, &mut actuator, deadline);
    assert_eq!(stopped.output, DriveCommand::IDLE);
    assert_eq!(actuator.target(), DriveCommand::IDLE);
    assert_eq!(stopped.status.duty.azimuth.remaining_ms, 299_950);
    assert_eq!(stopped.status.motion_fault, None);
    assert_eq!(
        service(&mut session, &mut actuator, deadline + 1).output,
        DriveCommand::IDLE
    );
}
