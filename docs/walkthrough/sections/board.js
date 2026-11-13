(() => {
  'use strict';
  const $ = id => document.getElementById(id);
  if (!$('board-pins')) return;

  // The walkthrough's shared colours (style.css).
  const NAVY = '#18354a', INK = '#20323e', MUTED = '#506574', QUIET = '#8fa5b5', PALE = '#edf4fa', PAPER = '#fff';
  const AZ = '#245bce', EL = '#087e83', WARN = '#925100', OK = '#11643e', STOP = '#a33317';
  const TINT = { [AZ]: '#dce7fb', [EL]: '#d6eeee', [WARN]: '#f6e6cc', [OK]: '#dcefe3', [STOP]: '#f7ddd6', [QUIET]: '#e6ecf1' };
  const FONT = "'Trebuchet MS','Segoe UI',sans-serif";
  const RAD = Math.PI / 180;

  // ── The rules, with every constant taken from the source ──────────────────

  const MARGIN_MV = 25;      // position.rs FEEDBACK_ENDPOINT_MARGIN_MV
  const RAIL_MV = 3200;      // ads1015.rs FEEDBACK_SUPPLY_RAIL_MV
  const STEP_MV = 2;         // ads1015.rs FullScale::V4_096: 4096 mV over 2048 counts
  const MAX_AGE_MS = 500;    // feedback.rs FEEDBACK_MAX_AGE_MS
  const MAX_READ_MS = 150;   // feedback.rs FEEDBACK_MAX_LATENCY_MS
  const PAUSE_MS = 100;      // feedback.rs FEEDBACK_PERIOD_MS
  const COAST_MS = 500;      // interlock.rs TravelProfile::DEFAULT
  // position.rs HARD_CODED_CALIBRATION and interlock.rs TravelProfile::DEFAULT.
  const AXES = {
    az: { name: 'azimuth', color: AZ, minDeg: 0, maxDeg: 360, mvAtMin: 114, mvAtMax: 2033, speed: 7, out: 'clockwise', back: 'counter-clockwise', window: 12 },
    el: { name: 'elevation', color: EL, minDeg: 0, maxDeg: 90, mvAtMin: 1281, mvAtMax: 58, speed: 3, out: 'up', back: 'down', window: 5 },
  };
  // The firmware computes in 32-bit floats; rounding the same way keeps whole-millisecond results identical.
  const f32 = Math.fround;

  // position.rs: validate_adc_readings, then AxisMapping::voltage_to_angle.
  function position(axis, mv) {
    const low = Math.min(axis.mvAtMin, axis.mvAtMax), high = Math.max(axis.mvAtMin, axis.mvAtMax);
    if (mv <= 0 || mv >= RAIL_MV) return { refused: 'edge' };
    if (mv < low - MARGIN_MV || mv > high + MARGIN_MV) return { refused: 'range', past: mv < low ? low - mv : mv - high };
    const held = Math.min(high, Math.max(low, mv));
    const fraction = f32(f32(held - axis.mvAtMin) / f32(axis.mvAtMax - axis.mvAtMin));
    return { deg: f32(axis.minDeg + f32(fraction * f32(axis.maxDeg - axis.minDeg))), past: Math.abs(mv - held) };
  }
  // position.rs: AxisCalibration::angle_to_voltage, before rounding.
  const voltage = (axis, deg) => axis.mvAtMin + (deg - axis.minDeg) / (axis.maxDeg - axis.minDeg) * (axis.mvAtMax - axis.mvAtMin);
  // position.rs: InstalledCalibration::resolution_deg with interlock.rs CODE_ROUNDING_MARGIN_MV.
  const stepDeg = axis => f32(f32(f32(axis.maxDeg - axis.minDeg) * f32(STEP_MV + 0.5)) / f32(Math.abs(axis.mvAtMax - axis.mvAtMin)));
  // interlock.rs: AxisLimits::refusal_age_ms for travel towards the high-angle end.
  function refusalAge(axis, deg) {
    const perMs = f32(axis.speed / 1000);
    const room = f32(f32(f32(axis.maxDeg - stepDeg(axis)) - deg) - f32(perMs * f32(COAST_MS)));
    return room > 0 ? Math.trunc(f32(room / perMs)) : 0;
  }

  // ── Drawing ───────────────────────────────────────────────────────────────

  const redraws = [];
  // A canvas sized in CSS pixels by `heightFor(width)` and redrawn whenever its width or the pixel ratio changes.
  function canvas(id, heightFor, draw) {
    const el = $(id), ctx = el.getContext('2d');
    const redraw = () => {
      const w = el.clientWidth;
      if (!w) return;
      const h = heightFor(w), ratio = window.devicePixelRatio || 1;
      const pw = Math.round(w * ratio), ph = Math.round(h * ratio);
      if (el.width !== pw || el.height !== ph) { el.width = pw; el.height = ph; }
      ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
      ctx.clearRect(0, 0, w, h);
      draw(ctx, w, h);
    };
    redraws.push(redraw);
    new ResizeObserver(redraw).observe(el);
    return redraw;
  }
  window.addEventListener('resize', () => redraws.forEach(redraw => redraw()));

  // Dragging on the drawing moves the slider it belongs to, so pointer and keyboard share one control.
  // `valueAt` gets the pointer's position, the drawing's width, and where and at what value the drag began.
  function drag(id, input, valueAt) {
    const el = $(id);
    let began;
    const move = event => {
      const box = el.getBoundingClientRect(), x = event.clientX - box.left, y = event.clientY - box.top;
      began = began || { x, y, value: +input.value };
      const value = valueAt(x, y, box.width, began);
      if (value === null) return;
      input.value = value;
      input.dispatchEvent(new Event('input', { bubbles: true }));
    };
    el.addEventListener('pointerdown', event => { began = null; el.setPointerCapture(event.pointerId); move(event); });
    el.addEventListener('pointermove', event => { if (el.hasPointerCapture(event.pointerId)) move(event); });
  }
  // The − and + beside a slider move it by one of its own steps, for values too fine to drag to.
  document.querySelectorAll('.board-fine button').forEach(button => button.addEventListener('click', () => {
    const input = button.parentElement.querySelector('input');
    if (button.textContent === '+') input.stepUp(); else input.stepDown();
    input.dispatchEvent(new Event('input', { bubbles: true }));
  }));

  function stroke(ctx, color, width = 1.5, dash = []) {
    ctx.strokeStyle = color; ctx.lineWidth = width; ctx.setLineDash(dash); ctx.stroke();
  }
  function line(ctx, x1, y1, x2, y2, color, width, dash) {
    ctx.beginPath(); ctx.moveTo(x1, y1); ctx.lineTo(x2, y2); stroke(ctx, color, width, dash);
  }
  function rect(ctx, x, y, w, h, fill, edge, radius = 0, dash) {
    ctx.beginPath(); ctx.roundRect(x, y, w, h, radius);
    if (fill) { ctx.fillStyle = fill; ctx.fill(); }
    if (edge) stroke(ctx, edge, 1.5, dash);
  }
  function disc(ctx, x, y, r, fill, edge, width, dash) {
    ctx.beginPath(); ctx.arc(x, y, r, 0, 2 * Math.PI);
    if (fill) { ctx.fillStyle = fill; ctx.fill(); }
    if (edge) stroke(ctx, edge, width, dash);
  }
  function triangle(ctx, x1, y1, x2, y2, x3, y3, fill) {
    ctx.beginPath(); ctx.moveTo(x1, y1); ctx.lineTo(x2, y2); ctx.lineTo(x3, y3); ctx.closePath();
    ctx.fillStyle = fill; ctx.fill();
  }
  // Write `words`, or the first of several wordings that fits in `room` pixels (nothing if none does).
  // With `halo`, a patch of paper goes behind the words so they stay legible over a line.
  function write(ctx, words, x, y, { color = INK, align = 'left', size = 13, bold = false, room = Infinity, halo = false } = {}) {
    ctx.font = `${bold ? 600 : 400} ${size}px ${FONT}`;
    const chosen = [].concat(words).find(wording => ctx.measureText(wording).width <= room);
    if (chosen === undefined) return 0;
    const width = ctx.measureText(chosen).width;
    if (halo) rect(ctx, x - (align === 'left' ? 3 : align === 'right' ? width + 3 : width / 2 + 3), y - size * 0.7, width + 6, size * 1.4, 'rgba(255,255,255,.88)');
    ctx.font = `${bold ? 600 : 400} ${size}px ${FONT}`;
    ctx.fillStyle = color; ctx.textAlign = align; ctx.textBaseline = 'middle';
    ctx.fillText(chosen, x, y);
    return width;
  }
  const clamp = (value, low, high) => Math.min(high, Math.max(low, value));
  const chosen = name => document.querySelector(`input[name="${name}"]:checked`).value;
  const onChoice = (name, act) => document.querySelectorAll(`input[name="${name}"]`).forEach(input => input.addEventListener('change', act));

  // ── Chapter 1: the board ──────────────────────────────────────────────────

  const PINS = [
    { pin: 'GP10', button: 'right', turns: 'clockwise', color: AZ },
    { pin: 'GP11', button: 'left', turns: 'counter-clockwise', color: AZ },
    { pin: 'GP20', button: 'up', turns: 'up', color: EL },
    { pin: 'GP21', button: 'down', turns: 'down', color: EL },
  ];

  // One part of the board, framed, and tagged with the piece of the program that holds it.
  function part(ctx, x, y, w, h, { holder, stopped } = {}) {
    const color = stopped ? STOP : NAVY;
    rect(ctx, x, y, w, h, PAPER, holder ? color : QUIET, 6, holder ? [] : [4, 3]);
    if (!holder) return;
    const size = w < 130 ? 12 : 13;
    ctx.font = `600 ${size}px ${FONT}`;
    const tag = (stopped ? [`${holder} stops here`, `${holder} stops`, holder] : [holder, holder.split(' ')[0]]).find(wording => ctx.measureText(wording).width <= w - 20) || holder;
    rect(ctx, x + 4, y + 4, ctx.measureText(tag).width + 12, 20, color, null, 10);
    write(ctx, tag, x + 10, y + 14.5, { color: PAPER, size, bold: true });
  }

  // The Pico, its four direction pins, each pin's light and switch, and the rotator's control box.
  // `levels` gives each pin as 'loose' (nothing drives it), 'low' or 'high'. With `parts`, the watchdog,
  // position sensor and radio are drawn too, each with its holder.
  function drawBoard(ctx, w, h, levels, parts) {
    const picoW = clamp(w * 0.27, 100, 196), boxW = clamp(w * 0.23, 60, 170);
    const left = picoW + 1, right = w - boxW - 1, gap = right - left;
    const rowY = i => (parts ? 74 : 54) + i * 32;

    rect(ctx, 1, 1, picoW, h - 2, PALE, NAVY, 8);
    write(ctx, 'Pico 2 W', 11, 15, { color: NAVY, bold: true });
    rect(ctx, right, 1, boxW, h - 2, PAPER, NAVY, 8);
    if (!write(ctx, ['G-5500 control box', 'Control box'], right + 11, 15, { color: NAVY, bold: true, room: boxW - 18 })) {
      write(ctx, 'Control', right + 9, 13, { color: NAVY, bold: true, size: 12 });
      write(ctx, 'box', right + 9, 27, { color: NAVY, bold: true, size: 12 });
    }
    if (parts) part(ctx, 6, 30, picoW - 10, 204, parts.pins);

    PINS.forEach((pin, i) => {
      const y = rowY(i), level = levels[i], high = level === 'high', loose = level === 'loose';
      const wire = loose ? QUIET : high ? pin.color : NAVY, dash = loose ? [3, 3] : [];
      const lamp = left + gap * 0.24, push = left + gap * 0.52, hinge = left + gap * 0.62, contact = left + gap * 0.86;
      // On the Pico: the pin and the voltage the program has put on it.
      if (!loose) write(ctx, high ? '3.3 V' : '0 V', 15, y, { color: high ? pin.color : MUTED, bold: high, size: 12 });
      write(ctx, pin.pin, left - 9, y, { align: 'right', color: loose ? MUTED : INK });
      // The pin's wire lights a lamp and works the switch.
      line(ctx, left, y, push, y, wire, high ? 2.5 : 1.5, dash);
      triangle(ctx, push + 6, y, push, y - 4, push, y + 4, wire);
      line(ctx, lamp, y, lamp, y - 6, wire, 1.5, dash);
      disc(ctx, lamp, y - 11, 5, high ? pin.color : PAPER, wire, 1.5);
      if (high) [-150, -90, -30].forEach(a => line(ctx, lamp + 8 * Math.cos(a * RAD), y - 11 + 8 * Math.sin(a * RAD), lamp + 12 * Math.cos(a * RAD), y - 11 + 12 * Math.sin(a * RAD), pin.color, 1.5));
      disc(ctx, left, y, 4, loose ? PAPER : wire, wire, 1.5);
      // The switch joins one control box input to ground when it closes.
      line(ctx, hinge, y, hinge, y + 7, NAVY, 1.5);
      [5, 3, 1].forEach((half, bar) => line(ctx, hinge - half, y + 7 + bar * 3, hinge + half, y + 7 + bar * 3, NAVY, 1.5));
      line(ctx, contact, y, right, y, high ? pin.color : NAVY, high ? 2.5 : 1.5);
      line(ctx, hinge, y, high ? contact : contact - 2, high ? y : y - 10, high ? pin.color : NAVY, 2.5);
      disc(ctx, hinge, y, 2.5, NAVY);
      disc(ctx, contact, y, 2.5, high ? pin.color : NAVY);
      // In the control box: the front-panel button this input stands in for.
      if (high) rect(ctx, right + 4, y - 12, boxW - 8, 24, TINT[pin.color], null, 5);
      write(ctx, high ? [`${pin.button}: pressed`, pin.button] : pin.button, right + 12, y, { color: high ? pin.color : INK, bold: high, room: boxW - 18 });
    });
    if (!parts) return;

    // The watchdog shares the pins' frame: whoever holds one holds the other.
    const dog = parts.watchdog;
    disc(ctx, 20, 206, 8.5, PAPER, dog ? NAVY : QUIET, 1.5, dog ? [] : [3, 3]);
    if (dog === 'counting') {
      ctx.beginPath(); ctx.moveTo(20, 206); ctx.arc(20, 206, 6, -Math.PI / 2, Math.PI); ctx.closePath();
      ctx.fillStyle = WARN; ctx.fill();
    }
    write(ctx, 'watchdog', 35, 206, { color: dog ? INK : MUTED });
    const dogSays = {
      idle: [['not started'], MUTED],
      passed: [['✓ last run did not freeze', '✓ no freeze'], OK],
      fired: [['✕ last run froze', '✕ it froze'], STOP],
      counting: [['restarts the board after 500 ms', '500 ms countdown', '500 ms'], WARN],
    }[dog];
    if (dogSays) write(ctx, dogSays[0], 13, 223, { color: dogSays[1], size: 12, bold: dog !== 'idle', room: picoW - 24 });

    // The radio and the Pico's own light, which the radio chip drives.
    const top = 240, radio = parts.radio;
    part(ctx, 6, top, picoW - 10, h - top - 6, radio);
    disc(ctx, 19, top + 46, 2.5, radio.holder ? NAVY : QUIET);
    [6, 10].forEach(r => { ctx.beginPath(); ctx.arc(19, top + 46, r, -80 * RAD, -10 * RAD); stroke(ctx, radio.holder ? NAVY : QUIET, 1.5); });
    write(ctx, 'radio', 35, top + 42, { color: radio.holder ? INK : MUTED });
    const radioSays = {
      joining: [['joining your Wi-Fi…', 'joining…'], MUTED],
      retrying: [['no network: trying again', 'retrying…'], STOP],
      joined: [['✓ on your network', '✓ joined'], OK],
    }[radio.state];
    if (radioSays) write(ctx, radioSays[0], 13, top + 59, { color: radioSays[1], size: 12, bold: radio.state !== 'joining', room: picoW - 24 });
    disc(ctx, 19, top + 77, 4, radio.flash ? OK : PAPER, radio.flash ? OK : QUIET, 1.5);
    if (radio.flash) [-135, -45, 45, 135].forEach(a => line(ctx, 19 + 6.5 * Math.cos(a * RAD), top + 77 + 6.5 * Math.sin(a * RAD), 19 + 10 * Math.cos(a * RAD), top + 77 + 10 * Math.sin(a * RAD), OK, 1.5));
    write(ctx, radio.flash ? ['light: three flashes', '3 flashes'] : 'light', 35, top + 77, { color: radio.flash ? OK : MUTED, size: 12, bold: radio.flash, room: picoW - 46 });

    // The position sensor sits between the two: voltages in from the control box, numbers out to the Pico.
    const sensor = parts.sensor, sw = Math.min(gap - 12, 176), sx = left + (gap - sw) / 2;
    [[top + 40, AZ], [top + 54, EL]].forEach(([y, color]) => {
      line(ctx, sx + sw, y, right, y, color, 1.5);
      line(ctx, left, y, sx, y, sensor.holder ? NAVY : QUIET, 1.5, sensor.holder ? [] : [3, 3]);
    });
    // Both wires are named at the same length: whichever wording the longer name fits in.
    ctx.font = `600 12px ${FONT}`;
    const wires = [['azimuth voltage', 'elevation voltage'], ['azimuth', 'elevation']].find(pair => ctx.measureText(pair[1]).width <= boxW - 18) || ['az', 'el'];
    write(ctx, wires[0], right + 12, top + 40, { color: AZ, size: 12, bold: true });
    write(ctx, wires[1], right + 12, top + 54, { color: EL, size: 12, bold: true });
    part(ctx, sx, top, sw, h - top - 6, sensor);
    if (!write(ctx, 'position sensor', sx + sw / 2, top + 46, { align: 'center', color: sensor.holder ? INK : MUTED, room: sw - 12 })) {
      write(ctx, 'position', sx + sw / 2, top + 39, { align: 'center', color: sensor.holder ? INK : MUTED });
      write(ctx, 'sensor', sx + sw / 2, top + 54, { align: 'center', color: sensor.holder ? INK : MUTED });
    }
    const sensorSays = {
      passed: [['✓ self-test passed', '✓ passed'], OK],
      silent: [['✕ no answer'], STOP],
      measuring: [['measuring both voltages', 'measuring'], MUTED],
    }[sensor.state];
    if (sensorSays) write(ctx, sensorSays[0], sx + sw / 2, top + 74, { align: 'center', color: sensorSays[1], size: 12, bold: sensor.state !== 'measuring', room: sw - 10 });
  }

  // Figure: four pins, four switches. The choices mirror DriveCommand: each axis is still or turns one way.
  {
    const redraw = canvas('board-pins-view', () => 180, (ctx, w, h) => {
      const high = [chosen('board-pins-az'), chosen('board-pins-el')];
      drawBoard(ctx, w, h, PINS.map((_, i) => high.includes(String(i)) ? 'high' : 'low'));
    });
    const update = () => {
      const high = [chosen('board-pins-az'), chosen('board-pins-el')].filter(Boolean).map(i => PINS[i]);
      $('board-pins-say').textContent = high.length
        ? `${high.map(pin => pin.pin).join(' and ')} ${high.length > 1 ? 'are' : 'is'} at 3.3 V. ${high.length > 1 ? 'Their switches close' : 'Its switch closes'}, and the control box sees its ${high.map(pin => `“${pin.button}”`).join(' and ')} ${high.length > 1 ? 'buttons' : 'button'} held down.`
        : 'All four pins are at 0 V. Every switch is open, and the control box sees no button pressed.';
      redraw();
    };
    onChoice('board-pins-az', update);
    onChoice('board-pins-el', update);
    update();
  }

  // Figure: startup takes hold of the board, in the order of main() in the firmware's main.rs.
  {
    const slider = $('board-wake-step');
    const STEPS = ['power arrives', 'pins driven low', 'why did the board restart?', 'one task takes the pins', 'the sensor is tested', 'the sensor is put to work', 'the radio is switched on', 'on the network'];
    const stopsAt = { froze: 2, sensor: 4 };
    function state() {
      const fault = chosen('board-wake-case'), step = +slider.value, at = Math.min(step, stopsAt[fault] || 7);
      const stopped = at === stopsAt[fault];
      const frozen = fault === 'froze' && stopped, silent = fault === 'sensor' && stopped, offline = fault === 'wifi' && at === 7;
      return {
        step, at, offline,
        levels: PINS.map(() => at ? 'low' : 'loose'),
        parts: {
          pins: { holder: at < 1 ? null : at < 3 ? 'main' : 'direction task', stopped: frozen },
          watchdog: at < 1 ? null : frozen ? 'fired' : at < 2 ? 'idle' : at < 3 ? 'passed' : 'counting',
          sensor: { holder: at < 4 ? null : at < 5 ? 'main' : 'feedback task', stopped: silent, state: at < 4 ? null : silent ? 'silent' : at < 5 ? 'passed' : 'measuring' },
          radio: { holder: at < 6 ? null : 'Wi-Fi task', state: at < 6 ? null : at < 7 ? 'joining' : offline ? 'retrying' : 'joined', flash: at === 7 && !offline },
        },
        say: frozen ? 'The chip reports that the watchdog restarted it: the last run froze. main switches the watchdog off and waits here for good, still holding all four pins at 0 V. Nothing else starts until someone has looked into it and reset the board.'
          : silent ? 'The sensor gives no answer. main waits here for good, keeping hold of the sensor so nothing else can use it. The radio never starts, so there is no dashboard. The direction task is still running: it keeps the pins at 0 V and keeps resetting the watchdog.'
          : offline ? 'There is no network to join, so the Wi-Fi task keeps trying. The light stays dark. Everything else is running, but the direction task will not raise a pin while the network is down.'
          : ['Power arrives. For the first few milliseconds ezal is not running: a loader built into the chip is finding it. Nothing drives the four pins yet; a resistor beside each switch holds it open.',
            'ezal’s first function, main, takes the four pins and puts 0 V on each. From this moment they are driven low.',
            'main asks the chip why it restarted. The watchdog was not the cause, so startup carries on.',
            'main starts the direction task and gives it the pins and the watchdog. The task starts the 500 ms countdown. From now on it is the only code that can change a pin.',
            'main tests the position sensor: it answers, it remembers what is written to it, and both inputs give a reading.',
            'main starts the feedback task and gives it the sensor. It measures both voltages about ten times a second.',
            'main switches the radio on, starts the dashboard’s web server and the remaining tasks, and ends. The Wi-Fi task begins joining your network.',
            'The network answers with an address, and the Pico’s light flashes three times. Startup is complete, and all four pins are still at 0 V.'][at],
      };
    }
    const redraw = canvas('board-wake-view', () => 342, (ctx, w, h) => {
      const now = state();
      drawBoard(ctx, w, h, now.levels, now.parts);
    });
    const update = () => {
      const now = state();
      const label = now.step > now.at ? `not reached: startup stopped at step ${now.at}` : now.offline ? 'no network to join' : STEPS[now.step];
      $('board-wake-step-value').textContent = `${now.step} of 7, ${label}`;
      slider.setAttribute('aria-valuetext', `Step ${now.step} of 7, ${label}`);
      $('board-wake-say').textContent = now.say;
      redraw();
    };
    slider.addEventListener('input', update);
    onChoice('board-wake-case', update);
    update();
  }

  // ── Chapter 2: position feedback ──────────────────────────────────────────

  // Figure: the resistor chain from HARDWARE.md. Along the chain the voltage falls in proportion to
  // resistance, so with resistance as the horizontal axis it is a straight ramp.
  {
    const slider = $('board-divider-v'), CHAIN = [33, 68, 82], TOTAL = 183, TAP = 82;
    const place = w => ({ x: 46, y: 24, w: w - 62, h: 190 });
    const redraw = canvas('board-divider-view', () => 296, (ctx, w) => {
      const p = place(w), volts = +slider.value, tap = volts * TAP / TOTAL;
      const X = kohm => p.x + kohm / TOTAL * p.w, Y = v => p.y + p.h - v / 5 * p.h;
      for (let v = 0; v <= 5; v++) {
        line(ctx, p.x, Y(v), p.x + p.w, Y(v), TINT[QUIET], 1);
        write(ctx, `${v} V`, p.x - 8, Y(v), { align: 'right', color: MUTED, size: 12 });
      }
      // The ramp, and the level at the tap.
      ctx.beginPath(); ctx.moveTo(X(0), Y(volts)); ctx.lineTo(X(TOTAL), Y(0)); ctx.lineTo(X(0), Y(0)); ctx.closePath();
      ctx.fillStyle = PALE; ctx.fill();
      line(ctx, p.x, Y(3.3), p.x + p.w, Y(3.3), WARN, 1.5, [6, 4]);
      // The ramp crosses this level in the left third of the plot; the label keeps to the right of it.
      write(ctx, ['3.3 V: the most the sensor may be shown', 'sensor’s limit: 3.3 V'], p.x + p.w - 4, Y(3.3) - 10, { align: 'right', color: WARN, size: 12, bold: true, room: p.w * 0.62 - 8 });
      line(ctx, X(0), Y(volts), X(TOTAL), Y(0), NAVY, 2.5);
      line(ctx, p.x, Y(tap), X(TOTAL - TAP), Y(tap), OK, 1.5, [4, 3]);
      line(ctx, X(TOTAL - TAP), Y(tap), X(TOTAL - TAP), Y(0) + 14, OK, 1.5, [4, 3]);
      disc(ctx, X(0), Y(volts), 6, NAVY);
      disc(ctx, X(TOTAL - TAP), Y(tap), 5, OK);
      write(ctx, [`${volts.toFixed(2)} V from the control box`, `${volts.toFixed(2)} V in`], X(0) + 12, Math.max(9, Y(volts) - 13), { color: NAVY, bold: true, room: p.w - 20 });
      write(ctx, [`${tap.toFixed(2)} V at the sensor`, `${tap.toFixed(2)} V`], X(TOTAL - TAP) + 10, Y(tap) - 11, { color: OK, bold: true, room: p.w - (X(TOTAL - TAP) - p.x) - 12 });
      // The three resistors, drawn to the same horizontal scale.
      const y = Y(0) + 24;
      let from = 0;
      // Both interface-board resistors get the same caption: the wording that fits under the narrower one.
      ctx.font = `400 12px ${FONT}`;
      const onBoard = ['on the interface board', 'interface board'].find(wording => ctx.measureText(wording).width <= X(CHAIN[1]) - X(0) - 4) || 'board';
      CHAIN.forEach((kohm, i) => {
        const x1 = X(from), x2 = X(from + kohm);
        rect(ctx, x1 + 5, y - 9, x2 - x1 - 10, 18, PAPER, NAVY, 3);
        line(ctx, x1, y, x1 + 5, y, NAVY, 1.5);
        line(ctx, x2 - 5, y, x2, y, NAVY, 1.5);
        write(ctx, [`${kohm} kΩ`, `${kohm}`], (x1 + x2) / 2, y, { align: 'center', size: 12, room: x2 - x1 - 14 });
        write(ctx, i ? onBoard : ['in the control box', 'in the box', 'box'], (x1 + x2) / 2, y + 22, { align: 'center', color: MUTED, size: 12, room: x2 - x1 - 4 });
        from += kohm;
      });
      [5, 3, 1].forEach((half, bar) => line(ctx, X(TOTAL) + 3 + bar * 3, y - half, X(TOTAL) + 3 + bar * 3, y + half, NAVY, 1.5));
      disc(ctx, X(TOTAL - TAP), y, 3.5, OK);
      write(ctx, ['↑ the sensor measures here', '↑ sensor'], X(TOTAL - TAP), y + 42, { align: 'center', color: OK, size: 12, bold: true, room: 2 * Math.min(X(TOTAL - TAP), w - X(TOTAL - TAP)) - 4 });
    });
    const update = () => {
      const volts = +slider.value, tap = volts * TAP / TOTAL;
      $('board-divider-v-value').textContent = `${volts.toFixed(2)} V`;
      slider.setAttribute('aria-valuetext', `${volts.toFixed(2)} volts`);
      $('board-divider-say').textContent = `${volts.toFixed(2)} V from the control box reaches the sensor as ${tap.toFixed(2)} V.${volts > 3.3 ? ' Wired straight to the sensor, this voltage would be above its 3.3 V limit.' : ''}`;
      redraw();
    };
    slider.addEventListener('input', update);
    drag('board-divider-view', slider, (x, y, w) => { const p = place(w); return clamp((p.y + p.h - y) / p.h * 5, 0, 5); });
    update();
  }

  // A dish as flat line art: a reflector and a boom along the direction (dx, dy) it points.
  function drawDish(ctx, x, y, length, dx, dy, color) {
    const half = length * 0.38, lean = length * 0.15;
    ctx.beginPath();
    ctx.moveTo(x - dy * half + dx * lean, y + dx * half + dy * lean);
    ctx.quadraticCurveTo(x - dx * lean, y - dy * lean, x + dy * half + dx * lean, y - dx * half + dy * lean);
    stroke(ctx, NAVY, 3.5);
    line(ctx, x, y, x + dx * length, y + dy * length, color, 2.5);
    triangle(ctx, x + dx * (length + 7), y + dy * (length + 7), x + dx * length - dy * 5, y + dy * length + dx * 5, x + dx * length + dy * 5, y + dy * length - dx * 5, color);
    disc(ctx, x, y, 3.5, NAVY);
  }
  // Azimuth: the dish from above, north up, angle growing clockwise.
  // With `window`, the last degrees before the end of travel are shaded and the end is marked.
  function drawDial(ctx, x, y, r, deg, window) {
    disc(ctx, x, y, r, PAPER, NAVY, 1.5);
    if (window) {
      ctx.beginPath(); ctx.moveTo(x, y); ctx.arc(x, y, r, (-90 - window) * RAD, -90 * RAD); ctx.closePath();
      ctx.fillStyle = TINT[AZ]; ctx.fill();
      line(ctx, x, y - r + 3, x, y - r - 9, NAVY, 4);
    }
    for (let a = 0; a < 360; a += 30) {
      const dx = Math.sin(a * RAD), dy = -Math.cos(a * RAD), inner = r - (a % 90 ? 5 : 9);
      line(ctx, x + dx * inner, y + dy * inner, x + dx * r, y + dy * r, NAVY, 1);
    }
    ['N', 'E', 'S', 'W'].forEach((point, i) => write(ctx, point, x + Math.sin(i * 90 * RAD) * (r - 17), y - Math.cos(i * 90 * RAD) * (r - 17), { align: 'center', color: MUTED, size: 12 }));
    drawDish(ctx, x, y, r - 28, Math.sin(deg * RAD), -Math.cos(deg * RAD), AZ);
  }
  // Elevation: the dish from the side, the horizon to the right, straight up at 90°.
  function drawArc(ctx, x, y, r, deg, window) {
    if (window) {
      ctx.beginPath(); ctx.moveTo(x, y); ctx.arc(x, y, r, -90 * RAD, (-90 + window) * RAD); ctx.closePath();
      ctx.fillStyle = TINT[EL]; ctx.fill();
      line(ctx, x, y - r + 3, x, y - r - 9, NAVY, 4);
    }
    ctx.beginPath(); ctx.arc(x, y, r, -Math.PI / 2, 0); stroke(ctx, NAVY, 1.5);
    line(ctx, x - 10, y, x + r + 8, y, NAVY, 1.5);
    for (let a = 0; a <= 90; a += 15) {
      const dx = Math.cos(a * RAD), dy = -Math.sin(a * RAD), inner = r - (a % 45 ? 5 : 9);
      line(ctx, x + dx * inner, y + dy * inner, x + dx * r, y + dy * r, NAVY, 1);
    }
    write(ctx, '0°', x + r + 4, y - 10, { color: MUTED, size: 12 });
    write(ctx, '90°', x + 8, y - r - 3, { color: MUTED, size: 12 });
    drawDish(ctx, x, y, r - 28, Math.cos(deg * RAD), -Math.sin(deg * RAD), EL);
  }

  // Figure: dish angle to sensor voltage to 2 mV steps to the angle the firmware computes.
  {
    const slider = $('board-line-deg');
    const axis = () => AXES[chosen('board-line-axis')];
    const place = w => w >= 640
      ? { h: 306, dish: { x: w * 0.14, y: 132, r: Math.min(104, w * 0.118) }, plot: { x: w * 0.345, y: 34, w: w * 0.34, h: 222 }, lens: { x: w * 0.755, y: 60, w: w * 0.24, h: 150 } }
      : { h: 444, dish: { x: w * 0.25, y: 84, r: Math.min(76, w * 0.21) }, lens: { x: w * 0.53, y: 30, w: w * 0.47 - 1, h: 106 }, plot: { x: 40, y: 246, w: w - 54, h: 150 } };
    const reported = mv => STEP_MV * Math.round(mv / STEP_MV);
    const redraw = canvas('board-line-view', w => place(w).h, (ctx, w) => {
      const { dish, plot, lens } = place(w), a = axis(), deg = +slider.value, mv = voltage(a, deg), says = reported(mv);
      const computed = position(a, says).deg;

      if (a === AXES.az) drawDial(ctx, dish.x, dish.y, dish.r, deg);
      else drawArc(ctx, dish.x - dish.r * 0.8, dish.y + dish.r * 0.8, dish.r * 1.6, deg);
      write(ctx, `${deg.toFixed(2)}°`, dish.x, dish.y + dish.r + 20, { align: 'center', color: a.color, bold: true, size: 15 });
      write(ctx, a === AXES.az ? ['the dish, seen from above', 'from above'] : ['the dish, seen from the side', 'from the side'], dish.x, dish.y + dish.r + 39, { align: 'center', color: MUTED, size: 12, room: dish.r * 2 + 30 });

      // The calibration: for each axis, a straight line between its two measured ends.
      const X = d => plot.x + d / 360 * plot.w, Y = v => plot.y + plot.h - v / 2200 * plot.h;
      [0, 500, 1000, 1500, 2000].forEach(v => {
        line(ctx, plot.x, Y(v), plot.x + plot.w, Y(v), TINT[QUIET], 1);
        write(ctx, String(v), plot.x - 6, Y(v), { align: 'right', color: MUTED, size: 12 });
      });
      [0, 90, 180, 270, 360].forEach(d => {
        line(ctx, X(d), plot.y + plot.h, X(d), plot.y + plot.h + 4, NAVY, 1);
        write(ctx, `${d}°`, X(d), plot.y + plot.h + 14, { align: 'center', color: MUTED, size: 12 });
      });
      line(ctx, plot.x, plot.y, plot.x, plot.y + plot.h, NAVY, 1);
      line(ctx, plot.x, plot.y + plot.h, plot.x + plot.w, plot.y + plot.h, NAVY, 1);
      write(ctx, [`${mv.toFixed(1)} mV at the sensor’s input`, `${mv.toFixed(1)} mV at the sensor`], plot.x - 30, plot.y - 16, { color: a.color, bold: true, room: plot.w + 34 });
      Object.values(AXES).forEach(each => {
        ctx.globalAlpha = each === a ? 1 : 0.4;
        line(ctx, X(each.minDeg), Y(each.mvAtMin), X(each.maxDeg), Y(each.mvAtMax), each.color, each === a ? 2.5 : 1.5);
        ctx.globalAlpha = 1;
      });
      write(ctx, 'azimuth', X(205) + 10, Y(voltage(AXES.az, 205)) + 16, { color: AZ, size: 12, bold: true });
      write(ctx, 'elevation', X(22) + 10, Y(voltage(AXES.el, 22)) - 2, { color: EL, size: 12, bold: true });
      // Each measured end, labelled on the side of the line that stays clear.
      (a === AXES.az ? [[0, 22, Y(0) - 9, 'left'], [360, -9, Y(a.mvAtMax) - 2, 'right']] : [[0, 9, Y(a.mvAtMin) - 13, 'left'], [90, 9, Y(a.mvAtMax) - 12, 'left']]).forEach(([d, dx, y, align]) => {
        const v = voltage(a, d);
        disc(ctx, X(d), Y(v), 4, PAPER, a.color, 2);
        write(ctx, `${v} mV at ${d}°`, X(d) + dx, y, { align, color: a.color, size: 12, halo: true });
      });
      line(ctx, plot.x, Y(mv), X(deg), Y(mv), a.color, 1, [3, 3]);
      line(ctx, X(deg), Y(mv), X(deg), plot.y + plot.h, a.color, 1, [3, 3]);
      disc(ctx, X(deg), Y(mv), 5, a.color);

      // Close-up of the same line around the dish: the sensor answers in steps of 2 mV.
      const perMv = (a.maxDeg - a.minDeg) / (a.mvAtMax - a.mvAtMin);
      const cx = lens.x + lens.w / 2, cy = lens.y + lens.h / 2, sx = lens.w / (5.6 * Math.abs(perMv)), sy = lens.h / 6.4;
      const LX = d => cx + (d - deg) * sx, LY = v => cy - (v - mv) * sy, angleAt = v => deg + (v - mv) * perMv;
      rect(ctx, lens.x, lens.y, lens.w, lens.h, PAPER);
      ctx.save();
      ctx.beginPath(); ctx.rect(lens.x, lens.y, lens.w, lens.h); ctx.clip();
      line(ctx, LX(angleAt(mv - 4)), LY(mv - 4), LX(angleAt(mv + 4)), LY(mv + 4), QUIET, 1.5);
      for (let step = says - 3 * STEP_MV; step <= says + 3 * STEP_MV; step += STEP_MV) {
        const from = LX(angleAt(step - 1)), to = LX(angleAt(step + 1)), current = step === says;
        line(ctx, to, LY(step), to, LY(step + STEP_MV), a.color, 1);
        line(ctx, from, LY(step), to, LY(step), a.color, current ? 4 : 1.5);
        // Above the half of the tread that the sloping line has not reached, a label is always clear.
        const at = Math.min(from, to) + Math.abs(to - from) * (perMv > 0 ? 0.3 : 0.7);
        if (LY(step) - 20 > lens.y && at - 26 > lens.x && at + 26 < lens.x + lens.w) write(ctx, `${step} mV`, at, LY(step) - 12, { align: 'center', color: current ? a.color : MUTED, size: 12, bold: current });
      }
      line(ctx, cx, cy, cx, LY(says), a.color, 1, [2, 2]);
      disc(ctx, cx, LY(says), 5, PAPER, a.color, 2.5);
      disc(ctx, cx, cy, 4, a.color);
      ctx.restore();
      rect(ctx, lens.x, lens.y, lens.w, lens.h, null, NAVY);
      rect(ctx, X(deg) - 7, Y(mv) - 7, 14, 14, null, NAVY);
      write(ctx, ['the sensor’s steps, close up', 'the steps, close up', 'close-up'], lens.x, lens.y - 13, { color: MUTED, size: 12, room: lens.w });
      write(ctx, `sensor reports ${says} mV`, lens.x, lens.y + lens.h + 17, { color: a.color, bold: true, room: lens.w + 4 }) ||
        write(ctx, `reports ${says} mV`, lens.x, lens.y + lens.h + 17, { color: a.color, bold: true });
      write(ctx, [`firmware computes ${computed.toFixed(2)}°`, `computes ${computed.toFixed(2)}°`], lens.x, lens.y + lens.h + 36, { color: a.color, bold: true, room: lens.w + 4 });
    });
    const update = () => {
      const a = axis(), deg = +slider.value, mv = voltage(a, deg), says = reported(mv);
      $('board-line-deg-value').textContent = `${deg.toFixed(2)}°`;
      slider.setAttribute('aria-valuetext', `${deg.toFixed(2)} degrees ${a.name}`);
      $('board-line-say').textContent = `The dish is at ${deg.toFixed(2)}° ${a.name}, which puts ${mv.toFixed(1)} mV on the sensor’s input. The sensor reports the nearest step, ${says} mV, and the firmware turns that into ${position(a, says).deg.toFixed(2)}°.`;
      redraw();
    };
    slider.addEventListener('input', update);
    onChoice('board-line-axis', () => {
      const az = axis() === AXES.az;
      slider.max = az ? 360 : 90; slider.step = az ? 0.05 : 0.02; slider.value = az ? 123.4 : 35;
      update();
    });
    drag('board-line-view', slider, (x, y, w, began) => {
      const { dish, plot, lens } = place(w), a = axis(), now = +slider.value;
      const within = (r, pad) => began.x >= r.x - pad && began.x <= r.x + r.w + pad && began.y >= r.y - pad && began.y <= r.y + r.h + pad;
      // The close-up is dragged like a map: it shows 5.6 mV across, so a small movement is a small angle.
      if (within(lens, 0)) return clamp(began.value - (x - began.x) / lens.w * 5.6 * (a.maxDeg - a.minDeg) / Math.abs(a.mvAtMax - a.mvAtMin), 0, a.maxDeg);
      if (within(plot, 10)) return clamp((x - plot.x) / plot.w * 360, 0, a.maxDeg);
      if (Math.hypot(began.x - dish.x, began.y - dish.y) > dish.r * 1.4) return null;
      if (a === AXES.el) return clamp(Math.atan2(dish.y + dish.r * 0.8 - y, x - (dish.x - dish.r * 0.8)) / RAD, 0, 90);
      // The rotator cannot pass through north: 0° and 360° are opposite ends of its travel.
      const turn = (Math.atan2(x - dish.x, dish.y - y) / RAD + 360) % 360;
      return now > 270 && turn < 90 ? 360 : now < 90 && turn > 270 ? 0 : turn;
    });
    update();
  }

  // Figure: which readings give a position. Mirrors validate_adc_readings and AxisMapping::voltage_to_angle.
  {
    const slider = $('board-bands-mv'), SPAN = 3300, NEAR = 45;
    const axis = () => AXES[chosen('board-bands-axis')];
    // Each measured end, lower voltage first, with the angle it stands for.
    const ends = a => [[a.mvAtMin, a.minDeg], [a.mvAtMax, a.maxDeg]].sort((one, other) => one[0] - other[0]);
    const place = w => {
      const wide = w >= 600, x = 14, each = wide ? (w - 2 * x - 30) / 2 : w - 2 * x;
      return {
        h: wide ? 204 : 298, track: { x, y: 50, w: w - 2 * x, h: 24, from: 0, to: SPAN },
        closeUps: a => ends(a).map(([mv], i) => ({ x: wide ? x + i * (each + 30) : x, y: wide || !i ? 142 : 236, w: each, h: 24, from: mv - NEAR, to: mv + NEAR })),
      };
    };
    const redraw = canvas('board-bands-view', w => place(w).h, (ctx, w) => {
      const p = place(w), a = axis(), mv = +slider.value, [[low, lowDeg], [high, highDeg]] = ends(a), result = position(a, mv);
      const color = result.refused ? STOP : result.past ? WARN : OK;
      const strip = (r, closeUp) => {
        const X = v => r.x + (clamp(v, r.from, r.to) - r.from) / (r.to - r.from) * r.w;
        [[0, low - MARGIN_MV, STOP, ['no position', 'none']], [low - MARGIN_MV, low, WARN, [`counts as ${lowDeg}°`, `${lowDeg}°`]],
          [low, high, OK, closeUp ? ['a position'] : [`${lowDeg}°`, `${highDeg}°`]], [high, high + MARGIN_MV, WARN, [`counts as ${highDeg}°`, `${highDeg}°`]],
          [high + MARGIN_MV, RAIL_MV, STOP, ['no position', 'none']], [RAIL_MV, SPAN, STOP, []]].forEach(([from, to, tone, words]) => {
          const x1 = X(from), x2 = X(to);
          if (x2 - x1 < 0.5) return;
          rect(ctx, x1, r.y, x2 - x1, r.h, from === RAIL_MV ? '#eab9ad' : TINT[tone]);
          if (!closeUp && tone === OK) {
            const clear = NEAR / SPAN * r.w + 7;
            write(ctx, words[0], x1 + clear, r.y + r.h / 2, { color: a.color, size: 12, bold: true });
            write(ctx, words[1], x2 - clear, r.y + r.h / 2, { color: a.color, size: 12, bold: true, align: 'right' });
            write(ctx, ['gives a position', 'position'], (x1 + x2) / 2, r.y + r.h / 2, { color: OK, size: 12, align: 'center', room: x2 - x1 - 2 * clear - 76 });
          } else if (closeUp || tone === STOP) {
            write(ctx, words, (x1 + x2) / 2, r.y + r.h / 2, { color: tone, size: 12, align: 'center', room: x2 - x1 - 8 });
          }
        });
        rect(ctx, r.x, r.y, r.w, r.h, null, NAVY);
        if (mv >= r.from && mv <= r.to) {
          line(ctx, X(mv), r.y - 12, X(mv), r.y + r.h + 3, color, 2.5);
          triangle(ctx, X(mv), r.y - 5, X(mv) - 7, r.y - 16, X(mv) + 7, r.y - 16, color);
        }
        return X;
      };

      const X = strip(p.track, false);
      line(ctx, p.track.x, p.track.y - 3, p.track.x, p.track.y + p.track.h + 3, STOP, 3);
      write(ctx, `${mv} mV`, clamp(X(mv), p.track.x + 28, p.track.x + p.track.w - 28), p.track.y - 30, { align: 'center', color, bold: true, size: 15 });
      [1000, 2000].forEach(v => {
        line(ctx, X(v), p.track.y + p.track.h, X(v), p.track.y + p.track.h + 4, NAVY, 1);
        write(ctx, String(v), X(v), p.track.y + p.track.h + 14, { align: 'center', color: MUTED, size: 12 });
      });
      write(ctx, '0 mV', p.track.x, p.track.y + p.track.h + 14, { color: STOP, size: 12, bold: true });
      write(ctx, '3200 mV', p.track.x + p.track.w, p.track.y + p.track.h + 14, { align: 'right', color: STOP, size: 12, bold: true });

      p.closeUps(a).forEach((r, i) => {
        const [end, deg] = ends(a)[i], CX = strip(r, true);
        rect(ctx, X(r.from) - 2, p.track.y - 4, X(r.to) - X(r.from) + 4, p.track.h + 8, null, NAVY);
        write(ctx, [`close-up of the ${deg}° end`, `the ${deg}° end`], r.x, r.y - 26, { color: MUTED, size: 12, room: r.w - 70 });
        [[end - MARGIN_MV, i ? null : WARN], [end, a.color], [end + MARGIN_MV, i ? WARN : null]].forEach(([v, tone]) => {
          if (!tone) return;
          line(ctx, CX(v), r.y + r.h, CX(v), r.y + r.h + 4, NAVY, 1);
          write(ctx, `${v} mV`, CX(v), r.y + r.h + 14, { align: 'center', color: tone, size: 12, bold: true });
        });
      });
    });
    const update = () => {
      const a = axis(), mv = +slider.value, [[low, lowDeg], [high, highDeg]] = ends(a), result = position(a, mv);
      const end = mv < low ? `${lowDeg}° end` : `${highDeg}° end`;
      $('board-bands-mv-value').textContent = `${mv} mV`;
      slider.setAttribute('aria-valuetext', `${mv} millivolts`);
      $('board-bands-say').textContent = result.refused === 'edge'
        ? (mv <= 0 ? '0 mV is the bottom edge of what the sensor can measure. A broken or shorted wire reads the same, so the reading is refused: no position.' : `${mv} mV is too close to the sensor’s 3.3 V supply to tell from a wire stuck there. Refused: no position.`)
        : result.refused ? `${mv} mV is ${result.past} mV past the ${end}, more than the 25 mV allowed. Refused: no position, so nothing may move.`
        : result.past ? `${mv} mV is ${result.past} mV past the ${end}, inside the 25 mV allowed for noise. It counts as exactly ${result.deg.toFixed(0)}°.`
        : `${mv} mV lies between the two measured ends: ${a.name} ${result.deg.toFixed(2)}°.`;
      redraw();
    };
    slider.addEventListener('input', update);
    onChoice('board-bands-axis', () => { slider.value = axis() === AXES.az ? 1074 : 670; update(); });
    drag('board-bands-view', slider, (x, y, w) => {
      const p = place(w), r = p.closeUps(axis()).find(r => y > r.y - 20 && y < r.y + r.h + 22 && x >= r.x && x <= r.x + r.w) || (y < p.track.y + p.track.h + 22 ? p.track : null);
      return r && r.from + clamp((x - r.x) / r.w, 0, 1) * (r.to - r.from);
    });
    update();
  }

  // A time axis shared by the two timing figures: 0 to 600 ms across the drawing.
  function timeAxis(ctx, w, y, title, marks) {
    const x = 14, T = ms => x + ms / 600 * (w - 2 * x);
    line(ctx, T(0), y, T(600), y, NAVY, 1);
    for (let ms = 0; ms <= 600; ms += 100) {
      const mark = marks.includes(ms);
      line(ctx, T(ms), y, T(ms), y + 4, NAVY, 1);
      write(ctx, String(ms), T(ms), y + 14, { align: ms ? ms === 600 ? 'right' : 'center' : 'left', color: mark ? WARN : MUTED, size: 12, bold: mark });
    }
    write(ctx, title, T(300), y + 33, { align: 'center', color: MUTED, size: 12 });
    return T;
  }

  // Figure: one reading grows old. The clock is noted before the read (firmware feedback.rs), a slow read
  // is refused when it arrives (FeedbackMonitor::sample) and an old one stops counting (FeedbackInterlock::current).
  {
    const read = $('board-age-read'), now = $('board-age-now');
    const redraw = canvas('board-age-view', () => 200, (ctx, w) => {
      const took = +read.value, at = +now.value, slow = took > MAX_READ_MS;
      const T = timeAxis(ctx, w, 154, 'milliseconds since the read began', [MAX_AGE_MS]);
      const arrived = at >= took, usable = arrived && !slow && at <= MAX_AGE_MS, color = !arrived ? MUTED : usable ? OK : STOP;
      // The read itself, and the limit on how long it may take.
      write(ctx, 'clock noted here', T(0), 11, { color: MUTED, size: 12 });
      line(ctx, T(0), 19, T(0), 154, NAVY, 1);
      line(ctx, T(MAX_READ_MS), 19, T(MAX_READ_MS), 50, WARN, 1.5, [5, 4]);
      rect(ctx, T(0), 24, Math.max(2, T(took) - T(0)), 20, slow ? TINT[STOP] : '#cfdcea', slow ? STOP : NAVY, 3);
      const words = [`reading both voltages: ${took} ms`, `the read: ${took} ms`], after = Math.max(T(took), T(MAX_READ_MS)) + 8;
      write(ctx, words, T(took) + 8, 34, { size: 12, room: T(MAX_READ_MS) - T(took) - 14 }) ||
        write(ctx, words, after, 34, { color: slow ? STOP : INK, size: 12, room: T(600) - after });
      write(ctx, ['a read may take 150 ms at most', 'limit: 150 ms'], T(MAX_READ_MS) + 6, 57, { color: WARN, size: 12, bold: true, room: T(600) - T(MAX_READ_MS) - 6 });
      // What the reading is worth at each moment.
      const zones = slow ? [[0, took, QUIET, ['no reading yet']], [took, 600, STOP, ['refused: the read was too slow', 'refused: too slow', 'refused']]]
        : [[0, took, QUIET, ['no reading yet']], [took, MAX_AGE_MS, OK, ['the reading may be used', 'may be used']], [MAX_AGE_MS, 600, STOP, ['too old', 'old']]];
      zones.forEach(([from, to, tone, wordings]) => {
        rect(ctx, T(from), 116, T(to) - T(from), 24, TINT[tone]);
        write(ctx, wordings, (T(from) + T(to)) / 2, 128, { align: 'center', color: tone === QUIET ? MUTED : tone, size: 12, room: T(to) - T(from) - 8 });
      });
      rect(ctx, T(0), 116, T(600) - T(0), 24, null, NAVY);
      line(ctx, T(MAX_AGE_MS), 108, T(MAX_AGE_MS), 154, WARN, 1.5, [5, 4]);
      // Now, and the age measured back to the moment the clock was noted.
      line(ctx, T(0), 102, T(at), 102, color, 2);
      line(ctx, T(0), 97, T(0), 107, color, 2);
      line(ctx, T(at), 87, T(at), 154, NAVY, 2);
      disc(ctx, T(at), 154, 6, NAVY);
      write(ctx, 'now', clamp(T(at), T(0) + 12, T(600) - 12), 78, { align: 'center', color: NAVY, bold: true });
      const tag = arrived ? `${at} ms old` : 'still reading';
      ctx.font = `600 12px ${FONT}`;
      if (ctx.measureText(tag).width <= T(at) - T(0) - 10) write(ctx, tag, (T(0) + T(at)) / 2, 92, { align: 'center', color, size: 12, bold: true });
      else write(ctx, tag, T(at) + 8, 96, { color, size: 12, bold: true });
    });
    const update = () => {
      const took = +read.value, at = +now.value;
      $('board-age-read-value').textContent = `${took} ms`;
      $('board-age-now-value').textContent = `${at} ms`;
      read.setAttribute('aria-valuetext', `${took} milliseconds`);
      now.setAttribute('aria-valuetext', `${at} milliseconds after the read began`);
      $('board-age-say').textContent = at < took ? `${at} ms in, the read is still under way. There is no new reading yet.`
        : took > MAX_READ_MS ? `The read took ${took} ms, over the 150 ms limit, so it is refused the moment it arrives. Until a quicker read succeeds there is no position, and nothing may move.`
        : at > MAX_AGE_MS ? `The reading is ${at} ms old, past the 500 ms limit. It no longer counts: nothing may move until a newer one arrives.`
        : `The read took ${took} ms and finished ${at - took} ms ago, but the reading is ${at} ms old: its age is counted from before the read began. It may be used.`;
      redraw();
    };
    read.addEventListener('input', update);
    now.addEventListener('input', update);
    drag('board-age-view', now, (x, y, w) => clamp((x - 14) / (w - 28) * 600, 0, 600));
    update();
  }

  // Figure: why a read may take 150 ms. Mirrors the compile-time assertion in core feedback.rs:
  // two reads at the limit, the pause between them and a scheduling allowance must fit in a reading's life.
  {
    const read = $('board-budget-read');
    const redraw = canvas('board-budget-view', () => 168, (ctx, w) => {
      const took = +read.value, slow = took > MAX_READ_MS, arrives = 2 * took + PAUSE_MS, spare = MAX_AGE_MS - arrives;
      const T = timeAxis(ctx, w, 122, 'milliseconds since the first read began', [MAX_AGE_MS]);
      const tone = slow ? STOP : NAVY, fill = slow ? TINT[STOP] : '#cfdcea', spareTone = spare >= 100 ? OK : WARN;
      [[0, took, fill, tone, ['first read', 'read']], [took, took + PAUSE_MS, PAPER, QUIET, ['pause']], [took + PAUSE_MS, arrives, fill, tone, ['next read', 'read']],
        [arrives, MAX_AGE_MS, TINT[spareTone], spareTone, ['to spare', 'spare']]].forEach(([from, to, colour, edge, words]) => {
        if (to <= from) return;
        rect(ctx, T(from), 30, T(to) - T(from), 26, colour, edge, 3);
        write(ctx, words, (T(from) + T(to)) / 2, 43, { align: 'center', color: edge === QUIET ? MUTED : edge, size: 12, room: T(to) - T(from) - 6 });
        write(ctx, [`${to - from} ms`, `${to - from}`], (T(from) + T(to)) / 2, 18, { align: 'center', color: edge === QUIET ? MUTED : edge, size: 12, bold: true, room: T(to) - T(from) - 2 });
      });
      line(ctx, T(MAX_AGE_MS), 8, T(MAX_AGE_MS), 122, WARN, 1.5, [5, 4]);
      triangle(ctx, T(arrives), 60, T(arrives) - 6, 70, T(arrives) + 6, 70, tone);
      ctx.font = `600 12px ${FONT}`;
      const words = ['the replacement arrives', 'replacement'].find(wording => ctx.measureText(wording).width < w - 40), half = ctx.measureText(words).width / 2;
      write(ctx, words, clamp(T(arrives), 14 + half, T(MAX_AGE_MS) - 6 - half), 80, { align: 'center', color: tone, size: 12, bold: true });
      write(ctx, ['the first reading is too old from here ▸', 'too old from here ▸'], T(MAX_AGE_MS) - 6, 102, { align: 'right', color: WARN, size: 12, bold: true, room: T(MAX_AGE_MS) - 20 });
    });
    const update = () => {
      const took = +read.value, arrives = 2 * took + PAUSE_MS, spare = MAX_AGE_MS - arrives;
      $('board-budget-read-value').textContent = `${took} ms`;
      read.setAttribute('aria-valuetext', `${took} milliseconds`);
      const sum = `${took} + ${PAUSE_MS} + ${took} = ${arrives} ms`;
      $('board-budget-say').textContent = took > MAX_READ_MS
        ? `A read of ${took} ms is over the limit and is refused. Here is why the limit exists: ${sum}, so a replacement this slow would arrive with ${spare > 0 ? `only ${spare} ms` : 'nothing'} to spare.`
        : `${sum}: the replacement arrives when the first reading is ${arrives} ms old, with ${spare} ms to spare before it turns 500 ms old.${took === MAX_READ_MS ? ' That last 100 ms is the allowance for tasks waiting their turn.' : ''}`;
      redraw();
    };
    read.addEventListener('input', update);
    drag('board-budget-view', read, (x, y, w) => clamp((x - 14) / (w - 28) * 600, 0, 200));
    update();
  }

  // Figure: the room kept clear before the end of travel. Mirrors AxisLimits::refusal_age_ms and
  // FeedbackInterlock::guard_drive for the high-angle end of the chosen axis.
  {
    const reading = $('board-room-mv'), age = $('board-room-age');
    const axis = () => AXES[chosen('board-room-axis')];
    // The slider counts sensor steps towards the end of travel; elevation's voltage falls as it gets there.
    const steps = a => Math.floor(a.window / (a.maxDeg - a.minDeg) * Math.abs(a.mvAtMax - a.mvAtMin) / STEP_MV);
    const nearest = a => a.mvAtMax > a.mvAtMin ? STEP_MV * Math.floor(a.mvAtMax / STEP_MV) : STEP_MV * Math.ceil(a.mvAtMax / STEP_MV);
    const millivolts = a => nearest(a) - Math.sign(a.mvAtMax - a.mvAtMin) * STEP_MV * (steps(a) - reading.value);
    const place = w => {
      const dish = w >= 600 ? { x: 80, y: 126, r: 60 } : null, x = dish ? 174 : 14;
      return { dish, x, w: w - x - 50 };
    };
    const redraw = canvas('board-room-view', () => 266, (ctx, w) => {
      const p = place(w), a = axis(), deg = position(a, millivolts(a)).deg, old = +age.value, refused = old >= refusalAge(a, deg);
      const from = a.maxDeg - a.window, A = d => p.x + (d - from) / a.window * p.w, end = A(a.maxDeg);
      // Where on the dish's travel the close-up sits.
      if (p.dish) {
        if (a === AXES.az) drawDial(ctx, p.dish.x, p.dish.y, p.dish.r, deg, a.window);
        else drawArc(ctx, p.dish.x - p.dish.r * 0.8, p.dish.y + p.dish.r * 0.8, p.dish.r * 1.6, deg, a.window);
        write(ctx, `the last ${a.window}° of travel`, p.dish.x, p.dish.y + p.dish.r + 24, { align: 'center', color: MUTED, size: 12 });
      }
      const out = `${a.out}, towards the end: ${refused ? 'refused' : 'allowed'} ▸`, back = `◂ ${a.back}, away from it: allowed`;
      ctx.font = `600 13px ${FONT}`;
      const oneRow = ctx.measureText(out).width + ctx.measureText(back).width + 24 < w - p.x;
      write(ctx, back, p.x, 12, { color: OK, bold: true });
      write(ctx, out, w - 2, oneRow ? 12 : 32, { align: 'right', color: refused ? STOP : OK, bold: true });
      // The last few degrees of travel, and where they end.
      rect(ctx, p.x, 66, end - p.x, 108, TINT[a.color]);
      line(ctx, p.x, 174, end, 174, NAVY, 1);
      for (let d = Math.ceil(from); d <= a.maxDeg; d += a.window > 6 ? 2 : 1) {
        line(ctx, A(d), 174, A(d), 178, NAVY, 1);
        write(ctx, `${d}°`, A(d), 188, { align: 'center', color: MUTED, size: 12 });
      }
      rect(ctx, end, 66, 8, 108, NAVY);
      write(ctx, 'end', end + 14, 78, { color: NAVY, size: 12, bold: true });
      // From where the dish was measured: how far it could have gone, could still coast, and could be misread.
      const moved = a.speed * old / 1000, coast = a.speed * COAST_MS / 1000, step = stepDeg(a);
      const chain = [[moved, TINT[WARN], WARN], [coast, '#e9c98f', WARN], [step, PAPER, NAVY]];
      let at = deg;
      chain.forEach(([length, fill, edge]) => {
        if (length > 0) rect(ctx, A(at), 108, A(at + length) - A(at), 26, fill, edge);
        at += length;
      });
      if (refused && A(at) > end + 8) rect(ctx, end + 8, 108, A(at) - end - 8, 26, TINT[STOP], STOP);
      line(ctx, A(deg), 70, A(deg), 174, a.color, 2.5);
      disc(ctx, A(deg), 70, 5, a.color);
      write(ctx, `measured ${deg.toFixed(2)}°`, clamp(A(deg), p.x + 50, end - 44), 54, { align: 'center', color: a.color, bold: true });
      [[`turned since the reading began: up to ${moved.toFixed(2)}°`, `since the reading: ${moved.toFixed(2)}°`], [`coast after the switch opens: up to ${coast.toFixed(2)}°`, `coast after switch-off: ${coast.toFixed(2)}°`], [`one sensor step, rounded up: ${step.toFixed(2)}°`, `one sensor step: ${step.toFixed(2)}°`]].forEach((words, i) => {
        rect(ctx, p.x, 205 + i * 19, 12, 12, chain[i][1], chain[i][2]);
        write(ctx, words, p.x + 20, 211.5 + i * 19, { size: 12, room: w - p.x - 24 });
      });
    });
    const update = () => {
      const a = axis(), mv = millivolts(a), deg = position(a, mv).deg, old = +age.value, refusedFrom = refusalAge(a, deg);
      const left = a.maxDeg - deg, travel = left - stepDeg(a) - a.speed * COAST_MS / 1000;
      $('board-room-mv-value').textContent = `${mv} mV, ${deg.toFixed(2)}°`;
      $('board-room-age-value').textContent = `${old} ms`;
      reading.setAttribute('aria-valuetext', `${mv} millivolts, ${deg.toFixed(2)} degrees`);
      age.setAttribute('aria-valuetext', `${old} milliseconds`);
      $('board-room-say').textContent = `Measured ${deg.toFixed(2)}°, ${left.toFixed(2)}° from the end of travel. ` + (refusedFrom === 0
        ? `One sensor step and the coast use up all of that, so ${a.out} is refused however fresh the reading.`
        : refusedFrom > MAX_AGE_MS ? `Take off one sensor step and the coast and ${travel.toFixed(2)}° is left, more than the dish can turn in a reading’s 500 ms life: ${a.out} is allowed.`
        : `Take off one sensor step and the coast and ${travel.toFixed(2)}° is left, which the dish could cover in ${refusedFrom} ms. The reading is ${old} ms old: ${a.out} is ${old >= refusedFrom ? 'refused' : 'allowed'}.`) + ` ${a.back[0].toUpperCase()}${a.back.slice(1)} is allowed.`;
      redraw();
    };
    const choose = () => {
      const a = axis();
      reading.max = steps(a);
      reading.value = steps(a) - (a === AXES.az ? 13 : 18);
      update();
    };
    reading.addEventListener('input', update);
    age.addEventListener('input', update);
    onChoice('board-room-axis', choose);
    drag('board-room-view', reading, (x, y, w) => {
      const p = place(w), a = axis(), deg = a.maxDeg - a.window + clamp((x - p.x) / p.w, 0, 1) * a.window;
      return steps(a) - Math.round(Math.abs(nearest(a) - voltage(a, deg)) / STEP_MV);
    });
    choose();
  }
})();
