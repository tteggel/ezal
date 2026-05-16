"""
ezal/design/circuit.py
=======================

Schemdraw source for the planned Raspberry Pi Pico 2 ↔ Yaesu G-5500
interface circuit. This is the canonical schematic for the project:
edit this file when the circuit changes, then re-render to refresh
`circuit.svg` (and `circuit.png`).

Topology
--------

  ┌────────┐  4× GPIO  ┌──────────┐  4× switch closure  ┌────────┐
  │ Pico 2 │──────────▶│ 4× 2N3904│────────────────────▶│ G-5500 │
  │        │           └──────────┘                     │ DIN-8  │
  │        │  I2C      ┌──────────┐  2× analog          │        │
  │        │──────────▶│ ADS1015  │◀────[2× R-divider]──│        │
  └────────┘           └──────────┘                     └────────┘

The four direction inputs (G-5500 pins 2/3/4/5) are driven straight from
Pico GPIOs through 2N3904 open-collector switches. The two feedback
outputs (G-5500 pins 1 & 6) are read by an Adafruit ADS1015 12-bit I²C
ADC, fed through a per-channel resistor-divider that scales the 2.0–4.5 V
feedback down into a range the ADS1015 can sample comfortably.

Rendering
---------
    pip install --user schemdraw matplotlib
    python3 circuit.py

Produces `circuit.svg` (always) and `circuit.png` (best-effort, needs
matplotlib). Both files are checked in so GitHub renders the schematic
without the reader needing a Python environment.

Notes
-----
* GPIO assignments are *suggestions* — pick whatever is convenient on
  the board. I²C0 defaults to GP4 (SDA) / GP5 (SCL); use any I²C-capable
  pair if those conflict.
* Resistor values are picked from first principles for the G-5500 /
  G-5500DC interface (same external-control pinout on both):
    direction channels — 2.2 K series base, 10 K base-to-emitter pull-down
                         (gives ~1.1 mA I_B, robust saturation of the 2N3904)
    indicator LEDs    — 470 Ω + standard 2 V Vf LED on each GPIO. Lights
                         up on direction command alone, so the bench
                         shows which channel is switched without the
                         rotator connected. Pico sinks ~4 mA total per
                         channel (1.1 mA base + 2.8 mA LED) — within the
                         12 mA per-pin GPIO budget.
    feedback dividers — 68 K upper, 82 K lower for **both** channels.
                        The G-5500's NJM2902 op-amp output drives a 33 K
                        series resistor (R6010 / R6011 in the service
                        manual) before the external DIN, so the effective
                        ratio seen at the ADS1015 is 82 / (33 + 68 + 82)
                        ≈ 0.448. That puts 4.5 V at the rotator's full
                        scale into 2.02 V at the ADC — 99% of the ±2.048 V
                        FSR PGA setting, ~1 mV LSB.
    feedback filter cap — 100 nF in parallel with R_lower
                        (≈ 35 Hz LP with the ~45 K Thévenin impedance,
                        rejects 50/60 Hz pickup at no cost to tracking
                        bandwidth; the G-5500's own 220 µH + 0.01 µF LC
                        on the output handles the RFI-rate stuff)
    I²C pull-ups        — 4.7 K to 3.3 V on SDA and SCL
* The ADS1015's ADDR pin is tied to GND for the default address 0x48.
* These values assume the G-5500's direction inputs sink a few mA at
  logic-low (typical for Yaesu controllers).
* The feedback-divider sizing comes from the service-manual page showing
  an NJM2902 op-amp driving a 33 K series resistor to the external DIN.
  If a different unit / revision shows a different series value, the
  divider math is in the comment above the FEEDBACK_CHANNELS table.
"""

import schemdraw
import schemdraw.elements as elm


# ── Per-channel data ────────────────────────────────────────────────────
# Order: top of schematic → bottom.
DIRECTION_CHANNELS = [
    # (Pico pin label, G-5500 pin label, label for what this does)
    ("GP10", "pin 2", "CW  (right)"),
    ("GP11", "pin 4", "CCW (left)"),
    ("GP12", "pin 3", "UP"),
    ("GP13", "pin 5", "DOWN"),
]

FEEDBACK_CHANNELS = [
    # (ADS1015 input, G-5500 pin, R_upper, R_lower, what it measures)
    # 68 K / 82 K accounts for the G-5500's NJM2902 op-amp + 33 K series
    # output resistor (R6010 / R6011 in the service manual). Net divider
    # math: V_ADC = V_pot × 82 / (33 + 68 + 82) ≈ 0.448. See HARDWARE.md.
    ("A0", "pin 1", "68 K", "82 K", "elevation (0°–180°)"),
    ("A1", "pin 6", "68 K", "82 K", "azimuth   (0°–450°)"),
]


# ── Layout constants ────────────────────────────────────────────────────
# Vertical positions (schematic grows downwards from the title).
TITLE_Y    = 3.0
DIR_Y0     = 0.0
DIR_PITCH  = 6.0   # channel-to-channel; bumped from 5 to keep labels breathable.
I2C_Y0     = DIR_Y0 - len(DIRECTION_CHANNELS) * DIR_PITCH - 1.5  # ADS1015 zone
FB_PITCH   = 3.5

# Horizontal columns.
X_PICO        = 0.0    # Pico pin label sits at x=0.
X_LED_BRANCH  = 1.5    # branch point for the per-channel indicator LED.
X_R_SERIES    = 3.0    # left end of the 2.2 K base resistor.
X_BASE_NODE   = 5.5    # base of the NPN (and top of the pull-down).
X_DIN         = 13.0   # DIN pin label / wire to the connector.

# ADS1015 chip block (positioned in the feedback half of the schematic).
# We make the chip deliberately tall so the two used analog pins (A0 at
# the top, A1 at the bottom) sit far enough apart for each one's
# pull-down resistor to land in its own zone with no visual collision.
ADS_W = 3.5
ADS_H = 6.0
ADS_X = 5.0
ADS_Y = I2C_Y0 - ADS_H - 0.5


# ── Direction-switch row ────────────────────────────────────────────────
def direction_channel(d: schemdraw.Drawing, idx: int, gpio: str,
                      din_pin: str, what: str) -> None:
    """Draw one open-collector NPN direction-switch channel.

    Each channel also carries an **indicator LED** hanging off the GPIO
    wire. The LED is driven directly by the Pico GPIO (not by the
    switching transistor), so it lights up whenever the firmware
    commands that direction — independent of whether the rotator is
    actually plugged in. Handy for bench testing and for sanity-checking
    a misbehaving deadband / hysteresis loop visually.
    """
    y = DIR_Y0 - idx * DIR_PITCH

    # Pico GPIO terminal on the left. The label is placed with explicit
    # coordinates instead of loc/ofst so it sits clearly clear of the dot.
    d += elm.Dot(open=True).at((X_PICO, y))
    d += elm.Label().label(gpio, fontsize=10).at((X_PICO - 0.6, y))

    # GPIO wire jogs through a branch point so we can hang the indicator
    # LED off it before continuing to the base-resistor network.
    led_branch_x = X_LED_BRANCH
    d += elm.Line().endpoints((X_PICO, y), (led_branch_x, y))
    d += elm.Dot().at((led_branch_x, y))
    d += elm.Line().endpoints((led_branch_x, y), (X_R_SERIES, y))

    # Indicator LED branch: GPIO → 470 Ω → LED → GND. At 3.3 V drive
    # and ~2 V LED forward drop, this sinks ~2.8 mA — bright enough to
    # see in a lit room and well within the Pico GPIO's drive budget
    # (the 2.2 K base resistor below takes another ~1.1 mA, total ~4 mA).
    #
    # Labels for the resistor and LED are positioned with explicit .at()
    # coords because schemdraw's loc=left placement on a vertical Resistor
    # / LED puts the label at the body's mid-point, which for short bodies
    # collides with the next element underneath.
    d += (r_led := elm.Resistor().down().at((led_branch_x, y)).length(1.1))
    d += elm.Label().label("470 Ω", fontsize=8).at(
        (led_branch_x - 0.6, y - 0.55)
    )
    d += (led := elm.LED().down().at(r_led.end).length(0.9))
    d += elm.Label().label(f"D{idx + 1}", fontsize=8).at(
        (led_branch_x - 0.6, y - 1.55)
    )
    d += elm.Ground().at(led.end)

    # 2.2 K series base resistor: gives ~1.1 mA I_B for 3.3 V GPIO drive,
    # plenty to saturate a 2N3904 driving the G-5500 direction input.
    d += elm.Resistor().endpoints((X_R_SERIES, y), (X_BASE_NODE, y)).label("2.2 K")

    # Base node.
    d += elm.Dot().at((X_BASE_NODE, y))

    # 10 K base-pull-down to GND: holds the base low while the Pico GPIO
    # is high-Z (boot/reset) but doesn't steal appreciable I_B once on.
    # Label on the LEFT to stay clear of the transistor's body and
    # part-number labels to the right of the base node.
    d += (r_pd := elm.Resistor().down().at((X_BASE_NODE, y))
          .length(1.8).label("10 K", loc="left"))
    d += elm.Ground().at(r_pd.end)

    # NPN transistor, base anchored at the base node, body extending right.
    d += (q := elm.BjtNpn(circle=True).right().anchor("base")
          .at((X_BASE_NODE, y))
          .label(f"Q{idx + 1}", loc="top", ofst=(0, 0.15)))
    # 2N3904 part-number under the body, with extra clearance so the
    # 10 K label on the left of the pull-down resistor doesn't collide.
    d += elm.Label().label("2N3904", fontsize=8).at(
        ((q.collector[0] + q.emitter[0]) / 2, y - 1.5)
    )

    # Emitter → GND.
    d += elm.Line().endpoints(q.emitter, (q.emitter[0], y - 1.8))
    d += elm.Ground().at((q.emitter[0], y - 1.8))

    # Collector → DIN pin (jog up to the row's y-line, then run right).
    d += elm.Line().endpoints(q.collector, (q.collector[0], y))
    d += elm.Line().endpoints((q.collector[0], y), (X_DIN, y))
    d += elm.Dot(open=True).at((X_DIN, y))
    d += elm.Label().label(f"DIN {din_pin}\n{what}", loc="right",
                           ofst=(0.2, 0), fontsize=9)


# ── ADS1015 block ───────────────────────────────────────────────────────
def ads1015_block(d: schemdraw.Drawing) -> tuple[dict, dict]:
    """Draw the ADS1015 as a labelled rectangle. Returns
    `(left_pin_positions, right_pin_positions)` mapping pin name → (x, y).

    Layout choices:
      * The chip is drawn tall so A0 (top-right) and A1 (bottom-right)
        sit far apart, leaving room for each one's pull-down resistor
        below it.
      * Only A0 and A1 are drawn — A2/A3 are unconnected on the board
        and would just be visual noise.
    """
    x0, y0 = ADS_X, ADS_Y
    x1, y1 = ADS_X + ADS_W, ADS_Y + ADS_H
    # Build the chip body from four explicit lines. (elm.Rect is documented
    # to do the same thing, but on some backends it gets drawn outside the
    # natural drawing flow and ends up displaced — four explicit lines
    # always land where we put them.)
    d += elm.Line().endpoints((x0, y0), (x1, y0))
    d += elm.Line().endpoints((x1, y0), (x1, y1))
    d += elm.Line().endpoints((x1, y1), (x0, y1))
    d += elm.Line().endpoints((x0, y1), (x0, y0))

    # Chip part-number label, centred.
    d += elm.Label().label(
        "ADS1015\nI²C ADC\n(addr 0x48)",
        loc="center", fontsize=10,
    ).at(((x0 + x1) / 2, (y0 + y1) / 2))

    # ── Left-side pins ──────────────────────────────────────────────
    left_names = ["SDA", "SCL", "VDD", "GND", "ADDR"]
    n = len(left_names)
    left: dict[str, tuple[float, float]] = {}
    for i, name in enumerate(left_names):
        py = y1 - (i + 0.5) * (ADS_H / n)
        left[name] = (x0, py)
        d += elm.Line().endpoints((x0 - 0.3, py), (x0, py))
        d += elm.Label().label(name, loc="right", ofst=(-0.1, 0),
                               fontsize=8).at((x0, py))

    # ── Right-side pins — only A0 (top) and A1 (bottom) ────────────
    right: dict[str, tuple[float, float]] = {}
    for name, py in (("A0", y1 - 0.6), ("A1", y0 + 0.6)):
        right[name] = (x1, py)
        d += elm.Line().endpoints((x1, py), (x1 + 0.3, py))
        d += elm.Label().label(name, loc="left", ofst=(0.1, 0),
                               fontsize=8).at((x1, py))

    return left, right


# ── I²C wiring + pull-ups ───────────────────────────────────────────────
def i2c_bus(d: schemdraw.Drawing, ads_left: dict) -> None:
    """Wire the ADS1015's I²C and power pins to the Pico-side labels.

    Layout:

        3.3 V rail ─●────────●─────────────
                    │        │
                  4.7 K    4.7 K           ┌─ ADS1015 ─┐
                    │        │             │  SDA     ─┤
        GP4 ●───────┴────────│─────────────┤           │
        GP5 ●────────────────┴─────────────┤  SCL     ─┤
                                           │  VDD     ─┤── 3.3 V
                                           │  GND     ─┤── GND
                                           │  ADDR    ─┤── GND
                                           └───────────┘
    """
    sda_x, sda_y = ads_left["SDA"]
    scl_x, scl_y = ads_left["SCL"]
    vdd_x, vdd_y = ads_left["VDD"]
    gnd_x, gnd_y = ads_left["GND"]
    addr_x, addr_y = ads_left["ADDR"]

    # Horizontal SDA / SCL bus lines: ADS1015 → Pico label.
    # Labels positioned with explicit .at() coords (and shortened from
    # "GP4 (SDA)" to "GP4 SDA") so matplotlib's auto-bbox doesn't crop
    # them at the canvas's left edge.
    d += elm.Line().endpoints((sda_x - 0.3, sda_y), (X_PICO, sda_y))
    d += elm.Dot(open=True).at((X_PICO, sda_y))
    d += elm.Label().label("GP4  SDA", fontsize=10).at((X_PICO - 1.0, sda_y))

    d += elm.Line().endpoints((scl_x - 0.3, scl_y), (X_PICO, scl_y))
    d += elm.Dot(open=True).at((X_PICO, scl_y))
    d += elm.Label().label("GP5  SCL", fontsize=10).at((X_PICO - 1.0, scl_y))

    # Common 3.3 V rail running horizontally above the SDA line. Two
    # pull-ups hang down from it onto SDA and SCL respectively.
    rail_y = sda_y + 2.0
    sda_pu_x = X_PICO + 1.2
    scl_pu_x = X_PICO + 2.4

    d += elm.Line().endpoints((X_PICO, rail_y), (sda_x - 0.3, rail_y)).color("dimgray")
    d += elm.Label().label("3.3 V", loc="left", ofst=(0.1, 0)).at((X_PICO, rail_y))

    # SDA pull-up.
    d += elm.Dot().at((sda_pu_x, rail_y))
    d += elm.Resistor().endpoints((sda_pu_x, rail_y), (sda_pu_x, sda_y)).label("4.7 K", loc="right")
    d += elm.Dot().at((sda_pu_x, sda_y))

    # SCL pull-up.
    d += elm.Dot().at((scl_pu_x, rail_y))
    d += elm.Resistor().endpoints((scl_pu_x, rail_y), (scl_pu_x, scl_y)).label("4.7 K", loc="right")
    d += elm.Dot().at((scl_pu_x, scl_y))

    # VDD → 3.3 V (re-uses the rail).
    d += elm.Line().endpoints((vdd_x - 0.3, vdd_y), (vdd_x - 0.8, vdd_y))
    d += elm.Line().endpoints((vdd_x - 0.8, vdd_y), (vdd_x - 0.8, rail_y))
    d += elm.Dot().at((vdd_x - 0.8, rail_y))

    # GND → ground symbol (just below the chip pin).
    d += elm.Line().endpoints((gnd_x - 0.3, gnd_y), (gnd_x - 0.8, gnd_y))
    d += elm.Line().endpoints((gnd_x - 0.8, gnd_y), (gnd_x - 0.8, gnd_y - 0.8))
    d += elm.Ground().at((gnd_x - 0.8, gnd_y - 0.8))

    # ADDR → GND (tying ADDR to GND sets the I²C address to 0x48).
    d += elm.Line().endpoints((addr_x - 0.3, addr_y), (addr_x - 0.8, addr_y))
    d += elm.Line().endpoints((addr_x - 0.8, addr_y), (addr_x - 0.8, addr_y - 0.8))
    d += elm.Ground().at((addr_x - 0.8, addr_y - 0.8))


# ── Feedback dividers ───────────────────────────────────────────────────
def feedback_divider(d: schemdraw.Drawing, idx: int, ads_pin_xy: tuple,
                     din_pin: str, r_up: str, r_lo: str, what: str) -> None:
    """One resistor-divider channel between an ADS1015 input and a DIN pin.

    Topology:

        chip pin ●──tap──[R_up]──── DIN pin (G-5500 feedback, 2.0–4.5 V)
                  │
                  ●─────────●
                  │         │
                 R_lo      100 nF
                  │         │
                  ●─────────●
                       │
                      GND

    The 100 nF cap in parallel with R_lo gives a ≈ 350 Hz low-pass
    against 50/60 Hz pickup and switching noise; the corner is well
    above any realistic mechanical bandwidth of the rotator.
    """
    ax, ay = ads_pin_xy
    tap_y = ay
    tap_x = ax + 0.3

    # Tap dot on the chip pin stub.
    d += elm.Dot().at((tap_x, tap_y))

    # R_upper: tap → DIN pin (horizontal).
    d += elm.Resistor().endpoints((tap_x, tap_y), (X_DIN, tap_y)).label(r_up)
    d += elm.Dot(open=True).at((X_DIN, tap_y))
    d += elm.Label().label(f"DIN {din_pin}\n{what}", loc="right",
                           ofst=(0.1, 0), fontsize=9)

    # The two parallel-to-GND elements (R_lo and 100 nF) live in a
    # branch zone just below the tap so they don't share the y-line
    # with R_up.
    branch_y = tap_y - 0.6
    r_lo_x   = tap_x + 1.0
    cap_x    = tap_x + 2.0

    # Vertical stub from the tap down to the branch bar.
    d += elm.Line().endpoints((tap_x, tap_y), (tap_x, branch_y))
    # Horizontal branch bar, from below the tap out to the cap column.
    d += elm.Line().endpoints((tap_x, branch_y), (cap_x, branch_y))
    d += elm.Dot().at((r_lo_x, branch_y))
    d += elm.Dot().at((cap_x, branch_y))

    # R_lo straight down.
    d += (r := elm.Resistor().down().at((r_lo_x, branch_y))
          .length(1.5).label(r_lo, loc="left"))

    # 100 nF cap in parallel with R_lo.
    d += (c := elm.Capacitor().down().at((cap_x, branch_y))
          .length(1.5).label("100 nF", loc="right"))

    # Bottom bar joining R_lo and the cap, with a single GND symbol.
    d += elm.Line().endpoints(r.end, c.end)
    mid_x = (r_lo_x + cap_x) / 2
    d += elm.Ground().at((mid_x, r.end[1]))


# ── Common ground ───────────────────────────────────────────────────────
def common_ground(d: schemdraw.Drawing) -> None:
    """Pico GND ↔ G-5500 DIN pin 8."""
    y = ADS_Y - 3.5
    d += elm.Dot(open=True).at((X_PICO, y))
    d += elm.Label().label("Pico GND", fontsize=10).at((X_PICO - 1.0, y))
    d += elm.Line().endpoints((X_PICO, y), (X_DIN, y)).color("dimgray")
    d += elm.Dot(open=True).at((X_DIN, y))
    d += elm.Label().label("DIN pin 8\n(common ground)",
                           loc="right", ofst=(0.2, 0), fontsize=9)


# ── Title block ─────────────────────────────────────────────────────────
def title(d: schemdraw.Drawing) -> None:
    d += elm.Label().label(
        "Pico 2 ↔ Yaesu G-5500 / G-5500DC interface",
        loc="top", fontsize=14,
    ).at((X_DIN / 2, TITLE_Y))
    d += elm.Label().label(
        "Top: 4× 2N3904 direction switches (G-5500 DIN pins 2/3/4/5) + per-channel indicator LEDs (D1–D4).\n"
        "Bottom: Adafruit ADS1015 I²C ADC reads the 2.0–4.5 V position feedback (pins 1, 6) via 2× resistor dividers.\n"
        "See docs/HARDWARE.md for component-value derivation and the G-5500-side circuit it interfaces with.",
        loc="top", fontsize=8, color="dimgray",
    ).at((X_DIN / 2, TITLE_Y - 1.4))


def build(filename: str | None, backend: str = "svg") -> None:
    schemdraw.use(backend)
    with schemdraw.Drawing(file=filename, show=False) as d:
        d.config(unit=1.6, fontsize=10, lw=1.1)

        title(d)

        for i, ch in enumerate(DIRECTION_CHANNELS):
            direction_channel(d, i, *ch)

        ads_left, ads_right = ads1015_block(d)
        i2c_bus(d, ads_left)

        for i, ch in enumerate(FEEDBACK_CHANNELS):
            ads_pin_name, din_pin, r_up, r_lo, what = ch
            feedback_divider(d, i, ads_right[ads_pin_name],
                             din_pin, r_up, r_lo, what)

        common_ground(d)


def main() -> None:
    # SVG via the default text-only backend (always works).
    build("circuit.svg", backend="svg")

    # PNG via matplotlib backend (best-effort).
    try:
        build("circuit.png", backend="matplotlib")
    except Exception as exc:  # noqa: BLE001
        print(f"PNG render skipped: {exc}")


if __name__ == "__main__":
    main()
