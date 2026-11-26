//! Thin browser ABI. All planning, simulation and result identity live in ezal-core.
use ezal_core::tracking::offload::{PlanningInput, UploadedPlan, INPUT_CAPACITY};
use ezal_core::tracking::{Planner, PreparedTrack};
use std::cell::RefCell;

struct State {
    input: [u8; INPUT_CAPACITY],
    planner: Option<Box<Planner<PreparedTrack>>>,
    job: u64,
    output: String,
    status: u32,
}
thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State {
        input: [0; INPUT_CAPACITY], planner: None, job: 0, output: String::new(), status: 3,
    }) };
}

#[no_mangle]
pub extern "C" fn planner_input_ptr() -> *mut u8 {
    STATE.with_borrow_mut(|s| s.input.as_mut_ptr())
}
#[no_mangle]
pub extern "C" fn planner_input_capacity() -> usize {
    INPUT_CAPACITY
}
#[no_mangle]
pub extern "C" fn planner_init(len: usize) -> u32 {
    STATE.with_borrow_mut(|s| {
        s.planner = None;
        s.output.clear();
        s.status = 3;
        let Some(input) = s.input.get(..len).and_then(PlanningInput::decode) else {
            s.output = "{\"error\":\"Invalid planning snapshot or version\"}".into();
            return 3;
        };
        s.job = input.job;
        match input.planner() {
            Ok(planner) => {
                s.planner = Some(Box::new(planner));
                s.status = 0;
            }
            Err(error) => {
                s.output = format!("{{\"error\":\"{error:?}\"}}");
            }
        }
        s.status
    })
}
/// At most 8192 bounded shared-Rust steps per call; the worker yields between batches.
#[no_mangle]
pub extern "C" fn planner_step(budget: u32) -> u32 {
    STATE.with_borrow_mut(|s| {
        if s.status != 0 { return s.status; }
        let planner = s.planner.as_mut().expect("initialized");
        for _ in 0..budget.clamp(1, 8192) {
            let _ = planner.step();
            if planner.is_finished() { break; }
        }
        let p = planner.progress();
        if planner.is_finished() {
            match planner.take_report() {
                Ok(report) => match report.plan {
                    Some(plan) => {
                        let u = UploadedPlan::new(s.job, &plan);
                        s.output = format!("{{\"job\":\"{}\",\"candidate\":{},\"partial\":{},\"digest\":\"{}\",\"work_steps\":{}}}", u.job, u.candidate, u.partial, u.digest, report.work_steps);
                        s.status = 1;
                    }
                    None => { s.output = format!("{{\"error\":\"No supported route found\",\"work_steps\":{}}}", report.work_steps); s.status = 2; }
                },
                Err(error) => { s.output = format!("{{\"error\":\"{error:?}\"}}"); s.status = 3; }
            }
            s.planner = None;
        } else {
            s.output = format!("{{\"phase\":\"{}\",\"work_steps\":{},\"candidate\":{},\"candidates_done\":{},\"candidates_total\":{},\"scenario\":{},\"scenarios\":{},\"partial\":{}}}", p.phase, p.work_steps, p.candidate, p.candidates_done, p.candidates_total, p.scenario, p.scenarios, p.partial);
        }
        s.status
    })
}
#[no_mangle]
pub extern "C" fn planner_output_ptr() -> *const u8 {
    STATE.with_borrow(|s| s.output.as_ptr())
}
#[no_mangle]
pub extern "C" fn planner_output_len() -> usize {
    STATE.with_borrow(|s| s.output.len())
}
