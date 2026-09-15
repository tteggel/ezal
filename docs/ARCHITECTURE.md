# Architecture

ezal separates deterministic tracking logic from the RP2350 peripherals that
feed and apply it. The default build is a hardware-driving walking skeleton:
only the deterministic METOP-C target source is synthetic. The ADS1015,
calibration, controller, direction GPIOs, and installed rotator all participate.

## Runtime data flow

```text
 walking skeleton (default)

 METOP-C pass scheduler ── target az/el ─┐
                                         ▼
 G-5500 ─ ADS1015 mV ─ calibrated az/el ─ controller ─ lease/timing ─ GPIO
    ▲                                                                  │
    └────────────────────── interface direction inputs ◀───────────────┘

 manual hardware (--no-default-features)

 dashboard manual command ─ command lease ─ relay timing ─ GPIO ─ G-5500
                                                            │
 dashboard ◀── calibrated az/el ◀── A0/A1 mV ◀── ADS1015 ◀──┘
```

Both modes POST the ADS1015 before WiFi association or DHCP. Default mode then
begins autonomous acquisition; manual mode waits for leased dashboard input.

Before every synthetic pass, the sequencer commands its first horizon target.
The dish must remain within 4° on each axis for one second before the 180-second
pass clock begins. A 120-second acquisition timeout stops and latches the
sequence in fault. A pass peaks at 82° elevation, returns to the horizon, then
supplies no target for exactly 30 seconds. Successive passes reverse azimuth
direction. This is a representative METOP-C-labelled profile, not orbital
propagation from a TLE.

## Workspace boundary

`crates/ezal-core` is `no_std`, deterministic, and hardware-independent:

- `position` owns angle units, installed-system calibration, and both sides of
  the degrees ↔ ADC-millivolts mapping;
- `simulation` owns the pass schedule and acquire → track → pause sequencer;
- `control` owns input validation, freshness watchdogs, mechanical limits, and
  hysteretic bang-bang decisions;
- `drive` owns the command vocabulary, 750 ms movement lease, 500 ms minimum
  active time, and 2 s minimum inactive time;
- `protocol` and `dashboard` expose raw and calibrated telemetry;
- `ads1015` and `wifi` contain pure register/credential logic.

`crates/ezal-firmware` is the thin Embassy/HAL shell:

- `main` selects autonomous walking-skeleton or manual control and runs the
  periodic feedback/control tasks;
- `drive` is the only owner of the four direction GPIOs and applies the core
  actuator guard at the final output boundary;
- `ads1015` and `wifi` perform hardware transactions and power-on tests;
- `state` coordinates tasks with atomics and a command signal;
- `web` serves the dashboard and WebSocket telemetry.

The production target transport or TLE source will yield timestamped
`TimedPointing` values to the existing controller. It does not need to know how
feedback is measured or commands are applied.

## Closed-loop tick

The hardware walking skeleton runs this sequence every 100 ms:

1. Read A0/A1 millivolts from the real ADS1015.
2. Convert those readings to calibrated mechanical az/el.
3. Acquire and settle at the pass start, or sample the moving pass target (no
   target is supplied during the pause).
4. Validate target and feedback values and timestamps.
5. Run the hysteretic controller and publish a leased drive/stop command.
6. Publish coherent tracking telemetry for the dashboard.

Feedback keeps the timestamp from before the first channel read, and the
controller checks its age against the clock after both reads. The pair has a
500 ms read timeout; a timeout requests stop, reports `feedback-stale`, and
disables further ADC reads until reset because the cancelled I²C transfer may
be unfinished. Stale feedback cannot satisfy the acquisition settle gate.

The output task independently processes that command through lease expiry and
relay timing. If the control task stops refreshing, movement is removed even
without an explicit stop.

## Safety layers

| layer | invariant |
|-------|-----------|
| acquisition gate | pass clock starts only after a one-second settle; failure within 120 s stops in fault |
| calibrated feedback | non-finite/out-of-envelope positions and voltages beyond the 25 mV endpoint margin are rejected |
| target/feedback watchdog | data older than 500 ms (or timestamped in the future) requests stop |
| mechanical envelope | scheduler and controller use the installed calibration's angle endpoints |
| hysteresis | axes engage at 4° error and release at 1.5°, avoiding noisy chatter |
| movement lease | active commands expire after 750 ms without refresh |
| relay timing | outputs stay on for at least 500 ms and off for at least 2 s before reactivation/reversal |
| pin ownership | one Embassy task owns all direction pins and clears them before its loop starts |

No software layer replaces correct mechanical end stops, an accessible motor
power disconnect, safe cable routing, or the calibration procedure.

## Shared state and control ownership

The tracking task is the only writer of autonomous telemetry. A small sequence
lock lets WebSocket readers obtain one coherent pass/target/position snapshot
without a heap or async mutex. The GPIO task records its logical applied drive
separately.

Each dashboard position card shows the applied drive direction or `IDLE`,
including in autonomous mode. These indicators use the GPIO task's control
telemetry and show `unknown` when the WebSocket disconnects.

When the autonomous walking skeleton is active, browser clients cannot acquire
manual control. In the explicit manual hardware build, the first dashboard
client may control the direction buttons; disconnect, loss of control
ownership, malformed commands, and lease timeout all request stop.

## Verification

Host tests cover calibration endpoints and round trips, channel ordering,
controller hysteresis and every fail-safe input class, movement lease expiry,
relay timing, pass/pause boundaries, acquisition settling and timeout,
protocol JSON, and dashboard wiring. Firmware verification cross-compiles the
walking-skeleton and manual hardware feature sets for
`thumbv8m.main-none-eabihf`.

Hardware-in-the-loop verification is still required before production use:
measure real endpoint voltages, replace the placeholder calibration, confirm
direction polarity, inject ADC/link faults, and measure motor coast/brake
behaviour. See [CALIBRATION.md](CALIBRATION.md) and
[HARDWARE.md](HARDWARE.md).
