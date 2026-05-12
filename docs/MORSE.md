# The "hello LED" demo, line by line

This document walks through the step-one firmware — the one that blinks
**HELLO WORLD** in international Morse code on the Pico 2's onboard LED.
It is aimed at someone who knows Rust but is new to embedded Rust on the
RP2350.

The demo is split across two crates:

- [`crates/ezal-core/src/morse.rs`](../crates/ezal-core/src/morse.rs)
  — *pure* logic. Turns a `&str` into an iterator of on/off pulses.
- [`crates/ezal-firmware/src/main.rs`](../crates/ezal-firmware/src/main.rs)
  — *firmware*. Walks the iterator and toggles GPIO 25 in real time.

The split is deliberate. Everything in `ezal-core` runs on the host under
`cargo test`; everything in `ezal-firmware` only runs on the chip. The
core/firmware boundary is the most important architectural idea in the
whole project — see [ARCHITECTURE.md](ARCHITECTURE.md).

## Morse 101

International Morse Code maps each letter to a sequence of *dots* and
*dashes*. Letters are joined into words with gaps:

| element                              | duration  |
|--------------------------------------|-----------|
| dot (also "dit")                     | 1 unit on |
| dash (also "dah")                    | 3 units on |
| gap between dots/dashes of a letter  | 1 unit off |
| gap between two letters of a word    | 3 units off |
| gap between two words                | 7 units off |

The actual length of a "unit" is the transmitter's choice. We use **150 ms**
in firmware (see `UNIT_MS` in `main.rs`). At that rate, "HELLO WORLD" takes
about 16 seconds.

For example, "SOS" is `...---...` with gaps:

```
S      O          S
.   .   .   -   -   -   .   .   .
on 1 off 1 on 1 off 1 on 1 off 3 on 3 off 1 on 3 off 1 on 3 off 3 on 1 off 1 on 1 off 1 on 1
```

…or, in `#`/` ` notation:

```
# # # ### ### ### # # #
```

## The encoder (`ezal-core/morse.rs`)

The encoder produces a stream of `Pulse` values:

```rust
pub struct Pulse {
    pub on: bool,        // key down (true) or key up (false)
    pub units: u8,       // duration in dit times
}
```

The public entry point is:

```rust
pub fn pulses(text: &str) -> Pulses<'_>;
```

…which returns an iterator with a few invariants:

1. **Pulses always alternate.** Two on-pulses never appear in a row, nor
   do two off-pulses. The firmware can blindly toggle the LED on every
   pulse, knowing the state will be right.
2. **The first pulse is always on.** Leading whitespace is dropped, so
   there is no silent gap at the start.
3. **The last pulse is always on.** Trailing whitespace is dropped, so
   there is no trailing silence.

These invariants are tested explicitly in
[`crates/ezal-core/tests/morse.rs`](../crates/ezal-core/tests/morse.rs).
They make the firmware code trivial.

### The state machine

`Pulses::next()` returns one pulse per call. Inside, it cycles through
three priorities:

1. **Is there a pending off-pulse?** Inter-element gaps, inter-letter
   gaps, and inter-word gaps are all just *off* pulses of differing
   length, so they share one slot: `pending_gap: Option<u8>`. If it's
   `Some`, take it and return.
2. **Are there dots/dashes left in the current letter?** Emit the next
   one (an on-pulse), schedule a 1-unit off-pulse for next time *if*
   there's another symbol after it.
3. **Pull the next input character.** Whitespace queues a 7-unit gap;
   a known letter loads its pattern and queues a 3-unit gap (unless a
   bigger gap is already queued); an unknown character is dropped.

It is intentionally a small, explicit state machine rather than
generators or `async` cleverness. Embedded code with no allocator should
err on the side of "easy to reason about".

### The code-point table

The mapping from `'A'` → `[DOT, DASH]` etc. is a simple `match` returning
a `&'static [bool]`. The slices live in `.rodata` (flash on the Pico 2),
so the whole alphabet costs essentially no SRAM. Unknown characters
return `None` rather than panicking.

We could pack the dots/dashes into a `u8` per letter and save a couple
of hundred bytes of flash, but that would obscure the table for
no real benefit on a 4 MiB part. *Optimise for readability first; the
optimiser is faster than you think.*

## The firmware (`ezal-firmware/src/main.rs`)

### Crate attributes

```rust
#![no_std]   // no Rust standard library
#![no_main]  // we don't have a normal `fn main() -> ()`
```

`no_std` removes `std`. We still have `core` (intrinsics, slices, iterator
combinators, …) and `alloc` if we ever enable an allocator (we don't).

`no_main` says we don't have a conventional `fn main()`; the program's
real entry point comes from `cortex-m-rt` (the runtime crate that
provides the ARM Cortex-M reset handler and ISR vector table). The
`#[embassy_executor::main]` macro further wraps our `async fn main` in
an executor that runs it.

### The image header

The RP2350 boot ROM refuses to run a binary that doesn't start with a
valid "image definition" block. The Cargo manifest enables the
`imagedef-secure-exe` feature on `embassy-rp`, which makes the HAL emit
the right bytes for us automatically. Internally, that feature expands
to roughly:

```rust
#[unsafe(link_section = ".start_block")]
#[used]
static IMAGE_DEF: ImageDef = ImageDef::secure_exe();
```

…where the `#[link_section = ".start_block"]` attribute tells the
linker to put this static at the very beginning of flash, and `#[used]`
tells LTO not to garbage-collect it just because nothing else references
it. You can confirm the block landed correctly:

```
$ llvm-objdump -t target/.../ezal-firmware | grep IMAGE_DEF
10000000 g O .start_block 00000014 _ZN10embassy_rp...IMAGE_DEF...
```

If you ever need to ship a *signed* image, swap the feature flag for
`imagedef-nonsecure-exe`. If you want to write the static yourself
(say, for a custom block layout), set `imagedef-none` to opt out of
embassy's auto-insertion and supply your own.

### Logging

```rust
use defmt_rtt as _;
use panic_probe as _;
```

These `use … as _` lines link the crates in without naming any of their
items. They install:

- a global `defmt::Logger` that transmits log frames over RTT,
- a panic handler that, on `panic!()`, formats the message via defmt and
  halts the core (so we see panics as log lines instead of silent
  reboots).

### The `Spawner` and `Output`

```rust
#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let mut led = Output::new(p.PIN_25, Level::Low);
    ...
}
```

`embassy_rp::init(Default::default())` consumes the global Peripherals
singleton and hands us back a struct with one field per chip resource —
`p.PIN_25`, `p.UART0`, `p.PWM_SLICE0`, and so on. Rust's move semantics
then guarantee that **only one place** in the program can ever own
`PIN_25`; if you try to call `Output::new(p.PIN_25, …)` twice, it won't
compile.

`Output::new` configures the pin as a push-pull output and gives us a
type with `.set_high() / .set_low() / .set_level()`. Behind the scenes
it's writing to a single register, but you don't need to know that to
use it.

### The blink loop

```rust
loop {
    for pulse in morse::pulses(MESSAGE) {
        led.set_level(if pulse.on { Level::High } else { Level::Low });
        Timer::after(Duration::from_millis(UNIT_MS * u64::from(pulse.units))).await;
    }
    led.set_low();
    Timer::after(INTER_MESSAGE_PAUSE).await;
}
```

Three things to notice:

1. **No `delay()`.** `Timer::after(...).await` is non-blocking. While we
   wait, the Embassy executor could run another task; the chip can even
   sleep until the timer fires (a single WFI instruction), which saves
   power. We currently have no other task, but the *structure* matters
   — the rotator firmware will need exactly this shape with many tasks
   sharing the executor.
2. **The encoder is iterator-shaped, not callback-shaped.** That makes
   it trivial to combine with `for … in` and `.await`. A callback-based
   API would have made the `.await` inside the callback awkward.
3. **The morse string is `&'static str`.** It lives in flash, not RAM.

That's the entire firmware. Everything else is just plumbing.

## Trying changes

Some quick experiments to get the feel of it:

- Change `UNIT_MS` from 150 to 80 to hear the difference (well, see it).
- Change `MESSAGE` to `"73 DE EZAL"` (ham radio Q-code for "best regards
  from ezal").
- Change `LED` to `Level::High` on init and watch the first pulse
  invert — proving the alternation invariant.
- In `Cargo.toml`, swap `imagedef-secure-exe` for `imagedef-none`,
  rebuild, flash, and watch the chip refuse to run (the boot ROM can't
  find a valid image header). Restore the feature flag.

These are good for an intuition of what each piece is doing.
