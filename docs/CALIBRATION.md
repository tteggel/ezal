# Position calibration

ezal converts the G-5500's two feedback voltages into angles with a linear
two-point calibration for each axis. The default walking skeleton uses that
mapping with the real ADS1015. For now the four installed-system endpoints are
deliberately hard-coded; this document is the procedure for replacing them
with measurements from your unit.

## Current hard-coded values

The single source of truth is `HARD_CODED_CALIBRATION` in
[`crates/ezal-core/src/position.rs`](../crates/ezal-core/src/position.rs).

| ADC channel | G-5500 signal | mechanical range | placeholder ADC range |
|-------------|---------------|------------------|-----------------------|
| A0          | elevation     | 0°–90°           | 1281 mV at 0°, 58 mV at 90° |
| A1          | azimuth       | 0°–360°          | 114 mV at 0°, 2033 mV at 360° |

These checked-in installation values drive both autonomous and manual mode.
The mapping accepts at most 25 mV of endpoint noise before declaring feedback
invalid and stopping the controller.

## Safety setup

Do this with the antenna installation visible, the coax and other snag-prone
cables managed for the full travel, and one person ready to remove G-5500
motor power. Never use software as the only protection against an unverified
end stop. Start with the Pico disconnected from the four direction inputs and
confirm that all direction GPIOs are low during reset.

The default walking skeleton raises physical direction pins automatically as
it acquires a simulated pass. Do not use it for initial calibration. Build the
explicit manual hardware mode instead:

```bash
cargo run -p ezal-firmware --release --no-default-features
```

In manual hardware mode the dashboard's buttons are leased controls. A held
button must refresh its command every 150 ms; firmware stops an unrefreshed
movement after 750 ms. Direction changes also pass through the relay minimum
on/off timer. Release the button early and let the rotator coast to the end
rather than holding a motor against a stop.

## 1. Trim the G-5500 controller

First follow the G-5500 service procedure for its rear-panel **FULL SCALE
ADJ** controls:

1. Slew azimuth to the left end, mark the housing, then drive right through one
   complete turn back to the mark. Adjust azimuth **VR0004** for 360° on the
   controller meter. Confirm the clockwise end reads the additional 90°
   (450° continuous travel).
2. Slew elevation to its 180° mark and adjust elevation **VR0003** for 180° on
   the controller meter.
3. Recheck both zero ends. The external DIN feedback should be roughly 2.0 V
   at the low end and 4.5 V at the high end.

This trim keeps the external signal in the ADC's useful range. It does not
replace ezal's next calibration stage.

## 2. Record ezal's ADC endpoints

Open the dashboard after flashing the hardware build. Its azimuth card shows
raw **A1** millivolts and its elevation card shows raw **A0** millivolts. Let
each reading settle before recording it.

| measurement | mechanical position | dashboard reading | record as |
|-------------|---------------------|-------------------|-----------|
| elevation low | 0° / DOWN end     | A0, mV            | `el_min`  |
| elevation high| 180° / UP end     | A0, mV            | `el_max`  |
| azimuth low   | 0° / CCW end      | A1, mV            | `az_min`  |
| azimuth high  | 450° / CW end     | A1, mV            | `az_max`  |

Repeat every reading while approaching the endpoint from the normal direction.
If repeated values differ by more than about 5 mV, investigate loose wiring,
grounding, or a noisy/worn position potentiometer before continuing.

Sanity-check the worksheet:

- each high-angle reading must differ clearly from its low-angle reading;
- voltage may increase or decrease with angle—the calibration supports both
  polarities;
- the absolute value of both spans should normally be near 1120 mV and should
  not be less than about 780 mV
  (70% of nominal span);
- every reading must remain inside the ADS1015 ±2.048 V setting;
- A0 must move only with elevation, and A1 only with azimuth.

## 3. Replace the placeholders

Edit the four `voltage_*_mv` fields in `HARD_CODED_CALIBRATION` and confirm the
angle endpoints match the mechanically permitted installation range.
`voltage_min_mv` means “voltage measured at `angle_min_deg`” and
`voltage_max_mv` means “voltage measured at `angle_max_deg`”; they do not need
to be in ascending numerical order:

```rust
pub const HARD_CODED_CALIBRATION: PositionCalibration = PositionCalibration {
    azimuth: AxisCalibration {
        angle_min_deg: 0.0,
        angle_max_deg: 360.0,
        voltage_min_mv: AZ_MIN_FROM_A1,
        voltage_max_mv: AZ_MAX_FROM_A1,
    },
    elevation: AxisCalibration {
        angle_min_deg: 0.0,
        angle_max_deg: 90.0,
        voltage_min_mv: EL_MIN_FROM_A0,
        voltage_max_mv: EL_MAX_FROM_A0,
    },
};
```

Use integer millivolts exactly as displayed. Set angle endpoints from actual
permitted mechanical travel, not to make an intermediate reading look better;
if the mechanism is non-linear, that is a feedback hardware fault a two-point
calibration should not hide.

## 4. Verify before tracking hardware

Run the host suite and rebuild both modes:

```bash
./scripts/test-host.sh
cargo build -p ezal-firmware --release
cargo build -p ezal-firmware --release --no-default-features
```

Then, still in manual hardware mode, stop at 0%, 25%, 50%, 75%, and 100% of
each scale. Compare the dashboard angle with the G-5500 meter or a trusted
mechanical reference. Record the four endpoint readings, date, controller
serial number, and resulting error table with the installation maintenance
log.

Recalibrate after replacing the controller, ADC/interface board, divider
resistors, rotator position potentiometers, or after any persistent position
offset. Flash persistence and an authenticated calibration UI are intentionally
future work; until those exist, the checked-in constant and installation log
are the authoritative record.
