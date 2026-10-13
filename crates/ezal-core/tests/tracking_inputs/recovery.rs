//! T6 uses the same frozen real-orbit fixture as the T5 input boundary tests.

use super::*;
use std::collections::VecDeque;

/// Live-style feedback/actuator/runtime loop with a separate physical model.
/// Orbit propagation and the recovery search use the production frozen inputs.
struct Rig {
    runtime: TrackingRuntime,
    run: RunToken,
    actuator: Actuator,
    model: MotorModel,
    feedback: FeedbackSnapshot,
    pending: VecDeque<(u64, FeedbackSample)>,
    now: u64,
}

impl Rig {
    /// Freeze the run without establishing readiness, `offset` degrees above
    /// the route's first point, so it enters recovery at AOS as a late preparation.
    fn new(offset: f32, used: [u64; 2]) -> Self {
        let (mut runtime, _, prep) = armed();
        runtime.service(observation(prep)).unwrap();
        let run = runtime.snapshot().run.unwrap();
        let plan = runtime.bound_plan().unwrap().plan();
        let mut pose = plan.route.pointing_at(0).unwrap();
        pose.elevation_deg += offset;
        let mut actuator = Actuator::with_config(plan.actuator, DutyHistory::Known(used));
        let feedback = healthy(full_travel_sample(pose, AOS - 100));
        let mut interlock = FeedbackInterlock::new(full_travel_installed());
        for at in [AOS - 2000, AOS] {
            interlock.publish(Some(full_travel_sample(pose, at)));
            actuator.step(
                ActuatorInput {
                    interlock,
                    network: observation(at).network,
                    command: None,
                },
                at,
            );
        }
        let mut rig = Self {
            runtime,
            run,
            actuator,
            model: MotorModel::new(MotorProfile::AC_50HZ, FULL_TRAVEL_CALIBRATION, pose, used)
                .unwrap(),
            feedback,
            pending: VecDeque::new(),
            now: AOS,
        };
        rig.tick(AOS);
        rig
    }

    fn tick(&mut self, at: u64) {
        self.model
            .advance(self.actuator.actual(), at - self.now)
            .unwrap();
        self.now = at;
        if (at - AOS).is_multiple_of(100) {
            self.pending
                .push_back((at + 100, full_travel_sample(self.model.position(), at)));
        }
        while self.pending.front().is_some_and(|(ready, _)| *ready <= at) {
            self.feedback = healthy(self.pending.pop_front().unwrap().1);
        }
        let mut interlock = FeedbackInterlock::new(full_travel_installed());
        interlock.publish(self.feedback.last_valid);
        let obs = observation(at);
        // A coherent snapshot is serviced before planning; GPIO would be applied
        // only after the command/stop has also passed through this same owner.
        self.actuator.step(
            ActuatorInput {
                interlock,
                network: obs.network,
                command: None,
            },
            at,
        );
        self.runtime
            .execution_command(self.feedback, &self.actuator, obs);
        let command = self.runtime.take_command(obs);
        self.actuator.step(
            ActuatorInput {
                interlock,
                network: obs.network,
                command,
            },
            at,
        );
        self.model.advance(self.actuator.actual(), 0).unwrap();
        assert_eq!(self.actuator.fault(), None);
    }

    fn until(&mut self, at: u64) {
        while self.now < at {
            self.tick((self.now + 20).min(at));
        }
    }

    fn entered(&self) -> RecoveryStatus {
        assert_eq!(self.runtime.snapshot().phase, TrackingPhase::Reacquiring);
        self.runtime.recovery().unwrap()
    }

    fn plan(&mut self) -> RecoveryStatus {
        let mut work = 0;
        while self
            .runtime
            .step_reacquisition(observation(self.now))
            .unwrap()
        {
            work += 1;
            assert!(work < MAX_PLANNING_STEPS);
        }
        self.runtime.recovery().unwrap()
    }
}

#[test]
fn an_admitted_late_real_pass_executes_recovery_and_retains_partial_coverage_at_los() {
    let at = AOS + 1000;
    let (mut runtime, owner) = selected(EVENT, observation(at));
    let inputs = ExecutionInputs::capture(
        runtime.planning_ticket().unwrap(),
        catalog().snapshot(EVENT.norad_id).unwrap(),
        InputSettings::default(),
        clock(UTC),
        execution_window(clock(UTC).anchor().unwrap(), UTC + 120_000, UTC + 150_000).unwrap(),
        &nominal(),
        at,
    )
    .unwrap();
    assert_eq!(inputs.freeze_before_ms().unwrap(), LOS);
    let look = inputs.look_at_uptime(at).unwrap();
    let pose = Pointing::new(look.azimuth_deg as f32, look.elevation_deg as f32 + 5.0);
    let mut actuator = Actuator::with_config(
        ActuatorConfig {
            duty: DutyConfig::CONFIRMED_AC,
            ..ActuatorConfig::DEFAULT
        },
        DutyHistory::Known([0; 2]),
    );
    let mut interlock = FeedbackInterlock::new(full_travel_installed());
    for now in [at - 2000, at] {
        interlock.publish(Some(full_travel_sample(pose, now)));
        actuator.step(
            ActuatorInput {
                interlock,
                network: observation(now).network,
                command: None,
            },
            now,
        );
    }
    let start = RestingStart {
        at_ms: at,
        feedback: full_travel_sample(pose, at),
        actuator,
        network: observation(at).network,
    };
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
    let report = planner.take_report().unwrap();
    let plan = report.plan.as_ref().unwrap_or_else(|| panic!("{report:?}"));
    let coverage = plan.reception();
    let gap = coverage.gap().unwrap();
    assert!(gap.late_acquisition);
    runtime
        .publish_plan(inputs.ticket(), Ok(report), inputs, observation(at))
        .unwrap();
    assert_eq!(runtime.snapshot().plan, PlanStatus::SupportedPartial);
    action(&mut runtime, owner, 2, TrackingAction::Arm, observation(at)).unwrap();
    runtime
        .service(observation(gap.recovery_at_ms - 1))
        .unwrap();
    assert_eq!(runtime.snapshot().phase, TrackingPhase::Armed);
    runtime.service(observation(gap.recovery_at_ms)).unwrap();
    let run = runtime.snapshot().run.unwrap();
    let mut rig = Rig {
        runtime,
        run,
        actuator,
        model: MotorModel::new(MotorProfile::AC_50HZ, FULL_TRAVEL_CALIBRATION, pose, [0; 2])
            .unwrap(),
        feedback: healthy(full_travel_sample(pose, gap.recovery_at_ms)),
        pending: VecDeque::new(),
        now: gap.recovery_at_ms,
    };
    rig.tick(gap.recovery_at_ms);
    assert_eq!(
        rig.runtime.recovery().unwrap().episode.cause(),
        RecoveryCause::LateAcquisition
    );
    let recovery = rig.plan();
    assert!(recovery.intercept_at_ms.is_some(), "{recovery:?}");
    while rig.runtime.snapshot().phase == TrackingPhase::Reacquiring && rig.now < LOS {
        rig.tick(rig.now + 20);
    }
    assert_eq!(
        rig.runtime.snapshot().phase,
        TrackingPhase::Following,
        "{:?}",
        rig.runtime.snapshot()
    );
    assert!(rig.now <= AOS + u64::from(gap.missed.end_ms));
    assert!(rig.runtime.deviation(rig.now).maximum_deg.unwrap() > 4.0);
    assert!(rig.runtime.operator(rig.now).is_none());
    rig.until(LOS);
    assert_eq!(rig.runtime.snapshot().phase, TrackingPhase::Complete);
    let summary = rig.runtime.last_summary().unwrap();
    assert_eq!(summary.reception, coverage);
    assert_eq!(summary.window.aos_at_ms, AOS);
    assert_eq!(summary.window.duration_ms, (LOS - AOS) as u32);
    assert_eq!(summary.termination.reason, TrackingStop::LossOfSignal);
    assert!(!rig.actuator.actual().is_active());
}

#[test]
fn recoverable_lag_executes_a_future_intercept_and_preserves_duty() {
    let mut rig = Rig::new(10.0, [9000, 21000]);
    let episode = rig.entered().episode;
    // Reacquiring never drives the normal follower; the dish holds until launch.
    assert!(!rig.actuator.target().is_active());
    let plan = rig.plan();
    assert!(plan.intercept_at_ms.is_some(), "{plan:?}");
    assert!(plan.intercept_at_ms.unwrap() > plan.launch_at_ms.unwrap());
    assert_eq!(plan.episode, episode);
    while rig.runtime.snapshot().phase == TrackingPhase::Reacquiring && rig.now < LOS {
        rig.tick(rig.now + 20);
    }
    assert_eq!(
        rig.runtime.snapshot().phase,
        TrackingPhase::Following,
        "{:?}, recovery {:?}",
        rig.runtime.snapshot(),
        rig.runtime.recovery()
    );
    let final_error = rig.runtime.deviation(rig.now);
    assert!(final_error.current.unwrap().degrees <= REACQUISITION_TOLERANCE_DEG);
    assert!(final_error.maximum_deg.unwrap() > 9.0);
    assert!(final_error.outside_tolerance_ms > 1000);
    assert_eq!(rig.runtime.recovery(), None);
    assert!(rig.actuator.status().duty.elevation.remaining_ms < 300000 - 21000);
    assert_eq!(rig.actuator.fault(), None);
    assert!(
        rig.runtime.operator(rig.now).is_none(),
        "browser lease is irrelevant"
    );
}

#[test]
fn unfinished_frozen_preparation_acquires_a_future_point_without_chasing_aos() {
    let mut rig = Rig::new(10.0, [0, 0]);
    let episode = rig.entered().episode;
    assert_eq!(episode.cause(), RecoveryCause::LatePreparation);
    assert_eq!(episode.started_at_ms(), AOS);
    let plan = rig.plan();
    assert!(plan.intercept_at_ms.unwrap() > AOS);
    while rig.runtime.snapshot().phase == TrackingPhase::Reacquiring && rig.now < LOS {
        rig.tick(rig.now + 20);
    }
    assert_eq!(rig.runtime.snapshot().phase, TrackingPhase::Following);
    assert!(rig.runtime.deviation(rig.now).outside_tolerance_ms > 1000);
}

#[test]
fn duty_exhaustion_holds_and_retries_without_extending_episode_or_resetting_history() {
    let mut rig = Rig::new(10.0, [300000, 300000]);
    let episode = rig.entered().episode;
    let first = rig.plan();
    assert_eq!(first.hold, Some(RecoveryHold::NoIntercept));
    assert!(!rig.actuator.actual().is_active());
    rig.until(rig.now + INTERCEPT_RETRY_MS);
    let second = rig.plan();
    assert!(second.attempts > first.attempts);
    assert_eq!(second.episode, episode);
    assert_eq!(episode.deadline_ms(), LOS);
    rig.until(LOS);
    assert_eq!(
        rig.runtime.snapshot().termination.unwrap().reason,
        TrackingStop::ReacquisitionLossOfSignal
    );
    assert_eq!(rig.runtime.snapshot().mode, TrackingMode::Manual);
    assert!(!rig.actuator.actual().is_active());
    assert_eq!(rig.actuator.status().duty.elevation.remaining_ms, 0);
    assert!(rig.runtime.deviation(LOS).current.is_none());
    assert!(rig.runtime.deviation(LOS).maximum_deg.unwrap() > 9.0);
}

#[test]
fn late_computation_or_missed_launch_never_chases_an_expired_intercept() {
    for finish_search in [false, true] {
        let mut rig = Rig::new(10.0, [0, 0]);
        let first = rig.entered();
        let launch = if finish_search {
            rig.plan().launch_at_ms.unwrap()
        } else {
            first.episode.started_at_ms() + INTERCEPT_PLANNING_LEAD_MS
        };
        // Service feedback while holding, deliberately omit the exact launch.
        rig.until(launch - 20);
        rig.tick(launch + 1);
        rig.runtime
            .step_reacquisition(observation(rig.now))
            .unwrap();
        let status = rig.runtime.recovery().unwrap();
        assert_eq!(status.hold, Some(RecoveryHold::MissedLaunch));
        assert_eq!(status.episode, first.episode);
        assert!(!rig.actuator.actual().is_active());
    }
}

#[test]
fn failed_and_stale_feedback_stop_recovery_instead_of_reusing_last_position() {
    for health in [
        FeedbackHealth::Unavailable,
        FeedbackHealth::ReadTimedOut,
        FeedbackHealth::Invalid,
    ] {
        let mut rig = Rig::new(10.0, [0, 0]);
        rig.entered();
        rig.plan();
        let mut feedback = rig.feedback;
        feedback.health = health;
        rig.runtime
            .observe_tracking(rig.run, feedback, &rig.actuator, observation(rig.now))
            .unwrap();
        assert_eq!(
            rig.runtime.snapshot().termination.unwrap().reason,
            TrackingStop::Feedback(health)
        );
        assert_eq!(
            rig.runtime.take_command(observation(rig.now)),
            Some(CommandEnvelope::INHIBIT)
        );
        assert!(rig.runtime.deviation(rig.now).current.is_none());
        assert!(!rig
            .runtime
            .step_reacquisition(observation(rig.now))
            .unwrap());
    }
    let mut rig = Rig::new(10.0, [0, 0]);
    rig.entered();
    let expiry = rig.feedback.last_valid.unwrap().sampled_at_ms() + 500;
    assert!(rig.runtime.deviation(expiry).current.is_none());
    assert_eq!(
        rig.runtime.take_command(observation(expiry)),
        Some(CommandEnvelope::INHIBIT)
    );
    assert_eq!(
        rig.runtime.snapshot().termination.unwrap().reason,
        TrackingStop::Feedback(FeedbackHealth::Stale)
    );
}

#[test]
fn cancellation_discards_pending_search_and_retains_final_deviation() {
    let mut rig = Rig::new(10.0, [0, 0]);
    rig.entered();
    // Measure the offset after AOS while the search is still before its launch.
    rig.until(AOS + 1000);
    rig.runtime
        .step_reacquisition(observation(rig.now))
        .unwrap();
    let token = rig.runtime.claim(2, observation(rig.now)).unwrap().token;
    action(
        &mut rig.runtime,
        token,
        1,
        TrackingAction::Cancel,
        observation(rig.now),
    )
    .unwrap();
    assert_eq!(rig.runtime.snapshot().phase, TrackingPhase::Cancelled);
    assert_eq!(rig.runtime.recovery(), None);
    assert!(!rig
        .runtime
        .step_reacquisition(observation(rig.now))
        .unwrap());
    rig.runtime
        .execution_command(rig.feedback, &rig.actuator, observation(rig.now));
    assert_eq!(
        rig.runtime.take_command(observation(rig.now)),
        Some(CommandEnvelope::INHIBIT)
    );
    let final_status = rig.runtime.deviation(rig.now);
    assert!(final_status.maximum_deg.unwrap() > 9.0);
    assert_eq!(rig.runtime.deviation(rig.now + 10000), final_status);
    let summary = rig.runtime.last_summary().unwrap();
    assert_eq!(summary.termination.run, Some(rig.run));
    assert_eq!(summary.termination.reason, TrackingStop::Cancelled);
    assert_eq!(summary.deviation, final_status);
    action(
        &mut rig.runtime,
        token,
        2,
        TrackingAction::Select(EVENT),
        observation(rig.now),
    )
    .unwrap();
    assert_eq!(rig.runtime.last_summary(), Some(summary));
}

#[test]
fn physical_fault_and_network_loss_discard_validated_intercepts() {
    for physical in [false, true] {
        let mut rig = Rig::new(10.0, [0, 0]);
        rig.entered();
        rig.plan();
        let mut obs = observation(rig.now);
        let reason = if physical {
            let mut interlock = FeedbackInterlock::new(full_travel_installed());
            interlock.publish(rig.feedback.last_valid);
            rig.actuator.step(
                ActuatorInput {
                    interlock,
                    network: obs.network,
                    command: None,
                },
                rig.now - 1,
            );
            TrackingStop::Motion(rig.actuator.fault().unwrap())
        } else {
            obs.network.observe(rig.now, false);
            TrackingStop::NetworkUnavailable
        };
        rig.runtime
            .execution_command(rig.feedback, &rig.actuator, obs);
        assert_eq!(rig.runtime.snapshot().termination.unwrap().reason, reason);
        assert_eq!(rig.runtime.recovery(), None);
        assert_eq!(
            rig.runtime.take_command(obs),
            Some(CommandEnvelope::INHIBIT)
        );
        assert!(rig
            .runtime
            .last_summary()
            .unwrap()
            .deviation
            .current
            .is_none());
        if physical {
            assert!(rig.actuator.fault().is_some());
        }
    }
}

#[test]
fn missed_measured_intercept_retries_with_the_original_deadline_and_consumed_duty() {
    let mut rig = Rig::new(10.0, [0, 10000]);
    let episode = rig.entered().episode;
    let planned = rig.plan();
    assert!(planned.intercept_at_ms.is_some());
    // Actual motion is slower than the provisional scenarios but still produces
    // progress. This exercises measured failure, not an artificially cleared fault.
    let slow = MotorProfile {
        elevation: AxisProfile {
            rate_deg_per_s: 1.0,
            ..MotorProfile::AC_50HZ.elevation
        },
        ..MotorProfile::AC_50HZ
    };
    rig.model = MotorModel::new(
        slow,
        FULL_TRAVEL_CALIBRATION,
        rig.model.position(),
        [0, 10000],
    )
    .unwrap();
    rig.until(planned.intercept_at_ms.unwrap() + REACQUISITION_DWELL_MS + 500);
    let missed = rig.runtime.recovery().unwrap();
    assert_eq!(missed.hold, Some(RecoveryHold::MissedIntercept));
    assert_eq!(missed.episode, episode);
    assert!(!rig.actuator.actual().is_active());
    let remaining = rig.actuator.status().duty.elevation.remaining_ms;
    assert!(remaining < 290000);
    rig.until(rig.now + INTERCEPT_RETRY_MS);
    assert_eq!(rig.runtime.recovery().unwrap().episode, episode);
    assert!(rig.runtime.recovery().unwrap().attempts > planned.attempts);
    assert_eq!(rig.actuator.status().duty.elevation.remaining_ms, remaining);
}

#[test]
fn deviation_uses_acquisition_time_and_reports_real_sampling_uncertainty() {
    let (mut runtime, run) = following();
    let bound = runtime.bound_plan().unwrap();
    let at = AOS + 1000;
    let look = bound.inputs().look_at_uptime(at).unwrap();
    let pose = Pointing::new(look.azimuth_deg as f32, look.elevation_deg as f32);
    let actuator = Actuator::with_config(bound.plan().actuator, DutyHistory::Known([0, 0]));
    runtime
        .observe_tracking(
            run,
            healthy(full_travel_sample(pose, at)),
            &actuator,
            observation(at + 400),
        )
        .unwrap();
    let reading = runtime.deviation(at + 400).current.unwrap();
    assert!(reading.degrees < 0.2, "{reading:?}");
    assert_eq!(reading.sampled_at_ms, at);
    assert_eq!(reading.desired, pose);
    assert!(
        reading.uncertainty_deg > 2.0,
        "includes source reserve, one ADC code and 150 ms skew"
    );
    assert!(runtime.deviation(at + 500).current.is_none());
}

#[test]
fn delayed_output_drain_and_missed_control_tick_hold_before_reusing_a_command() {
    for delayed_launch in [true, false] {
        let mut rig = Rig::new(10.0, [0, 0]);
        let episode = rig.entered().episode;
        let launch = rig.plan().launch_at_ms.unwrap();
        if delayed_launch {
            rig.until(launch - 20);
            let mut interlock = FeedbackInterlock::new(full_travel_installed());
            interlock.publish(rig.feedback.last_valid);
            rig.actuator.step(
                ActuatorInput {
                    interlock,
                    network: observation(launch).network,
                    command: None,
                },
                launch,
            );
            rig.runtime
                .execution_command(rig.feedback, &rig.actuator, observation(launch));
            // Launched on time: the drain below is what is late.
            assert_eq!(rig.runtime.recovery().unwrap().hold, None);
        } else {
            rig.until(launch);
        }
        let late = launch + if delayed_launch { 1 } else { 21 };
        assert_eq!(
            rig.runtime.take_command(observation(late)),
            Some(CommandEnvelope::INHIBIT)
        );
        let status = rig.runtime.recovery().unwrap();
        assert_eq!(status.episode, episode);
        assert_eq!(
            status.hold,
            Some(if delayed_launch {
                RecoveryHold::MissedLaunch
            } else {
                RecoveryHold::MissedControlDeadline
            })
        );
        assert_eq!(status.intercept_at_ms, None);
    }
}
