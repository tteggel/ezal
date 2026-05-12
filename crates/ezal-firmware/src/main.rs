//! # ezal-firmware — `hello-morse`
//!
//! The very first firmware in the ezal project. It blinks **HELLO WORLD**
//! in international Morse code on the Pico 2's onboard LED (GPIO 25).
//!
//! Why this exists:
//!
//!  * It proves end-to-end that the toolchain, build script, linker
//!    layout, RP2350 image header, and probe-rs runner are all working.
//!  * It exercises the `ezal-core` morse encoder on real hardware, so we
//!    know the host-side unit tests reflect actual chip behaviour.
//!  * It gives every new contributor a "hello world" that they can flash
//!    in under a minute and *see* succeed without any external wiring.
//!
//! The eventual G5500-rotator firmware will replace this `main` with a
//! task graph (USB-serial in, az/el smoothing, two PWM outputs, …) but
//! every line below should still be familiar.
//!
//! ## How it runs
//!
//!   1. `cortex-m-rt` provides the reset handler and ISR vector table.
//!   2. The RP2350 boot ROM finds our `ImageDef` block at the start of
//!      flash and jumps to the reset handler.
//!   3. `cortex-m-rt`'s startup code zeroes BSS, copies `.data` from
//!      flash to RAM, sets up the stack, and calls into Rust.
//!   4. `#[embassy_executor::main]` builds a single-thread async executor
//!      and runs our `main` future on it.
//!   5. `main` initialises the HAL, takes ownership of `PIN_25`, and
//!      enters the morse loop forever.
//!
//! ## How to flash
//!
//! ```text
//! cargo run -p ezal-firmware --release         # via probe-rs (SWD)
//! ```
//!
//! Or, if you have no debug probe:
//!
//! ```text
//! cargo build -p ezal-firmware --release
//! picotool load -uvx -t elf \
//!     target/thumbv8m.main-none-eabihf/release/ezal-firmware
//! ```
//!
//! See [`docs/DEVELOPMENT.md`](../../../../docs/DEVELOPMENT.md) for setup.

// ─── crate attributes ───────────────────────────────────────────────────
// `no_std`  : we have no operating system and no Rust std library.
// `no_main` : we provide our own entry point via cortex-m-rt's runtime
//             (which #[embassy_executor::main] wraps).
#![no_std]
#![no_main]

// ─── imports ────────────────────────────────────────────────────────────
use defmt::info;
use embassy_executor::Spawner;
use embassy_rp::gpio::{Level, Output};
use embassy_time::{Duration, Timer};

// `use foo as _` keeps the crate linked in even though we never name any
// of its items. These two carry essential runtime plumbing:
//
//   defmt_rtt   — installs a global `defmt::Logger` that forwards log
//                 frames over RTT (Real-Time Transfer). probe-rs reads
//                 those frames and prints them on the host.
//   panic_probe — installs the panic handler. On panic, it formats the
//                 panic message via defmt and then halts the core, so a
//                 panic shows up on the host as a clear log line rather
//                 than a silent reboot.
use defmt_rtt as _;
use panic_probe as _;

use ezal_core::morse;

// ─── RP2350 image header ────────────────────────────────────────────────
//
// The RP2350 boot ROM does not trust arbitrary code; it walks the start of
// flash looking for a *block loop* that contains a valid "image definition"
// block. If it doesn't find one, the chip enters BOOTSEL mode and refuses
// to run our binary. This is the RP2350 equivalent of the RP2040's "boot2"
// 2nd-stage bootloader — but simpler, because the RP2350 has hardware XIP
// support built in. We literally just say "yes, please run me".
//
// We don't need to write the static manually: `embassy-rp` does it for us
// when the `imagedef-secure-exe` feature is enabled (see the workspace
// `Cargo.toml`). It places a `pub static IMAGE_DEF: ImageDef` in the
// `.start_block` linker section, which cortex-m-rt's link script puts at
// the very start of flash. If you ever need a *non-secure* or *signed*
// image, swap the feature flag for `imagedef-nonsecure-exe` (or disable
// embassy's auto-insert with `imagedef-none` and provide your own static).
//
// You can confirm it's there with:
//     llvm-objdump -t target/.../ezal-firmware | grep IMAGE_DEF

// ─── tunables ───────────────────────────────────────────────────────────

/// Length of one Morse "dit" (the base time unit), in milliseconds.
///
/// At 150 ms a dit is comfortable to follow visually; "HELLO WORLD" then
/// takes ~16 seconds end to end. Drop to 80 ms for a sharper rhythm, raise
/// to 250 ms for absolute beginners. Real CW operators routinely run at
/// 60 ms or faster.
const UNIT_MS: u64 = 150;

/// Pause between consecutive transmissions, so the loop is easy to follow.
const INTER_MESSAGE_PAUSE: Duration = Duration::from_secs(2);

/// The phrase we transmit. Kept in flash via the `&'static str`.
const MESSAGE: &str = "HELLO WORLD";

// ─── entry point ────────────────────────────────────────────────────────

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    // `embassy_rp::init` consumes the global Peripherals singleton and
    // hands us a struct of every chip resource (`p.PIN_25`, `p.UART0`,
    // `p.PWM_SLICE0`, …). After this point, peripheral ownership is
    // tracked by Rust's normal move semantics — you cannot accidentally
    // talk to PIN_25 from two places.
    let p = embassy_rp::init(Default::default());

    // The Pico 2 (non-W variant) wires the onboard LED to GPIO 25.
    //
    // **Caveat for Pico 2 W users**: on the *wireless* variant the LED is
    // connected to the wireless co-processor (CYW43439), not directly to
    // GPIO 25. This firmware will not light that LED. Bringing up CYW43
    // is a separate task and lives behind a future `pico2w` feature.
    let mut led = Output::new(p.PIN_25, Level::Low);

    info!("ezal hello-morse: starting (dit = {} ms)", UNIT_MS);

    loop {
        info!("Transmitting: {}", MESSAGE);

        // Walk the morse iterator; for each pulse, set the LED to the
        // requested state and yield to the executor for the right number
        // of milliseconds. `Timer::after(..).await` is non-blocking:
        // while we wait, the executor is free to run other tasks (it
        // currently has nothing else to do, but the structure is what
        // matters — it's the same shape we'll need for the rotator).
        for pulse in morse::pulses(MESSAGE) {
            let level = if pulse.on { Level::High } else { Level::Low };
            led.set_level(level);
            Timer::after(Duration::from_millis(UNIT_MS * u64::from(pulse.units))).await;
        }

        // Always finish with the LED off, then pause before repeating so
        // a viewer can clearly see the message boundary.
        led.set_low();
        Timer::after(INTER_MESSAGE_PAUSE).await;
    }
}
