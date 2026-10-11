# Position calibration

ezal converts the G-5500's two feedback voltages into angles with a linear
two-point calibration for each axis. The manual firmware uses that mapping
with the real ADS1015. For now the four installed-system endpoints are
deliberately hard-coded; this document is the procedure for replacing them
with measurements from your unit.

The planned tracking installation has an 80 cm parabolic dish with a custom
helical feed and permits over-the-top movement. Its full-travel calibration
and pointing tolerance remain unmeasured. Follow this procedure before using
that extra travel; the existing firmware still provides manual control and
advisory prediction. The [tracking design](PASS_PREDICTION.md#planned-tracking)
and [provisional motor profile](HARDWARE.md#provisional-tracking-profile)
describe assumptions for future tracking, not measured installation values.

## Current hard-coded values

The single source of truth is `HARD_CODED_CALIBRATION` in
[`crates/ezal-core/src/position.rs`](../crates/ezal-core/src/position.rs).

| ADC channel | G-5500 signal | mechanical range | placeholder ADC range |
|-------------|---------------|------------------|-----------------------|
| A0          | elevation     | 0°–90°           | 1281 mV at 0°, 58 mV at 90° |
| A1          | azimuth       | 0°–360°          | 114 mV at 0°, 2033 mV at 360° |

These checked-in installation values govern manual movement.
The mapping accepts at most 25 mV of endpoint noise before declaring feedback
invalid and inhibiting movement.

## Safety setup

Do this with the antenna installation visible, the coax and other snag-prone
cables managed for the full travel, and one person ready to remove G-5500
motor power. Never use software as the only protection against an unverified
end stop. Start with the Pico disconnected from the four direction inputs and
confirm that all direction GPIOs are low during reset.

The firmware boots with all direction outputs idle. Build and flash it for
manual calibration:

```bash
cargo run -p ezal-firmware --release
```

The dashboard's buttons are leased controls. A held
button refreshes its command every 150 ms. Releasing or reversing a held
button, lease expiry at 750 ms, an operator stop, and feedback faults bypass
the relay minimum-on timer; the 2-second minimum-off interval still applies
before reactivation. An output only starts when its lease covers the 500 ms
minimum on-time, so a stalled network cannot produce a brief relay click.
Electrical output removal does not stop mechanical coasting instantly.

Movement also requires a working station link and DHCP address. Network loss
cuts motion; reconnecting requires ownership and fresh manual input.
A browser hold also expires after one second without fresh
application telemetry. Expiring server tokens prevent delayed commands from
receiving a new unrestricted lease when they eventually arrive.

Dashboard jogging requires fresh feedback inside the installed calibration.
The direction buttons stay disabled when the controller reports unusable
feedback, with the reason shown below the pad. `feedback-invalid` means at
least one ADC voltage is outside the accepted calibrated range or at an ADC
rail; being online and owning control does not clear that block. With the
current values and the 25 mV margin, A0 must be 33–1306 mV and A1 must be
89–2058 mV. If feedback becomes invalid during a hold, the dashboard stops
the request; recovery requires releasing and pressing again.

For initial measurements, keep the Pico's four direction leads disconnected
and move the rotator with the G-5500 front-panel controls. The dashboard still
reports raw ADC readings when calibration rejects them — including when the
calibration itself is rejected, which the dashboard shows as
`calibration-invalid` while inhibiting motion. Connect the direction leads for
dashboard jogging only after installing and verifying the new calibration.

## 1. Trim the G-5500 controller

First record the rotator and controller model, serial numbers and supply
frequency. Use the matching
[G-5500](https://www.yaesu.com/Files/4CB6273C-1018-01AF-FA4D504B591F641A/G-5500_IM_ENG_E12901004.pdf)
or [G-5500DC](https://www.yaesu.com/Files/4CB6273C-1018-01AF-FA4D504B591F641A/G-5500DC_IM_ENG_EBB70X101_1906V-BS.pdf)
manual for the rear-panel **FULL SCALE ADJ** controls, which align the
controller's own meters:

1. Slew azimuth to the left end, mark the housing, then drive right through one
   complete turn back to the mark. Adjust the rear azimuth **FULL SCALE ADJ**
   control for 360° on the meter. Confirm the clockwise end reads the additional 90°
   (450° continuous travel).
2. Slew elevation to its 180° mark and adjust the rear elevation **FULL SCALE
   ADJ** control for 180° on the meter.
3. Recheck both zero ends.

This trim sets what the controller's meters read. It does not establish ezal's
ADC endpoints, and the manual's nominal 2.0–4.5 V DIN description is not a
figure to trim towards: measure the complete connected feedback chain in step 2
instead, exactly as [HARDWARE.md](HARDWARE.md#stage-1-g-5500-hardware-trim-one-off-on-the-bench)
describes. The installed values in this document differ from that nominal
range, and a correctly working installation may well differ from both.

## 2. Record ezal's ADC endpoints

Open the dashboard after flashing the hardware build. Its azimuth card shows
raw **A1** millivolts and its elevation card shows raw **A0** millivolts. Let
each reading settle before recording it.

Measure at the endpoints of the **permitted installation range**, recording
both the physical angle and its ADC reading. The current configuration uses
0°–360° azimuth and 0°–90° elevation; a full-travel G-5500 installation may use
0°–450° and 0°–180°. Do not measure at one range's endpoints and label those
voltages with another range's angles.

For over-the-top use, measure permitted elevation travel through 90° and
towards 180°. Record intermediate values at 45°, 90° and 135° as well as the
endpoints. Preserve continuous mechanical angles: elevation 135° is a
different motor position from 45°, although an azimuth shift of 180° can make
them point in the same sky direction. Similarly, keep azimuth 360°–450° as
continuous travel if that overlap is available on this installation.

Do not obtain the new calibration by changing the current `90.0` endpoint
to `180.0` while retaining its voltage. In particular, the current 58 mV
reading at 90° cannot be extrapolated to infer the far endpoint. Measure the
entire feedback chain at the actual angles. If the readings reach a rail
before the allowed mechanical endpoint, investigate the feedback interface
and controller setup before extending the calibration.

| measurement | physical angle to record | dashboard reading to record |
|-------------|--------------------------|-----------------------------|
| azimuth low / CCW | `AZ_MIN_DEG` | A1: `AZ_MIN_MV` |
| azimuth high / CW | `AZ_MAX_DEG` | A1: `AZ_MAX_MV` |
| elevation low / DOWN | `EL_MIN_DEG` | A0: `EL_MIN_MV` |
| elevation high / UP | `EL_MAX_DEG` | A0: `EL_MAX_MV` |

Repeat every reading while approaching the endpoint from the normal direction.
If repeated values differ by more than about 5 mV, investigate loose wiring,
grounding, or a noisy/worn position potentiometer before continuing.

Sanity-check the worksheet:

- each high-angle reading must differ clearly from its low-angle reading;
- voltage may increase or decrease with angle—the calibration supports both
  polarities;
- compare each voltage span with the expected span for that axis's selected
  angular range and interface; a 90° elevation range need not have the same
  span as 180° travel. Investigate unexpectedly small spans or large changes
  from previous installation measurements;
- every reading must remain inside the ADS1015 ±4.096 V setting and the
  board's 0–3.3 V input range; the entire 25 mV endpoint margin must fit
  strictly between zero and **3200 mV**. That ceiling is the supply rail, not
  the ±4.096 V saturation code: the ADS1015 cannot report a reading above its
  own 3.3 V supply, so a band reaching that high would make an input stuck at
  the rail indistinguishable from a valid endpoint;
- A0 must move only with elevation, and A1 only with azimuth.

## 3. Replace the placeholders

Replace **all eight angle and voltage fields** in `HARD_CODED_CALIBRATION`
with the paired measurements from the worksheet. In the example below,
substitute each uppercase symbol with your recorded numeric value.
`voltage_min_mv` means “voltage measured at `angle_min_deg`” and
`voltage_max_mv` means “voltage measured at `angle_max_deg`”; they do not need
to be in ascending numerical order:

```rust
pub const HARD_CODED_CALIBRATION: PositionCalibration = PositionCalibration {
    azimuth: AxisCalibration {
        angle_min_deg: AZ_MIN_DEG,
        angle_max_deg: AZ_MAX_DEG,
        voltage_min_mv: AZ_MIN_MV,
        voltage_max_mv: AZ_MAX_MV,
    },
    elevation: AxisCalibration {
        angle_min_deg: EL_MIN_DEG,
        angle_max_deg: EL_MAX_DEG,
        voltage_min_mv: EL_MIN_MV,
        voltage_max_mv: EL_MAX_MV,
    },
};
```

Use integer millivolts exactly as displayed. Set angle endpoints from actual
permitted mechanical travel, not to make an intermediate reading look better;
if the mechanism is non-linear, that is a feedback hardware fault a two-point
calibration should not hide.

## 4. Verify before commanding hardware

Run the host suite and rebuild the firmware:

```bash
./scripts/test-host.sh
cargo build -p ezal-firmware --release
```

The host suite checks that your calibration is one the feedback ADC can
supervise: endpoints in order, and both 25 mV fault margins clear of ground
and of the supply-rail ceiling. The policy tests use their own fixture rather
than the installed numbers, so a correct measurement cannot fail them — a
failure here means the calibration needs another look, not the tests.

Then stop at 0%, 25%, 50%, 75%, and 100% of
each scale. Compare the dashboard angle with the G-5500 meter or a trusted
mechanical reference. Repeat the intermediate checks approaching from both
directions to distinguish a mapping error from backlash. Verify the entire
permitted elevation range, including both sides of zenith, while the Pico
direction leads remain disconnected. After this check, reconnect the leads
and verify small commanded movements inside the measured range before
approaching its boundaries. Record the four endpoint readings, date,
controller serial number, and resulting error table with the installation
maintenance log.

Recalibrate after replacing the controller, ADC/interface board, divider
resistors, rotator position potentiometers, or after any persistent position
offset. Flash persistence and an authenticated calibration UI are intentionally
future work; until those exist, the checked-in constant and installation log
are the authoritative record.

## 5. Verify sky pointing

Electrical calibration establishes motor angles. Tracking also needs the
dish's radio beam to agree with the predicted sky direction:

- Check azimuth zero against true north and elevation zero against a known
  horizontal reference. Record any dish mounting or feed offset; the face
  angle of an offset reflector need not equal its beam direction.
- Compare reference directions at several azimuths and elevations, including
  permitted over-the-top positions. Record approach direction and residual
  error. A controller meter alone cannot establish RF boresight alignment.
- With the external receiver, record received level or decoding quality as
  the dish is moved by small known angular offsets around a stable reference
  or a time-corrected satellite direction. Keep receiver settings fixed and
  distinguish signal variation from pointing loss. Record frequency and
  weather/load conditions with the measurements.
- Establish the acceptable pointing deviation and the allocation between
  alignment uncertainty, orbital/time uncertainty and controller following.
  The initial 3° tracking target and estimated beamwidth in the plan remain
  provisional until these checks support them.

## Motion-profile worksheet

Measurements can follow initial tracking development; use the documented
provisional profile for simulation meanwhile. Record actual loaded behavior
when commissioning hardware, keeping each value separate from its assumption.
Position endpoints must be measured before extending permitted travel.

| Measurement | CW | CCW | Up | Down |
|---|---|---|---|---|
| Steady travel: angle change / elapsed seconds | TBD | TBD | TBD | TBD |
| Startup delay and shortest repeatably useful pulse | TBD | TBD | TBD | TBD |
| Angular travel after output release and time to rest | TBD | TBD | TBD | TBD |
| Reversal delay and backlash/take-up angle | TBD | TBD | TBD | TBD |
| Feedback noise at rest and position residual | TBD | TBD | TBD | TBD |

Time travel over a known span away from the endpoints, in both directions
and at several elevations under the normal dish load. Record slow and fast
runs, not only their average. Repeat pulses with progressively shorter
intervals, starting from the implemented 500 ms on / 2 s off behavior.
The proposed shorter off/reversal intervals are commissioning candidates;
observe useful movement, stopping and supervision before applying them.

Use the measured slow rates for preparation/intercept estimates and the
measured fast rates plus stopping travel for endpoint margins. Separate the
motor's release/coasting distance from movement caused by an old feedback
sample. Record powered time and rest intervals against the model-specific
duty specification in [HARDWARE.md](HARDWARE.md#published-specifications).

## Motion-supervision commissioning

`MotionConfig::DEFAULT` in `crates/ezal-core/src/motion.rs` starts with a
five-second budget of accumulated energized time without resolvable progress,
one second of startup/reversal settling, three calibrated ADC codes to prove
movement, and two codes of measurement tolerance. The speed limits remain
7°/s azimuth and 3°/s elevation in `drive.rs`. These are explicit engineering
starting points, not measured limits for every installation. To use different
settings, pass a `MotionConfig` to `Actuator::with_motion_config` in the
firmware output task.

Measure normal startup delay, movement at the slowest expected load, feedback
noise, and reversal settling. Verify healthy movement clears the progress
threshold within its budget and normal noise cannot simulate progress. Idle
time pauses the budget; short jogs and reversals do not reset it. Confirm that
frozen midrange feedback, wrong-direction feedback after settling, and abrupt
implausible movement produce the named dashboard motion fault. Each fault
latches both outputs off until the board is reset after investigation; a new
command, feedback recovery, or network reconnection cannot clear it.

Tracking pulse trains need an additional check: `startup_settle_ms` restarts
on every activation, so pulses shorter than the current 1 s allowance can
avoid all direction/speed observations. Accumulated no-progress detection
alone does not detect useful-looking movement in the wrong direction.
Implement and test supervision across short runs and their stopping intervals
before enabling the relaxed tracking timings. Include wrong-direction and
implausible feedback in repeated short-pulse tests, alongside genuine backlash
and coasting, rather than testing only long continuous runs.
