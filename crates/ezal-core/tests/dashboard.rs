//! Tests for the static dashboard asset.

use ezal_core::dashboard::{CONFIG_PATH, INDEX_HTML};
use ezal_core::protocol::TAKE_CONTROL_MESSAGE;

#[test]
fn dashboard_loads_firmware_configuration_before_the_client() {
    let config = INDEX_HTML
        .find(&format!("<script src=\"{CONFIG_PATH}\"></script>"))
        .unwrap();
    let client = INDEX_HTML.find("<script>").unwrap();
    assert!(config < client);
    assert!(INDEX_HTML.contains("new WebSocket"));
    assert!(INDEX_HTML.contains(TAKE_CONTROL_MESSAGE));
    assert!(INDEX_HTML.contains(r#"id="take""#));
}

#[test]
fn held_direction_buttons_have_feedback_separate_from_applied_output() {
    // The behavioural tests drive a DOM without CSS. Input feedback must
    // have its own style without overriding the firmware's active output.
    assert!(INDEX_HTML.contains(".dir.held:not(.active)"));
    assert!(INDEX_HTML.contains(".dir.active"));
}

#[test]
fn dashboard_embeds_a_favicon() {
    assert!(INDEX_HTML.contains(r#"<link rel="icon" href="data:image/svg+xml,"#));
    assert!(INDEX_HTML.contains("viewBox='0 0 64 64'"));
}
