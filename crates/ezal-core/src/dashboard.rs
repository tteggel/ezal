//! The dashboard single-page asset served at `/`.
//!
//! One HTML document contains markup, styles, and the WebSocket client; a
//! separate script supplies configuration. Both stay apart from the wire
//! protocol in [`crate::protocol`] so the
//! dashboard asset does not bury the parsing and framing logic. The firmware's
//! `/` route serves [`INDEX_HTML`] verbatim and [`CONFIG_PATH`] supplies the
//! client settings from the same constants the firmware uses.
//!
//! The page is held in a sibling `dashboard.html` file (rather than an inline
//! string literal) so editors give it real HTML/CSS/JS tooling.

use core::fmt;

use crate::protocol::WS_PATH;
use crate::session::{COMMAND_MAX_AGE_MS, COMMAND_REFRESH_MS};

/// The inline dashboard served at `/`.
pub const INDEX_HTML: &str = include_str!("dashboard.html");

/// Script containing the browser's firmware-owned transport settings.
pub const CONFIG_PATH: &str = "/dashboard-config.js";

/// The exact JavaScript configuration served to the browser and used in tests.
pub struct ClientConfig;

impl fmt::Display for ClientConfig {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            out,
            "window.EZAL_CONFIG = {{\"wsPath\":\"{}\",\"refreshMs\":{},\"telemetryTimeoutMs\":{},\"feedbackMaxAgeMs\":{}}};",
            WS_PATH, COMMAND_REFRESH_MS, COMMAND_MAX_AGE_MS, crate::feedback::FEEDBACK_MAX_AGE_MS
        )
    }
}
