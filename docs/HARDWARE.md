# Hardware

This document describes the Pico 2 W, interface board, ADS1015 feedback path,
and Yaesu G-5500 that ezal is designed to drive.

## Pico 2 W and walking-skeleton modes

Board connector and pin details are in the
[Pico 2 W datasheet](https://datasheets.raspberrypi.com/picow/pico-2-w-datasheet.pdf).

The default firmware is an end-to-end hardware walking skeleton. It requires
the Pico 2 W, interface board, ADS1015, calibrated G-5500 feedback, and a
connected rotator. On boot it can raise the direction pins to acquire the
simulated METOP-C pass start:

```
┌──────────────────────────┐
│  Raspberry Pi Pico 2 W   │
│  (RP2350A, 4 MiB flash)  │
│                          │
│  USB ────────── host PC  │
│                          │
│  GP10 ─┐                 │
│  GP11 ─┼─▶ interface     │
│  GP20 ─┤   board (D1–D4) │
│  GP21 ─┘                 │
│                          │
│  SWCLK ──┐               │
│  SWDIO ──┼── debug probe │
│  GND   ──┘               │
└──────────────────────────┘
```

You need:

- one Raspberry Pi **Pico 2 W** (WiFi is required for the dashboard),
- a micro-USB cable for power and BOOTSEL flashing,
- *optionally* a Raspberry Pi Debug Probe (or any CMSIS-DAP probe) for
  flashing + log streaming with `probe-rs`. Without one you can still
  flash via the BOOTSEL UF2 mechanism, but you won't see the live RTT log,
- the [interface board](../design/circuit.svg), ADS1015, and G-5500.

### The board is the Pico 2 W

The firmware now brings up the Pico 2 W's onboard **CYW43439** radio at boot
and joins WiFi in station mode (see
[`wifi.rs`](../crates/ezal-firmware/src/wifi.rs) and
[WiFi credentials](#wifi-credentials) below), so the target is specifically
the **W** variant. On a plain (non-W) Pico 2 the WiFi POST has no radio to
talk to and the bring-up stalls there.

The CYW43439 hangs off four RP2350 GPIOs that are *not* brought out to the
module's header — they're dedicated to the radio — plus (on the W) the
onboard LED, which is wired to the CYW43439's own GPIO 0 rather than to
RP2350 GPIO 25. None of these collide with the direction, I²C, or debug pins:

| signal        | RP2350 GPIO | role                                  |
|---------------|------------:|---------------------------------------|
| WL_ON (`pwr`) |     23      | radio power-on / regulator enable     |
| gSPI CS       |     25      | chip-select                           |
| gSPI DIO      |     24      | bidirectional data                    |
| gSPI CLK      |     29      | clock                                 |

The CYW43439's bus is a "gSPI" link the RP2350 has no dedicated peripheral
for, so the firmware emulates it on a **PIO** state machine (`PIO0`) fed by
one DMA channel — that's what the `PIO0_IRQ_0` / `DMA_IRQ_0` bindings and the
`cyw43-pio` dependency are for.

### Pinout reference (for the curious)

| signal          | Pico 2 pin | RP2350 GPIO | notes                        |
|-----------------|-----------:|------------:|------------------------------|
| direction CW    |     14     |     10      | → G-5500 DIN 2, LED D1       |
| direction CCW   |     15     |     11      | → G-5500 DIN 4, LED D2       |
| direction up    |     26     |     20      | → G-5500 DIN 3, LED D3       |
| direction down  |     27     |     21      | → G-5500 DIN 5, LED D4       |
| CYW43 gSPI CS   |     —      |     25      | radio chip-select on the W — see above |
| SWCLK           |     —      |      —      | dedicated debug pin          |
| SWDIO           |     —      |      —      | dedicated debug pin          |
| GND             | many       |      —      |                              |

The four direction pins above are claimed by the firmware and can be driven by
the default walking skeleton; their G-5500 wiring is in
[`design/circuit.py`](../design/circuit.py) / `circuit.svg`. The I²C
feedback pins (GP4 = SDA, GP5 = SCL, to the ADS1015 at 0x48) are driven by
the firmware's ADS1015 driver
([`crates/ezal-firmware/src/ads1015.rs`](../crates/ezal-firmware/src/ads1015.rs)),
which self-tests (POSTs) the ADC at boot and samples both channels in default
and explicit `--no-default-features` manual hardware builds.

### WiFi credentials

The radio joins your access point in **station mode**. The Pico has no
filesystem to read credentials from at runtime, so they are baked into the
firmware image at *build* time: copy [`.env.example`](../.env.example) to
`.env` at the repo root and set `EZAL_WIFI_SSID` / `EZAL_WIFI_PASSWORD`.
`crates/ezal-firmware/build.rs` reads that file and the WiFi POST joins the
network at boot (an empty password selects an open network; protected joins
use WPA3/SAE). Credential validation runs after radio initialization and before
association. `.env` is git-ignored, but the credentials do end up in the flashed `.elf`/`.uf2`
in the clear — there's no secure element on the board.

The CYW43439 also needs three firmware blobs uploaded at every boot
(MAC firmware, regulatory table, board NVRAM). These are vendored in-tree
under [`crates/ezal-firmware/cyw43-firmware/`](../crates/ezal-firmware/cyw43-firmware/)
and `include_bytes!`d into the image, so there's nothing extra to flash — the
same single `.elf`/`.uf2` is self-contained. See that directory's README for
their provenance and licence.

## G-5500 rotator

The Yaesu G-5500 (and the -DC variant) is the target rotator. The nominal
feedback ranges and connector functions below follow the
[G-5500 instruction manual, page 5](https://www.yaesu.com/Files/4CB6273C-1018-01AF-FA4D504B591F641A/G-5500_IM_ENG_E12901004.pdf).
Its "external control" connector is an 8-pin DIN. The interface is **switch closures** for direction (not analog speed control;
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
| 7   | output    | Auxiliary supply, unused by ezal; consult your model manual |
| 8   | —         | Common ground                                                |

In ezal terms, the data flow is:

```
browser ──WiFi/WebSocket──▶ Pico 2 W ──×4 switches──▶ G-5500 pins 2–5
                              ▲                               │
                              └── I²C ── ADS1015 ──dividers──┤
                              └──── shared GND ──────── pin 8
```

The interface circuit (canonical version in
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

  Each GPIO also drives a **per-channel indicator LED** (D1–D4 in the
  schematic) through a **470 Ω** series resistor, tapped off the GPIO
  wire ahead of the base-resistor network. The LED is driven by the
  Pico directly, not by the transistor — so it lights up whenever
  the firmware *commands* that direction, regardless of whether the
  rotator is plugged in. Useful for bench-testing the controller and
  for eyeballing a noisy deadband loop. Drive current works out to ~2.8
  mA per LED at 3.3 V GPIO and a typical 2 V Vf, plus ~1.1 mA into the
  base network = ~4 mA per channel — within the Pico's 12 mA per-pin
  GPIO budget.

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

  This nominal divider model maps a 2.0–4.5 V source to approximately
  **0.90–2.02 V** at the ADC. Firmware uses **±4.096 V full scale**, or
  **2 mV per ADC code**: about 560 codes across that nominal span. This
  corresponds to approximately 0.32° per code over 180° elevation or 0.80°
  per code over 450° azimuth. Full-scale selection and code size follow
  [ADS1015 datasheet Table 7-1](https://www.ti.com/lit/ds/symlink/ads1015.pdf).

  Runtime calibration uses measured ADC millivolts, rather than those
  nominal ranges. The checked-in installation maps azimuth **114–2033 mV
  to 0–360°**, and elevation **1281–58 mV to 0–90°**. Its resolution is
  approximately **0.375° per code in azimuth** and **0.147° per code in
  elevation**. The reversed elevation voltage scale is intentional.

  The wider ADC range leaves room around the measured 2033 mV endpoint
  for the **25 mV endpoint margin** and observable overrange readings.
  The old ±2.048 V range saturated at 2047 mV, inside that accepted margin.
  Both supervisors validate that the entire calibration band sits above zero
  and below **3200 mV**, the lower of the ±4.096 V saturation code (4094 mV)
  and the 3.3 V supply-rail ceiling. The ceiling matters because the ADS1015
  cannot report a reading above its own supply: a band reaching that high
  would make an input stuck at the rail indistinguishable from a valid
  endpoint. Zero, negative, rail, and saturated samples are rejected before
  endpoint clamping. The ±4.096 V PGA setting does not increase the chip's
  permitted input voltage above its supply; retain the divider and 3.3 V
  input-supply constraints.

  There is no filter capacitor across the 82 K resistor: the dividers
  feed the ADS1015 inputs directly. The only analog filtering on the
  line is inside the G-5500 — a 220 µH + 0.01 µF LC (L6004 / C6019 in
  the manual) with a ≈ 107 kHz corner, there for RFI.

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

Check the front-panel angle indications using the G-5500 manual's
pre-installation adjustment procedure. The rear **FULL SCALE ADJ** controls
align the meter scales: azimuth at the marked full-turn position and
elevation at its 180° markers. Consult the procedure for the particular
controller model before adjusting it.

These meter checks do not establish ezal's ADC endpoints. Measure the
complete connected feedback chain separately; the installed values below
differ from the nominal DIN voltage description.

### Stage 2: ezal software calibration (one-off, at first install)

Record two known angles per axis and the corresponding dashboard **ADC
millivolts**, including both angle endpoints and both voltage endpoints in
[`HARD_CODED_CALIBRATION`](../crates/ezal-core/src/position.rs). The current
installation covers 0–360° azimuth and 0–90° elevation; those limits are not
interchangeable with the G-5500's full 450°/180° travel.

Use the G-5500's own controls to obtain initial measurements when ezal's
feedback interlock blocks browser movement. Manual firmware still publishes
raw readings when calibration rejects a sample, and keeps sampling even when
the calibration itself is rejected — it reports `calibration-invalid` and
inhibits motion, so the endpoints can be measured again. It has no
feedback-interlock bypass, and a cancelled I²C read requires a reset before
more reads or motion.

The linear mapping accounts for offset, voltage direction, and gain at the
time of measurement. It cannot compensate for later drift or nonlinear
feedback; repeat measurements when the hardware changes or independent
angle checks disagree. Keep the full 25 mV endpoint tolerance inside the
ADC's unsaturated range and below the supply-rail ceiling; a calibration that
overlaps either is rejected.

The complete safety setup, channel worksheet, source location, edit example,
and verification steps are in [CALIBRATION.md](CALIBRATION.md). Flash-backed
calibration storage is not implemented yet; the checked-in constants are the
source of truth for this walking skeleton.

## Motor switching budget

> **Rough notes.** Numbers below are paraphrased datasheet / typical
> figures, not bench-measured operating guarantees. The implemented firmware
> limits are listed separately below; verify motor and relay behavior on the
> installed hardware.

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

The implemented rules are in
[`drive.rs`](../crates/ezal-core/src/drive.rs) and
[`control.rs`](../crates/ezal-core/src/control.rs):

1. **Ordinary minimum on-time: 500 ms per axis.** Routine target release
   respects this hold. Feedback faults, endpoint inhibition, browser stops,
   releasing or reversing a held browser control, ownership changes, and
   movement-lease expiry clear the relevant output immediately when serviced,
   even inside the hold. Releasing one of two held buttons therefore stops
   that axis at once and leaves the other one running.
2. **Minimum off-time: 2 s per axis.** Reversal and reactivation wait for
   the inactive interval, including after a safety inhibition. At the ordinary
   500 ms minimum on-time, a complete on/off cycle is at least 2.5 s.
3. **Controller thresholds: engage at 4°, release at 1.5°.** Both modes
   sample feedback approximately every 100 ms and reject a pair that took
   longer than 150 ms to read, while relay transitions are governed
   independently. These thresholds require validation with the installed
   antenna and load.

Every active command has a **750 ms movement lease**, and an output starts
only when its lease covers the 500 ms minimum on-time — a stalled command
stream leaves the relay alone instead of producing a click too short to move
the rotator. The actuator also checks that the oldest feedback channel is at
most **500 ms** old and that movement does not continue into a calibrated
endpoint: outward motion stops one ADC code before it, widened by the travel
the rotator could have made since that sample began, and an energised axis is
cut at that computed instant rather than at the next sample. Its next wake-up
is the earliest of those deadlines or a fixed 100 ms service pass. These are
software deadlines; motor coasting and physical relay release add mechanical
stopping time beyond them.

A **500 ms hardware watchdog** covers executor stalls, disabled interrupts,
and panics after the actuator task has started. Only that task feeds it,
after processing safety policy and applying GPIOs. The RP2350 watchdog reset
includes SIO and both CPUs, taking the pins out of the stalled executor's
control, and the board's 10 K base pull-downs then keep the switches off.
That hand-off may not be instantaneous: RP2350 pads have isolation latches
that can hold a pin's last output state through a reset until boot
reinitialises the pad, so measure the pin *during* the reset window rather
than only after the reboot (see [DEVELOPMENT.md](DEVELOPMENT.md)). Startup
checks for a watchdog timeout this firmware armed and leaves outputs low
without restarting a pass; a UF2 or `picotool` reboot, which also restarts the
chip through the watchdog, is not mistaken for one. A board reset, power
cycle, or debugger warm reset clears that reason. The watchdog relies on the
chip's clock and reset hardware; this board has no separate external
motor-enable interlock. See the
[RP2350 reset and watchdog registers](https://datasheets.raspberrypi.com/rp2350/rp2350-datasheet.pdf).

### To verify on the bench

- Actual motor start-up and brake-stop times for each axis (the 100–
  300 ms ranges above are typicals, not measured).
- Whether the Yaesu relay has any extra contact protection beyond the
  snubber we see, which might extend the electrical-life rating.
- Whether the 4° engage / 1.5° release thresholds suit the antenna beam-widths
  and measured slew rates. The G-5500 manual gives approximately 58 s per
  360° azimuth and 67 s per 180° elevation at 60 Hz; measure the installed
  system before changing the controller settings.
- Whether an energised direction pin stays high through a watchdog reset.
  Capture the pin across the reset itself, not just after the reboot: the pad
  isolation latch can hold the last output state until boot claims the pin.
- Fault-injection checks: invalid feedback, interrupted command refresh,
  executor stall, watchdog reset, and confirmation that no pass restarts
  until an explicit reset.

## Debug probe

The recommended probe is the [Raspberry Pi Debug Probe](https://www.raspberrypi.com/products/debug-probe/)
(it's a CMSIS-DAP device that probe-rs supports out of the box). Any
J-Link, Black Magic Probe, ST-Link with CMSIS-DAP firmware, or "Picoprobe"
(another Pico flashed with the debug firmware) also works.

Wiring is three lines: SWCLK, SWDIO, GND. The Pico 2 exposes them on
dedicated debug pads. Power the Pico separately through its micro-USB port;
the Raspberry Pi Debug Probe's three-wire SWD connector provides signals and
ground, not target power. Other probes have their own voltage-reference and
power requirements; follow their wiring documentation. See the
[Debug Probe documentation](https://www.raspberrypi.com/documentation/microcontrollers/debug-probe.html).

## Power

For development, power the Pico 2 W through its micro-USB port. The G-5500
rotator and its control box have their own mains supply; the only
electrical connections between ezal and the rotator are the four
switch lines (pins 2–5) and the two feedback lines (pins 1, 6), all
referenced to G-5500 pin 8.
