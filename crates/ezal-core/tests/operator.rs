use ezal_core::operator::{LeaseError, OperatorLease, OPERATOR_LEASE_MS};

#[test]
fn occupied_lease_cannot_be_taken_and_reads_or_repeated_claims_do_not_renew() {
    let mut lease = OperatorLease::new();
    let grant = lease.claim(11, 100).unwrap();
    assert_eq!(lease.claim(22, 101), Err(LeaseError::Occupied));
    assert_eq!(lease.claim(11, 300), Ok(grant));
    assert_eq!(lease.current(5100), None);
    assert_eq!(lease.current(5099), Some(grant)); // a read did not expire state
    assert_eq!(lease.renew(grant.token, 5100), Err(LeaseError::Invalid));
    let next = lease.claim(22, 5100).unwrap();
    assert_ne!(next.token.generation(), grant.token.generation());
    assert!(!lease.release(grant.token, 5101).unwrap());
    assert_eq!(lease.current(5101), Some(next));
}

#[test]
fn renewal_keeps_generation_but_release_and_reacquisition_never_reuse_it() {
    let mut lease = OperatorLease::new();
    let first = lease.claim(1, 0).unwrap();
    let renewed = lease.renew(first.token, 4000).unwrap();
    assert_eq!(renewed.token, first.token);
    assert_eq!(renewed.expires_at_ms, 4000 + OPERATOR_LEASE_MS);
    assert!(lease.release(first.token, 4100).unwrap());
    let second = lease.claim(1, 4100).unwrap();
    assert_ne!(second.token, first.token);
    assert_eq!(lease.renew(first.token, 4100), Err(LeaseError::Invalid));
    assert_eq!(lease.claim(0, 4100), Err(LeaseError::Invalid));
}

#[test]
fn backwards_time_and_deadline_overflow_never_leave_an_immortal_owner() {
    let mut lease = OperatorLease::new();
    let grant = lease.claim(1, 1000).unwrap();
    assert_eq!(
        lease.renew(grant.token, 999),
        Err(LeaseError::ClockOrCounter)
    );
    assert!(lease.current(1000).is_none());
    assert_eq!(lease.claim(2, 1001), Err(LeaseError::ClockOrCounter));
    let mut lease = OperatorLease::new();
    assert_eq!(
        lease.claim(1, u64::MAX - 100),
        Err(LeaseError::ClockOrCounter)
    );
    assert!(lease.current(u64::MAX - 100).is_none());
}
