mod common;
use common::*;
use ezal_core::catalog::Catalog;
use ezal_core::coordination::{CoordinationState, PlanningWorker, TrackingSource};
use ezal_core::feedback::{FeedbackHealth, FeedbackSnapshot};
use ezal_core::interlock::FeedbackInterlock;
use ezal_core::ntp::UtcClock;
use ezal_core::protocol::{ClientMessage, PlanningIntent};
use ezal_core::session::CommandFreshness;
use ezal_core::tracking::offload::*;
use ezal_core::tracking::*;

const EVENT: PassEvent = PassEvent {
    norad_id: 39260,
    aos_unix_s: 1_791_385_770,
    los_unix_s: 1_791_385_800,
};
fn source() -> TrackingSource {
    let mut catalog = Catalog::new();
    catalog
        .commit(
            download(
                include_str!("fixtures/weather-2026-10-07.tle"),
                EVENT.aos_unix_s - 120,
            ),
            2000,
        )
        .unwrap();
    (
        catalog.snapshot(EVENT.norad_id).unwrap(),
        heapless::Vec::from_slice(&[EVENT]).unwrap(),
    )
}
fn setup() -> (
    CoordinationState,
    PlanningWorker,
    u32,
    CommandFreshness,
    PlanningIntent,
) {
    let (_, sample) = common::planning::portable("nominal");
    let mut state = CoordinationState::new();
    state.publish_network(true, 2000);
    let mut clock = UtcClock::new();
    clock
        .synchronize(2000, ntp_sample(EVENT.aos_unix_s as u64 * 1000 - 120_000))
        .unwrap();
    state.publish_clock(clock, 2000);
    let mut interlock = FeedbackInterlock::new(full_travel_installed());
    interlock.publish(Some(sample.start.feedback));
    state.publish_feedback(
        FeedbackSnapshot {
            health: FeedbackHealth::Healthy,
            last_valid: Some(sample.start.feedback),
            ..FeedbackSnapshot::INITIAL
        },
        interlock,
        2000,
    );
    state.record_actuator(&sample.start.actuator, 2000);
    let client = state.register_client(2000);
    let grant = state.operator(client, 2000).unwrap();
    state.tracking_operation(2000, |runtime, obs| {
        runtime
            .request(
                TrackingRequest {
                    operator: grant.token,
                    sequence: 1,
                    expected_revision: runtime.snapshot().revision,
                    action: TrackingAction::Select(EVENT),
                },
                obs,
            )
            .unwrap()
    });
    state.refresh_tracking(|_| Some(source()), 2000);
    let mut worker =
        PlanningWorker::new(FULL_TRAVEL_CALIBRATION, nominal(), InputSettings::default());
    for _ in 0..10_000 {
        worker.poll(&mut state, |_| Some(source()), || 2000);
        if state.planning_status().is_some_and(|s| s.offload_ready) {
            break;
        }
    }
    assert!(state.planning_status().unwrap().offload_ready);
    let mut freshness = CommandFreshness::new(client);
    let proof = freshness.issue(2000).unwrap().token;
    let intent = PlanningIntent {
        proof,
        lease: grant.token.generation(),
        job: state.tracking().planning_ticket().unwrap().serial(),
        upload: None,
    };
    (state, worker, client, freshness, intent)
}
fn offer(state: &CoordinationState, job: u64) -> PlanningInput {
    let mut bytes = [0; INPUT_CAPACITY];
    let (len, total) = state.planning_chunk(job, 0, &mut bytes).unwrap();
    assert_eq!(len, total);
    PlanningInput::decode(&bytes[..len]).unwrap()
}
fn calculate(input: PlanningInput) -> TrackingPlan {
    let mut planner = input.planner().unwrap();
    while !planner.is_finished() {
        planner.step().unwrap();
    }
    planner.take_report().unwrap().plan.unwrap()
}
fn finish(state: &mut CoordinationState, worker: &mut PlanningWorker) {
    let src = source();
    for _ in 0..100_000 {
        worker.poll(state, |_| Some(src.clone()), || 2000);
        if state.tracking().planning_ticket().is_none() {
            return;
        }
    }
    panic!("bounded fixture did not finish");
}
#[test]
fn uploaded_plan_is_independently_rebuilt_before_publication() {
    let (mut state, mut worker, client, freshness, mut intent) = setup();
    let plan = calculate(offer(&state, intent.job));
    intent.upload = Some(UploadedPlan::new(intent.job, &plan));
    assert!(!state.request_planning(client + 1, intent, &freshness, 2000));
    assert!(!state.request_planning(
        client,
        PlanningIntent {
            lease: intent.lease + 1,
            ..intent
        },
        &freshness,
        2000
    ));
    assert!(!state.request_planning(
        client,
        PlanningIntent { proof: 0, ..intent },
        &freshness,
        2000
    ));
    assert!(state.request_planning(client, intent, &freshness, 2000));
    assert!(!state.request_planning(client, intent, &freshness, 2000));
    assert!(state.tracking().bound_plan().is_none());
    worker.poll(&mut state, |_| Some(source()), || 2000);
    assert!(state.planning_status().unwrap().verifying);
    assert!(state.tracking().bound_plan().is_none());
    finish(&mut state, &mut worker);
    assert_eq!(state.tracking().bound_plan().unwrap().plan(), &plan);
    assert_eq!(state.tracking().snapshot().mode, TrackingMode::Manual);
    let status = state.planning_status().unwrap();
    assert!(status.verifying);
    assert!(!status.offload_ready);
    assert_eq!(status.work.unwrap().phase, "complete");
}
#[test]
fn mismatched_result_falls_back_without_publishing_browser_claims() {
    let (mut state, mut worker, client, freshness, mut intent) = setup();
    let plan = calculate(offer(&state, intent.job));
    let mut upload = UploadedPlan::new(intent.job, &plan);
    upload.digest ^= 1;
    intent.upload = Some(upload);
    assert!(state.request_planning(client, intent, &freshness, 2000));
    // Fresh feedback can precede the next output service without changing the
    // starting-pose revision. Fallback must use its original captured start.
    let feedback = full_travel_sample(state.feedback().last_valid.unwrap().position(), 2001);
    let mut interlock = FeedbackInterlock::new(full_travel_installed());
    interlock.publish(Some(feedback));
    let revision = state.start_revision();
    state.publish_feedback(
        FeedbackSnapshot {
            health: FeedbackHealth::Healthy,
            last_valid: Some(feedback),
            ..FeedbackSnapshot::INITIAL
        },
        interlock,
        2001,
    );
    assert_eq!(state.start_revision(), revision);
    assert!(state.resting_start().is_none());
    let src = source();
    for _ in 0..100_000 {
        worker.poll(&mut state, |_| Some(src.clone()), || 2000);
        if state.planning_status().is_some_and(|s| s.upload_error) {
            break;
        }
    }
    assert!(state.planning_status().unwrap().upload_error);
    assert!(!state.planning_status().unwrap().verifying);
    assert!(state.tracking().bound_plan().is_none());
    assert!(!state.request_planning(client, intent, &freshness, 2000));
    finish(&mut state, &mut worker);
    assert_eq!(state.tracking().bound_plan().unwrap().plan(), &plan);
}

#[test]
fn in_place_verification_preserves_completed_local_candidates_and_best_report() {
    let (_, input) = common::planning::portable("nominal");
    let mut baseline = input.clone().planner().unwrap();
    while !baseline.is_finished() {
        baseline.step().unwrap();
    }
    let expected = baseline.take_report().unwrap();
    let mut upload = UploadedPlan::new(input.job, expected.plan.as_ref().unwrap());
    upload.digest ^= 1;
    let mut planner = input.clone().planner().unwrap();
    while planner.progress().candidates_done < 3 {
        planner.step().unwrap();
    }
    let before = planner.progress();
    assert!(planner.queue_verification(upload));
    let mut result = None;
    while result.is_none() && !planner.is_finished() {
        planner.step().unwrap();
        result = planner.take_verification_result();
    }
    assert_eq!(result, Some(false));
    assert!(!planner.is_finished());
    assert!(planner.progress().candidates_done >= before.candidates_done);
    assert!(planner.progress().candidate > 1);
    assert!(planner.progress().work_steps > before.work_steps);
    assert_eq!(planner.resting_start(), input.start);
    while !planner.is_finished() {
        planner.step().unwrap();
    }
    let actual = planner.take_report().unwrap();
    assert_eq!(actual.plan, expected.plan);
    assert_eq!(actual.closest_attempt, expected.closest_attempt);
    assert_eq!(actual.evaluated_candidates, expected.evaluated_candidates);
    assert_eq!(actual.geometry_rejections, expected.geometry_rejections);
    assert!(actual.work_steps > expected.work_steps);
}

#[test]
fn in_place_verification_reproduces_complete_and_partial_browser_plans() {
    for case in ["nominal", "partial"] {
        let (_, input) = common::planning::portable(case);
        let plan = calculate(input.clone());
        let mut planner = input.clone().planner().unwrap();
        assert!(planner.queue_verification(UploadedPlan::new(input.job, &plan)));
        while !planner.is_finished() {
            planner.step().unwrap();
        }
        assert_eq!(planner.take_verification_result(), Some(true), "{case}");
        assert_eq!(planner.take_report().unwrap().plan, Some(plan), "{case}");
    }
}

#[test]
fn mismatched_partial_candidate_restores_the_original_partial_search() {
    let (_, input) = common::planning::portable("partial");
    let mut baseline = input.clone().planner().unwrap();
    while !baseline.is_finished() {
        baseline.step().unwrap();
    }
    let expected = baseline.take_report().unwrap();
    let mut upload = UploadedPlan::new(input.job, expected.plan.as_ref().unwrap());
    upload.digest ^= 1;
    let mut planner = input.planner().unwrap();
    assert!(planner.queue_verification(upload));
    while planner.take_verification_result().is_none() {
        assert!(!planner.is_finished());
        planner.step().unwrap();
    }
    assert!(planner.progress().partial);
    assert_eq!(planner.progress().candidates_total, CANDIDATES);
    while !planner.is_finished() {
        planner.step().unwrap();
    }
    let actual = planner.take_report().unwrap();
    assert_eq!(actual.plan, expected.plan);
    assert_eq!(actual.evaluated_candidates, expected.evaluated_candidates);
    assert_eq!(actual.geometry_rejections, expected.geometry_rejections);
}

#[test]
fn late_start_progress_counts_only_the_partial_round() {
    let (_, input) = common::planning::portable("partial");
    let mut planner = input.planner().unwrap();
    loop {
        let progress = planner.progress();
        assert!(progress.partial);
        assert_eq!(progress.candidates_total, CANDIDATES);
        if planner.is_finished() {
            break;
        }
        planner.step().unwrap();
    }
}
#[test]
fn replaced_jobs_reject_queued_uploads_and_retire_snapshot_chunks() {
    let (mut state, mut worker, client, freshness, mut intent) = setup();
    intent.upload = Some(UploadedPlan::new(
        intent.job,
        &calculate(offer(&state, intent.job)),
    ));
    assert!(state.request_planning(client, intent, &freshness, 2000));
    let next = PlanRevisions {
        orbit: 1,
        start: state.start_revision() + 1,
    };
    state.tracking_operation(2000, |runtime, obs| {
        runtime.refresh(&[EVENT], next, obs).unwrap()
    });
    worker.poll(&mut state, |_| Some(source()), || 2000);
    assert_ne!(
        state.tracking().planning_ticket().unwrap().serial(),
        intent.job
    );
    assert!(!state.request_planning(client, intent, &freshness, 2000));
    assert!(state.planning_chunk(intent.job, 0, &mut [0; 1]).is_none());
    let status = state.planning_status().unwrap();
    assert_eq!(status.restarts, 1);
    assert_eq!(
        status.restart_reason,
        "starting pose or motor history changed"
    );
    assert!(!status.verifying);
}
#[test]
fn snapshots_reject_truncation_trailing_bytes_and_wrong_versions() {
    let (_, input) = common::planning::portable("scenarios");
    let mut bytes = [0; INPUT_CAPACITY];
    let len = input.encode(&mut bytes).unwrap();
    for end in 0..len {
        assert!(PlanningInput::decode(&bytes[..end]).is_none());
    }
    assert!(PlanningInput::decode(&bytes[..len + 1]).is_none());
    bytes[0] ^= 1;
    assert!(PlanningInput::decode(&bytes[..len]).is_none());
    assert!(input.encode(&mut [0; 10]).is_none());
}
#[test]
fn upload_protocol_accepts_full_width_digests_and_rejects_malformed_values() {
    assert!(matches!(
        ClientMessage::parse("plan:1:2:3:put:23:1:18446744073709551615"),
        Some(ClientMessage::Planning(_))
    ));
    for value in [
        "plan:1:0:3:get",
        "plan:1:2:0:get",
        "plan:1:2:3:get:extra",
        "plan:1:2:3:put:24:0:1",
        "plan:1:2:3:put:1:2:1",
        "plan:1:2:3:put:1:0:18446744073709551616",
    ] {
        assert!(ClientMessage::parse(value).is_none(), "{value}");
    }
}

#[test]
fn progress_reads_do_not_advance_work_and_eta_uses_elapsed_device_time() {
    let (mut state, mut worker, client, _, _) = setup();
    let before = state.planning_status().unwrap().work.unwrap().work_steps;
    let mut output: heapless::String<{ ezal_core::protocol::TELEMETRY_CAPACITY }> =
        heapless::String::new();
    state
        .tracking_telemetry(client, 3000)
        .write_json(&mut output)
        .unwrap();
    assert!(output.contains("\"progress\":{\"job\":"));
    assert_eq!(
        state.planning_status().unwrap().work.unwrap().work_steps,
        before
    );
    worker.poll(&mut state, |_| Some(source()), || 3000);
    let after = state.planning_status().unwrap();
    assert!(after.work.unwrap().work_steps > before);
    assert_eq!(after.last_progress_at_ms, 3000);
    assert!(after.upper_ms.is_some_and(|ms| ms > 0));
}

#[test]
fn validation_cannot_extend_the_original_execution_window() {
    let (mut state, mut worker, client, freshness, mut intent) = setup();
    intent.upload = Some(UploadedPlan::new(
        intent.job,
        &calculate(offer(&state, intent.job)),
    ));
    assert!(state.request_planning(client, intent, &freshness, 2000));
    let src = source();
    for _ in 0..100_000 {
        worker.poll(&mut state, |_| Some(src.clone()), || 200_000);
        if state.tracking().planning_ticket().is_none() {
            break;
        }
    }
    assert!(state.tracking().planning_ticket().is_none());
    assert!(state.tracking().bound_plan().is_none());
    assert!(matches!(
        state.tracking().snapshot().plan,
        PlanStatus::Blocked(_)
    ));
    assert_eq!(state.tracking().snapshot().mode, TrackingMode::Manual);
}
