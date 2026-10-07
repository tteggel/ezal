//! CYW43439 initialization, bounded association, and network supervision.
//!
//! This is the hardware-touching half of ezal's WiFi support, the counterpart
//! to [`ezal_core::wifi`]'s pure credential rules. It powers the Pico 2 W's
//! on-module Infineon **CYW43439** radio, spawns the driver's background task,
//! and joins the access point whose SSID/password `build.rs` baked in from
//! `.env`. Initialization runs once; the supervisor owns every retry.
//!
//! ## Why the radio needs a background task
//!
//! The CYW43439 has no dedicated SPI peripheral on the RP2350; its gSPI bus is
//! bit-banged on a **PIO** state machine ([`cyw43_pio`]) with a DMA channel.
//! All of that — plus the chip's event pump and the control-message plumbing —
//! is driven by a long-lived [`cyw43::Runner`] future, which we hand to the
//! Embassy executor as its own task ([`cyw43_task`]). Everything else (join,
//! GPIO) talks to the chip through the returned [`cyw43::Control`] handle,
//! which we wrap in [`Wifi`]. The IP stack uses the separate [`NetDriver`].
//!
//! ## STA mode
//!
//! "Station mode" (STA) is the ordinary client role — the Pico joins someone
//! else's access point, as opposed to *being* one (AP mode). It is the
//! CYW43439's default after [`Control::init`], so bringing up STA is simply a
//! matter of [`Control::join`]-ing a network; there is no explicit mode call.
//! (AP mode would instead use `control.start_ap_*`.)
//!
//! ## Association and recovery
//!
//! The supervisor validates immutable credentials once, then owns every retry,
//! DHCP wait, and link/address check. [`Wifi::associate_once`] clears the old
//! association, waits for it to settle, and attempts one bounded join.
//!
//! A successful join establishes station association; motion permission also
//! requires a usable DHCP address. Every unsuccessful attempt stays offline.

use cyw43::aligned_bytes;
use cyw43::{Control, JoinAuth, JoinError, JoinOptions, NetDriver, PowerManagementMode};
use cyw43_pio::{PioSpi, RM2_CLOCK_DIVIDER};
use defmt::{error, info, warn, Format};
use embassy_executor::Spawner;
use embassy_futures::select::{select3, Either3};
use embassy_net::{ConfigV4, Stack};
use embassy_rp::dma::Channel;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::{PIN_23, PIN_24, PIN_25, PIN_29, PIO0};
use embassy_rp::pio::Pio;
use embassy_rp::Peri;
use embassy_time::{with_timeout, Duration, Timer};
use static_cell::StaticCell;

use ezal_core::network::NETWORK_POLL_MS;
use ezal_core::wifi::{ConnectionRetries, Credentials, Security};

use crate::state::SharedState;

/// The power-management profile we run the radio in. This is a mains-powered
/// controller and the dashboard sends direction leases over a WebSocket, so
/// predictable latency matters more than idle current. `Performance` still
/// enables CYW43 PM mode 2; `None` keeps the radio awake through the initial
/// WPA handshake as well as normal operation.
const POWER_MODE: PowerManagementMode = PowerManagementMode::None;

/// Protected-network auth mode to request from the CYW43439. Ezal is deployed
/// at one controlled location, so protected WiFi means WPA3/SAE only.
const PROTECTED_JOIN_AUTH: JoinAuth = JoinAuth::Wpa3;

/// Extra board-level off time before handing the power pin to `cyw43`.
/// `probe-rs run` resets/reprograms the RP2350 without necessarily removing
/// power from the CYW43439, and the upstream gSPI reset pulse is only 20 ms.
/// Holding WL_ON low here gives the radio and AP state a cleaner boundary
/// between debug-flash boots.
const POWER_OFF_SETTLE: Duration = Duration::from_millis(250);

/// Time to let a disassociation settle before starting a join. The retry path
/// that clears intermittent UniFi/CYW43 stale state is `leave + 500ms + join`,
/// so apply the same boundary before the first attempt as well.
const JOIN_LEAVE_SETTLE: Duration = Duration::from_millis(500);

/// Bound disassociation too: a timed-out join may indicate an unresponsive
/// driver, so waiting forever for its cleanup would defeat the join deadline.
const LEAVE_TIMEOUT: Duration = Duration::from_secs(1);

/// A DHCP outage must release the join attempt for another bounded retry.
const DHCP_TIMEOUT: Duration = Duration::from_secs(30);

/// The CYW43439's long-lived driver task: it owns the PIO/DMA gSPI bus and
/// pumps the chip's events for the life of the program. Spawned once by
/// [`Wifi::init`]; everything else reaches the radio through [`Control`].
///
/// The concrete type spells out our bus exactly — a [`PioSpi`] on `PIO0` state
/// machine 0 with a plain [`Output`] power pin — because an
/// `#[embassy_executor::task]` cannot be generic.
#[embassy_executor::task]
async fn cyw43_task(
    runner: cyw43::Runner<'static, cyw43::SpiBus<Output<'static>, PioSpi<'static, PIO0, 0>>>,
) -> ! {
    runner.run().await
}

/// A brought-up CYW43439 radio, reachable through its [`Control`] handle.
///
/// Built by [`Wifi::init`]; [`Wifi::associate_once`] joins the configured AP.
pub struct Wifi<'d> {
    control: Control<'d>,
}

/// What a successful [`Wifi::associate_once`] established, for logging.
#[derive(Format)]
struct AssociationReport {
    /// Whether the joined network is password-protected (`true`) or open
    /// (`false`).
    protected: bool,
}

/// Why a bounded association attempt failed.
#[derive(Format)]
enum AssociationError {
    /// The credentials were well-formed but the join failed — wrong password,
    /// AP out of range, or no AP with that SSID. Carries the driver's reason.
    Join(JoinError),
    /// The driver did not report association success or failure before the
    /// timeout. Usually means the chip/AP state got wedged across a debugger
    /// reset, or the expected join event was missed.
    JoinTimeout,
    /// The radio did not acknowledge disassociation. Its link state is
    /// unknown. Supervision keeps outputs inhibited and retries cleanup after
    /// backoff before attempting another association.
    LeaveTimeout,
}

impl Wifi<'static> {
    /// Power up the CYW43439 and get it ready to join a network.
    ///
    /// Uploads the three on-chip firmware blobs (see
    /// [`cyw43-firmware/`](../cyw43-firmware/README.md)), spawns the
    /// [`cyw43_task`] driver task on `spawner`, loads the regulatory table,
    /// and applies [`POWER_MODE`]. Returns once the radio is idle in STA mode,
    /// ready for [`Wifi::associate_once`].
    ///
    /// Hardware/firmware-upload failure can keep this initialization pending.
    /// The network permit remains offline, so motion stays inhibited. Such a
    /// failure needs hardware reset: after handing the PIO/DMA bus to the
    /// driver task, its static state and owned peripherals cannot be recreated
    /// safely by the association/DHCP retry loop.
    ///
    /// The caller (`main`) owns the interrupt bindings, so it constructs the
    /// two peripherals that consume them — the `pio` block and the `dma`
    /// channel — and hands them here along with the four CYW43439 pins. Those
    /// pins are fixed by the Pico 2 W's board wiring:
    ///
    /// | role            | GPIO   |
    /// |-----------------|--------|
    /// | power-on (`pwr`)| GP23   |
    /// | chip-select     | GP25   |
    /// | data (`dio`)    | GP24   |
    /// | clock (`clk`)   | GP29   |
    ///
    /// Their distinct peripheral types mean the compiler rejects a mis-wired
    /// call, so the positional arguments are safe.
    pub async fn init(
        spawner: Spawner,
        mut pio: Pio<'static, PIO0>,
        dma: Channel<'static>,
        pwr: Peri<'static, PIN_23>,
        cs: Peri<'static, PIN_25>,
        dio: Peri<'static, PIN_24>,
        clk: Peri<'static, PIN_29>,
    ) -> (NetDriver<'static>, Self) {
        // The three images the CYW43439 needs uploaded at every boot. They're
        // vendored in-tree and baked into our image, so there's no separate
        // flashing step. See that directory's README for provenance/licence.
        let fw = aligned_bytes!("../cyw43-firmware/43439A0.bin");
        let clm = aligned_bytes!("../cyw43-firmware/43439A0_clm.bin");
        let nvram = aligned_bytes!("../cyw43-firmware/nvram_rp2040.bin");

        // Power-enable line starts low (radio off); hold it there before
        // giving it to the driver so debugger resets don't leave the CYW43 in
        // a warm half-associated state. CS idles high. The gSPI bus is a PIO
        // program on state machine 0, clocked by the CYW43-tuned
        // `RM2_CLOCK_DIVIDER` (a plain fast divider corrupts the link — see
        // embassy-rs/embassy#3960).
        let pwr = Output::new(pwr, Level::Low);
        Timer::after(POWER_OFF_SETTLE).await;
        let cs = Output::new(cs, Level::High);
        let spi = PioSpi::new(
            &mut pio.common,
            pio.sm0,
            RM2_CLOCK_DIVIDER,
            pio.irq0,
            cs,
            dio,
            clk,
            dma,
        );

        // `cyw43::State` is the driver's working memory. It must outlive the
        // `Control` and `Runner` that borrow it — i.e. live for the whole
        // program — so it goes in a `StaticCell` rather than on the stack.
        static STATE: StaticCell<cyw43::State> = StaticCell::new();
        let state = STATE.init(cyw43::State::new());

        // Give the data-plane handle to `embassy-net` before association. That
        // lets both DHCP and HTTP tasks survive repeated association attempts;
        // the separate Control handle remains owned by our supervisor.
        let (net_device, mut control, runner) = cyw43::new(state, pwr, spi, fw, nvram).await;
        spawner.spawn(cyw43_task(runner).expect("static task pool exhausted"));

        // Load the Country Locale Matrix (regulatory channel/power limits),
        // then pick a power-management profile. Both talk to the chip via the
        // task we just spawned.
        control.init(clm).await;
        control.set_power_management(POWER_MODE).await;

        (net_device, Wifi { control })
    }

    /// Clear the old association and attempt one join with validated settings.
    ///
    /// The supervisor supplies the join deadline and owns all retries. A failed
    /// join needs cleanup before backoff or another attempt; a cleanup timeout
    /// must itself back off without issuing another join.
    async fn associate_once(
        &mut self,
        creds: &Credentials<'_>,
        security: Security,
        timeout: Duration,
    ) -> Result<AssociationReport, AssociationError> {
        self.leave().await?;
        Timer::after(JOIN_LEAVE_SETTLE).await;
        let options = match security {
            Security::Open => JoinOptions::new_open(),
            Security::Protected => {
                let mut options = JoinOptions::new(creds.password.as_bytes());
                options.auth = PROTECTED_JOIN_AUTH;
                options
            }
        };

        match with_timeout(timeout, self.control.join(creds.ssid, options)).await {
            Ok(Ok(())) => Ok(AssociationReport {
                protected: matches!(security, Security::Protected),
            }),
            Ok(Err(error)) => Err(AssociationError::Join(error)),
            Err(_) => Err(AssociationError::JoinTimeout),
        }
    }

    /// Clear chip association state before another join or after a failed one.
    /// The pinned CYW43 driver cancels its host-side IOCTL on future drop;
    /// timeout still does not prove the radio processed the request. End this
    /// attempt; the supervisor backs off before retrying cleanup. Every later
    /// attempt must receive a successful disassociation before joining again.
    /// Readiness additionally requires that join and a new DHCP lease succeed.
    async fn leave(&mut self) -> Result<(), AssociationError> {
        with_timeout(LEAVE_TIMEOUT, self.control.leave())
            .await
            .map_err(|_| AssociationError::LeaveTimeout)
    }
}

/// Maintain station association, DHCP, and the actuator's expiring network
/// permit for the lifetime of the firmware. HTTP acceptors are already running
/// when this task starts; late AP/DHCP recovery needs no reboot.
#[embassy_executor::task]
pub async fn supervise(
    mut wifi: Wifi<'static>,
    stack: Stack<'static>,
    creds: Credentials<'static>,
    state: &'static SharedState,
) -> ! {
    let security = match creds.validate() {
        Ok(security) => security,
        Err(error) => {
            state.publish_network(false);
            stack.set_config_v4(ConfigV4::None);
            error!(
                "WiFi credentials invalid: {=str}; motion inhibited; reconfigure and restart",
                error.message()
            );
            crate::park(wifi).await;
        }
    };
    let mut retries = ConnectionRetries::new();
    loop {
        state.publish_network(false);
        // Clear any cached address BEFORE retrying association. A lease from
        // the previous link must not count as successful DHCP on the new one.
        stack.set_config_v4(ConfigV4::None);
        let timeout = Duration::from_millis(retries.association_timeout_ms());
        match wifi.associate_once(&creds, security, timeout).await {
            Ok(report) => {
                info!(
                    "WiFi associated ({=str}); waiting for DHCP",
                    if report.protected { "secured" } else { "open" }
                );
                stack.set_config_v4(ConfigV4::Dhcp(Default::default()));
                let configured = with_timeout(DHCP_TIMEOUT, async {
                    stack.wait_link_up().await;
                    stack.wait_config_up().await;
                })
                .await
                .is_ok();
                if configured && network_ready(stack) {
                    retries.connected();
                    let address = stack.config_v4().map(|config| config.address);
                    info!("network ready; motion permission available");
                    loop {
                        if !network_ready(stack)
                            || stack.config_v4().map(|config| config.address) != address
                        {
                            break;
                        }
                        state.publish_network(true);
                        // Event waits cut permission as soon as the stack
                        // reports a loss. Polling also notices address changes
                        // and renews the independently enforced 300 ms permit.
                        match select3(
                            stack.wait_link_down(),
                            stack.wait_config_down(),
                            Timer::after(Duration::from_millis(NETWORK_POLL_MS)),
                        )
                        .await
                        {
                            Either3::First(()) | Either3::Second(()) => break,
                            Either3::Third(()) => {}
                        }
                    }
                    state.publish_network(false);
                    warn!("network lost or address changed; motion inhibited");
                } else {
                    warn!("DHCP unavailable after bounded wait; motion inhibited");
                }
            }
            Err(AssociationError::JoinTimeout) if retries.retry_timed_out_join() => {
                warn!("WiFi association timed out; clearing state and retrying");
                // The next attempt performs bounded leave + settle before
                // joining. If cleanup fails, it returns LeaveTimeout and
                // takes the backoff path rather than consuming another join.
                continue;
            }
            Err(error) => {
                // Terminal join failures clear radio state before sleeping,
                // just as prompt retries clear it before their next join.
                // Initial cleanup failures must not immediately retry cleanup.
                let error = match error {
                    AssociationError::Join(_) | AssociationError::JoinTimeout => {
                        wifi.leave().await.err().unwrap_or(error)
                    }
                    _ => error,
                };
                warn!("WiFi attempt failed: {}; motion inhibited", error);
            }
        }

        state.publish_network(false);
        let retry_secs = retries.backoff_secs();
        info!("network retry in {=u64}s", retry_secs);
        Timer::after(Duration::from_secs(retry_secs)).await;
    }
}

/// Readiness means a current station link and a usable local IPv4 address.
/// It deliberately does not depend on internet access or a connected viewer.
fn network_ready(stack: Stack<'_>) -> bool {
    stack.is_link_up()
        && stack.config_v4().is_some_and(|config| {
            let address = config.address.address();
            !address.is_unspecified()
                && !address.is_multicast()
                && !address.is_broadcast()
                && !address.is_loopback()
        })
}
