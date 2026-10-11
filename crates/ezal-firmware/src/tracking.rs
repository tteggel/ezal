//! Background work for the device-owned tracker. This task never accesses GPIO
//! or feeds the watchdog. Each planning unit yields to the thread executor.

use core::cell::Cell;
use embassy_futures::yield_now;
use embassy_sync::blocking_mutex::{raw::ThreadModeRawMutex, Mutex};
use embassy_time::{Duration, Instant, Timer};
use ezal_core::position::HARD_CODED_CALIBRATION;
use ezal_core::tracking::{
    execution_window, ExecutionInputs, InputError, InputSettings, PlanRevisions, PlanTicket,
    Planner, PlanningConfig, RestingStart, TimingError,
};

use crate::state::SharedState;

#[derive(Clone, Copy, Default)]
struct Timing {
    control_us: u64,
    control_gap_us: u64,
    planning_step_us: u64,
    recovery_step_us: u64,
    worker_poll_us: u64,
    planning_steps: u64,
}

static TIMING: Mutex<ThreadModeRawMutex, Cell<Timing>> = Mutex::new(Cell::new(Timing {
    control_us: 0,
    control_gap_us: 0,
    planning_step_us: 0,
    recovery_step_us: 0,
    worker_poll_us: 0,
    planning_steps: 0,
}));

pub fn record_control(elapsed_us: u64, gap_us: u64, tracking: bool) {
    TIMING.lock(|cell| {
        let mut timing = cell.get();
        timing.control_us = timing.control_us.max(elapsed_us);
        if tracking {
            timing.control_gap_us = timing.control_gap_us.max(gap_us);
        }
        cell.set(timing);
    });
}

fn record_worker(begin: Instant) {
    TIMING.lock(|cell| {
        let mut timing = cell.get();
        timing.worker_poll_us = timing
            .worker_poll_us
            .max(Instant::now().duration_since(begin).as_micros());
        cell.set(timing);
    });
}

/// Called both on atomic forecast/source publication and by the worker. A
/// browser cannot supply revisions, source data, motor policy or a start pose.
pub fn refresh_inputs(state: &SharedState) {
    let selected = state.with(|s| s.tracking().snapshot().selection);
    let Some(selected) = selected else { return };
    let Some((source, events)) = crate::prediction::tracking_source(selected.original.norad_id)
    else {
        return;
    };
    state.with(|s| {
        let start = s.start_revision();
        s.tracking_operation(Instant::now().as_millis(), |runtime, observation| {
            let revisions = PlanRevisions {
                orbit: source.publication().revision,
                observer: 1,
                calibration: 1,
                motion: 1,
                clock: observation.clock.anchor().map_or(0, |a| a.revision()),
                start,
            };
            let _ = runtime.refresh(&events, revisions, observation);
        });
    });
}

struct Captured {
    inputs: ExecutionInputs,
    start: RestingStart,
    config: PlanningConfig,
}

fn capture(state: &SharedState, ticket: PlanTicket) -> Result<Option<Captured>, InputError> {
    let Some((source, _)) = crate::prediction::tracking_source(ticket.pass().norad_id) else {
        return Err(InputError::MissingOrbit);
    };
    let Some(start) = state.with(|s| s.resting_start()) else {
        return Ok(None);
    };
    let config = PlanningConfig {
        preparation_timing: ticket.preparation_timing(),
        ..PlanningConfig::default()
    };
    let inputs = state.with(|s| {
        s.tracking_operation(Instant::now().as_millis(), |_, observation| {
            let anchor = observation.clock.anchor().ok_or(TimingError::Unavailable)?;
            let pass = ticket.pass();
            let window = execution_window(
                anchor,
                pass.aos_unix_s as u64 * 1000,
                pass.los_unix_s as u64 * 1000,
            )?;
            ExecutionInputs::capture(
                ticket,
                source,
                InputSettings {
                    observer: crate::prediction::observer(),
                    ..InputSettings::default()
                },
                observation.clock,
                window,
                &config,
                observation.now_ms,
            )
        })
    })?;
    Ok(Some(Captured {
        inputs,
        start,
        config,
    }))
}

#[embassy_executor::task]
pub async fn run(state: &'static SharedState) -> ! {
    // Embassy stores this bounded planner in the task's static future, not a
    // heap or the live output task. Never put it inside an IRQ-masking mutex.
    let mut work: Option<Planner<ExecutionInputs>> = None;
    let mut next_refresh = Instant::now();
    let mut next_log = Instant::now() + Duration::from_secs(60);
    loop {
        let now = Instant::now();
        if now >= next_refresh {
            refresh_inputs(state);
            next_refresh = now + Duration::from_millis(100);
        }
        if now >= next_log {
            let timing = TIMING.lock(Cell::get);
            defmt::info!("tracking timing maxima: output={=u64} us, active gap={=u64} us, planner={=u64} us, recovery={=u64} us, worker poll={=u64} us, steps={=u64}",
                timing.control_us, timing.control_gap_us, timing.planning_step_us, timing.recovery_step_us, timing.worker_poll_us, timing.planning_steps);
            next_log = now + Duration::from_secs(60);
        }
        let ticket = state.with(|s| s.tracking().planning_ticket());
        if work
            .as_ref()
            .is_some_and(|job| Some(job.source().ticket()) != ticket)
        {
            work = None;
        }
        let begin = Instant::now();
        let recovery = state.with(|s| {
            s.tracking_operation(begin.as_millis(), |runtime, observation| {
                runtime.step_reacquisition(observation).unwrap_or(false)
            })
        });
        let recovery_us = Instant::now().duration_since(begin).as_micros();
        TIMING.lock(|cell| {
            let mut timing = cell.get();
            timing.recovery_step_us = timing.recovery_step_us.max(recovery_us);
            cell.set(timing);
        });
        if recovery {
            record_worker(now);
            yield_now().await;
            continue;
        }
        if work.is_none() {
            if let Some(ticket) = ticket {
                match capture(state, ticket) {
                    Ok(Some(captured)) => {
                        let inputs = captured.inputs;
                        match Planner::new(
                            inputs.clone(),
                            inputs.window(),
                            HARD_CODED_CALIBRATION,
                            captured.start,
                            captured.config,
                        ) {
                            Ok(planner) => work = Some(planner),
                            Err(error) => state.with(|s| {
                                s.tracking_operation(
                                    Instant::now().as_millis(),
                                    |runtime, observation| {
                                        let _ = runtime.publish_plan(
                                            ticket,
                                            Err(error),
                                            inputs,
                                            observation,
                                        );
                                    },
                                )
                            }),
                        }
                    }
                    Ok(None) => {}
                    Err(error) => state.with(|s| {
                        s.tracking_operation(Instant::now().as_millis(), |runtime, observation| {
                            let _ = runtime.reject_inputs(ticket, error, observation);
                        })
                    }),
                }
            }
        }
        if let Some(job) = work.as_mut() {
            let begin = Instant::now();
            let _ = job.step();
            let elapsed = Instant::now().duration_since(begin).as_micros();
            TIMING.lock(|cell| {
                let mut timing = cell.get();
                timing.planning_step_us = timing.planning_step_us.max(elapsed);
                timing.planning_steps = timing.planning_steps.saturating_add(1);
                cell.set(timing);
            });
            if job.is_finished() {
                let inputs = job.source().clone();
                let result = job.take_report();
                state.with(|s| {
                    s.tracking_operation(Instant::now().as_millis(), |runtime, observation| {
                        let _ = runtime.publish_plan(inputs.ticket(), result, inputs, observation);
                    })
                });
            }
            if job.is_finished() {
                work = None;
            }
            // No millisecond timer per simulation step: millions of bounded
            // units still cooperate without imposing hours of artificial delay.
            record_worker(now);
            yield_now().await;
        } else {
            record_worker(now);
            Timer::after_millis(10).await;
        }
    }
}
