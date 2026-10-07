//! Physical-policy fixtures use the production step with healthy networking.

use super::{Actuator, ActuatorInput};
use crate::drive::Command;
use crate::interlock::FeedbackInterlock;
use crate::mailbox::CommandEnvelope;
use crate::network::NetworkGate;

#[path = "../../tests/common/mod.rs"]
mod common;
mod interlock;
mod motion;

impl Actuator {
    fn test_step(
        &mut self,
        interlock: &FeedbackInterlock,
        command: Option<CommandEnvelope>,
        now_ms: u64,
    ) -> bool {
        let before = self.actual();
        let mut network = NetworkGate::new();
        network.observe(now_ms, true);
        self.step(
            ActuatorInput {
                interlock: *interlock,
                network,
                command,
            },
            now_ms,
        )
        .output
            != before
    }

    fn service(&mut self, interlock: &FeedbackInterlock, now_ms: u64) -> bool {
        self.test_step(interlock, None, now_ms)
    }

    fn command(&mut self, command: Command, interlock: &FeedbackInterlock, now_ms: u64) -> bool {
        self.command_until(command, interlock, now_ms, u64::MAX)
    }

    fn command_until(
        &mut self,
        command: Command,
        interlock: &FeedbackInterlock,
        now_ms: u64,
        valid_until_ms: u64,
    ) -> bool {
        self.test_step(
            interlock,
            Some(CommandEnvelope {
                command,
                valid_until_ms,
            }),
            now_ms,
        )
    }
}
