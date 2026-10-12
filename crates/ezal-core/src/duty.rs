//! Energized-time accounting owned by the final actuator, across all command sources.
//!
//! Each axis replenishes its allowance only after a continuous full rest.
//! Stops, mode/owner changes, short pulse gaps and network recovery cannot reset
//! accumulated usage. Unknown boot history requires a full rest before movement.

use crate::drive::DriveCommand;

/// Conservative operating/rest allowance, not a thermal model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DutyConfig {
    /// Accumulated energized time available on each axis after a complete rest.
    pub energized_ms: u64,
    /// Required continuously de-energized interval before replenishment.
    pub rest_ms: u64,
}

impl DutyConfig {
    /// Unidentified/DC hardware: three minutes energized, fifteen minutes rest.
    pub const DEFAULT: Self = Self {
        energized_ms: 180_000,
        rest_ms: 900_000,
    };

    /// Use only after confirming the original AC G-5500 installation.
    pub const CONFIRMED_AC: Self = Self {
        energized_ms: 300_000,
        rest_ms: 900_000,
    };

    /// Both allowances must be nonzero.
    pub const fn is_valid(self) -> bool {
        self.energized_ms > 0 && self.rest_ms > 0
    }
}

/// Evidence about previously applied motion when creating an actuator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DutyHistory {
    /// A boot/reset has no trustworthy previous duty record; require rest.
    Unknown,
    /// Explicit known accumulated azimuth/elevation energized milliseconds.
    /// `[0, 0]` asserts a rested installation, as in a controlled simulation.
    Known([u64; 2]),
}

impl DutyHistory {
    pub(crate) const fn is_valid(self, config: DutyConfig) -> bool {
        match self {
            Self::Unknown => true,
            Self::Known(used) => used[0] <= config.energized_ms && used[1] <= config.energized_ms,
        }
    }
}

/// One axis's current allowance and any required rest before another pulse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AxisDutyStatus {
    /// Energized milliseconds still available.
    pub remaining_ms: u64,
    /// Remaining continuous rest when another minimum-length pulse cannot fit;
    /// zero while a new pulse is permitted. An active axis has not begun resting.
    pub cooldown_ms: u64,
}

/// Coherent per-axis duty telemetry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DutyStatus {
    /// Azimuth motor allowance.
    pub azimuth: AxisDutyStatus,
    /// Elevation motor allowance.
    pub elevation: AxisDutyStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AxisDuty {
    used_ms: u64,
    idle_ms: u64,
}

impl AxisDuty {
    const fn new(used_ms: u64) -> Self {
        Self {
            used_ms,
            idle_ms: 0,
        }
    }

    fn account(&mut self, energized: bool, elapsed_ms: u64, config: DutyConfig) {
        if energized {
            self.used_ms = self.used_ms.saturating_add(elapsed_ms);
            self.idle_ms = 0;
        } else {
            self.idle_ms = self.idle_ms.saturating_add(elapsed_ms);
            if self.idle_ms >= config.rest_ms {
                self.used_ms = 0;
            }
        }
    }

    const fn remaining(self, config: DutyConfig) -> u64 {
        config.energized_ms.saturating_sub(self.used_ms)
    }

    const fn status(self, config: DutyConfig, min_active_ms: u64) -> AxisDutyStatus {
        let remaining_ms = self.remaining(config);
        AxisDutyStatus {
            remaining_ms,
            cooldown_ms: if remaining_ms < min_active_ms {
                config.rest_ms.saturating_sub(self.idle_ms)
            } else {
                0
            },
        }
    }
}

/// Owned by the actuator so command-source changes cannot discard history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DutyMonitor {
    config: DutyConfig,
    min_active_ms: u64,
    axes: [AxisDuty; 2],
    observed_ms: Option<u64>,
}

impl DutyMonitor {
    /// Read accounting at the last serviced instant, for an offline clone.
    pub(crate) fn history(&self, now_ms: u64) -> Option<([u64; 2], [u64; 2])> {
        (self.observed_ms == Some(now_ms)).then_some((
            [self.axes[0].used_ms, self.axes[1].used_ms],
            [self.axes[0].idle_ms, self.axes[1].idle_ms],
        ))
    }

    pub(crate) const fn new(config: DutyConfig, history: DutyHistory, min_active_ms: u64) -> Self {
        let used = match history {
            DutyHistory::Unknown => [config.energized_ms; 2],
            DutyHistory::Known(used) => used,
        };
        Self {
            config,
            min_active_ms,
            axes: [AxisDuty::new(used[0]), AxisDuty::new(used[1])],
            observed_ms: None,
        }
    }

    /// Account for the previously applied output before any permission can cut it.
    pub(crate) fn observe(&mut self, applied: DriveCommand, now_ms: u64) {
        let Some(elapsed_ms) = now_ms.checked_sub(self.observed_ms.unwrap_or(now_ms)) else {
            return; // Motion supervision separately latches a backwards clock.
        };
        self.observed_ms = Some(now_ms);
        for (axis, energized) in self
            .axes
            .iter_mut()
            .zip([applied.azimuth.is_some(), applied.elevation.is_some()])
        {
            axis.account(energized, elapsed_ms, self.config);
        }
    }

    /// Active outputs may finish their remaining allowance. A new direction
    /// must have enough duty to cover an ordinary minimum pulse.
    pub(crate) fn guard_drive(
        &self,
        requested: DriveCommand,
        actual: DriveCommand,
    ) -> DriveCommand {
        let permitted = |axis: usize, continuing: bool| {
            let remaining = self.axes[axis].remaining(self.config);
            if continuing {
                remaining > 0
            } else {
                remaining >= self.min_active_ms
            }
        };
        DriveCommand {
            azimuth: requested
                .azimuth
                .filter(|direction| permitted(0, actual.azimuth == Some(*direction))),
            elevation: requested
                .elevation
                .filter(|direction| permitted(1, actual.elevation == Some(*direction))),
        }
    }

    /// Record activation at its actual instant, even before any on-time accrues.
    pub(crate) fn applied(&mut self, output: DriveCommand) {
        for (axis, active) in self
            .axes
            .iter_mut()
            .zip([output.azimuth.is_some(), output.elevation.is_some()])
        {
            if active {
                axis.idle_ms = 0;
            }
        }
    }

    pub(crate) const fn status(&self) -> DutyStatus {
        DutyStatus {
            azimuth: self.axes[0].status(self.config, self.min_active_ms),
            elevation: self.axes[1].status(self.config, self.min_active_ms),
        }
    }

    pub(crate) fn deadline_ms(&self, actual: DriveCommand, now_ms: u64) -> Option<u64> {
        let observed_ms = self.observed_ms?;
        self.axes
            .iter()
            .zip([actual.azimuth.is_some(), actual.elevation.is_some()])
            .filter_map(|(axis, active)| {
                let remaining = axis.remaining(self.config);
                let delay = if active {
                    remaining
                } else if remaining < self.min_active_ms {
                    self.config.rest_ms.saturating_sub(axis.idle_ms)
                } else {
                    return None;
                };
                observed_ms
                    .checked_add(delay)
                    .map(|deadline| deadline.max(now_ms))
            })
            .min()
    }
}
