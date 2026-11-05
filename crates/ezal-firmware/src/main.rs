//! Leased manual control, device-owned tracking and background pass prediction.
//! Firmware boots idle; browser-independent execution needs an explicitly armed pass.

#![no_std]
#![no_main]
// Computing the layout of the web task's future, whose type nests picoserve's
// router, exceeds rustc's default query depth of 128.
#![recursion_limit = "256"]

// ─── modules ────────────────────────────────────────────────────────────
mod ads1015;
mod drive;
mod feedback;
mod prediction;
mod state;
mod time;
mod tracking;
mod watchdog;
mod web;
mod wifi;
mod ws_receive;

// ─── imports ────────────────────────────────────────────────────────────
use cyw43::NetDriver;
use defmt::{error, info};
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_rp::bind_interrupts;
use embassy_rp::clocks::RoscRng;
use embassy_rp::dma;
use embassy_rp::i2c::InterruptHandler as I2cInterruptHandler;
use embassy_rp::i2c::{self, I2c};
use embassy_rp::peripherals::{DMA_CH0, I2C0, PIO0};
use embassy_rp::pio::{InterruptHandler as PioInterruptHandler, Pio};
use embassy_time::{with_timeout, Duration};
use static_cell::StaticCell;

use ads1015::Ads1015;
use ezal_core::position::FeedbackVoltages;
use ezal_core::wifi::Credentials;
use wifi::Wifi;

// Panic halts feeding the actuator-owned watchdog. A resulting watchdog reset
// boots with motion inhibited until an explicit board reset.
use defmt_rtt as _;
use panic_probe as _;

// The `imagedef-secure-exe` Embassy feature supplies the RP2350 image header;
// memory.x places it where the boot ROM looks.

// Each async peripheral receives the interrupt binding owned by main.
bind_interrupts!(struct Irqs {
    I2C0_IRQ => I2cInterruptHandler<I2C0>;
    PIO0_IRQ_0 => PioInterruptHandler<PIO0>;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>;
});

// ─── tunables ───────────────────────────────────────────────────────────

/// WiFi credentials, baked in from `.env` by `build.rs` (see
/// `docs/DEVELOPMENT.md`). The WiFi supervisor validates them and parks if they
/// cannot be used.
const WIFI_SSID: &str = env!("EZAL_WIFI_SSID");
const WIFI_PASSWORD: &str = env!("EZAL_WIFI_PASSWORD");

/// Bound the entire ADC self-test, including its presence and register checks.
/// A stuck bus can suspend an async transaction without stalling the actuator
/// task, so the actuator's watchdog alone cannot detect this startup failure.
const ADC_POST_TIMEOUT: Duration = Duration::from_millis(500);

/// Drives the Embassy TCP/IP stack.
#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, NetDriver<'static>>) -> ! {
    runner.run().await
}

/// Stop this startup path while retaining its hardware. Already spawned tasks continue.
async fn park<T>(resources: T) -> ! {
    let never = core::future::pending::<core::convert::Infallible>().await;
    drop(resources);
    match never {}
}

// ─── entry point ────────────────────────────────────────────────────────

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    let directions = drive::DirectionOutputs::new(p.PIN_10, p.PIN_11, p.PIN_20, p.PIN_21);
    let mut watchdog = embassy_rp::watchdog::Watchdog::new(p.WATCHDOG);
    if watchdog::firmware_timeout_caused_reset(&mut watchdog) {
        // Keep the owned outputs low until a hardware reset clears the reason.
        watchdog.stop();
        error!("watchdog timeout: motion inhibited; inspect the fault, then reset the board");
        park((directions, watchdog)).await;
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
    let mut adc = Ads1015::new(i2c);

    info!("ezal: POSTing ADS1015 at 0x{=u8:x}", ads1015::I2C_ADDR);
    match with_timeout(ADC_POST_TIMEOUT, adc.post()).await {
        Ok(Ok(r)) => {
            state::STATE.store_raw_feedback(FeedbackVoltages {
                a0_elevation_mv: r.a0_mv,
                a1_azimuth_mv: r.a1_mv,
            });
            info!(
                "ADS1015 POST OK: config=0x{=u16:x}, A0={=i32} mV, A1={=i32} mV",
                r.config, r.a0_mv, r.a1_mv
            );
        }
        Ok(Err(e)) => {
            error!("ADS1015 POST FAILED: {}", e);
            park(adc).await;
        }
        Err(_) => {
            // Cancellation can leave a HAL transaction partially completed.
            // Keep the bus owned but never use it again before a hardware
            // reset. No feedback monitor has started, so outputs remain inhibited.
            error!("ADS1015 POST timed out; motion inhibited; reset required");
            park(adc).await;
        }
    }

    spawner.spawn(feedback::run(adc, &state::STATE).expect("static task pool exhausted"));
    info!("ezal: manual control; outputs idle until operator input");

    // ─── WiFi: the CYW43439 radio, wired as docs/HARDWARE.md lists ───
    // main owns `Irqs`, so it builds the interrupt-driven PIO block and DMA
    // channel and hands them to `Wifi::init` with the four radio pins.
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
    spawner.spawn(tracking::run(&state::STATE).expect("static task pool exhausted"));

    // Start acceptors while offline. They remain registered across reconnects,
    // so successful retries expose the dashboard without rebuilding the stack.
    info!("ezal web: starting listener on http://<dhcp-address>/");
    web::spawn_servers(spawner, stack);
}
