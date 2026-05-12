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

The [Yaesu G-5500](https://www.yaesu.com/) (and the newer G-5500DC) is the
target rotator. Pertinent facts:

- It has **two motors**: azimuth (continuous 0–450°) and elevation (0–180°).
- The control box accepts two **analog control voltages** (0–5 V each) to
  request a target azimuth and elevation, and provides two **analog
  feedback voltages** (also 0–5 V) for the current position.
- An "external control" connector exposes both pairs; this is what we
  drive and read.

In ezal terms, the data flow will be:

```
host PC  ──USB-serial──▶  Pico 2  ──DAC/PWM──▶  G-5500 input
                            ▲                       │
                            └────ADC───── G-5500 feedback
```

### Analog drive

The Pico 2 has no real DAC. Two options:

1. **PWM + RC filter.** RP2350's PWM hardware can produce a low-pass-
   filterable rectangular wave at any duty cycle. A simple RC stage turns
   that into a smooth voltage. Cheap, no extra parts beyond two resistors
   and two capacitors, but limited bandwidth and resolution.
2. **External DAC.** An MCP4822 or DAC8552 over SPI gives 12–16 bits of
   resolution with no filtering needed. More parts on the board, but
   cleaner output.

We will start with PWM + RC because the rotator's mechanical bandwidth is
well under 10 Hz; the filter cutoff can be set very low and PWM ripple
becomes invisible.

### Voltage levels

The Pico 2's GPIO is 3.3 V. The G-5500 expects 0–5 V. A **level shifter**
or a **non-inverting op-amp** with gain ≈ 1.5 brings 3.3 V up to ~5 V.
For ADC input we go the other way and divide 5 V down with a resistor
pair to stay inside the ADC's range.

These details belong on the bench, not in the firmware; the firmware
treats the analog outputs as "0.0 to 1.0" and lets the calibration table
map that to whatever physical voltage the hardware actually produces.

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
rotator and its control box have their own mains supply; the *only*
electrical connection between ezal and the rotator is the analog
control/feedback wiring.

## Workshop note

Always power the rotator before connecting the analog control lines, and
**never** apply more than 5 V to the G-5500 inputs. The G-5500's internal
op-amps are tolerant but not unkillable.
