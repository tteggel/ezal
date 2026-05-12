# Architecture

This document explains *how* and *why* the project is structured the way
it is. It is intended for someone who has read the README and wants to
understand the trade-offs before adding code.

## High level

```
┌─────────────────────────────────────────────────────────────────────┐
│                            host PC                                  │
│  ┌──────────────────┐   USB-serial   ┌──────────────────────────┐   │
│  │  ground control  │ ─────────────▶ │   ezal-firmware (Pico 2) │   │
│  │  (orbit propag.) │   az/el        │                          │   │
│  └──────────────────┘                └─────────────┬────────────┘   │
│                                                    │   ▲           │
│                                  ×4 switch-closure │   │ ×2 ADC    │
│                                                    ▼   │ feedback  │
│                                          ┌──────────────────┐       │
│                                          │  Yaesu G-5500    │       │
│                                          │  az/el rotator   │       │
│                                          └──────────────────┘       │
└─────────────────────────────────────────────────────────────────────┘
```

The firmware is the bridge between *digital* az/el commands from a host
computer and the G-5500's *switch-closure* direction inputs, reading the
controller's *analog* position feedback to close the loop. It is
responsible for:

- accepting az/el targets over USB-serial (or, later, UART),
- driving the G-5500's four direction inputs through transistor switches,
- reading az/el feedback voltages on two ADC channels,
- running a small bang-bang position controller with deadband so the
  motors stop when the dish is close enough to the target,
- reporting status back to the host.

For step one we are doing none of that. The firmware just blinks the
onboard LED, but the architectural shape of the project is already in
place — `main` is an async function on the Embassy executor, the morse
encoder is in a separately-testable crate, and timing comes from
`embassy_time` rather than busy-loops. Every later feature slots in as
another async task.

## Workspace layout

The project is a Cargo workspace with two crates:

```
crates/
├── ezal-core/        ← pure logic, no_std, host-testable
└── ezal-firmware/    ← embedded application
```

This split is the single most important architectural decision in the
project. It is a long-standing best practice in embedded firmware. The
trade-offs are:

| concern                  | ezal-core                | ezal-firmware             |
|--------------------------|--------------------------|---------------------------|
| `std`                    | no (except under `cfg(test)`) | no                   |
| allocator                | no                       | no                        |
| async                    | yes (when needed)        | yes (Embassy)             |
| HAL deps                 | **none**                 | embassy-rp                |
| testable on host         | **yes**                  | no                        |
| testable on hardware     | yes (via firmware)       | yes                       |
| flashed onto the chip    | yes (as a dep)           | yes                       |

The rule of thumb: **if you can write it without thinking about a register
or a pin, it goes in `ezal-core`**. That covers morse encoding today, and
will cover pointing math, command parsing, slew planning, and the G-5500
protocol later.

The firmware crate stays a thin shell that wires pure logic to peripherals.

## Why Embassy?

There are three viable async/concurrency frameworks for the RP2350:

1. **Bare-metal** + `cortex-m-rt` + `embedded-hal`: maximum transparency,
   minimum framework, but lots of boilerplate for any non-trivial
   timing / concurrency.
2. **RTIC** (Real-Time Interrupt-driven Concurrency): static analysis of
   resource priorities, very predictable, but the programming model is
   priority-based and feels less natural for I/O-heavy code.
3. **Embassy**: cooperative `async` runtime; tasks `.await` on timers and
   peripherals; the executor sleeps the core between events. The
   programming model is the same `async` Rust you'd write on a host.

ezal will be I/O-bound: serial in, four GPIO direction outputs, two ADC
reads, no hard-realtime sub-microsecond deadlines. The Embassy model —
`Timer::after`, `UartRx::read_until_idle`, `Adc::read`, etc. — fits
cleanly and produces obvious code. That is the *whole reason* this
architecture works for a multi-feature tracker without devolving into a
state-machine spaghetti.

If a future feature needs sub-millisecond determinism we can pin one
core to a hard real-time loop and keep the other on Embassy.

## Why pin everything (toolchain, deps, profile)?

Embedded development punishes "version drift" more than most domains
because the same crash on a new compiler can be silent (corrupted RAM)
rather than a panic. Pinning:

- **rustup** channel to a specific version (in `rust-toolchain.toml`),
- every dependency version in `Cargo.toml` `[workspace.dependencies]`,
- `Cargo.lock` checked in,

means everyone — including CI and you, six months from now — builds the
same bytes. Bumping any of these is a deliberate, reviewable change.

## Future modules

The workspace shape is designed so new modules can be dropped in without
restructuring — but we'll add them when they have concrete code to hold,
not pre-emptively. Things on the horizon:

- a host-testable angle / unit-conversion module in `ezal-core`,
- a hysteretic position controller in `ezal-core`,
- firmware tasks for ADC sampling and direction-switch control, wired
  to those `ezal-core` modules via plain function calls.

Exact filenames and module boundaries will surface when we get there.

## Testing strategy

Three levels:

1. **Host unit tests** — `ezal-core` is plain Rust, so we use the regular
   test harness. CI runs these on Linux. This is where the majority of
   the test surface lives; it catches encoding/parsing/math bugs cheaply.

2. **Cross-compile** — CI builds `ezal-firmware` for the Pico 2 target
   and runs clippy on it. This catches type-level regressions in code
   that touches the HAL without needing real hardware.

3. **Hardware-in-the-loop** — *future*: a small smoke-test binary that
   asserts (e.g.) "after sending a known TLE the LED hits a known
   az/el position within X seconds". Runs on a dev kit attached to a
   self-hosted CI runner.

For now we only have layers 1 and 2.

## Logging and panics

We use [`defmt`](https://defmt.ferrous-systems.com/) for logging and
[`panic-probe`](https://github.com/knurling-rs/probe-run) for panics.
Both stream over RTT (Real-Time Transfer): a small ring buffer in the
chip's SRAM that the debug probe reads in real time. Two consequences:

- Log strings live in the *elf*, not in the *flashed image*; only their
  integer indices end up on the chip. This makes logging essentially
  free in flash space and very fast at runtime.
- You only see logs while a debugger probe is attached. For untethered
  debugging (e.g. in the field) we'd add a separate USB-serial logger
  later.

## What is deliberately *not* done

To keep the project tractable, the following are explicitly out of scope
for the early phases:

- TLE propagation on the chip (the host does the orbit math).
- Web UI / WiFi (the Pico 2 W variant is supported but not the target;
  we use the wired Pico 2).
- OTA firmware updates.
- Closed-loop control with encoder feedback finer than the G-5500's
  built-in pot.

These can come back when there's a real user demand.
