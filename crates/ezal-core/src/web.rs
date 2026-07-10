//! Static web UI assets and WebSocket wire protocol.
//!
//! The HTTP server and WebSocket transport live in `ezal-firmware`; this
//! module keeps the hardware-independent pieces here: command names, parser,
//! telemetry framing, and the inline dashboard served to the browser.

use core::fmt;

/// WebSocket route used by the dashboard.
pub const WS_PATH: &str = "/ws";

/// WebSocket message used by a viewer to become the controller.
pub const TAKE_CONTROL_MESSAGE: &str = "control:take";

/// How often the firmware samples feedback and emits position telemetry.
pub const TELEMETRY_PERIOD_MS: u64 = 500;

/// How often the browser repeats a held drive state.
pub const COMMAND_REFRESH_MS: u64 = 150;

/// How long the firmware allows a movement command to live without refresh.
pub const COMMAND_TIMEOUT_MS: u64 = 750;

/// Minimum time a direction output stays active once energised.
pub const OUTPUT_MIN_ACTIVE_MS: u64 = 500;

/// Minimum time an axis stays inactive before it may be energised again.
pub const OUTPUT_MIN_INACTIVE_MS: u64 = OUTPUT_MIN_ACTIVE_MS;

const _: () = assert!(COMMAND_REFRESH_MS < COMMAND_TIMEOUT_MS);
const _: () = assert!(OUTPUT_MIN_ACTIVE_MS <= COMMAND_TIMEOUT_MS);
const _: () = assert!(OUTPUT_MIN_INACTIVE_MS <= COMMAND_TIMEOUT_MS);

/// The inline dashboard served at `/`.
pub const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<title>ezal</title>
<link rel="icon" href="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 64 64'%3E%3Crect width='64' height='64' rx='14' fill='%23172023'/%3E%3Cpath d='M14 38a24 24 0 0 0 34-24A32 32 0 0 0 14 38Z' fill='%23f4f7f8'/%3E%3Cpath d='M18 37a20 20 0 0 0 28-20' fill='none' stroke='%23147a73' stroke-width='4' stroke-linecap='round'/%3E%3Cpath d='M30 42 22 54h20l-8-12' fill='none' stroke='%23f4f7f8' stroke-width='5' stroke-linecap='round' stroke-linejoin='round'/%3E%3Ccircle cx='47' cy='15' r='4' fill='%23df5d43'/%3E%3C/svg%3E">
<style>
:root {
  color-scheme: light;
  --bg: #f4f7f8;
  --ink: #172023;
  --muted: #68777c;
  --panel: #ffffff;
  --line: #d9e1e4;
  --teal: #147a73;
  --teal-dark: #0f5f59;
  --coral: #df5d43;
  --gold: #b88918;
  --shadow: 0 18px 48px rgba(23, 32, 35, 0.14);
}
* { box-sizing: border-box; }
html { min-height: 100%; background: var(--bg); }
body {
  min-height: 100vh;
  margin: 0;
  font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  color: var(--ink);
  user-select: none;
  -webkit-user-select: none;
  -webkit-touch-callout: none;
  background:
    linear-gradient(180deg, rgba(255,255,255,0.74), rgba(244,247,248,0.8)),
    var(--bg);
}
main {
  width: min(100%, 720px);
  min-height: 100vh;
  margin: 0 auto;
  padding: max(18px, env(safe-area-inset-top)) max(16px, env(safe-area-inset-right)) max(18px, env(safe-area-inset-bottom)) max(16px, env(safe-area-inset-left));
  display: grid;
  grid-template-rows: auto auto 1fr;
  gap: 16px;
}
header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}
.top-actions {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 8px;
}
.brand {
  font-size: 1rem;
  font-weight: 800;
  letter-spacing: 0;
  text-transform: uppercase;
}
.status {
  display: inline-flex;
  align-items: center;
  min-height: 32px;
  padding: 0 12px;
  border: 1px solid var(--line);
  border-radius: 999px;
  background: var(--panel);
  color: var(--muted);
  font-size: 0.88rem;
  font-weight: 700;
}
.take {
  min-height: 32px;
  padding: 0 12px;
  border: 1px solid #1d272b;
  border-radius: 999px;
  background: #1d272b;
  color: #fff;
  font-size: 0.82rem;
  font-weight: 800;
  text-transform: uppercase;
  touch-action: manipulation;
  user-select: none;
  -webkit-user-select: none;
  -webkit-touch-callout: none;
}
.take:disabled {
  border-color: var(--line);
  background: var(--panel);
  color: var(--muted);
}
.take:not(:disabled):active {
  background: var(--teal-dark);
}
.status::before {
  content: "";
  width: 8px;
  height: 8px;
  margin-right: 8px;
  border-radius: 50%;
  background: var(--coral);
}
.status.online::before { background: var(--teal); }
.readouts {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px;
}
.meter {
  min-width: 0;
  padding: 16px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--panel);
  box-shadow: 0 8px 28px rgba(23, 32, 35, 0.08);
}
.label {
  display: block;
  color: var(--muted);
  font-size: 0.82rem;
  font-weight: 800;
  text-transform: uppercase;
}
.value-row {
  display: flex;
  align-items: baseline;
  gap: 8px;
  margin-top: 6px;
  white-space: nowrap;
}
.value {
  font-variant-numeric: tabular-nums;
  font-size: 2.15rem;
  line-height: 1;
  font-weight: 850;
}
.unit {
  color: var(--muted);
  font-size: 0.92rem;
  font-weight: 800;
}
.pad-wrap {
  align-self: center;
  justify-self: center;
  width: min(100%, 430px);
  padding: 18px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--panel);
  box-shadow: var(--shadow);
}
.pad {
  display: grid;
  grid-template-columns: repeat(3, minmax(72px, 1fr));
  grid-template-rows: repeat(3, minmax(72px, 1fr));
  gap: 10px;
  aspect-ratio: 1;
}
.dir {
  width: 100%;
  height: 100%;
  border: 0;
  border-radius: 8px;
  background: #1d272b;
  color: #fff;
  font-size: 1rem;
  font-weight: 850;
  letter-spacing: 0;
  box-shadow: inset 0 -4px 0 rgba(0,0,0,0.24);
  touch-action: none;
  user-select: none;
  -webkit-user-select: none;
  -webkit-touch-callout: none;
  -webkit-tap-highlight-color: transparent;
}
.dir:active:not(.active) {
  background: #1d272b;
  transform: none;
  box-shadow: inset 0 -4px 0 rgba(0,0,0,0.24);
}
.dir.active {
  background: #079e91;
  transform: translateY(2px);
  box-shadow:
    inset 0 -2px 0 rgba(0,0,0,0.34),
    0 0 0 4px rgba(20, 122, 115, 0.36),
    0 13px 30px rgba(20, 122, 115, 0.38);
}
.dir:disabled {
  cursor: default;
  opacity: 0.52;
}
.dir:disabled.active {
  opacity: 1;
}
.dir:focus-visible {
  outline: 3px solid var(--gold);
  outline-offset: 3px;
}
.up { grid-column: 2; grid-row: 1; }
.left { grid-column: 1; grid-row: 2; }
.right { grid-column: 3; grid-row: 2; }
.down { grid-column: 2; grid-row: 3; }
.center {
  grid-column: 2;
  grid-row: 2;
  display: grid;
  place-items: center;
  border: 1px solid var(--line);
  border-radius: 8px;
  color: var(--muted);
  font-size: 0.78rem;
  font-weight: 800;
  text-transform: uppercase;
}
@media (max-width: 430px) {
  main { gap: 12px; }
  header { align-items: flex-start; }
  .top-actions {
    flex-direction: column-reverse;
    align-items: flex-end;
  }
  .readouts { grid-template-columns: 1fr; }
  .value { font-size: 1.85rem; }
  .pad-wrap { padding: 12px; }
  .pad {
    grid-template-columns: repeat(3, minmax(64px, 1fr));
    grid-template-rows: repeat(3, minmax(64px, 1fr));
    gap: 8px;
  }
}
</style>
</head>
<body>
<main>
  <header>
    <div class="brand">ezal</div>
    <div class="top-actions">
      <button id="take" class="take" type="button" disabled>viewer</button>
      <div id="status" class="status">offline</div>
    </div>
  </header>
  <section class="readouts" aria-label="Position feedback">
    <div class="meter">
      <span class="label">A0 raw</span>
      <div class="value-row"><span id="a0" class="value">--</span><span class="unit">mV</span></div>
    </div>
    <div class="meter">
      <span class="label">A1 raw</span>
      <div class="value-row"><span id="a1" class="value">--</span><span class="unit">mV</span></div>
    </div>
  </section>
  <section class="pad-wrap" aria-label="Direction controls">
    <div class="pad">
      <button class="dir up" data-cmd="up" aria-label="Move up">UP</button>
      <button class="dir left" data-cmd="ccw" aria-label="Move counter-clockwise">CCW</button>
      <div class="center" aria-hidden="true">idle</div>
      <button class="dir right" data-cmd="cw" aria-label="Move clockwise">CW</button>
      <button class="dir down" data-cmd="down" aria-label="Move down">DOWN</button>
    </div>
  </section>
</main>
<script>
(() => {
  // Must stay below COMMAND_TIMEOUT_MS so a held control keeps its firmware lease alive.
  const refreshMs = 150;
  const status = document.getElementById("status");
  const take = document.getElementById("take");
  const a0 = document.getElementById("a0");
  const a1 = document.getElementById("a1");
  const buttons = Array.from(document.querySelectorAll(".dir"));
  const commands = {
    cw: { axis: "azimuth" },
    ccw: { axis: "azimuth" },
    up: { axis: "elevation" },
    down: { axis: "elevation" }
  };
  let ws;
  let retry = 400;
  let controller = false;
  let desired = { azimuth: null, elevation: null };
  let drive = { azimuth: null, elevation: null };
  const pointers = new Map();
  const axisPointers = { azimuth: null, elevation: null };
  let repeat = 0;

  const setStatus = (text, online) => {
    status.textContent = text;
    status.classList.toggle("online", online);
  };

  const send = (message) => {
    if (ws && ws.readyState === WebSocket.OPEN) ws.send(message);
  };

  const hasDesired = () => desired.azimuth || desired.elevation;

  const command = () => {
    const parts = [];
    if (desired.azimuth) parts.push(desired.azimuth);
    if (desired.elevation) parts.push(desired.elevation);
    return parts.length ? `drive:${parts.join("+")}` : "stop";
  };

  const syncButtons = () => {
    buttons.forEach((button) => {
      const cmd = button.dataset.cmd;
      const meta = commands[cmd];
      button.disabled = !controller;
      button.classList.toggle("active", Boolean(meta && drive[meta.axis] === cmd));
    });
  };

  const syncControl = () => {
    take.disabled = !ws || ws.readyState !== WebSocket.OPEN || controller;
    take.textContent = controller ? "controller" : "take control";
    syncButtons();
  };

  const syncRepeat = () => {
    if (controller && hasDesired() && !repeat) {
      repeat = setInterval(() => send(command()), refreshMs);
    } else if ((!controller || !hasDesired()) && repeat) {
      clearInterval(repeat);
      repeat = 0;
    }
  };

  const publish = () => {
    syncRepeat();
    if (controller) send(command());
  };

  const stopAll = () => {
    const wasActive = hasDesired();
    desired.azimuth = null;
    desired.elevation = null;
    pointers.clear();
    axisPointers.azimuth = null;
    axisPointers.elevation = null;
    syncRepeat();
    if (controller && wasActive) send("stop");
  };

  const applyControl = (message) => {
    controller = Boolean(message.controller);
    drive.azimuth = message.azimuth || null;
    drive.elevation = message.elevation || null;
    if (!controller) {
      desired.azimuth = null;
      desired.elevation = null;
      pointers.clear();
      axisPointers.azimuth = null;
      axisPointers.elevation = null;
    }
    syncRepeat();
    syncControl();
  };

  const start = (button, pointerId) => {
    if (!controller) return false;
    const cmd = button.dataset.cmd;
    const meta = commands[cmd];
    if (!meta || axisPointers[meta.axis] !== null) return false;
    axisPointers[meta.axis] = pointerId;
    pointers.set(pointerId, { axis: meta.axis, cmd });
    desired[meta.axis] = cmd;
    publish();
    return true;
  };

  const release = (pointerId) => {
    const claim = pointers.get(pointerId);
    if (!claim) return;
    pointers.delete(pointerId);
    if (axisPointers[claim.axis] === pointerId) axisPointers[claim.axis] = null;
    if (!controller) return;
    if (desired[claim.axis] === claim.cmd) desired[claim.axis] = null;
    publish();
  };

  const connect = () => {
    const proto = location.protocol === "https:" ? "wss" : "ws";
    ws = new WebSocket(`${proto}://${location.host}/ws`);
    ws.addEventListener("open", () => {
      retry = 400;
      setStatus("online", true);
      syncControl();
    });
    ws.addEventListener("message", (event) => {
      try {
        const message = JSON.parse(event.data);
        if (message.type === "position") {
          a0.textContent = message.a0_mv;
          a1.textContent = message.a1_mv;
        } else if (message.type === "control") {
          applyControl(message);
        }
      } catch (_) {}
    });
    ws.addEventListener("close", () => {
      stopAll();
      controller = false;
      drive.azimuth = null;
      drive.elevation = null;
      syncControl();
      setStatus("offline", false);
      const wait = retry;
      retry = Math.min(retry * 1.6, 5000);
      setTimeout(connect, wait);
    });
  };

  take.addEventListener("click", () => send("control:take"));
  buttons.forEach((button) => {
    button.addEventListener("pointerdown", (event) => {
      event.preventDefault();
      if (!start(button, event.pointerId)) return;
      button.setPointerCapture(event.pointerId);
    });
    const end = (event) => {
      release(event.pointerId);
    };
    button.addEventListener("pointerup", end);
    button.addEventListener("pointercancel", end);
    button.addEventListener("lostpointercapture", end);
  });
  window.addEventListener("blur", stopAll);
  window.addEventListener("beforeunload", stopAll);
  connect();
})();
</script>
</body>
</html>"#;

/// Azimuth motion carried by the dashboard WebSocket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AzimuthDirection {
    /// Clockwise azimuth movement.
    Clockwise,
    /// Counter-clockwise azimuth movement.
    CounterClockwise,
}

/// Elevation motion carried by the dashboard WebSocket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElevationDirection {
    /// Upward elevation movement.
    Up,
    /// Downward elevation movement.
    Down,
}

/// Simultaneous drive state for the two independent rotator axes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DriveCommand {
    /// Azimuth direction, or `None` when azimuth is idle.
    pub azimuth: Option<AzimuthDirection>,
    /// Elevation direction, or `None` when elevation is idle.
    pub elevation: Option<ElevationDirection>,
}

impl DriveCommand {
    /// No axis is moving.
    pub const IDLE: Self = Self {
        azimuth: None,
        elevation: None,
    };

    /// Whether either axis is moving.
    pub const fn is_active(&self) -> bool {
        self.azimuth.is_some() || self.elevation.is_some()
    }

    /// Browser token for the azimuth direction.
    pub const fn azimuth_token(&self) -> Option<&'static str> {
        match self.azimuth {
            Some(AzimuthDirection::Clockwise) => Some("cw"),
            Some(AzimuthDirection::CounterClockwise) => Some("ccw"),
            None => None,
        }
    }

    /// Browser token for the elevation direction.
    pub const fn elevation_token(&self) -> Option<&'static str> {
        match self.elevation {
            Some(ElevationDirection::Up) => Some("up"),
            Some(ElevationDirection::Down) => Some("down"),
            None => None,
        }
    }
}

/// Minimum active/inactive-time filter for the four physical direction outputs.
///
/// The dashboard command stream is intentionally responsive, but the relay and
/// geartrain should not see sub-500 ms on/off or off/on chatter. This pure
/// state machine lets firmware enforce that rule at the GPIO boundary while
/// tests exercise the timing behaviour on the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DriveDebouncer {
    target: DriveCommand,
    actual: DriveCommand,
    cw_until_ms: u64,
    ccw_until_ms: u64,
    azimuth_off_until_ms: u64,
    up_until_ms: u64,
    down_until_ms: u64,
    elevation_off_until_ms: u64,
}

impl DriveDebouncer {
    /// Create an idle debouncer.
    pub const fn new() -> Self {
        Self {
            target: DriveCommand::IDLE,
            actual: DriveCommand::IDLE,
            cw_until_ms: 0,
            ccw_until_ms: 0,
            azimuth_off_until_ms: 0,
            up_until_ms: 0,
            down_until_ms: 0,
            elevation_off_until_ms: 0,
        }
    }

    /// The currently requested drive state.
    pub const fn target(&self) -> DriveCommand {
        self.target
    }

    /// The drive state that is allowed to be physically applied now.
    pub const fn actual(&self) -> DriveCommand {
        self.actual
    }

    /// Set a new target command and advance the debounced output state.
    ///
    /// Returns `true` when [`actual`](Self::actual) changed.
    pub fn set_target(&mut self, command: Command, now_ms: u64) -> bool {
        self.target = match command {
            Command::Drive(drive) => drive,
            Command::Stop => DriveCommand::IDLE,
        };
        self.update(now_ms)
    }

    /// Advance the debounced output state at `now_ms`.
    ///
    /// Returns `true` when [`actual`](Self::actual) changed.
    pub fn update(&mut self, now_ms: u64) -> bool {
        let before = self.actual;
        self.update_azimuth(now_ms);
        self.update_elevation(now_ms);
        self.actual != before
    }

    /// Next millisecond instant where a delayed target may become applicable.
    pub fn next_transition_ms(&self, now_ms: u64) -> Option<u64> {
        let mut next = None;

        if self.actual.azimuth != self.target.azimuth {
            let deadline = match self.actual.azimuth {
                Some(direction) => self.azimuth_until_ms(direction),
                None => self.azimuth_off_until_ms,
            };
            next = Some(min_option(next, deadline.max(now_ms)));
        }

        if self.actual.elevation != self.target.elevation {
            let deadline = match self.actual.elevation {
                Some(direction) => self.elevation_until_ms(direction),
                None => self.elevation_off_until_ms,
            };
            next = Some(min_option(next, deadline.max(now_ms)));
        }

        next
    }

    fn update_azimuth(&mut self, now_ms: u64) {
        if self.actual.azimuth == self.target.azimuth {
            return;
        }

        if let Some(current) = self.actual.azimuth {
            if now_ms < self.azimuth_until_ms(current) {
                return;
            }
            self.actual.azimuth = None;
            self.azimuth_off_until_ms = now_ms.saturating_add(OUTPUT_MIN_INACTIVE_MS);
        }

        if now_ms < self.azimuth_off_until_ms {
            return;
        }

        if let Some(target) = self.target.azimuth {
            self.actual.azimuth = Some(target);
            self.set_azimuth_until_ms(target, now_ms.saturating_add(OUTPUT_MIN_ACTIVE_MS));
        }
    }

    fn update_elevation(&mut self, now_ms: u64) {
        if self.actual.elevation == self.target.elevation {
            return;
        }

        if let Some(current) = self.actual.elevation {
            if now_ms < self.elevation_until_ms(current) {
                return;
            }
            self.actual.elevation = None;
            self.elevation_off_until_ms = now_ms.saturating_add(OUTPUT_MIN_INACTIVE_MS);
        }

        if now_ms < self.elevation_off_until_ms {
            return;
        }

        if let Some(target) = self.target.elevation {
            self.actual.elevation = Some(target);
            self.set_elevation_until_ms(target, now_ms.saturating_add(OUTPUT_MIN_ACTIVE_MS));
        }
    }

    const fn azimuth_until_ms(&self, direction: AzimuthDirection) -> u64 {
        match direction {
            AzimuthDirection::Clockwise => self.cw_until_ms,
            AzimuthDirection::CounterClockwise => self.ccw_until_ms,
        }
    }

    fn set_azimuth_until_ms(&mut self, direction: AzimuthDirection, until_ms: u64) {
        match direction {
            AzimuthDirection::Clockwise => self.cw_until_ms = until_ms,
            AzimuthDirection::CounterClockwise => self.ccw_until_ms = until_ms,
        }
    }

    const fn elevation_until_ms(&self, direction: ElevationDirection) -> u64 {
        match direction {
            ElevationDirection::Up => self.up_until_ms,
            ElevationDirection::Down => self.down_until_ms,
        }
    }

    fn set_elevation_until_ms(&mut self, direction: ElevationDirection, until_ms: u64) {
        match direction {
            ElevationDirection::Up => self.up_until_ms = until_ms,
            ElevationDirection::Down => self.down_until_ms = until_ms,
        }
    }
}

impl Default for DriveDebouncer {
    fn default() -> Self {
        Self::new()
    }
}

const fn min_option(current: Option<u64>, candidate: u64) -> u64 {
    match current {
        Some(current) if current < candidate => current,
        _ => candidate,
    }
}

/// A command received from the dashboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Move either or both axes. The firmware treats this as a lease that must be
    /// refreshed by the client until it sends [`Stop`](Self::Stop).
    Drive(DriveCommand),
    /// Stop all direction outputs.
    Stop,
}

impl Command {
    /// Parse a dashboard WebSocket text message.
    pub fn parse(message: &str) -> Option<Self> {
        if message == "stop" {
            return Some(Self::Stop);
        }

        let drive = message.strip_prefix("drive:")?;
        if drive.is_empty() {
            return None;
        }

        let mut command = DriveCommand::IDLE;

        for token in drive.split('+') {
            match token {
                "cw" if command.azimuth.is_none() => {
                    command.azimuth = Some(AzimuthDirection::Clockwise);
                }
                "ccw" if command.azimuth.is_none() => {
                    command.azimuth = Some(AzimuthDirection::CounterClockwise);
                }
                "up" if command.elevation.is_none() => {
                    command.elevation = Some(ElevationDirection::Up);
                }
                "down" if command.elevation.is_none() => {
                    command.elevation = Some(ElevationDirection::Down);
                }
                _ => return None,
            }
        }

        command.is_active().then_some(Self::Drive(command))
    }
}

/// A client-originated WebSocket message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientMessage {
    /// Request controller ownership for this connection.
    TakeControl,
    /// Drive-state command for the active controller.
    Command(Command),
}

impl ClientMessage {
    /// Parse a browser-originated WebSocket text message.
    pub fn parse(message: &str) -> Option<Self> {
        if message == TAKE_CONTROL_MESSAGE {
            Some(Self::TakeControl)
        } else {
            Command::parse(message).map(Self::Command)
        }
    }
}

/// Controller ownership and active drive state reported to each browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlStatus {
    /// Whether the receiving connection currently owns control.
    pub controller: bool,
    /// Server-confirmed drive state.
    pub drive: DriveCommand,
}

impl ControlStatus {
    /// Write this status as a compact WebSocket JSON message.
    pub fn write_json(&self, out: &mut impl fmt::Write) -> fmt::Result {
        fn write_option(out: &mut impl fmt::Write, value: Option<&str>) -> fmt::Result {
            match value {
                Some(value) => write!(out, "\"{}\"", value),
                None => fmt::Write::write_str(out, "null"),
            }
        }

        write!(
            out,
            "{{\"type\":\"control\",\"controller\":{},\"azimuth\":",
            if self.controller { "true" } else { "false" }
        )?;
        write_option(out, self.drive.azimuth_token())?;
        fmt::Write::write_str(out, ",\"elevation\":")?;
        write_option(out, self.drive.elevation_token())?;
        fmt::Write::write_str(out, "}")
    }
}

/// Raw position feedback reported to the browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionTelemetry {
    /// ADS1015 channel A0 in millivolts.
    pub a0_mv: i32,
    /// ADS1015 channel A1 in millivolts.
    pub a1_mv: i32,
}

impl PositionTelemetry {
    /// A zeroed telemetry sample used before the first ADC read completes.
    pub const ZERO: Self = Self { a0_mv: 0, a1_mv: 0 };

    /// Write this telemetry sample as a compact WebSocket JSON message.
    pub fn write_json(&self, out: &mut impl fmt::Write) -> fmt::Result {
        write!(
            out,
            "{{\"type\":\"position\",\"a0_mv\":{},\"a1_mv\":{}}}",
            self.a0_mv, self.a1_mv
        )
    }
}
