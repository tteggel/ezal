#!/usr/bin/env python3
"""Guided RF bench tests for the ezal receive chain (SatDump + HydraSDR RFOne).

Walks through short test recordings, makes each one with SatDump at the
right tuning, analyses it with matching settings, and prints one summary
block to copy back. Run it inside tmux: the Wi-Fi test freezes SSH briefly.

Groups (default: spur lna sky):
  spur     where the ~1700 MHz line comes from (coax off, retuned, Pi Wi-Fi
           off, ezal controller off, RFOne moved)
  lna      noise floor with the antenna, with bias off, and with coax off
  pickup   whether the ~1700 MHz line reaches the antenna side: LNA on/off,
           RFOne moved away from the feed, RFOne shielded
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
SPAN_HZ = 20e3     # window searched for the narrow line


# ---------------------------------------------------------------- analysis

def analyse(path, tuned_mhz, zoom_mhz, sr=SR):
    """Noise floor, level, clipping and the strongest narrow line near zoom_mhz."""
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
    zf = tuned_mhz * 1e6 + np.fft.fftshift(np.fft.fftfreq(zn, 1 / sr))
    sel = np.abs(zf - zoom_mhz * 1e6) <= SPAN_HZ / 2
    line = None
    if sel.any():
        zwin = np.hanning(zn).astype(np.float32)
        zp = np.zeros(np.count_nonzero(sel))
        zs = np.linspace(start, n - zn, 16).astype(np.int64)
        for s in zs:
            zp += np.abs(np.fft.fftshift(np.fft.fft(chunk(s, zn) * zwin))[sel]) ** 2
        i = int(np.argmax(zp))
        line = (zf[sel][i] - zoom_mhz * 1e6, 10 * np.log10(zp[i] / np.median(zp)))

    return {
        "floor_db": floor_db,
        "level_dbfs": 10 * np.log10(p_tot / nchunks),
        "clip_pct": 100 * clipped / total,
        "line": line,
        "seconds": n / sr,
    }


def fmt_row(name, tuned, bias, r):
    if r is None:
        return f"{name:<17} {tuned:>9.3f} {('on' if bias else 'off'):>4}  skipped"
    ln = r["line"]
    lhz = f"{ln[0]:+7.0f}" if ln else "      -"
    ldb = f"{ln[1]:6.1f}" if ln else "     -"
    lab = f"{line_abs(r):8.2f}" if ln else "       -"
    return (f"{name:<17} {tuned:>9.3f} {('on' if bias else 'off'):>4} {r['floor_db']:8.2f} "
            f"{r['level_dbfs']:8.2f} {r['clip_pct']:7.4f} {lhz} {ldb} {lab}")


def line_abs(r):
    """The line's strength on one scale across tests (floor + height above it)."""
    return r["floor_db"] + r["line"][1]


HEADER = (f"{'test':<17} {'tuned_MHz':>9} {'bias':>4} {'floor_dB':>8} {'lvl_dBFS':>8} "
          f"{'clip_%':>7} {'line_Hz':>7} {'line_dB':>6} {'line_abs':>8}")


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
        return r

    # --- groups

    def spur(self):
        self.test("coax_off_1699.3", 1699.3, False,
                  "Disconnect the coax from the RFOne's ANT port and leave the port open.")
        self.test("coax_off_1701", 1701.0, False, "Coax still off (retuned to 1701 MHz).")
        self.test("coax_off_1702.5", 1702.5, False, "Coax still off (retuned to 1702.5 MHz).")
        self.test("terminated", 1699.3, False,
                  "Fit a 50 ohm terminator on ANT (s to skip if you have none).")
        self.wifi_off_test()
        self.test("controller_off", 1699.3, False,
                  "Coax off. Power off ezal's Pico and the rotator control box.")
        self.test("rfone_far", 1699.3, False,
                  "Controller back on if you like. Coax off. Move the RFOne at least 1 m from\n"
                  "  the Pi on a USB extension, or into a black (USB 2) port.")

    def wifi_off_test(self):
        """Record with the Pi's Wi-Fi radio off; a detached timer turns it back on."""
        name, tuned = "wifi_off", 1699.3
        r = None
        if self.ask(f"[{name}] Coax off. The Pi's Wi-Fi is switched off for about 20 s while this\n"
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
                    path = self.record(name, tuned, False)
                finally:
                    subprocess.run(["sudo", "nmcli", "radio", "wifi", "on"])
                if path:
                    r = analyse(path, tuned, self.a.zoom)
                    if not self.a.keep:
                        os.remove(path)
        self.rows.append((name, tuned, False, r))
        self.res[name] = r
        print("  " + HEADER + "\n  " + fmt_row(name, tuned, False, r))

    def lna(self):
        self.test("sky_bias_on", 1699.3, True,
                  "Reconnect the coax to ANT. Point the dish at clear sky about 45 deg up,\n"
                  "  away from the sun and buildings.")
        self.test("sky_bias_off", 1699.3, False, "Same pointing, bias-tee off (LNA unpowered).")
        if "coax_off_1699.3" not in self.res:
            self.test("coax_off_1699.3", 1699.3, False,
                      "Disconnect the coax from ANT and leave the port open.")

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
        if "coax_off_1699.3" not in self.res:
            self.test("coax_off_1699.3", 1699.3, False,
                      "Disconnect the coax from ANT and leave the port open.")

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
        for x in [d("sky_bias_on", "coax_off_1699.3", "antenna+LNA over receiver noise (sky_bias_on - coax_off)"),
                  d("sky_bias_on", "sky_bias_off", "LNA powered vs unpowered (bias on - off)"),
                  d("ground", "sky_bias_on", "ground - cold sky"),
                  d("sun_on", "sun_off", "sun - off-sun")]:
            if x:
                lines.append(x)
        ref = R.get("coax_off_1699.3")
        if ref and ref["line"]:
            for n, label in (("sky_bias_on", "antenna on, LNA on"),
                             ("sky_bias_off", "antenna on, LNA off"),
                             ("ant_rfone_far", "antenna on, RFOne moved away"),
                             ("ant_rfone_tin", "antenna on, RFOne shielded")):
                if R.get(n) and R[n]["line"] and abs(R[n]["line"][0] - ref["line"][0]) < 1500:
                    lines.append(f"  ~1700 MHz line, {label}: {line_abs(R[n]) - line_abs(ref):+.1f} dB "
                                 "stronger than with the coax off")
        offs = [(n, R[n]["line"]) for n in ("coax_off_1699.3", "coax_off_1701", "coax_off_1702.5")
                if R.get(n) and R[n]["line"]]
        if offs:
            strong = [n for n, l in offs if l[1] > 15]
            lines.append("  ~1700 MHz line, coax off: " + ", ".join(
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
                   help="spur lna pickup sky sun sunscan, or: analyse FILE")
    p.add_argument("--gain", type=int, default=14, help="--general_gain for every test (default 14)")
    p.add_argument("--dir", default="/srv/passes/rftest", help="where recordings and logs go")
    p.add_argument("--zoom", type=float, default=1700.0, help="MHz to search for the narrow line")
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

    bad = [g for g in a.groups if g not in ("spur", "lna", "pickup", "sky", "sun", "sunscan")]
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
