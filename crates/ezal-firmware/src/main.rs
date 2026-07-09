//! # ezal-firmware — `wifi + adc bring-up`
//!
//! The project's bring-up firmware. At boot it runs two power-on self-tests
//! (POSTs) in turn, each a hard gate — on failure it reports the reason and
//! idles rather than limping on:
//!
//!  1. **WiFi** — powers the Pico 2 W's CYW43439 radio and joins, in station
//!     (STA) mode, the access point whose SSID/password were baked in from
//!     `.env` at build time (see `build.rs` and `wifi.rs`).
//!  2. **ADS1015** — initialises the I²C bus to the position-feedback ADC and
//!     round-trips its registers.
//!
//! If both pass it then periodically reads the two G-5500 feedback channels
//! (A0, A1) and logs their voltages, while the CYW43439 driver task keeps the
//! WiFi link up concurrently in the background. The four direction GPIOs are
//! still claimed and held low, so nothing moves: this is a *sensing* bring-up,
//! the counterpart to the earlier direction-sweep that proved the *drive*
//! side.
//!
//! Why this exists:
//!
//!  * It proves the I²C wiring end-to-end — bus pull-ups, the ADS1015's
//!    address strap, and the SDA/SCL pin mux — before any control logic
//!    depends on the readings.
//!  * The POST distinguishes "a device ACKs at 0x48" from "a working ADS1015
//!    that stores configuration and returns conversions", so a mis-wired or
//!    wrong part fails loudly at boot instead of silently feeding garbage
//!    into the eventual position loop.
//!  * The periodic readout lets you turn the rotator by hand (or with its own
//!    controller) and watch the divided feedback voltage track on the probe —
//!    the raw material for the software calibration in `docs/HARDWARE.md`.
//!
//! The eventual G-5500 rotator firmware will replace this `main` with a task
//! graph (USB-serial in, four direction-switch GPIOs, the ADS1015 feedback
//! reads below, a hysteretic position controller, …) but every line here
//! should still be familiar.
//!
//! ## How it runs
//!
//!   1. `cortex-m-rt` provides the reset handler and ISR vector table.
//!   2. The RP2350 boot ROM finds our `ImageDef` block at the start of flash
//!      and jumps to the reset handler.
//!   3. `cortex-m-rt`'s startup code zeroes BSS, copies `.data` from flash to
//!      RAM, sets up the stack, and calls into Rust.
//!   4. `#[embassy_executor::main]` builds a single-thread async executor and
//!      runs our `main` future on it.
//!   5. `main` initialises the HAL, claims the direction pins (held low),
//!      POSTs the WiFi (join) and then the ADS1015, and finally loops reading
//!      the feedback channels while the radio task runs concurrently.
//!
//! ## How to flash
//!
//! ```text
//! cargo run -p ezal-firmware --release         # via probe-rs (SWD)
//! ```
//!
//! Or, if you have no debug probe, build a UF2 and drag-and-drop it onto the
//! Pico 2's BOOTSEL drive:
//!
//! ```text
//! ./scripts/build-uf2.sh                       # → ezal-firmware.uf2
//! # hold BOOTSEL, plug in, copy the .uf2 onto the "RP2350" drive
//! ```
//!
//! See [`docs/DEVELOPMENT.md`](../../../../docs/DEVELOPMENT.md) for setup.

// ─── crate attributes ───────────────────────────────────────────────────
// `no_std`  : we have no operating system and no Rust std library.
// `no_main` : we provide our own entry point via cortex-m-rt's runtime
//             (which #[embassy_executor::main] wraps).
#![no_std]
#![no_main]

// ─── modules ────────────────────────────────────────────────────────────
mod ads1015;
mod wifi;

// ─── imports ────────────────────────────────────────────────────────────
use defmt::{error, info, warn};
use embassy_executor::Spawner;
use embassy_rp::bind_interrupts;
use embassy_rp::dma;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::i2c::{self, I2c, InterruptHandler as I2cInterruptHandler};
use embassy_rp::peripherals::{DMA_CH0, I2C0, PIO0};
use embassy_rp::pio::{InterruptHandler as PioInterruptHandler, Pio};
use embassy_time::{Duration, Timer};

use ads1015::Ads1015;
use ezal_core::ads1015::Mux;
use ezal_core::wifi::Credentials;
use wifi::Wifi;

// `use foo as _` keeps the crate linked in even though we never name any of
// its items. These two carry essential runtime plumbing:
//
//   defmt_rtt   — installs a global `defmt::Logger` that forwards log frames
//                 over RTT (Real-Time Transfer). probe-rs reads those frames
//                 and prints them on the host.
//   panic_probe — installs the panic handler. On panic, it formats the panic
//                 message via defmt and then halts the core, so a panic shows
//                 up on the host as a clear log line rather than a silent
//                 reboot.
use defmt_rtt as _;
use panic_probe as _;

// ─── RP2350 image header ────────────────────────────────────────────────
//
// The RP2350 boot ROM does not trust arbitrary code; it walks the start of
// flash looking for a *block loop* that contains a valid "image definition"
// block. If it doesn't find one, the chip enters BOOTSEL mode and refuses to
// run our binary. This is the RP2350 equivalent of the RP2040's "boot2"
// 2nd-stage bootloader — but simpler, because the RP2350 has hardware XIP
// support built in. We literally just say "yes, please run me".
//
// We don't need to write the static manually: `embassy-rp` does it for us
// when the `imagedef-secure-exe` feature is enabled (see the workspace
// `Cargo.toml`). It places a `static IMAGE_DEF: ImageDef` in the
// `.start_block` linker section. Crucially, cortex-m-rt's `link.x` does *not*
// place `.start_block` — our `memory.x` does, pinning it into the first 4 KiB
// of flash where the boot ROM scans for it. (Without that the section lands
// at the end of the image and the chip won't boot — see the long note in
// `memory.x`.) If you ever need a *non-secure* or *signed* image, swap the
// feature flag for `imagedef-nonsecure-exe` (or disable embassy's auto-insert
// with `imagedef-none` and provide your own static).
//
// You can confirm it's there with:
//     llvm-objdump -t target/.../ezal-firmware | grep IMAGE_DEF

// ─── interrupts ─────────────────────────────────────────────────────────
//
// Each async driver completes its transfers from an interrupt, so we bind the
// vectors they use to embassy-rp's handlers. `bind_interrupts!` generates the
// single `Irqs` type we hand to each `::new` below:
//
//   I2C0_IRQ    — the ADS1015 I²C bus.
//   PIO0_IRQ_0  — the CYW43439's gSPI, emulated on PIO0 (see wifi.rs).
//   DMA_IRQ_0   — the DMA channel that PIO gSPI streams through.
bind_interrupts!(struct Irqs {
    I2C0_IRQ => I2cInterruptHandler<I2C0>;
    PIO0_IRQ_0 => PioInterruptHandler<PIO0>;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>;
});

// ─── tunables ───────────────────────────────────────────────────────────

/// Station-mode WiFi credentials, baked in from `.env` at build time by
/// `build.rs` (there's no filesystem on the Pico to read them at runtime).
/// They're empty until you copy `.env.example` to `.env` and fill it in; the
/// WiFi POST validates them and fails loudly at boot if they're unusable.
const WIFI_SSID: &str = env!("EZAL_WIFI_SSID");
const WIFI_PASSWORD: &str = env!("EZAL_WIFI_PASSWORD");

/// How often to sample and log the feedback channels once POST has passed.
/// The rotator's mechanical bandwidth is well under 1 Hz, so 2 Hz here is
/// plenty for watching a hand-turned axis on the probe.
const SAMPLE_PERIOD: Duration = Duration::from_millis(500);

// ─── entry point ────────────────────────────────────────────────────────

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // `embassy_rp::init` consumes the global Peripherals singleton and hands
    // us a struct of every chip resource (`p.PIN_10`, `p.I2C0`, …). After
    // this point, peripheral ownership is tracked by Rust's normal move
    // semantics — you cannot accidentally talk to a pin from two places.
    let p = embassy_rp::init(Default::default());

    // Direction-switch outputs, held low so we never assert a direction (no
    // motion at boot, and none during this sensing bring-up). They're still
    // claimed here so the pins are owned and the 2N3904 switches stay off; the
    // walking direction *sweep* that proved the drive side now lives in git
    // history. Ordered to match the schematic:
    //
    //   GP10 → DIN 2  CW  (right)   D1
    //   GP11 → DIN 4  CCW (left)    D2
    //   GP20 → DIN 3  UP            D3
    //   GP21 → DIN 5  DOWN          D4
    let _directions = [
        Output::new(p.PIN_10, Level::Low),
        Output::new(p.PIN_11, Level::Low),
        Output::new(p.PIN_20, Level::Low),
        Output::new(p.PIN_21, Level::Low),
    ];

    // ─── WiFi: bring up the CYW43439 and join the AP (station mode) ───
    // The Pico 2 W's on-module radio. `Wifi::init` powers it and spawns its
    // driver task; `wifi.post` joins the access point whose SSID/password were
    // baked in from `.env` at build time. GP23/24/25/29, PIO0 and one DMA
    // channel are wired to the CYW43439 on the module and clash with nothing
    // else here. main owns `Irqs`, so it builds the two peripherals that
    // consume interrupts — the PIO block and the DMA channel — and hands them
    // over; the four radio pins go with them.
    let creds = Credentials {
        ssid: WIFI_SSID,
        password: WIFI_PASSWORD,
    };
    let pio = Pio::new(p.PIO0, Irqs);
    let dma = dma::Channel::new(p.DMA_CH0, Irqs);
    let mut wifi = Wifi::init(spawner, pio, dma, p.PIN_23, p.PIN_25, p.PIN_24, p.PIN_29).await;

    info!(
        "ezal wifi-bringup: joining \"{=str}\" in STA mode",
        WIFI_SSID
    );
    match wifi.post(&creds).await {
        Ok(r) => info!(
            "WiFi POST OK: joined \"{=str}\" ({=str})",
            WIFI_SSID,
            if r.protected { "secured" } else { "open" }
        ),
        Err(e) => {
            // A failed join means the link is untrustworthy, so — as with the
            // ADS1015 below — we do not fall through. Report and idle; the
            // operator fixes `.env` (or the AP) and re-flashes.
            error!("WiFi POST FAILED: {}", e);
            loop {
                Timer::after(Duration::from_secs(1)).await;
            }
        }
    }

    // I²C0 to the ADS1015: GP5 = SCL, GP4 = SDA (the schematic's fixed
    // feedback pins), with 4.7 K pull-ups to 3.3 V on the interface board.
    // Default config is 100 kHz standard mode, which the ADS1015 handles
    // comfortably.
    let i2c = I2c::new_async(p.I2C0, p.PIN_5, p.PIN_4, Irqs, i2c::Config::default());
    let mut adc = Ads1015::new(i2c, ezal_core::ads1015::I2C_ADDR);

    info!(
        "ezal adc-bringup: POSTing ADS1015 at 0x{=u8:x}",
        ezal_core::ads1015::I2C_ADDR
    );
    match adc.post().await {
        Ok(r) => info!(
            "ADS1015 POST OK: config=0x{=u16:x}, A0={=i32} mV, A1={=i32} mV",
            r.config, r.a0_mv, r.a1_mv
        ),
        Err(e) => {
            // A failed POST means the feedback path is untrustworthy, so we do
            // not fall through into the readout loop. Report it and idle; the
            // operator fixes the wiring and re-flashes.
            error!("ADS1015 POST FAILED: {}", e);
            loop {
                Timer::after(Duration::from_secs(1)).await;
            }
        }
    }

    // POST passed. Periodically read both feedback channels and log the
    // divided voltages. `Timer::after(..).await` is non-blocking: while we
    // wait the executor is free to run other tasks (it has none yet, but the
    // shape is what the rotator controller will need).
    loop {
        match (
            adc.read_channel_mv(Mux::Ain0).await,
            adc.read_channel_mv(Mux::Ain1).await,
        ) {
            (Ok(a0_mv), Ok(a1_mv)) => {
                info!("feedback: A0={=i32} mV, A1={=i32} mV", a0_mv, a1_mv)
            }
            _ => warn!("feedback: I²C read failed"),
        }
        Timer::after(SAMPLE_PERIOD).await;
    }
}
