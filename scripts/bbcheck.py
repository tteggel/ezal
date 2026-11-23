#!/usr/bin/env python3
"""Quick-look analysis of a SatDump cs16 baseband recording.

Writes PNGs next to the input (or to --out):
  *_waterfall.png  wideband spectrogram over the whole recording
  *_spectrum.png   median and max-hold spectrum
  *_power.png      in-band vs out-of-band power over time
  *_zoom.png       narrow waterfall around --zoom MHz (Doppler-curving carrier)
and prints level / clipping statistics.

Usage:
  python3 bbcheck.py FILE.cs16 --samplerate 10e6 --center 1702.5 \
      [--band 1700 3] [--zoom 1700 --span 120]
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
clip = 0
peak = 0.0
for r, s in enumerate(starts):
    x = chunk(s, a.nfft * navg)
    rms_db[r] = 10 * np.log10(np.mean(np.abs(x) ** 2) + 1e-20)
    m = max(np.max(np.abs(x.real)), np.max(np.abs(x.imag)))
    peak = max(peak, m)
    clip += np.count_nonzero((np.abs(x.real) > 0.99) | (np.abs(x.imag) > 0.99))
    f = np.fft.fftshift(np.fft.fft(x.reshape(navg, a.nfft) * win, axis=1), axes=1)
    wf[r] = 10 * np.log10(np.mean(np.abs(f) ** 2, axis=0) / wnorm + 1e-20)

total = rows * a.nfft * navg
freqs = a.center + np.fft.fftshift(np.fft.fftfreq(a.nfft, 1 / sr)) / 1e6
t_min = starts / sr / 60

print(f"level: mean {np.mean(rms_db):.1f} dBFS, min {np.min(rms_db):.1f}, max {np.max(rms_db):.1f}")
print(f"peak |sample|: {peak:.3f} of full scale; clipped samples: {100*clip/total:.4f} %")
if clip / total > 1e-4:
    print("  -> clipping: reduce --general_gain")
elif np.mean(rms_db) < -50:
    print("  -> very low level: gain may be too low, or LNA/bias not powered")

# Ignore the outer 5 % each side (anti-alias roll-off) when scaling colours.
edge = int(a.nfft * 0.05)
body = wf[:, edge:-edge]
vmin, vmax = np.percentile(body, 5), np.percentile(body, 99.9)

plt.figure(figsize=(12, 8))
plt.imshow(wf, aspect="auto", origin="lower", cmap="viridis", vmin=vmin, vmax=vmax,
           extent=[freqs[0], freqs[-1], t_min[0], t_min[-1]])
plt.colorbar(label="dB (relative)")
plt.xlabel("MHz"); plt.ylabel("minutes from start")
plt.title(os.path.basename(a.file) + " - waterfall")
plt.savefig(out + "_waterfall.png", dpi=110, bbox_inches="tight"); plt.close()

med = np.median(wf, axis=0)
plt.figure(figsize=(12, 5))
plt.plot(freqs, np.max(wf, axis=0), lw=0.6, label="max hold", alpha=0.7)
plt.plot(freqs, med, lw=0.8, label="median")
if a.band:
    plt.axvspan(a.band[0] - a.band[1] / 2, a.band[0] + a.band[1] / 2, color="orange", alpha=0.15,
                label="expected channel")
plt.ylim(np.percentile(med, 1) - 3, np.max(body) + 3)
plt.xlabel("MHz"); plt.ylabel("dB (relative)"); plt.legend(); plt.grid(alpha=0.3)
plt.title("spectrum")
plt.savefig(out + "_spectrum.png", dpi=110, bbox_inches="tight"); plt.close()

plt.figure(figsize=(12, 4))
plt.plot(t_min, rms_db, label="total power (dBFS)")
if a.band:
    lo, hi = a.band[0] - a.band[1] / 2, a.band[0] + a.band[1] / 2
    inb = (freqs >= lo) & (freqs <= hi)
    outb = ~inb
    outb[:edge] = outb[-edge:] = False
    lin = 10 ** (wf / 10)
    diff = 10 * np.log10(lin[:, inb].mean(1) / lin[:, outb].mean(1))
    print(f"in-band minus out-of-band: median {np.median(diff):+.2f} dB, max {np.max(diff):+.2f} dB")
    if np.max(diff) < 0.5:
        print("  -> nothing above the noise floor in the expected channel")
    ax2 = plt.gca().twinx()
    ax2.plot(t_min, diff, color="C1", label="in-band minus out-of-band (dB)")
    ax2.set_ylabel("dB", color="C1")
plt.xlabel("minutes from start"); plt.title("power over time"); plt.grid(alpha=0.3)
plt.savefig(out + "_power.png", dpi=110, bbox_inches="tight"); plt.close()

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

print("wrote", ", ".join(out + s for s in ["_waterfall.png", "_spectrum.png", "_power.png"]
                         + (["_zoom.png"] if a.zoom is not None else [])))
