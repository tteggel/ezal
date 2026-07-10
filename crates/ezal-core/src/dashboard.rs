//! The dashboard single-page asset served at `/`.
//!
//! One self-contained HTML document — markup, styles, and the WebSocket
//! client — kept apart from the wire protocol in [`crate::protocol`] so the
//! ~400-line asset does not bury the parsing and framing logic. The firmware's
//! `/` route serves [`INDEX_HTML`] verbatim.
//!
//! The page is held in a sibling `dashboard.html` file (rather than an inline
//! string literal) so editors give it real HTML/CSS/JS tooling. One value is
//! duplicated by hand: the client's `refreshMs` must stay below
//! [`COMMAND_TIMEOUT_MS`](crate::protocol::COMMAND_TIMEOUT_MS), a coupling the
//! `command_refresh_stays_inside_firmware_lease` test pins down.

/// The inline dashboard served at `/`.
pub const INDEX_HTML: &str = include_str!("dashboard.html");
