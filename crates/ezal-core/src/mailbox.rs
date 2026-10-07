//! Pending actuator commands, with safety inhibition preserved until consumed.

use crate::drive::Command;

/// A single pending command whose safety inhibition cannot be overwritten.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandMailbox {
    pending: Option<Command>,
}

impl CommandMailbox {
    /// Create an empty mailbox.
    pub const fn new() -> Self {
        Self { pending: None }
    }

    /// Queue the newest command, retaining an unconsumed safety inhibition.
    ///
    /// A later drive refresh may resume movement only after the actuator has
    /// actually consumed the inhibition and started its minimum inactive time.
    pub fn submit(&mut self, command: Command) {
        if self.pending != Some(Command::Inhibit) {
            self.pending = Some(command);
        }
    }

    /// Consume the current command, if any.
    pub fn take(&mut self) -> Option<Command> {
        self.pending.take()
    }
}

impl Default for CommandMailbox {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive::{ActuatorGuard, AzimuthDirection, DriveCommand, OUTPUT_MIN_INACTIVE_MS};

    const MOVE: Command = Command::Drive(DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: None,
    });

    #[test]
    fn held_refresh_cannot_overwrite_fault_before_actuator_consumes_it() {
        let mut mailbox = CommandMailbox::new();
        let mut guard = ActuatorGuard::new();
        guard.command(MOVE, 0);
        mailbox.submit(Command::Inhibit);
        mailbox.submit(MOVE);
        mailbox.submit(Command::Stop);
        guard.command(mailbox.take().unwrap(), 100);
        assert_eq!(guard.actual(), DriveCommand::IDLE);
        assert_eq!(guard.target(), DriveCommand::IDLE);
        assert_eq!(mailbox.take(), None);

        mailbox.submit(MOVE);
        guard.command(mailbox.take().unwrap(), 101);
        assert_eq!(guard.actual(), DriveCommand::IDLE);
        // A newly accepted lease still cannot skip the inactive relay hold.
        guard.command(MOVE, 100 + OUTPUT_MIN_INACTIVE_MS);
        assert!(guard.actual().is_active());
    }

    #[test]
    fn latest_ordinary_request_replaces_unconsumed_movement() {
        let mut mailbox = CommandMailbox::new();
        mailbox.submit(MOVE);
        mailbox.submit(Command::Stop);
        assert_eq!(mailbox.take(), Some(Command::Stop));
    }
}
