//! CYW43439 WiFi bring-up + station-mode join self-test (POST).
//!
//! This is the hardware-touching half of ezal's WiFi support, the counterpart
//! to [`ezal_core::wifi`]'s pure credential rules. It powers the Pico 2 W's
//! on-module Infineon **CYW43439** radio, spawns the driver's background task,
//! and — as a power-on self-test — joins the access point whose SSID/password
//! `build.rs` baked in from `.env`.
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
//! ## What the POST proves
//!
//! [`Wifi::post`] is a genuine self-test, mirroring the ADS1015 one:
//!
//!  1. **Credentials** — [`ezal_core::wifi::Credentials::validate`] rejects an
//!     empty/oversized SSID or a bad-length passphrase after radio initialization
//!     and before association, turning a mistyped `.env` into a clear message.
//!  2. **Association** — a successful [`Control::join`] proves the whole chain
//!     end to end: the PIO/DMA bus to the chip, the firmware upload, the
//!     antenna, and that the AP actually accepted our credentials. A failure
//!     here ends that bounded attempt. The long-lived supervisor retries with
//!     backoff while keeping all motion inhibited.
//!
//! Getting an IP address (DHCP over `embassy-net`) is the natural next layer
//! and deliberately out of scope here: this POST confirms we can *associate*,
//! which is what "connect to an AP" means at the link layer.

use cyw43::aligned_bytes;
use cyw43::{Control, JoinAuth, JoinError, JoinOptions, NetDriver, PowerManagementMode};
use cyw43_pio::{PioSpi, RM2_CLOCK_DIVIDER};
use defmt::{info, warn, Format};
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
use ezal_core::wifi::{Credentials, Security};

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

/// WPA3/SAE can get into a stale first association on UniFi where the station
/// reaches LINK/JOIN but never receives the PSK_SUP success event. A retry is
/// consistently faster once the AP/chip state has been cleared, so keep the
/// first attempt bounded below the normal retry timeout.
const INITIAL_JOIN_TIMEOUT: Duration = Duration::from_millis(2750);

/// Maximum time to wait for the CYW43 association event before treating the
/// attempt as stale. `cyw43::Control::join` has no built-in timeout, so a
/// missed event can otherwise pin boot forever.
const JOIN_TIMEOUT: Duration = Duration::from_millis(3500);

/// Bound disassociation too: a timed-out join may indicate an unresponsive
/// driver, so waiting forever for its cleanup would defeat the join deadline.
const LEAVE_TIMEOUT: Duration = Duration::from_secs(1);

/// Number of association attempts before POST fails.
const JOIN_ATTEMPTS: usize = 3;

/// A DHCP outage must release the join attempt for another bounded retry.
const DHCP_TIMEOUT: Duration = Duration::from_secs(30);
const RETRY_INITIAL_SECS: u64 = 1;
const RETRY_MAX_SECS: u64 = 30;

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
/// Built by [`Wifi::init`]; [`Wifi::post`] then joins the configured AP.
pub struct Wifi<'d> {
    control: Control<'d>,
}

/// What a successful [`Wifi::post`] established, for logging.
#[derive(Format)]
pub struct PostReport {
    /// Whether the joined network is password-protected (`true`) or open
    /// (`false`).
    pub protected: bool,
}

/// Why a [`Wifi::post`] failed.
#[derive(Format)]
pub enum PostError {
    /// The baked-in credentials didn't pass [`Credentials::validate`] — the
    /// radio was initialized but no association was attempted. The string
    /// comes from [`ezal_core::wifi::CredentialError::message`]; the usual
    /// cause is a missing or mis-filled `.env`.
    InvalidCredentials(&'static str),
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
    /// ready for [`Wifi::post`].
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

    /// Run the WiFi power-on self-test: validate the baked-in credentials,
    /// then join the access point in station mode.
    ///
    /// On success the radio is associated with `creds.ssid` and the returned
    /// [`PostReport`] notes whether the link is secured. Failure attempts a
    /// bounded disassociation; if the radio cannot acknowledge it, its state
    /// remains unknown. Supervision logs the error and retries cleanup after
    /// backoff; every unsuccessful attempt leaves motion inhibited.
    pub async fn post(&mut self, creds: &Credentials<'_>) -> Result<PostReport, PostError> {
        // 1. Validate before association; init() has already powered the radio
        //    and uploaded firmware. Report a clear error for a bad `.env`.
        let security = creds
            .validate()
            .map_err(|e| PostError::InvalidCredentials(e.message()))?;

        // 2. Join. An empty password means an open network; otherwise use the
        //    configured WPA3/SAE handshake with the passphrase.
        self.leave().await?;
        Timer::after(JOIN_LEAVE_SETTLE).await;

        for attempt in 1..=JOIN_ATTEMPTS {
            let options = match security {
                Security::Open => JoinOptions::new_open(),
                Security::Protected => {
                    let mut options = JoinOptions::new(creds.password.as_bytes());
                    options.auth = PROTECTED_JOIN_AUTH;
                    options
                }
            };
            let timeout = if attempt == 1 {
                INITIAL_JOIN_TIMEOUT
            } else {
                JOIN_TIMEOUT
            };

            match with_timeout(timeout, self.control.join(creds.ssid, options)).await {
                Ok(Ok(())) => {
                    return Ok(PostReport {
                        protected: matches!(security, Security::Protected),
                    });
                }
                Ok(Err(error)) => {
                    self.leave().await?;
                    return Err(PostError::Join(error));
                }
                Err(_) if attempt < JOIN_ATTEMPTS => {
                    if attempt == 1 && matches!(security, Security::Protected) {
                        info!(
                            "WiFi initial WPA3 join did not complete; clearing state and retrying"
                        );
                    } else {
                        warn!(
                            "WiFi join timed out; retrying ({=usize}/{=usize})",
                            attempt, JOIN_ATTEMPTS
                        );
                    }
                    self.leave().await?;
                    Timer::after(JOIN_LEAVE_SETTLE).await;
                }
                Err(_) => {
                    self.leave().await?;
                    return Err(PostError::JoinTimeout);
                }
            }
        }

        unreachable!()
    }

    /// Clear chip association state before another join or after a failed one.
    /// The pinned CYW43 driver cancels its host-side IOCTL on future drop;
    /// timeout still does not prove the radio processed the request. End this
    /// attempt; the supervisor backs off before retrying cleanup. Every later
    /// attempt must receive a successful disassociation before joining again.
    /// Readiness additionally requires that join and a new DHCP lease succeed.
    async fn leave(&mut self) -> Result<(), PostError> {
        with_timeout(LEAVE_TIMEOUT, self.control.leave())
            .await
            .map_err(|_| PostError::LeaveTimeout)
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
    let mut retry_secs = RETRY_INITIAL_SECS;
    loop {
        state.publish_network(false);
        // Clear any cached address BEFORE retrying association. A lease from
        // the previous link must not count as successful DHCP on the new one.
        stack.set_config_v4(ConfigV4::None);
        match wifi.post(&creds).await {
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
                    retry_secs = RETRY_INITIAL_SECS;
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
            Err(error) => warn!("WiFi attempt failed: {}; motion inhibited", error),
        }

        state.publish_network(false);
        info!("network retry in {=u64}s", retry_secs);
        Timer::after(Duration::from_secs(retry_secs)).await;
        retry_secs = (retry_secs * 2).min(RETRY_MAX_SECS);
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
