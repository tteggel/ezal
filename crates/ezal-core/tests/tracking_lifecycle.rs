mod common;

use common::{resting_start, FULL_TRAVEL_CALIBRATION};
use ezal_core::actuator::{Actuator, ActuatorInput};
use ezal_core::ads1015::FEEDBACK_FULL_SCALE;
use ezal_core::drive::{AzimuthDirection, Command, CommandEnvelope, DriveCommand};
use ezal_core::duty::DutyHistory;
use ezal_core::feedback::FeedbackSample;
use ezal_core::interlock::FeedbackInterlock;
use ezal_core::network::NetworkGate;
use ezal_core::operator::{LeaseError, OperatorToken};
use ezal_core::orbit::LookAngle;
use ezal_core::position::{InstalledCalibration, Pointing};
use ezal_core::tracking::*;
use std::sync::OnceLock;

const EVENT: PassEvent = PassEvent {
    norad_id: 39260,
    aos_unix_s: 1000,
    los_unix_s: 1030,
};
const REVISIONS: PlanRevisions = PlanRevisions {
    orbit: 1,
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
                FULL_TRAVEL_CALIBRATION,
                resting_start(FULL_TRAVEL_CALIBRATION, Pointing::new(180.0, 45.0), [0, 0]),
                config,
            )
            .unwrap();
            while !planner.is_finished() {
                planner.step().unwrap();
            }
            let report = planner.take_report().unwrap();
            assert!(report.plan.is_some());
            report
        })
        .clone()
}

fn request(
    lifecycle: &TrackingLifecycle<&'static str>,
    owner: OperatorToken,
    sequence: u64,
    action: TrackingAction,
) -> TrackingRequest {
    TrackingRequest {
        operator: owner,
        sequence,
        expected_revision: lifecycle.snapshot().revision,
        action,
    }
}

fn selected() -> (TrackingLifecycle<&'static str>, OperatorToken) {
    let mut lifecycle = TrackingLifecycle::new();
    let token = lifecycle.claim(1, 2000).unwrap().token;
    lifecycle
        .request(
            request(&lifecycle, token, 1, TrackingAction::Select(EVENT)),
            2000,
        )
        .unwrap();
    lifecycle.refresh(&[EVENT], REVISIONS, 2000).unwrap();
    let ticket = lifecycle.planning_ticket().unwrap();
    lifecycle
        .publish_plan(
            ticket,
            Ok(report(PreparationTiming::Scheduled)),
            "original inputs",
            AOS,
            2000,
        )
        .unwrap();
    assert_eq!(lifecycle.take_command(2000), Some(CommandEnvelope::INHIBIT));
    (lifecycle, token)
}

fn armed() -> (TrackingLifecycle<&'static str>, OperatorToken) {
    let (mut lifecycle, token) = selected();
    lifecycle
        .request(request(&lifecycle, token, 2, TrackingAction::Arm), 2000)
        .unwrap();
    assert_eq!(lifecycle.take_command(2000), Some(CommandEnvelope::INHIBIT));
    (lifecycle, token)
}

fn output(now: u64, command: Command) -> CommandEnvelope {
    CommandEnvelope {
        command,
        valid_until_ms: now + 750,
    }
}

#[test]
fn boot_and_selection_are_manual_and_do_not_begin_preparation() {
    let empty = TrackingLifecycle::<()>::new();
    assert_eq!(empty.snapshot().selection, None);
    assert_eq!(empty.snapshot().mode, TrackingMode::Manual);
    assert_eq!(empty.snapshot().run, None);
    let (mut lifecycle, _) = selected();
    lifecycle.tick(PREP).unwrap();
    assert_eq!(lifecycle.snapshot().phase, TrackingPhase::Idle);
    assert_eq!(lifecycle.snapshot().plan, PlanStatus::Supported);
    // Expiry still inhibits the manual source, even though selection never drove it.
    assert_eq!(lifecycle.take_command(PREP), Some(CommandEnvelope::INHIBIT));
    assert_eq!(lifecycle.take_command(PREP), None);
}

#[test]
fn device_progresses_without_browser_and_holds_final_position_without_next_pass() {
    let (mut lifecycle, owner) = armed();
    assert!(lifecycle.release(owner, 2100).unwrap());
    lifecycle.tick(PREP - 1).unwrap();
    assert_eq!(lifecycle.snapshot().phase, TrackingPhase::Armed);
    lifecycle.tick(PREP).unwrap();
    let run = lifecycle.snapshot().run.unwrap();
    assert_eq!(lifecycle.snapshot().phase, TrackingPhase::Preparing);
    assert!(lifecycle.tracking_command(run, output(PREP, Command::Drive(CW)), PREP));
    assert_eq!(
        lifecycle.take_command(PREP),
        Some(output(PREP, Command::Drive(CW)))
    );
    assert!(lifecycle.preparation_ready(run, PREP + 2000).unwrap());
    lifecycle.tick(AOS).unwrap();
    assert_eq!(lifecycle.snapshot().phase, TrackingPhase::Following);
    assert!(lifecycle.tracking_command(run, output(LOS - 1, Command::Drive(CW)), LOS - 1));
    assert_eq!(lifecycle.take_command(LOS), Some(CommandEnvelope::INHIBIT));
    assert_eq!(lifecycle.snapshot().phase, TrackingPhase::Complete);
    assert_eq!(lifecycle.snapshot().mode, TrackingMode::Manual);
    assert_eq!(
        lifecycle.snapshot().termination.unwrap().reason,
        TrackingStop::LossOfSignal
    );
    assert!(!lifecycle.tracking_command(run, output(LOS, Command::Drive(CW)), LOS));
    assert_eq!(lifecycle.take_command(LOS + 10_000), None);
    assert_eq!(lifecycle.snapshot().selection.unwrap().original, EVENT);
}

#[test]
fn operator_expiry_reacquisition_and_old_disconnect_never_interrupt_tracking() {
    let (mut lifecycle, old) = armed();
    assert_eq!(
        lifecycle.claim(2, 2100),
        Err(LifecycleError::Lease(LeaseError::Occupied))
    );
    lifecycle.tick(PREP).unwrap(); // original lease expired long before preparation
    let run = lifecycle.snapshot().run.unwrap();
    assert!(lifecycle.tracking_command(run, output(PREP, Command::Drive(CW)), PREP));
    let new = lifecycle.claim(2, PREP + 100).unwrap().token;
    assert!(!lifecycle.release(old, PREP + 100).unwrap());
    assert_eq!(
        lifecycle.renew(old, PREP + 100),
        Err(LifecycleError::Lease(LeaseError::Invalid))
    );
    assert_eq!(
        lifecycle.take_command(PREP + 100),
        Some(output(PREP, Command::Drive(CW)))
    );
    assert!(lifecycle.release(new, PREP + 100).unwrap());
    assert_eq!(lifecycle.snapshot().run, Some(run));
    assert_eq!(lifecycle.take_command(PREP + 101), None);
}

#[test]
fn only_current_lease_can_mutate_and_stale_generations_cannot_cancel() {
    let (mut lifecycle, old) = armed();
    let new = lifecycle.claim(1, 7000).unwrap().token;
    let before = lifecycle.snapshot();
    for action in [
        TrackingAction::Select(EVENT),
        TrackingAction::Arm,
        TrackingAction::PrepareNow,
        TrackingAction::Cancel,
        TrackingAction::Manual,
    ] {
        let stale = request(&lifecycle, old, 100, action);
        assert_eq!(
            lifecycle.request(stale, 7000),
            Err(LifecycleError::Lease(LeaseError::Invalid))
        );
        assert_eq!(lifecycle.snapshot(), before);
    }
    let mut cancel = request(&lifecycle, new, 1, TrackingAction::Cancel);
    cancel.expected_revision = 0; // cancel survives arbitrary automatic plan revisions
    lifecycle.request(cancel, 7000).unwrap();
    assert_eq!(lifecycle.snapshot().phase, TrackingPhase::Cancelled);
    assert_eq!(lifecycle.take_command(7000), Some(CommandEnvelope::INHIBIT));
}

#[test]
fn exact_retries_are_idempotent_and_old_requests_cannot_revive_or_cancel_a_replacement() {
    let (mut lifecycle, owner) = selected();
    let arm = request(&lifecycle, owner, 2, TrackingAction::Arm);
    let receipt = lifecycle.request(arm, 2000).unwrap();
    lifecycle
        .refresh(
            &[EVENT],
            PlanRevisions {
                start: 2,
                ..REVISIONS
            },
            2100,
        )
        .unwrap();
    let snapshot = lifecycle.snapshot();
    assert_eq!(lifecycle.request(arm, 2100), Ok(receipt));
    assert_eq!(lifecycle.snapshot(), snapshot);
    let cancel = request(&lifecycle, owner, 3, TrackingAction::Cancel);
    let receipt = lifecycle.request(cancel, 2100).unwrap();
    assert_eq!(lifecycle.request(cancel, 2100), Ok(receipt));
    let replace = request(
        &lifecycle,
        owner,
        4,
        TrackingAction::Select(PassEvent {
            norad_id: 12345,
            ..EVENT
        }),
    );
    lifecycle.request(replace, 2100).unwrap();
    let snapshot = lifecycle.snapshot();
    assert_eq!(
        lifecycle.request(cancel, 2100),
        Err(LifecycleError::RequestOrder)
    );
    assert_eq!(
        lifecycle.request(arm, 2100),
        Err(LifecycleError::RequestOrder)
    );
    assert_eq!(lifecycle.snapshot(), snapshot);
    let reused = TrackingRequest {
        action: TrackingAction::Cancel,
        ..replace
    };
    assert_eq!(
        lifecycle.request(reused, 2100),
        Err(LifecycleError::RequestOrder)
    );
}

#[test]
fn auto_refresh_preserves_identity_and_discards_old_or_ambiguous_work() {
    let (mut lifecycle, owner) = armed();
    let event = lifecycle.snapshot().selection.unwrap().id;
    let shifted = PassEvent {
        aos_unix_s: 1010,
        los_unix_s: 1040,
        ..EVENT
    };
    lifecycle
        .refresh(
            &[shifted],
            PlanRevisions {
                orbit: 2,
                ..REVISIONS
            },
            2100,
        )
        .unwrap();
    let old = lifecycle.planning_ticket().unwrap();
    lifecycle
        .refresh(
            &[shifted],
            PlanRevisions {
                orbit: 3,
                ..REVISIONS
            },
            2200,
        )
        .unwrap();
    let latest = lifecycle.planning_ticket().unwrap();
    assert_ne!(old, latest);
    assert_eq!(lifecycle.snapshot().selection.unwrap().id, event);
    assert_eq!(
        lifecycle.publish_plan(
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
    lifecycle
        .publish_plan(latest, Ok(result), "new", AOS, 2200)
        .unwrap();
    assert_eq!(*lifecycle.bound_plan().unwrap().inputs(), "new");
    let stale = request(&lifecycle, owner, 3, TrackingAction::PrepareNow);
    lifecycle
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
        lifecycle.snapshot().plan,
        PlanStatus::Blocked(PlanProblem::Event(EventError::Ambiguous))
    );
    assert_eq!(
        lifecycle.request(stale, 2300),
        Err(LifecycleError::StateRevision)
    );
    lifecycle.tick(PREP).unwrap();
    assert_eq!(lifecycle.snapshot().run, None);
    assert_eq!(lifecycle.snapshot().phase, TrackingPhase::Armed);
    lifecycle.tick(AOS).unwrap();
    assert_eq!(
        lifecycle.snapshot().termination.unwrap().reason,
        TrackingStop::LatePreparation
    );
}

#[test]
fn freeze_is_atomic_and_newer_revisions_cannot_replace_it() {
    let (mut lifecycle, _) = armed();
    lifecycle.tick(PREP).unwrap();
    let run = lifecycle.snapshot().run.unwrap();
    let frozen = lifecycle.bound_plan().unwrap().ticket();
    lifecycle
        .refresh(
            &[],
            PlanRevisions {
                orbit: 20,
                clock: 20,
                start: 20,
            },
            PREP,
        )
        .unwrap();
    assert_eq!(lifecycle.snapshot().run, Some(run));
    assert_eq!(lifecycle.bound_plan().unwrap().ticket(), frozen);
    assert_eq!(*lifecycle.bound_plan().unwrap().inputs(), "original inputs");
    assert_eq!(
        lifecycle.publish_plan(
            frozen,
            Ok(report(PreparationTiming::Scheduled)),
            "late",
            AOS,
            PREP
        ),
        Err(LifecycleError::StaleJob)
    );
}

#[test]
fn update_or_cancel_at_preparation_boundary_prevents_stale_activation() {
    for cancel in [false, true] {
        let (mut lifecycle, _) = armed();
        let owner = lifecycle.claim(2, PREP).unwrap().token;
        if cancel {
            lifecycle
                .request(request(&lifecycle, owner, 1, TrackingAction::Cancel), PREP)
                .unwrap();
        } else {
            lifecycle
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
        lifecycle.tick(PREP).unwrap();
        assert_eq!(lifecycle.snapshot().run, None);
        if cancel {
            assert_eq!(lifecycle.take_command(PREP), Some(CommandEnvelope::INHIBIT));
        } else {
            assert_eq!(lifecycle.take_command(PREP), None);
        }
    }
    let (mut lifecycle, _) = armed();
    lifecycle.tick(PREP).unwrap();
    let old = lifecycle.snapshot().run.unwrap();
    let owner = lifecycle.claim(2, PREP).unwrap().token;
    assert!(lifecycle.tracking_command(old, output(PREP, Command::Drive(CW)), PREP));
    lifecycle
        .request(
            request(&lifecycle, owner, 1, TrackingAction::Select(EVENT)),
            PREP,
        )
        .unwrap();
    assert!(!lifecycle.preparation_ready(old, PREP).unwrap());
    assert!(!lifecycle.tracking_command(old, output(PREP, Command::Drive(CW)), PREP));
    assert_eq!(lifecycle.take_command(PREP), Some(CommandEnvelope::INHIBIT));
    assert_eq!(lifecycle.snapshot().mode, TrackingMode::Manual);
}

#[test]
fn prepare_now_needs_a_new_plan_that_explicitly_evaluated_early_preparation() {
    let (mut lifecycle, owner) = armed();
    lifecycle
        .request(
            request(&lifecycle, owner, 3, TrackingAction::PrepareNow),
            2100,
        )
        .unwrap();
    let ticket = lifecycle.planning_ticket().unwrap();
    assert_eq!(ticket.preparation_timing(), PreparationTiming::Immediate);
    lifecycle.tick(2100).unwrap();
    assert_eq!(lifecycle.snapshot().run, None);
    lifecycle
        .publish_plan(
            ticket,
            Ok(report(PreparationTiming::Scheduled)),
            "wrong schedule",
            AOS,
            2100,
        )
        .unwrap();
    assert_eq!(
        lifecycle.snapshot().plan,
        PlanStatus::Blocked(PlanProblem::InvalidResult)
    );
    lifecycle
        .refresh(
            &[EVENT],
            PlanRevisions {
                start: 2,
                ..REVISIONS
            },
            2200,
        )
        .unwrap();
    let ticket = lifecycle.planning_ticket().unwrap();
    lifecycle
        .publish_plan(
            ticket,
            Ok(report(PreparationTiming::Immediate)),
            "early plan",
            AOS,
            2200,
        )
        .unwrap();
    lifecycle.tick(2200).unwrap();
    assert_eq!(lifecycle.snapshot().phase, TrackingPhase::Preparing);
    assert_eq!(*lifecycle.bound_plan().unwrap().inputs(), "early plan");
}

#[test]
fn manual_admission_cannot_cross_tracking_mode_or_reuse_a_moving_starting_pose() {
    let (mut lifecycle, owner) = selected();
    assert!(lifecycle.manual_command(owner, output(2100, Command::Jog(CW)), 2100));
    assert_eq!(lifecycle.snapshot().plan, PlanStatus::Unplanned);
    assert_eq!(
        lifecycle.request(request(&lifecycle, owner, 2, TrackingAction::Arm), 2100),
        Err(LifecycleError::NotReady)
    );
    assert_eq!(
        lifecycle.take_command(2100),
        Some(output(2100, Command::Jog(CW)))
    );
    lifecycle
        .refresh(
            &[EVENT],
            PlanRevisions {
                start: 2,
                ..REVISIONS
            },
            2100,
        )
        .unwrap();
    let ticket = lifecycle.planning_ticket().unwrap();
    lifecycle
        .publish_plan(
            ticket,
            Ok(report(PreparationTiming::Scheduled)),
            "new pose",
            AOS,
            2100,
        )
        .unwrap();
    lifecycle
        .request(request(&lifecycle, owner, 3, TrackingAction::Arm), 2100)
        .unwrap();
    assert!(!lifecycle.manual_command(owner, output(2100, Command::Jog(CW)), 2100));
    assert!(!lifecycle.manual_command(owner, CommandEnvelope::INHIBIT, 2100));
    assert_eq!(lifecycle.take_command(2100), Some(CommandEnvelope::INHIBIT));
}

#[test]
fn manual_owner_expiry_and_source_deadlines_still_stop_manual_output() {
    let mut lifecycle = TrackingLifecycle::<()>::new();
    let owner = lifecycle.claim(1, 0).unwrap().token;
    lifecycle.take_command(0);
    assert!(lifecycle.manual_command(owner, output(4900, Command::Jog(CW)), 4900));
    assert_eq!(lifecycle.take_command(5000), Some(CommandEnvelope::INHIBIT));
    assert!(!lifecycle.manual_command(owner, output(5000, Command::Jog(CW)), 5000));
    let owner = lifecycle.claim(2, 5000).unwrap().token;
    lifecycle.take_command(5000);
    assert!(lifecycle.manual_command(owner, output(5000, Command::Jog(CW)), 5000));
    assert_eq!(lifecycle.take_command(5750), Some(CommandEnvelope::INHIBIT));
    assert!(!lifecycle.manual_command(
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
        let (mut lifecycle, _) = armed();
        lifecycle
            .refresh(
                &[EVENT],
                PlanRevisions {
                    orbit: 2,
                    ..REVISIONS
                },
                2100,
            )
            .unwrap();
        let ticket = lifecycle.planning_ticket().unwrap();
        let mut invalid = report(PreparationTiming::Scheduled);
        let plan = invalid.plan.as_mut().unwrap();
        match corrupt {
            0 => plan.assessment.completed_scenarios = 0,
            1 => plan.assessment.gap_time_ms = 1,
            2 => plan.window.aos_unix_s += 20.0,
            _ => plan.calibration.elevation.angle_max_deg = 30.0,
        }
        lifecycle
            .publish_plan(ticket, Ok(invalid), "invalid", AOS, 2200)
            .unwrap();
        assert_eq!(
            lifecycle.snapshot().plan,
            PlanStatus::Blocked(PlanProblem::InvalidResult)
        );
        lifecycle.tick(PREP).unwrap();
        assert_eq!(lifecycle.snapshot().run, None);
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
        let (mut lifecycle, _) = armed();
        lifecycle
            .refresh(
                &[EVENT],
                PlanRevisions {
                    orbit: 2,
                    ..REVISIONS
                },
                2100,
            )
            .unwrap();
        let ticket = lifecycle.planning_ticket().unwrap();
        lifecycle
            .publish_plan(ticket, result, "failed", AOS, 2200)
            .unwrap();
        lifecycle.tick(PREP).unwrap();
        assert!(matches!(lifecycle.snapshot().plan, PlanStatus::Blocked(_)));
        assert_eq!(lifecycle.snapshot().run, None);
        assert!(lifecycle.bound_plan().is_none());
    }
    let (mut lifecycle, _) = armed();
    lifecycle
        .refresh(
            &[EVENT],
            PlanRevisions {
                orbit: 2,
                ..REVISIONS
            },
            2100,
        )
        .unwrap();
    let ticket = lifecycle.planning_ticket().unwrap();
    lifecycle
        .publish_plan(
            ticket,
            Ok(report(PreparationTiming::Scheduled)),
            "expires",
            PREP,
            2200,
        )
        .unwrap();
    lifecycle.tick(PREP).unwrap();
    assert_eq!(
        lifecycle.snapshot().plan,
        PlanStatus::Blocked(PlanProblem::Expired)
    );
    assert_eq!(lifecycle.snapshot().run, None);
}

#[test]
fn cancellation_and_safety_interruptions_invalidate_pending_worker_and_executor_results() {
    let (mut lifecycle, owner) = armed();
    lifecycle
        .refresh(
            &[EVENT],
            PlanRevisions {
                start: 2,
                ..REVISIONS
            },
            2100,
        )
        .unwrap();
    let ticket = lifecycle.planning_ticket().unwrap();
    lifecycle
        .request(request(&lifecycle, owner, 3, TrackingAction::Cancel), 2100)
        .unwrap();
    assert_eq!(
        lifecycle.publish_plan(
            ticket,
            Ok(report(PreparationTiming::Scheduled)),
            "cancelled",
            AOS,
            2100
        ),
        Err(LifecycleError::StaleJob)
    );
    let (mut lifecycle, _) = armed();
    lifecycle.tick(PREP).unwrap();
    let run = lifecycle.snapshot().run.unwrap();
    lifecycle
        .interrupt(TrackingStop::NetworkUnavailable, PREP)
        .unwrap();
    assert_eq!(lifecycle.take_command(PREP), Some(CommandEnvelope::INHIBIT));
    assert!(!lifecycle.preparation_ready(run, PREP).unwrap());
    assert!(!lifecycle.tracking_command(run, output(PREP, Command::Drive(CW)), PREP));
    lifecycle.tick(AOS).unwrap();
    assert_eq!(lifecycle.snapshot().phase, TrackingPhase::Stopped);
    assert_eq!(lifecycle.tick(AOS - 1), Err(LifecycleError::ClockOrCounter));
    assert_eq!(lifecycle.take_command(AOS), Some(CommandEnvelope::INHIBIT));
    assert_eq!(lifecycle.claim(3, AOS), Err(LifecycleError::ClockOrCounter));
}

#[test]
fn late_readiness_enters_bounded_acquisition_and_stale_inputs_cannot_roll_back() {
    let (mut lifecycle, _) = armed();
    lifecycle.tick(PREP).unwrap();
    let run = lifecycle.snapshot().run.unwrap();
    assert!(!lifecycle.preparation_ready(run, AOS).unwrap());
    assert_eq!(lifecycle.snapshot().phase, TrackingPhase::Reacquiring);
    let episode = lifecycle.snapshot().recovery.unwrap();
    assert_eq!(episode.cause(), RecoveryCause::LatePreparation);
    assert_eq!(episode.started_at_ms(), AOS);
    assert_eq!(episode.deadline_ms(), LOS);
    assert!(!lifecycle.tracking_command(run, output(AOS, Command::Drive(CW)), AOS));
    lifecycle.tick(LOS).unwrap();
    assert_eq!(
        lifecycle.snapshot().termination.unwrap().reason,
        TrackingStop::ReacquisitionLossOfSignal
    );
    let (mut lifecycle, _) = armed();
    lifecycle
        .refresh(
            &[EVENT],
            PlanRevisions {
                orbit: 2,
                ..REVISIONS
            },
            2100,
        )
        .unwrap();
    let before = lifecycle.snapshot();
    assert_eq!(
        lifecycle.refresh(&[], REVISIONS, 2200),
        Err(LifecycleError::StaleInputs)
    );
    assert_eq!(lifecycle.snapshot(), before);
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
    let (mut lifecycle, _) = armed();
    lifecycle.tick(PREP).unwrap();
    let run = lifecycle.snapshot().run.unwrap();
    let plan = lifecycle.bound_plan().unwrap().plan();
    let calibration = plan.calibration;
    let installed = InstalledCalibration::new(calibration, FEEDBACK_FULL_SCALE).unwrap();
    let mut actuator = Actuator::with_config(plan.actuator, DutyHistory::Known([0, 0]));
    let service =
        |lifecycle: &mut TrackingLifecycle<&'static str>, actuator: &mut Actuator, now| {
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
                    command: lifecycle.take_command(now),
                },
                now,
            )
        };
    assert!(lifecycle.tracking_command(run, output(PREP, Command::Drive(CW)), PREP));
    assert_eq!(service(&mut lifecycle, &mut actuator, PREP).output, CW);
    assert!(lifecycle.tracking_command(run, CommandEnvelope::INHIBIT, PREP + 50));
    assert_eq!(
        service(&mut lifecycle, &mut actuator, PREP + 50).output,
        DriveCommand::IDLE
    );
    // Queue an ordinary activation whose source lease covers the remaining hold.
    assert!(lifecycle.tracking_command(run, output(PREP + 1800, Command::Drive(CW)), PREP + 1800));
    assert_eq!(
        service(&mut lifecycle, &mut actuator, PREP + 1800).output,
        DriveCommand::IDLE
    );
    assert_eq!(actuator.target(), CW);
    let deadline = PREP + 2050;
    let owner = lifecycle.claim(2, deadline).unwrap().token;
    lifecycle
        .request(
            request(&lifecycle, owner, 1, TrackingAction::Cancel),
            deadline,
        )
        .unwrap();
    assert!(!lifecycle.tracking_command(run, output(deadline, Command::Drive(CW)), deadline));
    let stopped = service(&mut lifecycle, &mut actuator, deadline);
    assert_eq!(stopped.output, DriveCommand::IDLE);
    assert_eq!(actuator.target(), DriveCommand::IDLE);
    assert_eq!(stopped.status.duty.azimuth.remaining_ms, 299_950);
    assert_eq!(stopped.status.motion_fault, None);
    assert_eq!(
        service(&mut lifecycle, &mut actuator, deadline + 1).output,
        DriveCommand::IDLE
    );
}

#[test]
fn a_new_satellite_can_use_its_older_cached_publication_without_regressing_other_inputs() {
    let (mut lifecycle, token) = selected();
    lifecycle
        .refresh(
            &[EVENT],
            PlanRevisions {
                orbit: 9,
                ..REVISIONS
            },
            2000,
        )
        .unwrap();
    let next = PassEvent {
        norad_id: 43010,
        ..EVENT
    };
    lifecycle
        .request(
            request(&lifecycle, token, 2, TrackingAction::Select(next)),
            2000,
        )
        .unwrap();
    lifecycle.refresh(&[next], REVISIONS, 2000).unwrap();
    assert_eq!(lifecycle.planning_ticket().unwrap().revisions().orbit, 1);
    assert_eq!(
        lifecycle.refresh(
            &[next],
            PlanRevisions {
                clock: 0,
                ..REVISIONS
            },
            2000
        ),
        Err(LifecycleError::StaleInputs)
    );
}
