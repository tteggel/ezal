# Hardware

This document describes the hardware ezal runs on and the hardware ezal
will eventually drive. The "today" half is concrete; the "future" half is
a sketch to make sure we don't paint ourselves into a corner with the
firmware architecture.

## Today: just the Pico 2

For the step-one "hello morse" demo, the entire hardware setup is:

```
┌──────────────────────────┐
│   Raspberry Pi Pico 2    │
│  (RP2350A, 4 MiB flash)  │
│                          │
│  USB ────────── host PC  │
│                          │
│  GPIO 25 ─┐              │
│           │ onboard LED  │
│           ▼              │
│         (light)          │
│                          │
│  SWCLK ──┐               │
│  SWDIO ──┼── debug probe │
│  GND   ──┘               │
└──────────────────────────┘
```

You only need:

- one Raspberry Pi **Pico 2** (the wired variant, not Pico 2 W),
- a USB-C cable for power,
- *optionally* a Raspberry Pi Debug Probe (or any CMSIS-DAP probe) for
  flashing + log streaming with `probe-rs`. Without one you can still
  flash via the BOOTSEL UF2 mechanism, but you won't see live logs.

### Why not Pico 2 W?

The Pico 2 W's onboard LED is wired to the CYW43439 wireless co-processor,
not directly to GPIO 25. Lighting that LED requires bringing up the CYW43
stack, which is more involved than this demo warrants. The current
firmware assumes the plain Pico 2.

If you only have a Pico 2 W: most of the build still works, but the LED
will not blink. Add the CYW43 driver and we can fix that — track that work
in [the roadmap](../README.md#roadmap).

### Pinout reference (for the curious)

| signal     | Pico 2 pin | RP2350 GPIO | notes                       |
|------------|-----------:|------------:|-----------------------------|
| onboard LED|     —      |     25      | active high                 |
| SWCLK      |     —      |      —      | dedicated debug pin         |
| SWDIO      |     —      |      —      | dedicated debug pin         |
| GND        | many       |      —      |                             |

The future rotator-control pins are unassigned at this point; they will be
chosen when the analog hardware is designed.

## Future: the G-5500 rotator

The [Yaesu G-5500](https://www.yaesu.com/) (and the -DC variant) is the
target rotator. Its "external control" connector is an 8-pin DIN. The
interface is **switch closures** for direction (not analog speed control;
the speed comes from the G-5500's own controller) plus **analog
feedback** voltages for position.

| Pin | Direction | Function                                                     |
|-----|-----------|--------------------------------------------------------------|
| 1   | output    | Elevation feedback: 2.0–4.5 VDC corresponds to 0°–180°       |
| 2   | input     | Short to pin 8 → rotate right (clockwise azimuth)            |
| 3   | input     | Short to pin 8 → rotate up                                   |
| 4   | input     | Short to pin 8 → rotate left (counter-clockwise azimuth)     |
| 5   | input     | Short to pin 8 → rotate down                                 |
| 6   | output    | Azimuth feedback: 2.0–4.5 VDC corresponds to 0°–450°         |
| 7   | output    | Auxiliary supply: 8–13 VDC at up to 100 mA                   |
| 8   | —         | Common ground                                                |

In ezal terms, the data flow is:

```
host PC ──USB-serial──▶ Pico 2 ──×4 transistor switches──▶ G-5500 pins 2-5
                        ▲ ▲                                       │
                        │ └── I²C ── ADS1015 ──×2 R-dividers ─────┤
                        │                                         │
                        └─────────── shared GND ─────── G-5500 pin 8
```

The planned circuit (canonical version in
[`design/circuit.py`](../design/circuit.py) / `circuit.svg`):

- **Direction**: four GPIO outputs each drive the base of a 2N3904 NPN
  through a **2.2 K** series resistor, with a **10 K** base-to-emitter
  pull-down. The 2.2 K gives ≈ 1.1 mA of base current at a 3.3 V GPIO,
  comfortably saturating the transistor for any reasonable sink current
  from the G-5500 input. The 10 K pull-down holds the base low while the
  Pico GPIO is high-impedance (boot or reset) but does not steal
  appreciable base current once the GPIO drives high. Collectors connect
  to G-5500 pins 2–5; emitters to common ground. When the GPIO goes high
  the transistor saturates and shorts its G-5500 pin to ground —
  electrically equivalent to pressing the corresponding direction button
  on the controller's front panel.

- **Feedback**: the Pico does **not** sample the G-5500 directly with its
  on-chip ADC. Instead an Adafruit **ADS1015** I²C ADC sits between the
  G-5500 and the Pico, with its A0 and A1 inputs fed from the G-5500's
  feedback pins through identical **68 K (upper) / 82 K (lower)**
  resistor-dividers.

  The sizing is driven by what the G-5500's service manual actually
  shows on the feedback path: an **NJM2902** quad op-amp scales the
  position-pot wiper voltage, then a **33 K series resistor** (R6010
  for azimuth, R6011 for elevation) sits between the op-amp output and
  the external DIN connector — almost certainly there as fault-current
  limiting at the external pin. The series 33 K is therefore part of
  our divider whether we like it or not, so the effective ratio at the
  ADS1015 is:

  ```
    V_ADC / V_opamp = R_lo / (R_source + R_up + R_lo)
                    = 82  / (33      + 68    + 82  )
                    ≈ 0.448
  ```

  At the G-5500's 4.5 V full-scale that gives **2.02 V** into the
  ADS1015 — 99% of the ±2.048 V FSR PGA setting, which keeps the LSB
  at ≈ 1 mV. The 2.0 V minimum maps to 0.90 V, so we get ~1120 useful
  codes across the working range: 0.16°/code on elevation and
  0.40°/code on azimuth.

  A **100 nF** cap in parallel with the 82 K resistor forms a ≈ 35 Hz
  low-pass with the resulting ~45 K Thévenin, rejecting 50/60 Hz pickup
  without costing tracking bandwidth (the rotator's mechanical bandwidth
  is well under 1 Hz). The G-5500 already has a 220 µH + 0.01 µF LC
  filter (L6004 / C6019 in the manual) on the same line at a much higher
  corner (≈ 107 kHz) for RFI, so the two filters complement each other.

  The Pico talks to the ADS1015 over I²C0 (GP4 = SDA, GP5 = SCL) with
  **4.7 K** pull-ups to 3.3 V. The ADS1015's ADDR pin is tied to GND,
  giving it I²C address 0x48. Per-unit calibration of the divider
  ratios is captured in software.

There is no speed control on this interface; control is bang-bang. The
firmware-side strategy is therefore a hysteretic / deadband controller:
read the current angle, compare with the target, energise the appropriate
direction transistor until the error is within tolerance, stop.

## Calibration

The position-feedback chain has *two* calibration stages — one inside
the G-5500, one inside ezal — and both matter.

### Stage 1: G-5500 hardware trim (one-off, on the bench)

Two trimpots inside the controller set the gain of the position-feedback
amplifier so each axis's mechanical end-stops produce the documented
2.0 V and 4.5 V at the external DIN feedback pins. The schematic calls
them:

- **VR0003** (elevation) with R0002 = 22 K
- **VR0004** (azimuth) with R0004 = 3.9 K

(The different fixed resistors compensate for the two axes' different
travel ranges — 180° vs 450° — so both end up in the same 2.0–4.5 V
window after scaling.)

They're accessible from the **rear panel** of the controller, labelled
**FULL SCALE ADJ**, above the corresponding antenna terminals. The
manual's procedure is:

- **Azimuth** — drive the rotator to its LEFT end-stop and mark its
  housing. Press RIGHT to slew a full turn back to the mark; the meter
  should read 360°. Adjust VR0004 until it does. Continue clockwise to
  the right end-stop; the meter should read 90° at the right edge of
  the scale (controller wraps at 360° + 90° = 450°).
- **Elevation** — drive the elevation rotator UP to its 180° mark; the
  meter should read 180° at the right edge of the scale. If not, adjust
  VR0003.

This is a one-time step. Once set, the trimpots stay put — the only
time to revisit them is if the front-panel meter visibly disagrees with
where the antenna is actually pointed.

### Stage 2: ezal software calibration (one-off, at first install)

Even with the G-5500 perfectly trimmed, ezal's own signal path has
sources of unknown offset and gain — divider resistor tolerance (~1 %),
ADS1015 PGA offset (~ 1 mV), op-amp aging in the G-5500 over time — so
we can't simply assume that an ADC reading of *X* counts means *Y*
degrees. ezal therefore captures its own ADC → angle calibration
against the *mechanical* end-stops, which are repeatable to within a
few arcminutes:

1. Slew azimuth to its CCW end-stop and record the ADC code (`az_min`).
2. Slew azimuth fully CW to its other end-stop and record `az_max`.
3. Same for elevation: `el_min` at the down end-stop, `el_max` at the
   up end-stop.
4. Persist all four to flash. From then on, angle = linear interpolation
   between the two stored endpoints.

This makes the system insensitive to G-5500 trimpot drift, divider
tolerance, ADC offset, and slow degradation of the rotator's position
pots over time. It is *not* a substitute for the G-5500 trim above,
though, because that trim is what keeps the ADC operating near full
scale — pinching the input range below ~70 % of FSR starts to eat into
our angular resolution.

## Motor switching budget

> **Rough notes.** Numbers below are paraphrased datasheet / typical
> figures, not bench-measured. Worth verifying once we have hardware
> wired up. Captured now so the order-of-magnitude doesn't get lost
> between here and the firmware controller (roadmap step 4).

### Three constraints

**Relay (PL6102 = Fujitsu FTR-B4 series, DC 12 V coil)**

- Operate / release time: ~5 ms each (≈ 10 ms round trip).
- Mechanical life: ~10⁸ operations.
- Electrical life at rated load: ~5 × 10⁵ operations.
- Datasheet switching ceiling: ~60 operations/min = **1 Hz** for the
  electrical-life figure. Above that, contact wear and weld risk climb
  faster than spec.

**Motor mechanical settling**

The G-5500 runs an AC induction motor, the G-5500DC a brushed DC motor;
both have a holding brake that engages when power is cut. Rough:

- Start-up (apply power → full slew speed): **100–200 ms**.
- Stop / brake (cut power → fully stopped): **100–300 ms**.
- Minimum useful on-pulse: ~300 ms. Below this the relay clicks but the
  rotator doesn't actually move — just judders.
- Minimum useful cycle period: ~500 ms = **2 Hz absolute mechanical
  ceiling**.

**Snubber thermals (D6038 / D6039 + C6057 = 4.7 nF / 1 kV)**

Sized for the relay's nominal duty. Not binding below ~1 Hz; listed for
completeness.

### Operating envelope

| condition                     | rate                          |
|-------------------------------|-------------------------------|
| absolute mechanical ceiling   | ~2 Hz (500 ms period)         |
| relay datasheet ceiling       | 1 Hz                          |
| **target operating maximum**  | **0.5 Hz (≥ 2 s period)**     |
| typical during a LEO pass     | 0.05 – 0.2 Hz                 |
| idle / no satellite           | 0                             |

### Implications for the controller

Three rules together hold the rate inside that envelope:

1. **Minimum on-time per pulse: ≥ 300 ms.** Below this the relay
   actuates without moving the antenna; pulse is pure wear, no work.
2. **Minimum off-time between consecutive commands: ≥ 2 s.** Hard
   guardrail on relay life. Caps switching at 0.5 Hz regardless of
   what the deadband logic decides.
3. **Deadband sized for the slew rate.** The G-5500 slews at ~6 °/s
   on both axes; a typical LEO satellite tracks at ≤ 1 °/s across the
   sky. A **3–5 ° deadband per axis** lands typical correction rates
   at ~0.1 Hz, well inside the budget, and is fine for any sensible
   antenna beam-width.

The min-off-time is the load-bearing rule. Deadband alone *should*
keep us slow, but noisy feedback could otherwise oscillate the rotator
at whatever rate the control loop ticks (which we'll want at 10+ Hz
for responsiveness). Decoupling the *decision* rate from the
*actuation* rate is the standard pattern and what we'll implement.

### To verify on the bench

- Actual motor start-up and brake-stop times for each axis (the 100–
  300 ms ranges above are typicals, not measured).
- Whether the Yaesu relay has any extra contact protection beyond the
  snubber we see, which might extend the electrical-life rating.
- Whether 3–5 ° deadband is acceptable for the antenna beam-widths
  we'll use, or if it needs to come down (and the switching budget
  needs to widen accordingly).

## Debug probe

The recommended probe is the [Raspberry Pi Debug Probe](https://www.raspberrypi.com/products/debug-probe/)
(it's a CMSIS-DAP device that probe-rs supports out of the box). Any
J-Link, Black Magic Probe, ST-Link with CMSIS-DAP firmware, or "Picoprobe"
(another Pico flashed with the debug firmware) also works.

Wiring is three lines: SWCLK, SWDIO, GND. The Pico 2 exposes them on
dedicated through-holes near the USB connector. Connect the probe's
target-power line too if you want the probe to power the Pico 2; otherwise
power the Pico over its USB port.

## Power

For development the Pico 2's USB-C port provides plenty. The G-5500
rotator and its control box have their own mains supply; the only
electrical connections between ezal and the rotator are the four
switch lines (pins 2–5) and the two feedback lines (pins 1, 6), all
referenced to G-5500 pin 8.
