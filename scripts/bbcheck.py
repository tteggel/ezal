#!/usr/bin/env python3
"""Quick-look analysis of a SatDump cs16 baseband recording.

Writes PNGs next to the input (or to --out):
  *_waterfall.png       wideband spectrogram over the whole recording
  *_waterfall_flat.png  the same, divided by the pre-AOS baseline spectrum and
                        by each row's median, so the SDR's passband shape and
                        whole-band floor changes are removed and a channel
                        standing above its neighbours shows
  *_spectrum.png        median and max-hold spectrum
  *_power.png           total power and in-band excess over the baseline
  *_zoom.png            narrow waterfall around --zoom MHz (Doppler carrier)
and prints level / clipping / detection statistics, plus the frequency and
strength of the strongest narrow line in the --zoom window.

Start the recording at least a minute before AOS: the first --baseline
seconds (after --skip) are taken as noise only, and everything later is
compared with them bin by bin.

Usage:
  python3 bbcheck.py FILE.cs16 --samplerate 10e6 --center 1702.5 \
      [--band 1700 3] [--zoom 1700 --span 120] [--baseline 60]
"""
import argparse
import os
import sys

import numpy as np
import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

p = argparse.ArgumentParser()
p.add_argument("file")
p.add_argument("--samplerate", type=float, required=True, help="Hz, e.g. 10e6")
p.add_argument("--center", type=float, required=True, help="tuned frequency, MHz")
p.add_argument("--band", type=float, nargs=2, metavar=("MHZ", "WIDTH_MHZ"),
               help="expected signal channel, e.g. 1700 3 (for the power plot)")
p.add_argument("--zoom", type=float, help="MHz to zoom on for a carrier/Doppler view")
p.add_argument("--span", type=float, default=120.0, help="zoom span, kHz (default 120)")
p.add_argument("--baseline", type=float, default=60.0,
               help="seconds of pre-AOS noise at the start, after --skip (default 60; 0 disables)")
p.add_argument("--skip", type=float, default=1.0,
               help="seconds to ignore at the start for the SDR start-up transient (default 1)")
p.add_argument("--rows", type=int, default=600, help="time rows (default 600)")
p.add_argument("--nfft", type=int, default=4096, help="wideband FFT size")
p.add_argument("--out", help="output prefix (default: input path without extension)")
a = p.parse_args()

sr = a.samplerate
data = np.memmap(a.file, dtype=np.int16, mode="r")
nsamp = data.size // 2
dur = nsamp / sr
if nsamp < a.nfft * 8:
    sys.exit("file too short")
out = a.out or os.path.splitext(a.file)[0]
print(f"{a.file}: {nsamp:,} samples = {dur:.1f} s at {sr/1e6:g} MS/s")

def chunk(start, n):
    x = np.asarray(data[2 * start:2 * (start + n)], dtype=np.float32)
    return (x[0::2] + 1j * x[1::2]) / 32768.0

rows = min(a.rows, int(nsamp // (a.nfft * 8)))
navg = 8
win = np.hanning(a.nfft).astype(np.float32)
wnorm = np.sum(win ** 2)
starts = np.linspace(0, nsamp - a.nfft * navg, rows).astype(np.int64)
wf = np.empty((rows, a.nfft), dtype=np.float32)
rms_db = np.empty(rows)
peaks = np.empty(rows)
clips = np.empty(rows)
for r, s in enumerate(starts):
    x = chunk(s, a.nfft * navg)
    rms_db[r] = 10 * np.log10(np.mean(np.abs(x) ** 2) + 1e-20)
    peaks[r] = max(np.max(np.abs(x.real)), np.max(np.abs(x.imag)))
    clips[r] = np.count_nonzero((np.abs(x.real) > 0.99) | (np.abs(x.imag) > 0.99))
    f = np.fft.fftshift(np.fft.fft(x.reshape(navg, a.nfft) * win, axis=1), axes=1)
    wf[r] = 10 * np.log10(np.mean(np.abs(f) ** 2, axis=0) / wnorm + 1e-20)

t_s = starts / sr
t_min = t_s / 60
freqs = a.center + np.fft.fftshift(np.fft.fftfreq(a.nfft, 1 / sr)) / 1e6
lin = 10 ** (wf / 10)

# Rows inside --skip hold the SDR start-up transient: report them, then leave
# them out of every statistic and of the baseline.
valid = t_s >= a.skip
if not valid.any():
    valid[:] = True
if (~valid).any() and peaks[~valid].max() > 2 * peaks[valid].max():
    print(f"start-up transient in the first {a.skip:g} s (peak {peaks[~valid].max():.3f}); ignored")

clip_frac = clips[valid].sum() / (np.count_nonzero(valid) * a.nfft * navg)
lv = rms_db[valid]
print(f"level: mean {np.mean(lv):.1f} dBFS, min {np.min(lv):.1f}, max {np.max(lv):.1f}")
print(f"peak |sample|: {peaks[valid].max():.3f} of full scale; clipped samples: {100*clip_frac:.4f} %")
if clip_frac > 1e-4:
    print("  -> clipping: reduce --general_gain")
elif np.mean(lv) < -50:
    print("  -> very low level: gain may be too low, or LNA/bias not powered")

# Pre-AOS baseline: per-bin mean of the first --baseline seconds after --skip.
base_rows = valid & (t_s < a.skip + a.baseline)
base = None
if a.baseline > 0 and np.count_nonzero(base_rows) >= 3:
    base = lin[base_rows].mean(axis=0)
    print(f"baseline: {np.count_nonzero(base_rows)} rows from {a.skip:g} s to "
          f"{min(a.skip + a.baseline, dur):.0f} s (must be before AOS)")
    if a.skip + a.baseline > dur / 3:
        print("  -> baseline covers over a third of the recording; if the satellite was "
              "already up, rises will be understated. Use a shorter --baseline.")
elif a.baseline > 0:
    print("baseline: recording too short; no baseline comparison")

# Ignore the outer 5 % each side (anti-alias roll-off) when scaling colours.
edge = int(a.nfft * 0.05)
body = wf[valid][:, edge:-edge]
vmin, vmax = np.percentile(body, 5), np.percentile(body, 99.9)
ext = [freqs[0], freqs[-1], t_min[0], t_min[-1]]

plt.figure(figsize=(12, 8))
plt.imshow(wf, aspect="auto", origin="lower", cmap="viridis", vmin=vmin, vmax=vmax, extent=ext)
plt.colorbar(label="dB (relative)")
plt.xlabel("MHz"); plt.ylabel("minutes from start")
plt.title(os.path.basename(a.file) + " - waterfall")
plt.savefig(out + "_waterfall.png", dpi=110, bbox_inches="tight"); plt.close()
written = ["_waterfall.png"]

if base is not None:
    # Relative to the baseline bin by bin (removes the passband shape), then
    # to each row's own median (removes rises that hit the whole band at
    # once), so only a channel standing above its neighbours remains.
    flat = wf - 10 * np.log10(base)
    flat -= np.median(flat[:, edge:-edge], axis=1, keepdims=True)
    fb = flat[valid][:, edge:-edge]
    plt.figure(figsize=(12, 8))
    plt.imshow(flat, aspect="auto", origin="lower", cmap="viridis",
               vmin=np.percentile(fb, 2), vmax=max(np.percentile(fb, 99.9), 1.0), extent=ext)
    plt.colorbar(label="dB above pre-AOS baseline, relative to the rest of the band")
    plt.axhline(t_min[base_rows][-1], color="w", lw=0.8, ls="--")
    plt.xlim(freqs[edge], freqs[-edge])
    plt.xlabel("MHz"); plt.ylabel("minutes from start")
    plt.title(os.path.basename(a.file) + " - waterfall relative to baseline (above dashed line)")
    plt.savefig(out + "_waterfall_flat.png", dpi=110, bbox_inches="tight"); plt.close()
    written.append("_waterfall_flat.png")

med = np.median(wf[valid], axis=0)
plt.figure(figsize=(12, 5))
plt.plot(freqs, np.max(wf[valid], axis=0), lw=0.6, label="max hold", alpha=0.7)
plt.plot(freqs, med, lw=0.8, label="median")
if base is not None:
    plt.plot(freqs, 10 * np.log10(base), lw=0.8, label="pre-AOS baseline")
if a.band:
    plt.axvspan(a.band[0] - a.band[1] / 2, a.band[0] + a.band[1] / 2, color="orange", alpha=0.15,
                label="expected channel")
plt.ylim(np.percentile(med, 1) - 3, np.max(body) + 3)
plt.xlabel("MHz"); plt.ylabel("dB (relative)"); plt.legend(); plt.grid(alpha=0.3)
plt.title("spectrum")
plt.savefig(out + "_spectrum.png", dpi=110, bbox_inches="tight"); plt.close()
written.append("_spectrum.png")

fig, ax = plt.subplots(figsize=(12, 4))
ax.plot(t_min[valid], rms_db[valid], label="total power")
ax.set_ylabel("total power, dBFS", color="C0")
ax.set_xlabel("minutes from start")
if base is not None:
    ax.axvspan(t_min[base_rows][0], t_min[base_rows][-1], color="grey", alpha=0.15)
if a.band:
    lo, hi = a.band[0] - a.band[1] / 2, a.band[0] + a.band[1] / 2
    inb = (freqs >= lo) & (freqs <= hi)
    outb = ~inb
    outb[:edge] = outb[-edge:] = False
    ax2 = ax.twinx()
    if base is not None:
        # Each bin relative to its own baseline removes the passband tilt.
        # Subtracting the rest of the band's rise removes changes that hit the
        # whole band at once (the dish seeing more or less warm ground, gain).
        rel = lin / base
        rise_in = 10 * np.log10(rel[:, inb].mean(1))
        rise_out = 10 * np.log10(rel[:, outb].mean(1))
        excess = rise_in - rise_out
        sigma = np.std(excess[base_rows])
        after = valid & ~base_rows
        ex_after = excess[after] if after.any() else excess[valid]
        imax = np.argmax(np.where(after, excess, -np.inf)) if after.any() else np.argmax(excess)
        print(f"in-band excess over baseline: max {np.max(ex_after):+.2f} dB at "
              f"{t_min[imax]:.1f} min, median {np.median(ex_after):+.2f} dB "
              f"(baseline noise 1 sigma = {sigma:.2f} dB)")
        if np.max(ex_after) < max(4 * sigma, 0.2):
            print("  -> no wideband rise in the expected channel distinguishable from noise")
        ax2.plot(t_min[valid], excess[valid], color="C1")
        ax2.axhline(0, color="C1", lw=0.5, ls=":")
        ax2.set_ylabel("in-band excess over baseline, dB", color="C1")
    else:
        diff = 10 * np.log10(lin[:, inb].mean(1) / lin[:, outb].mean(1))
        print(f"in-band minus out-of-band (no baseline, biased by passband tilt): "
              f"median {np.median(diff[valid]):+.2f} dB, max {np.max(diff[valid]):+.2f} dB")
        ax2.plot(t_min[valid], diff[valid], color="C1")
        ax2.set_ylabel("in-band minus out-of-band, dB", color="C1")
ax.grid(alpha=0.3)
ax.set_title("power over time" + (" (grey: pre-AOS baseline)" if base is not None else ""))
plt.savefig(out + "_power.png", dpi=110, bbox_inches="tight"); plt.close()
written.append("_power.png")

if a.zoom is not None:
    # Fine FFT, cropped to +/- span/2 around the zoom frequency: shows a
    # residual carrier as a line curving with Doppler (+/- ~40 kHz at L-band).
    zn = 1 << int(np.ceil(np.log2(sr / 50)))  # ~50 Hz bins or finer
    zn = min(zn, nsamp // 2)
    zrows = min(a.rows, int(nsamp // zn))
    zs = np.linspace(0, nsamp - zn, zrows).astype(np.int64)
    zf = a.center + np.fft.fftshift(np.fft.fftfreq(zn, 1 / sr)) / 1e6
    sel = np.abs(zf - a.zoom) <= a.span / 2e3
    zwin = np.hanning(zn).astype(np.float32)
    zw = np.empty((zrows, np.count_nonzero(sel)), dtype=np.float32)
    for r, s in enumerate(zs):
        f = np.fft.fftshift(np.fft.fft(chunk(s, zn) * zwin))
        zw[r] = 10 * np.log10(np.abs(f[sel]) ** 2 + 1e-20)
    # Strongest narrow line in the window, averaged over the recording: use it
    # to follow a spur across tests (retuned, coax off, devices unplugged).
    zmean = (10 ** (zw / 10)).mean(0)
    zi = int(np.argmax(zmean))
    print(f"strongest line within +/-{a.span/2:g} kHz of {a.zoom} MHz: "
          f"{zf[sel][zi]:.6f} MHz ({(zf[sel][zi] - a.zoom) * 1e6:+.0f} Hz), "
          f"{10 * np.log10(zmean[zi] / np.median(zmean)):.1f} dB above the window median "
          f"({sr / zn:.0f} Hz bins)")
    k = 8  # merge bins for a readable plot
    w = zw.shape[1] // k * k
    zw = 10 * np.log10((10 ** (zw[:, :w] / 10)).reshape(zrows, -1, k).mean(2))
    zx = (zf[sel][:w].reshape(-1, k).mean(1) - a.zoom) * 1e3
    plt.figure(figsize=(10, 8))
    plt.imshow(zw, aspect="auto", origin="lower", cmap="magma",
               vmin=np.percentile(zw, 5), vmax=np.percentile(zw, 99.95),
               extent=[zx[0], zx[-1], zs[0] / sr / 60, zs[-1] / sr / 60])
    plt.colorbar(label="dB (relative)")
    plt.xlabel(f"kHz from {a.zoom} MHz"); plt.ylabel("minutes from start")
    plt.title("zoom - a satellite carrier curves with Doppler; local interference is a straight line")
    plt.savefig(out + "_zoom.png", dpi=110, bbox_inches="tight"); plt.close()
    written.append("_zoom.png")

print("wrote", ", ".join(out + s for s in written))
