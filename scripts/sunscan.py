#!/usr/bin/env python3
"""Automated sun-scan pointing calibration for ezal.

Runs on the decoder Pi. It streams the RFOne's noise power with hydrasdr_rx,
drives the rotator through the controller's dashboard WebSocket, and sweeps the
dish across the sun four times: azimuth, elevation, then both again in the
opposite direction, each sweep through the peak the previous one found. Every
sweep is fitted with a Gaussian beam on a sloping baseline, against the sun's
position at the moment of each reading. The result is where the beam really
points relative to the position feedback, plus the beamwidth and the sun's
rise over the sky.

The sweeps cross the sun without stopping on it: a dish can focus sunlight on
the feed. The coax stays connected; hydrasdr_rx switches the bias-tee on.

  python3 sunscan.py [--site LAT LON]          check only: connect, report, no motion
  python3 sunscan.py --go [--site LAT LON]     run the scan

Needs the controller in Manual mode with nobody else holding control (close
the dashboard's control first), healthy feedback, the startup rest cleared and
enough motor duty; the RFOne free (no SatDump running); and the sun at least
--min-sun-el degrees up. If the script stops for any reason it sends stop, and
the controller releases the motors by itself within 750 ms once holds cease.
Results, raw samples and a plot go to --out.
"""
import argparse
import base64
import datetime as dt
import json
import math
import os
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time

import numpy as np

from rftest import site_from_env, sun_azel

RATE = {"az": 6.2, "el": 3.1}       # installed steady rates, deg/s (installation.rs)
LEAD = {"az": 2.0, "el": 1.0}       # release this far short: telemetry age plus coast
TOL = {"az": 3.0, "el": 1.5}        # positioning tolerance; a 500 ms pulse moves ~3 / ~1.5 deg
REFRESH_S = 0.15                    # hold refresh (session::COMMAND_REFRESH_MS)
TOKEN_MAX_AGE_S = 0.9               # the controller rejects tokens older than 1 s
TELEMETRY_STALE_S = 1.5
SR = 10_000_000
BLOCK = 1 << 19                     # samples per power reading, ~52 ms at 10 MS/s
NFFT = 4096                        # average every spectrum in the block


class Abort(Exception):
    pass


# ------------------------------------------------------------------ sun

def sun_apparent(lat, lon, t):
    """Sun azimuth and apparent elevation (with refraction) at unix time t."""
    az, el = sun_azel(lat, lon, dt.datetime.fromtimestamp(t, dt.timezone.utc))
    if el > -1:
        el += 1.02 / math.tan(math.radians(el + 10.3 / (el + 5.11))) / 60
    return az, el


def wrap180(a):
    return (a + 180.0) % 360.0 - 180.0


# ------------------------------------------------------------ websocket

class WebSocket:
    """Minimal RFC 6455 client: masked text frames out, pongs for pings."""

    def __init__(self, host, port=80, path="/ws"):
        self.sock = socket.create_connection((host, port), timeout=5)
        self.sock.settimeout(3)
        self.rf = self.sock.makefile("rb")
        self.lock = threading.Lock()
        key = base64.b64encode(os.urandom(16)).decode()
        self.sock.sendall((f"GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\n"
                           f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
                           "Sec-WebSocket-Version: 13\r\n\r\n").encode())
        status = self.rf.readline()
        if b" 101 " not in status:
            raise Abort(f"WebSocket handshake failed: {status!r}")
        while self.rf.readline() not in (b"\r\n", b""):
            pass

    def _frame(self, opcode, payload):
        n = len(payload)
        head = bytes([0x80 | opcode])
        if n < 126:
            head += bytes([0x80 | n])
        elif n < 1 << 16:
            head += bytes([0x80 | 126]) + n.to_bytes(2, "big")
        else:
            head += bytes([0x80 | 127]) + n.to_bytes(8, "big")
        mask = os.urandom(4)
        body = bytes(b ^ mask[i & 3] for i, b in enumerate(payload))
        with self.lock:
            self.sock.sendall(head + mask + body)

    def send(self, text):
        self._frame(0x1, text.encode())

    def _read(self, n):
        data = self.rf.read(n)
        if data is None or len(data) < n:
            raise ConnectionError("controller closed the connection")
        return data

    def recv(self):
        """Next text message; answers pings, raises on close."""
        parts = []
        while True:
            b0, b1 = self._read(2)
            n = b1 & 0x7F
            if n == 126:
                n = int.from_bytes(self._read(2), "big")
            elif n == 127:
                n = int.from_bytes(self._read(8), "big")
            mask = self._read(4) if b1 & 0x80 else None
            payload = self._read(n)
            if mask:
                payload = bytes(b ^ mask[i & 3] for i, b in enumerate(payload))
            opcode = b0 & 0x0F
            if opcode == 0x9:                       # ping: the operator lease renews on pong
                self._frame(0xA, payload)
            elif opcode == 0x8:
                raise ConnectionError("controller closed the connection")
            elif opcode in (0x1, 0x0):
                parts.append(payload)
                if b0 & 0x80:
                    return b"".join(parts).decode()

    def close(self):
        try:
            self._frame(0x8, b"")
            self.sock.close()
        except OSError:
            pass


# ----------------------------------------------------------- controller

class Controller:
    """Telemetry, control ownership and held manual drive over the dashboard socket."""

    def __init__(self, address):
        host, _, port = address.partition(":")
        self.ws = WebSocket(host, int(port or 80))
        self.cond = threading.Condition()
        self.tel = None
        self.tel_at = 0.0
        self.token = None
        self.token_at = 0.0
        self.positions = []            # (unix time of the feedback sample, az, el)
        self.error = None
        self.want = ()                 # directions currently held, e.g. ("cw", "up")
        self.sequence = 0
        self.need_press = False
        threading.Thread(target=self._reader, daemon=True).start()
        threading.Thread(target=self._refresher, daemon=True).start()

    def _reader(self):
        try:
            while True:
                msg = json.loads(self.ws.recv())
                now = time.time()
                with self.cond:
                    if msg.get("type") == "telemetry":
                        self.tel, self.tel_at = msg, now
                        token = (msg.get("freshness") or {}).get("token")
                        if token != self.token:
                            self.token, self.token_at = token, now
                        fb, age = msg["feedback"], msg["tracking"].get("feedback_age_ms")
                        if fb.get("azimuth_tenths") is not None and age is not None:
                            self.positions.append((now - age / 1000, fb["azimuth_tenths"] / 10,
                                                   fb["elevation_tenths"] / 10))
                    elif msg.get("type") == "input_required":
                        # The controller dropped the held intent; a new press
                        # needs a newer telemetry token.
                        self.need_press, self.want = True, ()
                    self.cond.notify_all()
        except (OSError, ConnectionError, ValueError) as e:
            with self.cond:
                self.error = f"controller connection lost: {e}"
                self.cond.notify_all()

    def _send_drive(self, pressed):
        self.sequence += 1
        kind = "press" if pressed else "hold"
        self.ws.send(f"{kind}:{self.token}:{self.sequence}:drive:{'+'.join(self.want)}")

    def _refresher(self):
        while True:
            time.sleep(REFRESH_S)
            with self.cond:
                if self.want and self.error is None:
                    if time.time() - self.token_at > TOKEN_MAX_AGE_S:
                        self.want = ()
                        self.error = "telemetry token went stale while driving"
                        self._stop_locked()
                    else:
                        self._send_drive(False)

    def _stop_locked(self):
        try:
            self.ws.send("stop")
        except OSError:
            pass

    def state(self):
        """Latest telemetry, checked for freshness and for anything that forbids motion."""
        with self.cond:
            if self.error:
                raise Abort(self.error)
            if self.tel is None or time.time() - self.tel_at > TELEMETRY_STALE_S:
                raise Abort("telemetry is stale")
            return self.tel

    def wait_telemetry(self, timeout=5.0):
        end = time.time() + timeout
        with self.cond:
            while self.tel is None and self.error is None and time.time() < end:
                self.cond.wait(0.2)
        return self.state()

    def take_control(self):
        if self.state()["control"]["controller"]:
            return
        self.ws.send("control:take")
        end = time.time() + 6
        while time.time() < end:
            time.sleep(0.25)
            if self.state()["control"]["controller"]:
                return
        raise Abort("could not take control: release it in the dashboard (another viewer holds it)")

    def drive(self, directions):
        """Hold exactly these directions (empty = stop) until changed.

        Adding a direction is a press; releasing one while another stays held
        is a hold, as the dashboard sends."""
        directions = tuple(directions)
        with self.cond:
            if directions == self.want and not self.need_press:
                return
            if not directions:
                self.want = ()
                self._stop_locked()
                return
            pressed = self.need_press or bool(set(directions) - set(self.want))
            if self.need_press:
                # Wait for a telemetry batch newer than the rejected one.
                stale = self.token
                end = time.time() + 2
                while self.token == stale and time.time() < end and self.error is None:
                    self.cond.wait(0.1)
                self.need_press = False
            if time.time() - self.token_at > TOKEN_MAX_AGE_S:
                raise Abort("no fresh telemetry token for a press")
            self.want = directions
            self._send_drive(pressed)

    def stop(self):
        self.drive(())

    def close(self):
        with self.cond:
            self.want = ()
            self._stop_locked()
        time.sleep(0.2)
        self.ws.close()


def check_motion_allowed(tel):
    c, f, t = tel["control"], tel["feedback"], tel["tracking"]
    if t["mode"] != "manual":
        raise Abort(f"controller is in {t['mode']} mode ({t['phase']}): cancel or return to Manual first")
    if not c["network_ready"]:
        raise Abort("controller network not ready")
    if c["motion_fault"]:
        raise Abort(f"motion fault: {c['motion_fault']}")
    if f["health"] != "healthy" or f["azimuth_tenths"] is None:
        raise Abort(f"position feedback is {f['health']}")
    if c["duty_startup_rest"]:
        raise Abort("startup rest is pending: skip it in the dashboard or wait it out")


# -------------------------------------------------------------- radio

class Radio:
    """hydrasdr_rx streaming INT16 IQ to stdout; one robust power reading per block."""

    def __init__(self, freq_mhz, gain):
        cmd = ["hydrasdr_rx", "-r", "-", "-f", f"{freq_mhz}", "-a", str(SR), "-b", "1", "-i", str(gain)]
        # A file cannot fill a pipe and stall the producer while IQ is read.
        self.stderr = tempfile.TemporaryFile()
        try:
            self.proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=self.stderr, bufsize=0)
        except BaseException:
            self.stderr.close()
            raise
        self.readings = []             # (unix time at the block's middle, power dB, clipped fraction)
        self.error = None
        self.lock = threading.Lock()
        self.win = np.hanning(NFFT).astype(np.float32)
        self.reader = threading.Thread(target=self._reader, daemon=True)
        self.reader.start()

    def _reader(self):
        f = self.proc.stdout
        need = 4 * BLOCK
        while True:
            buf = bytearray()
            while len(buf) < need:
                chunk = f.read(need - len(buf))
                if not chunk:
                    end = self.stderr.seek(0, os.SEEK_END)
                    self.stderr.seek(max(0, end - 4096))
                    err = self.stderr.read().decode(errors="replace").strip().splitlines()
                    with self.lock:
                        self.error = f"hydrasdr_rx stopped: {err[-1] if err else 'no output'}"
                    return
                buf += chunk
            t = time.time() - BLOCK / SR / 2
            x = np.frombuffer(buf, dtype=np.int16).astype(np.float32) / 32768
            z = (x[0::2] + 1j * x[1::2]).reshape(-1, NFFT) * self.win
            psd = np.mean(np.abs(np.fft.fft(z, axis=1)) ** 2, axis=0)
            c = NFFT // 10              # central 80 %: off the roll-off; median ignores spurs
            band = np.fft.fftshift(psd)[c:-c]
            clipped = float(np.mean(np.abs(x) > 0.99))
            with self.lock:
                self.readings.append((t, 10 * np.log10(np.median(band)), clipped))

    def since(self, t0, t1):
        with self.lock:
            if self.error:
                raise Abort(self.error)
            return [r for r in self.readings if t0 <= r[0] <= t1]

    def wait_ready(self, timeout=8.0):
        end = time.time() + timeout
        while time.time() < end:
            with self.lock:
                if self.error:
                    raise Abort(self.error)
                if len(self.readings) > 20:   # past the start-up transient
                    return
            time.sleep(0.2)
        raise Abort("no samples from hydrasdr_rx: is the RFOne connected and free?")

    def close(self):
        self.proc.terminate()
        try:
            self.proc.wait(3)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
        self.reader.join()
        self.proc.stdout.close()
        self.stderr.close()


# -------------------------------------------------------------- motion

def position(ctl):
    tel = ctl.state()
    check_motion_allowed(tel)
    f = tel["feedback"]
    return f["azimuth_tenths"] / 10, f["elevation_tenths"] / 10


def duty_ok(ctl, axis, seconds):
    remaining = ctl.state()["control"]["duty_remaining_ms"][0 if axis == "az" else 1] / 1000
    if remaining < seconds + 3:
        raise Abort(f"{axis} motor duty: {remaining:.0f} s left, this move needs ~{seconds:.0f} s; "
                    "it refills after 15 min idle")


DIRECTION = {("az", 1): "cw", ("az", -1): "ccw", ("el", 1): "up", ("el", -1): "down"}


def move(ctl, target, stop_short=True):
    """Drive both axes toward target (az, el), releasing each once and never reversing.

    With stop_short, an axis more than TOL away moves and is released LEAD
    early so its coast lands it near the target; without, it runs until it
    reaches the target (used for sweeps). goto() corrects any remaining error."""
    here = dict(zip(("az", "el"), position(ctl)))
    goal = dict(zip(("az", "el"), target))
    sense = {}
    for axis in ("az", "el"):
        err = goal[axis] - here[axis]
        if abs(err) > (TOL[axis] if stop_short else 0.3):
            sense[axis] = 1 if err > 0 else -1
            duty_ok(ctl, axis, abs(err) / RATE[axis])
    deadline = time.time() + max([abs(goal[k] - here[k]) / RATE[k] for k in sense] + [0]) * 2 + 8
    last_change, last_pos = time.time(), dict(here)
    while sense:
        here = dict(zip(("az", "el"), position(ctl)))
        for axis in list(sense):
            if (goal[axis] - here[axis]) * sense[axis] <= (LEAD[axis] if stop_short else 0.0):
                del sense[axis]
        ctl.drive([DIRECTION[axis, sense[axis]] for axis in ("az", "el") if axis in sense])
        if not sense:
            return
        if max(abs(here[k] - last_pos[k]) for k in sense) > 0.5:
            last_change, last_pos = time.time(), dict(here)
        elif time.time() - last_change > 3:
            raise Abort(f"rotator not moving toward {target} (at {here['az']:.1f}, {here['el']:.1f})")
        if time.time() > deadline:
            raise Abort(f"move to {target} timed out at ({here['az']:.1f}, {here['el']:.1f})")
        time.sleep(0.1)


def settle(ctl, seconds=2.0):
    """Wait until no output is active and the position has stopped changing."""
    end = time.time() + 15
    prev = None
    while time.time() < end:
        time.sleep(seconds / 2)
        tel = ctl.state()
        c = tel["control"]
        pos = position(ctl)
        if c["azimuth"] is None and c["elevation"] is None and prev is not None \
                and max(abs(pos[0] - prev[0]), abs(pos[1] - prev[1])) < 0.35:
            return pos
        prev = pos
    raise Abort("rotator did not settle")


def goto(ctl, target):
    """Closed-loop positioning to within TOL, allowing for coast and overshoot."""
    for _ in range(4):
        move(ctl, target)
        az, el = settle(ctl)
        if abs(az - target[0]) <= TOL["az"] and abs(el - target[1]) <= TOL["el"]:
            return az, el
    raise Abort(f"could not reach {target}: at ({az:.1f}, {el:.1f})")


# ---------------------------------------------------------------- scans

def rotator_az(sky_az, near):
    """The G-5500 azimuth (0..450, unwrapped) for a sky azimuth, nearest `near`."""
    options = [a for a in (sky_az, sky_az + 360) if 0 <= a <= 450]
    return min(options, key=lambda a: abs(a - near))


def sweep(ctl, radio, site, axis, start, end, fixed):
    """Position at the start, sweep to the end, return per-reading offsets from the sun."""
    lat, lon = site
    goto(ctl, (start, fixed) if axis == "az" else (fixed, start))
    time.sleep(0.5)
    t0 = time.time()
    move(ctl, (end, fixed) if axis == "az" else (fixed, end), stop_short=False)
    settle(ctl)
    t1 = time.time()
    with ctl.cond:
        # Repeated telemetry can carry the same feedback sample; keep one of each.
        pos = np.array(sorted({round(p[0], 2): p for p in ctl.positions if t0 - 2 <= p[0] <= t1}.values()))
    rd = radio.since(t0, t1)
    if len(pos) < 4 or len(rd) < 20:
        raise Abort(f"{axis} sweep: too few readings ({len(pos)} positions, {len(rd)} power)")
    t = np.array([r[0] for r in rd])
    p_db = np.array([r[1] for r in rd])
    clipped = max(r[2] for r in rd)
    az = np.interp(t, pos[:, 0], pos[:, 1])
    el = np.interp(t, pos[:, 0], pos[:, 2])
    sun = np.array([sun_apparent(lat, lon, ti) for ti in t])
    d_az = np.array([wrap180(a - s) for a, s in zip(az, sun[:, 0])]) * np.cos(np.radians(sun[:, 1]))
    d_el = el - sun[:, 1]                                              # both on the sky
    return {"axis": axis, "t": t, "power_db": p_db, "az": az, "el": el, "sun_az": sun[:, 0],
            "sun_el": sun[:, 1], "x": d_az if axis == "az" else d_el,
            "other": d_el if axis == "az" else d_az, "clipped": clipped}


def fit(s):
    """Gaussian beam on a sloping baseline, in linear power against on-sky offset."""
    from scipy.optimize import curve_fit
    x = s["x"]
    y = 10 ** (s["power_db"] / 10)
    base = np.median(y)
    y = y / base

    def model(x, a, x0, w, b0, b1):
        return b0 + b1 * x + a * np.exp(-4 * np.log(2) * ((x - x0) / w) ** 2)

    k = max(3, len(y) // 40)
    smooth = np.convolve(y, np.ones(k) / k, mode="same")
    i = int(np.argmax(smooth))
    p0 = [max(smooth[i] - 1, 1e-3), x[i], 10.0, 1.0, 0.0]
    lo, hi = float(np.min(x)), float(np.max(x))
    bounds = ([0, lo, 2, 0, -np.inf], [np.inf, hi, 60, np.inf, np.inf])
    popt, pcov = curve_fit(model, x, y, p0=p0, bounds=bounds, maxfev=20000)
    # Refit within 1.5 beamwidths of that peak: a straight baseline cannot
    # follow ground pickup rising toward the horizon across a whole sweep.
    near = np.abs(x - popt[1]) <= 1.5 * popt[2]
    if near.sum() >= 30 and x[near].min() < popt[1] - 0.5 * popt[2] < popt[1] + 0.5 * popt[2] < x[near].max():
        x, y = x[near], y[near]
        popt, pcov = curve_fit(model, x, y, p0=popt, bounds=bounds, maxfev=20000)
    a, x0, w, b0, b1 = popt
    resid = y - model(x, *popt)
    floor = b0 + b1 * x0
    out = {"offset_deg": float(x0), "offset_sigma_deg": float(np.sqrt(pcov[1, 1])), "fwhm_deg": float(w),
           "sun_rise_db": float(10 * np.log10((floor + a) / floor)), "snr": float(a / np.std(resid)),
           "params": [float(v) for v in popt], "base": float(base)}
    out["good"] = bool(out["snr"] >= 8 and lo + 0.4 * w <= x0 <= hi - 0.4 * w and 3 <= w <= 40)
    return out


def describe(s, f):
    other = float(np.median(s["other"]))
    return (f"{s['axis']} sweep {s['x'][0]:+.1f} -> {s['x'][-1]:+.1f} deg on the sky "
            f"(other axis {other:+.1f} deg off the sun): peak at {f['offset_deg']:+.2f} "
            f"+/- {f['offset_sigma_deg']:.2f} deg, FWHM {f['fwhm_deg']:.1f} deg, sun "
            f"{f['sun_rise_db']:+.2f} dB over sky, SNR {f['snr']:.0f}"
            + ("" if f["good"] else "  ** unreliable **")
            + (f"  ** {100 * s['clipped']:.2f} % clipped **" if s["clipped"] > 1e-4 else ""))


def plot(sweeps, fits, path):
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    fig, axes = plt.subplots(1, len(sweeps), figsize=(4.2 * len(sweeps), 3.6), squeeze=False)
    for ax, s, f in zip(axes[0], sweeps, fits):
        ax.plot(s["x"], s["power_db"], ".", ms=3, label="power")
        xs = np.linspace(s["x"].min(), s["x"].max(), 400)
        a, x0, w, b0, b1 = f["params"]
        ys = (b0 + b1 * xs + a * np.exp(-4 * np.log(2) * ((xs - x0) / w) ** 2)) * f["base"]
        ax.plot(xs, 10 * np.log10(ys), "-", label="fit")
        ax.axvline(x0, ls=":", c="k")
        ax.set_title(f"{s['axis']} sweep: peak {x0:+.2f} deg")
        ax.set_xlabel("offset from sun on the sky, deg")
        ax.set_ylabel("power, dB")
        ax.grid(alpha=0.3)
    fig.tight_layout()
    fig.savefig(path, dpi=110)


# ----------------------------------------------------------------- main

def plan(a, site, ctl):
    tel = ctl.wait_telemetry()
    lat, lon = site
    sun_az, sun_el = sun_apparent(lat, lon, time.time())
    az, el = tel["feedback"]["azimuth_tenths"], tel["feedback"]["elevation_tenths"]
    c = tel["control"]
    print(f"sun: az {sun_az:.1f}, el {sun_el:.1f} (apparent)")
    print(f"controller: {tel['tracking']['mode']}/{tel['tracking']['phase']}, feedback "
          f"{tel['feedback']['health']} at az {az and az / 10}, el {el and el / 10}, duty left "
          f"{c['duty_remaining_ms'][0] / 1000:.0f}/{c['duty_remaining_ms'][1] / 1000:.0f} s, "
          f"startup rest {'pending' if c['duty_startup_rest'] else 'clear'}, "
          f"control {'held by this connection' if c['controller'] else 'available or held elsewhere'}")
    half_az = a.span_az / max(math.cos(math.radians(sun_el)), 0.2)
    el_lo, el_hi = max(a.min_el, sun_el - a.span_el), min(85.0, sun_el + a.span_el)
    print(f"plan: az sweeps over sun +/- {a.span_az:.0f} deg on the sky (+/- {half_az:.0f} deg of rotator), "
          f"el sweeps {el_lo:.0f} -> {el_hi:.0f} deg; about "
          f"{4 * 2 * half_az / RATE['az'] + 30:.0f} s of az and {4 * (el_hi - el_lo) / RATE['el'] + 30:.0f} s "
          "of el motor time")
    if sun_el < a.min_sun_el:
        raise Abort(f"sun at {sun_el:.1f} deg, below --min-sun-el {a.min_sun_el}")
    check_motion_allowed(tel)
    return half_az, el_lo, el_hi


def run(a, site, ctl, radio):
    lat, lon = site
    sweeps, fits = [], []
    d_az = 0.0                                   # on-sky offsets found so far
    d_el = 0.0
    order = [("az", +1), ("el", +1), ("az", -1), ("el", -1)]
    for n, (axis, sense) in enumerate(order, 1):
        sun_az, sun_el = sun_apparent(lat, lon, time.time() + 20)
        cosel = max(math.cos(math.radians(sun_el)), 0.2)
        here = position(ctl)
        if axis == "az":
            centre = rotator_az(sun_az, here[0]) + d_az / cosel
            half = a.span_az / cosel
            start, end = centre - sense * half, centre + sense * half
            fixed = min(max(sun_el + d_el, a.min_el), 85.0)
        else:
            lo, hi = max(a.min_el, sun_el + d_el - a.span_el), min(85.0, sun_el + d_el + a.span_el)
            start, end = (lo, hi) if sense > 0 else (hi, lo)
            fixed = rotator_az(sun_az, here[0]) + d_az / cosel
        if not (0 <= min(start, end) and max(start, end) <= (450 if axis == "az" else 85)):
            raise Abort(f"{axis} sweep {start:.1f} -> {end:.1f} leaves the travel range")
        print(f"[{n}/4] {axis} sweep {start:.1f} -> {end:.1f} at {'el' if axis == 'az' else 'az'} {fixed:.1f}",
              flush=True)
        s = sweep(ctl, radio, site, axis, start, end, fixed)
        f = fit(s)
        print("      " + describe(s, f), flush=True)
        sweeps.append(s)
        fits.append(f)
        if not f["good"]:
            raise Abort(f"{axis} sweep found no clear peak; widen --span-{axis} or check the receive chain")
        if axis == "az":
            d_az = f["offset_deg"]
        else:
            d_el = f["offset_deg"]
    return sweeps, fits


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--go", action="store_true", help="move the dish and scan (default: check only)")
    p.add_argument("--controller", default="ezal.legge.tt", help="host[:port] of the dashboard")
    p.add_argument("--site", type=float, nargs=2, metavar=("LAT", "LON"))
    p.add_argument("--freq", type=float, default=1701.0, help="MHz (default 1701)")
    p.add_argument("--gain", type=int, default=14, help="sensitivity gain, as SatDump's default (14)")
    p.add_argument("--span-az", type=float, default=25.0, help="half-width of az sweeps on the sky, deg")
    p.add_argument("--span-el", type=float, default=20.0, help="half-width of el sweeps, deg")
    p.add_argument("--min-el", type=float, default=3.0, help="lowest elevation to drive to, deg")
    p.add_argument("--min-sun-el", type=float, default=8.0)
    p.add_argument("--out", default="/srv/passes/sunscan")
    a = p.parse_args()

    site = a.site or site_from_env()
    if not site:
        sys.exit("need --site LAT LON (no .env found)")

    def interrupted(*_):
        raise KeyboardInterrupt        # unwinds through finally, which sends stop

    for sig in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, interrupted)

    ctl = radio = None
    try:
        ctl = Controller(a.controller)
        plan(a, site, ctl)
        if not a.go:
            print("check only: run again with --go to scan")
            return
        os.makedirs(a.out, exist_ok=True)
        radio = Radio(a.freq, a.gain)
        radio.wait_ready()
        ctl.take_control()
        sweeps, fits = run(a, site, ctl, radio)
    except (Abort, OSError) as e:
        sys.exit(f"stopped: {e}")
    except KeyboardInterrupt:
        sys.exit("stopped: interrupted")
    finally:
        if ctl:
            ctl.close()
        if radio:
            radio.close()

    sun_el = float(np.median(np.concatenate([s["sun_el"] for s in sweeps])))
    az_f = [f for s, f in zip(sweeps, fits) if s["axis"] == "az"]
    el_f = [f for s, f in zip(sweeps, fits) if s["axis"] == "el"]
    d_az = np.mean([f["offset_deg"] for f in az_f])
    d_el = np.mean([f["offset_deg"] for f in el_f])
    rot_az = d_az / max(math.cos(math.radians(sun_el)), 0.2)
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    lines = [
        f"=== ezal sun scan {stamp}  site {site[0]:.5f},{site[1]:.5f}  {a.freq} MHz  gain {a.gain} ===",
        *[describe(s, f) for s, f in zip(sweeps, fits)],
        f"beam peak: {d_az:+.2f} deg in azimuth on the sky ({rot_az:+.2f} deg of rotator azimuth at "
        f"el {sun_el:.0f}), {d_el:+.2f} deg in elevation",
        f"  sweep directions disagree by {abs(az_f[0]['offset_deg'] - az_f[1]['offset_deg']):.2f} deg (az) "
        f"and {abs(el_f[0]['offset_deg'] - el_f[1]['offset_deg']):.2f} deg (el): feedback lag and backlash",
        f"  the beam is on a target when the feedback reads target + ({rot_az:+.2f}, {d_el:+.2f}) deg;",
        f"  correct the feedback by subtracting {rot_az:+.2f} deg (az) and {d_el:+.2f} deg (el).",
        f"  FWHM {np.mean([f['fwhm_deg'] for f in az_f]):.1f} deg (az), "
        f"{np.mean([f['fwhm_deg'] for f in el_f]):.1f} deg (el); "
        f"sun {np.mean([f['sun_rise_db'] for f in fits]):+.2f} dB over the sky",
    ]
    text = "\n".join(lines)
    print("\n" + text)
    base = os.path.join(a.out, f"sunscan_{stamp}")
    with open(base + ".txt", "w") as f:
        f.write(text + "\n")
    with open(base + ".json", "w") as f:
        json.dump({"site": site, "freq_mhz": a.freq, "gain": a.gain, "fits": fits,
                   "sweeps": [{k: (v.tolist() if isinstance(v, np.ndarray) else v) for k, v in s.items()}
                              for s in sweeps]}, f)
    plot(sweeps, fits, base + ".png")
    print(f"(saved {base}.txt, .json, .png)")


if __name__ == "__main__":
    main()
