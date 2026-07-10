//! Tests for the static dashboard asset.

use ezal_core::dashboard::INDEX_HTML;
use ezal_core::protocol::{TAKE_CONTROL_MESSAGE, WS_PATH};

#[test]
fn dashboard_uses_the_firmware_websocket_path() {
    assert_eq!(WS_PATH, "/ws");
    assert!(INDEX_HTML.contains("new WebSocket"));
    assert!(INDEX_HTML.contains("`${proto}://${location.host}/ws`"));
    assert!(INDEX_HTML.contains(TAKE_CONTROL_MESSAGE));
    assert!(INDEX_HTML.contains(r#"id="take""#));
    assert!(INDEX_HTML.contains(r#"message.type === "control""#));
    assert!(INDEX_HTML.contains("drive[meta.axis] === cmd"));
    assert!(INDEX_HTML.contains("axisPointers[meta.axis] !== null"));
    assert!(INDEX_HTML.contains("axisPointers[claim.axis] = null"));
    assert!(INDEX_HTML.contains(".dir:active:not(.active)"));
}

#[test]
fn dashboard_embeds_a_favicon() {
    assert!(INDEX_HTML.contains(r#"<link rel="icon" href="data:image/svg+xml,"#));
    assert!(INDEX_HTML.contains("viewBox='0 0 64 64'"));
}
