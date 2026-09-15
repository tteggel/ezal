//! Pointing units and calibrated angle/feedback-voltage conversion.
//!
//! The G-5500 reports each axis as an analog voltage. [`AxisCalibration`]
//! captures the two measured mechanical end points and provides both sides of
//! the mapping: ADC millivolts to degrees for feedback, and degrees to
//! millivolts for simulation and diagnostics.

/// Small voltage excursion accepted beyond a measured endpoint before the
/// feedback channel is treated as faulty. Values inside this margin clamp to
/// the mechanical endpoint; larger excursions fail safe.
pub const FEEDBACK_ENDPOINT_MARGIN_MV: i32 = 25;

/// A paired azimuth/elevation pointing position in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pointing {
    /// Continuous G-5500 azimuth, 0° through 450° (no modulo wrapping).
    pub azimuth_deg: f32,
    /// Elevation, 0° at the horizon through 180°.
    pub elevation_deg: f32,
}

impl Pointing {
    /// Construct a pointing position.
    pub const fn new(azimuth_deg: f32, elevation_deg: f32) -> Self {
        Self {
            azimuth_deg,
            elevation_deg,
        }
    }

    /// Whether both coordinates are finite and inside the G-5500 envelope.
    pub fn is_valid(self) -> bool {
        self.azimuth_deg.is_finite()
            && self.elevation_deg.is_finite()
            && (0.0..=450.0).contains(&self.azimuth_deg)
            && (0.0..=180.0).contains(&self.elevation_deg)
    }

    /// Whether both coordinates are finite and inside the supplied installed
    /// calibration's mechanical angle endpoints.
    pub fn is_valid_for(self, calibration: PositionCalibration) -> bool {
        self.azimuth_deg.is_finite()
            && self.elevation_deg.is_finite()
            && self.azimuth_deg >= calibration.azimuth.angle_min_deg
            && self.azimuth_deg <= calibration.azimuth.angle_max_deg
            && self.elevation_deg >= calibration.elevation.angle_min_deg
            && self.elevation_deg <= calibration.elevation.angle_max_deg
    }
}

/// One axis's measured mechanical and ADC endpoints.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AxisCalibration {
    /// Angle at the low mechanical endpoint.
    pub angle_min_deg: f32,
    /// Angle at the high mechanical endpoint.
    pub angle_max_deg: f32,
    /// ADC reading measured at `angle_min_deg`.
    pub voltage_min_mv: i32,
    /// ADC reading measured at `angle_max_deg`.
    pub voltage_max_mv: i32,
}

/// Why a calibrated conversion could not be trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalibrationError {
    /// Calibration angles are non-finite/reversed, or voltage has no span.
    InvalidCalibration,
    /// Requested angle lies outside the calibrated mechanical range.
    AngleOutOfRange,
    /// Feedback voltage lies outside the endpoint fault margin.
    VoltageOutOfRange,
}

impl AxisCalibration {
    /// Check that the angle endpoints increase and the voltage endpoints differ.
    /// Voltage may increase or decrease with angle.
    pub fn validate(self) -> Result<(), CalibrationError> {
        if !self.angle_min_deg.is_finite()
            || !self.angle_max_deg.is_finite()
            || self.angle_max_deg <= self.angle_min_deg
            || self.voltage_max_mv == self.voltage_min_mv
        {
            Err(CalibrationError::InvalidCalibration)
        } else {
            Ok(())
        }
    }

    /// Convert an angle to the corresponding calibrated ADC millivolts.
    pub fn angle_to_voltage(self, angle_deg: f32) -> Result<i32, CalibrationError> {
        self.validate()?;
        if !angle_deg.is_finite()
            || angle_deg < self.angle_min_deg
            || angle_deg > self.angle_max_deg
        {
            return Err(CalibrationError::AngleOutOfRange);
        }

        let fraction = (angle_deg - self.angle_min_deg) / (self.angle_max_deg - self.angle_min_deg);
        let millivolts = self.voltage_min_mv as f32
            + fraction * (self.voltage_max_mv - self.voltage_min_mv) as f32;
        Ok(round_nearest(millivolts))
    }

    /// Convert calibrated ADC millivolts to an angle.
    ///
    /// Endpoint noise up to [`FEEDBACK_ENDPOINT_MARGIN_MV`] is clamped to the
    /// corresponding mechanical limit. A larger excursion is treated as a
    /// disconnected, shorted, or otherwise untrustworthy feedback channel.
    pub fn voltage_to_angle(self, millivolts: i32) -> Result<f32, CalibrationError> {
        self.validate()?;
        let voltage_low_mv = self.voltage_min_mv.min(self.voltage_max_mv);
        let voltage_high_mv = self.voltage_min_mv.max(self.voltage_max_mv);
        if millivolts < voltage_low_mv - FEEDBACK_ENDPOINT_MARGIN_MV
            || millivolts > voltage_high_mv + FEEDBACK_ENDPOINT_MARGIN_MV
        {
            return Err(CalibrationError::VoltageOutOfRange);
        }

        let clamped = millivolts.clamp(voltage_low_mv, voltage_high_mv);
        let fraction = (clamped - self.voltage_min_mv) as f32
            / (self.voltage_max_mv - self.voltage_min_mv) as f32;
        Ok(self.angle_min_deg + fraction * (self.angle_max_deg - self.angle_min_deg))
    }
}

/// The two ADC channels in their physical board order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeedbackVoltages {
    /// ADS1015 A0: elevation feedback.
    pub a0_elevation_mv: i32,
    /// ADS1015 A1: azimuth feedback.
    pub a1_azimuth_mv: i32,
}

/// Complete installed-system feedback calibration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PositionCalibration {
    /// ADS1015 A1 / G-5500 pin 6 azimuth mapping.
    pub azimuth: AxisCalibration,
    /// ADS1015 A0 / G-5500 pin 1 elevation mapping.
    pub elevation: AxisCalibration,
}

impl PositionCalibration {
    /// Validate both channel mappings.
    pub fn validate(self) -> Result<(), CalibrationError> {
        self.azimuth.validate()?;
        self.elevation.validate()
    }

    /// Convert a position into the voltages the installed feedback chain
    /// should produce. Used by calibration diagnostics and round-trip tests.
    pub fn position_to_feedback(
        self,
        position: Pointing,
    ) -> Result<FeedbackVoltages, CalibrationError> {
        Ok(FeedbackVoltages {
            a0_elevation_mv: self.elevation.angle_to_voltage(position.elevation_deg)?,
            a1_azimuth_mv: self.azimuth.angle_to_voltage(position.azimuth_deg)?,
        })
    }

    /// Convert the two physical ADC channels into a pointing position.
    pub fn feedback_to_position(
        self,
        feedback: FeedbackVoltages,
    ) -> Result<Pointing, CalibrationError> {
        Ok(Pointing {
            azimuth_deg: self.azimuth.voltage_to_angle(feedback.a1_azimuth_mv)?,
            elevation_deg: self.elevation.voltage_to_angle(feedback.a0_elevation_mv)?,
        })
    }
}

/// Current installed-system endpoints used by autonomous and manual modes.
/// Replace these values with measurements from the calibration procedure in
/// `docs/CALIBRATION.md` whenever the feedback hardware changes.
pub const HARD_CODED_CALIBRATION: PositionCalibration = PositionCalibration {
    azimuth: AxisCalibration {
        angle_min_deg: 0.0,
        angle_max_deg: 360.0,
        voltage_min_mv: 114,
        voltage_max_mv: 2_033,
    },
    elevation: AxisCalibration {
        angle_min_deg: 0.0,
        angle_max_deg: 90.0,
        voltage_min_mv: 1_281,
        voltage_max_mv: 58,
    },
};

fn round_nearest(value: f32) -> i32 {
    if value >= 0.0 {
        (value + 0.5) as i32
    } else {
        (value - 0.5) as i32
    }
}
