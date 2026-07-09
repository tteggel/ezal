//! ADS1015 I²C driver + power-on self-test (POST).
//!
//! This is the thin, hardware-touching half of the ADS1015 support: it owns
//! the I²C peripheral and issues the actual bus transactions. Every bit of
//! register encoding and result decoding is delegated to
//! [`ezal_core::ads1015`], which is pure and host-tested — so the only thing
//! that can go wrong *here* is the plumbing (address, byte order, waiting out
//! a conversion), and that is exactly what the on-hardware POST log surfaces.
//!
//! The [`Ads1015::post`] self-test gates on two things that a bare address
//! ACK can't prove on their own:
//!
//!  1. **Config round-trip** — write our operating configuration and read it
//!     back (masking the self-clearing OS bit). Proves the device stores
//!     configuration, not merely that *something* answers at 0x48.
//!  2. **Scratch round-trip** — write complementary bit patterns to the
//!     (comparator-disabled, hence inert) threshold registers and read them
//!     straight back. Proves the link carries every data bit in both
//!     directions to real, RAM-backed registers.
//!
//! It then takes one reading on each feedback channel and returns them for
//! logging. Those voltages are *informational*, not a pass/fail gate: on the
//! bench the G-5500 may be unplugged, in which case the ADC inputs float.

use defmt::Format;
use embassy_rp::i2c::{Async, Error as I2cError, I2c};
use embassy_rp::peripherals::I2C0;
use embassy_time::{Duration, Timer};

use ezal_core::ads1015 as ads;
use ezal_core::ads1015::{DataRate, FullScale, Mux, Register};

/// PGA range for every ezal reading: ±2.048 V, chosen so the divided G-5500
/// feedback (≈0.90–2.02 V) sits near the top of the span. See
/// `docs/HARDWARE.md`.
const FS: FullScale = FullScale::V2_048;

/// Data rate for the single-shot reads: 1600 SPS ⇒ ≈625 µs per conversion.
const DR: DataRate = DataRate::Sps1600;

/// How long to wait for a single-shot conversion before reading the result.
/// 1 ms comfortably clears the ≈625 µs a 1600 SPS conversion takes; if [`DR`]
/// is ever lowered below ~1 kSPS this needs to grow to match.
const CONVERSION_WAIT: Duration = Duration::from_millis(1);

/// An ADS1015 reachable over an owned I²C bus at a fixed 7-bit address.
pub struct Ads1015<'d> {
    i2c: I2c<'d, I2C0, Async>,
    addr: u8,
}

/// What a successful [`Ads1015::post`] measured, for logging.
#[derive(Format)]
pub struct PostReport {
    /// The Config register as read back after configuring the device.
    pub config: u16,
    /// Channel A0 in millivolts (informational — not a pass/fail gate).
    pub a0_mv: i32,
    /// Channel A1 in millivolts (informational — not a pass/fail gate).
    pub a1_mv: i32,
}

/// Why a [`Ads1015::post`] failed.
#[derive(Format)]
pub enum PostError {
    /// Nothing acknowledged at the ADC's address — wrong address, no board,
    /// or an SDA/SCL wiring or pull-up fault.
    NoDevice,
    /// The Config register did not read back what we wrote (OS bit masked).
    ConfigRoundtrip {
        /// The value written to the Config register.
        wrote: u16,
        /// The value read back.
        read: u16,
    },
    /// A threshold-register scratch pattern did not survive a round-trip.
    ScratchRoundtrip {
        /// The scratch pattern written.
        wrote: u16,
        /// The value read back.
        read: u16,
    },
    /// An I²C transaction failed after the device had answered the presence
    /// probe (a mid-POST bus glitch).
    Bus,
}

impl<'d> Ads1015<'d> {
    /// Wrap an already-configured I²C bus. `addr` is normally
    /// [`ezal_core::ads1015::I2C_ADDR`].
    pub fn new(i2c: I2c<'d, I2C0, Async>, addr: u8) -> Self {
        Self { i2c, addr }
    }

    /// Run the power-on self-test. On success the device is left configured
    /// for single-shot reads and the returned [`PostReport`] carries one
    /// sample from each feedback channel.
    pub async fn post(&mut self) -> Result<PostReport, PostError> {
        // 1. Presence: a NACK here means nothing is answering at `addr`.
        self.read_reg(Register::Config)
            .await
            .map_err(|_| PostError::NoDevice)?;

        // 2. Config round-trip. Writing this also (harmlessly) kicks off a
        //    conversion via the OS bit, which we ignore.
        let cfg = ads::config_single_shot(Mux::Ain0, FS, DR);
        self.write_reg(Register::Config, cfg)
            .await
            .map_err(|_| PostError::Bus)?;
        let cfg_read = self
            .read_reg(Register::Config)
            .await
            .map_err(|_| PostError::Bus)?;
        if !ads::config_matches(cfg, cfg_read) {
            return Err(PostError::ConfigRoundtrip {
                wrote: cfg,
                read: cfg_read,
            });
        }

        // 3. Scratch round-trip through the inert threshold registers.
        self.scratch_roundtrip(Register::LoThresh, ads::SCRATCH_LO)
            .await?;
        self.scratch_roundtrip(Register::HiThresh, ads::SCRATCH_HI)
            .await?;

        // 4. One real reading per feedback channel — informational only.
        let a0_mv = self
            .read_channel_mv(Mux::Ain0)
            .await
            .map_err(|_| PostError::Bus)?;
        let a1_mv = self
            .read_channel_mv(Mux::Ain1)
            .await
            .map_err(|_| PostError::Bus)?;

        Ok(PostReport {
            config: cfg_read,
            a0_mv,
            a1_mv,
        })
    }

    /// Run one single-shot conversion on `mux` and return it in millivolts.
    /// Waits (asynchronously) [`CONVERSION_WAIT`] for the conversion.
    pub async fn read_channel_mv(&mut self, mux: Mux) -> Result<i32, I2cError> {
        let cfg = ads::config_single_shot(mux, FS, DR);
        self.write_reg(Register::Config, cfg).await?;
        Timer::after(CONVERSION_WAIT).await;
        let raw = self.read_reg(Register::Conversion).await?;
        Ok(ads::count_to_mv(ads::decode_count(raw), FS))
    }

    /// Write `pattern` to `reg` and require it to read straight back.
    async fn scratch_roundtrip(&mut self, reg: Register, pattern: u16) -> Result<(), PostError> {
        self.write_reg(reg, pattern)
            .await
            .map_err(|_| PostError::Bus)?;
        let read = self.read_reg(reg).await.map_err(|_| PostError::Bus)?;
        if read != pattern {
            return Err(PostError::ScratchRoundtrip {
                wrote: pattern,
                read,
            });
        }
        Ok(())
    }

    /// Write a 16-bit value to `reg`, big-endian, as the ADS1015 expects.
    async fn write_reg(&mut self, reg: Register, value: u16) -> Result<(), I2cError> {
        let [hi, lo] = value.to_be_bytes();
        self.i2c.write_async(self.addr, [reg.addr(), hi, lo]).await
    }

    /// Read the 16-bit value of `reg`.
    async fn read_reg(&mut self, reg: Register) -> Result<u16, I2cError> {
        let mut buf = [0u8; 2];
        self.i2c
            .write_read_async(self.addr, [reg.addr()], &mut buf)
            .await?;
        Ok(u16::from_be_bytes(buf))
    }
}
