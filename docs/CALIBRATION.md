# Calibration and commissioning

ezal converts the G-5500's two feedback voltages into angles with a linear
two-point calibration for each axis. Manual and tracking control use that mapping
with the real ADS1015. The four installed-system endpoints are hard-coded;
this document is the procedure for replacing them with measurements from your
unit, measuring the motion profile, and then verifying the installation on the
bench with the [commissioning checklist](#bench-verification).

The tracking installation has an 80 cm parabolic dish with a custom
helical feed and permits over-the-top movement. Its full-travel calibration
and pointing tolerance are unmeasured. Follow this procedure before using
that extra travel. Host acceptance of [tracking](TRACKING.md#verification) does
not establish physical or RF performance; the tracking defaults and the
[provisional motor profile](HARDWARE.md#provisional-tracking-profile) are
assumptions, not measured installation values, and the
[known model limits](#known-model-limits) say where execution departs from
prediction.

## Current hard-coded values

The single source of truth is `HARD_CODED_CALIBRATION` in
[`crates/ezal-core/src/position.rs`](../crates/ezal-core/src/position.rs).

| ADC channel | G-5500 signal | mechanical range | placeholder ADC range |
|-------------|---------------|------------------|-----------------------|
| A0          | elevation     | 0°–90°           | 1281 mV at 0°, 58 mV at 90° |
| A1          | azimuth       | 0°–360°          | 114 mV at 0°, 2033 mV at 360° |

These checked-in installation values govern manual and tracking movement.
The mapping accepts at most 25 mV of endpoint noise before declaring feedback
invalid and inhibiting movement.

## Safety setup

Do this with the antenna installation visible, the coax and other snag-prone
cables managed for the full travel, and one person ready to remove G-5500
motor power. Never use software as the only protection against an unverified
end stop. Start with the Pico disconnected from the four direction inputs and
confirm that all direction GPIOs are low during reset.

The firmware boots with all direction outputs idle and unknown prior motor
duty. It requires a full
[duty rest](CONTROL_CONTRACT.md#ownership-and-recovery) before permitting
jogging; the dashboard shows the remaining rest separately for each axis. Build
and flash it for manual calibration:

```bash
cargo run -p ezal-firmware --release
```

The dashboard's buttons are leased controls under the
[control contract](CONTROL_CONTRACT.md#ownership-and-recovery). A held button
refreshes its command every 150 ms. Releasing or reversing a held button, lease
expiry, an operator stop, and feedback faults bypass the relay minimum on-time;
the minimum off-time still applies before reactivation. An output only starts
when its lease covers the minimum on-time, so a stalled network cannot produce
a brief relay click. Electrical output removal does not stop mechanical coasting
instantly. The endpoint gate reserves one ADC code, travel since the feedback
read began, and the assumed release/coast of `TravelProfile::DEFAULT`. It
therefore stops short of the calibrated endpoint; use the front-panel procedure
below to measure the full permitted range.

Movement also requires a working station link and DHCP address. Network loss
cuts motion; reconnecting requires ownership and fresh manual input. A browser
hold also expires after
[`COMMAND_MAX_AGE_MS`](CONTROL_CONTRACT.md#ownership-and-recovery) without
fresh application telemetry. Expiring server tokens prevent delayed commands
from receiving a new unrestricted lease when they eventually arrive.

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
Firmware duty accounting cannot see motors energized by the front panel.
After those measurements, allow a full 15-minute motor rest before using Pico
control; reset the Pico after the final front-panel movement to start its
rest countdown from that point.

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
Position endpoints must be measured before extending permitted travel. Keep
recording and RF measurements in the external receiver system.

| Measurement | CW | CCW | Up | Down |
|---|---|---|---|---|
| Steady travel: angle change / elapsed seconds | TBD | TBD | TBD | TBD |
| Startup delay and shortest repeatably useful pulse | TBD | TBD | TBD | TBD |
| Angular travel after output release and time to rest | TBD | TBD | TBD | TBD |
| Reversal delay and backlash/take-up angle | TBD | TBD | TBD | TBD |
| Feedback noise at rest and position residual | TBD | TBD | TBD | TBD |
| Paired ADC latency and A0-to-A1 capture delay | TBD | TBD | TBD | TBD |

Time travel over a known span away from the endpoints, in both directions
and at several elevations under the normal dish load. Record slow and fast
runs, not only their average. Repeat pulses with progressively shorter
intervals, starting from the implemented 500 ms on / 2 s off behavior.
The proposed shorter off/reversal intervals are commissioning candidates;
observe useful movement, stopping and supervision before applying them.

The [planner](TRACKING.md#planning) evaluates fixed and alternating one-code
offsets, 0–150 ms read latency and up to 150 ms between the two channel
captures. These are explicit test scenarios, not measured noise limits. In
the recorded fixture, an alternating-noise case trips the current speed
supervision during preparation. Record actual noise
and acquisition timing together before adjusting either the planner assumptions
or supervision thresholds; increasing noise tolerance also requires enough
progress resolution to distinguish movement from noise. Replay with the same
installed calibration, output policy and measured profile intended for execution.

Use the measured slow rates for preparation/intercept estimates and the
measured fast rates plus stopping travel for endpoint margins. Separate the
motor's release/coasting distance from movement caused by an old feedback
sample. Record powered time and rest intervals against the model-specific
duty specification in [HARDWARE.md](HARDWARE.md#published-specifications).

## Motion-supervision commissioning

`ActuatorConfig::DEFAULT` in `crates/ezal-core/src/actuator.rs` combines relay,
travel, motion and duty settings. `MotionConfig::DEFAULT` starts with the
[contract's](CONTROL_CONTRACT.md#ownership-and-recovery) budget of accumulated
energized time without resolvable progress, three calibrated ADC codes to prove
movement, and two codes of measurement tolerance. `TravelProfile::DEFAULT`
uses 7°/s azimuth and 3°/s elevation with the contract's release/coast on both
axes. The same travel profile governs endpoint margins and implausible-speed
checks.

These are engineering starting points, not measured limits for every
installation. To install measured settings, pass the complete `ActuatorConfig`
to `Actuator::with_config` in the firmware output task, retaining
`DutyHistory::Unknown` at boot. A reversal off-time shorter than coast plus
the maximum feedback age is rejected. `RelayConfig::TRACKING_CANDIDATE` offers
shorter same-direction and reversal off-times for simulation and
commissioning; firmware keeps `RelayConfig::DEFAULT` until they are verified.
Select `DutyConfig::CONFIRMED_AC`, the AC
[operating specification](HARDWARE.md#published-specifications), only after
identifying the original AC hardware; the
[contract](CONTROL_CONTRACT.md#ownership-and-recovery) gives the default
allowance and the continuous rest that both require.

Idle time pauses the no-progress budget; short jogs and reversals do not reset
it. Observed coast movement can establish progress, including the first fresh
observation within the maximum feedback age after the assumed coast ends, and
the wrong-direction and speed checks apply to that observation too; movement
first observed after a long idle cannot clear the stalled-motor budget.
Same-direction pulse trains keep their direction evidence, including expected
coast, and speed checks keep consecutive and one-second baselines across output
changes. An opposite direction establishes its baseline after the required
reversal hold. Each motion fault latches both outputs off until the board is
reset after investigation; a new command, feedback recovery or network
reconnection cannot clear it.

## Bench verification

Host tests and cross-compilation cannot verify GPIO reset behaviour, relay
release, motor coasting, board timing or RF pointing. Work through this list in
order, after any change to the safety path and before routine powered tracking,
and keep the evidence (logic-analyzer captures, timing logs, the worksheet)
with the hardware and motion-profile identity.

With motor power disconnected and a logic analyzer on the four direction
outputs:

1. Build the release image and inspect its RAM and stack frames:

   ```bash
   cargo size -p ezal-firmware --release -- -A
   cargo objdump -p ezal-firmware --release -- -d --demangle
   ```

   Include `.data`, `.bss` and `.uninit`; the Embassy task pools include all
   eight web acceptors and the planner. The linker reserves at least 192 KiB of
   free stack. Inspect the tracking poll and plan-publication frames after
   compiler or structure changes, then measure high-water use on the board;
   the assembly check alone does not bound nested calls plus interrupt use.
2. Flash a CI UF2 by BOOTSEL. It boots into normal operation, and the boot
   ROM's own watchdog reboot is not reported as a firmware watchdog fault.
3. Let the boot duty countdown finish: unknown prior usage requires 15 minutes
   continuously idle. Both axis countdowns are visible, and a command attempted
   during rest does not run when rest ends.
4. Hold the CPU at a breakpoint while a direction output is active. The
   watchdog resets the board and removes the output within the 500 ms interval
   (allow for oscillator tolerance), and the next boot stays inhibited until an
   explicit reset and fresh operator input. Capture the pin *through* the
   reset, not only after the reboot: an RP2350 pad isolation latch can hold the
   last output state until boot claims the pin, which is how long the base
   pull-downs take to win.
5. Stop refreshing a held command: the output is removed at the 750 ms lease
   deadline. When the 2 s off interval delays an activation, a lease with less
   than 500 ms left leaves the relay alone with no brief click, and a refreshed
   lease then starts the output normally.
6. Make feedback invalid, missing and stalled: each inhibits movement, and
   movement returns only with valid feedback and a fresh operator press. A
   cancelled read is not retried until reset. A read slower than 150 ms reports
   `feedback-stale` without latching.

With [calibration](#3-replace-the-placeholders) installed and verified, the
direction leads connected and motor power restored:

7. Record the rotator model and supply frequency, then measure the
   [motion profile](#motion-profile-worksheet) in each direction: loaded slew
   rate down to the slowest expected load, startup, useful pulse duration,
   stopping travel, reversal take-up and settling, and feedback noise at
   rest. Verify the installed relay and load before reading component
   endurance figures as a system switching budget.
8. At each measured endpoint an outward jog is inhibited and an inward jog is
   permitted. Approaching an endpoint under power, the output is cut before
   reaching it rather than at the next sample. Check intermediate positions at
   the 2 mV ADC resolution and measure coast and brake distance.
9. Healthy movement, including short pulse trains, continuous motion, startup,
   backlash, coast, reversal and delayed ADC samples, clears the progress
   threshold within its budget, and normal noise cannot simulate progress.
   Frozen midrange feedback, wrong-direction feedback across pulses and abrupt
   implausible movement each produce the named dashboard motion fault, latched
   as above.
10. Duty is charged only while each physical output is active, survives
    ownership transfer and short pulse gaps, and replenishes only after
    continuous rest. Exhausting one axis stops only that axis and clears its
    pending request; neither a new owner nor a power cycle grants a fresh
    allowance. The dashboard shows the required rest and needs a new press
    after recovery. Cancellation and stale feedback clear pending pulses.

[Tracking](TRACKING.md), first with the direction leads disconnected and
simulated feedback, then in supervised powered passes:

11. Select one predicted pass, inspect its feasibility and reception windows,
    then Enter Tracking. Selection alone does not move, scheduled preparation
    starts at the displayed time, and Prepare now evaluates its own schedule
    before moving. Test full and partial support. With eight viewers open,
    exercise forecast computation and a TLE download while preparing and
    following; close and reopen the controller and pause a slow viewer. The
    pass continues, only the current leased controller can cancel it, manual
    input cannot interrupt Tracking mode, and no viewer read or failed takeover
    extends ownership.
12. Record the once-per-minute `tracking timing maxima` log for full-window and
    late or partial passes. Active control fits the plan's 20 ms maximum
    interval with headroom. Recovery finishes before its 2 s launch lead and a
    late admission before its 5 s entry lead; longer jobs hold or fail visibly.
    Include a full recorded pass with a long remaining suffix and measure
    whole-search time under concurrent load against the launch lead; the
    [work figures](#known-model-limits) show why a single fast step proves
    nothing.
13. Suspend background planning alone: GPIO and watchdog service continue,
    holding during an unresolved intercept. Delay a normal control step beyond
    20 ms and check inhibition with `ExecutionDeadline`, including a relay
    waiting to activate. Delay a recovery launch or control step and check the
    bounded hold and retry. Drop link or address permission and confirm there is
    no automatic restart on recovery.
14. Repeat the watchdog check (4) with tracking active. Only the GPIO task
    feeds the watchdog; measure the actual pad release and the watchdog-fault
    boot inhibition. A planned 20 ms service interval does not guarantee 20 ms
    output release during a whole-executor stall.
15. In supervised powered passes, log predicted and actual preparation and
    recovery times and measured deviation through the whole pass, and compare
    them with the [known model limits](#known-model-limits) before accepting
    the pointing reserve.

## Known model limits

The host acceptance tests print these figures; reproduce them with the command
in [TRACKING.md](TRACKING.md#verification). They come from the shared motor
model at a virtual execution cadence, not from the board.

- **Predictions are conditional on the simulated schedule.** Planning samples
  every 20 ms and execution runs every 10 ms, and the changed ADC and pulse
  phase can exceed the reported modeled peak, even for a stationary target. In
  the recorded METOP-B window the planner predicts a 12.50 s gap and a 2.218°
  bounded peak; the 10 ms replay recovers 22.46 s after its planned entry and
  peaks at 2.345°. It stays within the original 30 s episode and the 3°
  operational threshold, but this does not establish the 1° reserve or the
  predicted reception windows as achievable RF coverage. Measure scheduling,
  plant and pointing errors before accepting the reserve, and broaden
  timing-phase qualification if those measurements require it.
- **Full-pass recovery is expensive to compute.** The successful recorded
  search takes 398,964 work units. The test supplies up to 2,048 units per
  virtual 10 ms turn, an explicit test throughput rather than a Pico figure; at
  128 units per turn the same run misses every launch opportunity and stops at
  its original deadline. If the board cannot finish whole searches within the
  2 s launch lead under real load, recovery needs optimization or a revised,
  prevalidated schedule before partial-pass operation is commissioned; an
  active episode deadline is never extended.
