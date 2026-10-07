//! The dashboard single-page asset served at `/`.
//!
//! One self-contained HTML document — markup, styles, and the WebSocket
//! client — kept apart from the wire protocol in [`crate::protocol`] so the
//! dashboard asset does not bury the parsing and framing logic. The firmware's
//! `/` route serves [`INDEX_HTML`] verbatim.
//!
//! The page is held in a sibling `dashboard.html` file (rather than an inline
//! string literal) so editors give it real HTML/CSS/JS tooling. The client's
//! `refreshMs` must stay below [`COMMAND_TIMEOUT_MS`](crate::protocol::COMMAND_TIMEOUT_MS),
//! and its telemetry watchdog matches [`COMMAND_MAX_AGE_MS`](crate::protocol::COMMAND_MAX_AGE_MS).
//! The `command_refresh_stays_inside_firmware_lease` test pins down both values.

/// The inline dashboard served at `/`.
pub const INDEX_HTML: &str = include_str!("dashboard.html");
