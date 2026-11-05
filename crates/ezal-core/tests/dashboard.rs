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
    assert!(INDEX_HTML.contains(TAKE_CONTROL_MESSAGE));
}
