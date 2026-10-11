//! Exclusive operator leases, independent of device-owned tracking execution.
//! Transport supplies a nonzero connection identity and authenticates its use.
//! A lease is ownership/freshness policy, not an authentication credential.

/// Provisional browser ownership lifetime; heartbeat transport is added with the UI.
pub const OPERATOR_LEASE_MS: u64 = 5_000;

/// A grant bound to both its connection and a never-reused generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperatorToken {
    client_id: u64,
    generation: u64,
}

impl OperatorToken {
    /// Server-allocated connection identity; never take this from browser input.
    pub const fn client_id(self) -> u64 {
        self.client_id
    }
    /// Generation to bind every operator mutation and renewal to its grant.
    pub const fn generation(self) -> u64 {
        self.generation
    }
}

/// Current grant and its exclusive monotonic expiry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperatorGrant {
    /// Identity required by mutations, renewals and releases.
    pub token: OperatorToken,
    /// The token is invalid at this instant, even if expiry has not been serviced.
    pub expires_at_ms: u64,
}

/// Why ownership was not granted or renewed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseError {
    /// Another connection still has a valid lease.
    Occupied,
    /// Zero connection identity, expired grant or wrong generation.
    Invalid,
    /// Time went backwards or a counter/deadline cannot be represented.
    ClockOrCounter,
}

/// One lease owner. Reads never renew or mutate permission. Grants never wrap.
#[derive(Debug, Default)]
pub struct OperatorLease {
    grant: Option<OperatorGrant>,
    generation: u64,
    observed_ms: Option<u64>,
    failed: bool,
}

impl OperatorLease {
    /// Boot without an owner.
    pub const fn new() -> Self {
        Self {
            grant: None,
            generation: 0,
            observed_ms: None,
            failed: false,
        }
    }

    /// Expire before processing input; returns whether ownership was removed.
    pub fn advance(&mut self, now_ms: u64) -> Result<bool, LeaseError> {
        if self.failed || self.observed_ms.is_some_and(|last| now_ms < last) {
            self.grant = None;
            self.failed = true;
            return Err(LeaseError::ClockOrCounter);
        }
        self.observed_ms = Some(now_ms);
        if self
            .grant
            .is_some_and(|grant| now_ms >= grant.expires_at_ms)
        {
            self.grant = None;
            return Ok(true);
        }
        Ok(false)
    }

    /// Snapshot valid at the supplied instant; reading does not extend it.
    pub fn current(&self, now_ms: u64) -> Option<OperatorGrant> {
        if self.failed || self.observed_ms.is_some_and(|last| now_ms < last) {
            return None;
        }
        self.grant.filter(|grant| now_ms < grant.expires_at_ms)
    }

    /// Check the original connection/generation without renewing its expiry.
    pub fn permits(&self, token: OperatorToken, now_ms: u64) -> bool {
        self.current(now_ms)
            .is_some_and(|grant| grant.token == token)
    }

    /// Claim unowned control. A repeated claim by its owner returns the same
    /// grant and deadline; a viewer cannot displace a valid grant.
    pub fn claim(&mut self, client_id: u64, now_ms: u64) -> Result<OperatorGrant, LeaseError> {
        self.advance(now_ms)?;
        if client_id == 0 {
            return Err(LeaseError::Invalid);
        }
        if let Some(grant) = self.grant {
            return if grant.token.client_id == client_id {
                Ok(grant)
            } else {
                Err(LeaseError::Occupied)
            };
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(LeaseError::ClockOrCounter)?;
        let expires_at_ms = now_ms
            .checked_add(OPERATOR_LEASE_MS)
            .ok_or(LeaseError::ClockOrCounter)?;
        self.generation = generation;
        let grant = OperatorGrant {
            token: OperatorToken {
                client_id,
                generation,
            },
            expires_at_ms,
        };
        self.grant = Some(grant);
        Ok(grant)
    }

    /// Only a still-current grant can heartbeat; an expired heartbeat cannot reacquire.
    pub fn renew(
        &mut self,
        token: OperatorToken,
        now_ms: u64,
    ) -> Result<OperatorGrant, LeaseError> {
        self.advance(now_ms)?;
        if !self.permits(token, now_ms) {
            return Err(LeaseError::Invalid);
        }
        let Some(expires_at_ms) = now_ms.checked_add(OPERATOR_LEASE_MS) else {
            self.grant = None;
            self.failed = true;
            return Err(LeaseError::ClockOrCounter);
        };
        let grant = OperatorGrant {
            token,
            expires_at_ms,
        };
        self.grant = Some(grant);
        Ok(grant)
    }

    /// Release exactly this grant. Closing an old socket cannot release a successor.
    pub fn release(&mut self, token: OperatorToken, now_ms: u64) -> Result<bool, LeaseError> {
        self.advance(now_ms)?;
        if !self.permits(token, now_ms) {
            return Ok(false);
        }
        self.grant = None;
        Ok(true)
    }
}
