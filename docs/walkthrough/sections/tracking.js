(() => {
  'use strict';
  const $ = id => document.getElementById(id);
  if (!$('tracking')) return;

  // ── Drawing helpers ──────────────────────────────────────────────────────
  // The colour vocabulary of style.css, for use inside drawings.
  const C = {
    az: '#245bce',
    el: '#087e83',
    sat: '#9b3fb5',
    warn: '#925100',
    ok: '#11643e',
    stop: '#a33317',
    navy: '#18354a',
    pale: '#edf4fa',
    line: '#c7d8e5',
    muted: '#506574',
  };
  // Lets a label sit on a line without the line running through its letters.
  const HALO = {stroke: '#fff', 'stroke-width': 4, 'paint-order': 'stroke', 'stroke-linejoin': 'round'};

  const draw = (tag, attrs, parent, content) => {
    const node = document.createElementNS('http://www.w3.org/2000/svg', tag);
    for (const key in attrs) node.setAttribute(key, attrs[key]);
    if (content !== undefined) node.textContent = content;
    if (parent) parent.appendChild(node);
    return node;
  };
  const label = (parent, x, y, content, attrs) =>
    draw('text', {x, y, 'font-size': 14, fill: C.navy, ...attrs}, parent, content);
  const line = (parent, x1, y1, x2, y2, attrs) =>
    draw('line', {x1, y1, x2, y2, stroke: C.navy, 'stroke-width': 1.5, ...attrs}, parent);
  const path = (parent, d, attrs) => draw('path', {d, fill: 'none', ...attrs}, parent);
  const dot = (parent, cx, cy, r, fill, attrs) => draw('circle', {cx, cy, r, fill, ...attrs}, parent);
  const box = (parent, x, y, width, height, attrs) => draw('rect', {x, y, width, height, ...attrs}, parent);

  const rad = degrees => degrees * Math.PI / 180;
  const clamp = (value, low, high) => Math.min(high, Math.max(low, value));
  const minutes = seconds => `${Math.floor(seconds / 60)}:${String(Math.floor(seconds % 60)).padStart(2, '0')}`;

  // A compass seen from above: north up, azimuth growing clockwise.
  const compass = (cx, cy, r, azimuth) => [cx + r * Math.sin(rad(azimuth)), cy - r * Math.cos(rad(azimuth))];
  const compassArc = (cx, cy, r, from, to) => {
    const [x0, y0] = compass(cx, cy, r, from);
    const [x1, y1] = compass(cx, cy, r, to);
    const large = ((to - from) % 360 + 360) % 360 > 180 ? 1 : 0;
    return `M${x0} ${y0}A${r} ${r} 0 ${large} 1 ${x1} ${y1}`;
  };
  const dial = (root, cx, cy, r) => {
    dot(root, cx, cy, r, C.pale, {stroke: C.navy, 'stroke-width': 1.5});
    for (let azimuth = 0; azimuth < 360; azimuth += 30) {
      line(root, ...compass(cx, cy, r, azimuth), ...compass(cx, cy, r - 6, azimuth), {'stroke-width': 1});
    }
    ['N', 'E', 'S', 'W'].forEach((name, quarter) => {
      const [x, y] = compass(cx, cy, r - 18, quarter * 90);
      label(root, x, y + 5, name, {'text-anchor': 'middle', fill: C.muted});
    });
    // Where the azimuth travel begins and ends.
    line(root, ...compass(cx, cy, r - 7, 0), ...compass(cx, cy, r + 7, 0), {stroke: C.stop, 'stroke-width': 4});
  };
  // An elevation seen from the side: the horizon to the right, straight up at 90°.
  const raised = (cx, cy, r, elevation) => [cx + r * Math.cos(rad(elevation)), cy - r * Math.sin(rad(elevation))];
  const quarter = (cx, cy, r) => `M${cx} ${cy}H${cx + r}A${r} ${r} 0 0 0 ${cx} ${cy - r}z`;

  // Readouts carry the drawing's colours: say(target, 'The ', ['c-sat', 'satellite'], ' is …').
  // A part is plain text, [class, text] for a coloured span, or ['b', text] for emphasis.
  const say = (target, ...parts) => target.replaceChildren(...parts.map(part => {
    if (typeof part === 'string') return part;
    const [kind, text] = part;
    const node = document.createElement(kind === 'b' ? 'strong' : 'span');
    if (kind !== 'b') node.className = kind;
    node.textContent = text;
    return node;
  }));

  // Each figure is drawn at its host's real width in pixels, so its text is as large on a phone as on a
  // desk. `paint(root, width)` draws into a fresh <svg> and returns the height it used.
  const mount = (id, description, paint) => {
    const host = $(id);
    let width = 0;
    const repaint = () => {
      if (!width) return;
      const root = draw('svg', {role: 'img', 'aria-label': description});
      const height = paint(root, width);
      root.setAttribute('viewBox', `0 0 ${width} ${height}`);
      root.setAttribute('width', width);
      root.setAttribute('height', height);
      host.replaceChildren(root);
    };
    new ResizeObserver(() => {
      const now = Math.floor(host.clientWidth);
      if (now && now !== width) {
        width = now;
        repaint();
      }
    }).observe(host);
    return repaint;
  };

  // ── What the planner rehearses ───────────────────────────────────────────
  // Every number is the repository's: RelayConfig::DEFAULT in drive.rs; MotorProfile::AC_50HZ, AC_SLOW and
  // AC_FAST in tracking.rs; PlanningConfig::default in planner.rs; feedback.rs; Scenario::NOMINAL in
  // simulation.rs; HARD_CODED_CALIBRATION in position.rs. Times are in milliseconds, angles in degrees.
  const STEP_MS = 20;           // one step of the rehearsal
  const MIN_ON = 500;           // a pin that goes high stays high this long
  const MIN_OFF = 2000;         // and then stays low this long
  const STARTUP = 300;          // the pin is high this long before the dish moves
  const COAST = 500;            // the dish keeps turning this long after the pin drops
  const BACKLASH = 1;           // slack in the gears, taken up on each reversal
  const DEADBAND = 0.15;        // the follower's margin against chatter
  const READ_PERIOD = 100;      // between position readings
  const READ_LATENCY = 100;     // from taking a reading to having it
  const READING_MAX_AGE = 500;  // an older reading asks for nothing
  const ALLOWED = 2;            // of the 3° budget, what the staircase may use
  const BUDGET_S = 180;         // seconds each motor may run before it must rest
  const REFERENCE = [360 / 70, 180 / 80];  // degrees per second the follower assumes: azimuth, elevation
  const MOTORS = {nominal: REFERENCE, slow: [4, 1.8], fast: [7, 3]};
  // Each axis spreads its travel over a voltage range (mV); the converter resolves 2 mV.
  const CALIBRATION = [{span: 360, low: 114, high: 2033}, {span: 90, low: 1281, high: 58}];
  // How far the shortest pulse carries the dish.
  const hop = axis => REFERENCE[axis] * (MIN_ON - STARTUP + COAST) / 1000;

  // Two passes over Clevedon, computed by the repository's orbit.rs from
  // crates/ezal-core/tests/fixtures/weather-2026-10-07.tle. `points` holds azimuth then elevation, sampled
  // every `every` seconds from the window opening, with one last sample at the moment it closes.
  const PASSES = {
    // METOP-C from 21:43:40 UTC on 7 October 2026: up to 36° in the west.
    metopc: {
      seconds: 729,
      every: 3,
      points: [
        193.13, 5.02, 193.31, 5.22, 193.49, 5.42, 193.68, 5.63, 193.86, 5.83, 194.05, 6.04, 194.24, 6.24, 194.44, 6.45,
        194.63, 6.66, 194.83, 6.87, 195.03, 7.09, 195.24, 7.30, 195.44, 7.52, 195.65, 7.73, 195.87, 7.95, 196.09, 8.17,
        196.31, 8.39, 196.53, 8.62, 196.75, 8.84, 196.98, 9.07, 197.22, 9.30, 197.45, 9.53, 197.70, 9.76, 197.94, 9.99,
        198.19, 10.23, 198.44, 10.47, 198.70, 10.71, 198.96, 10.95, 199.22, 11.19, 199.49, 11.44, 199.76, 11.68, 200.04, 11.93,
        200.32, 12.18, 200.61, 12.44, 200.90, 12.69, 201.20, 12.95, 201.50, 13.21, 201.81, 13.47, 202.12, 13.73, 202.44, 14.00,
        202.76, 14.26, 203.09, 14.53, 203.42, 14.80, 203.76, 15.08, 204.11, 15.35, 204.46, 15.63, 204.82, 15.91, 205.19, 16.19,
        205.56, 16.48, 205.94, 16.77, 206.33, 17.06, 206.72, 17.35, 207.13, 17.64, 207.54, 17.94, 207.95, 18.23, 208.38, 18.53,
        208.81, 18.84, 209.26, 19.14, 209.71, 19.45, 210.17, 19.76, 210.64, 20.07, 211.12, 20.38, 211.61, 20.69, 212.10, 21.01,
        212.61, 21.33, 213.13, 21.65, 213.66, 21.97, 214.20, 22.29, 214.75, 22.62, 215.32, 22.94, 215.89, 23.27, 216.48, 23.60,
        217.07, 23.93, 217.69, 24.26, 218.31, 24.59, 218.95, 24.92, 219.60, 25.25, 220.26, 25.59, 220.94, 25.92, 221.63, 26.25,
        222.34, 26.59, 223.06, 26.92, 223.79, 27.25, 224.54, 27.58, 225.31, 27.91, 226.09, 28.23, 226.89, 28.56, 227.70, 28.88,
        228.53, 29.20, 229.37, 29.52, 230.24, 29.83, 231.11, 30.15, 232.01, 30.45, 232.92, 30.75, 233.85, 31.05, 234.80, 31.34,
        235.76, 31.63, 236.74, 31.91, 237.73, 32.18, 238.74, 32.45, 239.77, 32.71, 240.82, 32.96, 241.88, 33.20, 242.95, 33.43,
        244.04, 33.65, 245.14, 33.87, 246.26, 34.07, 247.39, 34.26, 248.54, 34.44, 249.70, 34.61, 250.86, 34.77, 252.04, 34.91,
        253.23, 35.05, 254.43, 35.17, 255.63, 35.27, 256.85, 35.36, 258.06, 35.44, 259.29, 35.50, 260.52, 35.55, 261.75, 35.59,
        262.98, 35.61, 264.21, 35.61, 265.45, 35.60, 266.68, 35.58, 267.91, 35.54, 269.13, 35.49, 270.36, 35.42, 271.57, 35.34,
        272.78, 35.25, 273.98, 35.14, 275.18, 35.02, 276.36, 34.88, 277.53, 34.73, 278.70, 34.58, 279.85, 34.40, 280.99, 34.22,
        282.11, 34.03, 283.22, 33.82, 284.32, 33.61, 285.41, 33.38, 286.47, 33.15, 287.53, 32.91, 288.56, 32.65, 289.58, 32.40,
        290.59, 32.13, 291.58, 31.86, 292.55, 31.58, 293.50, 31.29, 294.44, 31.00, 295.36, 30.70, 296.27, 30.40, 297.16, 30.10,
        298.03, 29.79, 298.88, 29.48, 299.72, 29.16, 300.55, 28.84, 301.35, 28.52, 302.14, 28.20, 302.92, 27.87, 303.68, 27.54,
        304.42, 27.22, 305.15, 26.89, 305.87, 26.56, 306.57, 26.23, 307.25, 25.90, 307.92, 25.57, 308.58, 25.24, 309.23, 24.91,
        309.86, 24.58, 310.48, 24.25, 311.09, 23.92, 311.68, 23.60, 312.26, 23.27, 312.83, 22.95, 313.39, 22.62, 313.94, 22.30,
        314.48, 21.98, 315.00, 21.66, 315.52, 21.34, 316.02, 21.03, 316.52, 20.72, 317.01, 20.41, 317.48, 20.10, 317.95, 19.79,
        318.41, 19.48, 318.86, 19.18, 319.30, 18.88, 319.73, 18.58, 320.15, 18.28, 320.57, 17.99, 320.98, 17.69, 321.38, 17.40,
        321.77, 17.11, 322.16, 16.83, 322.54, 16.54, 322.91, 16.26, 323.28, 15.98, 323.63, 15.70, 323.99, 15.43, 324.33, 15.16,
        324.67, 14.88, 325.01, 14.62, 325.34, 14.35, 325.66, 14.08, 325.98, 13.82, 326.29, 13.56, 326.60, 13.30, 326.90, 13.05,
        327.20, 12.79, 327.49, 12.54, 327.78, 12.29, 328.06, 12.04, 328.34, 11.79, 328.61, 11.55, 328.88, 11.31, 329.15, 11.06,
        329.41, 10.83, 329.66, 10.59, 329.92, 10.35, 330.17, 10.12, 330.41, 9.89, 330.66, 9.66, 330.89, 9.43, 331.13, 9.20,
        331.36, 8.98, 331.59, 8.75, 331.81, 8.53, 332.04, 8.31, 332.25, 8.09, 332.47, 7.88, 332.68, 7.66, 332.89, 7.45,
        333.10, 7.24, 333.30, 7.02, 333.50, 6.82, 333.70, 6.61, 333.90, 6.40, 334.09, 6.20, 334.28, 5.99, 334.47, 5.79,
        334.66, 5.59, 334.84, 5.39, 335.02, 5.19, 335.20, 4.99,
      ],
    },
    // METOP-B from 09:33:00 UTC on 8 October 2026: up to 77°, nearly overhead, passing to the east.
    metopb: {
      seconds: 773,
      every: 3,
      points: [
        16.51, 4.98, 16.55, 5.20, 16.59, 5.41, 16.63, 5.63, 16.68, 5.85, 16.72, 6.07, 16.77, 6.29, 16.81, 6.52,
        16.86, 6.74, 16.90, 6.97, 16.95, 7.20, 16.99, 7.44, 17.04, 7.67, 17.09, 7.91, 17.14, 8.15, 17.19, 8.39,
        17.23, 8.63, 17.28, 8.88, 17.34, 9.13, 17.39, 9.38, 17.44, 9.63, 17.49, 9.89, 17.55, 10.15, 17.60, 10.41,
        17.65, 10.68, 17.71, 10.95, 17.77, 11.22, 17.82, 11.49, 17.88, 11.77, 17.94, 12.05, 18.00, 12.33, 18.06, 12.62,
        18.12, 12.91, 18.19, 13.20, 18.25, 13.50, 18.32, 13.80, 18.38, 14.11, 18.45, 14.41, 18.52, 14.73, 18.59, 15.04,
        18.66, 15.36, 18.73, 15.69, 18.80, 16.02, 18.88, 16.35, 18.96, 16.69, 19.03, 17.04, 19.11, 17.38, 19.19, 17.74,
        19.28, 18.10, 19.36, 18.46, 19.45, 18.83, 19.53, 19.21, 19.62, 19.59, 19.72, 19.97, 19.81, 20.37, 19.91, 20.76,
        20.00, 21.17, 20.10, 21.58, 20.21, 22.00, 20.31, 22.43, 20.42, 22.86, 20.53, 23.30, 20.64, 23.75, 20.76, 24.21,
        20.88, 24.67, 21.00, 25.14, 21.13, 25.62, 21.26, 26.11, 21.39, 26.61, 21.53, 27.12, 21.67, 27.64, 21.82, 28.16,
        21.97, 28.70, 22.13, 29.25, 22.29, 29.81, 22.45, 30.38, 22.62, 30.96, 22.80, 31.55, 22.99, 32.16, 23.18, 32.77,
        23.37, 33.40, 23.58, 34.05, 23.79, 34.70, 24.01, 35.37, 24.24, 36.06, 24.48, 36.76, 24.73, 37.47, 24.99, 38.20,
        25.26, 38.94, 25.55, 39.70, 25.84, 40.48, 26.16, 41.27, 26.48, 42.08, 26.83, 42.91, 27.19, 43.75, 27.57, 44.62,
        27.97, 45.50, 28.39, 46.39, 28.84, 47.31, 29.32, 48.25, 29.82, 49.20, 30.36, 50.17, 30.93, 51.16, 31.54, 52.17,
        32.19, 53.20, 32.89, 54.25, 33.65, 55.31, 34.46, 56.39, 35.33, 57.48, 36.28, 58.59, 37.32, 59.71, 38.44, 60.84,
        39.68, 61.99, 41.03, 63.14, 42.52, 64.29, 44.16, 65.45, 45.99, 66.60, 48.02, 67.75, 50.29, 68.88, 52.84, 70.00,
        55.70, 71.08, 58.93, 72.13, 62.57, 73.12, 66.67, 74.05, 71.28, 74.90, 76.42, 75.65, 82.10, 76.28, 88.28, 76.77,
        94.87, 77.11, 101.73, 77.26, 108.66, 77.24, 115.47, 77.05, 121.98, 76.68, 128.06, 76.15, 133.63, 75.49, 138.65, 74.72,
        143.15, 73.85, 147.15, 72.90, 150.69, 71.89, 153.84, 70.84, 156.63, 69.74, 159.11, 68.62, 161.33, 67.48, 163.31, 66.33,
        165.10, 65.17, 166.71, 64.01, 168.16, 62.86, 169.49, 61.70, 170.69, 60.56, 171.80, 59.42, 172.81, 58.30, 173.74, 57.19,
        174.61, 56.10, 175.40, 55.02, 176.14, 53.96, 176.83, 52.91, 177.48, 51.89, 178.08, 50.88, 178.64, 49.89, 179.17, 48.92,
        179.66, 47.96, 180.13, 47.03, 180.57, 46.11, 180.99, 45.22, 181.39, 44.34, 181.76, 43.48, 182.12, 42.63, 182.46, 41.81,
        182.78, 41.00, 183.09, 40.21, 183.38, 39.43, 183.66, 38.67, 183.93, 37.93, 184.18, 37.20, 184.43, 36.49, 184.67, 35.79,
        184.89, 35.11, 185.11, 34.44, 185.32, 33.78, 185.52, 33.14, 185.72, 32.51, 185.90, 31.89, 186.08, 31.29, 186.26, 30.70,
        186.43, 30.12, 186.59, 29.55, 186.75, 28.99, 186.90, 28.44, 187.05, 27.91, 187.19, 27.38, 187.33, 26.86, 187.47, 26.35,
        187.60, 25.86, 187.72, 25.37, 187.85, 24.89, 187.97, 24.41, 188.08, 23.95, 188.20, 23.50, 188.31, 23.05, 188.42, 22.61,
        188.52, 22.17, 188.63, 21.75, 188.73, 21.33, 188.82, 20.92, 188.92, 20.51, 189.01, 20.11, 189.10, 19.72, 189.19, 19.34,
        189.28, 18.95, 189.36, 18.58, 189.45, 18.21, 189.53, 17.85, 189.61, 17.49, 189.68, 17.13, 189.76, 16.79, 189.84, 16.44,
        189.91, 16.10, 189.98, 15.77, 190.05, 15.44, 190.12, 15.12, 190.19, 14.79, 190.25, 14.48, 190.32, 14.17, 190.38, 13.86,
        190.45, 13.55, 190.51, 13.25, 190.57, 12.95, 190.63, 12.66, 190.69, 12.37, 190.75, 12.08, 190.80, 11.80, 190.86, 11.52,
        190.91, 11.24, 190.97, 10.97, 191.02, 10.70, 191.07, 10.43, 191.12, 10.16, 191.17, 9.90, 191.22, 9.64, 191.27, 9.39,
        191.32, 9.13, 191.37, 8.88, 191.42, 8.63, 191.46, 8.39, 191.51, 8.14, 191.55, 7.90, 191.60, 7.66, 191.64, 7.42,
        191.68, 7.19, 191.73, 6.95, 191.77, 6.72, 191.81, 6.49, 191.85, 6.27, 191.89, 6.04, 191.93, 5.82, 191.97, 5.60,
        192.01, 5.38, 192.05, 5.16, 192.07, 5.02,
      ],
    },
  };
  // The satellite's [azimuth, elevation] at a moment of a pass, on a straight line between samples.
  const sky = (pass, seconds) => {
    const at = clamp(seconds, 0, pass.seconds);
    const i = Math.min(Math.floor(at / pass.every), pass.points.length / 2 - 2);
    const from = i * pass.every;
    const to = Math.min(from + pass.every, pass.seconds);
    return [0, 1].map(axis => {
      const start = pass.points[2 * i + axis];
      return start + (pass.points[2 * i + 2 + axis] - start) * (at - from) / (to - from);
    });
  };

  // tracking.rs pointing_deviation_deg: the true angle between two directions.
  const apart = (a, b) => {
    const unit = ([azimuth, elevation]) => [
      Math.cos(rad(elevation)) * Math.sin(rad(azimuth)),
      Math.cos(rad(elevation)) * Math.cos(rad(azimuth)),
      Math.sin(rad(elevation)),
    ];
    const u = unit(a);
    const v = unit(b);
    const cross = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
    const along = u[0] * v[0] + u[1] * v[1] + u[2] * v[2];
    return Math.atan2(Math.hypot(...cross), along) * 180 / Math.PI;
  };

  // simulation.rs quantize: what one reading reports, the angle rounded to a 2 mV step of the converter.
  const reading = (axis, angle) => {
    const {span, low, high} = CALIBRATION[axis];
    const millivolts = low + angle / span * (high - low);
    return (Math.round(millivolts / 2) * 2 - low) / (high - low) * span;
  };

  // controller.rs AxisFollower::request. A direction is true (the angle grows), false, or null (nothing).
  const request = (follower, error, velocity, sampledAt, now, rate) => {
    if (follower.active !== null) {
      // Running: carry on only while stopping at the earliest moment allowed would still fall short.
      if (follower.releasing) return null;
      const stop = Math.max(now, follower.started + MIN_ON);
      const movingFrom = Math.max(sampledAt, follower.started + STARTUP);
      const future = error + velocity * (stop - now + COAST) / 1000;
      const stillToTravel = rate * (Math.max(0, stop - movingFrom) + COAST) / 1000;
      const sign = follower.active ? 1 : -1;
      if (future * sign - stillToTravel > DEADBAND) return follower.active;
      follower.releasing = true;
      return null;
    }
    // Resting: wait for a reading taken after the last coast, then ask whether one pulse would help.
    if (follower.stopped !== null && sampledAt < follower.stopped + COAST) return null;
    const pulseTravel = rate * (MIN_ON - STARTUP + COAST) / 1000;
    const future = error + velocity * (MIN_ON + COAST) / 1000;
    return Math.abs(future) > 0.5 * pulseTravel + DEADBAND ? future > 0 : null;
  };

  // controller.rs AxisFollower::observe: remember when the output really changed.
  const observe = (follower, active, now) => {
    if (active === follower.active) return;
    if (follower.active !== null) follower.stopped = now;
    if (active !== null) follower.started = now;
    follower.releasing = false;
    follower.active = active;
  };

  // drive.rs AxisRelay::update: the shortest pulse and the rest after it, from chapter 4.
  const switchOutput = (relay, now) => {
    if (relay.actual === relay.target) return;
    if (relay.actual !== null) {
      if (now < relay.onUntil) return;
      relay.actual = null;
      relay.offUntil = now + MIN_OFF;
    }
    if (now >= relay.offUntil && relay.target !== null) {
      relay.actual = relay.target;
      relay.onUntil = now + MIN_ON;
    }
  };

  // model.rs AxisModel::travel: turning takes up the slack in the gears before it moves the dish.
  const turn = (motor, grows, ms, rate) => {
    if (!ms) return;
    const distance = rate * ms / 1000;
    if (motor.slack === null) motor.slack = grows ? BACKLASH : 0;
    const taken = grows ? Math.min(BACKLASH - motor.slack, distance) : Math.min(motor.slack, distance);
    motor.slack += grows ? taken : -taken;
    motor.angle += (grows ? 1 : -1) * (distance - taken);
  };

  // model.rs AxisModel::advance: a delay before moving, one speed, and a coast after the pin drops.
  const advance = (motor, output, ms, rate) => {
    if (output !== motor.applied) {
      if (motor.applied !== null && motor.coast === 0 && motor.moving !== null) motor.coast = COAST;
      if (motor.coast === 0) motor.moving = null;
      motor.startup = output !== null ? STARTUP : 0;
      motor.applied = output;
    }
    const coasting = Math.min(ms, motor.coast);
    if (motor.moving !== null) turn(motor, motor.moving, coasting, rate);
    motor.coast -= coasting;
    if (motor.coast === 0 && (coasting > 0 || output === null)) motor.moving = null;
    if (output === null) return;
    const starting = Math.min(ms - coasting, motor.startup);
    const driven = ms - coasting - starting;
    motor.startup -= starting;
    if (driven > 0) {
      turn(motor, output, driven, rate);
      motor.moving = output;
    }
  };

  // simulation.rs Simulation::step_inner, from the window opening until the last coast after it closes.
  // `rates` are the speeds the simulated motors really have; the follower goes on assuming REFERENCE.
  const rehearse = (pass, rates) => {
    const end = pass.seconds * 1000;
    const tenths = pass.seconds * 10 + 1;
    const run = {
      dish: new Float32Array(2 * tenths),  // azimuth then elevation, every tenth of a second
      off: new Float32Array(tenths),       // the worst pointing error within each tenth of a second
      pulses: [[], []],                    // per axis: [started, stopped, direction]
      worst: 0,
      outside: 0,                          // seconds spent further off than ALLOWED
    };
    const axes = sky(pass, 0).map(angle => ({
      angle, applied: null, moving: null, startup: 0, coast: 0, slack: null,
      follower: {active: null, started: 0, stopped: null, releasing: false},
      relay: {target: null, actual: null, onUntil: 0, offUntil: 0},
      since: 0,
    }));
    let serviced = 0;
    let pending = null;   // a reading that has been taken but has not arrived yet
    let nextRead = 0;
    let latest = null;    // the newest reading to have arrived
    let previous = null;  // the target one step ago, for the satellite's speed

    for (let now = 0; ; now += STEP_MS) {
      // The dish moves under whatever the pins were doing since the last step.
      axes.forEach((axis, k) => advance(axis, axis.relay.actual, now - serviced, rates[k]));
      serviced = now;

      if (!pending && now >= nextRead) {
        pending = {at: now, ready: now + READ_LATENCY, angles: axes.map((axis, k) => reading(k, axis.angle))};
      }
      if (pending && pending.ready <= now) {
        latest = pending;
        pending = null;
        nextRead = now + READ_PERIOD;
      }

      // The follower decides from the newest reading and how fast the target is moving.
      const target = now < end ? sky(pass, now / 1000) : null;
      axes.forEach(axis => observe(axis.follower, axis.relay.actual, now));
      let wanted = [null, null];
      if (target && latest && now - latest.at <= READING_MAX_AGE) {
        const velocity = [0, 1].map(k => previous ? (target[k] - previous[k]) / (STEP_MS / 1000) : 0);
        wanted = axes.map((axis, k) =>
          request(axis.follower, target[k] - latest.angles[k], velocity[k], latest.at, now, REFERENCE[k]));
        previous = target;
      } else {
        previous = null;
      }

      // The output timing of chapter 4 turns what is wanted into what the pin does.
      axes.forEach((axis, k) => {
        const before = axis.relay.actual;
        axis.relay.target = wanted[k];
        if (target) {
          switchOutput(axis.relay, now);
        } else if (before !== null) {
          // The window has closed: a stop cuts even the shortest pulse.
          axis.relay.actual = null;
          axis.relay.offUntil = now + MIN_OFF;
        }
        if (before !== null && axis.relay.actual !== before) run.pulses[k].push([axis.since, now, before]);
        if (axis.relay.actual !== null && axis.relay.actual !== before) axis.since = now;
        observe(axis.follower, axis.relay.actual, now);
        advance(axis, axis.relay.actual, 0, rates[k]);
      });

      if (now <= end) {
        const off = apart(axes.map(axis => axis.angle), sky(pass, now / 1000));
        const tenth = Math.floor(now / 100);
        if (now % 100 === 0) {
          run.dish[2 * tenth] = axes[0].angle;
          run.dish[2 * tenth + 1] = axes[1].angle;
        }
        run.off[tenth] = Math.max(run.off[tenth], off);
        run.worst = Math.max(run.worst, off);
        if (off > ALLOWED) run.outside += STEP_MS / 1000;
      } else if (axes.every(axis => axis.relay.actual === null && axis.coast === 0)) {
        return run;
      }
    }
  };

  // ── Figure: one pulse, and whether it is worth taking ────────────────────
  {
    const gap = $('tb-pulse-gap');
    const motor = $('tb-pulse-axis');
    const description = 'Above: the pin held high for half a second, and the angle of the dish rising over one ' +
      'second. Below: a number line with the dish, the satellite and the reach of one pulse.';

    const repaint = mount('tb-pulse-draw', description, (root, w) => {
      const axis = Number(motor.value);
      const ahead = Number(gap.value);
      const colour = axis ? C.el : C.az;
      const reach = hop(axis);
      const limit = reach / 2 + DEADBAND;

      // Above: time runs left to right, from a quarter of a second before the pin rises.
      const x = seconds => 10 + (seconds + 0.25) / 3.35 * (w - 20);
      label(root, x(-0.25), 14, 'Pin', {fill: C.muted});
      box(root, x(0.5), 22, x(2.5) - x(0.5), 40, {fill: C.warn, opacity: 0.1});
      label(root, (x(0.5) + x(2.5)) / 2, 47, 'off for at least 2 s', {fill: C.warn, 'text-anchor': 'middle'});
      path(root, `M${x(-0.25)} 58H${x(0)}V28H${x(0.5)}V58H${x(3.1)}`, {stroke: C.navy, 'stroke-width': 2});

      const name = axis ? 'Elevation of the dish' : 'Azimuth of the dish';
      label(root, x(3.1), 88, name, {fill: C.muted, 'text-anchor': 'end'});
      for (const [seconds, from] of [[0.3, 28], [0.5, 62], [1, 62]]) {
        line(root, x(seconds), from, x(seconds), 136, {stroke: C.line, 'stroke-width': 1, 'stroke-dasharray': '3 3'});
      }
      path(root, `M${x(-0.25)} 136H${x(0.3)}L${x(1)} 98H${x(3.1)}`, {stroke: colour, 'stroke-width': 2.5});
      label(root, x(1) + 8, 116, `${reach.toFixed(1)}°`, {fill: colour, 'font-weight': 700});
      if (w >= 640) {
        for (const [seconds, y, phase] of [[0.15, 128, 'starts'], [0.4, 119, 'runs'], [0.75, 101, 'coasts']]) {
          label(root, x(seconds), y, phase, {'text-anchor': 'middle', fill: C.muted, 'font-size': 13});
        }
      }
      for (const [seconds, tick] of [[0, '0'], [0.5, '0.5'], [1, '1'], [2.5, '2.5 s']]) {
        line(root, x(seconds), 136, x(seconds), 142, {'stroke-width': 1});
        label(root, x(seconds), 158, tick, {'text-anchor': 'middle', fill: C.muted});
      }

      // Below: degrees either side of where the dish points now.
      const at = degrees => w / 2 + degrees * (w - 28) / 12;
      const y = 214;
      const sign = Math.sign(ahead) || 1;
      const worthIt = Math.abs(ahead) > limit;
      box(root, at(-limit), y - 15, at(limit) - at(-limit), 30, {fill: C.pale, stroke: C.line});
      line(root, at(-6), y, at(6), y);
      for (let degrees = -6; degrees <= 6; degrees++) {
        line(root, at(degrees), y - 3, at(degrees), y + 3, {'stroke-width': 1});
        if (degrees % 2 || !degrees) continue;
        const anchor = degrees === -6 ? 'start' : degrees === 6 ? 'end' : 'middle';
        const tick = `${degrees < 0 ? '−' : ''}${Math.abs(degrees)}°`;
        label(root, at(degrees), y + 38, tick, {'text-anchor': anchor, fill: C.muted, 'font-size': 13});
      }
      if (worthIt) {
        // The hop, and where it would leave the dish.
        line(root, at(0), y, at(sign * reach) - sign * 7, y, {stroke: colour, 'stroke-width': 4});
        path(root, `M${at(sign * reach)} ${y}l${-sign * 10} -6v12z`, {fill: colour});
        path(root, `M${at(sign * reach)} ${y + 8}l-7 12h14z`, {stroke: colour, 'stroke-width': 1.5, 'stroke-dasharray': '3 2'});
      }
      path(root, `M${at(0)} ${y + 8}l-7 12h14z`, {fill: colour});
      label(root, at(0), y + 38, 'dish', {'text-anchor': 'middle', fill: colour});
      dot(root, at(ahead), y, 6, C.sat);
      label(root, clamp(at(ahead), 30, w - 30), y - 24, 'satellite', {'text-anchor': 'middle', fill: C.sat});

      const text = degrees => `${degrees.toFixed(2)}°`;
      const after = Math.abs(Math.abs(ahead) - reach);
      gap.setAttribute('aria-valuetext', `${text(Math.abs(ahead))} ${ahead < 0 ? 'behind' : 'ahead'}`);
      $('tb-pulse-gap-out').textContent = `${ahead < 0 ? '−' : ''}${text(Math.abs(ahead))}`;
      const opening = [
        'The ', ['c-sat', 'satellite'], ` is ${text(Math.abs(ahead))} from where the dish points. One pulse of the `,
        [axis ? 'c-el' : 'c-az', axis ? 'elevation motor' : 'azimuth motor'],
      ];
      if (worthIt) {
        say($('tb-pulse-read'), ...opening,
          ` carries the dish ${text(reach)} and leaves it ${text(after)} away instead. That is worth a pulse.`);
      } else {
        say($('tb-pulse-read'), ...opening,
          ` would carry the dish ${text(reach)} and leave it ${text(after)} away, `,
          after >= Math.abs(ahead) ? 'no better than now' : 'hardly better than now',
          `. The follower waits until the gap passes ${text(limit)}.`);
      }
      return 262;
    });
    gap.addEventListener('input', repaint);
    motor.addEventListener('change', repaint);
  }

  // ── Figure: the whole pass, replayed ─────────────────────────────────────
  {
    const time = $('tb-pass-time');
    const play = $('tb-pass-play');
    const which = $('tb-pass-which');
    const motors = $('tb-pass-motor');
    const read = $('tb-pass-read');
    const still = matchMedia('(prefers-reduced-motion: reduce)');
    const runs = {};  // one rehearsal per choice of pass and motors, made the first time it is asked for
    let pass;         // the pass on show
    let run;          // its rehearsal
    let live;         // the parts of the drawing that move with the time slider
    let frame = 0;    // the pending animation frame while playing, otherwise 0
    let clockAt = 0;
    const description = 'Three views of the dish and the satellite at one moment of the pass: from above, from ' +
      'the side, and along the line of sight of the dish. Below, when each motor ran and how far the dish was ' +
      'from the satellite, across the whole pass.';

    // The rehearsal kept the dish's two angles every tenth of a second.
    const dishAt = seconds => {
      const at = clamp(seconds, 0, pass.seconds) * 10;
      const i = Math.min(Math.floor(at), pass.seconds * 10 - 1);
      return [0, 1].map(axis => {
        const from = run.dish[2 * i + axis];
        return from + (run.dish[2 * i + 2 + axis] - from) * (at - i);
      });
    };
    // Where the satellite sits in the dish's own view, in degrees right of and above its aim.
    const view = seconds => {
      const satellite = sky(pass, seconds);
      const dish = dishAt(seconds);
      return [(satellite[0] - dish[0]) * Math.cos(rad(satellite[1])), satellite[1] - dish[1]];
    };
    const set = (node, attrs) => {
      for (const key in attrs) node.setAttribute(key, attrs[key]);
    };

    // Move the drawing and the readout to the slider's time.
    const update = () => {
      const seconds = Number(time.value);
      const satellite = sky(pass, seconds);
      const dish = dishAt(seconds);
      const off = apart(dish, satellite);

      const above = live.above;
      const [aboveX, aboveY] = compass(above.cx, above.cy, above.r, satellite[0]);
      set(above.satellite, {cx: aboveX, cy: aboveY});
      const [aimX, aimY] = compass(above.cx, above.cy, above.r - 4, dish[0]);
      set(above.dish, {x2: aimX, y2: aimY});

      const side = live.side;
      const [sideX, sideY] = raised(side.cx, side.cy, side.r, satellite[1]);
      set(side.satellite, {cx: sideX, cy: sideY});
      const [tiltX, tiltY] = raised(side.cx, side.cy, side.r - 4, dish[1]);
      set(side.dish, {x2: tiltX, y2: tiltY});

      // Along the aim: anything further off than 3.5° is held at the edge of the view.
      const along = live.along;
      const place = ([right, up]) => {
        const far = Math.hypot(right, up);
        const k = along.scale * Math.min(1, 3.5 / (far || 1));
        return [along.cx + right * k, along.cy - up * k];
      };
      const trail = [];
      for (let earlier = Math.max(0, seconds - 40); earlier < seconds; earlier += 0.5) {
        trail.push(place(view(earlier)).join(' '));
      }
      const here = place(view(seconds));
      trail.push(here.join(' '));
      set(along.trail, {points: trail.join(' ')});
      set(along.satellite, {cx: here[0], cy: here[1]});

      set(live.cursor, {transform: `translate(${live.x(seconds)} 0)`});

      const ran = axis => run.pulses[axis]
        .reduce((sum, [from, to]) => sum + clamp(seconds * 1000, from, to) - from, 0) / 1000;
      time.setAttribute('aria-valuetext', `${minutes(seconds)}, dish ${off.toFixed(1)} degrees off the satellite`);
      $('tb-pass-time-out').textContent = minutes(seconds);
      say(read,
        ['c-sat', `Satellite ${satellite[0].toFixed(1)}°, ${satellite[1].toFixed(1)}°`],
        ' · dish ', ['c-az', `${dish[0].toFixed(1)}°`], ', ', ['c-el', `${dish[1].toFixed(1)}°`],
        ' · ', [off > ALLOWED ? 'c-stop' : 'c-ok', `${off.toFixed(2)}° off`],
        ' · motors run so far: ', ['c-az', `${ran(0).toFixed(1)} s`], ' and ', ['c-el', `${ran(1).toFixed(1)} s`],
        ` of ${BUDGET_S} s each`);
    };

    // The three views of one moment. Returns the y where they end.
    const paintViews = (root, w) => {
      // Side by side on a wide page; on a phone the third view sits beneath the other two.
      const wide = w >= 640;
      const size = wide ? Math.min(250, (w - 56) / 3) : Math.min(150, (w - 16) / 2);
      const gutter = wide ? 28 : 16;
      const left = (w - (wide ? 3 : 2) * size - (wide ? 2 : 1) * gutter) / 2;
      const top = 24;
      const third = wide ? size : Math.min(210, w - 30);
      const thirdX = wide ? left + 2 * (size + gutter) : (w - third) / 2;
      const thirdY = wide ? top : top + size + 34;
      const heading = {'text-anchor': 'middle', fill: C.muted};
      const sweep = {stroke: C.sat, 'stroke-width': 5, opacity: 0.35};  // where the satellite goes
      const marker = {stroke: '#fff'};

      label(root, left + size / 2, 15, 'From above', heading);
      const above = live.above = {cx: left + size / 2, cy: top + size / 2, r: size / 2 - 10};
      dial(root, above.cx, above.cy, above.r);
      path(root, compassArc(above.cx, above.cy, above.r, sky(pass, 0)[0], sky(pass, pass.seconds)[0]), sweep);
      above.dish = line(root, above.cx, above.cy, 0, 0, {stroke: C.az, 'stroke-width': 3, 'stroke-linecap': 'round'});
      dot(root, above.cx, above.cy, 4, C.az);
      above.satellite = dot(root, 0, 0, 5.5, C.sat, marker);

      label(root, left + size * 1.5 + gutter, 15, 'From the side', heading);
      const side = live.side = {cx: left + size + gutter + 16, cy: top + size - 26, r: size - 50};
      const highest = Math.max(...pass.points.filter((_, i) => i % 2));
      line(root, side.cx - 10, side.cy, side.cx + side.r + 14, side.cy);
      path(root, quarter(side.cx, side.cy, side.r), {fill: C.pale, stroke: C.navy, 'stroke-width': 1.5});
      const [lowX, lowY] = raised(side.cx, side.cy, side.r, sky(pass, 0)[1]);
      const [highX, highY] = raised(side.cx, side.cy, side.r, highest);
      path(root, `M${lowX} ${lowY}A${side.r} ${side.r} 0 0 0 ${highX} ${highY}`, sweep);
      label(root, side.cx + side.r, side.cy + 18, '0°', {'text-anchor': 'middle', fill: C.muted});
      label(root, side.cx + 8, side.cy - side.r - 6, '90°', {fill: C.muted});
      side.dish = line(root, side.cx, side.cy, 0, 0, {stroke: C.el, 'stroke-width': 3, 'stroke-linecap': 'round'});
      dot(root, side.cx, side.cy, 4, C.el);
      side.satellite = dot(root, 0, 0, 5.5, C.sat, marker);

      label(root, thirdX + third / 2, thirdY - 9, 'Along the dish’s aim', heading);
      const along = live.along = {cx: thirdX + third / 2, cy: thirdY + third / 2, scale: (third / 2 - 4) / 3.7};
      dot(root, along.cx, along.cy, 3.7 * along.scale, C.pale);
      dot(root, along.cx, along.cy, 3 * along.scale, 'none', {stroke: C.line});
      const ring = ALLOWED * along.scale;
      dot(root, along.cx, along.cy, ring, 'none', {stroke: C.warn, 'stroke-width': 1.5, 'stroke-dasharray': '5 4'});
      label(root, along.cx, along.cy - ring - 5, '2° allowed', {'text-anchor': 'middle', fill: C.warn, 'font-size': 13});
      line(root, along.cx - 9, along.cy, along.cx + 9, along.cy);
      line(root, along.cx, along.cy - 9, along.cx, along.cy + 9);
      along.trail = draw('polyline', {fill: 'none', stroke: C.sat, 'stroke-width': 1.5, opacity: 0.45}, root);
      along.satellite = dot(root, 0, 0, 5.5, C.sat, marker);

      return wide ? top + size : thirdY + third;
    };

    // The whole pass at a glance: when each motor ran, and how far off the dish was. Returns the height used.
    const paintStrips = (root, w, top) => {
      const x = live.x = seconds => 9 + seconds / pass.seconds * (w - 18);
      const cursor = live.cursor = draw('g', {});
      let y = top + 26;

      ['Azimuth motor running', 'Elevation motor running'].forEach((name, axis) => {
        const colour = axis ? C.el : C.az;
        label(root, 9, y, name, {fill: colour});
        box(root, 9, y + 6, w - 18, 20, {fill: C.pale});
        // A pulse that raises the angle sits in the upper half of its row, one that lowers it in the lower half.
        for (const [from, to, grows] of run.pulses[axis]) {
          const width = Math.max(1.6, x(to / 1000) - x(from / 1000));
          box(root, x(from / 1000), y + (grows ? 6 : 16), width, 10, {fill: colour});
        }
        line(cursor, 0, y + 3, 0, y + 29, {'stroke-width': 2});
        y += 46;
      });

      label(root, 9, y, 'Degrees off the satellite');
      const base = y + 76;
      const high = degrees => base - degrees / 3.5 * 68;
      box(root, 9, high(3.5), w - 18, 68, {fill: C.pale});
      // The worst error in each half second. Drawn twice: navy below the 2° line, red above it.
      let trace = '';
      for (let tenth = 0; tenth < run.off.length; tenth += 5) {
        const worst = Math.min(3.5, Math.max(...run.off.subarray(tenth, tenth + 5)));
        trace += `${tenth ? 'L' : 'M'}${x(tenth / 10).toFixed(1)} ${high(worst).toFixed(1)}`;
      }
      const bands = [
        ['tb-pass-inside', high(ALLOWED), base, C.navy],
        ['tb-pass-outside', high(3.5), high(ALLOWED), C.stop],
      ];
      for (const [id, from, to, colour] of bands) {
        box(draw('clipPath', {id}, root), 0, from, w, to - from);
        path(root, trace, {stroke: colour, 'stroke-width': 1.5, 'clip-path': `url(#${id})`});
      }
      line(root, 9, high(ALLOWED), w - 9, high(ALLOWED), {stroke: C.warn, 'stroke-dasharray': '5 4'});
      label(root, w - 12, high(ALLOWED) - 5, '2° allowed', {'text-anchor': 'end', fill: C.warn, 'font-size': 13});
      const every = w >= 420 ? 2 : 3;
      for (let minute = 0; minute <= 12; minute += every) {
        const anchor = minute === 0 ? 'start' : minute === 12 ? 'end' : 'middle';
        line(root, x(minute * 60), base, x(minute * 60), base + 5, {'stroke-width': 1});
        label(root, x(minute * 60), base + 20, minute === 12 ? '12 min' : minute, {
          'text-anchor': anchor, fill: C.muted, 'font-size': 13,
        });
      }
      line(cursor, 0, high(3.5) - 3, 0, base + 5, {'stroke-width': 2});
      root.appendChild(cursor);

      // Dragging anywhere on the strips moves the time, like the slider beneath.
      const scrub = box(root, 0, top + 12, w, base + 24 - top - 12, {fill: 'transparent', class: 'tb-scrub'});
      const seek = event => {
        const frame = root.getBoundingClientRect();
        const at = (event.clientX - frame.left) * w / frame.width;
        time.value = clamp((at - 9) / (w - 18), 0, 1) * pass.seconds;
        pause();
        update();
      };
      scrub.addEventListener('pointerdown', event => {
        scrub.setPointerCapture(event.pointerId);
        seek(event);
      });
      scrub.addEventListener('pointermove', event => {
        if (event.buttons) seek(event);
      });
      return base + 26;
    };

    const repaint = mount('tb-pass-draw', description, (root, w) => {
      const key = `${which.value} ${motors.value}`;
      pass = PASSES[which.value];
      run = runs[key] || (runs[key] = rehearse(pass, MOTORS[motors.value]));
      time.max = pass.seconds;
      live = {};
      const height = paintStrips(root, w, paintViews(root, w));
      update();
      const longest = axis => Math.max(...run.pulses[axis].map(([from, to]) => to - from)) / 1000;
      say($('tb-pass-sum'),
        'Whole pass with these motors: ',
        ['c-az', `${run.pulses[0].length} azimuth pulses`], ` (the longest ${longest(0).toFixed(1)} s), `,
        ['c-el', `${run.pulses[1].length} elevation pulses`], ` (the longest ${longest(1).toFixed(1)} s). `,
        `Furthest off: ${run.worst.toFixed(2)}°. Outside the 2° allowance for `,
        [run.outside ? 'c-stop' : 'c-ok', `${run.outside.toFixed(1)} s`], '.');
      return height;
    });

    const pause = () => {
      cancelAnimationFrame(frame);
      frame = 0;
      play.textContent = still.matches ? 'Step 30 s' : 'Play';
      read.setAttribute('aria-live', 'polite');
    };
    // The whole pass replayed in thirty seconds.
    const step = now => {
      time.value = Math.min(pass.seconds, Number(time.value) + (now - clockAt) / 1000 * pass.seconds / 30);
      clockAt = now;
      update();
      if (Number(time.value) >= pass.seconds) pause();
      else frame = requestAnimationFrame(step);
    };
    play.addEventListener('click', () => {
      if (frame) return pause();
      if (Number(time.value) >= pass.seconds) time.value = 0;
      if (still.matches) {
        // No animation for a reader who has asked for none: each press moves on half a minute.
        time.value = Math.min(pass.seconds, Number(time.value) + 30);
        return update();
      }
      play.textContent = 'Pause';
      read.setAttribute('aria-live', 'off');  // a screen reader should not be told every frame
      clockAt = performance.now();
      frame = requestAnimationFrame(step);
    });
    time.addEventListener('input', () => {
      pause();
      update();
    });
    which.addEventListener('change', () => {
      pause();
      time.value = 0;
      repaint();
    });
    motors.addEventListener('change', repaint);
    still.addEventListener('change', pause);
    pause();
  }

  // ── Figure: one direction, two pairs of motor angles, and a stop at north ─
  {
    const slider = $('tb-north-az');
    const flip = $('tb-north-flip');
    const ELEVATION = 40;
    const description = 'Left: the rotator from above, with the end of its travel at north, the satellite on the rim and ' +
      'the pointer of the azimuth motor. Right: the rotator from the side, with the dish raised toward the satellite.';

    const repaint = mount('tb-north-draw', description, (root, w) => {
      // The slider carries the satellite from 60° east of north, across north, to 60° west of it.
      const unwound = 60 - slider.value;
      const azimuth = (unwound + 360) % 360;
      const flipped = flip.checked;
      const blocked = !flipped && unwound < 0;
      const motorAzimuth = flipped ? (azimuth + 180) % 360 : blocked ? 0 : azimuth;
      const motorElevation = flipped ? 180 - ELEVATION : ELEVATION;

      // From above. On a wide page the side view sits to its right, on a phone beneath it.
      const wide = w >= 520;
      const size = wide ? Math.min(250, (w - 40) / 2) : Math.min(230, w - 20);
      const cx = wide ? w / 2 - 20 - size / 2 : w / 2;
      const cy = 24 + size / 2;
      const r = size / 2 - 18;
      const pointer = blocked ? C.stop : C.az;
      dial(root, cx, cy, r);
      path(root, compassArc(cx, cy, r + 9, 4, 356), {stroke: C.az, 'stroke-width': 4, opacity: 0.3});  // its travel
      if (blocked) path(root, compassArc(cx, cy, r, azimuth, 360), {stroke: C.stop, 'stroke-width': 5});
      line(root, cx, cy, ...compass(cx, cy, r, azimuth), {stroke: C.sat, 'stroke-dasharray': '5 4'});
      line(root, cx, cy, ...compass(cx, cy, r - 4, motorAzimuth), {stroke: pointer, 'stroke-width': 3, 'stroke-linecap': 'round'});
      dot(root, cx, cy, 4, pointer);
      dot(root, ...compass(cx, cy, r, azimuth), 6, C.sat, {stroke: '#fff'});
      label(root, cx + 10, cy - r - 12, 'limit', {fill: C.stop, 'font-size': 13});

      // From the side, looking across the way the azimuth motor faces. Past 90° the dish leans over backwards.
      const sr = wide ? r : Math.min(r, (w - 90) / 2);
      const mx = wide ? w / 2 + 20 + size / 2 : w / 2;
      const my = wide ? cy + sr / 2 + 4 : 24 + size + 30 + sr;
      const colour = flipped ? C.stop : C.el;
      const [ux, uy] = [Math.cos(rad(motorElevation)), -Math.sin(rad(motorElevation))];
      const [bowlX, bowlY] = raised(mx, my, sr * 0.4, motorElevation);
      const [seenX, seenY] = raised(mx, my, sr, motorElevation);
      line(root, mx - sr - 16, my, mx + sr + 16, my);
      path(root, quarter(mx, my, sr), {fill: C.pale});
      path(root, `M${mx + sr} ${my}A${sr} ${sr} 0 0 0 ${mx} ${my - sr}`, {stroke: C.el, 'stroke-width': 2});
      path(root, `M${mx} ${my - sr}A${sr} ${sr} 0 0 0 ${mx - sr} ${my}`, {stroke: C.muted, 'stroke-dasharray': '4 4'});
      line(root, bowlX, bowlY, seenX, seenY, {stroke: C.sat, 'stroke-dasharray': '5 4'});
      line(root, mx, my, bowlX, bowlY, {stroke: colour, 'stroke-width': 3});
      // The dish: a shallow bowl on the end of its arm, open toward where it looks.
      const rim = side => `${bowlX + side * -uy * 18 + ux * 9} ${bowlY + side * ux * 18 + uy * 9}`;
      path(root, `M${rim(1)}Q${bowlX - ux * 9} ${bowlY - uy * 9} ${rim(-1)}`, {
        stroke: colour, 'stroke-width': 3, 'stroke-linecap': 'round',
      });
      dot(root, mx, my, 4, colour);
      dot(root, seenX, seenY, 6, C.sat, {stroke: '#fff'});
      label(root, mx + sr, my + 18, '0°', {'text-anchor': 'middle', fill: C.muted});
      label(root, mx, my - sr - 7, '90°', {'text-anchor': 'middle', fill: C.muted});
      label(root, mx - sr, my + 18, '180°', {'text-anchor': 'middle', fill: C.muted});
      label(root, mx, my + 36, 'the way the azimuth motor faces  →', {'text-anchor': 'middle', fill: C.muted, 'font-size': 13});

      slider.setAttribute('aria-valuetext', `${azimuth} degrees`);
      $('tb-north-az-out').textContent = `${azimuth}°`;
      const where = ['c-sat', `Satellite at azimuth ${azimuth}°, ${ELEVATION}° up.`];
      if (flipped) {
        say($('tb-north-read'), where, ' Flipped over the top: ',
          ['c-az', `azimuth motor at ${motorAzimuth}°`], ', half a turn away, and ',
          ['c-stop', `elevation motor at ${motorElevation}°`],
          `, which is ${ELEVATION}° above the horizon behind it. The dish looks at the same spot, and north is no ` +
          `longer in the way. But ${motorElevation}° is past the 90° that the elevation calibration allows today.`);
      } else if (blocked) {
        const longWay = 360 + unwound;
        say($('tb-north-read'), where, ' ', ['c-stop', 'The azimuth motor is at its limit, 0°.'],
          ` The satellite is ${-unwound}° beyond it. The only way there is back round the other way: ${longWay}° ` +
          `of turning, about ${Math.round(longWay / REFERENCE[0])} s at full speed, looking at the wrong sky all the while.`);
      } else {
        say($('tb-north-read'), where, ' ', ['c-az', `Azimuth motor at ${motorAzimuth}°`], ', ',
          ['c-el', `elevation motor at ${motorElevation}°`],
          `. Motor angles and sky direction are the same numbers, and the limit is ${unwound}° away.`);
      }
      return wide ? 24 + size + 8 : my + 46;
    });
    slider.addEventListener('input', repaint);
    flip.addEventListener('change', repaint);
  }

  // ── Figure: two clocks tied at one matched pair ──────────────────────────
  {
    // clock.rs: of the 1° held in reserve, 0.5° is left for alignment and orbit and 0.5° goes to timing.
    // At the planner's 2° per second that is 250 ms. The board's crystal is allowed 100 parts in a million.
    const SPARE_MS = 250;
    const DOUBT_MS = 20;  // how wrong the example's new answer says it might be
    const DRIFT_MS = Math.round((1949 - 1300) * 100 / 1000);  // from the new answer until the window closes
    const slider = $('tb-clock-gap');
    const description = 'Two timelines, UTC above and uptime below, tied together at one matched pair of readings. ' +
      'Further along, a new time check is compared with the run’s own clock, and a bar shows how much of the ' +
      '250 millisecond allowance it uses.';

    const repaint = mount('tb-clock-draw', description, (root, w) => {
      const differs = Number(slider.value);
      const total = Math.abs(differs) + DOUBT_MS + DRIFT_MS;
      const stops = total > SPARE_MS;
      const verdict = stops ? C.stop : C.ok;
      const pad = w < 520 ? 34 : 70;
      const small = w < 520 ? 13 : 14;
      const column = i => pad + i * (w - 2 * pad) / 3;  // four moments, evenly spaced rather than to scale
      const utc = 96;
      const uptime = 146;
      const centred = {'text-anchor': 'middle', 'font-size': small};
      const strong = {'font-weight': 700, 'font-size': small};

      [['last time', 'check'], ['window', 'opens'], ['next time', 'check'], ['window', 'closes']].forEach((rows, i) => {
        rows.forEach((row, n) => label(root, column(i), 14 + n * 17, row, {...centred, fill: C.muted}));
      });
      label(root, 4, 62, 'UTC, from a time server', strong);
      label(root, 4, 190, 'Uptime: the board’s count since power-on', strong);
      line(root, 4, utc, w - 4, utc);
      line(root, 4, uptime, w - 4, uptime);
      for (const y of [utc, uptime]) line(root, column(1), y, column(3), y, {stroke: C.sat, 'stroke-width': 8, opacity: 0.3});
      ['21:40:00', '21:43:40', '21:45:00', '21:55:49'].forEach((text, i) => label(root, column(i), utc - 9, text, centred));
      ['1 000 s', '1 220 s', '1 300 s', '1 949 s'].forEach((text, i) => label(root, column(i), uptime + 21, text, centred));

      // The matched pair, and the two moments of the schedule that are counted from it.
      line(root, column(0), utc, column(0), uptime, {'stroke-width': 4});
      if (w >= 520) label(root, column(0) + 9, (utc + uptime) / 2 + 5, 'the matched pair', {'font-size': 13});
      line(root, column(1), utc, column(1), uptime, {'stroke-width': 1, 'stroke-dasharray': '2 3'});
      label(root, column(1) + 5, (utc + uptime) / 2 + 5, '+220 s', {fill: C.muted, 'font-size': 13});
      line(root, column(3), utc, column(3), uptime, {'stroke-width': 1, 'stroke-dasharray': '2 3'});
      label(root, column(3) - 5, (utc + uptime) / 2 + 5, '+949 s', {fill: C.muted, 'font-size': 13, 'text-anchor': 'end'});

      // The next check. Its disagreement is drawn hundreds of times larger than life, or it could not be seen.
      const answer = column(2) + differs / 400 * Math.min(44, (w - 2 * pad) / 6 - 4);
      line(root, column(2), uptime, answer, utc, {stroke: verdict, 'stroke-width': 2, 'stroke-dasharray': '5 3'});
      dot(root, answer, utc, 5, verdict);
      dot(root, column(2), uptime, 4, C.navy);
      for (const i of [0, 1, 3]) for (const y of [utc, uptime]) dot(root, column(i), y, 4, C.navy);

      // What the check adds up, against the 250 ms to spare.
      const bar = 232;
      const x = ms => pad + ms / 500 * (w - 2 * pad);
      label(root, 4, bar - 10, 'How much of the 250 ms it uses', strong);
      box(root, x(0), bar, x(500) - x(0), 16, {fill: C.pale});
      box(root, x(0), bar, x(DOUBT_MS + DRIFT_MS) - x(0), 16, {fill: C.muted});
      box(root, x(DOUBT_MS + DRIFT_MS), bar, x(Math.min(total, 500)) - x(DOUBT_MS + DRIFT_MS), 16, {fill: verdict});
      line(root, x(SPARE_MS), bar - 5, x(SPARE_MS), bar + 21, {stroke: C.warn, 'stroke-width': 2.5});
      label(root, x(SPARE_MS), bar + 37, '250 ms', {'text-anchor': 'middle', fill: C.warn, 'font-size': 13});
      label(root, x(0), bar + 37, '0', {'text-anchor': 'middle', fill: C.muted, 'font-size': 13});
      box(root, 4, bar + 49, 10, 10, {fill: C.muted});
      label(root, 19, bar + 59, 'uncertainty and drift', {'font-size': 13});
      box(root, 152, bar + 49, 10, 10, {fill: verdict});
      label(root, 167, bar + 59, 'disagreement', {'font-size': 13});

      const side = differs < 0 ? 'behind' : 'ahead of';
      slider.setAttribute('aria-valuetext', `${Math.abs(differs)} milliseconds ${side} the run’s clock`);
      $('tb-clock-gap-out').textContent = `${differs < 0 ? '−' : '+'}${Math.abs(differs)} ms`;
      const sum = 'At uptime 1 300 s the run’s clock says 21:45:00.000. The time server’s answer is ' +
        `${(Math.abs(differs) / 1000).toFixed(3)} s ${side} that. Its own uncertainty (${DOUBT_MS} ms) and the drift ` +
        `allowed until the window closes (${DRIFT_MS} ms) come on top: ${total} ms, `;
      if (stops) {
        say($('tb-clock-read'), sum, ['c-stop', 'more than the 250 ms to spare. The run stops.'],
          ' The window would still have opened at uptime 1 220 s: the schedule never moved.');
      } else {
        say($('tb-clock-read'), sum, ['c-ok', 'inside the 250 ms to spare. The run carries on'],
          ' with the pair it started with. The window still opens at uptime 1 220 s and closes at 1 949 s.');
      }
      return bar + 66;
    });
    slider.addEventListener('input', repaint);
  }

  // ── Figure: one run on one timeline, with two switches ───────────────────
  {
    // Seconds, counted from the window opening. Preparation starts a minute ahead (planner.rs: at least 60 s),
    // and in this example the dish needs 7 s to settle at the start.
    const FIRST = -600;
    const PREPARE = -60;
    const SETTLE = 7;
    const CLOSE = PASSES.metopc.seconds;
    const LAST = CLOSE + 120;
    // The time axis is stretched around the start, where the stages are short: [seconds, fraction of the width].
    const MARKS = [[FIRST, 0], [PREPARE, 0.2], [PREPARE + SETTLE, 0.36], [0, 0.52], [CLOSE, 0.87], [LAST, 1]];
    // The dashboard's name for each stage, and its fill in the drawing.
    const STAGES = {
      idle: ['Preview only', '#fff'],
      armed: ['Armed', '#dbe7f6'],
      preparing: ['Preparing', '#bcd3f5'],
      ready: ['Ready', '#cfe9ea'],
      following: ['Following', '#e6d3ee'],
      complete: ['Complete', '#d5eadf'],
      cancelled: ['Cancelled', '#f3e3cf'],
      stopped: ['Stopped', '#f2d6cf'],
    };
    const MOVING = ['preparing', 'ready', 'following'];
    const ENDINGS = {
      stopped: ' · Device network unavailable',
      cancelled: ' · Cancelled by controller',
      complete: ' · Pass finished',
    };
    const time = $('tb-life-time');
    const browser = $('tb-life-browser');
    const wifi = $('tb-life-wifi');
    const arm = $('tb-life-arm');
    const cancel = $('tb-life-cancel');
    const description = 'A timeline from ten minutes before the window opens to two minutes after it closes. Rows ' +
      'show when a browser holds control, when the Pico is on the network, which stage the run is in, and when ' +
      'the run may ask the motors to move.';
    let events = [];  // what the reader has done, in time order: [seconds, 'arm' | 'cancel' | 'browser' | 'wifi']

    const across = seconds => {
      const i = Math.max(1, MARKS.findIndex(([at]) => at >= seconds));
      const [t0, f0] = MARKS[i - 1];
      const [t1, f1] = MARKS[i];
      return f0 + (seconds - t0) / (t1 - t0) * (f1 - f0);
    };
    const secondsAt = fraction => {
      const i = Math.max(1, MARKS.findIndex(([, at]) => at >= fraction));
      const [t0, f0] = MARKS[i - 1];
      const [t1, f1] = MARKS[i];
      return t0 + (fraction - f0) / (f1 - f0) * (t1 - t0);
    };
    const now = () => Math.round(secondsAt(time.value / 1000));

    // Replay the events. Each result is a list of [from, state] changes along the timeline.
    const history = () => {
      const open = [[FIRST, true]];
      const online = [[FIRST, true]];
      const stages = [[FIRST, 'idle']];
      let armed = null;
      let ended = null;
      for (const [at, kind] of events) {
        if (kind === 'browser') open.push([at, !open.at(-1)[1]]);
        if (kind === 'arm') armed = at;
        if (kind === 'cancel') ended = [at, 'cancelled'];
        if (kind === 'wifi') {
          online.push([at, !online.at(-1)[1]]);
          // Losing the network ends a run that has been armed and has not finished. Nothing brings it back.
          const running = armed !== null && ended === null && at < CLOSE;
          if (!online.at(-1)[1] && running) ended = [at, 'stopped'];
        }
      }
      if (armed !== null) {
        const moving = Math.max(armed, PREPARE);
        stages.push([armed, 'armed'], [moving, 'preparing'], [moving + SETTLE, 'ready'], [0, 'following'], [CLOSE, 'complete']);
        if (ended) {
          while (stages.length > 1 && stages.at(-1)[0] >= ended[0]) stages.pop();
          stages.push(ended);
        }
      }
      return {open, online, stages, armed, ended};
    };
    const stateAt = (changes, seconds) => changes.filter(([from]) => from <= seconds).at(-1)[1];
    // [from, to, state] for each stretch that has any length.
    const spans = changes => changes
      .map(([from, state], i) => [from, i + 1 < changes.length ? changes[i + 1][0] : LAST, state])
      .filter(([from, to]) => to > from);
    // Acting at the time shown replaces anything that had been done after it.
    const act = kind => {
      const at = now();
      events = events.filter(([earlier]) => earlier <= at).concat([[at, kind]]);
      repaint();
    };

    const repaint = mount('tb-life-draw', description, (root, w) => {
      const {open, online, stages, armed, ended} = history();
      const at = now();
      const x = seconds => 6 + across(seconds) * (w - 12);
      const small = {'font-size': 13.5};
      // A browser holds control while it is open and the Pico can be reached.
      const control = spans(open).flatMap(([from, to, isOpen]) => !isOpen ? [] : spans(online)
        .filter(([f, t, up]) => up && t > from && f < to)
        .map(([f, t]) => [Math.max(f, from), Math.min(t, to), true]));
      const lane = (y, name, colour, parts) => {
        label(root, 6, y, name, {...small, fill: colour});
        line(root, 6, y + 12, w - 6, y + 12, {stroke: C.line, 'stroke-width': 1});
        for (const [from, to, on] of parts) {
          if (on) line(root, x(from), y + 12, x(to), y + 12, {stroke: colour, 'stroke-width': 7});
        }
      };
      lane(14, 'A browser holds control', C.navy, control);
      lane(52, 'The Pico is on the network', C.navy, spans(online));

      // The run. Names go beneath their stretch, on the first row where they do not collide.
      label(root, 6, 90, 'The run', small);
      const rowEnds = [];
      const names = spans(stages).map(([from, to, stage]) => {
        const [name, fill] = STAGES[stage];
        const stroke = stage === 'stopped' ? C.stop : C.navy;
        const middle = (x(from) + x(to)) / 2;
        const half = name.length * 3.8 + 4;  // roughly half the width of the name
        const centre = clamp(middle, 6 + half, w - 6 - half);
        let row = rowEnds.findIndex(end => centre - half > end);
        if (row < 0) row = rowEnds.length;
        rowEnds[row] = centre + half;
        box(root, x(from), 98, x(to) - x(from), 30, {
          fill, stroke, 'stroke-width': stage === 'stopped' ? 2 : 1, 'stroke-dasharray': stage === 'idle' ? '4 3' : 'none',
        });
        line(root, middle, 128, centre, 137 + row * 18, {stroke: C.muted, 'stroke-width': 1});
        return [centre, row, name, stroke, at >= from && at < to];
      });
      const below = 137 + rowEnds.length * 18;
      lane(below + 17, 'The run may ask the motors to move', C.az,
        spans(stages).map(([from, to, stage]) => [from, to, MOVING.includes(stage)]));

      const foot = below + 39;
      const moments = [[FIRST, '10 min before', 'start'], [0, 'window opens', 'middle'], [CLOSE, 'closes', 'middle']];
      for (const [seconds, name, anchor] of moments) {
        line(root, x(seconds), 8, x(seconds), foot, {stroke: C.sat, 'stroke-width': 1, 'stroke-dasharray': '2 4'});
        label(root, x(seconds), foot + 16, name, {'text-anchor': anchor, fill: C.sat, 'font-size': 13});
      }
      line(root, x(at), 4, x(at), foot, {'stroke-width': 2.5});
      path(root, `M${x(at)} ${foot}l-6 9h12z`, {fill: C.navy});
      // The names go on last, so that no line runs through their letters.
      for (const [centre, row, name, fill, current] of names) {
        label(root, centre, 149 + row * 18, name, {
          ...small, ...HALO, 'text-anchor': 'middle', fill, 'font-weight': current ? 700 : 400,
        });
      }

      // The controls follow the time shown.
      const stage = stateAt(stages, at);
      const isOpen = stateAt(open, at);
      const up = stateAt(online, at);
      const holds = isOpen && up;
      const tracking = ['armed', ...MOVING].includes(stage);
      browser.checked = isOpen;
      wifi.checked = up;
      arm.setAttribute('aria-disabled', !(stage === 'idle' && holds && armed === null && at + SETTLE < 0));
      cancel.setAttribute('aria-disabled', !(holds && armed !== null && ended === null && at >= armed && at < CLOSE));

      const when = at < 0 ? `${minutes(-at)} before the window opens`
        : at < CLOSE ? `${minutes(at)} into the window`
        : `${minutes(at - CLOSE)} after the window closes`;
      time.setAttribute('aria-valuetext', `${when}, ${STAGES[stage][0]}`);
      $('tb-life-time-out').textContent = when;
      // The dashboard's own status line, then what it means.
      const status = `${tracking ? 'Tracking' : 'Manual'} · ${stage}${holds ? ' · you have control' : ''}${ENDINGS[stage] || ''}.`;
      const meaning = {
        idle: ' A pass is selected and its preview is on the dashboard. Nothing moves, now or when the window ' +
          'opens, unless someone with control presses Enter Tracking.',
        armed: ' The Pico has the instruction and is waiting until a minute before the window opens. The plan ' +
          'is still redone whenever the orbit data, the time or the position of the dish changes.',
        preparing: ' The Pico has fixed its plan and is driving the dish to where the satellite will rise.',
        ready: ' Position readings have shown the dish settled at the start for 2 s. The follower still ' +
          'watches: if the dish is pushed off, it pulses a motor to put it back.',
        following: ' The window is open and the route is the target.',
        complete: ' The window has closed. The Pico has withdrawn its requests and left the dish where it ' +
          'stopped. It will not park it or start another pass.',
        cancelled: ' Someone holding control pressed Cancel tracking. The dish stays where it stopped.',
        stopped: up
          ? ' The Pico is back on the network, and the run stays stopped. A new run needs a new preview and a ' +
            'new press of Enter Tracking.'
          : ' The Pico lost its own network, so the run ended at once. Nobody could have reached it to cancel.',
      }[stage];
      const absent = holds ? ''
        : !up ? ' No browser can reach the Pico.'
        : tracking ? ' No browser is connected, and the run does not need one.'
        : ' No browser is connected.';
      say($('tb-life-read'), ['b', status], meaning, absent);
      return foot + 24;
    });

    time.addEventListener('input', repaint);
    browser.addEventListener('change', () => act('browser'));
    wifi.addEventListener('change', () => act('wifi'));
    // These stay focusable when they cannot be used, so they are switched off with aria-disabled.
    for (const [button, kind] of [[arm, 'arm'], [cancel, 'cancel']]) {
      button.addEventListener('click', () => {
        if (button.getAttribute('aria-disabled') !== 'true') act(kind);
      });
    }
    $('tb-life-reset').addEventListener('click', () => {
      events = [];
      time.value = 0;
      repaint();
    });
  }

  // ── Figure: one budget for catching up ───────────────────────────────────
  {
    // deviation.rs REACQUISITION_LIMIT_MS; lifecycle.rs enter_reacquisition cuts the deadline at the window's end.
    const BUDGET = 30;
    const left = $('tb-recover-left');
    const took = $('tb-recover-took');
    const description = 'A strip of 48 seconds that starts when recovery begins. It shows the rest of the window, ' +
      'the time allowed for recovery, which is at most 30 seconds, and how long the dish takes to catch up.';

    const repaint = mount('tb-recover-draw', description, (root, w) => {
      const closes = Number(left.value);
      const needs = Number(took.value);
      const deadline = Math.min(BUDGET, closes);
      const back = needs < deadline;
      const x = seconds => 8 + (seconds + 3) / 51 * (w - 16);
      const row = (y, name, colour, from, to, attrs) => {
        line(root, x(from), y + 13, x(to), y + 13, {stroke: colour, 'stroke-width': 8, ...attrs});
        return () => label(root, 8, y, name, {...HALO, fill: colour, 'font-size': 13.5});
      };
      const names = [
        row(14, 'The window', C.sat, -3, closes, {opacity: 0.4}),
        row(54, deadline < BUDGET ? 'Time allowed: cut short as the window closes' : 'Time allowed: 30 s', C.warn, 0, deadline),
        row(94, back ? 'Catching up' : 'Still catching up when time runs out', back ? C.ok : C.stop, 0, Math.min(needs, deadline)),
      ];
      line(root, x(deadline), 46, x(deadline), 118, {stroke: C.warn, 'stroke-width': 2.5});
      dot(root, x(Math.min(needs, deadline)), 107, 6, back ? C.ok : C.stop);
      names.forEach(write => write());  // names last, so the deadline does not cross them out
      line(root, x(-3), 124, x(48), 124, {'stroke-width': 1});
      for (let seconds = 0; seconds <= 40; seconds += 10) {
        line(root, x(seconds), 124, x(seconds), 130, {'stroke-width': 1});
        label(root, x(seconds), 146, seconds === 40 ? '40 s' : seconds, {'text-anchor': 'middle', fill: C.muted, 'font-size': 13});
      }

      left.setAttribute('aria-valuetext', `${closes} seconds`);
      took.setAttribute('aria-valuetext', `${needs} seconds`);
      $('tb-recover-left-out').textContent = `${closes} s`;
      $('tb-recover-took-out').textContent = `${needs} s`;
      if (back) {
        say($('tb-recover-read'), ['c-ok', `Back within 2° after ${needs} s.`],
          ' The stage returns to following, and the rest of the window is followed as planned.');
      } else {
        const clipped = deadline < BUDGET;
        say($('tb-recover-read'), ['c-stop', clipped ? `The window closes ${closes} s in.` : 'Thirty seconds pass.'],
          ' The run stops, and the dashboard shows ', ['b', clipped ? 'Pass ended during recovery' : 'Recovery deadline reached'],
          '. Trying again inside those seconds is allowed; a fresh allowance is not.');
      }
      return 152;
    });
    left.addEventListener('input', repaint);
    took.addEventListener('input', repaint);
  }

  // ── Figure: the whole path, with one link broken ─────────────────────────
  {
    // What each break does: where the cross goes, the longest the pin can stay high afterwards (ms), and how
    // the pin, the switch and the motor end up. The times are MOVEMENT_LEASE_MS, NETWORK_HEALTH_TIMEOUT_MS,
    // FEEDBACK_MAX_AGE_MS, WATCHDOG_TIMEOUT and MotionConfig::DEFAULT.progress_timeout_ms.
    const BREAKS = {
      none: {after: 0, pin: 'high', closed: true, turning: true},
      quiet: {cut: 'write', after: 750, pin: 'low', closed: false, turning: false},
      network: {cut: 'network', after: 300, pin: 'low', closed: false, turning: false},
      reading: {cut: 'sense', after: 500, pin: 'low', closed: false, turning: false},
      hang: {cut: 'task', after: 500, pin: '?', closed: false, turning: false},
      stall: {cut: 'power', after: 5000, pin: 'low', closed: false, turning: false},
      stuck: {cut: 'switch', after: Infinity, pin: 'low', closed: true, turning: true},
    };
    const PIN_FILL = {high: '#d5eadf', low: '#fff', '?': '#f3e3cf'};
    const chosen = name => document.querySelector(`input[name=${name}]:checked`).value;
    const description = 'The path of one request: its writer, the note in the mailbox, the direction task, pin ' +
      'GP10, the transistor switch, the motor and dish, and the position reading that returns to the direction ' +
      'task. A cross marks the broken link.';

    // What the readout says for each break. `run` is true when a tracking run writes the note.
    const explain = (name, run) => ({
      none: [
        run
          ? 'A tracking run writes the note itself, every 10 ms, from the route and the newest position reading. ' +
            'Each note is good for 750 ms. No browser is in this loop.'
          : 'A held clockwise button. The browser repeats the request every 150 ms, and each repeat replaces the ' +
            'note and its deadline.',
        ' The direction task reads the note beside the newest reading and the network permission, and holds ',
        ['b', 'GP10 high'], '. The switch closes, the G-5500 runs its motor, and the next reading reports the new angle.',
      ],
      quiet: run ? [
        'A tracking run cannot go quiet the way a browser can: the follower takes its turn inside the same loop ' +
        'that sets the pins. Two rules still stand behind it. A turn that comes more than 20 ms after the one ' +
        'before stops the run. And a note that nobody replaces runs out ', ['b', '750 ms'],
        ' after it was written, like any other.',
      ] : [
        'The browser goes quiet: a frozen tab, a flat battery, a dropped connection. No stop message arrives, and ' +
        'none is needed. The last note carries its own deadline, at most ', ['b', '750 ms'],
        ' after it was written, and the direction task drops the pin when it passes (chapter 3). A connection ' +
        'that closes cleanly is quicker: the Pico knows at once.',
      ],
      network: [
        'The Pico loses its own Wi-Fi link or its address. A good check of the link is trusted for less than ',
        ['b', '300 ms'], '. When it lapses the direction task drops the pin, whatever the note says (chapter 3).',
        run ? ' The tracking run stops, and does not resume when the network returns.'
          : ' Control is taken from the browser, too.',
      ],
      reading: [
        'The position readings stop: a broken wire, an ADC that no longer answers. The newest reading keeps ' +
        'ageing, and once it is more than ', ['b', '500 ms'], ' old no pin may stay high (chapter 2).',
        run ? ' The tracking run stops.' : ' Moving again takes a fresh reading and a fresh press.',
      ],
      hang: [
        'The processor stops running the direction task. No rule on this page can act, because nothing is ' +
        'checking them. The fallback is the watchdog of chapter 1: the direction task feeds it only after ' +
        'setting the pins, so ', ['b', '500 ms'], ' after the last feed it resets the chip. Whether GP10 falls ' +
        'at that instant, or only when the restarted program drives it low, has not been measured.',
      ],
      stall: [
        'The pin is high but the reading does not change: a jammed gear, a motor without power, or a position ' +
        'wire stuck at one voltage. The Pico cannot tell which. After ', ['b', '5 s'], ' of running with less ' +
        'than three counts of movement it drops every pin and refuses to move again until the board is reset.',
        run ? ' The tracking run stops.' : '',
      ],
      stuck: [
        run ? 'The follower stops asking' : 'You let go of the button',
        ', and GP10 goes low as it should. But the switch has failed closed, or a wire has shorted to ground, ' +
        'and the motor runs on. ', ['c-stop', 'Nothing in the firmware can open a switch it does not control'],
        ', and the board has no second cut-off. The stop is a person at the G-5500’s power switch. This is ' +
        'where the evidence in the software ends.',
      ],
    }[name]);

    const repaint = mount('tb-path-draw', description, (root, w) => {
      const run = chosen('tb-path-source') === 'run';
      const name = chosen('tb-path-break');
      const fault = BREAKS[name];
      const bad = C.stop;
      // Six pictures: in a row on a wide page with the reading returning beneath, in a column on a phone
      // with the reading returning up the right-hand side.
      const wide = w >= 700;
      const gap = wide ? (w - 110) / 5 : 64;
      const spot = i => wide ? [55 + i * gap, 86] : [w / 2 - 6, 34 + i * gap];
      const [writerX, writerY] = spot(0);
      const [noteX, noteY] = spot(1);
      const [taskX, taskY] = spot(2);
      const [pinX, pinY] = spot(3);
      const [switchX, switchY] = spot(4);
      const [motorX, motorY] = spot(5);
      const back = wide ? [(taskX + motorX) / 2, 150] : [w - 22, (taskY + motorY) / 2];

      const cross = (x, y) => {
        line(root, x - 7, y - 7, x + 7, y + 7, {stroke: bad, 'stroke-width': 3.5});
        line(root, x - 7, y + 7, x + 7, y - 7, {stroke: bad, 'stroke-width': 3.5});
      };
      // A link stays solid while something still flows along it once the break has played out.
      const flows = {
        // A tracking run that is stopped or hung writes no more notes; a browser can go on asking in vain.
        write: !['quiet', 'network', 'stuck'].includes(name) && !(run && ['reading', 'stall', 'hang'].includes(name)),
        read: name !== 'hang',
        drive: fault.pin === 'high',
        close: fault.pin === 'high',
        power: fault.closed,
        sense: name !== 'reading' && name !== 'hang',
      };
      const link = (kind, points) => {
        const colour = flows[kind] ? C.navy : C.line;
        draw('polyline', {
          points: points.map(point => point.join(' ')).join(' '),
          fill: 'none', stroke: colour, 'stroke-width': 2, 'stroke-dasharray': flows[kind] ? 'none' : '4 4',
        }, root);
        const [[ax, ay], [bx, by]] = points.slice(-2);
        const far = Math.hypot(bx - ax, by - ay);
        const [ux, uy] = [(bx - ax) / far, (by - ay) / far];
        // An arrowhead on the last stretch.
        const barb = side => `${bx - ux * 9 + side * uy * 5} ${by - uy * 9 - side * ux * 5}`;
        path(root, `M${bx} ${by}L${barb(-1)}L${barb(1)}z`, {fill: colour});
        if (fault.cut === kind) cross((points[0][0] + points[1][0]) / 2, (points[0][1] + points[1][1]) / 2);
      };
      ['write', 'read', 'drive', 'close', 'power'].forEach((kind, i) => {
        const [ax, ay] = spot(i);
        const [bx, by] = spot(i + 1);
        link(kind, wide ? [[ax + 24, ay], [bx - 24, by]] : [[ax, ay + 20], [bx, by - 20]]);
      });
      link('sense', wide
        ? [[motorX, motorY + 22], [motorX, back[1]], [taskX, back[1]], [taskX, taskY + 24]]
        : [[motorX + 22, motorY], [back[0], motorY], [back[0], taskY], [taskX + 26, taskY]]);
      dot(root, back[0], back[1], 5, flows.sense ? C.navy : C.line);

      // The writer: a held direction button, or the satellite that the follower chases.
      if (run) {
        const arc = `M${writerX - 18} ${writerY + 10}Q${writerX} ${writerY - 26} ${writerX + 18} ${writerY + 10}`;
        path(root, arc, {stroke: C.sat, 'stroke-width': 2});
        dot(root, writerX + 6, writerY - 7, 5, C.sat);
      } else {
        box(root, writerX - 18, writerY - 13, 36, 26, {rx: 5, fill: '#fff', stroke: C.navy, 'stroke-width': 1.5});
        label(root, writerX, writerY + 5, 'CW', {'text-anchor': 'middle', 'font-size': 13, 'font-weight': 700});
      }

      // The note: an envelope.
      box(root, noteX - 18, noteY - 12, 36, 24, {fill: '#fff', stroke: C.navy, 'stroke-width': 1.5});
      const flap = `M${noteX - 18} ${noteY - 12}L${noteX} ${noteY + 2}L${noteX + 18} ${noteY - 12}`;
      path(root, flap, {stroke: C.navy, 'stroke-width': 1.5});

      // The direction task: a chip. Beside it, whether the Pico is on the network.
      const hung = name === 'hang';
      const offline = name === 'network';
      const chip = hung ? bad : C.navy;
      box(root, taskX - 16, taskY - 16, 32, 32, {fill: hung ? '#f2d6cf' : C.pale, stroke: chip, 'stroke-width': 1.5});
      for (const side of [-1, 1]) {
        for (const leg of [-9, 0, 9]) {
          line(root, taskX + side * 16, taskY + leg, taskX + side * 21, taskY + leg, {stroke: chip});
        }
      }
      if (hung) cross(taskX, taskY);
      const [netX, netY] = wide ? [taskX - 54, taskY + 44] : [taskX + 46, taskY - 26];
      const netColour = offline ? C.line : C.navy;
      for (const r of [5, 11, 17]) {
        const [dx, y] = [r * 0.7, netY + 8 - r * 0.7];
        path(root, `M${netX - dx} ${y}A${r} ${r} 0 0 1 ${netX + dx} ${y}`, {stroke: netColour, 'stroke-width': 2});
      }
      dot(root, netX, netY + 8, 2.5, netColour);
      line(root, netX + (wide ? 16 : -14), netY + (wide ? -4 : 10), taskX + (wide ? -19 : 19), taskY + (wide ? 15 : -13), {
        stroke: netColour, 'stroke-dasharray': offline ? '4 4' : 'none',
      });
      label(root, wide ? netX : netX + 22, wide ? netY + 28 : netY + 9, 'network', {
        'text-anchor': wide ? 'middle' : 'start', 'font-size': 13.5,
      });
      if (offline) cross(netX, netY);

      // The pin, with its level.
      dot(root, pinX, pinY, 17, PIN_FILL[fault.pin], {stroke: C.navy, 'stroke-width': 1.5});
      label(root, pinX, pinY + 5, fault.pin, {'text-anchor': 'middle', 'font-size': 13, 'font-weight': 700});

      // The switch: two contacts and a blade.
      const stuck = name === 'stuck';
      const contact = stuck ? bad : C.navy;
      for (const side of [-1, 1]) dot(root, switchX + side * 13, switchY, 3.5, contact);
      line(root, switchX - 13, switchY, switchX + (fault.closed ? 13 : 9), switchY - (fault.closed ? 0 : 13), {
        stroke: contact, 'stroke-width': 3, 'stroke-linecap': 'round',
      });
      if (stuck) cross(wide ? switchX : switchX + 32, wide ? switchY + 22 : switchY);

      // The motor, turning or still.
      const motorFill = !fault.turning ? '#fff' : stuck ? '#f2d6cf' : '#d5eadf';
      dot(root, motorX, motorY, 17, motorFill, {stroke: stuck ? bad : C.navy, 'stroke-width': 1.5});
      label(root, motorX, motorY + 5, 'M', {'text-anchor': 'middle', 'font-weight': 700});
      if (fault.turning) {
        const turning = `M${motorX - 22} ${motorY - 8}A24 24 0 0 1 ${motorX + 14} ${motorY - 20}l-7 -3m7 3l-4 6`;
        path(root, turning, {stroke: stuck ? bad : C.ok, 'stroke-width': 2});
      }

      // Names: above each picture in the row, to the left of each in the column.
      const names = [
        [run ? 'tracking run' : 'held button'], ['note with', 'a deadline'], ['direction', 'task'],
        ['pin GP10'], ['switch'], ['motor', 'and dish'],
      ];
      names.forEach((rows, i) => rows.forEach((text, n) => {
        const [x, y] = spot(i);
        const dy = wide ? -30 - (rows.length - 1 - n) * 17 : 5 + (n - (rows.length - 1) / 2) * 17;
        label(root, wide ? x : x - 32, y + dy, text, {'text-anchor': wide ? 'middle' : 'end', 'font-size': 13.5});
      }));
      ['position', 'reading'].forEach((text, n) => {
        const x = wide ? back[0] + (n ? 30 : -30) : back[0] - 10;
        const y = wide ? back[1] + 22 : back[1] - 3 + n * 17;
        label(root, x, y, text, {'text-anchor': wide ? 'middle' : 'end', 'font-size': 13.5});
      });

      // The longest the pin can stay high after the break, on a scale of one second.
      const top = wide ? 204 : motorY + 58;
      const x = ms => 12 + ms / 1000 * (w - 24);
      const beyond = fault.after > 1000;
      const title = name === 'none' ? 'Nothing broken' : 'Longest the pin can stay high';
      label(root, 12, top, title, {'font-size': 13.5, 'font-weight': 700});
      box(root, x(0), top + 10, x(1000) - x(0), 16, {fill: C.pale});
      if (name !== 'none') box(root, x(0), top + 10, x(Math.min(fault.after, 1000)) - x(0), 16, {fill: beyond ? bad : C.warn});
      for (const ms of [0, 300, 500, 750, 1000]) {
        const anchor = ms === 0 ? 'start' : ms === 1000 ? 'end' : 'middle';
        line(root, x(ms), top + 26, x(ms), top + 31, {'stroke-width': 1});
        const tick = ms === 0 ? '0 ms' : ms === 1000 ? '1 s' : ms;
        label(root, x(ms), top + 46, tick, {'text-anchor': anchor, fill: C.muted, 'font-size': 13});
      }
      if (beyond) {
        const limit = fault.after === Infinity ? 'no limit in software' : '5 s of running';
        label(root, x(1000) - 6, top + 23, limit, {'text-anchor': 'end', fill: '#fff', 'font-size': 13, 'font-weight': 700});
      }
      if (name === 'hang') label(root, x(500) + 6, top + 23, '+ restart, unmeasured', {fill: C.warn, 'font-size': 13});

      say($('tb-path-read'), ...explain(name, run));
      return top + 52;
    });
    for (const input of document.querySelectorAll('input[name=tb-path-source],input[name=tb-path-break]')) {
      input.addEventListener('change', repaint);
    }
  }
})();
