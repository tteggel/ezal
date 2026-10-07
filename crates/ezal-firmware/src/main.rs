//! Manual rotator control, feedback acquisition and advisory pass prediction.
//! Firmware boots idle; only fresh operator input can request movement.

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
mod prediction;
mod state;
mod time;
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
use ezal_core::supervisor::FeedbackReadError;
use ezal_core::supervisor::ManualSupervisor;
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

/// Delay between acquisitions; telemetry has its own slower cadence.
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

/// Sample feedback and publish health plus the actuator interlock.
#[embassy_executor::task]
async fn feedback_task(mut adc: Ads1015<'static>, state: &'static SharedState) -> ! {
    let mut supervisor = ManualSupervisor::new(HARD_CODED_CALIBRATION);
    if let Some(error) = supervisor.calibration_error() {
        // Keep sampling anyway: the dashboard's raw millivolts are how an
        // operator measures the endpoints again. Motion stays inhibited.
        error!("feedback: calibration rejected: {=str}", error.as_str());
    }
    let read_timeout = Duration::from_millis(FEEDBACK_TIMEOUT_MS);
    let mut last_control_state = ControlState::FeedbackUnavailable;

    loop {
        // Timestamp before the oldest channel; include both reads in its age.
        let sample_started_ms = Instant::now().as_millis();
        let readings =
            read_feedback(&mut adc, read_timeout, supervisor.feedback_read_permitted()).await;
        let now_ms = Instant::now().as_millis();
        let update = supervisor.update(now_ms, sample_started_ms, readings);
        state.publish_feedback(update.raw_feedback, update.feedback, update.interlock);
        if update.state != last_control_state {
            info!("feedback: {=str}", update.state.as_str());
            last_control_state = update.state;
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

    spawner.spawn(feedback_task(adc, &state::STATE).expect("static task pool exhausted"));
    info!("ezal: manual control; outputs idle until operator input");

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
    // Eight web sockets, DHCP, DNS, NTP UDP and a catalog HTTP client, plus spare.
    static NET_RESOURCES: StaticCell<StackResources<14>> = StaticCell::new();
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
    spawner.spawn(time::synchronize(stack).expect("static task pool exhausted"));
    spawner.spawn(prediction::run(stack).expect("static task pool exhausted"));

    // Start acceptors while offline. They remain registered across reconnects,
    // so successful retries expose the dashboard without rebuilding the stack.
    info!("ezal web: starting listener on http://<dhcp-address>/");
    web::serve(spawner, stack).await;
}
