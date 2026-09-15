//! # ezal-firmware — autonomous tracking walking skeleton
//!
//! The default `simulator` feature starts a synthetic METOP-C pass immediately
//! after the ADS1015 power-on test. It reads the installed rotator through A0
//! and A1, acquires and settles at the pass start, then follows the target with
//! the real direction GPIOs through the fail-safe controller and actuator
//! guard. This is intentionally a hardware-driving walking skeleton.
//!
//! After network bring-up the dashboard reports target, calibrated position,
//! raw feedback-domain values, pass/pause countdown, controller state, and the
//! guarded logical output. Each 180-second pass is followed by an exact
//! 30-second no-target pause before the next reversed pass.
//!
//! Building with `--no-default-features` selects manual hardware/calibration
//! mode. See `docs/CALIBRATION.md` before driving a rotator.
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
mod web;
mod wifi;

// ─── imports ────────────────────────────────────────────────────────────
use cyw43::NetDriver;
#[cfg(not(feature = "simulator"))]
use defmt::warn;
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
#[cfg(feature = "simulator")]
use embassy_time::Instant;
use embassy_time::{with_timeout, Duration, Timer};
use static_cell::StaticCell;

use ads1015::Ads1015;
use ezal_core::ads1015::Mux;
use ezal_core::control::ControlState;
#[cfg(feature = "simulator")]
use ezal_core::control::{ControlConfig, ControlDecision, TimedPointing, TrackingController};
use ezal_core::position::{FeedbackVoltages, HARD_CODED_CALIBRATION};
use ezal_core::protocol::{PositionTelemetry, TrackingMode, TrackingTelemetry};
use ezal_core::simulation::PassPhase;
#[cfg(feature = "simulator")]
use ezal_core::simulation::{MetopPassScheduler, TrackingSequence};
use ezal_core::wifi::Credentials;
use state::SharedState;
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

/// How often to sample and publish the feedback channels once POST has passed.
/// The rotator's mechanical bandwidth is well under 1 Hz, so 2 Hz here is
/// plenty for the dashboard.
#[cfg(not(feature = "simulator"))]
const SAMPLE_PERIOD: Duration = Duration::from_millis(ezal_core::protocol::TELEMETRY_PERIOD_MS);

/// Closed-loop cadence for autonomous tracking. This is comfortably faster
/// than the rotator mechanics while refreshing the actuator lease with margin.
#[cfg(feature = "simulator")]
const CONTROL_PERIOD: Duration = Duration::from_millis(100);

/// Maximum time to wait for DHCP before failing visibly instead of masking the
/// rest of bring-up behind an unbounded network wait.
const DHCP_TIMEOUT_SECS: u64 = 30;
const DHCP_TIMEOUT: Duration = Duration::from_secs(DHCP_TIMEOUT_SECS);

/// Owns the ADS1015 and publishes the latest raw millivolt readings.
#[cfg(not(feature = "simulator"))]
#[embassy_executor::task]
async fn feedback_task(mut adc: Ads1015<'static>, state: &'static SharedState) -> ! {
    loop {
        match (
            adc.read_channel_mv(Mux::Ain0).await,
            adc.read_channel_mv(Mux::Ain1).await,
        ) {
            (Ok(a0_mv), Ok(a1_mv)) => {
                state.store_position(PositionTelemetry { a0_mv, a1_mv });
                match HARD_CODED_CALIBRATION.feedback_to_position(FeedbackVoltages {
                    a0_elevation_mv: a0_mv,
                    a1_azimuth_mv: a1_mv,
                }) {
                    Ok(position) => state.store_tracking(TrackingTelemetry {
                        mode: TrackingMode::Manual,
                        pass_index: 0,
                        phase: PassPhase::Pause,
                        phase_remaining_ms: 0,
                        azimuth_tenths: degrees_to_tenths(position.azimuth_deg),
                        elevation_tenths: degrees_to_tenths(position.elevation_deg),
                        target_azimuth_tenths: None,
                        target_elevation_tenths: None,
                        control_state: ControlState::Idle,
                    }),
                    Err(_) => {
                        warn!(
                            "feedback calibration rejected A0={=i32} mV, A1={=i32} mV",
                            a0_mv, a1_mv
                        );
                        store_feedback_fault(state, ControlState::FeedbackInvalid);
                    }
                }
            }
            _ => {
                warn!("feedback: I²C read failed");
                store_feedback_fault(state, ControlState::FeedbackUnavailable);
            }
        }

        Timer::after(SAMPLE_PERIOD).await;
    }
}

#[cfg(not(feature = "simulator"))]
fn store_feedback_fault(state: &'static SharedState, control_state: ControlState) {
    let previous = state.tracking();
    state.store_tracking(TrackingTelemetry {
        mode: TrackingMode::Manual,
        pass_index: previous.pass_index,
        phase: PassPhase::Pause,
        phase_remaining_ms: 0,
        azimuth_tenths: previous.azimuth_tenths,
        elevation_tenths: previous.elevation_tenths,
        target_azimuth_tenths: None,
        target_elevation_tenths: None,
        control_state,
    });
}

/// Run the hardware-driving autonomous walking skeleton.
///
/// Only the METOP-C target source is synthetic. Feedback comes from the real
/// ADS1015, and commands go through the same leased GPIO task used by manual
/// hardware mode.
#[cfg(feature = "simulator")]
#[embassy_executor::task]
async fn hardware_tracking_task(mut adc: Ads1015<'static>, state: &'static SharedState) -> ! {
    let scheduler = match MetopPassScheduler::for_calibration(HARD_CODED_CALIBRATION) {
        Ok(scheduler) => scheduler,
        Err(_) => {
            error!("tracking: invalid hard-coded calibration");
            state.apply_command(ezal_core::drive::Command::Stop);
            loop {
                Timer::after(Duration::from_secs(1)).await;
            }
        }
    };
    let boot_ms = Instant::now().as_millis();
    let mut sequence = TrackingSequence::new(scheduler, boot_ms);
    let control_config = ControlConfig::for_calibration(HARD_CODED_CALIBRATION);
    let mut controller = TrackingController::new(control_config);
    let feedback_timeout = Duration::from_millis(control_config.feedback_timeout_ms);
    let mut feedback_timed_out = false;
    let mut last_phase = PassPhase::Pause;
    let mut last_control_state = ControlState::Idle;

    loop {
        // Conservatively timestamp the pair before starting its oldest channel.
        // The decision clock below must include time spent waiting for either read.
        let feedback_started_ms = Instant::now().as_millis();
        let readings = if feedback_timed_out {
            Err(ControlState::FeedbackStale)
        } else {
            match with_timeout(feedback_timeout, async {
                let a0_mv = adc.read_channel_mv(Mux::Ain0).await?;
                let a1_mv = adc.read_channel_mv(Mux::Ain1).await?;
                Ok::<_, i2c::Error>((a0_mv, a1_mv))
            })
            .await
            {
                Ok(Ok(readings)) => Ok(readings),
                Ok(Err(_)) => Err(ControlState::FeedbackUnavailable),
                Err(_) => {
                    // Cancelling an I²C transfer can leave the peripheral mid-
                    // transaction. Keep motion inhibited until reset rather than
                    // reuse that bus and risk accepting a partial conversion.
                    feedback_timed_out = true;
                    error!("tracking: ADC read timed out; reset required");
                    Err(ControlState::FeedbackStale)
                }
            }
        };
        let now_ms = Instant::now().as_millis();
        let (feedback, feedback_fault) = match readings {
            Ok((a0_mv, a1_mv)) => {
                state.store_position(PositionTelemetry { a0_mv, a1_mv });
                match HARD_CODED_CALIBRATION.feedback_to_position(FeedbackVoltages {
                    a0_elevation_mv: a0_mv,
                    a1_azimuth_mv: a1_mv,
                }) {
                    Ok(position) => (
                        Some(TimedPointing::new(position, feedback_started_ms)),
                        None,
                    ),
                    Err(_) => (None, Some(ControlState::FeedbackInvalid)),
                }
            }
            Err(fault) => (None, Some(fault)),
        };

        // Stale samples must not count toward acquisition settling, even if a
        // delayed read completes just as its timeout becomes ready.
        let position = feedback
            .filter(|feedback| now_ms - feedback.timestamp_ms <= control_config.feedback_timeout_ms)
            .map(|feedback| feedback.position);
        let pass = sequence.update(now_ms, position);
        let decision = tracking_decision(&mut controller, now_ms, pass, feedback, feedback_fault);
        let previous = state.tracking();
        let reported = position.unwrap_or(ezal_core::position::Pointing::new(
            previous.azimuth_tenths as f32 / 10.0,
            previous.elevation_tenths as f32 / 10.0,
        ));

        store_autonomous_telemetry(
            state,
            TrackingMode::HardwareWalkingSkeleton,
            pass,
            reported,
            decision,
        );
        state.apply_command(decision.command);

        if pass.phase != last_phase {
            info!(
                "tracking: METOP-C {=str} {=u32}, {=u64} ms remaining",
                pass.phase.as_str(),
                pass.pass_index + 1,
                pass.phase_remaining_ms
            );
            last_phase = pass.phase;
        }
        if decision.state != last_control_state {
            info!("tracking: controller {=str}", decision.state.as_str());
            last_control_state = decision.state;
        }

        Timer::after(CONTROL_PERIOD).await;
    }
}

#[cfg(feature = "simulator")]
fn tracking_decision(
    controller: &mut TrackingController,
    now_ms: u64,
    pass: ezal_core::simulation::PassSample,
    feedback: Option<TimedPointing>,
    feedback_fault: Option<ControlState>,
) -> ControlDecision {
    if pass.phase == PassPhase::Fault {
        return ControlDecision {
            command: ezal_core::drive::Command::Stop,
            state: ControlState::AcquisitionTimeout,
        };
    }

    if let Some(feedback_fault) = feedback_fault {
        let _ = controller.update(
            now_ms,
            pass.target.map(|target| TimedPointing::new(target, now_ms)),
            None,
        );
        return ControlDecision {
            command: ezal_core::drive::Command::Stop,
            state: feedback_fault,
        };
    }

    controller.update(
        now_ms,
        pass.target.map(|target| TimedPointing::new(target, now_ms)),
        feedback,
    )
}

#[cfg(feature = "simulator")]
fn store_autonomous_telemetry(
    state: &'static SharedState,
    mode: TrackingMode,
    pass: ezal_core::simulation::PassSample,
    position: ezal_core::position::Pointing,
    decision: ControlDecision,
) {
    state.store_tracking(TrackingTelemetry {
        mode,
        pass_index: pass.pass_index,
        phase: pass.phase,
        phase_remaining_ms: pass.phase_remaining_ms,
        azimuth_tenths: degrees_to_tenths(position.azimuth_deg),
        elevation_tenths: degrees_to_tenths(position.elevation_deg),
        target_azimuth_tenths: pass
            .target
            .map(|target| degrees_to_tenths(target.azimuth_deg)),
        target_elevation_tenths: pass
            .target
            .map(|target| degrees_to_tenths(target.elevation_deg)),
        control_state: decision.state,
    });
}

fn degrees_to_tenths(degrees: f32) -> i32 {
    (degrees * 10.0 + 0.5) as i32
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
    spawner.must_spawn(drive::direction_task(directions, &state::STATE));

    // I²C0 to the ADS1015: GP5 = SCL, GP4 = SDA (the schematic's fixed
    // feedback pins), with 4.7 K pull-ups to 3.3 V on the interface board.
    // POST before WiFi so acquisition is not delayed by network bring-up.
    let i2c = I2c::new_async(p.I2C0, p.PIN_5, p.PIN_4, Irqs, i2c::Config::default());
    let mut adc = Ads1015::new(i2c, ezal_core::ads1015::I2C_ADDR);

    info!(
        "ezal adc-bringup: POSTing ADS1015 at 0x{=u8:x}",
        ezal_core::ads1015::I2C_ADDR
    );
    match adc.post().await {
        Ok(r) => {
            state::STATE.store_position(PositionTelemetry {
                a0_mv: r.a0_mv,
                a1_mv: r.a1_mv,
            });
            info!(
                "ADS1015 POST OK: config=0x{=u16:x}, A0={=i32} mV, A1={=i32} mV",
                r.config, r.a0_mv, r.a1_mv
            );
        }
        Err(e) => {
            error!("ADS1015 POST FAILED: {}", e);
            loop {
                Timer::after(Duration::from_secs(1)).await;
            }
        }
    }

    #[cfg(feature = "simulator")]
    {
        state::STATE.set_autonomous(true);
        spawner.must_spawn(hardware_tracking_task(adc, &state::STATE));
        info!("ezal: autonomous walking skeleton; hardware drive enabled");
    }

    #[cfg(not(feature = "simulator"))]
    {
        spawner.must_spawn(feedback_task(adc, &state::STATE));
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

    // ─── TCP/IP: run DHCP over the CYW43439 data plane ──────────────────
    static NET_RESOURCES: StaticCell<StackResources<10>> = StaticCell::new();
    let net_config = embassy_net::Config::dhcpv4(Default::default());
    let mut rng = RoscRng;
    let seed = rng.next_u64();
    let (stack, runner) = embassy_net::new(
        wifi.into_net_device(),
        net_config,
        NET_RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.must_spawn(net_task(runner));

    info!("ezal net: waiting for DHCP");
    match with_timeout(DHCP_TIMEOUT, stack.wait_config_up()).await {
        Ok(()) => info!("ezal net: DHCP OK"),
        Err(_) => {
            error!("ezal net: DHCP timed out after {=u64}s", DHCP_TIMEOUT_SECS);
            loop {
                Timer::after(Duration::from_secs(1)).await;
            }
        }
    }

    info!("ezal web: listening on http://<dhcp-address>/");
    web::serve(spawner, stack).await;
}
