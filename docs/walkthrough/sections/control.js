(() => {
  'use strict';
  const $ = id => document.getElementById(id);
  if (!$('control-token')) return;

  // Every constant is copied from the Rust source named beside it.
  const TELEMETRY_PERIOD = 500; // session.rs TELEMETRY_PERIOD_MS
  const REFRESH = 150;          // session.rs COMMAND_REFRESH_MS
  const TOKEN_LIFE = 1000;      // session.rs COMMAND_MAX_AGE_MS
  const LEASE = 750;            // drive.rs MOVEMENT_LEASE_MS
  const MIN_ON = 500;           // drive.rs OUTPUT_MIN_ACTIVE_MS
  const MIN_OFF = 2000;         // drive.rs OUTPUT_MIN_INACTIVE_MS (same direction and reversal)
  const NET_TIMEOUT = 300;      // network.rs NETWORK_HEALTH_TIMEOUT_MS
  const DUTY_RUN = 180000;      // duty.rs DutyConfig::DEFAULT.energized_ms
  const DUTY_REST = 900000;     // duty.rs DutyConfig::DEFAULT.rest_ms

  // --- models: plain arithmetic, no drawing -------------------------------

  // session.rs CommandFreshness, for one browser connection.
  function freshness() {
    const self = {
      tokens: [], serial: 0, last: 0, armed: null,
      issue(now) {
        self.serial += 1;
        self.tokens.push({ id: self.serial, expires: now + TOKEN_LIFE });
        if (self.tokens.length > 3) self.tokens.shift();
        return self.serial;
      },
      disarm() { self.armed = null; },
      reject() { self.armed = null; self.tokens = []; },
      admit(request, now) {
        const token = self.tokens.find(t => t.id === request.token);
        const expires = token && now < token.expires ? token.expires : null;
        const live = request.pressed || (self.armed !== null && now < self.armed);
        if (request.sequence <= self.last || !live || expires === null) {
          self.reject();
          return { ok: false, why: !live ? 'no live press' : expires === null ? 'token' : 'sequence' };
        }
        self.last = request.sequence;
        const deadline = Math.min(expires, now + LEASE);
        self.armed = deadline;
        return { ok: true, deadline, capped: expires < now + LEASE, token: token.id };
      }
    };
    return self;
  }

  // One browser holding one button across a Wi-Fi stall. The browser half follows
  // the `transition` function in dashboard.html; the board half is `freshness`.
  function simulateHold(release, stall) {
    const PRESS = 300, DELAY = 30, STALL_AT = 1100, END = 4000;
    const stallEnd = STALL_AT + stall;
    const queue = [];
    let order = 0;
    const at = (t, run) => queue.push({ t, order: order++, run });
    const lastArrival = { up: -Infinity, down: -Infinity };
    const deliver = (sent, way) => {
      const late = stall > 0 && sent + DELAY > STALL_AT && sent < stallEnd;
      const arrives = Math.max(late ? stallEnd + DELAY : sent + DELAY, lastArrival[way] + (late ? 6 : 0));
      lastArrival[way] = arrives;
      return { arrives, late };
    };

    const board = freshness();
    const browser = { token: null, until: -Infinity, held: false, sequence: 0 };
    const result = { press: PRESS, release, stallAt: STALL_AT, stallEnd, end: END, reports: [], ups: [], life: [], ended: null, gaveUp: null };
    let deadline = null, grant = null;
    const lapse = now => {
      if (deadline !== null && now >= deadline) {
        result.ended = { t: deadline, cause: 'lapsed', grant };
        deadline = null;
      }
    };
    const receive = (message, now) => {
      lapse(now);
      if (message.kind === 'stop') {
        board.disarm();
        message.ok = true;
        if (deadline !== null) result.ended = { t: now, cause: 'stop' };
        deadline = null;
        result.life.push({ t: now, until: now });
        return;
      }
      const verdict = board.admit({ token: message.token, sequence: message.sequence, pressed: message.kind === 'press' }, now);
      message.ok = verdict.ok;
      if (verdict.ok) {
        deadline = verdict.deadline;
        grant = { at: now, deadline, capped: verdict.capped, token: verdict.token };
        Object.assign(message, { deadline, capped: verdict.capped });
        result.life.push({ t: now, until: deadline });
      } else {
        if (deadline !== null) result.ended = { t: now, cause: 'refused' };
        deadline = null;
        result.life.push({ t: now, until: now });
        at(deliver(now, 'down').arrives, t => giveUp(t, 'refused'));
      }
    };
    const send = (kind, now) => {
      const message = { kind, sent: now, token: browser.token, sequence: kind === 'stop' ? null : ++browser.sequence, ...deliver(now, 'up') };
      result.ups.push(message);
      at(message.arrives, t => receive(message, t));
    };
    const giveUp = (now, why) => {
      if (browser.held) {
        browser.held = false;
        result.gaveUp = { t: now, why };
        send('stop', now);
      }
      browser.token = null;
    };
    const stale = now => { if (browser.token !== null && now >= browser.until) giveUp(now, 'stale'); };
    const repeat = now => {
      stale(now);
      if (!browser.held) return;
      send('hold', now);
      at(now + REFRESH, repeat);
    };

    for (let t = 0; t <= END; t += TELEMETRY_PERIOD) {
      at(t, now => {
        const report = { id: board.issue(now), sent: now, expires: now + TOKEN_LIFE, ...deliver(now, 'down') };
        result.reports.push(report);
        at(report.arrives, then => { stale(then); browser.token = report.id; browser.until = then + TOKEN_LIFE; });
      });
    }
    for (let t = 100; t <= END; t += 100) at(t, stale);
    at(PRESS, now => {
      stale(now);
      if (browser.token === null) return;
      browser.held = true;
      send('press', now);
      at(now + REFRESH, repeat);
    });
    at(release, now => {
      stale(now);
      if (!browser.held) return;
      browser.held = false;
      send('stop', now);
    });
    while (queue.length) {
      let first = 0;
      for (let i = 1; i < queue.length; i++) {
        if (queue[i].t < queue[first].t || (queue[i].t === queue[first].t && queue[i].order < queue[first].order)) first = i;
      }
      const event = queue.splice(first, 1)[0];
      if (event.t <= END) event.run(event.t);
    }
    lapse(END);
    return result;
  }

  // tracking/mailbox.rs CommandMailbox, with the expiry check lifecycle.rs applies on reading.
  function mailbox() {
    let pending = null;
    return {
      peek: () => pending,
      submit(note) {
        if (pending && pending.kind === 'stop') return { kept: pending, dropped: note, why: 'stop waiting' };
        const replaced = pending;
        pending = note;
        return replaced ? { kept: note, dropped: replaced, why: 'replaced' } : { kept: note };
      },
      take(now) {
        const note = pending;
        pending = null;
        if (!note || note.kind === 'stop') return { note };
        const fresh = note.until > now && note.until - now <= LEASE;
        return fresh ? { note } : { note: { kind: 'stop' }, expired: note };
      }
    };
  }

  // One axis of drive.rs (AxisRelay and ActuatorGuard) in the order actuator.rs
  // Actuator::step applies it. Position, run-time and motion checks are left out.
  function actuator() {
    const s = { target: null, actual: null, last: null, onUntil: 0, offUntil: 0, reverseUntil: 0, lease: null, generation: 1 };
    const release = now => {
      if (s.actual === null) return;
      s.last = s.actual;
      s.actual = null;
      s.offUntil = now + MIN_OFF;
      s.reverseUntil = now + MIN_OFF;
    };
    const inactiveUntil = () => (s.last !== null && s.target !== s.last ? s.reverseUntil : s.offUntil);
    const inhibit = now => { s.target = null; release(now); };
    const limit = () => (s.lease === null ? 0 : s.lease);
    const update = now => {                       // AxisRelay::update
      if (s.actual === s.target) return;
      if (s.actual !== null) {
        if (now < s.onUntil) return;
        release(now);
      }
      if (now < inactiveUntil()) return;
      if (s.target !== null && now + MIN_ON <= limit()) {
        s.actual = s.target;
        s.onUntil = now + MIN_ON;
      }
    };
    const expire = now => {
      if (s.lease === null || s.lease > now) return false;
      s.lease = null;
      inhibit(now);
      return true;
    };
    const clearIdleLease = () => { if (s.target === null && s.actual === null) s.lease = null; };
    const command = (given, now, until) => {      // ActuatorGuard::command_until
      let kind = given.kind;
      expire(now);
      if (kind !== 'stop' && given.dir !== null) {
        const deadline = Math.min(now + LEASE, until);
        if (deadline <= now) kind = 'stop'; else s.lease = deadline;
      }
      if (kind === 'stop') inhibit(now);
      else {
        if (kind === 'jog' && s.actual !== null && s.actual !== given.dir) inhibit(now);
        s.target = given.dir;
        update(now);
      }
      clearIdleLease();
    };
    const ready = (now, network) => now - network.seen < NET_TIMEOUT;
    return {
      state: s,
      inactiveUntil,
      step(now, input) {                          // Actuator::step
        const network = input.network || { seen: now, generation: 1 };
        const up = ready(now, network);
        if (!up || network.generation !== s.generation) command({ kind: 'stop' }, now, Infinity);
        s.generation = network.generation;
        if (input.command) command(up ? input.command : { kind: 'stop' }, now, input.command.until === undefined ? Infinity : input.command.until);
        else {
          if (!expire(now)) update(now);
          clearIdleLease();
        }
      },
      next(now, network) {                        // ActuatorGuard::next_transition_ms, plus the network deadline
        const times = [];
        if (s.actual !== s.target) {
          if (s.actual !== null) times.push(Math.max(s.onUntil, now));
          else {
            const start = Math.max(inactiveUntil(), now);
            if (start + MIN_ON <= limit()) times.push(start);
          }
        }
        if (s.lease !== null) times.push(s.lease);
        if (network && ready(now, network)) times.push(network.seen + NET_TIMEOUT);
        return times.length ? Math.min(...times) : null;
      }
    };
  }

  // duty.rs AxisDuty for one axis, driven in whole actions rather than service passes.
  function dutyTank() {
    const s = { used: DUTY_RUN, idle: 0, startup: true };
    return {
      state: s,
      left: () => Math.max(0, DUTY_RUN - s.used),
      boot() { s.used = DUTY_RUN; s.idle = 0; s.startup = true; },
      run(ms) {
        const left = Math.max(0, DUTY_RUN - s.used);
        if (left < MIN_ON) return { refused: true };
        const ran = Math.min(ms, left);
        s.used += ran;
        s.idle = 0;
        s.startup = false;
        return { ran, cut: ran < ms };
      },
      rest(ms) {
        const before = s.idle;
        s.idle += ms;
        const refilled = before < DUTY_REST && s.idle >= DUTY_REST && s.used > 0;
        if (s.idle >= DUTY_REST) { s.used = 0; s.startup = false; }
        return { refilled, after: refilled ? DUTY_REST - before : null };
      },
      skip() {
        if (!s.startup) return false;
        s.startup = false;
        s.used = 0;
        return true;
      }
    };
  }
  // --- end of models ------------------------------------------------------

  const FONT = "'Trebuchet MS','Segoe UI',sans-serif";
  const C = { ink: '#20323e', navy: '#18354a', az: '#245bce', warn: '#925100', ok: '#11643e', stop: '#a33317', pale: '#edf4fa', line: '#c7d8e5', muted: '#506574', soft: '#8fa3b3', paper: '#fff' };
  const calm = window.matchMedia('(prefers-reduced-motion: reduce)');
  const redraws = [];
  let unit = 12;                // label size in CSS pixels; a little larger on a wide canvas

  // Size a canvas to its box at the device's pixel density and hand back a
  // context that draws in CSS pixels.
  function surface(canvas, height) {
    const w = Math.max(200, Math.round(canvas.clientWidth || 600));
    const ratio = window.devicePixelRatio || 1;
    unit = w < 620 ? 12 : 13;
    canvas.width = Math.round(w * ratio);
    canvas.height = Math.round(height * ratio);
    const g = canvas.getContext('2d');
    g.setTransform(ratio, 0, 0, ratio, 0, 0);
    g.clearRect(0, 0, w, height);
    g.textBaseline = 'middle';
    g.lineJoin = 'round';
    return { g, w };
  }
  function stroke(g, points, color, width = 1, dash = []) {
    g.beginPath();
    g.setLineDash(dash);
    points.forEach(([x, y], i) => (i ? g.lineTo(x, y) : g.moveTo(x, y)));
    g.strokeStyle = color;
    g.lineWidth = width;
    g.stroke();
    g.setLineDash([]);
  }
  function setFont(g, o = {}) { g.font = `${o.bold ? 'bold ' : ''}${o.size || unit}px ${FONT}`; }
  function text(g, string, x, y, o = {}) {
    setFont(g, o);
    g.fillStyle = o.color || C.ink;
    g.textAlign = o.align || 'left';
    g.fillText(string, x, y);
  }
  function measure(g, string, o = {}) { setFont(g, o); return g.measureText(string).width; }
  function rounded(g, x, y, w, h, r = 3) {
    g.beginPath();
    if (w < 2 * r) r = Math.max(0, w / 2);
    g.moveTo(x + r, y);
    g.arcTo(x + w, y, x + w, y + h, r);
    g.arcTo(x + w, y + h, x, y + h, r);
    g.arcTo(x, y + h, x, y, r);
    g.arcTo(x, y, x + w, y, r);
    g.closePath();
  }
  function block(g, x, y, w, h, fill, outline, width = 1, r = 3) {
    rounded(g, x, y, w, h, r);
    if (fill) { g.fillStyle = fill; g.fill(); }
    if (outline) { g.strokeStyle = outline; g.lineWidth = width; g.stroke(); }
  }
  function hatch(g, x, y, w, h, color, gap = 6) {
    g.save();
    g.beginPath();
    g.rect(x, y, w, h);
    g.clip();
    g.beginPath();
    for (let i = -h; i < w; i += gap) { g.moveTo(x + i, y + h); g.lineTo(x + i + h, y); }
    g.strokeStyle = color;
    g.lineWidth = 1;
    g.stroke();
    g.restore();
  }
  function cross(g, x, y, r, color, width = 2) {
    stroke(g, [[x - r, y - r], [x + r, y + r]], color, width);
    stroke(g, [[x - r, y + r], [x + r, y - r]], color, width);
  }
  function dot(g, x, y, r, fill, outline) {
    g.beginPath();
    g.arc(x, y, r, 0, 2 * Math.PI);
    if (fill) { g.fillStyle = fill; g.fill(); }
    if (outline) { g.strokeStyle = outline; g.lineWidth = 1.5; g.stroke(); }
  }
  const ms = value => `${Math.round(value)} ms`;
  const plural = (n, word) => `${n} ${word}${n === 1 ? '' : 's'}`;
  function say(element, string) { if (element.textContent !== string) element.textContent = string; }

  // --- chapter 3, figure 1: one press against one token ---------------------
  {
    const canvas = $('control-token-canvas'), slider = $('control-token-arrival');
    const T0 = -60, T1 = 1420;
    const draw = () => {
      const arrival = Number(slider.value);
      const session = freshness();
      const verdict = session.admit({ token: session.issue(0), sequence: 1, pressed: true }, arrival);
      const life = verdict.ok ? verdict.deadline - arrival : 0;
      const fits = verdict.ok && arrival + MIN_ON <= verdict.deadline;
      const { g, w } = surface(canvas, 178);
      const x = t => 14 + (t - T0) / (T1 - T0) * (w - 28);
      const axis = 152;

      stroke(g, [[x(T0), axis + .5], [x(T1), axis + .5]], C.line);
      for (let t = 0; t <= 1250; t += 250) {
        stroke(g, [[x(t), axis], [x(t), axis + (t % 500 ? 4 : 7)]], C.muted);
        if (t % 500 === 0) text(g, t ? `${t} ms` : '0', x(t), axis + 17, { color: C.muted, align: 'center' });
      }

      text(g, 'report sent', x(0), 11, { color: C.navy });
      stroke(g, [[x(0), 19], [x(0), 30]], C.navy, 2);
      block(g, x(0), 30, x(TOKEN_LIFE) - x(0), 22, C.pale, C.navy);
      text(g, w > 520 ? 'its token, good for 1000 ms' : 'its token', x(0) + 8, 41, { color: C.navy });
      stroke(g, [[x(TOKEN_LIFE), 22], [x(TOKEN_LIFE), 60]], C.warn, 3);
      text(g, w > 520 ? 'token expires' : 'expires', x(TOKEN_LIFE) + 7, 41, { color: C.warn, bold: true });

      const px = x(arrival), colour = verdict.ok ? C.ok : C.stop;
      stroke(g, [[px, 64], [px, axis]], colour, 2);
      g.beginPath();
      g.moveTo(px - 7, 62); g.lineTo(px + 7, 62); g.lineTo(px, 73); g.closePath();
      g.fillStyle = colour;
      g.fill();
      const flip = arrival > 700;
      text(g, `press arrives, ${ms(arrival)}`, px + (flip ? -11 : 11), 69, { color: colour, bold: true, align: flip ? 'right' : 'left' });

      if (verdict.ok) {
        g.save();
        g.beginPath();
        g.rect(0, 0, x(T1), 178);
        g.clip();
        const end = x(verdict.deadline), full = x(arrival + LEASE);
        block(g, px, 86, end - px, 20, C.az, null, 1, 2);
        if (verdict.capped) {
          g.setLineDash([4, 3]);
          g.strokeStyle = C.az;
          g.lineWidth = 1;
          g.strokeRect(end + .5, 86.5, full - end, 19);
          g.setLineDash([]);
        }
        const note = `request lives ${ms(life)}`;
        const width = measure(g, note, { bold: true });
        if (end - px > width + 14) text(g, note, px + 7, 96, { bold: true, color: '#fff' });
        else if (px - width - 10 > 4) text(g, note, px - 8, 96, { bold: true, color: C.az, align: 'right' });
        else text(g, note, (verdict.capped ? full : end) + 8, 96, { bold: true, color: C.az });
        const tint = fits ? C.ok : C.stop, need = x(arrival + MIN_ON);
        stroke(g, [[px, 114], [px, 121], [need, 121], [need, 114]], tint, 2);
        g.restore();
        const verdictText = `a motor start needs ${MIN_ON} ms: ${fits ? 'fits' : 'does not fit'}`;
        const right = px + 7 + measure(g, verdictText) > w - 8;
        const span = measure(g, verdictText);
        g.fillStyle = C.paper;
        g.fillRect(right ? w - 12 - span : px + 3, 127, span + 8, 18);
        text(g, verdictText, right ? w - 8 : px + 7, 136, { color: tint, align: right ? 'right' : 'left' });
      } else {
        cross(g, px, 96, 6, C.stop, 2.5);
        text(g, 'refused: the token has expired', px - 13, 96, { color: C.stop, bold: true, align: 'right' });
      }

      const told = !verdict.ok
        ? `The press arrives ${ms(arrival)} after the report. Refused: its token expired at ${TOKEN_LIFE} ms, so the board cannot tell how old this press is.`
        : !verdict.capped
          ? `The press arrives ${ms(arrival)} after the report. Accepted, with a full ${LEASE} ms: the request will lapse at ${ms(verdict.deadline)} unless something renews it.`
          : fits
            ? `The press arrives ${ms(arrival)} after the report. Accepted, but the request may not outlive the token: it gets ${ms(life)} instead of ${LEASE}, lapsing at ${TOKEN_LIFE} ms. That still covers the ${MIN_ON} ms a motor start needs.`
            : `The press arrives ${ms(arrival)} after the report. Accepted, but with only ${ms(life)} before the token expires. That is too little to start a motor; a later message quoting a newer token has to extend it first.`;
      say($('control-token-readout'), told);
      $('control-token-value').textContent = ms(arrival);
      slider.setAttribute('aria-valuetext', `${arrival} milliseconds after the report`);
      canvas.setAttribute('aria-label', `Timeline of one status report and one press. The report leaves the board at 0 and its token expires at ${TOKEN_LIFE} milliseconds. ${told}`);
    };
    slider.addEventListener('input', draw);
    slider.addEventListener('change', draw);
    // Dragging on the drawing moves the press; the slider is the keyboard route.
    const place = event => {
      const box = canvas.getBoundingClientRect();
      const t = T0 + (event.clientX - box.left - 14) / (box.width - 28) * (T1 - T0);
      slider.value = Math.round(Math.min(1200, Math.max(0, t)) / 10) * 10;
      draw();
    };
    let dragging = false;
    canvas.addEventListener('pointerdown', event => {
      if (event.button !== 0) return;
      dragging = true;
      try { canvas.setPointerCapture(event.pointerId); } catch (_) { /* not a live pointer */ }
      if (event.pointerType !== 'touch') place(event);
    });
    canvas.addEventListener('pointermove', event => { if (dragging) place(event); });
    for (const ending of ['pointerup', 'pointercancel', 'lostpointercapture']) canvas.addEventListener(ending, () => { dragging = false; });
    canvas.style.cursor = 'ew-resize';
    redraws.push(draw);
  }

  // --- chapter 3, figure 2: holding the button across a stall ---------------
  {
    const canvas = $('control-hold-canvas'), releaseAt = $('control-hold-release'), stallFor = $('control-hold-stall');
    const draw = () => {
      const run = simulateHold(Number(releaseAt.value), Number(stallFor.value));
      const { g, w } = surface(canvas, 302);
      const wide = w > 560;
      const T0 = -90, T1 = run.end + 40;
      const x = t => 12 + (t - T0) / (T1 - T0) * (w - 24);
      const board = 36, browser = 134, top = 198, floor = 268;

      if (run.stallEnd > run.stallAt) {
        const a = x(run.stallAt), b = x(run.stallEnd);
        g.fillStyle = '#f1f3f5';
        g.fillRect(a, board - 12, b - a, browser - board + 24);
        hatch(g, a, board - 12, b - a, browser - board + 24, '#cfd6dc', 7);
        const name = b - a > 96 ? 'Wi-Fi stalled' : b - a > 44 ? 'stall' : '';
        if (name) {
          g.fillStyle = '#f1f3f5';
          g.fillRect(a + 4, board + 21, measure(g, name, { bold: true }) + 8, 18);
          text(g, name, a + 8, board + 30, { bold: true, color: C.muted });
        }
      }

      // The board: a report every 500 ms, each with a token that lasts 1000 ms.
      text(g, wide ? 'The board sends a status report every 500 ms' : 'The board: a report every 500 ms', 12, 10, { color: C.navy, bold: true });
      stroke(g, [[x(T0), board + .5], [x(T1), board + .5]], C.navy);
      run.reports.forEach((report, i) => {
        const row = board + 7 + (i % 2) * 9, a = x(report.sent), b = Math.min(x(report.expires), x(T1));
        g.fillStyle = '#dfe9f3';
        g.fillRect(a, row, b - a, 6);
        if (report.expires <= run.end) stroke(g, [[b, row - 2], [b, row + 8]], C.warn, 2.5);
        if (report.arrives <= run.end) stroke(g, [[x(report.sent), board], [x(report.arrives), browser]], C.soft, 1);
        dot(g, a, board - 6, 4, C.navy);
        if (wide) text(g, String(report.id), a + 7, board - 9, { size: unit - 1, color: C.navy });
      });

      // The browser: a press, a hold every 150 ms, a stop.
      stroke(g, [[x(T0), browser + .5], [x(T1), browser + .5]], C.navy);
      const fingerEnd = run.gaveUp ? Math.min(run.gaveUp.t, run.release) : run.release;
      g.fillStyle = 'rgba(24,53,74,.3)';
      g.fillRect(x(run.press), browser + 7, x(fingerEnd) - x(run.press), 7);
      if (run.release > fingerEnd) hatch(g, x(fingerEnd), browser + 7, x(run.release) - x(fingerEnd), 7, C.soft, 4);
      text(g, wide ? 'Your browser, while you hold the button: a hold every 150 ms' : 'Your browser: a hold every 150 ms', 12, browser + 28, { color: C.navy, bold: true });
      for (const message of run.ups) {
        if (message.arrives > run.end) continue;
        const refused = message.ok === false, stop = message.kind === 'stop';
        const a = x(message.sent), b = x(message.arrives);
        stroke(g, [[a, browser], [b, board]], refused ? C.stop : stop ? C.navy : C.az, message.kind === 'press' ? 2.2 : 1.2, stop ? [3, 3] : []);
        if (message.kind === 'press') dot(g, a, browser, 5, C.az);
        else if (stop) block(g, a - 4, browser - 4, 8, 8, C.paper, C.navy, 1.5, 0);
        else stroke(g, [[a, browser], [a, browser + 5]], C.az, 1.5);
        if (refused) cross(g, b, board, 4, C.stop);
        else if (stop) block(g, b - 3.5, board - 3.5, 7, 7, C.navy, null, 1, 0);
        else dot(g, b, board, 3, C.ok);
      }

      // How long the request has left: each accepted message lifts it, time wears it down.
      const y = left => floor - Math.min(left, LEASE) / LEASE * (floor - top);
      text(g, 'Time left on the request', 12, top - 14, { color: C.az, bold: true });
      stroke(g, [[x(0), top + .5], [x(T1), top + .5]], C.line, 1, [2, 3]);
      text(g, `${LEASE} ms`, x(T1), top - 9, { size: unit - 1, color: C.muted, align: 'right' });
      stroke(g, [[x(T0), floor + .5], [x(T1), floor + .5]], C.navy);
      for (let t = 0; t <= run.end; t += 500) {
        stroke(g, [[x(t), floor], [x(t), floor + (t % 1000 ? 4 : 7)]], C.muted);
        if (t % 1000 === 0) text(g, t ? `${t / 1000} s` : '0', x(t), floor + 18, { color: C.muted, align: 'center' });
      }
      const outline = [];
      run.life.forEach((point, i) => {
        if (point.until <= point.t && !outline.length) return;
        const until = Math.min(point.until, i + 1 < run.life.length ? run.life[i + 1].t : run.end);
        if (!outline.length || outline[outline.length - 1][0] < x(point.t)) outline.push([x(point.t), floor]);
        outline.push([x(point.t), y(point.until - point.t)], [x(until), y(point.until - until)]);
      });
      while (outline.length > 1 && outline[outline.length - 1][1] === floor && outline[outline.length - 2][1] === floor) outline.pop();
      if (outline.length) {
        g.beginPath();
        outline.forEach(([px, py], i) => (i ? g.lineTo(px, py) : g.moveTo(px, py)));
        g.lineTo(outline[outline.length - 1][0], floor);
        g.closePath();
        g.fillStyle = 'rgba(36,91,206,.18)';
        g.fill();
        stroke(g, outline, C.az, 2);
      }
      if (run.ended) {
        const ex = x(run.ended.t);
        stroke(g, [[ex, top - 4], [ex, floor]], C.warn, 2, [4, 3]);
        const note = `${w < 420 ? 'ends' : 'request ends,'} ${ms(run.ended.t)}`, width = measure(g, note, { bold: true });
        const flip = ex + width + 12 > x(T1);
        if (flip) { g.fillStyle = 'rgba(255,255,255,.88)'; g.fillRect(ex - width - 12, top + 3, width + 9, 19); }
        text(g, note, ex + (flip ? -7 : 7), top + 13, { color: C.warn, bold: true, align: flip ? 'right' : 'left' });
      }

      const holds = run.ups.filter(message => message.kind !== 'stop');
      const lateOk = holds.filter(message => message.late && message.ok), refused = holds.filter(message => message.ok === false);
      const lateStop = run.ups.some(message => message.kind === 'stop' && message.late);
      let told;
      if (!run.ended) told = 'The request is still alive at the end of the chart.';
      else if (run.ended.cause === 'stop' && !lateOk.length && !lateStop) {
        told = `Every hold lands in time and pushes the end of the request further off, though never past the expiry of the token it quotes. You let go at ${ms(run.release)}; the stop lands ${ms(run.ended.t - run.release)} later and the request ends at once.`;
      } else if (run.ended.cause === 'stop') {
        const landed = lateOk.length ? lateOk[0].arrives : run.ended.t;
        told = `The stall holds back ${plural(lateOk.length, 'hold')}${lateStop ? ' and your stop' : ''}. ${lateOk.length ? `They land together at ${ms(landed)}, before the request has run out, so it survives` : `The request is still alive when the stop lands at ${ms(run.ended.t)}`}${lateStop ? (lateOk.length ? `; the stop right behind them ends it at ${ms(run.ended.t)}.` : '.') : ` until you let go at ${ms(run.release)}.`}`;
      } else if (run.ended.cause === 'refused') {
        told = `A message is refused at ${ms(run.ended.t)} and the request ends there.`;
      } else {
        const g0 = run.ended.grant;
        told = `Nothing reaches the board during the stall. The request runs out at ${ms(run.ended.t)}: ${g0.capped ? `the last hold to arrive quoted report ${g0.token}, whose token expires then` : `${LEASE} ms after the last hold arrived`}. ${refused.length ? `The ${plural(refused.length, 'hold')} that ${refused.length === 1 ? 'lands' : 'land'} at ${ms(refused[0].arrives)} ${refused.length === 1 ? 'is' : 'are'} refused: ${refused.length === 1 ? 'it continues' : 'they continue'} a press that is no longer alive.` : ''} To move again you would have to let go and press afresh.`;
      }
      say($('control-hold-readout'), told);
      $('control-hold-release-value').textContent = ms(run.release);
      $('control-hold-stall-value').textContent = Number(stallFor.value) ? `${ms(Number(stallFor.value))}, from ${ms(run.stallAt)}` : 'not at all';
      releaseAt.setAttribute('aria-valuetext', `${run.release} milliseconds`);
      stallFor.setAttribute('aria-valuetext', `${stallFor.value} milliseconds`);
      canvas.setAttribute('aria-label', `Timeline of four seconds. The board sends a status report every 500 milliseconds; the browser sends a press at ${run.press} milliseconds and a hold every 150 while the button is down. A saw-toothed trace shows how long the request has left. ${told}`);
    };
    for (const input of [releaseAt, stallFor]) { input.addEventListener('input', draw); input.addEventListener('change', draw); }
    redraws.push(draw);
  }

  // --- chapter 3, figure 3: the one-note mailbox ----------------------------
  {
    const canvas = $('control-mailbox-canvas');
    let box, now, dropped, read, told;
    const reset = () => {
      box = mailbox();
      now = 0;
      dropped = null;
      read = null;
      told = 'The mailbox is empty. Leave a note.';
    };
    const name = note => (note.kind === 'stop' ? 'stop' : `move ${note.dir === 'cw' ? 'clockwise' : 'counter-clockwise'}`);
    const card = (g, cx, cy, note, o = {}) => {
      const w = 138, h = 48, x = cx - w / 2, y = cy - h / 2;
      g.save();
      if (o.faded) g.globalAlpha = .5;
      // A discarded note keeps the clock it was discarded at.
      const stop = note.kind === 'stop', expired = !stop && (o.at === undefined ? now : o.at) >= note.until;
      block(g, x, y, w, h, stop ? '#fbefec' : '#fff', stop ? C.stop : C.az, 2, 5);
      text(g, stop ? 'STOP' : `MOVE ${note.dir === 'cw' ? '↻' : '↺'}`, cx, cy - 9, { bold: true, size: unit + 2, align: 'center', color: stop ? C.stop : C.az });
      text(g, o.sub || (stop ? 'never expires' : expired ? `expired at ${ms(note.until)}` : `good until ${ms(note.until)}`), cx, cy + 11, { align: 'center', color: expired ? C.warn : C.muted, bold: expired });
      if (o.struck) stroke(g, [[x + 8, y + h - 8], [x + w - 8, y + 8]], C.muted, 1.5);
      g.restore();
    };
    const draw = () => {
      const stacked = (canvas.clientWidth || 600) < 560;
      const { g, w } = surface(canvas, stacked ? 384 : 132);
      // Side by side on a wide canvas; on a narrow one the mailbox comes first.
      const zone = stacked
        ? i => ({ cx: w / 2, top: 6 + [2, 0, 1][i] * 128 })
        : i => ({ cx: w * (1 + 2 * i) / 6, top: 6 });
      const [left, middle, right] = [0, 1, 2].map(zone);

      text(g, 'Mailbox: room for one note', middle.cx, middle.top + 8, { bold: true, color: C.navy, align: 'center' });
      block(g, middle.cx - 82, middle.top + 24, 164, 70, C.pale, C.navy, 2, 6);
      const waiting = box.peek();
      if (waiting) card(g, middle.cx, middle.top + 59, waiting);
      else text(g, 'empty', middle.cx, middle.top + 59, { size: unit + 1, color: C.muted, align: 'center' });
      text(g, `clock: ${ms(now)}`, middle.cx, middle.top + 108, { color: C.muted, align: 'center' });

      text(g, 'Thrown away unread', left.cx, left.top + 8, { bold: true, color: C.muted, align: 'center' });
      if (dropped) {
        card(g, left.cx, left.top + 59, dropped.note, { faded: true, struck: true, at: dropped.at });
        text(g, dropped.why, left.cx, left.top + 96, { color: C.muted, align: 'center' });
      } else text(g, 'nothing yet', left.cx, left.top + 59, { size: unit + 1, color: C.soft, align: 'center' });

      text(g, stacked ? '↓ The direction task read' : 'The direction task read', right.cx, right.top + 8, { bold: true, color: C.navy, align: 'center' });
      if (read) {
        if (read.note) card(g, right.cx, right.top + 59, read.note, { sub: read.expired ? 'handed over instead' : null });
        else text(g, 'no note: nothing new', right.cx, right.top + 59, { size: unit + 1, color: C.muted, align: 'center' });
        if (read.expired) text(g, `the move expired at ${ms(read.expired.until)}`, right.cx, right.top + 96, { color: C.warn, align: 'center', bold: true });
      } else text(g, 'nothing yet', right.cx, right.top + 59, { size: unit + 1, color: C.soft, align: 'center' });

      if (!stacked) text(g, '→', (right.cx + middle.cx) / 2, 65, { size: 18, color: C.navy, align: 'center' });
      say($('control-mailbox-readout'), told);
      const holds = box.peek();
      canvas.setAttribute('aria-label', `A mailbox with room for one note, at ${now} milliseconds. It holds ${holds ? `a ${name(holds)} note` : 'nothing'}. ${told}`);
    };
    const leave = note => {
      const outcome = box.submit(note);
      if (outcome.why === 'stop waiting') {
        dropped = { note, at: now, why: 'a stop was waiting' };
        told = `A stop is waiting, so the ${name(note)} note is thrown away. Nothing can push a stop out of the mailbox; it stays until the direction task reads it.`;
      } else if (outcome.why === 'replaced') {
        dropped = { note: outcome.dropped, at: now, why: 'replaced by a newer note' };
        told = note.kind === 'stop'
          ? `The stop takes the place of the ${name(outcome.dropped)} note, which will never be read.`
          : `The newer ${name(note)} note takes the place of the ${name(outcome.dropped)} note. Only the latest wish matters, and it keeps its own expiry.`;
      } else {
        told = note.kind === 'stop' ? 'A stop is waiting for the direction task.' : `A ${name(note)} note is waiting, good until ${ms(note.until)}. Waiting does not extend it.`;
      }
      draw();
    };
    const move = dir => leave({ kind: 'move', dir, until: now + LEASE });
    $('control-mailbox-cw').addEventListener('click', () => move('cw'));
    $('control-mailbox-ccw').addEventListener('click', () => move('ccw'));
    $('control-mailbox-stop').addEventListener('click', () => leave({ kind: 'stop' }));
    $('control-mailbox-wait').addEventListener('click', () => {
      now += 400;
      const waiting = box.peek();
      told = waiting && waiting.kind === 'move' && now >= waiting.until
        ? `400 ms pass. The ${name(waiting)} note has now expired where it lies.`
        : `400 ms pass${waiting ? ' and the note is still waiting' : ''}.`;
      draw();
    });
    $('control-mailbox-read').addEventListener('click', () => {
      read = box.take(now);
      told = !read.note ? 'The mailbox was empty. The direction task carries on with what it already had.'
        : read.expired ? `The ${name(read.expired)} note expired at ${ms(read.expired.until)}, so the direction task is handed a stop instead. The mailbox is empty again.`
          : `The direction task takes the ${name(read.note)} note${read.note.kind === 'move' ? ` with its expiry of ${ms(read.note.until)}` : ''}. The mailbox is empty again.`;
      draw();
    });
    reset();
    redraws.push(draw);
  }

  // --- chapter 4: a strip chart of what is asked for and what the pins do ----
  // `data.ask` and `data.pin` hold [from, to] spans per direction; `data.off`
  // the off-time spans; `data.marks` things worth pointing at.
  function chart(canvas, data, o) {
    const narrow = (canvas.clientWidth || 600) < 560;
    const { g, w } = surface(canvas, 228);
    const gutter = narrow ? 46 : 150, left = 8 + gutter, right = w - 12;
    const x = t => left + (t - o.from) / (o.to - o.from) * (right - left);
    const rows = { askCw: 40, askCcw: 62, lease: 103, pinCw: 134, pinCcw: 162 }, h = 18, axis = 198;
    const clip = () => { g.save(); g.beginPath(); g.rect(left, 0, right - left + 1, 228); g.clip(); };

    const names = narrow ? ['↻', '↺', 'GP10', 'GP11'] : ['↻ clockwise', '↺ counter-clockwise', 'GP10  ↻', 'GP11  ↺'];
    [rows.askCw, rows.askCcw, rows.pinCw, rows.pinCcw].forEach((y, i) => {
      text(g, names[i], left - 8, y + h / 2, { color: C.muted, align: 'right' });
      stroke(g, [[left, y + h + .5], [right, y + h + .5]], C.line);
    });

    stroke(g, [[left, axis + .5], [right, axis + .5]], C.navy);
    for (let t = o.from; t <= o.to; t += o.tick) {
      const major = Math.round(t / o.tick) % o.every === 0;
      stroke(g, [[x(t), axis], [x(t), axis + (major ? 7 : 4)]], C.muted);
      if (major) text(g, o.name(t), x(t), axis + 18, { color: C.muted, align: t === o.from ? 'left' : t === o.to ? 'right' : 'center' });
    }

    clip();
    for (const [a, b] of data.off) {
      g.fillStyle = 'rgba(146,81,0,.07)';
      g.fillRect(x(a), rows.pinCw - 4, x(b) - x(a), rows.pinCcw + h + 4 - rows.pinCw + 4);
      hatch(g, x(a), rows.pinCw - 4, x(b) - x(a), rows.pinCcw + h + 8 - rows.pinCw, 'rgba(146,81,0,.4)', 7);
      stroke(g, [[x(b), rows.pinCw - 6], [x(b), rows.pinCcw + h + 4]], C.warn, 2);
      const shown = Math.min(x(b), right) - x(a);
      if (shown > 84) {
        const width = measure(g, 'off-time, 2 s', { size: unit - 1, bold: true }) + 8, cx = x(a) + shown / 2;
        g.fillStyle = '#fbf6ee';
        g.fillRect(cx - width / 2, rows.pinCw + h - 2, width, 16);
        text(g, 'off-time, 2 s', cx, rows.pinCw + h + 6, { size: unit - 1, bold: true, color: C.warn, align: 'center' });
      }
    }
    for (const [dir, y] of [['cw', rows.askCw], ['ccw', rows.askCcw]]) {
      for (const [a, b] of data.ask[dir]) block(g, x(a), y + 1, Math.max(2, x(b) - x(a)), h - 1, '#dbe6fa', C.az, 1.5, 2);
    }
    for (const [dir, y] of [['cw', rows.pinCw], ['ccw', rows.pinCcw]]) {
      for (const [a, b] of data.pin[dir]) {
        g.fillStyle = C.az;
        g.fillRect(x(a), y, Math.max(2, x(b) - x(a)), h + 1);
      }
    }
    for (const span of data.minimum || []) {
      const y = (span.dir === 'cw' ? rows.pinCw : rows.pinCcw) - 4;
      stroke(g, [[x(span.from), y + 3], [x(span.from), y], [x(span.to), y], [x(span.to), y + 3]], C.warn, 2);
    }
    for (const mark of data.marks) {
      const y = mark.dir === 'ccw' ? rows.pinCcw : rows.pinCw;
      if (mark.kind === 'cut') {
        g.beginPath();
        g.moveTo(x(mark.t) - 6, y - 9); g.lineTo(x(mark.t) + 6, y - 9); g.lineTo(x(mark.t), y); g.closePath();
        g.fillStyle = C.stop;
        g.fill();
      } else cross(g, x(mark.t), y + h / 2, 5, C.stop, 2.5);
    }
    for (const t of data.breaks || []) {
      stroke(g, [[x(t), 36], [x(t), axis]], C.soft, 1, [2, 4]);
      stroke(g, [[x(t) - 5, axis + 4], [x(t) - 1, axis - 4]], C.navy, 1.5);
      stroke(g, [[x(t) + 1, axis + 4], [x(t) + 5, axis - 4]], C.navy, 1.5);
    }
    if (o.view) {
      // What has not happened yet is veiled. Then the state at the cursor: how
      // long the request is good for, and whether a start fits.
      const v = o.view;
      g.fillStyle = 'rgba(255,255,255,.72)';
      g.fillRect(x(v.t) + 1, 36, right - x(v.t), axis - 36);
      if (v.lease !== null) {
        stroke(g, [[x(v.t), rows.lease], [x(v.lease), rows.lease]], C.az, 3);
        stroke(g, [[x(v.lease), rows.lease - 7], [x(v.lease), rows.lease + 7]], C.warn, 3);
      }
      if (v.start !== null) {
        const y = (v.dir === 'ccw' ? rows.pinCcw : rows.pinCw) + h + 5, tint = v.fits ? C.ok : C.stop;
        stroke(g, [[x(v.start), y - 4], [x(v.start), y], [x(v.start + MIN_ON), y], [x(v.start + MIN_ON), y - 4]], tint, 2.5);
      }
    }
    g.restore();
    if (o.view) {
      const v = o.view;
      if (v.lease !== null) {
        const flip = x(v.lease) > right - 150;
        text(g, `request good until ${ms(v.lease)}`, x(v.lease) + (flip ? 2 : -2), rows.lease - 13, { size: unit - 1, bold: true, color: C.warn, align: flip ? 'right' : 'left' });
      }
      for (const t of o.steps) stroke(g, [[x(t), 28], [x(t), 34]], C.navy, 1.5);
      stroke(g, [[x(v.t) + .5, 30], [x(v.t) + .5, axis]], C.navy, 1, [3, 3]);
      dot(g, x(v.t), 26, 8, C.navy);
      text(g, String(v.number), x(v.t), 26.5, { size: unit - 1, bold: true, color: '#fff', align: 'center' });
    }
    if (o.pen !== undefined) {
      stroke(g, [[x(o.pen) + .5, 32], [x(o.pen) + .5, axis]], C.navy, 1);
      g.beginPath();
      g.moveTo(x(o.pen) - 5, 26); g.lineTo(x(o.pen) + 5, 26); g.lineTo(x(o.pen), 34); g.closePath();
      g.fillStyle = C.navy;
      g.fill();
    }
    // The two headings go on last, on paper, so no line runs through them.
    for (const [title, y] of [[o.askTitle, 10], ['Applied: what the pins do', 120]]) {
      g.fillStyle = C.paper;
      g.fillRect(4, y - 9, measure(g, title, { bold: true }) + 8, 18);
      text(g, title, 8, y, { bold: true, color: C.navy });
    }
  }
  const spans = () => ({ ask: { cw: [], ccw: [] }, pin: { cw: [], ccw: [] }, off: [], marks: [], breaks: [], minimum: [] });
  const span = (list, from) => { list.push([from, from]); };
  const extend = (data, now) => {
    for (const group of [data.ask, data.pin]) for (const dir of ['cw', 'ccw']) for (const s of group[dir]) if (s.open) s[1] = now;
  };
  // Note a change of pin state on the chart data.
  function trace(data, before, after, now) {
    if (before === after) return;
    if (before !== null) {
      const open = data.pin[before].find(s => s.open);
      if (open) { open[1] = now; open.open = false; }
      data.off.push([now, now + MIN_OFF]);
    }
    if (after !== null) {
      const fresh = [now, now];
      fresh.open = true;
      data.pin[after].push(fresh);
    }
  }

  // Figure 1: the reader holds the buttons and the chart records, in real time.
  {
    const canvas = $('control-live-canvas'), readout = $('control-live-readout');
    const WINDOW = 15000;
    const buttons = { cw: $('control-live-cw'), ccw: $('control-live-ccw') };
    let data, model, now, held, nextHold, running, last, frame, quietSince, full, told, aside;
    const seconds = t => `${(t / 1000).toFixed(1)} s`;
    const draw = () => {
      chart(canvas, data, {
        from: 0, to: WINDOW, tick: 1000, every: (canvas.clientWidth || 600) < 560 ? 5 : (canvas.clientWidth || 600) < 860 ? 2 : 1, name: t => `${t / 1000} s`,
        askTitle: 'Requested: what you hold', pen: now
      });
      say(readout, told + aside);
      const high = model.state.actual;
      canvas.setAttribute('aria-label', `Strip chart of fifteen seconds. Upper lanes show which button you hold; lower lanes show pins GP10 and GP11. At ${seconds(now)}: ${held ? `you hold ${held === 'cw' ? 'clockwise' : 'counter-clockwise'}` : 'no button held'}; ${high ? `${high === 'cw' ? 'GP10' : 'GP11'} is high` : 'both pins are low'}.`);
    };
    const reset = () => {
      data = spans();
      model = actuator();
      now = 0; held = null; running = false; full = false; quietSince = null; aside = '';
      told = 'Nothing is held and both pins are low. Hold a button.';
      for (const button of Object.values(buttons)) { button.setAttribute('aria-pressed', 'false'); button.style.background = ''; }
    };
    const service = (t, command) => {
      const before = model.state.actual, wanted = model.state.target;
      model.step(t, { command });
      const after = model.state.actual;
      trace(data, before, after, t);
      const pin = dir => (dir === 'cw' ? 'GP10' : 'GP11');
      if (before !== after) {
        if (after) told = `${pin(after)} goes high at ${seconds(t)}${wanted === after && !command ? ', the moment the off-time ends' : ''}.`;
        else if (command && command.kind === 'stop') told = `You let go at ${seconds(t)} and ${pin(before)} drops at once. This axis is now shut until ${seconds(t + MIN_OFF)}.`;
        else told = `${pin(before)} drops at ${seconds(t)}.`;
      } else if (command && command.kind === 'jog' && after === null && !command.repeat) {
        told = `You ask for ${command.dir === 'cw' ? 'clockwise' : 'counter-clockwise'} at ${seconds(t)}, but the off-time runs until ${seconds(model.inactiveUntil())}. The request waits; ${pin(command.dir)} stays low.`;
      } else if (command && command.kind === 'stop' && wanted !== null && before === null) {
        told = `You let go at ${seconds(t)}, before the off-time was over. The waiting start is dropped: ${pin(wanted)} never went high.`;
      }
    };
    const advance = to => {
      for (let guard = 0; guard < 500; guard++) {
        const due = model.next(now);
        const times = [];
        if (held !== null) times.push(nextHold);
        if (due !== null && due > now) times.push(due);
        const t = Math.min(...times);
        if (!(t <= to)) break;
        if (held !== null && t === nextHold) {
          service(t, { kind: 'jog', dir: held, until: t + LEASE, repeat: true });
          nextHold += REFRESH;
        } else service(t, null);
        now = t;
      }
      now = to;
      extend(data, now);
    };
    const quiet = () => held === null && model.state.target === null && model.state.actual === null && now >= model.inactiveUntil();
    const stop = () => {
      running = false;
      cancelAnimationFrame(frame);
      clearTimeout(frame);
    };
    const tick = () => {
      const real = performance.now();
      advance(Math.min(WINDOW, now + Math.min(real - last, 250)));
      last = real;
      if (now >= WINDOW) {
        if (held !== null) letGo(held);
        full = true;
        aside = ' The chart is full; hold a button to start a fresh one.';
        stop();
      } else if (quiet()) {
        if (quietSince === null) quietSince = now;
        if (now - quietSince >= 700) {
          data.breaks.push(now);
          aside = ' The chart waits here until you hold a button again.';
          stop();
        }
      } else quietSince = null;
      draw();
      if (running) schedule();
    };
    // With reduced motion the trace advances in quarter-second steps instead of gliding.
    const schedule = () => { frame = calm.matches ? setTimeout(tick, 250) : requestAnimationFrame(tick); };
    const start = () => {
      aside = '';
      if (running) return;
      running = true;
      quietSince = null;
      last = performance.now();
      schedule();
    };
    const catchUp = () => {
      if (!running) return;
      const real = performance.now();
      advance(Math.min(WINDOW, now + Math.min(real - last, 250)));
      last = real;
    };
    const press = dir => {
      if (held !== null) return;            // one button per axis, as on the dashboard
      if (full) reset();
      catchUp();
      held = dir;
      const fresh = [now, now];
      fresh.open = true;
      data.ask[dir].push(fresh);
      service(now, { kind: 'jog', dir, until: now + LEASE });
      nextHold = now + REFRESH;
      buttons[dir].setAttribute('aria-pressed', 'true');
      buttons[dir].style.background = '#dbe6fa';
      start();
      draw();
    };
    function letGo(dir) {
      if (held !== dir) return;
      catchUp();
      held = null;
      const open = data.ask[dir].find(s => s.open);
      if (open) { open[1] = now; open.open = false; }
      service(now, { kind: 'stop' });
      buttons[dir].setAttribute('aria-pressed', 'false');
      buttons[dir].style.background = '';
      draw();
    }
    // The same press-and-hold handling as the real dashboard's direction buttons.
    for (const [dir, button] of Object.entries(buttons)) {
      button.addEventListener('pointerdown', event => {
        if (event.button !== 0) return;
        event.preventDefault();
        button.focus();
        press(dir);
        // Capture keeps the release coming here if the pointer slides off the button.
        try { button.setPointerCapture(event.pointerId); } catch (_) { /* not a live pointer */ }
      });
      for (const ending of ['pointerup', 'pointercancel', 'lostpointercapture', 'blur']) button.addEventListener(ending, () => letGo(dir));
      button.addEventListener('keydown', event => {
        if (event.key !== ' ' && event.key !== 'Enter') return;
        event.preventDefault();
        if (!event.repeat) press(dir);
      });
      button.addEventListener('keyup', event => {
        if (event.key !== ' ' && event.key !== 'Enter') return;
        event.preventDefault();
        letGo(dir);
      });
      button.addEventListener('contextmenu', event => event.preventDefault());
    }
    document.addEventListener('visibilitychange', () => { if (document.hidden && held !== null) letGo(held); });
    $('control-live-clear').addEventListener('click', () => { stop(); reset(); draw(); });
    reset();
    redraws.push(draw);
  }

  // Figure 2: four cases with exact timing, computed by the same model.
  {
    const canvas = $('control-cases-canvas'), choice = $('control-cases-case'), slider = $('control-cases-step');
    const jog = (dir, until) => ({ kind: 'jog', dir, until });
    const drive = (dir, until) => ({ kind: 'drive', dir, until });
    const STOP = { kind: 'stop' };
    const cases = {
      short: {
        end: 3000, ask: { cw: [[0, 200]], ccw: [[1900, 2400]] },
        events: [
          [0, jog('cw', 750), 'You press clockwise. The request is good for 750 ms, the motor has been off for a long time, and the 500 ms a start needs fits inside the request: GP10 goes high.'],
          [200, STOP, 'You let go. GP10 drops at once, 300 ms short of the minimum on-time: that minimum never holds a pin up against your wishes. The axis is now shut until 2200 ms.'],
          [1900, jog('ccw', 2650), 'You press counter-clockwise. The request is good until 2650 ms, but the off-time has 300 ms to run, so it waits. Now suppose the browser’s next two holds are held up.'],
          [2200, null, 'The off-time is over. A start now would need the request to last until 2700 ms, and it is good only until 2650. No start: GP11 stays low rather than begin a pulse that would be cut 50 ms short.'],
          [2250, jog('ccw', 3000), 'A hold gets through and renews the request until 3000 ms. Now 500 ms fits, and GP11 goes high.'],
          [2400, STOP, 'You let go. GP11 drops at once and the axis is shut until 4400 ms.']
        ]
      },
      tracker: {
        end: 3000, ask: { cw: [[0, 200]], ccw: [] }, minimum: true,
        events: [
          [0, drive('cw', 750), 'The tracker (chapter 6) asks for clockwise. GP10 goes high, and the minimum on-time runs until 500 ms.'],
          [200, drive(null, 950), 'The tracker stops asking: the dish is close enough. This is an ordinary release, not a stop, so GP10 stays high and the pulse is allowed to finish.'],
          [500, null, 'The minimum on-time is over and nothing is asking any more. GP10 drops, and the axis is shut until 2500 ms.'],
          [2500, null, 'The off-time ends. Nothing is asking, so nothing happens: a timer running out never starts a motor by itself.']
        ]
      },
      link: {
        end: 1000, ask: { cw: [[0, 300]], ccw: [] }, minimum: true,
        network: t => (t >= 400 ? { seen: 400, generation: 2 } : { seen: 0, generation: 1 }),
        events: [
          [0, jog('cw', 750), 'You press clockwise and GP10 goes high. At this same moment the board’s Wi-Fi task sees a working link. Suppose that is the last time it does.'],
          [299, null, 'The last good look at the link is 299 ms old. That is still under 300 ms, so the pin stays high.'],
          [300, null, 'Now it is 300 ms old, and that is too old. GP10 is cut, although the minimum on-time had 200 ms to run and the request itself was good until 750 ms.'],
          [400, null, 'The link is back. Nothing restarts. The old request was dropped, and so was your browser’s control of the board: you would have to take control again and press afresh.']
        ]
      },
      race: {
        end: 3000, ask: { cw: [[0, 200], [2100, 2200]], ccw: [] },
        events: [
          [0, jog('cw', 750), 'You press clockwise and GP10 goes high.'],
          [200, STOP, 'You let go. GP10 drops, and the axis is shut until 2200 ms.'],
          [2100, jog('cw', 2850), 'You press clockwise again. The request is good until 2850 ms, so a start is pencilled in for 2200 ms, the moment the off-time ends.'],
          [2200, STOP, 'You let go, and your stop note is in the mailbox at the very moment the start falls due. The direction task reads the note first, so the pencilled-in start is rubbed out. Not even a blip reaches GP10.'],
          [2300, null, 'Nothing is waiting and nothing starts.']
        ]
      }
    };
    // Run one case through the model, servicing at every deadline the firmware would wake for.
    const play = script => {
      const model = actuator(), data = spans(), views = [];
      const network = script.network || (t => ({ seen: t, generation: 1 }));
      let now = 0;
      const service = (t, command) => {
        const before = model.state.actual, wanted = model.state.target;
        model.step(t, { command, network: network(t) });
        trace(data, before, model.state.actual, t);
        if (before === null && model.state.actual !== null && script.minimum) data.minimum.push({ dir: model.state.actual, from: t, to: t + MIN_ON });
        if (before !== null && model.state.actual === null && wanted === before && !(command && command.kind === 'stop')) data.marks.push({ kind: 'cut', t, dir: before });
        now = t;
      };
      for (const [t, command] of script.events) {
        for (let guard = 0; guard < 50; guard++) {
          const due = model.next(now, script.network ? network(now) : null);
          if (due === null || due <= now || due >= t) break;
          service(due, null);
        }
        service(t, command);
        const s = model.state, waiting = s.target !== null && s.actual === null;
        const start = waiting ? Math.max(model.inactiveUntil(), t) : null;
        const fits = waiting && start + MIN_ON <= s.lease;
        if (waiting && !fits && start === t) data.marks.push({ kind: 'refused', t, dir: s.target });
        views.push({ t, number: views.length + 1, lease: s.lease, dir: s.target, start, fits });
      }
      for (let guard = 0; guard < 50; guard++) {
        const due = model.next(now, script.network ? network(now) : null);
        if (due === null || due <= now || due > script.end) break;
        service(due, null);
      }
      extend(data, script.end);
      data.ask = script.ask;
      return { data, views };
    };
    const played = {};
    for (const key of Object.keys(cases)) played[key] = play(cases[key]);
    const draw = () => {
      const script = cases[choice.value], run = played[choice.value];
      slider.max = String(script.events.length - 1);
      const step = Math.min(Number(slider.value), script.events.length - 1);
      const view = run.views[step];
      chart(canvas, run.data, {
        from: 0, to: script.end, tick: script.end > 1000 ? 500 : 100, every: script.end > 1000 ? 2 : 5, name: t => (t === script.end ? `${t} ms` : String(t)),
        askTitle: choice.value === 'tracker' ? 'Requested: what the tracker asks for' : 'Requested: what you hold',
        view, steps: run.views.map(v => v.t)
      });
      const told = `At ${ms(view.t)}. ${script.events[step][2]}`;
      say($('control-cases-readout'), told);
      $('control-cases-step-value').textContent = `${step + 1} of ${script.events.length}`;
      slider.setAttribute('aria-valuetext', `moment ${step + 1} of ${script.events.length}, at ${view.t} milliseconds`);
      canvas.setAttribute('aria-label', `Strip chart of one scripted case: upper lanes show what is asked for, lower lanes pins GP10 and GP11, with the 2 second off-time hatched. ${told}`);
    };
    choice.addEventListener('change', () => { slider.value = '0'; draw(); });
    slider.addEventListener('input', draw);
    slider.addEventListener('change', draw);
    redraws.push(draw);
  }

  // --- chapter 4: the run-time tank ---------------------------------------
  {
    const canvas = $('control-tank-canvas'), skip = $('control-tank-skip');
    let tank, clock, level, runs, restarts, told;
    const clockText = value => `${Math.floor(value / 60000)}:${String(Math.floor(value / 1000) % 60).padStart(2, '0')}`;
    const secs = value => `${Math.round(value / 100) / 10} s`;
    const boot = () => {
      tank.boot();
      restarts.push(clock);
      level.push([clock, 0]);
    };
    const reset = () => {
      tank = dutyTank();
      clock = 0; level = []; runs = []; restarts = [];
      boot();
      told = 'The board has just started. It has no record of how long the motor ran before, so the tank counts as empty.';
    };
    const draw = () => {
      const stacked = (canvas.clientWidth || 600) < 620;
      const { g, w } = surface(canvas, stacked ? 330 : 192);
      const left = tank.left(), unknown = tank.state.startup;

      // The tank.
      const tx = 44, ty = 26, tw = 58, th = 132;
      text(g, '180 s', tx - 6, ty + 2, { size: unit - 1, color: C.muted, align: 'right' });
      text(g, '0', tx - 6, ty + th - 2, { size: unit - 1, color: C.muted, align: 'right' });
      for (const mark of [60, 120]) stroke(g, [[tx - 4, ty + th - mark / 180 * th], [tx, ty + th - mark / 180 * th]], C.muted);
      const fill = left / DUTY_RUN * th;
      g.fillStyle = C.az;
      g.fillRect(tx, ty + th - fill, tw, fill);
      if (unknown) hatch(g, tx, ty, tw, th, C.soft, 8);
      stroke(g, [[tx, ty - 6], [tx, ty + th], [tx + tw, ty + th], [tx + tw, ty - 6]], C.navy, 2.5);
      text(g, 'Run-time left', tx + tw + 14, ty + 6, { bold: true, color: C.navy });
      text(g, unknown ? '0 s ?' : secs(left), tx + tw + 14, ty + 34, { size: 24, bold: true, color: left < MIN_ON ? C.stop : C.az });
      text(g, unknown ? 'history unknown' : left < MIN_ON ? 'too little to start' : 'of 180 s', tx + tw + 14, ty + 58, { color: C.muted });

      // The rest clock.
      const cx = tx + tw + 52, cy = ty + 104, r = 24, rested = Math.min(tank.state.idle, DUTY_REST) / DUTY_REST;
      g.beginPath();
      g.arc(cx, cy, r, 0, 2 * Math.PI);
      g.strokeStyle = C.line;
      g.lineWidth = 6;
      g.stroke();
      if (rested > 0) {
        g.beginPath();
        g.arc(cx, cy, r, -Math.PI / 2, -Math.PI / 2 + rested * 2 * Math.PI);
        g.strokeStyle = C.ok;
        g.lineWidth = 6;
        g.stroke();
      }
      text(g, clockText(Math.min(tank.state.idle, DUTY_REST)), cx, cy + 1, { bold: true, color: C.ok, align: 'center' });
      text(g, 'rested,', cx + r + 12, cy - 8, { color: C.muted });
      text(g, 'of 15:00', cx + r + 12, cy + 8, { color: C.muted });

      // The history: run-time left against time since the first start.
      const px = stacked ? 58 : 344, py = stacked ? 206 : 26, pw = w - px - 14, ph = stacked ? 74 : 110;
      const span = Math.max(30 * 60000, Math.ceil(clock / 600000) * 600000);
      const x = t => px + t / span * pw, y = value => py + ph - value / DUTY_RUN * ph;
      text(g, 'Run-time left, over time', px, py - 14, { bold: true, color: C.navy });
      text(g, '180 s', px - 6, py + 2, { size: unit - 1, color: C.muted, align: 'right' });
      text(g, '0', px - 6, py + ph - 2, { size: unit - 1, color: C.muted, align: 'right' });
      stroke(g, [[px, py + .5], [px + pw, py + .5]], C.line, 1, [2, 3]);
      stroke(g, [[px, py + ph + .5], [px + pw, py + ph + .5]], C.navy);
      const every = [5, 10, 15, 20, 30, 60, 120].find(step => step * 60000 / span * pw >= 58) || 240;
      for (let m = 0; m <= span / 60000; m += every) {
        stroke(g, [[x(m * 60000), py + ph], [x(m * 60000), py + ph + 5]], C.muted);
        text(g, m ? `${m} min` : '0', x(m * 60000), py + ph + 16, { size: unit - 1, color: C.muted, align: !m ? 'left' : m * 60000 === span ? 'right' : 'center' });
      }
      for (const t of restarts) if (t > 0) stroke(g, [[x(t) + .5, py - 4], [x(t) + .5, py + ph]], C.warn, 1.5, [4, 3]);
      for (const [a, b] of runs) { g.fillStyle = C.az; g.fillRect(x(a), py + ph + 30, Math.max(2, x(b) - x(a)), 7); }
      text(g, 'motor on', px - 6, py + ph + 34, { size: unit - 1, color: C.az, align: 'right' });
      stroke(g, level.concat([[clock, left]]).map(([t, value]) => [x(t), y(value)]), C.az, 2);
      dot(g, x(clock), y(left), 4, C.az);

      skip.disabled = !unknown;
      say($('control-tank-readout'), told);
      canvas.setAttribute('aria-label', `A tank of motor run-time holding ${Math.round(left / 1000)} of 180 seconds${unknown ? ', counted as empty because the history is unknown' : ''}, a rest clock at ${clockText(Math.min(tank.state.idle, DUTY_REST))} of 15 minutes, and a chart of run-time left over ${Math.round(clock / 60000)} minutes. ${told}`);
    };
    const need = () => clockText(DUTY_REST - Math.min(tank.state.idle, DUTY_REST));
    $('control-tank-run').addEventListener('click', () => {
      const before = tank.left(), outcome = tank.run(30000);
      if (outcome.refused) {
        told = tank.state.startup
          ? `Start refused. Since the restart the tank counts as empty; it needs ${need()} more rest, or Skip startup rest.`
          : `Start refused: a start needs at least half a second of run-time and ${secs(before)} is left. ${need()} more unbroken rest will refill it.`;
      } else {
        level.push([clock, before], [clock + outcome.ran, tank.left()]);
        runs.push([clock, clock + outcome.ran]);
        clock += outcome.ran;
        told = outcome.cut
          ? `The motor ran ${secs(outcome.ran)} and was cut when the tank ran dry. The rest clock starts again from zero.`
          : `The motor ran 30 s. ${secs(tank.left())} of run-time is left, and the rest clock is back to zero.`;
      }
      draw();
    });
    const rest = minutes => {
      const before = tank.left(), outcome = tank.rest(minutes * 60000);
      if (outcome.refilled) level.push([clock + outcome.after, before], [clock + outcome.after, tank.left()]);
      clock += minutes * 60000;
      told = outcome.refilled ? `Fifteen unbroken minutes of rest: the tank is full again, all at once.`
        : tank.state.idle >= DUTY_REST ? `Rested ${plural(minutes, 'minute')}. The tank is already full.`
          : `Rested ${plural(minutes, 'minute')}. Still ${secs(before)} in the tank: a rest shorter than 15 minutes gives nothing back. ${need()} to go.`;
      draw();
    };
    $('control-tank-rest1').addEventListener('click', () => rest(1));
    $('control-tank-rest5').addEventListener('click', () => rest(5));
    skip.addEventListener('click', () => {
      const before = tank.left();
      if (!tank.skip()) return;
      level.push([clock, before], [clock, tank.left()]);
      told = 'You have vouched that both motors were rested. The tank is full; the offer is gone until the next restart, and later rests cannot be skipped.';
      draw();
    });
    $('control-tank-restart').addEventListener('click', () => {
      level.push([clock, tank.left()]);
      boot();
      told = 'The board restarts and forgets how long the motor has run. It assumes the worst: an empty tank and a rest clock at zero.';
      draw();
    });
    reset();
    redraws.push(draw);
  }

  // --- chapter 4: one channel of the real circuit --------------------------
  {
    const figure = $('control-circuit'), pin = $('control-circuit-gp10'), plug = $('control-circuit-plug');
    const show = (selector, visible) => figure.querySelectorAll(selector).forEach(node => { node.style.display = visible ? '' : 'none'; });
    const draw = () => {
      const high = pin.checked, plugged = plug.checked, turning = high && plugged;
      show('[data-when="high"]', high);
      show('[data-when="low"]', !high);
      show('[data-when="turning"]', turning);
      show('[data-when="still"]', !turning);
      $('control-circuit-led').setAttribute('fill', high ? C.az : '#fff');
      $('control-circuit-rotator').setAttribute('transform', plugged ? '' : 'translate(30 0)');
      $('control-circuit-rotator').setAttribute('opacity', plugged ? '1' : '.45');
      const told = !high
        ? 'GP10 is low: 0 V. The 10 kΩ resistor holds the transistor’s base at ground, the transistor is off, pin 2 is left alone and the LED is dark.'
        : plugged
          ? 'GP10 is high: 3.3 V. About 2.8 mA lights the LED. About 1.1 mA through the 2.2 kΩ resistor turns the transistor on, which joins pin 2 to pin 8. The controller reads that as its own clockwise switch and powers the motor from its own supply.'
          : 'GP10 is high: 3.3 V. The LED lights and the transistor turns on exactly as before, but with the rotator unplugged there is nothing for it to switch. The LED shows what was asked for, not what moved.';
      say($('control-circuit-readout'), told);
      $('control-circuit-desc').textContent = told;
    };
    pin.addEventListener('change', draw);
    plug.addEventListener('change', draw);
    draw();
  }

  const redraw = () => redraws.forEach(draw => draw());
  window.addEventListener('resize', redraw);
  redraw();
})();
