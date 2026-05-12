//! International Morse Code encoder.
//!
//! This module turns an ASCII string such as `"HELLO WORLD"` into a stream
//! of [`Pulse`] values that describe how long to hold a key — or, in our
//! case, an LED — *on* or *off*. The firmware can then walk the iterator
//! and toggle a GPIO line accordingly.
//!
//! # The Morse timing model
//!
//! All Morse timing is expressed in multiples of the **dit**, the duration
//! of a single dot. By convention:
//!
//! | element                           | duration  |
//! |-----------------------------------|-----------|
//! | dot (dit)                         | 1 unit on |
//! | dash (dah)                        | 3 units on |
//! | gap between dots/dashes of a letter | 1 unit off |
//! | gap between letters of a word     | 3 units off |
//! | gap between words                 | 7 units off |
//!
//! The actual wall-clock length of a unit is a transmitter choice and
//! lives in the firmware (see [`UNIT_MS`] in `ezal-firmware`). 100–200 ms
//! is comfortable for visual decoding on an LED; classic CW operators
//! routinely run faster.
//!
//! # API at a glance
//!
//! Call [`pulses`] with a `&str` and iterate. Each [`Pulse`] tells you:
//!   * whether the key should be down (LED on) or up (LED off), and
//!   * how many *units* the pulse lasts.
//!
//! ```
//! # use ezal_core::morse;
//! for pulse in morse::pulses("SOS") {
//!     // toggle an LED here in a real firmware:
//!     // led.set_state(pulse.on);
//!     // wait pulse.units × UNIT_MS milliseconds.
//!     let _ = (pulse.on, pulse.units);
//! }
//! ```
//!
//! # Design notes
//!
//! The iterator is a small state machine — it does **not** allocate. That
//! matters because it has to run on the Pico 2 with no allocator. The full
//! state is fewer than ten bytes plus the `Chars` iterator over the input.
//! Unknown characters are silently skipped (rather than panicking) so a
//! stray `!` in a debug message can't crash the firmware mid-blink.

// ─────────────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────────────

/// Duration of a [`Pulse`] expressed in *dit times* (Morse units).
///
/// The longest legal Morse element is 7 units (the inter-word gap), so a
/// `u8` is overkill — but a `u8` is also the most natural unit and costs
/// nothing on a Cortex-M33.
pub type Units = u8;

/// A single on-or-off pulse of given duration, in Morse units.
///
/// `on == true`  → key down → LED on.
/// `on == false` → key up   → LED off.
///
/// Successive pulses **always alternate** between on and off when produced
/// by [`pulses`]: the iterator never emits two on-pulses or two off-pulses
/// in a row, which keeps consumer code simple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Pulse {
    /// Whether the key is down (the LED is on) for this pulse.
    pub on: bool,
    /// Length of the pulse in dit units.
    pub units: Units,
}

// ─────────────────────────────────────────────────────────────────────────
// Code-point table
// ─────────────────────────────────────────────────────────────────────────
//
// Each character maps to a slice of booleans:
//     `false` = dot, `true` = dash.
// The slices live in `.rodata`; on RP2350 that's flash, not SRAM, so the
// whole table costs essentially nothing in RAM.

/// Dot/dash pattern for a single character, or `None` if unsupported.
///
/// We use `false` for *dot* and `true` for *dash*. A bool slice is the
/// smallest representation that's still readable; a more compact bit-
/// packed form would save a few hundred bytes of flash but make the table
/// harder to audit, which is the wrong trade for this project today.
///
/// Letters, digits, and a handful of common punctuation are supported.
/// Anything else returns `None`; callers (such as [`Pulses`]) treat that
/// as "skip silently".
fn pattern(c: char) -> Option<&'static [bool]> {
    // Local aliases so the table reads as it would on a worksheet.
    // The Rust compiler folds these into the slice literals at compile
    // time; there is no runtime cost.
    const DOT: bool = false;
    const DASH: bool = true;

    Some(match c.to_ascii_uppercase() {
        // ── Letters ────────────────────────────────────────────────────
        'A' => &[DOT, DASH],
        'B' => &[DASH, DOT, DOT, DOT],
        'C' => &[DASH, DOT, DASH, DOT],
        'D' => &[DASH, DOT, DOT],
        'E' => &[DOT],
        'F' => &[DOT, DOT, DASH, DOT],
        'G' => &[DASH, DASH, DOT],
        'H' => &[DOT, DOT, DOT, DOT],
        'I' => &[DOT, DOT],
        'J' => &[DOT, DASH, DASH, DASH],
        'K' => &[DASH, DOT, DASH],
        'L' => &[DOT, DASH, DOT, DOT],
        'M' => &[DASH, DASH],
        'N' => &[DASH, DOT],
        'O' => &[DASH, DASH, DASH],
        'P' => &[DOT, DASH, DASH, DOT],
        'Q' => &[DASH, DASH, DOT, DASH],
        'R' => &[DOT, DASH, DOT],
        'S' => &[DOT, DOT, DOT],
        'T' => &[DASH],
        'U' => &[DOT, DOT, DASH],
        'V' => &[DOT, DOT, DOT, DASH],
        'W' => &[DOT, DASH, DASH],
        'X' => &[DASH, DOT, DOT, DASH],
        'Y' => &[DASH, DOT, DASH, DASH],
        'Z' => &[DASH, DASH, DOT, DOT],

        // ── Digits ─────────────────────────────────────────────────────
        '0' => &[DASH, DASH, DASH, DASH, DASH],
        '1' => &[DOT, DASH, DASH, DASH, DASH],
        '2' => &[DOT, DOT, DASH, DASH, DASH],
        '3' => &[DOT, DOT, DOT, DASH, DASH],
        '4' => &[DOT, DOT, DOT, DOT, DASH],
        '5' => &[DOT, DOT, DOT, DOT, DOT],
        '6' => &[DASH, DOT, DOT, DOT, DOT],
        '7' => &[DASH, DASH, DOT, DOT, DOT],
        '8' => &[DASH, DASH, DASH, DOT, DOT],
        '9' => &[DASH, DASH, DASH, DASH, DOT],

        // ── A few useful punctuation marks ─────────────────────────────
        '.' => &[DOT, DASH, DOT, DASH, DOT, DASH],
        ',' => &[DASH, DASH, DOT, DOT, DASH, DASH],
        '?' => &[DOT, DOT, DASH, DASH, DOT, DOT],
        '/' => &[DASH, DOT, DOT, DASH, DOT],

        // Anything else: caller decides what to do.
        _ => return None,
    })
}

// ─────────────────────────────────────────────────────────────────────────
// Iterator
// ─────────────────────────────────────────────────────────────────────────

/// Create a [`Pulses`] iterator that walks `text` and produces the Morse
/// timing sequence described in the [module docs](self).
///
/// Whitespace is treated as a word gap. Unknown characters are skipped.
/// The iterator borrows from `text`, so the string must outlive it.
pub fn pulses(text: &str) -> Pulses<'_> {
    Pulses {
        chars: text.chars(),
        pattern: &[],
        idx: 0,
        pending_gap: None,
        has_started: false,
        word_break_pending: false,
    }
}

/// Iterator over the Morse pulses that encode some text.
///
/// Construct with [`pulses`]. See the [module docs](self) for the timing
/// model.
#[derive(Debug, Clone)]
pub struct Pulses<'a> {
    /// Source character stream.
    chars: core::str::Chars<'a>,
    /// Dot/dash slice for the *currently-being-emitted* letter, or empty
    /// when we are between letters.
    pattern: &'static [bool],
    /// Index of the next dot/dash to emit within `pattern`.
    idx: usize,
    /// If `Some`, an off-pulse of this many units **will be emitted on the
    /// next call**. Set by step (2) for 1-unit inter-element gaps, and by
    /// step (3) for 3- and 7-unit inter-letter / inter-word gaps once we
    /// know a letter actually follows.
    pending_gap: Option<Units>,
    /// Set once we have emitted at least one on-pulse. We use this to
    /// suppress *leading* gaps so the output always starts with key-down.
    has_started: bool,
    /// We have seen one or more whitespace characters since the last
    /// emitted letter. If a letter then follows, the inter-letter gap is
    /// upgraded from 3 units to 7 (a word gap). If end-of-input follows
    /// instead, we drop the gap on the floor — trailing whitespace must
    /// not produce a trailing silent pulse.
    word_break_pending: bool,
}

impl<'a> Iterator for Pulses<'a> {
    type Item = Pulse;

    fn next(&mut self) -> Option<Self::Item> {
        // The state machine has three priorities, evaluated in order on
        // every call until it can return a pulse:
        //
        //   (1) If an off-pulse is queued, emit it.
        //   (2) Otherwise, if the current letter has dots/dashes left,
        //       emit the next one (on-pulse), queueing a 1-unit gap if
        //       it's not the final symbol of the letter.
        //   (3) Otherwise, advance the input by one character to load the
        //       next letter (or note a word break), then go round again.
        //
        // Steps (1) and (2) return a pulse. Step (3) only loops; it
        // returns `None` when the input runs out, which guarantees we
        // never emit a *trailing* off-pulse: a queued gap is only
        // committed when we have proof a letter follows.
        loop {
            // (1) Emit any pending off-pulse first.
            if let Some(units) = self.pending_gap.take() {
                return Some(Pulse { on: false, units });
            }

            // (2) Emit the next dot/dash of the current letter, if any.
            if self.idx < self.pattern.len() {
                let dash = self.pattern[self.idx];
                self.idx += 1;
                if self.idx < self.pattern.len() {
                    // There is another symbol after this one within the
                    // same letter, so schedule a 1-unit inter-element gap.
                    self.pending_gap = Some(1);
                }
                self.has_started = true;
                return Some(Pulse {
                    on: true,
                    units: if dash { 3 } else { 1 },
                });
            }

            // (3) Out of symbols in the current letter. Pull the next
            // character and decide what to do with it.
            //
            // Note: we never queue a gap here speculatively for a
            // whitespace character — only when a real letter follows do
            // we commit the inter-letter gap (3) or word gap (7). That's
            // what stops trailing whitespace from emitting a trailing
            // silent pulse.
            match self.chars.next() {
                None => return None,
                Some(c) if c.is_whitespace() => {
                    // Remember the word break. Multiple consecutive
                    // whitespace characters collapse into one — the flag
                    // is a simple bool, not a counter.
                    if self.has_started {
                        self.word_break_pending = true;
                    }
                }
                Some(c) => match pattern(c) {
                    None => {
                        // Unknown character: skip silently, but **do not**
                        // clear `word_break_pending` — a sequence like
                        // "A ~B" should still treat the gap between A
                        // and B as a word gap.
                    }
                    Some(pat) => {
                        self.pattern = pat;
                        self.idx = 0;
                        if self.has_started {
                            // Commit the gap now that we know a letter
                            // really does follow. 7 for word, 3 otherwise.
                            self.pending_gap = Some(if self.word_break_pending { 7 } else { 3 });
                        }
                        self.word_break_pending = false;
                    }
                },
            }
        }
    }
}

// `FusedIterator` is a marker trait that promises `next()` keeps returning
// `None` once it has returned `None` once. Our iterator naturally has that
// property — `self.chars.next()` is itself fused — so we declare it.
impl<'a> core::iter::FusedIterator for Pulses<'a> {}

// ─────────────────────────────────────────────────────────────────────────
// Inline tests
// ─────────────────────────────────────────────────────────────────────────
//
// These are simple smoke tests that live next to the code. The thorough
// behavioural tests are in `tests/morse.rs` so they exercise the public
// API the same way an external user would.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_no_pulses() {
        assert!(pulses("").next().is_none());
    }

    #[test]
    fn lone_e_is_one_unit_on() {
        let mut it = pulses("E");
        assert_eq!(it.next(), Some(Pulse { on: true, units: 1 }));
        assert_eq!(it.next(), None);
    }

    #[test]
    fn pattern_table_round_trips_alphabet() {
        // Every ASCII letter must have a non-empty pattern, and the
        // patterns must be made of at most 4 symbols (Morse alphabet
        // upper bound; digits and punctuation get more, hence "letters").
        for c in 'A'..='Z' {
            let p = pattern(c).unwrap_or_else(|| panic!("no pattern for {c}"));
            assert!(!p.is_empty());
            assert!(p.len() <= 4, "letter {c} has too many symbols");
        }
    }
}
