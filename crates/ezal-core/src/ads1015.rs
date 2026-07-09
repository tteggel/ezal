//! # ADS1015 register model (pure, host-testable)
//!
//! The [ADS1015] is the 12-bit I²C ADC that reads the G-5500's azimuth and
//! elevation position-feedback voltages (through the on-board resistor
//! dividers — see [`docs/HARDWARE.md`](../../../../docs/HARDWARE.md)). This
//! module is the *pure* half of the driver: it knows the register map, packs
//! the Config register, and decodes conversion results, but it never touches
//! a bus. The bus transactions live in the firmware crate's `ads1015`
//! module, which delegates every bit of encoding/decoding here.
//!
//! Keeping this half hardware-free is what makes it testable with a plain
//! `cargo test` on the host — and the bit-twiddling below (field positions,
//! the self-clearing OS bit, sign-extending a left-justified 12-bit result)
//! is exactly where an ADC bring-up tends to go wrong, so that is where the
//! tests earn their keep.
//!
//! [ADS1015]: https://www.ti.com/product/ADS1015

/// 7-bit I²C address of the ADS1015 with its ADDR pin tied to GND — the
/// wiring on the ezal interface board. (Tying ADDR to VDD / SDA / SCL would
/// instead give 0x49 / 0x4A / 0x4B; we only ever use 0x48.)
pub const I2C_ADDR: u8 = 0x48;

/// The four ADS1015 registers, selected by the *pointer* byte that opens
/// every I²C transaction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Register {
    /// Conversion result (read-only): the 12-bit sample, left-justified in
    /// the top bits of a 16-bit word.
    Conversion = 0x00,
    /// Configuration / control register.
    Config = 0x01,
    /// Comparator low threshold. Unused as a comparator here (we disable it),
    /// which frees it as a scratch register for the POST link check.
    LoThresh = 0x02,
    /// Comparator high threshold. Unused as a comparator here — see
    /// [`LoThresh`](Register::LoThresh).
    HiThresh = 0x03,
}

impl Register {
    /// The pointer byte that selects this register.
    pub const fn addr(self) -> u8 {
        self as u8
    }
}

/// Programmable-gain-amplifier full-scale range (Config bits 11:9). This sets
/// the ± voltage that maps to the full ±2048-code span — i.e. the volts per
/// code.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FullScale {
    /// ±6.144 V
    V6_144,
    /// ±4.096 V
    V4_096,
    /// ±2.048 V — the ezal setting: the divided feedback tops out at ≈2.02 V,
    /// ≈99 % of this range, keeping the LSB near 1 mV.
    V2_048,
    /// ±1.024 V
    V1_024,
    /// ±0.512 V
    V0_512,
    /// ±0.256 V
    V0_256,
}

impl FullScale {
    /// The 3-bit PGA selector, already shifted into Config bits 11:9.
    pub const fn bits(self) -> u16 {
        let field: u16 = match self {
            FullScale::V6_144 => 0b000,
            FullScale::V4_096 => 0b001,
            FullScale::V2_048 => 0b010,
            FullScale::V1_024 => 0b011,
            FullScale::V0_512 => 0b100,
            FullScale::V0_256 => 0b101,
        };
        field << 9
    }

    /// Full-scale magnitude in millivolts (the voltage at +2048 codes).
    pub const fn full_scale_mv(self) -> i32 {
        match self {
            FullScale::V6_144 => 6144,
            FullScale::V4_096 => 4096,
            FullScale::V2_048 => 2048,
            FullScale::V1_024 => 1024,
            FullScale::V0_512 => 512,
            FullScale::V0_256 => 256,
        }
    }
}

/// Input-multiplexer selection (Config bits 14:12). ezal reads the two
/// single-ended channels [`Ain0`](Mux::Ain0) and [`Ain1`](Mux::Ain1); the
/// differential and other single-ended modes are modelled for completeness.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mux {
    /// AINp = AIN0, AINn = AIN1 (differential) — the power-on default.
    Diff0_1,
    /// AINp = AIN0, AINn = AIN3 (differential).
    Diff0_3,
    /// AINp = AIN1, AINn = AIN3 (differential).
    Diff1_3,
    /// AINp = AIN2, AINn = AIN3 (differential).
    Diff2_3,
    /// AIN0 vs GND (single-ended) — one G-5500 feedback channel.
    Ain0,
    /// AIN1 vs GND (single-ended) — the other G-5500 feedback channel.
    Ain1,
    /// AIN2 vs GND (single-ended) — unused on ezal.
    Ain2,
    /// AIN3 vs GND (single-ended) — unused on ezal.
    Ain3,
}

impl Mux {
    /// The 3-bit MUX selector, already shifted into Config bits 14:12.
    pub const fn bits(self) -> u16 {
        let field: u16 = match self {
            Mux::Diff0_1 => 0b000,
            Mux::Diff0_3 => 0b001,
            Mux::Diff1_3 => 0b010,
            Mux::Diff2_3 => 0b011,
            Mux::Ain0 => 0b100,
            Mux::Ain1 => 0b101,
            Mux::Ain2 => 0b110,
            Mux::Ain3 => 0b111,
        };
        field << 12
    }
}

/// Conversion data rate in samples/second (Config bits 7:5). A higher rate is
/// a shorter conversion time; ezal uses 1600 SPS (≈625 µs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DataRate {
    /// 128 SPS
    Sps128,
    /// 250 SPS
    Sps250,
    /// 490 SPS
    Sps490,
    /// 920 SPS
    Sps920,
    /// 1600 SPS — the ADS1015 power-on default and the ezal setting.
    Sps1600,
    /// 2400 SPS
    Sps2400,
    /// 3300 SPS
    Sps3300,
}

impl DataRate {
    /// The 3-bit data-rate selector, already shifted into Config bits 7:5.
    pub const fn bits(self) -> u16 {
        let field: u16 = match self {
            DataRate::Sps128 => 0b000,
            DataRate::Sps250 => 0b001,
            DataRate::Sps490 => 0b010,
            DataRate::Sps920 => 0b011,
            DataRate::Sps1600 => 0b100,
            DataRate::Sps2400 => 0b101,
            DataRate::Sps3300 => 0b110,
        };
        field << 5
    }
}

/// Config bit 15 (OS). On a *write*, 1 starts a single-shot conversion. On a
/// *read*, 1 means idle (conversion complete) and 0 means a conversion is in
/// progress. Because the device drives this bit itself, it is not part of the
/// persistent configuration and must be masked out of any round-trip check —
/// see [`config_matches`].
pub const OS: u16 = 0x8000;

/// Config bit 8 (MODE) = 1 selects single-shot mode (the device powers down
/// between conversions); 0 would select continuous conversion.
const MODE_SINGLE_SHOT: u16 = 1 << 8;

/// Config bits 1:0 (COMP_QUE) = 0b11 disables the comparator. As a bonus this
/// makes the threshold registers functionally inert, so the POST can borrow
/// them as scratch storage (see [`SCRATCH_LO`] / [`SCRATCH_HI`]).
const COMP_QUE_DISABLE: u16 = 0b11;

/// Build the 16-bit Config value that starts a single-shot conversion on
/// `mux` at `fs` full-scale and `dr` data rate, with the comparator disabled.
/// This is the exact word the firmware writes to [`Register::Config`].
pub const fn config_single_shot(mux: Mux, fs: FullScale, dr: DataRate) -> u16 {
    OS | mux.bits() | fs.bits() | MODE_SINGLE_SHOT | dr.bits() | COMP_QUE_DISABLE
}

/// True if a Config read-back matches what was written, ignoring the
/// self-clearing [`OS`] bit (which the device drives on its own, so it may
/// read back either way regardless of what we wrote).
pub const fn config_matches(written: u16, read_back: u16) -> bool {
    (written & !OS) == (read_back & !OS)
}

/// POST scratch pattern for [`Register::LoThresh`]. The two scratch patterns
/// are bitwise complements, so a stuck-high or stuck-low data line corrupts
/// at least one of them — a stronger link check than a bare address ACK.
pub const SCRATCH_LO: u16 = 0x5AA5;

/// POST scratch pattern for [`Register::HiThresh`]; the complement of
/// [`SCRATCH_LO`].
pub const SCRATCH_HI: u16 = 0xA55A;

/// Decode a raw 16-bit Conversion-register word into a signed 12-bit count.
///
/// The ADS1015 left-justifies its 12-bit result in the top bits of the word
/// (the low 4 bits are always zero), so we arithmetic-shift right by 4 to
/// recover the count *and* sign-extend it. Single-ended reads are
/// non-negative; only differential reads go negative, but decoding the sign
/// correctly costs nothing and avoids a lurking trap.
pub const fn decode_count(raw: u16) -> i16 {
    (raw as i16) >> 4
}

/// Convert a signed conversion count to millivolts at the given full-scale
/// range. +2048 codes corresponds to +full-scale, so this is
/// `count * full_scale_mv / 2048`.
pub const fn count_to_mv(count: i16, fs: FullScale) -> i32 {
    (count as i32 * fs.full_scale_mv()) / 2048
}
