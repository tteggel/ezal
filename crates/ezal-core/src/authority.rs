//! Connection identity and exclusive manual ownership.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ControlAuthority {
    next_client_id: u32,
    controller_id: Option<u32>,
}

impl ControlAuthority {
    pub const fn new() -> Self {
        Self {
            next_client_id: 1,
            controller_id: None,
        }
    }

    pub fn register_client(&mut self) -> u32 {
        let id = self.next_client_id;
        self.next_client_id = id.wrapping_add(1).max(1);
        if self.controller_id.is_none() {
            self.controller_id = Some(id);
        }
        id
    }

    pub fn is_controller(&self, client_id: u32) -> bool {
        self.controller_id == Some(client_id)
    }

    pub fn revoke_manual(&mut self) {
        self.controller_id = None;
    }

    pub fn take_control(&mut self, client_id: u32) -> bool {
        if client_id == 0 {
            return false;
        }
        self.controller_id = Some(client_id);
        true
    }

    pub fn release_client(&mut self, client_id: u32) -> bool {
        if !self.is_controller(client_id) {
            return false;
        }
        self.controller_id = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_never_uses_zero_when_counter_wraps() {
        let mut authority = ControlAuthority::new();
        authority.next_client_id = u32::MAX;
        assert_eq!(authority.register_client(), u32::MAX);
        assert_eq!(authority.register_client(), 1);
        assert!(!authority.take_control(0));
    }
}
