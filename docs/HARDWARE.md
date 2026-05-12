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
