//! Hardware fallback when the executor, interrupts, or panic handler stop making
//! progress. Only the direction-output task may feed this watchdog, after it has
//! serviced its safety timers and applied the resulting GPIO state.

use embassy_rp::watchdog::{ResetReason, Watchdog};
use embassy_time::Duration;

/// Maximum interval between completed output-service iterations.
pub const WATCHDOG_TIMEOUT: Duration = Duration::from_millis(500);

/// Marks a watchdog armed by this firmware rather than by a reboot request.
/// The Pico SDK writes the same value for the same purpose. The boot ROM acts
/// only on its own scratch magic, so claiming this register changes no boot.
const FIRMWARE_ARMED_MAGIC: u32 = 0x6ab7_3121;

/// Start a watchdog which keeps running even when a debugger halts the CPU.
pub fn start(watchdog: &mut Watchdog) {
    // Claim scratch[4]. The boot ROM treats its own values here as a reboot
    // request, and a flashing tool's reboot replaces this marker, so only a
    // timeout of the watchdog started below can be mistaken for a fault.
    watchdog.set_scratch(4, FIRMWARE_ARMED_MAGIC);
    watchdog.pause_on_debug(false);
    watchdog.start(WATCHDOG_TIMEOUT);

    // embassy-rp 0.10's start() uses the RP2040 PSM bit mask on RP2350 too.
    // Explicitly select the RP2350 reset domains, including SIO and both CPUs,
    // while retaining the oscillators. This follows the Pico SDK's
    // hardware_watchdog/watchdog.c reset selection. The SIO reset takes the
    // GPIOs out of the stalled executor's control. RP2350 pads also have
    // isolation latches that can hold a pin's last output state through a
    // reset until boot reinitialises the pad, so the board's transistor-base
    // pull-downs may only take effect once the next boot claims the pins.
    // docs/DEVELOPMENT.md has the bench check that measures this window.
    embassy_rp::pac::PSM.wdsel().write(|w| {
        *w = embassy_rp::pac::psm::regs::Wdsel(0x01ff_ffff);
        w.set_rosc(false);
        w.set_xosc(false);
    });
}

/// Whether the last reset was a timeout of a watchdog armed by [`start`].
///
/// The RP2350 boot ROM also restarts the chip through the watchdog after a UF2
/// download or a `picotool` reboot, which sets the same reason bit. Those paths
/// write their own scratch values, so flashing does not look like a fault.
pub fn firmware_timeout_caused_reset(watchdog: &mut Watchdog) -> bool {
    matches!(watchdog.reset_reason(), Some(ResetReason::TimedOut))
        && watchdog.get_scratch(4) == FIRMWARE_ARMED_MAGIC
}
