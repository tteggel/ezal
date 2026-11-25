#!/usr/bin/env python3
"""Guided RF bench tests for the ezal receive chain (SatDump + HydraSDR RFOne).

Walks through short test recordings, makes each one with SatDump at the
right tuning, analyses it with matching settings, and prints one summary
block to copy back. Run it inside tmux: the Wi-Fi test freezes SSH briefly.

No test disconnects the coax: unplugging it once damaged the receive chain.
The reference for the receiver's own noise, and for lines picked up after
the LNA, is a recording with the bias-tee off instead.

Groups (default: spur lna sky):
  spur     where the lines near 1700 MHz come from (retuned, LNA off, Pi
           Wi-Fi off, ezal controller off, RFOne's USB port)
  lna      noise floor with the LNA powered and unpowered
  pickup   whether the ~1700 MHz line reaches the antenna side: LNA on/off,
           RFOne moved away from the feed, RFOne shielded
  bench    feed + LNA + RFOne without the dish: sky/ground, LNA off,
           RFOne foil-shielded, short USB cable
  foil     RFOne foil off/on three times with the feed fixed, then sky/ground
           with the foil on
  usb      5 m USB cable against a short one, bracketed
  sky      cold sky against ground (antenna and LNA working?)
  sun      on the sun against 20 deg away (pointing and antenna, works in cloud)
  sunscan  step around the sun to find the azimuth/elevation pointing offset

  python3 rftest.py [GROUP ...] [--gain 14] [--dir /srv/passes/rftest]
  python3 rftest.py analyse FILE.cs16 --tuned 1699.3   # one existing file

Site coordinates come from --site LAT LON, else EZAL_LATITUDE_DEG /
EZAL_LONGITUDE_DEG in the repository's .env or .env.example.
"""
import argparse
import datetime as dt
import math
import os
import subprocess
import sys
import time

import numpy as np

SR = 10e6
RECORD_S = 12      # seconds recorded per test
SKIP_S = 1.0       # SDR start-up transient ignored at the start
SPAN_HZ = 80e3     # window searched for narrow lines: Meteor's Doppler range, about +/-37 kHz
LINE_DB = 10.0     # a line stands this far above the mean noise in one snapshot
LINE_STEP_HZ = 1000  # a steady line moves less than this between snapshots
STEADY = 0.5       # ... and is found in at least this share of them; a satellite carrier is not


# ---------------------------------------------------------------- analysis

def find_peaks(snr_db, bin_hz):
    """Bins at least LINE_DB up, strongest first, one per 200 Hz (a windowed tone's width)."""
    found = []
    for i in np.flatnonzero(snr_db >= LINE_DB)[np.argsort(-snr_db[snr_db >= LINE_DB])]:
        if all(abs(int(i) - j) * bin_hz > 200 for j in found):
            found.append(int(i))
    return found


def analyse(path, tuned_mhz, zoom_mhz, sr=SR):
    """Noise floor, level, clipping and the steady narrow lines near zoom_mhz.

    Lines are found in each of 16 snapshots and followed from one to the next,
    so a line that drifts as the SDR warms up is still one line, and a
    satellite carrier, which moves further between snapshots, is not one."""
    data = np.memmap(path, dtype=np.int16, mode="r")
    n = data.size // 2
    start = int(SKIP_S * sr)
    if n - start < sr:
        raise RuntimeError(f"{path}: only {n / sr:.1f} s recorded")

    def chunk(s, k):
        x = np.asarray(data[2 * s:2 * (s + k)], dtype=np.float32)
        return (x[0::2] + 1j * x[1::2]) / 32768.0

    nfft, navg, nchunks = 4096, 4, 64
    win = np.hanning(nfft).astype(np.float32)
    psd = np.zeros(nfft)
    p_tot, clipped, total = 0.0, 0, 0
    for s in np.linspace(start, n - nfft * navg, nchunks).astype(np.int64):
        x = chunk(s, nfft * navg)
        p_tot += np.mean(np.abs(x) ** 2)
        clipped += np.count_nonzero((np.abs(x.real) > 0.99) | (np.abs(x.imag) > 0.99))
        total += x.size
        f = np.fft.fft(x.reshape(navg, nfft) * win, axis=1)
        psd += np.fft.fftshift(np.mean(np.abs(f) ** 2, axis=0)) / np.sum(win ** 2)
    psd /= nchunks
    c = nfft // 10                       # central 80 %: away from the roll-off
    floor_db = 10 * np.log10(np.median(psd[c:-c]))  # median ignores narrow lines

    zn = 1 << 18                         # 38 Hz bins at 10 MS/s
    off = tuned_mhz * 1e6 + np.fft.fftshift(np.fft.fftfreq(zn, 1 / sr)) - zoom_mhz * 1e6
    sel = np.abs(off) <= SPAN_HZ / 2
    lines = []
    if sel.any():
        zwin = np.hanning(zn).astype(np.float32)
        snaps = np.linspace(start, n - zn, 16).astype(np.int64)
        tracks = []                      # [offsets, snrs, last snapshot]
        for k, s in enumerate(snaps):
            p = np.abs(np.fft.fftshift(np.fft.fft(chunk(s, zn) * zwin))[sel]) ** 2
            # Noise bins are exponential: median / ln 2 is their mean, unmoved by lines.
            snr = 10 * np.log10(p * np.log(2) / np.median(p))
            for i in find_peaks(snr, sr / zn):
                f = off[sel][i]
                near = [t for t in tracks if t[2] < k and abs(t[0][-1] - f) <= LINE_STEP_HZ]
                if near:
                    t = min(near, key=lambda t: abs(t[0][-1] - f))
                    t[0].append(f)
                    t[1].append(snr[i])
                    t[2] = k
                else:
                    tracks.append([[f], [snr[i]], k])
        lines = sorted(((float(np.median(f)), float(np.median(v)), float(np.ptp(f)))
                        for f, v, _ in tracks if len(f) >= STEADY * len(snaps)),
                       key=lambda ln: -ln[1])

    return {
        "floor_db": floor_db,
        "level_dbfs": 10 * np.log10(p_tot / nchunks),
        "clip_pct": 100 * clipped / total,
        "line": lines[0][:2] if lines else None,   # the strongest: (offset Hz, dB)
        "lines": lines,                            # every steady line: (offset Hz, dB, drift Hz)
        "seconds": n / sr,
    }


def fmt_row(name, tuned, bias, r):
    if r is None:
        return f"{name:<17} {tuned:>9.3f} {('on' if bias else 'off'):>4}  skipped"
    ln = r["line"]
    lhz = f"{ln[0]:+7.0f}" if ln else "      -"
    ldb = f"{ln[1]:6.1f}" if ln else "     -"
    lab = f"{line_abs(r):8.2f}" if ln else "       -"
    rest = "  ".join(f"{f:+.0f}Hz {v:.0f}dB" + (f" drift {d:.0f}Hz" if d > 200 else "")
                     for f, v, d in r["lines"])
    return (f"{name:<17} {tuned:>9.3f} {('on' if bias else 'off'):>4} {r['floor_db']:8.2f} "
            f"{r['level_dbfs']:8.2f} {r['clip_pct']:7.4f} {lhz} {ldb} {lab}  {rest}")


def line_abs(r):
    """The line's strength on one scale across tests (floor + height above it)."""
    return r["floor_db"] + r["line"][1]


HEADER = (f"{'test':<17} {'tuned_MHz':>9} {'bias':>4} {'floor_dB':>8} {'lvl_dBFS':>8} "
          f"{'clip_%':>7} {'line_Hz':>7} {'line_dB':>6} {'line_abs':>8}  all steady lines")


# ------------------------------------------------------------- sun position

def sun_azel(lat, lon, when=None):
    """Sun azimuth (deg from true north, clockwise) and elevation, ~0.1 deg."""
    when = when or dt.datetime.now(dt.timezone.utc)
    jd = when.timestamp() / 86400.0 + 2440587.5
    d = jd - 2451545.0
    g = math.radians((357.529 + 0.98560028 * d) % 360)
    q = (280.459 + 0.98564736 * d) % 360
    lam = math.radians(q + 1.915 * math.sin(g) + 0.020 * math.sin(2 * g))
    eps = math.radians(23.439 - 0.00000036 * d)
    ra = math.atan2(math.cos(eps) * math.sin(lam), math.cos(lam))
    dec = math.asin(math.sin(eps) * math.sin(lam))
    gmst = (18.697374558 + 24.06570982441908 * d) % 24
    ha = math.radians(gmst * 15 + lon) - ra
    la = math.radians(lat)
    el = math.asin(math.sin(la) * math.sin(dec) + math.cos(la) * math.cos(dec) * math.cos(ha))
    az = math.atan2(-math.sin(ha), math.tan(dec) * math.cos(la) - math.sin(la) * math.cos(ha))
    return math.degrees(az) % 360, math.degrees(el)


def site_from_env():
    here = os.path.dirname(os.path.abspath(__file__))
    for p in [os.path.join(here, "..", ".env"), os.path.join(here, "..", ".env.example"),
              os.path.expanduser("~/ezal/.env"), os.path.expanduser("~/ezal/.env.example")]:
        try:
            kv = dict(l.strip().split("=", 1) for l in open(p) if "=" in l and not l.startswith("#"))
            return float(kv["EZAL_LATITUDE_DEG"]), float(kv["EZAL_LONGITUDE_DEG"])
        except (OSError, KeyError, ValueError):
            continue
    return None


# ------------------------------------------------------------------ runner

class Runner:
    def __init__(self, a):
        self.a = a
        self.rows = []          # (name, tuned, bias, result)
        self.res = {}
        self.extra = []
        os.makedirs(a.dir, exist_ok=True)

    def ask(self, text):
        print("\n" + text)
        r = input("  Enter = record, s = skip, q = quit: ").strip().lower()
        if r == "q":
            raise KeyboardInterrupt
        return r != "s"

    def record(self, name, tuned, bias, delay=0, secs=RECORD_S):
        base = os.path.join(self.a.dir, name)
        for ext in (".cs16",):
            if os.path.exists(base + ext):
                os.remove(base + ext)
        if delay:
            print(f"  recording starts in {delay} s ...", flush=True)
            time.sleep(delay)
        cmd = ["satdump", "legacy", "record", base, "--source", "hydrasdr",
               # Plain digits only: SatDump's flag parser reads "1e+07" as a string.
               "--samplerate", f"{SR:.0f}", "--frequency", f"{tuned * 1e6:.0f}",
               "--general_gain", str(self.a.gain), "--baseband_format", "cs16",
               "--timeout", str(secs)]
        if bias:
            cmd.insert(cmd.index("--baseband_format"), "--bias")
        print(f"  recording {secs} s at {tuned} MHz, bias {'on' if bias else 'off'} ...", flush=True)
        with open(base + ".log", "w") as log:
            subprocess.run(cmd, stdout=log, stderr=subprocess.STDOUT)
        path = base + ".cs16"
        if not os.path.exists(path):
            with open(base + ".log", errors="replace") as log:
                errs = [l.strip() for l in log if "(E)" in l]
            print(f"  !! no recording: {errs[-1] if errs else 'see ' + base + '.log'}")
            return None
        got = os.path.getsize(path) / (4 * SR)
        if got < 0.9 * secs:
            print(f"  !! only {got:.1f} s of {secs} s recorded: samples dropped? see {base}.log")
        return path

    def test(self, name, tuned, bias, text, delay=0, secs=RECORD_S):
        r = None
        if self.ask(f"[{name}] {text}"):
            path = self.record(name, tuned, bias, delay, secs)
            if path:
                r = analyse(path, tuned, self.a.zoom)
                if not self.a.keep:
                    os.remove(path)
        self.rows.append((name, tuned, bias, r))
        self.res[name] = r
        print("  " + HEADER + "\n  " + fmt_row(name, tuned, bias, r))
        self.check_lna(name, bias, r)
        return r

    def receiver_floor(self):
        r = self.res.get("sky_bias_off")
        return r["floor_db"] if r else None

    def check_lna(self, name, bias, r):
        """Bias on but the floor at the receiver's own level: the LNA is not working."""
        ref = self.receiver_floor()
        if bias and r and ref is not None and r["floor_db"] - ref < 3:
            msg = (f"!! {name}: bias on but the floor is only {r['floor_db'] - ref:+.1f} dB above the "
                   "receiver's own noise: LNA not powered? (Pi supply, cable, bias-tee)")
            print("  " + msg)
            self.extra.append(msg)

    def need_reference(self):
        """The receiver's own noise, and the lines picked up after the LNA."""
        if "sky_bias_off" not in self.res:
            self.test("sky_bias_off", 1699.3, False,
                      "Reference: bias-tee off (LNA unpowered), everything else as normal.")

    def avg(self, names, key="floor_db"):
        vals = [line_abs(self.res[n]) if key == "line_abs" else self.res[n][key]
                for n in names if self.res.get(n) and (key != "line_abs" or self.res[n]["line"])]
        return (float(np.mean(vals)), float(np.ptp(vals)), len(vals)) if vals else None

    # --- groups

    def foil(self):
        """Repeat RFOne foil off/on with the feed fixed, then sky/ground with foil on."""
        self.need_reference()
        print("\nFor the whole group: feed OUTDOORS, MOUNTED pointing straight up (not hand-held),\n"
              "nobody within 2 m, cables in the same place each time. Only the foil changes.")
        for i in (1, 2, 3):
            self.test(f"foil_off_{i}", 1699.3, True, f"Round {i}: foil OFF the RFOne, bias on.")
            self.test(f"foil_on_{i}", 1699.3, True,
                      f"Round {i}: foil ON the RFOne (wrapped, or a metal sheet between it and the LNA).")
        self.test("ground_foil", 1699.3, True,
                  "Foil still on. Point the feed straight down at the ground, about 1 m above it.")
        self.test("sky_foil", 1699.3, True, "Foil still on. Feed back straight up at the sky.")
        offs = [f"foil_off_{i}" for i in (1, 2, 3)]
        ons = [f"foil_on_{i}" for i in (1, 2, 3)]
        a, b = self.avg(offs), self.avg(ons)
        if a and b:
            self.extra.append(f"foil: floor {b[0] - a[0]:+.2f} dB with foil (spread off {a[1]:.2f}, "
                              f"on {b[1]:.2f} dB over {a[2]}/{b[2]} rounds)")
        a, b = self.avg(offs, "line_abs"), self.avg(ons, "line_abs")
        if a and b:
            self.extra.append(f"foil: ~1700 MHz line {b[0] - a[0]:+.1f} dB with foil")
        sky = self.avg(["foil_on_3", "sky_foil"])
        if sky and self.res.get("ground_foil"):
            self.extra.append(f"foil on: ground - cold sky {self.res['ground_foil']['floor_db'] - sky[0]:+.2f} dB "
                              f"(sky before/after spread {sky[1]:.2f} dB)")

    def usb(self):
        """5 m USB cable against a short one, bracketed, same feed position."""
        self.need_reference()
        print("\nFor the whole group: Pi on its normal 5 A supply. Feed mounted and pointing the\n"
              "same way for all three tests (if the short cable can't reach the sky, point all\n"
              "three at the same thing). Foil the same for all three.")
        self.test("usb_5m", 1699.3, True, "5 m USB cable, ferrites as usual, bias on.")
        self.test("usb_short", 1699.3, True, "Short USB cable, bias on.")
        self.test("usb_5m_again", 1699.3, True, "Back to the 5 m cable, bias on.")
        long = self.avg(["usb_5m", "usb_5m_again"])
        if long and self.res.get("usb_short"):
            self.extra.append(f"usb: floor with short cable {self.res['usb_short']['floor_db'] - long[0]:+.2f} dB "
                              f"vs 5 m (5 m before/after spread {long[1]:.2f} dB)")
        long = self.avg(["usb_5m", "usb_5m_again"], "line_abs")
        if long and self.res.get("usb_short") and self.res["usb_short"]["line"]:
            self.extra.append(f"usb: ~1700 MHz line with short cable "
                              f"{line_abs(self.res['usb_short']) - long[0]:+.1f} dB vs 5 m")

    def spur(self):
        self.test("sky_1699.3", 1699.3, True,
                  "Everything connected as normal, bias on. Dish at clear sky, away from the sun.")
        self.test("sky_1701", 1701.0, True, "Same (retuned to 1701 MHz).")
        self.test("sky_1702.5", 1702.5, True, "Same (retuned to 1702.5 MHz).")
        self.need_reference()
        self.wifi_off_test()
        self.test("controller_off", 1699.3, True,
                  "Bias on. Power off ezal's Pico and the rotator control box.")
        self.test("rfone_port", 1699.3, True,
                  "Controller back on if you like. Bias on. Move the RFOne's USB cable to the\n"
                  "  Pi's other kind of port (black USB 2 / blue USB 3).")

    def wifi_off_test(self):
        """Record with the Pi's Wi-Fi radio off; a detached timer turns it back on."""
        name, tuned = "wifi_off", 1699.3
        r = None
        if self.ask(f"[{name}] Bias on. The Pi's Wi-Fi is switched off for about 20 s while this\n"
                    "  records, so SSH will freeze, then recover: keep this running in tmux. A\n"
                    "  safety timer turns Wi-Fi back on after 90 s whatever happens. sudo may\n"
                    "  ask for your password first."):
            if subprocess.run(["sudo", "-v"]).returncode != 0:
                print("  !! sudo failed; skipped")
            else:
                subprocess.Popen(["sudo", "sh", "-c", "sleep 90; nmcli radio wifi on"],
                                 start_new_session=True, stdout=subprocess.DEVNULL,
                                 stderr=subprocess.DEVNULL)
                path = None
                try:
                    subprocess.run(["sudo", "nmcli", "radio", "wifi", "off"])
                    time.sleep(3)
                    path = self.record(name, tuned, True)
                finally:
                    subprocess.run(["sudo", "nmcli", "radio", "wifi", "on"])
                if path:
                    r = analyse(path, tuned, self.a.zoom)
                    if not self.a.keep:
                        os.remove(path)
        self.rows.append((name, tuned, True, r))
        self.res[name] = r
        print("  " + HEADER + "\n  " + fmt_row(name, tuned, True, r))

    def lna(self):
        self.test("sky_bias_on", 1699.3, True,
                  "Bias on. Point the dish at clear sky about 45 deg up, away from the sun\n"
                  "  and buildings.")
        self.test("sky_bias_off", 1699.3, False, "Same pointing, bias-tee off (LNA unpowered).")

    def pickup(self):
        """Does the ~1700 MHz line reach the antenna side, and from where?"""
        if "sky_bias_on" not in self.res:
            self.test("sky_bias_on", 1699.3, True,
                      "Coax on ANT, bias on. Point the dish at clear sky about 45 deg up, away\n"
                      "  from the sun and buildings. RFOne and Pi where they normally live.")
        if "sky_bias_off" not in self.res:
            self.test("sky_bias_off", 1699.3, False, "Same pointing, bias-tee off (LNA unpowered).")
        self.test("ant_rfone_far", 1699.3, True,
                  "Coax on ANT, bias on, same pointing. Move the RFOne (and the Pi if it can go)\n"
                  "  as far from the feed and LNA as the cables allow.")
        self.test("ant_rfone_tin", 1699.3, True,
                  "Coax on ANT, bias on, same pointing, RFOne back in its usual place but inside\n"
                  "  a closed metal tin or wrapped in kitchen foil (USB and coax through a small\n"
                  "  gap; keep foil off the connectors' contacts).")

    def bench(self):
        """Feed + LNA + RFOne on the bench, no dish or rotator."""
        self.test("sky_bias_on", 1699.3, True,
                  "Everything connected as normal, bias on. Take the feed OUTDOORS and point it\n"
                  "  straight up at open sky, clear of walls, trees and people (indoors the\n"
                  "  walls are as warm as the ground and the next test shows nothing).")
        self.test("ground", 1699.3, True,
                  "Same, but point the feed straight down at the ground.")
        self.test("sky_bias_off", 1699.3, False,
                  "Feed back up at the sky, bias-tee off (LNA unpowered).")
        self.test("rfone_foil", 1699.3, True,
                  "Feed reconnected and pointing at the sky, bias on. Wrap the RFOne itself in\n"
                  "  kitchen foil inside the enclosure, or put a metal sheet between the RFOne\n"
                  "  and the LNA/feed (keep foil off connector contacts and the LNA circuit).")
        self.test("short_usb", 1699.3, True,
                  "Foil off. Feed at the sky, bias on. Replace the 5 m USB cable with a short\n"
                  "  one (bring the Pi to the bench), ferrites as usual.")

    def sky(self):
        if "sky_bias_on" not in self.res:
            self.test("sky_bias_on", 1699.3, True,
                      "Coax on ANT. Point the dish at clear sky about 45 deg up, away from the\n"
                      "  sun and buildings.")
        self.test("ground", 1699.3, True,
                  "Point the dish at the ground or trees (lowest elevation, facing something\n"
                  "  solid and not a building full of electronics).")

    def site(self):
        s = self.a.site or site_from_env()
        if not s:
            sys.exit("sun tests need --site LAT LON (no .env found)")
        return s

    def sun(self):
        lat, lon = self.site()
        az, el = sun_azel(lat, lon)
        if el < 5:
            print(f"\nSun at az {az:.1f}, el {el:.1f}: too low for the sun test; skipped.")
            return
        self.test("sun_on", 1699.3, True,
                  f"Coax on ANT, bias on. Point at the sun NOW: az {az:.1f}, el {el:.1f}\n"
                  "  (the sun moves ~0.25 deg/min; the position is for this moment).")
        az, el = sun_azel(lat, lon)
        off = (az + 20) % 360
        self.test("sun_off", 1699.3, True,
                  f"Same elevation, 20 deg along in azimuth: az {off:.1f}, el {el:.1f}.")

    def sunscan(self):
        lat, lon = self.site()
        steps = [-9, -6, -3, 0, 3, 6, 9]
        for axis in ("az", "el"):
            pts = []
            for o in steps:
                az, el = sun_azel(lat, lon)
                if el < 5:
                    print(f"\nSun at el {el:.1f}: too low; scan stopped.")
                    return
                taz = (az + (o / max(math.cos(math.radians(el)), 0.2) if axis == "az" else 0)) % 360
                tel = el + (o if axis == "el" else 0)
                name = f"scan_{axis}{o:+d}"
                r = self.test(name, 1699.3, True,
                              f"Sun scan, {axis} offset {o:+d} deg on the sky: point az {taz:.1f}, el {tel:.1f}.",
                              secs=6)
                if r:
                    pts.append((o, r["floor_db"]))
            if len(pts) >= 3:
                x, y = np.array(pts).T
                c2, c1, c0 = np.polyfit(x, y, 2)
                peak = -c1 / (2 * c2) if c2 < 0 else float("nan")
                rot = (f" = {peak / max(math.cos(math.radians(el)), 0.2):+.1f} deg of rotator azimuth"
                       if axis == "az" else "")
                self.extra.append(f"sun scan {axis}: peak at {peak:+.1f} deg on the sky{rot}, "
                                  f"rise {np.max(y) - np.min(y):.2f} dB over the scan "
                                  f"(points: {', '.join(f'{int(o)}:{v:.2f}' for o, v in pts)})")
                print("  " + self.extra[-1])

    # --- summary

    def summary(self):
        R = self.res
        def d(a, b, label):
            if R.get(a) and R.get(b):
                return f"  {label}: {R[a]['floor_db'] - R[b]['floor_db']:+.2f} dB"
            return None
        lines = [
            f"=== ezal rftest {dt.datetime.now(dt.timezone.utc):%Y-%m-%dT%H:%M:%SZ}  "
            f"gain={self.a.gain}  {SR / 1e6:g} MS/s  line window {self.a.zoom} MHz "
            f"+/-{SPAN_HZ / 2e3:g} kHz ===",
            HEADER,
        ]
        lines += [fmt_row(*row) for row in self.rows]
        lines.append("derived:")
        for x in [d("sky_bias_on", "sky_bias_off", "LNA powered vs unpowered (bias on - off)"),
                  d("ground", "sky_bias_on", "ground - cold sky"),
                  d("short_usb", "sky_bias_on", "floor with short USB - with 5 m USB"),
                  d("sun_on", "sun_off", "sun - off-sun")]:
            if x:
                lines.append(x)
        ref = R.get("sky_bias_off")
        if ref and ref["line"]:
            for n, label in (("sky_bias_on", "LNA on"),
                             ("rfone_foil", "LNA on, RFOne foil-shielded"),
                             ("short_usb", "LNA on, short USB cable"),
                             ("ant_rfone_far", "LNA on, RFOne moved away"),
                             ("ant_rfone_tin", "LNA on, RFOne shielded")):
                if R.get(n) and R[n]["line"] and abs(R[n]["line"][0] - ref["line"][0]) < 1500:
                    lines.append(f"  ~1700 MHz line, {label}: {line_abs(R[n]) - line_abs(ref):+.1f} dB "
                                 "stronger than with the LNA off")
        offs = [(n, R[n]["line"]) for n in ("sky_1699.3", "sky_1701", "sky_1702.5")
                if R.get(n) and R[n]["line"]]
        if offs:
            strong = [n for n, l in offs if l[1] > 15]
            lines.append("  strongest line near 1700 MHz, retuned: " + ", ".join(
                f"{n.split('_')[-1]} MHz -> {l[0]:+.0f} Hz {l[1]:.1f} dB" for n, l in offs)
                + (" => present at every tuning (fixed frequency)" if len(strong) == len(offs) else
                   " => absent at some tunings (tuning-dependent)" if strong else
                   " => not present"))
        lines += ["  " + e for e in self.extra]
        text = "\n".join(lines)
        path = os.path.join(self.a.dir, f"rftest_{dt.datetime.now():%Y%m%dT%H%M%S}.txt")
        with open(path, "w") as f:
            f.write(text + "\n")
        print("\n\n" + "-" * 30 + " copy from here " + "-" * 30 + "\n" + text +
              "\n" + "-" * 30 + " copy to here " + "-" * 32)
        print(f"(also saved to {path})")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("groups", nargs="*", default=["spur", "lna", "sky"],
                   help="spur lna pickup bench foil usb sky sun sunscan, or: analyse FILE")
    p.add_argument("--gain", type=int, default=14, help="--general_gain for every test (default 14)")
    p.add_argument("--dir", default="/srv/passes/rftest", help="where recordings and logs go")
    p.add_argument("--zoom", type=float, default=1700.0,
                   help=f"centre of the +/-{SPAN_HZ / 2e3:g} kHz search for narrow lines, MHz")
    p.add_argument("--site", type=float, nargs=2, metavar=("LAT", "LON"))
    p.add_argument("--keep", action="store_true", help="keep the .cs16 recordings (480 MB each)")
    p.add_argument("--tuned", type=float, help="analyse: the frequency the file was recorded at, MHz")
    a = p.parse_args()

    if a.groups and a.groups[0] == "analyse":
        if len(a.groups) != 2 or a.tuned is None:
            sys.exit("usage: rftest.py analyse FILE.cs16 --tuned MHZ")
        r = analyse(a.groups[1], a.tuned, a.zoom)
        print(HEADER + "\n" + fmt_row(os.path.basename(a.groups[1])[:17], a.tuned, False, r))
        return

    bad = [g for g in a.groups if g not in ("spur", "lna", "pickup", "bench", "foil", "usb", "sky", "sun", "sunscan")]
    if bad:
        sys.exit(f"unknown group(s): {' '.join(bad)}")
    if not os.environ.get("TMUX"):
        print("Note: not inside tmux. The wifi_off test freezes SSH for ~20 s; run"
              " `tmux new -s rftest` first, or skip that test.")
    print(f"Gain {a.gain}, {RECORD_S} s per test, recordings in {a.dir}"
          f"{'' if a.keep else ' (deleted after analysis)'}.")
    run = Runner(a)
    try:
        for g in a.groups:
            getattr(run, g)()
    except KeyboardInterrupt:
        print("\nstopped early")
    run.summary()


if __name__ == "__main__":
    main()
