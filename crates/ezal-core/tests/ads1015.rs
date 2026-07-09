//! Host unit tests for the pure ADS1015 register logic.
//!
//! These run on the host with a plain `cargo test` (see
//! `scripts/test-host.sh`). They lock down the parts of an ADC bring-up that
//! are easy to get subtly wrong — field positions in the Config register, the
//! self-clearing OS bit, and the sign-extension of a left-justified 12-bit
//! conversion result — so a mistake shows up here, in seconds, rather than as
//! a puzzling voltage on the bench.

use ezal_core::ads1015::*;

/// The Config word ezal actually writes must land every field in the right
/// place. Worked bit-by-bit against the datasheet:
///   OS 1        → 0x8000
///   MUX 100     → 0x4000   (AIN0 vs GND)
///   PGA 010     → 0x0400   (±2.048 V)
///   MODE 1      → 0x0100   (single-shot)
///   DR 100      → 0x0080   (1600 SPS)
///   COMP_QUE 11 → 0x0003   (comparator disabled)
/// OR'd together: 0xC583.
#[test]
fn config_single_shot_packs_expected_bits() {
    assert_eq!(
        config_single_shot(Mux::Ain0, FullScale::V2_048, DataRate::Sps1600),
        0xC583
    );
    // Switching to AIN1 flips MUX 100 → 101, i.e. +0x1000.
    assert_eq!(
        config_single_shot(Mux::Ain1, FullScale::V2_048, DataRate::Sps1600),
        0xD583
    );
}

/// Each field's selector must sit in the documented bit position.
#[test]
fn config_fields_occupy_the_right_bits() {
    assert_eq!(Mux::Diff0_1.bits(), 0x0000); // MUX 000
    assert_eq!(Mux::Ain0.bits(), 0x4000); // MUX 100 << 12
    assert_eq!(FullScale::V6_144.bits(), 0x0000); // PGA 000
    assert_eq!(FullScale::V2_048.bits(), 0x0400); // PGA 010 << 9
    assert_eq!(DataRate::Sps128.bits(), 0x0000); // DR 000
    assert_eq!(DataRate::Sps1600.bits(), 0x0080); // DR 100 << 5
}

/// The round-trip check must ignore the OS bit (the device drives it) but
/// still reject a genuinely different configuration.
#[test]
fn config_matches_ignores_os_but_catches_real_differences() {
    let written = config_single_shot(Mux::Ain0, FullScale::V2_048, DataRate::Sps1600);
    // Device reports OS = 0 (mid-conversion) but otherwise identical: match.
    assert!(config_matches(written, written & !OS));
    // Device reports OS = 1 (idle) and otherwise identical: match.
    assert!(config_matches(written, written | OS));
    // A different PGA is a real mismatch and must be rejected.
    let other_pga = config_single_shot(Mux::Ain0, FullScale::V4_096, DataRate::Sps1600);
    assert!(!config_matches(written, other_pga));
    // The all-ones / all-zeros bus-fault readings must never pass.
    assert!(!config_matches(written, 0xFFFF));
    assert!(!config_matches(written, 0x0000));
}

/// The 12-bit result is left-justified in the top bits, so decoding is an
/// arithmetic (sign-extending) shift right by 4.
#[test]
fn decode_count_sign_extends_left_justified_result() {
    assert_eq!(decode_count(0x0000), 0);
    assert_eq!(decode_count(0x7FF0), 2047); // largest positive code
    assert_eq!(decode_count(0x8000), -2048); // largest negative (differential)
    assert_eq!(decode_count(0xFFF0), -1); // −1 LSB, sign-extended
    assert_eq!(decode_count(0x7E40), 2020); // ≈2.02 V at ±2.048 V
}

/// Counts convert to millivolts as `count * full_scale_mv / 2048`.
#[test]
fn count_to_mv_scales_by_full_scale() {
    // At ±2.048 V the LSB is exactly 1 mV.
    assert_eq!(count_to_mv(2048, FullScale::V2_048), 2048);
    assert_eq!(count_to_mv(2020, FullScale::V2_048), 2020);
    // At ±4.096 V the LSB is 2 mV, so half the codes give the same voltage.
    assert_eq!(count_to_mv(1024, FullScale::V4_096), 2048);
    // Sign is preserved through the conversion.
    assert_eq!(count_to_mv(-2048, FullScale::V2_048), -2048);
    assert_eq!(count_to_mv(0, FullScale::V6_144), 0);
}

/// The two POST scratch patterns must be exact complements so a stuck data
/// line fails at least one of them.
#[test]
fn scratch_patterns_are_complementary() {
    assert_eq!(SCRATCH_LO, !SCRATCH_HI);
}
