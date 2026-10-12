//! Partial support must be earned by a bounded intercept and a usable suffix.
use super::*;
use ezal_core::tracking::*;

fn fixed(_: f64) -> Option<LookAngle> {
    Some(LookAngle {
        azimuth_deg: 180.0,
        elevation_deg: 45.0,
    })
}

fn late_plan(aos_at_ms: u64, used: [u64; 2], duration_ms: u32) -> PlanningReport {
    run(Planner::new(
        fixed,
        PlanningWindow {
            aos_at_ms,
            ..window(duration_ms)
        },
        CAL,
        snapshot(CAL, Pointing::new(180.0, 40.0), used),
        PlanningConfig::default(),
    )
    .unwrap())
}

#[test]
fn late_acquisition_before_and_after_aos_preserves_original_pass_and_validates_all_cases() {
    for aos in [1000, 3000] {
        let report = late_plan(aos, [1000, 2000], 90_000);
        let plan = report.plan.as_ref().unwrap_or_else(|| panic!("{report:?}"));
        assert_eq!(plan.window.aos_at_ms, aos);
        assert_eq!(plan.window.duration_ms, 90_000);
        assert_eq!(plan.route.duration_ms(), 90_000);
        assert_eq!(plan.assessment.completed_scenarios, 9);
        let gap = plan.reception().gap().expect("explicit partial support");
        assert!(gap.late_acquisition);
        assert_eq!(gap.missed.start_ms, 0);
        assert_eq!(gap.recovery_at_ms, 2000 + PARTIAL_ADMISSION_LEAD_MS);
        assert_eq!(gap.deadline_ms, gap.recovery_at_ms + REACQUISITION_LIMIT_MS);
        assert!(gap.latest_join_at_ms >= gap.earliest_join_at_ms);
        assert!(aos + u64::from(gap.missed.end_ms) < gap.deadline_ms);
        assert_eq!(
            plan.reception().windows(90_000),
            [
                None,
                Some(ReceptionWindow {
                    start_ms: gap.missed.end_ms,
                    end_ms: 90_000
                })
            ]
        );
        assert_eq!(
            plan.reception().received_ms(90_000),
            90_000 - gap.missed.end_ms
        );
        assert_eq!(plan.assessment.gap_time_ms, u64::from(gap.missed.end_ms));
        assert_eq!(plan.assessment.failure, None);
    }
}

fn keyhole(t: f64) -> Option<LookAngle> {
    Some(LookAngle {
        azimuth_deg: 120.0 + (t - 20.0).clamp(0.0, 6.0) * 10.0,
        elevation_deg: 80.0,
    })
}

fn gap_report(source: impl Ephemeris, admission: AdmissionPolicy) -> PlanningReport {
    let mut cal = CAL;
    cal.elevation.angle_max_deg = 90.0;
    run(Planner::new(
        source,
        window(90_000),
        cal,
        snapshot(cal, Pointing::new(120.0, 80.0), [1000; 2]),
        PlanningConfig {
            admission,
            ..nominal()
        },
    )
    .unwrap())
}

#[test]
fn predicted_keyhole_gap_requires_a_new_recovery_trajectory_with_supported_prefix_and_suffix() {
    let diagnostic = gap_report(keyhole, AdmissionPolicy::CompleteOnly);
    assert!(diagnostic.plan.is_none());
    assert!(diagnostic.closest_attempt.unwrap().gap_time_ms > 0);
    let report = gap_report(keyhole, AdmissionPolicy::AllowPartial);
    let plan = report.plan.as_ref().unwrap_or_else(|| panic!("{report:?}"));
    let gap = plan
        .reception()
        .gap()
        .expect("gap cannot be relabeled complete");
    assert!(!gap.late_acquisition);
    assert!(gap.missed.start_ms >= 20_000);
    assert!(gap.missed.end_ms - gap.missed.start_ms < 30_000);
    assert!(gap.missed.end_ms < 90_000);
    assert_eq!(
        plan.assessment.completed_scenarios,
        plan.config.scenarios.len()
    );
    assert_eq!(
        plan.assessment.gaps.as_slice(),
        &[PlannedGap {
            start_ms: gap.missed.start_ms,
            end_ms: gap.missed.end_ms
        }]
    );
    let windows = plan.reception().windows(90_000);
    assert_eq!(windows[0].unwrap().end_ms, gap.missed.start_ms);
    assert_eq!(windows[1].unwrap().start_ms, gap.missed.end_ms);
}

#[test]
fn expired_short_exhausted_and_separated_gap_passes_remain_unsupported() {
    assert!(late_plan(1000, [0; 2], 8000).plan.is_none());
    assert!(late_plan(1000, [300_000; 2], 90_000).plan.is_none());
    let report = gap_report(
        |t| {
            let mut look = keyhole(t)?;
            look.azimuth_deg += (t - 65.0).clamp(0.0, 6.0) * 10.0;
            Some(look)
        },
        AdmissionPolicy::AllowPartial,
    );
    assert!(
        report.plan.is_none(),
        "two separated recoveries are outside this finite search"
    );
    assert!(matches!(
        Planner::new(
            fixed,
            PlanningWindow {
                aos_at_ms: 1000,
                ..window(1000)
            },
            CAL,
            snapshot(CAL, Pointing::new(180.0, 40.0), [0; 2]),
            PlanningConfig::default()
        ),
        Err(PlanningError::PassEnded)
    ));
    assert!(matches!(
        Planner::new(
            fixed,
            PlanningWindow {
                aos_at_ms: 1000,
                ..window(90_000)
            },
            CAL,
            snapshot(CAL, Pointing::new(180.0, 40.0), [0; 2]),
            nominal()
        ),
        Err(PlanningError::PassAlreadyStarted)
    ));
}

fn publish(
    report: PlanningReport,
    now: u64,
) -> (
    TrackingLifecycle<&'static str>,
    ezal_core::operator::OperatorToken,
) {
    let window = report.plan.as_ref().unwrap().window;
    let event = PassEvent {
        norad_id: 42,
        aos_unix_s: window.aos_unix_s as i64,
        los_unix_s: window.aos_unix_s as i64 + i64::from(window.duration_ms / 1000),
    };
    let mut session = TrackingLifecycle::new();
    let owner = session.claim(1, now).unwrap().token;
    act(&mut session, owner, 1, TrackingAction::Select(event), now).unwrap();
    session
        .refresh(
            &[event],
            PlanRevisions {
                orbit: 1,
                ..PlanRevisions::default()
            },
            now,
        )
        .unwrap();
    session
        .publish_plan(
            session.planning_ticket().unwrap(),
            Ok(report),
            "frozen source",
            u64::MAX,
            now,
        )
        .unwrap();
    (session, owner)
}

fn act(
    session: &mut TrackingLifecycle<&'static str>,
    owner: ezal_core::operator::OperatorToken,
    sequence: u64,
    action: TrackingAction,
    at: u64,
) -> Result<u64, LifecycleError> {
    session.request(
        TrackingRequest {
            operator: owner,
            sequence,
            expected_revision: session.snapshot().revision,
            action,
        },
        at,
    )
}

#[test]
fn late_preview_requires_explicit_arming_and_freezes_at_its_fixed_entry() {
    let report = late_plan(1000, [0; 2], 90_000);
    let gap = report.plan.as_ref().unwrap().reception().gap().unwrap();
    let (mut idle, _) = publish(report.clone(), 2000);
    assert_eq!(idle.snapshot().plan, PlanStatus::SupportedPartial);
    idle.tick(gap.recovery_at_ms).unwrap();
    assert_eq!(idle.snapshot().mode, TrackingMode::Manual);
    assert!(idle.snapshot().run.is_none());
    idle.tick(gap.recovery_at_ms + 1).unwrap();
    assert_eq!(
        idle.snapshot().plan,
        PlanStatus::Blocked(PlanProblem::Expired)
    );

    let (mut missed, owner) = publish(report.clone(), 2000);
    act(&mut missed, owner, 2, TrackingAction::Arm, 2000).unwrap();
    missed.tick(gap.recovery_at_ms + 1).unwrap();
    assert_eq!(missed.snapshot().phase, TrackingPhase::Stopped);
    assert!(missed.snapshot().run.is_none());
    assert_eq!(
        missed.snapshot().termination.unwrap().reason,
        TrackingStop::LatePreparation
    );

    let (mut session, owner) = publish(report, 2000);
    let original = session.snapshot().selection.unwrap();
    act(&mut session, owner, 2, TrackingAction::Arm, 2000).unwrap();
    session.tick(gap.recovery_at_ms - 1).unwrap();
    assert_eq!(session.snapshot().phase, TrackingPhase::Armed);
    assert!(session.snapshot().run.is_none());
    session.tick(gap.recovery_at_ms).unwrap();
    let recovery = session.snapshot().recovery.unwrap();
    assert_eq!(session.snapshot().selection, Some(original));
    assert_eq!(session.snapshot().plan, PlanStatus::Frozen);
    assert_eq!(session.snapshot().phase, TrackingPhase::Reacquiring);
    assert_eq!(recovery.cause(), RecoveryCause::LateAcquisition);
    assert_eq!(recovery.started_at_ms(), gap.recovery_at_ms);
    assert_eq!(recovery.deadline_ms(), gap.deadline_ms);
    session.tick(gap.recovery_at_ms + 1000).unwrap();
    assert_eq!(session.snapshot().recovery, Some(recovery));
    session.tick(gap.deadline_ms).unwrap();
    assert_eq!(
        session.snapshot().termination.unwrap().reason,
        TrackingStop::ReacquisitionTimeout
    );
    assert_eq!(
        session.take_command(gap.deadline_ms),
        Some(CommandEnvelope::INHIBIT)
    );
}

#[test]
fn planned_gap_interrupts_queued_following_and_only_the_current_controller_can_cancel() {
    let report = gap_report(keyhole, AdmissionPolicy::AllowPartial);
    let plan = report.plan.as_ref().unwrap();
    let gap = plan.reception().gap().unwrap();
    let prep = plan.assessment.prepare_at_ms;
    let aos = plan.window.aos_at_ms;
    let (mut session, owner) = publish(report, 2000);
    act(&mut session, owner, 2, TrackingAction::Arm, 2000).unwrap();
    session.take_command(2000);
    session.tick(prep).unwrap();
    let run = session.snapshot().run.unwrap();
    assert!(session.preparation_ready(run, prep + 2000).unwrap());
    session.tick(aos).unwrap();
    assert_eq!(session.snapshot().phase, TrackingPhase::Following);
    let drive = CommandEnvelope {
        command: Command::Drive(DriveCommand {
            azimuth: Some(AzimuthDirection::Clockwise),
            elevation: None,
        }),
        valid_until_ms: gap.recovery_at_ms + 700,
    };
    assert!(session.tracking_command(run, drive, gap.recovery_at_ms - 1));
    assert_eq!(
        session.take_command(gap.recovery_at_ms),
        Some(CommandEnvelope::INHIBIT)
    );
    let episode = session.snapshot().recovery.unwrap();
    assert_eq!(episode.cause(), RecoveryCause::PlannedGap);
    assert_eq!(episode.started_at_ms(), gap.recovery_at_ms);
    assert_eq!(episode.deadline_ms(), gap.deadline_ms);
    assert!(!session.tracking_command(run, drive, gap.recovery_at_ms));
    assert!(act(
        &mut session,
        owner,
        3,
        TrackingAction::Cancel,
        gap.recovery_at_ms
    )
    .is_err());
    let controller = session.claim(2, gap.recovery_at_ms).unwrap().token;
    act(
        &mut session,
        controller,
        1,
        TrackingAction::Cancel,
        gap.recovery_at_ms,
    )
    .unwrap();
    assert_eq!(
        session.snapshot().termination.unwrap().reason,
        TrackingStop::Cancelled
    );
    assert_eq!(
        session.take_command(gap.recovery_at_ms),
        Some(CommandEnvelope::INHIBIT)
    );
}

#[test]
fn late_publication_and_inconsistent_partial_claims_are_rejected() {
    let report = late_plan(1000, [0; 2], 90_000);
    let gap = report.plan.as_ref().unwrap().reception().gap().unwrap();
    let (expired, _) = publish(report.clone(), gap.recovery_at_ms + 1);
    assert_eq!(
        expired.snapshot().plan,
        PlanStatus::Blocked(PlanProblem::Expired)
    );
    for mutate in [
        |plan: &mut TrackingPlan| plan.assessment.gaps.clear(),
        |plan: &mut TrackingPlan| plan.assessment.gap_time_ms = 0,
        |plan: &mut TrackingPlan| plan.assessment.completed_scenarios -= 1,
        |plan: &mut TrackingPlan| plan.config.admission = AdmissionPolicy::CompleteOnly,
        |plan: &mut TrackingPlan| plan.assessment.prepare_at_ms += 1,
    ] {
        let mut invalid = report.clone();
        mutate(invalid.plan.as_mut().unwrap());
        let (session, _) = publish(invalid, 2000);
        assert_eq!(
            session.snapshot().plan,
            PlanStatus::Blocked(PlanProblem::InvalidResult)
        );
    }
}

#[test]
fn partial_work_yields_and_cancellation_or_later_source_failure_discards_it() {
    for cancel in [true, false] {
        let calls = Cell::new(0);
        let fail = Cell::new(false);
        let source = |t| {
            calls.set(calls.get() + 1);
            if fail.get() {
                None
            } else {
                fixed(t)
            }
        };
        let mut planner = Planner::new(
            source,
            PlanningWindow {
                aos_at_ms: 1000,
                ..window(90_000)
            },
            CAL,
            snapshot(CAL, Pointing::new(180.0, 40.0), [0; 2]),
            PlanningConfig::default(),
        )
        .unwrap();
        for _ in 0..2000 {
            let before = calls.get();
            planner.step().unwrap();
            assert!(calls.get() - before <= 1);
            assert!(!planner.is_finished());
        }
        if cancel {
            planner.cancel();
            assert_eq!(planner.into_report(), Err(PlanningError::Cancelled));
        } else {
            fail.set(true);
            let error = loop {
                if let Err(error) = planner.step() {
                    break error;
                }
            };
            assert_eq!(error, PlanningError::Source);
            assert_eq!(planner.into_report(), Err(error));
        }
    }
}

#[test]
fn a_planned_gap_cannot_extend_an_earlier_recovery_episode() {
    let report = gap_report(keyhole, AdmissionPolicy::AllowPartial);
    let plan = report.plan.as_ref().unwrap();
    let gap = plan.reception().gap().unwrap();
    let prep = plan.assessment.prepare_at_ms;
    let aos = plan.window.aos_at_ms;
    let (mut session, owner) = publish(report, 2000);
    act(&mut session, owner, 2, TrackingAction::Arm, 2000).unwrap();
    session.tick(prep).unwrap();
    // Deliberately omit the executor's measured preparation-ready callback.
    session.tick(aos).unwrap();
    let earlier = session.snapshot().recovery.unwrap();
    assert_eq!(earlier.cause(), RecoveryCause::LatePreparation);
    assert!(gap.recovery_at_ms < earlier.deadline_ms());
    session.tick(gap.recovery_at_ms).unwrap();
    assert_eq!(session.snapshot().recovery, Some(earlier));
    session.tick(earlier.deadline_ms()).unwrap();
    assert_eq!(
        session.snapshot().termination.unwrap().reason,
        TrackingStop::ReacquisitionTimeout
    );
}

#[test]
fn partial_admission_does_not_bypass_noisy_scenario_motion_faults() {
    let mut cal = CAL;
    cal.elevation.angle_max_deg = 90.0;
    let report = run(Planner::new(
        keyhole,
        window(90_000),
        cal,
        snapshot(cal, Pointing::new(120.0, 80.0), [1000; 2]),
        PlanningConfig::default(),
    )
    .unwrap());
    assert!(report.plan.is_none());
    assert!(matches!(
        report.closest_attempt.unwrap().failure,
        Some(CandidateFailure::Simulation(SimulationError::Motion(_)))
    ));
}
