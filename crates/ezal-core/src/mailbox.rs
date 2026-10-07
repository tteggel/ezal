//! Pending actuator commands, preserving source deadlines and inhibition.

use crate::drive::Command;

/// A request and the original exclusive expiry assigned at admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandEnvelope {
    /// Operator jog or immediate safety inhibition.
    pub command: Command,
    /// Monotonic source deadline; unused for inhibition.
    pub valid_until_ms: u64,
}

impl CommandEnvelope {
    /// Immediate inhibition has no movement lease.
    pub const INHIBIT: Self = Self {
        command: Command::Inhibit,
        valid_until_ms: 0,
    };
}

/// One pending request; unconsumed inhibition cannot be overwritten.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandMailbox {
    pending: Option<CommandEnvelope>,
}

impl CommandMailbox {
    /// Create an empty mailbox.
    pub const fn new() -> Self {
        Self { pending: None }
    }

    /// Replace the whole envelope unless inhibition is awaiting consumption.
    pub fn submit(&mut self, envelope: CommandEnvelope) {
        if !self
            .pending
            .is_some_and(|pending| pending.command == Command::Inhibit)
        {
            self.pending = Some(envelope);
        }
    }

    /// Consume the stored request and its original deadline together.
    pub fn take(&mut self) -> Option<CommandEnvelope> {
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
    use crate::drive::{AzimuthDirection, DriveCommand};

    fn movement(deadline: u64) -> CommandEnvelope {
        CommandEnvelope {
            command: Command::Jog(DriveCommand {
                azimuth: Some(AzimuthDirection::Clockwise),
                elevation: None,
            }),
            valid_until_ms: deadline,
        }
    }

    #[test]
    fn inhibition_keeps_its_entire_envelope_until_consumed() {
        let mut mailbox = CommandMailbox::new();
        mailbox.submit(movement(500));
        mailbox.submit(CommandEnvelope::INHIBIT);
        mailbox.submit(movement(900));
        assert_eq!(mailbox.take(), Some(CommandEnvelope::INHIBIT));
        assert_eq!(mailbox.take(), None);
        mailbox.submit(movement(901));
        assert_eq!(mailbox.take(), Some(movement(901)));
    }

    #[test]
    fn replacement_retains_the_new_sources_exact_deadline() {
        let mut mailbox = CommandMailbox::new();
        mailbox.submit(movement(500));
        mailbox.submit(movement(400));
        assert_eq!(mailbox.take(), Some(movement(400)));
    }
}
