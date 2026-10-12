use super::{Axis, FollowingController, MotorModel, MotorProfile, SimulationError, TrackingTarget};
use crate::actuator::{Actuator, ActuatorConfig, ActuatorInput};
use crate::ads1015::FEEDBACK_FULL_SCALE;
use crate::drive::{Command, DriveCommand, MOVEMENT_LEASE_MS};
use crate::duty::DutyHistory;
use crate::feedback::{
    FeedbackHealth, FeedbackMonitor, FeedbackSample, FEEDBACK_MAX_LATENCY_MS, FEEDBACK_PERIOD_MS,
};
use crate::interlock::FeedbackInterlock;
use crate::mailbox::CommandEnvelope;
use crate::network::NetworkGate;
use crate::position::{AxisCalibration, FeedbackVoltages, Pointing, PositionCalibration};

/// One repeatable uncertainty case. These finite cases do not prove all
/// intermediate combinations of motor behavior, noise or atmospheric effects.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scenario {
    /// Assumed loaded motor response; the follower keeps its own fixed reference.
    pub motor: MotorProfile,
    /// Total paired acquisition latency, at most 150 ms. The pair keeps the
    /// first channel's capture time as its timestamp.
    pub latency_ms: u64,
    /// Delay between capturing A0 and A1, included in that latency.
    pub channel_skew_ms: u64,
    /// Azimuth/elevation ADC-code offsets before quantization.
    pub noise_codes: [i8; 2],
    /// Alternating offsets exercise code-boundary chatter as well as fixed bias.
    pub alternate_noise: bool,
    /// Gear engagement at preparation start; `None` assumes the first
    /// requested direction is engaged.
    pub initial_engagement: [Option<bool>; 2],
}

impl Scenario {
    /// Nominal AC case, initially engaged in the first requested direction.
    pub const NOMINAL: Self = Self {
        motor: MotorProfile::AC_50HZ,
        latency_ms: 100,
        channel_skew_ms: 0,
        noise_codes: [0, 0],
        alternate_noise: false,
        initial_engagement: [None, None],
    };
}

/// Explicit plant, acquisition and sampling assumptions for a development run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SimulationSettings {
    /// Upper bound on time between simulation samples, 1–100 ms. Actuator and
    /// acquisition deadlines can produce shorter intervals.
    pub step_ms: u64,
    /// Motor response and feedback acquisition being modeled.
    pub scenario: Scenario,
}

/// Sampled development metrics; these are not a trajectory feasibility proof.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SimulationSummary {
    /// Total applied on-time per axis, including time before following begins.
    pub energized_ms: [u64; 2],
    /// Number of actual relay-state changes per axis, including releases.
    pub transitions: [u64; 2],
    /// Number of changes between successive energized directions per axis.
    pub reversals: [u64; 2],
}

/// One serviced output decision with the simulated physical position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SimulationSample {
    /// Monotonic simulation time at this sample.
    pub at_ms: u64,
    /// Actual mechanical pose; distinct from the quantized, delayed feedback.
    /// Its deviation from the sky direction is the simulation's ground truth.
    pub position: Pointing,
    /// Latest acquired feedback; initially absent until the first read completes.
    pub feedback: Option<FeedbackSample>,
    /// The controller's request before actuator permissions and timing.
    pub requested: Command,
    /// Output actually admitted by the production actuator.
    pub applied: DriveCommand,
}

#[derive(Clone, Copy, Debug)]
struct PendingRead {
    sampled_at_ms: u64,
    ready_at_ms: u64,
    raw: FeedbackVoltages,
    azimuth_captured: bool,
}

/// A coherent, already-serviced snapshot for evaluating preparation. The live
/// actuator is copied, never reset or mutated by planning. It must have no
/// active/pending output and have completed its configured mechanical coast.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RestingStart {
    /// Original monotonic service instant, independent of the pass's UTC anchor.
    pub at_ms: u64,
    /// Current calibrated position acquired after mechanical coast completed.
    pub feedback: FeedbackSample,
    /// Output-owner state, including duty and relay availability.
    pub actuator: Actuator,
    /// Permission generation copied with the actuator snapshot.
    pub network: NetworkGate,
}

/// Bounded, allocation-free simulation using the real feedback and output policy.
///
/// Each `step` services at most one acquisition and one actuator decision.
/// The caller supplies the route position at `now_ms`, and can yield between
/// steps. The harness honors actuator deadlines as well as its sampling cadence.
/// Network permission is renewed locally in the model; outage/task-stall tests
/// belong to the lifecycle/firmware integration tasks.
///
/// A target of `None` immediately inhibits output. Continue stepping with
/// `None` until `is_settled()` to include final coast after cancellation or LOS.
/// A failed simulation remains failed and its output is inhibited.
pub struct Simulation {
    calibration: PositionCalibration,
    settings: SimulationSettings,
    controller: FollowingController,
    model: MotorModel,
    actuator: Actuator,
    monitor: FeedbackMonitor,
    interlock: FeedbackInterlock,
    network: NetworkGate,
    pending_read: Option<PendingRead>,
    next_read_ms: u64,
    reads: u64,
    now_ms: u64,
    serviced_ms: u64,
    last_direction: [Option<bool>; 2],
    summary: SimulationSummary,
    fault: Option<SimulationError>,
}

impl Simulation {
    /// Construct an idle simulated installation with the supplied output policy
    /// and already consumed azimuth/elevation duty. Calibration is explicit;
    /// the harness never extends the firmware's installed endpoints implicitly.
    pub fn new(
        calibration: PositionCalibration,
        initial: Pointing,
        controller: FollowingController,
        settings: SimulationSettings,
        actuator: ActuatorConfig,
        used_duty_ms: [u64; 2],
    ) -> Result<Self, SimulationError> {
        let scenario = settings.scenario;
        if !(1..=100).contains(&settings.step_ms)
            || scenario.latency_ms > FEEDBACK_MAX_LATENCY_MS
            || scenario.channel_skew_ms > scenario.latency_ms
            || scenario
                .noise_codes
                .iter()
                .any(|code| code.unsigned_abs() > 2)
            || !actuator.is_valid()
            || controller.minimum_pulse_ms() != actuator.relays.min_active_ms
            || !DutyHistory::Known(used_duty_ms).is_valid(actuator.duty)
        {
            return Err(SimulationError::InvalidSettings);
        }
        let mut model = MotorModel::new(scenario.motor, calibration, initial, used_duty_ms)?;
        model.set_initial_rest([0, 0], scenario.initial_engagement);
        Ok(Self {
            calibration,
            settings,
            controller,
            model,
            actuator: Actuator::with_config(actuator, DutyHistory::Known(used_duty_ms)),
            monitor: FeedbackMonitor::new(calibration, None),
            interlock: FeedbackInterlock::INHIBITED,
            network: NetworkGate::new(),
            pending_read: None,
            next_read_ms: 0,
            reads: 0,
            now_ms: 0,
            serviced_ms: 0,
            last_direction: [None, None],
            summary: SimulationSummary::default(),
            fault: None,
        })
    }

    /// Evaluate from an existing idle installation instead of assuming rested
    /// relays and unused duty. The output policy and consumed duty are the snapshot's.
    pub fn from_rest(
        calibration: PositionCalibration,
        controller: FollowingController,
        settings: SimulationSettings,
        start: RestingStart,
    ) -> Result<Self, SimulationError> {
        let (used, idle) = start
            .actuator
            .resting_history(start.at_ms)
            .ok_or(SimulationError::InvalidStart)?;
        let age = start
            .at_ms
            .checked_sub(start.feedback.sampled_at_ms())
            .ok_or(SimulationError::InvalidStart)?;
        let actuator = start.actuator.configuration();
        if age > crate::feedback::FEEDBACK_MAX_AGE_MS
            || idle[0] < age.saturating_add(actuator.travel.azimuth.coast_ms)
            || idle[1] < age.saturating_add(actuator.travel.elevation.coast_ms)
        {
            return Err(SimulationError::InvalidStart);
        }
        let mut sim = Self::new(
            calibration,
            start.feedback.position(),
            controller,
            settings,
            actuator,
            used,
        )?;
        sim.model
            .set_initial_rest(idle, settings.scenario.initial_engagement);
        sim.actuator = start.actuator;
        sim.network = start.network;
        sim.now_ms = start.at_ms;
        sim.serviced_ms = start.at_ms;
        sim.next_read_ms = start.at_ms;
        Ok(sim)
    }

    /// Skip a fully idle waiting interval before preparation. No pending motion
    /// or coast may be discarded; duty rest is accounted at the next step.
    pub fn skip_idle_until(&mut self, at_ms: u64) -> Result<(), SimulationError> {
        if self.fault.is_some()
            || !self.is_settled()
            || self.actuator.target().is_active()
            || at_ms < self.now_ms
        {
            return Err(SimulationError::InvalidStart);
        }
        self.controller.clear_target();
        self.pending_read = None;
        self.next_read_ms = at_ms;
        self.now_ms = at_ms;
        Ok(())
    }

    /// Last serviced output-owner state, for preparation/readiness assessment.
    pub const fn actuator(&self) -> &Actuator {
        &self.actuator
    }

    /// Honor phase boundaries such as AOS/LOS exactly, even between grid ticks.
    pub fn stop_step_at(&mut self, at_ms: u64) {
        if at_ms > self.serviced_ms {
            self.now_ms = self.now_ms.min(at_ms);
        }
    }

    /// Next sample time; evaluate the supplied trajectory at this time.
    pub const fn now_ms(&self) -> u64 {
        self.now_ms
    }

    /// Metrics accumulated so far, including after a terminal simulation error.
    pub const fn summary(&self) -> SimulationSummary {
        self.summary
    }

    /// Actual model position, including coast that has already been simulated.
    pub fn position(&self) -> Pointing {
        self.model.position()
    }

    /// No output or coast remains. This does not imply the target was acquired.
    pub fn is_settled(&self) -> bool {
        !self.actuator.actual().is_active() && self.model.is_settled()
    }

    /// Copy a coherent post-coast start without resetting relay or duty history.
    pub(super) fn resting_start(&self) -> Option<RestingStart> {
        if self.fault.is_some() || !self.is_settled() || self.actuator.target().is_active() {
            return None;
        }
        let feedback = self.interlock.feedback()?;
        let age = self.serviced_ms.checked_sub(feedback.sampled_at_ms())?;
        let (_, idle) = self.actuator.resting_history(self.serviced_ms)?;
        let travel = self.actuator.configuration().travel;
        if age >= crate::feedback::FEEDBACK_MAX_AGE_MS
            || idle[0] < age.saturating_add(travel.azimuth.coast_ms)
            || idle[1] < age.saturating_add(travel.elevation.coast_ms)
        {
            return None;
        }
        Some(RestingStart {
            at_ms: self.serviced_ms,
            feedback,
            actuator: self.actuator,
            network: self.network,
        })
    }

    /// Advance one bounded step toward an already planned mechanical target,
    /// taken as exactly aligned with the sky.
    pub fn step(&mut self, target: Option<Pointing>) -> Result<SimulationSample, SimulationError> {
        self.step_target(target.map(TrackingTarget::exact))
    }

    /// Advance toward the route's mechanical position; the sky direction is
    /// validated but never drives output. `None` cancels output immediately,
    /// just as for `step`. Sky deviation, including the route's intentional
    /// offset, is measured from each sample's `position`.
    pub fn step_target(
        &mut self,
        target: Option<TrackingTarget>,
    ) -> Result<SimulationSample, SimulationError> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        let result = self.step_inner(target);
        if let Err(fault) = result {
            self.fault = Some(fault);
            let before = self.actuator.actual();
            self.actuator.step(
                ActuatorInput {
                    interlock: self.interlock,
                    network: self.network,
                    command: Some(CommandEnvelope {
                        command: Command::Inhibit,
                        valid_until_ms: self.now_ms,
                    }),
                },
                self.now_ms,
            );
            self.account_transitions(before, self.actuator.actual());
            self.controller
                .observe_output(self.actuator.actual(), self.now_ms);
            self.controller.clear_target();
        }
        result
    }

    fn step_inner(
        &mut self,
        target: Option<TrackingTarget>,
    ) -> Result<SimulationSample, SimulationError> {
        let now_ms = self.now_ms;
        let elapsed_ms = now_ms - self.serviced_ms;
        let before = self.actuator.actual();
        self.model.advance(before, elapsed_ms)?;
        for (i, active) in [before.azimuth.is_some(), before.elevation.is_some()]
            .into_iter()
            .enumerate()
        {
            if active {
                self.summary.energized_ms[i] += elapsed_ms;
            }
        }
        self.serviced_ms = now_ms;
        if target.is_some_and(|p| !p.mechanical.is_valid_for(self.calibration) || !p.sky.is_valid())
        {
            return Err(SimulationError::InvalidPointing);
        }
        self.acquire_feedback()?;
        self.network.observe(now_ms, true);
        let feedback = self.interlock.feedback();
        if target.is_none() {
            self.controller.clear_target();
        }
        let requested = target.map_or(Command::Inhibit, |target| {
            self.controller
                .command(target.mechanical, feedback, before, now_ms)
        });
        let update = self.actuator.step(
            ActuatorInput {
                interlock: self.interlock,
                network: self.network,
                command: Some(CommandEnvelope {
                    command: requested,
                    valid_until_ms: now_ms
                        .checked_add(MOVEMENT_LEASE_MS)
                        .ok_or(SimulationError::TimeOverflow)?,
                }),
            },
            now_ms,
        );
        self.account_transitions(before, update.output);
        self.controller.observe_output(update.output, now_ms);
        if let Some(fault) = update.status.motion_fault {
            return Err(SimulationError::Motion(fault));
        }
        if let Command::Drive(drive) | Command::Jog(drive) = requested {
            for (axis, requested, applied, duty) in [
                (
                    Axis::Azimuth,
                    drive.azimuth.is_some(),
                    update.output.azimuth.is_some(),
                    update.status.duty.azimuth,
                ),
                (
                    Axis::Elevation,
                    drive.elevation.is_some(),
                    update.output.elevation.is_some(),
                    update.status.duty.elevation,
                ),
            ] {
                if requested && !applied && duty.cooldown_ms > 0 {
                    return Err(SimulationError::DutyLimit(axis));
                }
            }
        }
        self.model.advance(update.output, 0)?;
        let mut next_ms = now_ms
            .checked_add(self.settings.step_ms)
            .ok_or(SimulationError::TimeOverflow)?;
        let read_ms = self.pending_read.map_or(self.next_read_ms, |r| {
            if r.azimuth_captured {
                r.ready_at_ms
            } else {
                r.sampled_at_ms + self.settings.scenario.channel_skew_ms
            }
        });
        next_ms = next_ms.min(read_ms);
        if let Some(deadline) = update.next_deadline_ms {
            // Once serviced, a current deadline must not cause a zero-time loop.
            next_ms = next_ms.min(deadline.max(now_ms + 1));
        }
        for (i, active) in [
            update.output.azimuth.is_some(),
            update.output.elevation.is_some(),
        ]
        .into_iter()
        .enumerate()
        {
            if active {
                let remaining = self.model.remaining_duty_ms()[i];
                if remaining == 0 {
                    return Err(SimulationError::DutyLimit(if i == 0 {
                        Axis::Azimuth
                    } else {
                        Axis::Elevation
                    }));
                }
                next_ms = next_ms.min(now_ms.saturating_add(remaining));
            }
        }
        self.now_ms = next_ms;
        Ok(SimulationSample {
            at_ms: now_ms,
            position: self.model.position(),
            feedback,
            requested,
            applied: update.output,
        })
    }

    fn acquire_feedback(&mut self) -> Result<(), SimulationError> {
        if self.pending_read.is_none() && self.now_ms >= self.next_read_ms {
            let position = self.model.position();
            // Quantize the analog voltage directly. Going through the diagnostic
            // integer-mV conversion first would round twice and bias ADC codes.
            let sign = if self.settings.scenario.alternate_noise && self.reads % 2 == 1 {
                -1
            } else {
                1
            };
            self.pending_read = Some(PendingRead {
                sampled_at_ms: self.now_ms,
                ready_at_ms: self
                    .now_ms
                    .checked_add(self.settings.scenario.latency_ms)
                    .ok_or(SimulationError::TimeOverflow)?,
                raw: FeedbackVoltages {
                    a0_elevation_mv: quantize(
                        self.calibration.elevation,
                        position.elevation_deg,
                        sign * self.settings.scenario.noise_codes[1],
                    ),
                    a1_azimuth_mv: quantize(
                        self.calibration.azimuth,
                        position.azimuth_deg,
                        sign * self.settings.scenario.noise_codes[0],
                    ),
                },
                azimuth_captured: self.settings.scenario.channel_skew_ms == 0,
            });
        }
        if let Some(read) = &mut self.pending_read {
            if !read.azimuth_captured
                && self.now_ms >= read.sampled_at_ms + self.settings.scenario.channel_skew_ms
            {
                let sign = if self.settings.scenario.alternate_noise && self.reads % 2 == 1 {
                    -1
                } else {
                    1
                };
                read.raw.a1_azimuth_mv = quantize(
                    self.calibration.azimuth,
                    self.model.position().azimuth_deg,
                    sign * self.settings.scenario.noise_codes[0],
                );
                read.azimuth_captured = true;
            }
        }
        if let Some(read) = self.pending_read.filter(|r| r.ready_at_ms <= self.now_ms) {
            let update = self
                .monitor
                .update(self.now_ms, read.sampled_at_ms, Ok(read.raw));
            self.interlock = update.interlock;
            self.pending_read = None;
            self.reads = self.reads.saturating_add(1);
            self.next_read_ms = self
                .now_ms
                .checked_add(FEEDBACK_PERIOD_MS)
                .ok_or(SimulationError::TimeOverflow)?;
            if update.snapshot.health != FeedbackHealth::Healthy {
                return Err(SimulationError::Feedback(update.snapshot.health));
            }
        }
        Ok(())
    }

    fn account_transitions(&mut self, before: DriveCommand, after: DriveCommand) {
        let previous = [
            before.azimuth.map(|d| d.increases_angle()),
            before.elevation.map(|d| d.increases_angle()),
        ];
        let next = [
            after.azimuth.map(|d| d.increases_angle()),
            after.elevation.map(|d| d.increases_angle()),
        ];
        for i in 0..2 {
            if previous[i] != next[i] {
                self.summary.transitions[i] += 1;
            }
            if let Some(direction) = next[i] {
                if self.last_direction[i].is_some_and(|old| old != direction) {
                    self.summary.reversals[i] += 1;
                }
                self.last_direction[i] = Some(direction);
            }
        }
    }
}

fn quantize(axis: AxisCalibration, angle: f32, noise: i8) -> i32 {
    let fraction =
        f64::from(angle - axis.angle_min_deg) / f64::from(axis.angle_max_deg - axis.angle_min_deg);
    let mv = f64::from(axis.voltage_min_mv)
        + fraction * f64::from(axis.voltage_max_mv - axis.voltage_min_mv);
    let code_mv = f64::from(FEEDBACK_FULL_SCALE.code_mv());
    (libm::round(mv / code_mv + f64::from(noise)) * code_mv) as i32
}
