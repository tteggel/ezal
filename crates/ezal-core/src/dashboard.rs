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

impl ClientConfig {
    /// Include whether this firmware image embeds the optional browser module.
    pub fn with_browser_planner(available: bool) -> impl fmt::Display {
        struct Config(bool);
        impl fmt::Display for Config {
            fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                write_config(out, self.0)
            }
        }
        Config(available)
    }
}

impl fmt::Display for ClientConfig {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_config(out, true)
    }
}

fn write_config(out: &mut fmt::Formatter<'_>, browser_planner: bool) -> fmt::Result {
    write!(
            out,
            "window.EZAL_CONFIG = {{\"wsPath\":\"{}\",\"refreshMs\":{},\"telemetryTimeoutMs\":{},\"planning\":{{\"available\":{},\"candidates\":{},\"inputCapacity\":{},\"chunkSize\":{},\"headerSize\":{}}}}};",
            WS_PATH, COMMAND_REFRESH_MS, COMMAND_MAX_AGE_MS, browser_planner,
            crate::tracking::CANDIDATES, crate::tracking::offload::INPUT_CAPACITY,
            crate::protocol::PLANNING_CHUNK_SIZE, crate::protocol::PLANNING_CHUNK_HEADER_SIZE
        )
}
