//! Background work for the device-owned tracker. This task never accesses GPIO
//! or feeds the watchdog. Each planning unit yields to the thread executor.

use core::cell::Cell;
use embassy_futures::yield_now;
use embassy_sync::blocking_mutex::{raw::ThreadModeRawMutex, Mutex};
use embassy_time::{Duration, Instant, Timer};
use ezal_core::coordination::{PlanningUnit, PlanningWorker};
use ezal_core::installation;
use ezal_core::position::HARD_CODED_CALIBRATION;
use ezal_core::tracking::InputSettings;

use crate::prediction;
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

fn record(update: impl FnOnce(&mut Timing)) {
    TIMING.lock(|cell| {
        let mut timing = cell.get();
        update(&mut timing);
        cell.set(timing);
    });
}

pub fn record_control(elapsed_us: u64, gap_us: u64, tracking: bool) {
    record(|timing| {
        timing.control_us = timing.control_us.max(elapsed_us);
        if tracking {
            timing.control_gap_us = timing.control_gap_us.max(gap_us);
        }
    });
}

/// Called both on atomic forecast/source publication and by the worker.
pub fn refresh_inputs(state: &SharedState) {
    state.with(|s| s.refresh_tracking(prediction::tracking_source, Instant::now().as_millis()));
}

#[embassy_executor::task]
pub async fn run(state: &'static SharedState) -> ! {
    // Embassy stores this bounded planner in the task's static future, not a
    // heap or the live output task. Never put it inside an IRQ-masking mutex.
    let mut worker = PlanningWorker::new(
        HARD_CODED_CALIBRATION,
        installation::planning_config(),
        InputSettings {
            observer: prediction::observer(),
            ..InputSettings::default()
        },
    );
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
        // Thread-mode locking leaves IRQs enabled through the unit.
        let begin = Instant::now();
        let unit = state.with(|s| {
            worker.poll(s, prediction::tracking_source, || {
                Instant::now().as_millis()
            })
        });
        let unit_us = begin.elapsed().as_micros();
        record(|timing| {
            match unit {
                PlanningUnit::Idle => {}
                PlanningUnit::Recovery => {
                    timing.recovery_step_us = timing.recovery_step_us.max(unit_us)
                }
                PlanningUnit::Planning => {
                    timing.planning_step_us = timing.planning_step_us.max(unit_us);
                    timing.planning_steps = timing.planning_steps.saturating_add(1);
                }
            }
            timing.worker_poll_us = timing.worker_poll_us.max(now.elapsed().as_micros());
        });
        if unit == PlanningUnit::Idle {
            Timer::after_millis(10).await;
        } else {
            // No millisecond timer per simulation step: millions of bounded
            // units still cooperate without imposing hours of artificial delay.
            yield_now().await;
        }
    }
}
