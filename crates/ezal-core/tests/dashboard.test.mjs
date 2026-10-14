// Exercise the exact script served by firmware through browser events. The
// fakes implement browser I/O only; all control state and command generation
// come from dashboard.html. These tests do not cover rendering or real browser
// pointer-capture delivery.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { runInNewContext } from "node:vm";

const html = readFileSync(new URL("../src/dashboard.html", import.meta.url), "utf8");
const scripts = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)];
assert.equal(scripts.length, 1, "load the dashboard's single embedded script");
assert.ok(process.env.EZAL_DASHBOARD_CONFIG, "run scripts/test-dashboard.sh to export firmware configuration");
const configScript = readFileSync(process.env.EZAL_DASHBOARD_CONFIG, "utf8");
const configContext = { window: {} };
runInNewContext(configScript, configContext);
const config = configContext.window.EZAL_CONFIG;

class EventTarget {
  listeners = new Map();

  addEventListener(type, callback) {
    if (!this.listeners.has(type)) this.listeners.set(type, []);
    this.listeners.get(type).push(callback);
  }

  dispatch(type, properties = {}) {
    const event = {
      target: this,
      defaultPrevented: false,
      preventDefault() { this.defaultPrevented = true; },
      ...properties,
    };
    for (const callback of this.listeners.get(type) || []) callback(event);
    return event;
  }
}

// The firmware's real `/api/passes` reply, pinned by ezal-core's passes tests.
const servedReport = JSON.parse(readFileSync(new URL("./fixtures/passes-report.json", import.meta.url), "utf8"));

function idleTracking(overrides = {}) {
  return {
    revision: "0", operator: null, mode: "manual", phase: "idle", run: null, selection: null,
    plan: { status: "unplanned", reason: null, preview: null },
    feedback_stale: false, feedback_age_ms: 0,
    deviation: { run: null, warning: false, maximum_deg: null, outside_ms: 0, current: null },
    recovery: null, termination: null, summary: null, ...overrides,
  };
}

function forecastFixture(overrides = {}) {
  const now = Date.parse("2026-10-07T23:59:00Z") / 1000;
  return {
    status: "ready", now_unix_s: now, valid_until_unix_s: now + 60,
    sync_age_ms: 60000, tle_updated_unix_s: now - 3600,
    download_failed: false, truncated: false, window_s: 21600,
    observer: { latitude_deg: 51.44266, longitude_deg: -2.84053, altitude_m: 0 },
    min_elevation_deg: 5, min_peak_elevation_deg: 10,
    satellites: [{ name: "METOP-C", available: true, failed: false, experimental: false }],
    passes: [{ norad_id: "43689", name: "METOP-C", frequency_mhz: "1701.3", experimental: false,
      aos_unix_s: now - 30, los_unix_s: now + 90, peak_elevation_deg: 40.5 }],
    ...overrides,
  };
}

test("forecast shows full UTC crossings over midnight and identifies an ongoing pass", async () => {
  const ui = dashboard();
  await ui.forecast(forecastFixture());
  assert.equal(ui.requests[0].url, "/api/passes");
  assert.equal(ui.requests[0].options.cache, "no-store");
  const row = ui.ids.get("pass-rows").children[0];
  // A cell's text includes its label, exactly as a browser reports it.
  assert.equal(row.children[1].textContent, "10-07 23:58:30In progress");
  assert.equal(row.children[1].children[0].textContent, "In progress");
  assert.equal(row.children[2].textContent, "10-08 00:00:30");
  assert.ok(row.classList.contains("underway"));
  assert.equal(row.children[3].textContent, "40.5°");
  ui.clock.advance(2000);
  assert.match(ui.ids.get("utc-clock").textContent, /23:59:02/);
});

test("forecast distinguishes no passes, missing elements, no time and failed refresh", async () => {
  for (const [data, status] of [
    [forecastFixture({ passes: [] }), /No qualifying passes/],
    [forecastFixture({ status: "waiting_for_tles", valid_until_unix_s: null, passes: [] }), /Waiting for fresh orbital elements/],
    [forecastFixture({ status: "predicting", valid_until_unix_s: null, passes: [] }), /Calculating passes/],
    [forecastFixture({ status: "waiting_for_time", now_unix_s: null, valid_until_unix_s: null, passes: [] }), /fresh NTP/],
    [forecastFixture({ satellites: [{ name: "METOP-C", available: false, failed: false }], download_failed: true }), /incomplete coverage/],
  ]) {
    const ui = dashboard();
    await ui.forecast(data);
    assert.match(ui.ids.get("pass-status").textContent, status);
    if (data.now_unix_s === null) assert.equal(ui.ids.get("utc-clock").textContent, "Waiting for NTP time");
    if (data.download_failed) {
      assert.match(ui.ids.get("tle-status").textContent, /refresh failed/);
      assert.match(ui.ids.get("pass-coverage").textContent, /METOP-C: missing or stale elements/);
    }
  }
});

test("a failed prediction is reported separately from missing elements", async () => {
  const ui = dashboard();
  await ui.forecast(forecastFixture({
    satellites: [{ name: "METOP-C", available: true, failed: true, experimental: false }],
  }));
  assert.match(ui.ids.get("pass-status").textContent, /incomplete coverage/);
  assert.equal(ui.ids.get("pass-coverage").textContent, "METOP-C: prediction failed");
});

test("rows last exactly as long as the device says its list is complete", async () => {
  const ui = dashboard();
  const data = forecastFixture({ passes: [{ ...forecastFixture().passes[0], los_unix_s: forecastFixture().now_unix_s + 600 }] });
  await ui.forecast({ ...data, valid_until_unix_s: data.now_unix_s + 10 });
  ui.clock.advance(9000);
  assert.equal(ui.ids.get("pass-rows").children.length, 1);
  ui.clock.advance(2000);
  assert.equal(ui.ids.get("pass-rows").children.length, 0, "no client-side age limit overrides the device");
  assert.match(ui.ids.get("pass-status").textContent, /Calculating passes/);
  const longer = dashboard();
  await longer.forecast({ ...data, valid_until_unix_s: data.now_unix_s + 40 });
  longer.clock.advance(30000);
  assert.equal(longer.ids.get("pass-rows").children.length, 1);
});

test("unchanged forecasts leave the live status and rows untouched", async () => {
  const ui = dashboard();
  const now = forecastFixture().now_unix_s;
  await ui.forecast(forecastFixture({
    valid_until_unix_s: now + 600,
    passes: [{ norad_id: "43689", name: "METOP-C", frequency_mhz: "1701.3", experimental: false,
      aos_unix_s: now + 5, los_unix_s: now + 300, peak_elevation_deg: 40.5 }],
  }));
  const status = ui.ids.get("pass-status");
  const rows = ui.ids.get("pass-rows");
  const writes = status.textWrites;
  const replacements = rows.replacements;
  ui.clock.advance(4000);
  assert.equal(status.textWrites, writes, "the polite live region is not rewritten every second");
  assert.equal(rows.replacements, replacements, "rows are not rebuilt every second");
  ui.clock.advance(2000);
  assert.equal(rows.replacements, replacements + 1, "a pass starting does rebuild its row");
  assert.equal(rows.children[0].children[1].children[0].textContent, "In progress");
  assert.equal(status.textWrites, writes, "the count of windows is unchanged");
});

test("the station line never names a site the device has not reported", async () => {
  assert.ok(!html.includes("Clevedon"), "no hard-coded default site in the served page");
  const ui = dashboard();
  await ui.forecast(forecastFixture(), false);
  assert.equal(ui.ids.get("pass-site").textContent, "Station settings unavailable · retrying");
  ui.clock.advance(15000);
  await ui.forecast(forecastFixture({ observer: { latitude_deg: 40.015, longitude_deg: -105.27, altitude_m: 1655 } }));
  const reported = ui.ids.get("pass-site").textContent;
  assert.match(reported, /^40\.01500°, -105\.27000° · 1655 m/);
  ui.clock.advance(15000);
  await ui.forecast(forecastFixture(), false);
  assert.equal(ui.ids.get("pass-site").textContent, reported, "a later failure keeps the reported site");
});

test("the firmware's serialized report renders as served", async () => {
  const ui = dashboard();
  await ui.forecast(servedReport);
  const expected = servedReport.passes.filter((pass) => pass.aos_unix_s < servedReport.now_unix_s + servedReport.window_s);
  assert.ok(expected.length > 0);
  assert.equal(ui.ids.get("pass-rows").children.length, expected.length);
  assert.equal(ui.ids.get("pass-status").textContent, `${expected.length} reception windows`);
  assert.match(ui.ids.get("pass-coverage").textContent, /^METOP-B: ready · METOP-C: ready/);
  assert.match(ui.ids.get("pass-site").textContent, /^51\.44266°, -2\.84053° · 0 m · cutoff 5° · minimum peak 10°$/);
  assert.match(ui.ids.get("tle-status").textContent, /^Elements downloaded 10-07 14:50:00 UTC$/);
});

test("forecast expiry and HTTP failure clear rows without renewing motor authority", async () => {
  const ui = dashboard("http:", false);
  ui.socket.open();
  ui.control();
  ui.press("up", 1);
  await ui.forecast(forecastFixture());
  ui.clock.advance(1100);
  assert.ok(ui.buttons.get("up").disabled, "forecast data is not control telemetry");
  ui.clock.advance(45000);
  assert.equal(ui.ids.get("pass-rows").children.length, 0);
  assert.match(ui.ids.get("pass-status").textContent, /unavailable/);
  const failed = dashboard();
  await failed.forecast(forecastFixture(), false);
  assert.match(failed.ids.get("pass-status").textContent, /unavailable/);
});

test("ended passes disappear and experimental names are rendered only as text", async () => {
  const ui = dashboard();
  const data = forecastFixture();
  data.passes[0].name = "<img src=x onerror=alert(1)>";
  data.passes[0].experimental = true;
  data.passes[0].los_unix_s = data.now_unix_s + 2;
  await ui.forecast(data);
  const name = ui.ids.get("pass-rows").children[0].children[0];
  assert.equal(name.textContent, `${data.passes[0].name}1701.3 MHz · experimental / unconfirmed`);
  assert.equal(name.innerHTML, undefined, "markup is never parsed");
  assert.match(name.children[0].textContent, /experimental/);
  ui.clock.advance(2000);
  assert.equal(ui.ids.get("pass-rows").children.length, 0);
  assert.match(ui.ids.get("pass-status").textContent, /No qualifying passes/);
});

test("malformed forecast cannot stop subsequent polling", async () => {
  const ui = dashboard();
  await ui.forecast(forecastFixture({ passes: [null] }));
  assert.match(ui.ids.get("pass-status").textContent, /unavailable/);
  ui.clock.advance(15000);
  assert.equal(ui.requests.length, 2);
  await ui.forecast(forecastFixture({ valid_until_unix_s: "soon" }));
  assert.match(ui.ids.get("pass-status").textContent, /unavailable/);
  ui.clock.advance(15000);
  await ui.forecast(forecastFixture());
  assert.equal(ui.ids.get("pass-rows").children.length, 1);
});

class Element extends EventTarget {
  attributes = new Map();
  setAttribute(name, value) { this.attributes.set(name, value); }
  disabled = false;
  dataset = {};
  captures = new Set();
  // DOM semantics: reading textContent concatenates the node's own text and
  // every descendant's; writing it replaces all children with that text.
  #text = "";
  children = [];
  textWrites = 0;
  replacements = 0;
  get textContent() { return this.#text + this.children.map((child) => child.textContent).join(""); }
  set textContent(text) {
    this.#text = String(text);
    this.children = [];
    this.textWrites += 1;
  }
  appendChild(child) { this.children.push(child); return child; }
  replaceChildren(...children) {
    this.#text = "";
    this.children = children;
    this.replacements += 1;
  }
  classes = new Set();
  classList = {
    toggle: (name, enabled) => {
      if (enabled) this.classes.add(name);
      else this.classes.delete(name);
    },
    contains: (name) => this.classes.has(name),
  };

  setPointerCapture(pointerId) {
    this.captures.add(pointerId);
  }
}

class Clock {
  now = 0;
  nextId = 1;
  jobs = new Map();

  schedule(callback, delay, interval = false) {
    assert.ok(delay > 0, "timers must advance time");
    const id = this.nextId++;
    this.jobs.set(id, { callback, at: this.now + delay, interval, delay });
    return id;
  }

  advance(milliseconds) {
    const end = this.now + milliseconds;
    while (true) {
      const pending = [...this.jobs].sort((a, b) => a[1].at - b[1].at);
      const next = pending[0];
      if (!next || next[1].at > end) break;
      const [id, job] = next;
      this.now = job.at;
      if (job.interval) job.at += job.delay;
      else this.jobs.delete(id);
      job.callback();
    }
    this.now = end;
  }
}

function dashboard(protocol = "http:", autoTelemetry = true, clientConfig = configScript) {
  const ids = new Map([...html.matchAll(/\bid="([^"]+)"/g)]
    .map((match) => [match[1], new Element()]));
  const buttons = new Map([...html.matchAll(/\bdata-cmd="([^"]+)"/g)]
    .map((match) => {
      const button = new Element();
      button.dataset.cmd = match[1];
      return [match[1], button];
    }));
  const body = new Element();
  const window = new EventTarget();
  const document = Object.assign(new EventTarget(), {
    body,
    hidden: false,
    createElement() { return new Element(); },
    getElementById(id) {
      assert.ok(ids.has(id), `element #${id} exists in the served HTML`);
      return ids.get(id);
    },
    querySelectorAll(selector) {
      assert.equal(selector, ".dir");
      return [...buttons.values()];
    },
  });
  const clock = new Clock();
  const sockets = [];
  const requests = [];
  let telemetry = {
    tracking: idleTracking(),
    position: { type: "position", a0_mv: 0, a1_mv: 0 },
    feedback: { type: "feedback", azimuth_tenths: null, elevation_tenths: null, control_state: "idle" },
    control: { type: "control", controller: false, azimuth: null, elevation: null, network_ready: true, motion_fault: null, duty_remaining_ms: [180000, 180000], duty_cooldown_ms: [0, 0] },
  };
  class WebSocket extends EventTarget {
    static OPEN = 1;
    readyState = 0;
    messages = [];
    wireMessages = [];

    constructor(url) {
      super();
      this.url = url;
      sockets.push(this);
      telemetry.control = { ...telemetry.control, controller: false, azimuth: null, elevation: null };
    }

    send(message) {
      assert.equal(this.readyState, WebSocket.OPEN, "send only on an open socket");
      this.wireMessages.push(message);
      // Existing interaction tests compare operator intents. Freshness tests
      // below inspect the complete envelope separately via wireMessages.
      this.messages.push(message.replace(/^(press|hold):\d+:\d+:/, ""));
    }

    open() {
      this.readyState = WebSocket.OPEN;
      this.dispatch("open");
    }

    receive(message) {
      if (message.type === "control" && this === sockets.at(-1)) telemetry.control = { ...telemetry.control, ...message };
      this.dispatch("message", { data: JSON.stringify(message) });
    }

    close() {
      this.readyState = 3;
      this.dispatch("close");
    }
  }
  runInNewContext(`${clientConfig}\n${scripts[0][1]}`, {
    document,
    location: { protocol, host: "ezal.local" },
    window,
    WebSocket,
    performance: { now: () => clock.now },
    AbortController,
    fetch: (url, options) => new Promise((resolve, reject) => {
      requests.push({ url, options, resolve, reject });
      options.signal.addEventListener("abort", () => reject(new Error("aborted")));
    }),
    setInterval: (callback, delay) => clock.schedule(callback, delay, true),
    clearInterval: (id) => clock.jobs.delete(id),
    setTimeout: (callback, delay) => clock.schedule(callback, delay),
    clearTimeout: (id) => clock.jobs.delete(id),
  }, { filename: "dashboard.html" });
  let token = 0;
  const freshness = (updates = {}) => {
    telemetry = { ...telemetry, ...updates };
    telemetry.tracking = { ...telemetry.tracking, operator: telemetry.control.controller ? (telemetry.tracking.operator ?? "1") : null };
    sockets.at(-1).receive({ type: "telemetry", ...telemetry, freshness: { token: String(++token) } });
  };
  if (autoTelemetry) clock.schedule(() => {
    if (sockets.at(-1).readyState === WebSocket.OPEN) freshness();
  }, config.telemetryTimeoutMs / 2, true);
  return {
    ids, buttons, body, window, document, clock, sockets, freshness, requests,
    async forecast(data, ok = true) {
      requests.at(-1).resolve({ ok, json: async () => data });
      await new Promise((resolve) => setImmediate(resolve));
    },
    get socket() { return sockets.at(-1); },
    get telemetry() { return telemetry; },
    control(controller = true, azimuth = null, elevation = null) {
      this.socket.receive({ type: "control", controller, azimuth, elevation,
        network_ready: true, motion_fault: null });
      freshness();
    },
    press(command, pointerId, button = 0) {
      return buttons.get(command).dispatch("pointerdown", { pointerId, button });
    },
    release(command, pointerId, type = "pointerup") {
      buttons.get(command).dispatch(type, { pointerId });
    },
  };
}

function controller(autoTelemetry = true) {
  const page = dashboard("http:", autoTelemetry);
  page.socket.open();
  page.control();
  return page;
}

test("a viewer must request and receive control before issuing motion", () => {
  const page = dashboard();
  assert.match(page.ids.get("control-status").textContent, /Reconnecting/);
  page.press("cw", 1);
  page.socket.open();
  assert.equal(page.ids.get("status").textContent, "waiting for telemetry");
  assert.match(page.ids.get("control-status").textContent, /Waiting for fresh controller data/);
  page.control(false);
  assert.equal(page.ids.get("status").textContent, "online");
  assert.equal(page.ids.get("take").disabled, false);
  assert.match(page.ids.get("control-status").textContent, /Select Take control/);
  assert.match(page.buttons.get("cw").title, /Select Take control/);
  for (const button of page.buttons.values()) assert.equal(button.disabled, true);
  page.ids.get("take").dispatch("click");
  page.press("cw", 1);
  assert.deepEqual(page.socket.messages, ["control:take"]);
  page.control();
  assert.equal(page.ids.get("take").textContent, "controller");
  assert.equal(page.ids.get("take").disabled, true);
  const event = page.press("cw", 1);
  assert.equal(event.defaultPrevented, true);
  assert.equal(page.buttons.get("cw").captures.has(1), true);
  assert.deepEqual(page.socket.messages, ["control:take", "drive:cw"]);
});

test("each axis retains its first pointer; releasing a rejected pointer cannot stop it", () => {
  const page = controller();
  page.press("cw", 1);
  page.press("ccw", 2);
  page.press("cw", 3);
  assert.equal(page.buttons.get("ccw").captures.has(2), false);
  assert.equal(page.buttons.get("cw").captures.has(3), false);
  page.release("ccw", 2);
  page.release("cw", 3);
  page.press("up", 4);
  page.release("cw", 1);
  page.press("ccw", 5);
  page.release("up", 4);
  page.release("ccw", 5);
  assert.deepEqual(page.socket.messages, [
    "drive:cw", "drive:cw+up", "drive:up", "drive:ccw+up", "drive:ccw", "stop",
  ]);
});

test("secondary pointer buttons cannot start motion or retain a primary hold", () => {
  const page = controller();
  for (const button of [1, 2, 5]) {
    page.press("cw", 1, button);
    page.release("cw", 1);
  }
  assert.deepEqual(page.socket.messages, []);
  page.press("cw", 1);
  page.press("up", 2);
  // Releasing the primary mouse button while the secondary remains pressed
  // is a pointermove with buttons=2, not a pointerup.
  page.buttons.get("cw").dispatch("pointermove", { pointerId: 1, button: 0, buttons: 2 });
  page.release("cw", 1);
  page.release("up", 2);
  page.clock.advance(1000);
  assert.deepEqual(page.socket.messages, ["drive:cw", "drive:cw+up", "drive:up", "stop"]);
});

for (const ending of ["pointerup", "pointercancel", "lostpointercapture"]) {
  test(`${ending} releases just its axis and repeated release is harmless`, () => {
    const page = controller();
    page.press("ccw", 1);
    page.press("down", 2);
    page.release("ccw", 1, ending);
    page.release("ccw", 1, "lostpointercapture");
    page.release("down", 2, ending);
    page.clock.advance(1000);
    assert.deepEqual(page.socket.messages, ["drive:ccw", "drive:ccw+down", "drive:down", "stop"]);
    page.press("cw", 3);
    assert.equal(page.socket.messages.at(-1), "drive:cw");
  });
}

test("held commands use the served refresh interval and stop when released", () => {
  const page = controller();
  page.press("up", 1);
  page.clock.advance(config.refreshMs - 1);
  assert.deepEqual(page.socket.messages, ["drive:up"]);
  page.clock.advance(1);
  page.press("cw", 2);
  page.clock.advance(config.refreshMs * 2);
  page.release("up", 1);
  page.clock.advance(config.refreshMs);
  page.release("cw", 2);
  page.clock.advance(1000);
  assert.deepEqual(page.socket.messages, [
    "drive:up", "drive:up", "drive:cw+up", "drive:cw+up", "drive:cw+up", "drive:cw", "drive:cw", "stop",
  ]);
});

test("held inputs respond immediately while active highlights follow applied output", () => {
  const page = controller();
  assert.equal(page.ids.get("control-state").textContent, "ready");
  page.press("cw", 1);
  assert.equal(page.buttons.get("cw").classList.contains("held"), true);
  assert.equal(page.buttons.get("cw").classList.contains("active"), false);
  assert.equal(page.ids.get("azimuth-drive").textContent, "IDLE");
  assert.match(page.ids.get("control-status").textContent, /CW requested.*Waiting for movement confirmation/);
  page.control(true, "cw", null);
  assert.equal(page.buttons.get("cw").classList.contains("active"), true);
  assert.equal(page.ids.get("azimuth-drive").textContent, "CW");
  assert.equal(page.ids.get("control-state").textContent, "driving");
  page.release("cw", 1);
  assert.equal(page.buttons.get("cw").classList.contains("held"), false);
  assert.equal(page.buttons.get("cw").classList.contains("active"), true);
  assert.equal(page.ids.get("control-state").textContent, "stopping");
  page.control(true, null, null);
  assert.equal(page.buttons.get("cw").classList.contains("active"), false);
  assert.equal(page.ids.get("azimuth-drive").textContent, "IDLE");
  assert.equal(page.ids.get("control-state").textContent, "ready");
});

test("each requested axis shows waiting feedback until its own output is applied", () => {
  const page = controller();
  page.press("cw", 1);
  page.press("up", 2);
  page.control(true, "cw", null);
  assert.match(page.ids.get("control-status").textContent, /^UP requested/);
  assert.equal(page.buttons.get("up").classList.contains("held"), true);
  assert.equal(page.buttons.get("up").classList.contains("active"), false);
  page.release("up", 2, "pointercancel");
  assert.equal(page.buttons.get("up").classList.contains("held"), false);
  assert.match(page.ids.get("control-status").textContent, /Driving CW/);
});

for (const ending of ["pointerup", "pointercancel", "pointermove"]) {
  test(`${ending} outside the pad releases only its pointer's axis if capture is lost`, () => {
    const page = controller();
    page.press("cw", 1);
    page.press("up", 2);
    page.window.dispatch(ending, { pointerId: 1, buttons: 2 });
    assert.equal(page.socket.messages.at(-1), "drive:up");
    assert.equal(page.buttons.get("cw").classList.contains("held"), false);
    assert.equal(page.buttons.get("up").classList.contains("held"), true);
    page.window.dispatch(ending, { pointerId: 2, buttons: 0 });
    assert.equal(page.socket.messages.at(-1), "stop");
    const sent = page.socket.messages.length;
    page.window.dispatch(ending, { pointerId: 2, buttons: 0 });
    assert.equal(page.socket.messages.length, sent);
  });
}

for (const key of [" ", "Enter"]) {
  test(`${JSON.stringify(key)} holds a focused direction and stops on key release`, () => {
    const page = controller();
    const button = page.buttons.get("up");
    assert.equal(button.dispatch("keydown", { key }).defaultPrevented, true);
    assert.match(page.socket.wireMessages.at(-1), /^press:\d+:\d+:drive:up$/);
    assert.equal(button.classList.contains("held"), true);
    assert.equal(button.classList.contains("active"), false);
    assert.equal(button.captures.size, 0, "keyboard input does not capture a pointer");
    button.dispatch("keydown", { key, repeat: true });
    assert.equal(page.socket.wireMessages.length, 1, "auto-repeat is not a new press");
    page.clock.advance(config.refreshMs);
    assert.match(page.socket.wireMessages.at(-1), /^hold:\d+:\d+:drive:up$/);
    assert.equal(button.dispatch("keyup", { key }).defaultPrevented, true);
    assert.equal(page.socket.messages.at(-1), "stop");
    assert.equal(button.classList.contains("held"), false);
    const sent = page.socket.messages.length;
    page.clock.advance(config.refreshMs * 2);
    assert.equal(page.socket.messages.length, sent);
  });
}

test("keyboard and pointer releases cannot cancel another input's axis claim", () => {
  const page = controller();
  const cw = page.buttons.get("cw");
  page.press("cw", 1);
  cw.dispatch("keydown", { key: " " });
  cw.dispatch("keyup", { key: " " });
  assert.deepEqual(page.socket.messages, ["drive:cw"]);
  page.buttons.get("up").dispatch("keydown", { key: "Enter" });
  assert.equal(page.socket.messages.at(-1), "drive:cw+up");
  page.release("cw", 1);
  assert.equal(page.socket.messages.at(-1), "drive:up");
  page.release("up", 1);
  assert.equal(page.socket.messages.at(-1), "drive:up");
  page.buttons.get("up").dispatch("keyup", { key: "Enter" });
  assert.equal(page.socket.messages.at(-1), "stop");
});

test("losing button focus stops keyboard motion and auto-repeat cannot restart it", () => {
  const page = controller();
  const button = page.buttons.get("cw");
  button.dispatch("keydown", { key: " " });
  button.dispatch("blur");
  assert.equal(page.socket.messages.at(-1), "stop");
  assert.equal(button.classList.contains("held"), false);
  button.dispatch("keydown", { key: " ", repeat: true });
  page.clock.advance(config.refreshMs * 2);
  assert.deepEqual(page.socket.messages, ["drive:cw", "stop"]);
  button.dispatch("keyup", { key: " " });
  button.dispatch("keydown", { key: " " });
  assert.equal(page.socket.messages.at(-1), "drive:cw");
});

test("keyboard holds expire with telemetry and require a new press after recovery", () => {
  const page = controller(false);
  const button = page.buttons.get("cw");
  button.dispatch("keydown", { key: "Enter" });
  page.clock.advance(config.telemetryTimeoutMs + 100);
  assert.equal(page.socket.messages.at(-1), "stop");
  assert.equal(button.classList.contains("held"), false);
  assert.equal(button.disabled, true);
  assert.match(page.ids.get("control-status").textContent, /Waiting for fresh controller data/);
  page.freshness();
  button.dispatch("keydown", { key: "Enter", repeat: true });
  assert.equal(page.socket.messages.at(-1), "stop");
  button.dispatch("keyup", { key: "Enter" });
  button.dispatch("keydown", { key: "Enter" });
  assert.match(page.socket.wireMessages.at(-1), /^press:\d+:\d+:drive:cw$/);
});

test("keyboard input respects ownership and ignores shortcuts and unrelated keys", () => {
  const page = dashboard();
  page.socket.open();
  page.control(false);
  const button = page.buttons.get("up");
  button.dispatch("keydown", { key: "Enter" });
  page.control();
  button.dispatch("keydown", { key: "ArrowUp" });
  for (const modifier of ["ctrlKey", "altKey", "metaKey"]) {
    button.dispatch("keydown", { key: "Enter", [modifier]: true });
  }
  assert.deepEqual(page.socket.messages, []);
  assert.equal(button.classList.contains("held"), false);
});

test("missing control configuration explains disabled buttons while forecasts still load", async () => {
  const page = dashboard("http:", false, "");
  page.clock.advance(1000);
  assert.equal(page.sockets.length, 0);
  assert.equal(page.ids.get("status").textContent, "configuration unavailable");
  assert.match(page.ids.get("control-status").textContent, /settings failed to load.*Reload/);
  for (const button of page.buttons.values()) assert.equal(button.disabled, true);
  await page.forecast(forecastFixture());
  assert.equal(page.ids.get("pass-rows").children.length, 1);
});

test("unusable position feedback disables directions and explains why movement cannot run", () => {
  for (const [health, reason] of [
    ["feedback-invalid", /outside the calibrated range.*A0\/A1/],
    ["calibration-invalid", /calibration is invalid/],
    ["no-feedback", /Waiting for position feedback/],
    ["feedback-stale", /feedback is stale/],
    ["timestamp-invalid", /timing is invalid/],
  ]) {
    const page = controller();
    page.freshness({ feedback: { ...page.telemetry.feedback, control_state: health } });
    page.press("cw", 1);
    page.buttons.get("up").dispatch("keydown", { key: "Enter" });
    assert.deepEqual(page.socket.messages, []);
    for (const button of page.buttons.values()) {
      assert.equal(button.disabled, true);
      assert.equal(button.classList.contains("held"), false);
      assert.match(button.title, reason);
    }
    assert.equal(page.ids.get("control-state").textContent, "blocked");
    assert.match(page.ids.get("control-status").textContent, reason);
  }
});

test("bad feedback stops held inputs and recovery requires a fresh press", () => {
  const page = controller();
  page.press("cw", 1);
  page.buttons.get("up").dispatch("keydown", { key: "Enter" });
  page.freshness({ feedback: { ...page.telemetry.feedback, control_state: "feedback-invalid" } });
  assert.equal(page.socket.messages.at(-1), "stop");
  for (const button of page.buttons.values()) assert.equal(button.classList.contains("held"), false);
  page.freshness({ feedback: { ...page.telemetry.feedback, control_state: "idle" } });
  page.buttons.get("up").dispatch("keydown", { key: "Enter", repeat: true });
  page.clock.advance(config.refreshMs * 2);
  assert.equal(page.socket.messages.at(-1), "stop");
  assert.equal(page.ids.get("control-state").textContent, "ready");
  page.release("cw", 1);
  page.press("cw", 2);
  assert.match(page.socket.wireMessages.at(-1), /^press:\d+:\d+:drive:cw$/);
});

test("control takeover cancels old held commands until a fresh pointer press", () => {
  const page = controller();
  page.press("cw", 1);
  page.press("up", 2);
  page.control(false, "ccw", "down");
  page.clock.advance(1000);
  page.release("cw", 1);
  page.release("up", 2);
  assert.deepEqual(page.socket.messages, ["drive:cw", "drive:cw+up"]);
  assert.equal(page.buttons.get("ccw").disabled, true);
  assert.equal(page.buttons.get("ccw").classList.contains("active"), true);
  for (const button of page.buttons.values()) assert.equal(button.classList.contains("held"), false);
  page.ids.get("take").dispatch("click");
  page.control();
  page.clock.advance(1000);
  assert.equal(page.socket.messages.at(-1), "control:take");
  page.press("down", 3);
  assert.equal(page.socket.messages.at(-1), "drive:down");
});

for (const ending of ["blur", "beforeunload", "pagehide"]) {
  test(`${ending} clears all pointer claims and sends a single stop`, () => {
    const page = controller();
    page.press("cw", 1);
    page.press("up", 2);
    page.window.dispatch(ending);
    page.window.dispatch(ending);
    page.release("cw", 1);
    page.release("up", 2);
    page.clock.advance(1000);
    assert.deepEqual(page.socket.messages, ["drive:cw", "drive:cw+up", "stop"]);
    page.press("ccw", 3);
    assert.equal(page.socket.messages.at(-1), "drive:ccw");
  });
}

test("hiding and restoring a page cannot resume a previous operator hold", () => {
  const page = controller();
  page.press("cw", 1);
  page.press("up", 2);
  page.document.hidden = true;
  page.document.dispatch("visibilitychange");
  page.clock.advance(1000);
  page.document.hidden = false;
  page.document.dispatch("visibilitychange");
  page.clock.advance(1000);
  page.release("cw", 1);
  page.release("up", 2);
  assert.deepEqual(page.socket.messages, ["drive:cw", "drive:cw+up", "stop"]);
  page.press("ccw", 3);
  assert.equal(page.socket.messages.at(-1), "drive:ccw");
  // A visible notification alone must not cancel a newly started hold.
  page.document.dispatch("visibilitychange");
  page.clock.advance(150);
  assert.equal(page.socket.messages.at(-1), "drive:ccw");
});

test("reconnection clears held motion and waits for newly granted control", () => {
  const page = controller();
  const oldSocket = page.socket;
  page.press("cw", 1);
  page.control(true, "cw", null);
  oldSocket.close();
  assert.equal(page.ids.get("status").textContent, "offline");
  assert.equal(page.ids.get("azimuth-drive").textContent, "unknown");
  assert.equal(page.buttons.get("cw").disabled, true);
  assert.equal(page.buttons.get("cw").classList.contains("active"), false);
  page.clock.advance(399);
  assert.equal(page.sockets.length, 1);
  page.clock.advance(1);
  assert.equal(page.sockets.length, 2);
  page.socket.open();
  page.press("up", 2);
  page.clock.advance(1000);
  assert.deepEqual(page.socket.messages, []);
  page.control();
  page.release("cw", 1);
  page.clock.advance(1000);
  assert.deepEqual(page.socket.messages, []);
  page.press("up", 3);
  assert.deepEqual(page.socket.messages, ["drive:up"]);
  assert.deepEqual(oldSocket.messages, ["drive:cw"]);
});

test("failed reconnects back off and a successful open resets the delay", () => {
  const page = dashboard("https:");
  assert.equal(page.socket.url, "wss://ezal.local/ws");
  for (const delay of [400, 640, 1024, 1638.4, 2621.44, 4194.304, 5000, 5000]) {
    page.socket.close();
    const count = page.sockets.length;
    page.clock.advance(delay - 1);
    assert.equal(page.sockets.length, count);
    page.clock.advance(1.001);
    assert.equal(page.sockets.length, count + 1);
  }
  page.socket.open();
  page.socket.close();
  const count = page.sockets.length;
  page.clock.advance(400);
  assert.equal(page.sockets.length, count + 1);
});

test("telemetry renders measured position and feedback health", () => {
  const page = controller();
  page.socket.dispatch("message", { data: "{malformed" });
  page.freshness({
    position: { type: "position", a0_mv: 901, a1_mv: -12 },
    feedback: { type: "feedback", control_state: "idle", azimuth_tenths: 4247, elevation_tenths: 3 },
  });
  assert.equal(String(page.ids.get("a0").textContent), "901");
  assert.equal(String(page.ids.get("a1").textContent), "-12");
  assert.equal(page.ids.get("health").textContent, "idle");
  assert.equal(page.ids.get("azimuth").textContent, "424.7");
  assert.equal(page.ids.get("elevation").textContent, "0.3");
});

test("movement carries a server token and increasing input sequence; refreshes cannot rearm", () => {
  const page = controller();
  page.press("cw", 1);
  page.clock.advance(150);
  page.press("up", 2);
  page.release("cw", 1);
  page.release("up", 2);
  assert.deepEqual(page.socket.wireMessages, [
    "press:1:1:drive:cw", "hold:1:2:drive:cw", "press:1:3:drive:cw+up",
    "hold:1:4:drive:up", "stop",
  ]);
});

test("an open socket with a silent telemetry return path loses control and held intent", () => {
  const page = controller(false);
  page.press("cw", 1);
  page.control(true, "cw");
  page.freshness({ position: { type: "position", a0_mv: 900, a1_mv: 1200 } });
  page.clock.advance(config.telemetryTimeoutMs);
  assert.equal(page.socket.readyState, 1);
  assert.equal(page.socket.messages.at(-1), "stop");
  assert.equal(page.ids.get("status").textContent, "telemetry lost");
  assert.equal(page.ids.get("azimuth-drive").textContent, "unknown");
  assert.equal(page.ids.get("a1").textContent, "--");
  assert.equal(page.buttons.get("cw").disabled, true);
  const stopped = page.socket.messages.length;
  page.clock.advance(1000);
  page.press("up", 2);
  assert.equal(page.socket.messages.length, stopped);
  // Recovery supplies a new token but never revives the previous pointer.
  page.control();
  page.clock.advance(150);
  page.release("cw", 1);
  assert.equal(page.socket.messages.length, stopped);
  page.press("up", 3);
  assert.match(page.socket.wireMessages.at(-1), /^press:\d+:\d+:drive:up$/);
});

test("acknowledgements and incomplete or malformed telemetry cannot renew lifetime", () => {
  const page = controller(false);
  page.press("cw", 1);
  page.clock.advance(900);
  page.socket.receive({ type: "control", controller: true, azimuth: "cw",
    network_ready: true, motion_fault: null });
  page.socket.receive({ type: "position", a0_mv: 2, a1_mv: 3 });
  page.socket.receive({ type: "freshness", token: 12 });
  page.socket.receive({ type: "freshness", token: "invalid" });
  for (const invalid of [
    { ...page.telemetry, feedback: null, freshness: { token: "12" } },
    { ...page.telemetry, freshness: { token: 12 } },
    { ...page.telemetry, freshness: { token: "invalid" } },
  ]) page.socket.receive({ type: "telemetry", ...invalid });
  page.clock.advance(100);
  assert.equal(page.ids.get("status").textContent, "telemetry lost");
  assert.equal(page.socket.messages.at(-1), "stop");
  page.socket.receive({ type: "position", a0_mv: 4, a1_mv: 5 });
  assert.equal(page.ids.get("a1").textContent, "--");
});

test("one telemetry snapshot commits position, health and ownership together", () => {
  const page = controller(false);
  page.freshness({
    position: { type: "position", a0_mv: 901, a1_mv: 1200 },
    feedback: { type: "feedback", azimuth_tenths: 1234, elevation_tenths: 456, control_state: "idle" },
    control: { type: "control", controller: false, azimuth: "ccw", elevation: null, network_ready: true, motion_fault: null, duty_remaining_ms: [180000, 180000], duty_cooldown_ms: [0, 0] },
  });
  assert.equal(page.ids.get("a0").textContent, "901");
  assert.equal(page.ids.get("azimuth").textContent, "123.4");
  assert.equal(page.ids.get("azimuth-drive").textContent, "CCW");
  assert.equal(page.ids.get("take").disabled, false);
  assert.equal(page.buttons.get("cw").disabled, true);
  page.socket.receive({ type: "position", a0_mv: 999, a1_mv: 888 });
  assert.equal(page.ids.get("a0").textContent, "901", "obsolete partial frames are not assembled");
});

test("unknown boot duty displays a rest countdown and permits movement only after fresh readiness", () => {
  const page = controller(false);
  page.freshness({ control: { ...page.telemetry.control,
    duty_remaining_ms: [0, 0], duty_cooldown_ms: [900000, 900000] } });
  assert.equal(page.ids.get("control-state").textContent, "resting");
  assert.match(page.ids.get("control-status").textContent, /Azimuth motor needs 15:00/);
  assert.match(page.ids.get("elevation-duty").textContent, /15:00 remaining/);
  page.press("cw", 1);
  assert.deepEqual(page.socket.messages, []);
  page.freshness({ control: { ...page.telemetry.control, duty_cooldown_ms: [61000, 61000] } });
  assert.match(page.ids.get("azimuth-duty").textContent, /1:01 remaining/);
  page.freshness({ control: { ...page.telemetry.control,
    duty_remaining_ms: [180000, 180000], duty_cooldown_ms: [0, 0] } });
  assert.equal(page.buttons.get("cw").disabled, false);
  assert.match(page.ids.get("azimuth-duty").textContent, /3:00 on-time/);
  page.clock.advance(config.refreshMs);
  assert.deepEqual(page.socket.messages, []);
  page.press("cw", 2);
  assert.equal(page.socket.messages.at(-1), "drive:cw");
});

test("one exhausted axis clears its hold while the other keeps its existing input", () => {
  const page = controller(false);
  page.press("cw", 1);
  page.press("up", 2);
  page.freshness({ control: { ...page.telemetry.control, elevation: "up",
    duty_remaining_ms: [100, 120000], duty_cooldown_ms: [900000, 0] } });
  assert.equal(page.socket.messages.at(-1), "drive:up");
  assert.equal(page.buttons.get("cw").disabled, true);
  assert.equal(page.buttons.get("ccw").disabled, true);
  assert.equal(page.buttons.get("up").disabled, false);
  assert.equal(page.buttons.get("cw").classList.contains("held"), false);
  assert.equal(page.buttons.get("up").classList.contains("held"), true);
  page.freshness({ control: { ...page.telemetry.control,
    duty_remaining_ms: [180000, 120000], duty_cooldown_ms: [0, 0] } });
  page.clock.advance(config.refreshMs);
  assert.equal(page.socket.messages.at(-1), "drive:up", "cooldown recovery never restores the old azimuth hold");
  page.press("cw", 3);
  assert.equal(page.socket.messages.at(-1), "drive:cw+up");
});

test("duty acknowledgements and malformed duty telemetry cannot renew the observation", () => {
  const page = controller(false);
  page.press("cw", 1);
  page.clock.advance(900);
  page.socket.receive({ ...page.telemetry.control,
    duty_remaining_ms: [0, 180000], duty_cooldown_ms: [900000, 0] });
  assert.equal(page.socket.messages.at(-1), "stop");
  for (const invalid of [undefined, [0], [-1, 0], [null, 0], [0.5, 0], [Number.MAX_SAFE_INTEGER + 1, 0]]) {
    page.socket.receive({ type: "telemetry", ...page.telemetry,
      control: { ...page.telemetry.control, duty_cooldown_ms: invalid }, freshness: { token: "20" } });
  }
  page.clock.advance(100);
  assert.equal(page.ids.get("status").textContent, "telemetry lost");
  assert.equal(page.ids.get("azimuth-duty").textContent, "Motor allowance unknown");
  assert.equal(page.buttons.get("up").disabled, true);
});

test("a snapshot without a freshness proof clears a hold and cannot admit movement", () => {
  const page = controller(false);
  page.press("cw", 1);
  page.socket.receive({ type: "telemetry", ...page.telemetry, freshness: null });
  assert.equal(page.socket.messages.at(-1), "stop");
  assert.equal(page.buttons.get("up").disabled, true);
  page.freshness();
  page.release("cw", 1);
  page.clock.advance(config.refreshMs);
  assert.deepEqual(page.socket.messages, ["drive:cw", "stop"]);
});

test("callbacks from a replaced socket cannot change the new session", () => {
  const page = controller(false);
  const old = page.socket;
  old.close();
  page.clock.advance(400);
  page.socket.open();
  page.control(false);
  old.receive({ type: "control", controller: true, network_ready: true, azimuth: "cw" });
  old.close();
  page.press("up", 2);
  assert.equal(page.buttons.get("cw").disabled, true);
  assert.equal(page.ids.get("azimuth-drive").textContent, "IDLE");
  assert.deepEqual(page.socket.messages, []);
  page.clock.advance(400);
  assert.equal(page.sockets.length, 2, "a stale close does not schedule another connection");
});

test("suspended timers and server rejection both require a new physical press", () => {
  const page = controller(false);
  page.press("cw", 1);
  // Skip timer callbacks, as a suspended browser would, then deliver the
  // queued application traffic before any resumed timer can run.
  page.clock.now = 2000;
  page.freshness();
  assert.equal(page.socket.messages.at(-1), "stop");
  assert.equal(page.socket.messages.filter((message) => message === "drive:cw").length, 1);
  page.press("up", 2);
  page.socket.receive({ type: "input_required" });
  assert.equal(page.buttons.get("up").disabled, true);
  page.freshness();
  page.release("up", 2);
  assert.equal(page.socket.messages.at(-1), "stop");
  page.press("down", 3);
  assert.match(page.socket.wireMessages.at(-1), /^press:\d+:\d+:drive:down$/);
});

test("network and motion inhibition disable movement and show the latched cause", () => {
  const page = controller();
  page.press("cw", 1);
  page.socket.receive({ type: "control", controller: true, azimuth: null,
    network_ready: false, motion_fault: null });
  assert.equal(page.socket.messages.at(-1), "stop");
  assert.equal(page.buttons.get("up").disabled, true);
  assert.equal(page.buttons.get("cw").classList.contains("held"), false);
  assert.match(page.ids.get("control-status").textContent, /network connection is not ready/);
  page.socket.receive({ type: "control", controller: true, azimuth: null,
    network_ready: true, motion_fault: "azimuth_no_progress" });
  assert.equal(page.ids.get("health").textContent, "azimuth no progress");
  assert.equal(page.buttons.get("up").disabled, true);
  assert.match(page.ids.get("control-status").textContent, /Movement locked: azimuth no progress/);
});

function trackingFixture(overrides = {}) {
  const pass = forecastFixture().passes[0];
  return idleTracking({
    revision: "9007199254740993", operator: "9007199254740995",
    selection: { id: "9007199254740997", norad_id: pass.norad_id,
      aos_unix_s: pass.aos_unix_s, los_unix_s: pass.los_unix_s, associated: true },
    plan: { status: "supported", reason: null, preview: {
      route: "over-the-top", azimuth_turns: 1, prepare_unix_s: pass.aos_unix_s - 60,
      preparation_lead_ms: 60000, bound_deg: 2.8, tolerance_deg: 3, reserve_deg: 1,
      reception: { aos_unix_s: pass.aos_unix_s, duration_ms: 120000, windows: [[0,120000]], missed: null },
    } },
    ...overrides,
  });
}
const trackClick = (ui, action) => ui.ids.get(`tracking-${action}`).dispatch("click");
const trackingMessages = (ui) => ui.socket.wireMessages.filter((m) => m.startsWith("track:"));
const ackTracking = (ui, sequence, ok = true, error = null) => ui.socket.receive({ type: "tracking_result", sequence: String(sequence), ok, revision: "9007199254740998", error });

test("only the controller can select a pass and selection sends exact identity without arming", async () => {
  const ui = dashboard();
  ui.socket.open();
  ui.freshness({ tracking: idleTracking() });
  await ui.forecast(forecastFixture());
  const select = () => ui.ids.get("pass-rows").children[0].children[4].children[0];
  assert.ok(select().disabled);
  select().dispatch("click");
  assert.deepEqual(trackingMessages(ui), []);
  ui.control();
  assert.equal(select().disabled, false);
  select().dispatch("click");
  const pass = forecastFixture().passes[0];
  assert.match(trackingMessages(ui)[0], new RegExp(`^track:\\d+:1:1:0:select:${pass.norad_id}:${pass.aos_unix_s}:${pass.los_unix_s}$`));
  assert.equal(trackingMessages(ui).length, 1);
  assert.ok(ui.ids.get("tracking-arm").disabled, "wait for the device plan");
  assert.equal(ui.ids.get("manual-controls").hidden, false, "selection is a preview");
  ackTracking(ui, 1);
  ui.freshness({ tracking: trackingFixture() });
  assert.equal(select().textContent, "Selected");
  assert.equal(select().attributes.get("aria-pressed"), "true");
});

test("arming clears held input, uses exact 64-bit revision and lease, and awaits device mode", () => {
  const ui = controller(false);
  ui.freshness({ tracking: trackingFixture() });
  ui.press("cw", 1);
  trackClick(ui, "arm");
  assert.equal(ui.socket.wireMessages.at(-2), "stop");
  assert.match(trackingMessages(ui)[0], /^track:\d+:9007199254740995:1:9007199254740993:arm$/);
  assert.equal(ui.ids.get("manual-controls").hidden, false);
  assert.ok(ui.buttons.get("cw").disabled);
  trackClick(ui, "arm");
  assert.equal(trackingMessages(ui).length, 1, "double click cannot repeat intent");
  ackTracking(ui, 1);
  assert.ok(ui.buttons.get("cw").disabled, "acknowledgement is not a mode observation");
  ui.freshness({ tracking: trackingFixture({ mode: "tracking", phase: "armed" }) });
  assert.equal(ui.ids.get("manual-controls").hidden, true);
  assert.match(ui.ids.get("tracking-schedule").textContent, /Prepare now requests a new feasibility check/);
  ui.press("up", 2);
  ui.clock.advance(config.refreshMs);
  assert.equal(trackingMessages(ui).length, 1);
  trackClick(ui, "prepare");
  assert.match(trackingMessages(ui).at(-1), /:2:9007199254740993:prepare$/);
});

test("partial reception and automatic plan replacement are visible before arming", () => {
  const ui = controller();
  const t = trackingFixture();
  t.plan.status = "partial";
  t.plan.preview.reception.windows = [[20000,120000]];
  t.plan.preview.reception.missed = [0,20000];
  ui.freshness({ tracking: t });
  assert.match(ui.ids.get("tracking-plan").textContent, /Partial reception supported/);
  assert.match(ui.ids.get("tracking-reception").textContent, /Missed interval:.*0:20/);
  assert.match(ui.ids.get("tracking-arm").textContent, /partial pass/);
  ui.freshness({ tracking: { ...t, revision: "9007199254741001", plan: { status: "planning", reason: null, preview: null } } });
  assert.ok(ui.ids.get("tracking-arm").disabled);
  trackClick(ui, "arm");
  assert.deepEqual(trackingMessages(ui), []);
  ui.freshness({ tracking: { ...t, revision: "9007199254741002", plan: { status: "blocked", reason: "Unsupported(Some(PointingBudget))", preview: null } } });
  assert.match(ui.ids.get("tracking-plan").textContent, /Predicted pointing error exceeds/);
  assert.equal(ui.ids.get("tracking-reception").textContent, "", "never retain an older successful preview");
  ui.freshness({ tracking: { ...t, revision: "9007199254741003", plan: { status: "blocked", reason: "Unsupported(Some(Simulation(TravelLimit(Azimuth))))", preview: null } } });
  assert.match(ui.ids.get("tracking-plan").textContent, /Tracking blocked: Unsupported Some Simulation Travel Limit Azimuth\./, "an unexplained leaf shows its full path");
});

test("cancel overrides a pending request and remains available with stale or faulty telemetry", () => {
  const ui = controller(false);
  ui.freshness({ tracking: trackingFixture({ mode: "tracking", phase: "armed" }) });
  trackClick(ui, "prepare");
  ui.clock.advance(config.telemetryTimeoutMs + 1);
  assert.ok(ui.ids.get("tracking-prepare").disabled);
  assert.equal(ui.ids.get("tracking-cancel").disabled, false);
  trackClick(ui, "cancel");
  assert.match(trackingMessages(ui).at(-1), /^track:0:9007199254740995:2:9007199254740993:cancel$/);
  ackTracking(ui, 1, false, "Rejected(Lifecycle(StateRevision))");
  assert.match(ui.ids.get("tracking-message").textContent, /Waiting for device acknowledgement/);
  ackTracking(ui, 2);
  ui.freshness({ tracking: trackingFixture({ phase: "cancelled", termination: { reason: "Cancelled", run: null, event: "9007199254740997" } }) });
  assert.equal(ui.ids.get("manual-controls").hidden, false);
  assert.match(ui.ids.get("tracking-state").textContent, /Cancelled by controller/);
  assert.equal(ui.buttons.get("cw").classList.contains("held"), false);
});

test("returning to Manual explicitly cancels tracking and never restores old holds", () => {
  const ui = controller();
  ui.freshness({ tracking: trackingFixture({ mode: "tracking", phase: "following", run: "7" }) });
  trackClick(ui, "manual");
  assert.match(trackingMessages(ui)[0], /:manual$/);
  assert.equal(ui.ids.get("manual-controls").hidden, true);
  ackTracking(ui, 1);
  ui.freshness({ tracking: trackingFixture({ phase: "cancelled" }) });
  assert.equal(ui.ids.get("manual-controls").hidden, false);
  ui.clock.advance(config.refreshMs * 2);
  assert.equal(ui.socket.messages.filter((m) => m.startsWith("drive:")).length, 0);
});

test("tracking survives browser hiding and reconnect without replaying any action", () => {
  const ui = controller();
  const running = trackingFixture({ mode: "tracking", phase: "following", run: "7" });
  ui.freshness({ tracking: running });
  for (const event of ["blur", "pagehide", "beforeunload"]) ui.window.dispatch(event);
  ui.document.hidden = true;
  ui.document.dispatch("visibilitychange");
  assert.deepEqual(ui.socket.wireMessages, []);
  const old = ui.socket;
  old.close();
  ui.clock.advance(400);
  ui.socket.open();
  ui.document.hidden = false;
  ui.document.dispatch("visibilitychange");
  ui.freshness({ tracking: running });
  assert.match(ui.ids.get("tracking-state").textContent, /Tracking · following · viewer/);
  for (const action of ["arm", "prepare", "cancel", "manual"]) {
    assert.ok(ui.ids.get(`tracking-${action}`).disabled);
    trackClick(ui, action);
  }
  old.receive({ type: "tracking_result", sequence: "1", ok: true });
  assert.deepEqual(ui.socket.wireMessages, []);
  assert.equal(ui.ids.get("manual-controls").hidden, true);
  ui.control();
  trackClick(ui, "cancel");
  assert.equal(trackingMessages(ui).length, 1, "a fresh leased owner can interrupt the same run");
});

test("pointing shows device-computed spherical error at the matched sample time and hides stale values", () => {
  const ui = controller(false);
  const t = trackingFixture({ mode: "tracking", phase: "following", run: "7", feedback_age_ms: 100,
    deviation: { run: "7", warning: true, maximum_deg: 4.2, outside_ms: 1200,
      current: { age_ms: 100, desired: [1,60], measured: [181,120], degrees: 4.2, uncertainty_deg: 1.1 } } });
  ui.freshness({ tracking: t });
  assert.match(ui.ids.get("tracking-deviation").textContent, /Pointing warning · Spherical deviation 4.20°/);
  assert.match(ui.ids.get("tracking-pointing").textContent, /desired sky 1.00° az \/ 60.00° el; measured dish 181.00° az \/ 120.00° el/);
  ui.clock.advance(config.feedbackMaxAgeMs - 100);
  assert.match(ui.ids.get("tracking-deviation").textContent, /unavailable.*stale/);
  assert.equal(ui.ids.get("tracking-pointing").textContent, "");
  ui.freshness({ tracking: { ...t, feedback_stale: true } });
  assert.match(ui.ids.get("tracking-deviation").textContent, /stale or invalid/);
});

test("recovery shows its fixed deadline, intercept estimate, retries and retained final metrics", () => {
  const ui = controller(false);
  const t = trackingFixture({ mode: "tracking", phase: "reacquiring", run: "7", recovery: {
    episode: "9", cause: "Deviation", attempts: 1, planning: false, work_steps: 2000,
    remaining_ms: 28000, launch_in_ms: 0, intercept_in_ms: 5000, hold: null,
  } });
  ui.freshness({ tracking: t });
  assert.match(ui.ids.get("tracking-recovery").textContent, /Reacquiring · Measured pointing error exceeded tolerance · attempt 1.*0:28 until fixed deadline.*Predicted join in 0:05; measured recovery still required/);
  ui.freshness({ tracking: { ...t, recovery: { ...t.recovery, attempts: 2, remaining_ms: 25000, hold: "NoIntercept", intercept_in_ms: null } } });
  assert.match(ui.ids.get("tracking-recovery").textContent, /attempt 2.*holding and retrying.*0:25.*No validated intercept/);
  const summary = { termination: { reason: "ReacquisitionTimeout", event: "3", run: "7" },
    reception: t.plan.preview.reception, deviation: { run: "7", warning: false, current: null, maximum_deg: 10.5, outside_ms: 30000 } };
  ui.freshness({ tracking: trackingFixture({ phase: "stopped", termination: summary.termination, summary }) });
  assert.match(ui.ids.get("tracking-summary").textContent, /Last finished run 7: Recovery deadline reached. Peak measured error 10.50°; observed outside tolerance 0:30/);
  assert.match(ui.ids.get("tracking-summary").textContent, /predictions, not receiver-lock measurements/);
  ui.freshness({ tracking: trackingFixture({ summary }) });
  assert.match(ui.ids.get("tracking-summary").textContent, /Last finished run 7/, "new preview retains the preceding run summary");
});

test("selection follows device reassociation and survives missing HTTP forecasts", async () => {
  const ui = controller();
  const t = trackingFixture();
  await ui.forecast(forecastFixture());
  ui.freshness({ tracking: t });
  const changed = forecastFixture();
  changed.passes[0].aos_unix_s += 20;
  changed.passes[0].los_unix_s += 20;
  ui.clock.advance(15000);
  await ui.forecast(changed);
  ui.freshness({ tracking: { ...t, selection: { ...t.selection, aos_unix_s: changed.passes[0].aos_unix_s, los_unix_s: changed.passes[0].los_unix_s } } });
  assert.equal(ui.ids.get("pass-rows").children.filter((r) => r.classList.contains("selected")).length, 1);
  ui.clock.advance(15000);
  await ui.forecast(changed, false);
  assert.equal(ui.ids.get("pass-rows").children.length, 0);
  assert.match(ui.ids.get("tracking-selection").textContent, /METOP-C/, "selection comes from device tracking, independently of the row list");
  assert.deepEqual(trackingMessages(ui), []);
});

test("rejected, unacknowledged and malformed tracking messages cannot replay intent or renew authority", () => {
  const ui = controller(false);
  ui.freshness({ tracking: trackingFixture() });
  trackClick(ui, "arm");
  ackTracking(ui, 1, false, "Rejected(Lifecycle(StateRevision))");
  assert.match(ui.ids.get("tracking-message").textContent, /Plan changed; review/);
  ui.freshness({ tracking: trackingFixture() });
  trackClick(ui, "arm");
  const bad = { ...trackingFixture(), deviation: { current: {} } };
  ui.clock.advance(900);
  ui.socket.receive({ type: "telemetry", ...ui.telemetry, tracking: bad, freshness: { token: "999" } });
  ui.clock.advance(4200);
  assert.match(ui.ids.get("tracking-message").textContent, /No acknowledgement received/);
  assert.equal(trackingMessages(ui).length, 2);
  assert.ok(ui.ids.get("tracking-arm").disabled, "acknowledgements never refresh observations");
});

test("real firmware tracking snapshots render arming, independent following, reconnect and completion", () => {
  const snapshots = JSON.parse(readFileSync(new URL("./fixtures/tracking-telemetry.json", import.meta.url), "utf8"));
  const ui = dashboard("http:", false);
  ui.socket.open();
  ui.socket.receive(snapshots[0]);
  assert.match(ui.ids.get("tracking-state").textContent, /Tracking · armed · you have control/);
  assert.match(ui.ids.get("tracking-plan").textContent, /Full pass supported/);
  assert.equal(ui.ids.get("manual-controls").hidden, true);
  ui.socket.receive(snapshots[1]);
  assert.match(ui.ids.get("tracking-state").textContent, /viewer/);
  assert.ok(ui.ids.get("tracking-cancel").disabled);
  ui.socket.receive(snapshots[2]);
  assert.match(ui.ids.get("tracking-state").textContent, /preparing/);
  ui.socket.receive(snapshots[3]);
  assert.match(ui.ids.get("tracking-state").textContent, /following/);
  assert.match(ui.ids.get("tracking-deviation").textContent, /Spherical deviation/);
  assert.match(ui.ids.get("tracking-pointing").textContent, /At the same sample time/);
  ui.socket.receive(snapshots[4]);
  assert.equal(ui.ids.get("tracking-cancel").disabled, false);
  ui.socket.receive(snapshots[5]);
  assert.equal(ui.ids.get("manual-controls").hidden, false);
  assert.match(ui.ids.get("tracking-state").textContent, /complete/);
  assert.match(ui.ids.get("tracking-summary").textContent, /Pass finished/);
  assert.deepEqual(ui.socket.wireMessages, [], "reading status never becomes operator input");
});
