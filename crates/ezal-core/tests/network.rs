//! Network permission observes readiness, expiry, and recovery generations.

use ezal_core::network::{NetworkGate, NETWORK_HEALTH_TIMEOUT_MS};

#[test]
fn boot_is_inhibited_and_positive_observations_have_exact_deadlines() {
    let mut network = NetworkGate::new();
    assert!(!network.ready(0));
    assert_eq!(network.deadline_ms(), None);
    assert!(!network.observe(100, true));
    assert_eq!(network.generation(), 1);
    assert_eq!(network.deadline_ms(), Some(100 + NETWORK_HEALTH_TIMEOUT_MS));
    assert!(network.ready(100 + NETWORK_HEALTH_TIMEOUT_MS - 1));
    assert!(!network.ready(100 + NETWORK_HEALTH_TIMEOUT_MS));
    assert!(!network.ready(99));
    assert!(network.expire(100 + NETWORK_HEALTH_TIMEOUT_MS));
    assert!(!network.expire(100 + NETWORK_HEALTH_TIMEOUT_MS));
}

#[test]
fn late_positive_observation_reports_outage_before_recovering() {
    let mut network = NetworkGate::new();
    network.observe(0, true);
    assert!(!network.observe(100, true));
    assert_eq!(network.generation(), 1);
    assert!(network.observe(100 + NETWORK_HEALTH_TIMEOUT_MS, true));
    assert_eq!(network.generation(), 2);
    assert!(network.ready(100 + NETWORK_HEALTH_TIMEOUT_MS));
}

#[test]
fn unrepresentable_deadline_cannot_grant_permission() {
    let mut network = NetworkGate::new();
    network.observe(u64::MAX, true);
    assert!(!network.ready(u64::MAX));
    assert_eq!(network.deadline_ms(), None);
}
