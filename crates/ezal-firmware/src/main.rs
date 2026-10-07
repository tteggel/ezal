//! # ezal-firmware — autonomous tracking walking skeleton
//!
//! The default `simulator` feature begins acquisition for a synthetic METOP-C
//! pass after the ADS1015 power-on test and network readiness. It reads the installed rotator through A0
//! and A1, acquires and settles at the pass start, then follows the target with
//! the real direction GPIOs through the fail-safe controller and actuator
//! guard. This is intentionally a hardware-driving walking skeleton.
//!
//! Network loss inhibits both manual and autonomous motion. Association and
//! DHCP retry with backoff; recovery begins a fresh acquisition of the current
//! pass, while previously latched hardware/acquisition faults remain latched.
//! After network bring-up the dashboard reports target, calibrated position,
//! raw feedback-domain values, pass/pause countdown, controller state, and the
//! guarded logical output. Each pass has a 180-second target duration, followed
//! by a 30-second no-target pause before acquiring the next reversed pass.
//! Phase changes occur on the first supervisor tick at or after each deadline.
//!
//! Building with `--no-default-features` selects manual hardware/calibration
//! mode with the same ADC deadline, feedback interlock, and watchdog protection.
//! A watchdog timeout leaves outputs inhibited on the next boot until an
//! explicit reset. Flashing tools also restart the chip through the watchdog,
//! so boot only inhibits motion after a timeout this firmware armed. See
//! `docs/CALIBRATION.md` before driving a rotator.
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
//!   5. `main` claims idle direction pins, starts the actuator and selected
//!      feedback/control graph, then brings up WiFi, TCP/IP, and the dashboard.
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
#![recursion_limit = "256"]

// ─── modules ────────────────────────────────────────────────────────────
mod ads1015;
mod drive;
mod state;
mod watchdog;
mod web;
mod wifi;

// ─── imports ────────────────────────────────────────────────────────────
use cyw43::NetDriver;
use defmt::{error, info};
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_rp::bind_interrupts;
use embassy_rp::clocks::RoscRng;
use embassy_rp::dma;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::i2c::InterruptHandler as I2cInterruptHandler;
use embassy_rp::i2c::{self, I2c};
use embassy_rp::peripherals::{DMA_CH0, I2C0, PIO0};
use embassy_rp::pio::{InterruptHandler as PioInterruptHandler, Pio};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use static_cell::StaticCell;

use ads1015::Ads1015;
use ezal_core::ads1015::Mux;
use ezal_core::control::{ControlState, FEEDBACK_PERIOD_MS, FEEDBACK_TIMEOUT_MS};
use ezal_core::position::{FeedbackVoltages, HARD_CODED_CALIBRATION};
use ezal_core::protocol::PositionTelemetry;
use ezal_core::simulation::{PassPhase, SIMULATED_SATELLITE};
use ezal_core::supervisor::FeedbackReadError;
#[cfg(not(feature = "simulator"))]
use ezal_core::supervisor::ManualSupervisor;
#[cfg(feature = "simulator")]
use ezal_core::supervisor::TrackingSupervisor;
use ezal_core::wifi::Credentials;
use state::SharedState;
use wifi::Wifi;

// `use foo as _` keeps the crate linked in even though we never name any of
// its items. These two carry essential runtime plumbing:
//
//   defmt_rtt   — installs a global `defmt::Logger` that forwards log frames
//                 over RTT (Real-Time Transfer). probe-rs reads those frames
//                 and prints them on the host.
//   panic_probe — logs and halts on panic. Once the output task has armed the
//                 hardware watchdog, a halt causes a reset within 500 ms.
//                 Boot then inhibits motion until an operator resets the board.
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

/// Sample both modes at the core's feedback period. Manual mode samples just
/// as fast as autonomous tracking: the actuator's interlock needs a sample
/// newer than its validity window before it will admit any browser jog, and a
/// front-panel move must be visible to the endpoint checks just as quickly.
/// Dashboard publication has its own slower cadence.
const FEEDBACK_PERIOD: Duration = Duration::from_millis(FEEDBACK_PERIOD_MS);

/// Bound the entire ADC self-test, including its presence and register checks.
/// A stuck bus can suspend an async transaction without stalling the actuator
/// task, so the actuator's watchdog alone cannot detect this startup failure.
const ADC_POST_TIMEOUT: Duration = Duration::from_millis(500);

/// Read both physical feedback channels under one deadline. A supervisor
/// refuses to reuse a bus after a cancelled transaction until hardware reset.
async fn read_feedback(
    adc: &mut Ads1015<'static>,
    timeout: Duration,
    read_permitted: bool,
) -> Result<FeedbackVoltages, FeedbackReadError> {
    if !read_permitted {
        return Err(FeedbackReadError::TimedOut);
    }
    match with_timeout(timeout, async {
        let a0_elevation_mv = adc.read_channel_mv(Mux::Ain0).await?;
        let a1_azimuth_mv = adc.read_channel_mv(Mux::Ain1).await?;
        Ok::<_, i2c::Error>(FeedbackVoltages {
            a0_elevation_mv,
            a1_azimuth_mv,
        })
    })
    .await
    {
        Ok(Ok(readings)) => Ok(readings),
        Ok(Err(_)) => Err(FeedbackReadError::Unavailable),
        Err(_) => {
            error!("feedback: ADC read timed out; reset required");
            Err(FeedbackReadError::TimedOut)
        }
    }
}

/// The mode's supervision policy: a synthetic pass sequencer driving the real
/// rotator, or manual feedback health for browser-owned motion. Both validate
/// the same calibration, deadlines, and faults.
#[cfg(feature = "simulator")]
type Supervisor = TrackingSupervisor;
#[cfg(not(feature = "simulator"))]
type Supervisor = ManualSupervisor;

#[cfg(feature = "simulator")]
fn new_supervisor(now_ms: u64) -> Supervisor {
    TrackingSupervisor::new(HARD_CODED_CALIBRATION, now_ms)
}

#[cfg(not(feature = "simulator"))]
fn new_supervisor(_now_ms: u64) -> Supervisor {
    ManualSupervisor::new(HARD_CODED_CALIBRATION)
}

/// Sample feedback, publish telemetry and the actuator's interlock, and submit
/// whatever command the mode's supervision produced.
///
/// One loop serves both modes: only the supervisor differs, so the deadlines,
/// fault precedence, and publication path cannot drift apart between them.
#[embassy_executor::task]
async fn feedback_task(mut adc: Ads1015<'static>, state: &'static SharedState) -> ! {
    let mut supervisor = new_supervisor(Instant::now().as_millis());
    if let Some(error) = supervisor.calibration_error() {
        // Keep sampling anyway: the dashboard's raw millivolts are how an
        // operator measures the endpoints again. Motion stays inhibited.
        error!("feedback: calibration rejected: {=str}", error.as_str());
    }
    let read_timeout = Duration::from_millis(FEEDBACK_TIMEOUT_MS);
    let mut last_phase = PassPhase::Pause;
    let mut last_control_state = ControlState::FeedbackUnavailable;
    #[cfg(feature = "simulator")]
    let mut network_generation = 0;

    loop {
        // Timestamp before the oldest channel; include both reads in its age.
        let sample_started_ms = Instant::now().as_millis();
        let readings =
            read_feedback(&mut adc, read_timeout, supervisor.feedback_read_permitted()).await;
        let now_ms = Instant::now().as_millis();
        #[cfg(feature = "simulator")]
        {
            let network = state.network();
            if !network.ready(now_ms) || network_generation != network.generation() {
                // Do not spend acquisition/settling time while output is
                // inhibited by networking. The recovery generation catches
                // even an outage that starts and ends during the ADC read.
                // This preserves ADC cancellation and acquisition fault latches.
                supervisor.restart_tracking(now_ms);
                network_generation = network.generation();
            }
        }
        let update = supervisor.update(now_ms, sample_started_ms, readings);
        state.publish_feedback(update.raw_feedback, update.tracking, update.interlock);
        if let Some(command) = update.command {
            state.submit_command(command);
        }

        if update.tracking.phase != last_phase {
            info!(
                "tracking: {=str} {=str} {=u64}, {=u64} ms remaining",
                SIMULATED_SATELLITE,
                update.tracking.phase.as_str(),
                u64::from(update.tracking.pass_index) + 1,
                update.tracking.phase_remaining_ms
            );
            last_phase = update.tracking.phase;
        }
        if update.decision.state != last_control_state {
            info!("feedback: {=str}", update.decision.state.as_str());
            last_control_state = update.decision.state;
        }

        Timer::after(FEEDBACK_PERIOD).await;
    }
}

/// Drives the Embassy TCP/IP stack.
#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, NetDriver<'static>>) -> ! {
    runner.run().await
}

// ─── entry point ────────────────────────────────────────────────────────

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // `embassy_rp::init` consumes the global Peripherals singleton and hands
    // us a struct of every chip resource (`p.PIN_10`, `p.I2C0`, …). After
    // this point, peripheral ownership is tracked by Rust's normal move
    // semantics — you cannot accidentally talk to a pin from two places.
    let p = embassy_rp::init(Default::default());

    // Direction-switch outputs, initially low and ordered to match the
    // schematic:
    //
    //   GP10 → DIN 2  CW  (right)   D1
    //   GP11 → DIN 4  CCW (left)    D2
    //   GP20 → DIN 3  UP            D3
    //   GP21 → DIN 5  DOWN          D4
    let directions = drive::DirectionOutputs::new(
        Output::new(p.PIN_10, Level::Low),
        Output::new(p.PIN_11, Level::Low),
        Output::new(p.PIN_20, Level::Low),
        Output::new(p.PIN_21, Level::Low),
    );
    let mut watchdog = embassy_rp::watchdog::Watchdog::new(p.WATCHDOG);
    if watchdog::firmware_timeout_caused_reset(&mut watchdog) {
        // Do not restart an autonomous pass after a panic or executor stall.
        // Keep the owned outputs low until a hardware reset clears the reason.
        watchdog.stop();
        error!("watchdog timeout: motion inhibited; inspect the fault, then reset the board");
        loop {
            Timer::after(Duration::from_secs(1)).await;
        }
    }
    spawner.spawn(
        drive::direction_task(directions, &state::STATE, watchdog)
            .expect("static task pool exhausted"),
    );

    // I²C0 to the ADS1015: GP5 = SCL, GP4 = SDA (the schematic's fixed
    // feedback pins), with 4.7 K pull-ups to 3.3 V on the interface board.
    // POST before WiFi so feedback diagnostics remain available during retry.
    // The actuator's network gate keeps every direction low until readiness.
    let i2c = I2c::new_async(p.I2C0, p.PIN_5, p.PIN_4, Irqs, i2c::Config::default());
    let mut adc = Ads1015::new(i2c, ezal_core::ads1015::I2C_ADDR);

    info!(
        "ezal adc-bringup: POSTing ADS1015 at 0x{=u8:x}",
        ezal_core::ads1015::I2C_ADDR
    );
    match with_timeout(ADC_POST_TIMEOUT, adc.post()).await {
        Ok(Ok(r)) => {
            state::STATE.store_position(PositionTelemetry {
                a0_mv: r.a0_mv,
                a1_mv: r.a1_mv,
            });
            info!(
                "ADS1015 POST OK: config=0x{=u16:x}, A0={=i32} mV, A1={=i32} mV",
                r.config, r.a0_mv, r.a1_mv
            );
        }
        Ok(Err(e)) => {
            error!("ADS1015 POST FAILED: {}", e);
            loop {
                Timer::after(Duration::from_secs(1)).await;
            }
        }
        Err(_) => {
            // Cancellation can leave a HAL transaction partially completed.
            // Keep the bus owned but never use it again before a hardware
            // reset. No supervisor has started, so outputs remain inhibited.
            error!("ADS1015 POST timed out; motion inhibited; reset required");
            loop {
                Timer::after(Duration::from_secs(1)).await;
            }
        }
    }

    #[cfg(feature = "simulator")]
    state::STATE.set_autonomous(true);
    spawner.spawn(feedback_task(adc, &state::STATE).expect("static task pool exhausted"));
    if cfg!(feature = "simulator") {
        info!("ezal: autonomous walking skeleton; waiting for network permission");
    } else {
        info!("ezal: manual hardware mode");
    }

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
    let (net_device, wifi) =
        Wifi::init(spawner, pio, dma, p.PIN_23, p.PIN_25, p.PIN_24, p.PIN_29).await;

    // ─── TCP/IP: run DHCP over the CYW43439 data plane ──────────────────
    static NET_RESOURCES: StaticCell<StackResources<10>> = StaticCell::new();
    let net_config = embassy_net::Config::dhcpv4(Default::default());
    let mut rng = RoscRng;
    let seed = rng.next_u64();
    let (stack, runner) = embassy_net::new(
        net_device,
        net_config,
        NET_RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.spawn(net_task(runner).expect("static task pool exhausted"));

    spawner.spawn(
        wifi::supervise(wifi, stack, creds, &state::STATE).expect("static task pool exhausted"),
    );

    // Start acceptors while offline. They remain registered across reconnects,
    // so successful retries expose the dashboard without rebuilding the stack.
    info!("ezal web: starting listener on http://<dhcp-address>/");
    web::serve(spawner, stack).await;
}
