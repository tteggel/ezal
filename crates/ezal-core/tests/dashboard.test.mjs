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

class Element extends EventTarget {
  disabled = false;
  textContent = "";
  dataset = {};
  captures = new Set();
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

function dashboard(protocol = "http:", autoTelemetry = true) {
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
  class WebSocket extends EventTarget {
    static OPEN = 1;
    readyState = 0;
    messages = [];
    wireMessages = [];

    constructor(url) {
      super();
      this.url = url;
      sockets.push(this);
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
      this.dispatch("message", { data: JSON.stringify(message) });
    }

    close() {
      this.readyState = 3;
      this.dispatch("close");
    }
  }
  runInNewContext(scripts[0][1], {
    document,
    location: { protocol, host: "ezal.local" },
    window,
    WebSocket,
    performance: { now: () => clock.now },
    setInterval: (callback, delay) => clock.schedule(callback, delay, true),
    clearInterval: (id) => clock.jobs.delete(id),
    setTimeout: (callback, delay) => clock.schedule(callback, delay),
  }, { filename: "dashboard.html" });
  let token = 0;
  const freshness = () => {
    sockets.at(-1).receive({ type: "freshness", token: String(++token) });
  };
  if (autoTelemetry) clock.schedule(() => {
    if (sockets.at(-1).readyState === WebSocket.OPEN) freshness();
  }, 500, true);
  return {
    ids, buttons, body, window, document, clock, sockets, freshness,
    get socket() { return sockets.at(-1); },
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
  page.press("cw", 1);
  page.socket.open();
  assert.equal(page.ids.get("status").textContent, "waiting for telemetry");
  page.control(false);
  assert.equal(page.ids.get("status").textContent, "online");
  assert.equal(page.ids.get("take").disabled, false);
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

test("held commands refresh at 150 ms and stop refreshing when released", () => {
  const page = controller();
  page.press("up", 1);
  page.clock.advance(149);
  assert.deepEqual(page.socket.messages, ["drive:up"]);
  page.clock.advance(1);
  page.press("cw", 2);
  page.clock.advance(300);
  page.release("up", 1);
  page.clock.advance(150);
  page.release("cw", 2);
  page.clock.advance(1000);
  assert.deepEqual(page.socket.messages, [
    "drive:up", "drive:up", "drive:cw+up", "drive:cw+up", "drive:cw+up", "drive:cw", "drive:cw", "stop",
  ]);
});

test("button highlights and drive readouts follow applied output telemetry", () => {
  const page = controller();
  page.press("cw", 1);
  assert.equal(page.buttons.get("cw").classList.contains("active"), false);
  page.control(true, "cw", null);
  assert.equal(page.buttons.get("cw").classList.contains("active"), true);
  assert.equal(page.ids.get("azimuth-drive").textContent, "CW");
  page.release("cw", 1);
  assert.equal(page.buttons.get("cw").classList.contains("active"), true);
  page.control(true, null, null);
  assert.equal(page.buttons.get("cw").classList.contains("active"), false);
  assert.equal(page.ids.get("azimuth-drive").textContent, "IDLE");
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

test("telemetry renders positions, countdown and nullable targets across modes", () => {
  const page = controller();
  page.socket.dispatch("message", { data: "{malformed" });
  page.socket.receive({ type: "position", a0_mv: 901, a1_mv: -12 });
  assert.equal(String(page.ids.get("a0").textContent), "901");
  assert.equal(String(page.ids.get("a1").textContent), "-12");
  const tracking = {
    type: "tracking", mode: "hardware-walking-skeleton", satellite: "METOP-C",
    pass: 2, phase: "pause", remaining_ms: 29500, control_state: "idle",
    azimuth_tenths: 4247, elevation_tenths: 3,
    target_azimuth_tenths: null, target_elevation_tenths: null,
  };
  page.socket.receive(tracking);
  assert.equal(page.body.classList.contains("autonomous"), true);
  assert.equal(page.ids.get("source").textContent, "METOP-C · hardware-walking-skeleton");
  assert.equal(page.ids.get("run").textContent, "pass 2");
  assert.equal(page.ids.get("phase").textContent, "pause · 0:30");
  assert.equal(page.ids.get("health").textContent, "idle");
  assert.equal(page.ids.get("azimuth").textContent, "424.7");
  assert.equal(page.ids.get("elevation").textContent, "0.3");
  assert.equal(page.ids.get("target-azimuth").textContent, "--");
  assert.equal(page.ids.get("target-elevation").textContent, "--");
  page.socket.receive({ ...tracking, mode: "manual", satellite: null, target_azimuth_tenths: 201 });
  assert.equal(page.body.classList.contains("autonomous"), false);
  assert.equal(page.ids.get("source").textContent, "manual · manual");
  assert.equal(page.ids.get("run").textContent, "manual");
  assert.equal(page.ids.get("phase").textContent, "calibration");
  assert.equal(page.ids.get("target-azimuth").textContent, "20.1");
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
  page.socket.receive({ type: "position", a0_mv: 900, a1_mv: 1200 });
  page.clock.advance(1000);
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

test("ordinary responses and malformed freshness messages cannot renew telemetry", () => {
  const page = controller(false);
  page.press("cw", 1);
  page.clock.advance(900);
  page.socket.receive({ type: "control", controller: true, azimuth: "cw",
    network_ready: true, motion_fault: null });
  page.socket.receive({ type: "position", a0_mv: 2, a1_mv: 3 });
  page.socket.receive({ type: "freshness", token: 12 });
  page.socket.receive({ type: "freshness", token: "invalid" });
  page.clock.advance(100);
  assert.equal(page.ids.get("status").textContent, "telemetry lost");
  assert.equal(page.socket.messages.at(-1), "stop");
  page.socket.receive({ type: "position", a0_mv: 4, a1_mv: 5 });
  assert.equal(page.ids.get("a1").textContent, "--");
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
  page.socket.receive({ type: "control", controller: true, azimuth: null,
    network_ready: true, motion_fault: "azimuth_no_progress" });
  assert.equal(page.ids.get("health").textContent, "azimuth no progress");
  assert.equal(page.buttons.get("up").disabled, true);
});
