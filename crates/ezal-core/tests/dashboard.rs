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
fn pressed_direction_buttons_only_look_active_when_the_firmware_agrees() {
    // The behavioural tests drive a DOM without CSS, so assert the rule that
    // keeps a held button looking idle until telemetry reports that axis
    // driving. Without it the operator could believe a refused command ran.
    assert!(INDEX_HTML.contains(".dir:active:not(.active)"));
}

#[test]
fn dashboard_embeds_a_favicon() {
    assert!(INDEX_HTML.contains(r#"<link rel="icon" href="data:image/svg+xml,"#));
    assert!(INDEX_HTML.contains("viewBox='0 0 64 64'"));
}
