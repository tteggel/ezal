// The opening figure: hold a direction and the dish turns. It mirrors the
// wiring and the installed calibration, and none of ezal's checks.
(() => {
  'use strict';
  const el = id => document.getElementById(id);
  if (!el('hero')) return;
  // °/s: the G-5500's brochure pace on 50 Hz mains (docs/HARDWARE.md).
  const RATE = {az: 5.14, el: 2.25};
  const TRAVEL = {az: 360, el: 90};
  // mV at each end of travel: HARD_CODED_CALIBRATION in ezal-core's position.rs.
  const MILLIVOLTS = {az: [114, 2033], el: [1281, 58]};
  const BUTTONS = {cw: ['az', 1], ccw: ['az', -1], up: ['el', 1], down: ['el', -1]};
  const angle = {az: 180, el: 30};
  const held = {az: null, el: null};
  let last = null;

  // The sensor reports in 2 mV steps, so the reading moves in small jumps.
  const millivolts = axis => {
    const [low, high] = MILLIVOLTS[axis];
    return 2 * Math.round((low + (high - low) * angle[axis] / TRAVEL[axis]) / 2);
  };

  function draw() {
    for (const [name, [axis]] of Object.entries(BUTTONS)) {
      const on = held[axis] === name;
      el(`hero-led-${name}`).setAttribute('fill', on ? (axis === 'az' ? '#245bce' : '#087e83') : '#fff');
      el('hero').querySelector(`[data-hero=${name}]`).classList.toggle('is-held', on);
    }
    el('hero-az-needle').setAttribute('transform', `rotate(${angle.az / 3 - 60} 56 100)`);
    el('hero-el-needle').setAttribute('transform', `rotate(${angle.el * 4 / 3 - 60} 130 100)`);
    el('hero-az-pointer').setAttribute('transform', `rotate(${angle.az} 60 76)`);
    el('hero-el-pointer').setAttribute('transform', `rotate(${-angle.el} 128 120)`);
    el('hero-az-deg').textContent = `${angle.az.toFixed(1)}°`;
    el('hero-el-deg').textContent = `${angle.el.toFixed(1)}°`;
    el('hero-az-mv').textContent = `${millivolts('az')} mV`;
    el('hero-el-mv').textContent = `${millivolts('el')} mV`;
    el('hero-power').textContent = held.az && held.el ? 'Both motors on' : held.az ? 'Azimuth motor on' : held.el ? 'Elevation motor on' : 'Both motors off';
  }

  function frame(now) {
    const seconds = Math.min(now - (last ?? now), 100) / 1000;
    last = now;
    for (const axis of ['az', 'el']) {
      if (held[axis]) angle[axis] = Math.min(TRAVEL[axis], Math.max(0, angle[axis] + BUTTONS[held[axis]][1] * RATE[axis] * seconds));
    }
    draw();
    if (held.az || held.el) requestAnimationFrame(frame);
    else last = null;
  }

  function press(name) {
    const [axis] = BUTTONS[name];
    if (held[axis]) return; // one direction per motor, as in the firmware
    const idle = !held.az && !held.el;
    held[axis] = name;
    if (idle) requestAnimationFrame(frame);
  }

  function release(name) {
    const [axis] = BUTTONS[name];
    if (held[axis] !== name) return;
    held[axis] = null;
    draw();
    el('hero-status').textContent = `Stopped at azimuth ${angle.az.toFixed(1)} degrees, ${millivolts('az')} millivolts; elevation ${angle.el.toFixed(1)} degrees, ${millivolts('el')} millivolts.`;
  }

  for (const button of el('hero').querySelectorAll('[data-hero]')) {
    const name = button.dataset.hero;
    button.addEventListener('pointerdown', event => { button.setPointerCapture(event.pointerId); press(name); });
    for (const type of ['pointerup', 'pointercancel', 'lostpointercapture', 'blur']) button.addEventListener(type, () => release(name));
    button.addEventListener('keydown', event => { if ((event.key === ' ' || event.key === 'Enter') && !event.repeat) press(name); });
    button.addEventListener('keyup', event => { if (event.key === ' ' || event.key === 'Enter') release(name); });
    button.addEventListener('contextmenu', event => event.preventDefault());
  }
  draw();
})();
