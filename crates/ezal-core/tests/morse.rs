//! Behavioural tests for the morse encoder.
//!
//! These tests live in `tests/` rather than inside the module so they
//! exercise the *public* API the same way an external consumer would.
//! They run on the host under `cargo test` and never see the embedded
//! target.
//!
//! The renderer helper turns the pulse stream into a visual string of
//! '#' (on) and ' ' (off), one character per Morse unit. That makes
//! expected outputs human-readable and makes any test failure trivial
//! to debug — you can almost hear the dits and dahs.

use ezal_core::morse;

/// Render the morse pulses for `text` as a string of `#` (key down) and
/// ` ` (key up), with one character per Morse unit. Convenient for
/// `assert_eq!` against a literal.
fn render(text: &str) -> String {
    morse::pulses(text)
        .flat_map(|p| std::iter::repeat_n(if p.on { '#' } else { ' ' }, p.units as usize))
        .collect()
}

#[test]
fn empty_text_emits_nothing() {
    assert_eq!(render(""), "");
}

#[test]
fn single_dot_is_one_unit_on() {
    // E = .
    assert_eq!(render("E"), "#");
}

#[test]
fn single_dash_is_three_units_on() {
    // T = -
    assert_eq!(render("T"), "###");
}

#[test]
fn a_is_dot_short_gap_dash() {
    // A = .-   →   on(1) off(1) on(3)
    assert_eq!(render("A"), "# ###");
}

#[test]
fn inter_letter_gap_is_three_units_off() {
    // EE = . / .   →   on(1) off(3) on(1)
    assert_eq!(render("EE"), "#   #");
}

#[test]
fn word_gap_is_seven_units_off() {
    // "E E"   →   on(1) off(7) on(1)
    assert_eq!(render("E E"), "#       #");
}

#[test]
fn case_is_normalised() {
    assert_eq!(render("hello"), render("HELLO"));
    assert_eq!(render("HeLlO"), render("HELLO"));
}

#[test]
fn unknown_characters_are_silently_skipped() {
    // '~' has no Morse code in our table, so "E~T" should encode like "ET".
    assert_eq!(render("E~T"), render("ET"));
}

#[test]
fn leading_whitespace_does_not_emit_a_gap() {
    // A leading space must NOT produce a phantom 7-unit silence — that
    // would mean the LED waits before saying anything at all.
    assert_eq!(render("  E"), render("E"));
}

#[test]
fn trailing_whitespace_does_not_emit_a_gap() {
    // Trailing space → no spurious trailing off-pulse.
    assert_eq!(render("E   "), render("E"));
}

#[test]
fn multiple_spaces_collapse_to_one_word_gap() {
    // Two or more consecutive spaces still encode as one word gap (7).
    assert_eq!(render("E   E"), render("E E"));
}

#[test]
fn output_always_starts_with_an_on_pulse() {
    // Whatever the input, the very first pulse must be key-down so the
    // listener / observer hears or sees something happen immediately.
    for input in ["E", "T", "HELLO", "  HELLO", "?A", "73"] {
        let first = morse::pulses(input).next();
        assert!(
            first.is_some_and(|p| p.on),
            "input {input:?} did not start with key-down"
        );
    }
}

#[test]
fn output_always_ends_with_an_on_pulse() {
    // Symmetrically, the last pulse must be key-down: trailing silence
    // is implicit, never an emitted off-pulse.
    for input in ["E", "T", "HELLO", "HELLO  ", "?A", "73"] {
        let last = morse::pulses(input).last();
        assert!(
            last.is_some_and(|p| p.on),
            "input {input:?} did not end with key-down"
        );
    }
}

#[test]
fn on_and_off_pulses_strictly_alternate() {
    // Two consecutive pulses must differ in `on`. This is the invariant
    // that lets firmware just toggle a GPIO on every pulse without
    // tracking state.
    let pulses: Vec<_> = morse::pulses("HELLO WORLD").collect();
    for window in pulses.windows(2) {
        assert_ne!(
            window[0].on, window[1].on,
            "two consecutive pulses had the same on-state: {window:?}"
        );
    }
}

#[test]
fn hello_world_matches_canonical_encoding() {
    // The full expected timing for "HELLO WORLD", built from the
    // canonical Morse alphabet. We use `concat!` so the source reads
    // letter by letter and reviewers can sanity-check each one.
    //
    // Format: '#' = 1 unit on, ' ' = 1 unit off.
    let expected = concat!(
        // H = . . . .
        "# # # #",
        "   ", // inter-letter (3 off)
        // E = .
        "#",
        "   ",
        // L = . - . .
        "# ### # #",
        "   ",
        // L = . - . .
        "# ### # #",
        "   ",
        // O = - - -
        "### ### ###",
        "       ", // word gap (7 off)
        // W = . - -
        "# ### ###",
        "   ",
        // O = - - -
        "### ### ###",
        "   ",
        // R = . - .
        "# ### #",
        "   ",
        // L = . - . .
        "# ### # #",
        "   ",
        // D = - . .
        "### # #",
    );
    assert_eq!(render("HELLO WORLD"), expected);
}

#[test]
fn digit_five_encodes_as_five_dots() {
    // 5 = .....   →   on(1) off(1) on(1) off(1) on(1) off(1) on(1) off(1) on(1)
    assert_eq!(render("5"), "# # # # #");
}

#[test]
fn digit_zero_encodes_as_five_dashes() {
    // 0 = -----
    assert_eq!(render("0"), "### ### ### ### ###");
}

#[test]
fn sos_encodes_as_three_dots_three_dashes_three_dots() {
    // S O S = ... --- ...
    let expected = concat!(
        "# # #",       // S
        "   ",         // inter-letter
        "### ### ###", // O
        "   ",         // inter-letter
        "# # #",       // S
    );
    assert_eq!(render("SOS"), expected);
}
