//! Drive-command ownership, independent of the dashboard transport and runtime.
//!
//! A newly connected client gets manual control only when no owner exists.
//! Takeover and owner disconnect return an inhibit command, so the caller can
//! revoke movement belonging to the old owner in the same transaction. Browser
//! movement is operator-held [`Command::Jog`]: releasing one control stops
//! that axis immediately, just as releasing every control does.

use crate::drive::Command;

/// Allocation and routing policy for manual and autonomous drive commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlAuthority {
    next_client_id: u32,
    controller_id: Option<u32>,
    autonomous: bool,
}

impl Default for ControlAuthority {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlAuthority {
    /// Start in manual mode without a connected controller.
    pub const fn new() -> Self {
        Self {
            next_client_id: 1,
            controller_id: None,
            autonomous: false,
        }
    }

    /// Allocate a nonzero connection id and grant unclaimed manual control.
    pub fn register_client(&mut self) -> u32 {
        let id = self.next_client_id;
        self.next_client_id = id.wrapping_add(1).max(1);
        if !self.autonomous && self.controller_id.is_none() {
            self.controller_id = Some(id);
        }
        id
    }

    /// Whether this connection owns the manual command stream.
    pub fn is_controller(&self, client_id: u32) -> bool {
        !self.autonomous && self.controller_id == Some(client_id)
    }

    /// Change command source, clearing manual ownership and stale movement.
    pub fn set_autonomous(&mut self, autonomous: bool) -> Command {
        self.autonomous = autonomous;
        self.controller_id = None;
        Command::Inhibit
    }

    /// Forget manual ownership after a network interruption. Keeping the id
    /// allocator and selected mode prevents old sockets from silently becoming
    /// owners again when connectivity returns.
    pub fn revoke_manual(&mut self) {
        self.controller_id = None;
    }

    /// Transfer manual control and revoke the previous owner's drive request.
    pub fn take_control(&mut self, client_id: u32) -> Option<Command> {
        if self.autonomous || client_id == 0 {
            return None;
        }
        self.controller_id = Some(client_id);
        Some(Command::Inhibit)
    }

    /// Revoke movement only if the disconnected connection still owns it.
    pub fn release_client(&mut self, client_id: u32) -> Option<Command> {
        if !self.is_controller(client_id) {
            return None;
        }
        self.controller_id = None;
        Some(Command::Inhibit)
    }

    /// Route a command only from the current manual controller. Browser
    /// movement is operator-held, and a browser stop is an immediate stop.
    pub fn accept_command(&self, client_id: u32, command: Command) -> Option<Command> {
        self.is_controller(client_id).then_some(match command {
            Command::Drive(drive) | Command::Jog(drive) => Command::Jog(drive),
            Command::Stop | Command::Inhibit => Command::Inhibit,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive::{AzimuthDirection, DriveCommand};

    const MOVE: Command = Command::Drive(DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: None,
    });
    const JOG: Command = Command::Jog(DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: None,
    });

    #[test]
    fn first_client_owns_control_and_other_clients_are_viewers() {
        let mut authority = ControlAuthority::new();
        let first = authority.register_client();
        let second = authority.register_client();
        assert_ne!(first, second);
        assert_eq!(authority.accept_command(first, MOVE), Some(JOG));
        assert_eq!(authority.accept_command(second, MOVE), None);
        assert_eq!(
            authority.accept_command(first, Command::Stop),
            Some(Command::Inhibit)
        );
        assert!(!authority.is_controller(0));
        assert_eq!(authority.take_control(0), None);
        assert!(authority.is_controller(first));
    }

    #[test]
    fn takeover_stops_movement_and_rejects_old_owner_refreshes() {
        let mut authority = ControlAuthority::new();
        let old_owner = authority.register_client();
        let new_owner = authority.register_client();
        assert_eq!(authority.accept_command(old_owner, MOVE), Some(JOG));
        assert_eq!(authority.take_control(new_owner), Some(Command::Inhibit));
        assert_eq!(authority.accept_command(old_owner, MOVE), None);
        assert_eq!(authority.accept_command(old_owner, Command::Stop), None);
        assert_eq!(authority.accept_command(new_owner, MOVE), Some(JOG));
        // An old socket closing after takeover must not revoke the new owner.
        assert_eq!(authority.release_client(old_owner), None);
        assert!(authority.is_controller(new_owner));
    }

    #[test]
    fn owner_disconnect_stops_without_silently_promoting_a_viewer() {
        let mut authority = ControlAuthority::new();
        let owner = authority.register_client();
        let viewer = authority.register_client();
        assert_eq!(authority.release_client(viewer), None);
        assert_eq!(authority.release_client(owner), Some(Command::Inhibit));
        assert_eq!(authority.accept_command(owner, MOVE), None);
        assert_eq!(authority.accept_command(viewer, MOVE), None);
        assert_eq!(authority.take_control(viewer), Some(Command::Inhibit));
        assert_eq!(authority.accept_command(viewer, MOVE), Some(JOG));
    }

    #[test]
    fn autonomous_mode_excludes_all_browser_commands_and_takeovers() {
        let mut authority = ControlAuthority::new();
        let previous_owner = authority.register_client();
        assert_eq!(authority.set_autonomous(true), Command::Inhibit);
        let new_client = authority.register_client();
        for client in [previous_owner, new_client] {
            assert!(!authority.is_controller(client));
            assert_eq!(authority.accept_command(client, MOVE), None);
            assert_eq!(authority.accept_command(client, Command::Stop), None);
            assert_eq!(authority.take_control(client), None);
            assert_eq!(authority.release_client(client), None);
        }
        assert_eq!(authority.set_autonomous(false), Command::Inhibit);
        assert_eq!(authority.accept_command(previous_owner, MOVE), None);
        assert_eq!(authority.take_control(new_client), Some(Command::Inhibit));
        assert_eq!(authority.accept_command(new_client, MOVE), Some(JOG));
    }

    #[test]
    fn allocation_never_uses_zero_when_counter_wraps() {
        let mut authority = ControlAuthority::new();
        authority.next_client_id = u32::MAX;
        assert_eq!(authority.register_client(), u32::MAX);
        assert_eq!(authority.register_client(), 1);
    }

    #[test]
    fn network_revocation_requires_explicit_takeover_after_recovery() {
        let mut authority = ControlAuthority::new();
        let owner = authority.register_client();
        authority.revoke_manual();
        assert_eq!(authority.accept_command(owner, MOVE), None);
        assert_eq!(authority.take_control(owner), Some(Command::Inhibit));
        assert_eq!(authority.accept_command(owner, MOVE), Some(JOG));
        authority.set_autonomous(true);
        authority.revoke_manual();
        assert_eq!(authority.take_control(owner), None);
    }
}
