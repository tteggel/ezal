//! Paired ADC acquisition and publication of calibrated feedback.

use defmt::{error, info};
use embassy_rp::i2c;
use embassy_time::{with_timeout, Duration, Instant, Timer};

use ezal_core::ads1015::Mux;
use ezal_core::feedback::{
    AcquisitionState, FeedbackHealth, FeedbackMonitor, FeedbackReadError, FEEDBACK_PERIOD_MS,
    FEEDBACK_READ_TIMEOUT_MS,
};
use ezal_core::position::{FeedbackVoltages, HARD_CODED_CALIBRATION};

use crate::ads1015::Ads1015;
use crate::state::SharedState;

const FEEDBACK_PERIOD: Duration = Duration::from_millis(FEEDBACK_PERIOD_MS);

/// Own the ADC and its cancellation state together so a cancelled bus cannot be reused.
struct FeedbackReader {
    adc: Ads1015<'static>,
    acquisition: AcquisitionState,
}

impl FeedbackReader {
    fn new(adc: Ads1015<'static>) -> Self {
        Self {
            adc,
            acquisition: AcquisitionState::default(),
        }
    }

    async fn read(&mut self) -> Result<FeedbackVoltages, FeedbackReadError> {
        if !self.acquisition.read_permitted() {
            return Err(FeedbackReadError::TimedOut);
        }
        let result = match with_timeout(Duration::from_millis(FEEDBACK_READ_TIMEOUT_MS), async {
            let a0_elevation_mv = self.adc.read_channel_mv(Mux::Ain0).await?;
            let a1_azimuth_mv = self.adc.read_channel_mv(Mux::Ain1).await?;
            Ok::<_, i2c::Error>(FeedbackVoltages {
                a0_elevation_mv,
                a1_azimuth_mv,
            })
        })
        .await
        {
            Ok(Ok(readings)) => Ok(readings),
            Ok(Err(_)) => Err(FeedbackReadError::Unavailable),
            Err(_) => {
                error!("feedback: ADC read timed out; reset required");
                Err(FeedbackReadError::TimedOut)
            }
        };
        self.acquisition.complete(result)
    }
}

/// Sample feedback and publish health plus the actuator interlock.
#[embassy_executor::task]
pub async fn run(adc: Ads1015<'static>, state: &'static SharedState) -> ! {
    let mut reader = FeedbackReader::new(adc);
    let mut monitor = FeedbackMonitor::new(HARD_CODED_CALIBRATION, state.feedback().raw);
    if let Some(error) = monitor.calibration_error() {
        // Keep sampling anyway: the dashboard's raw millivolts are how an
        // operator measures the endpoints again. Motion stays inhibited.
        error!("feedback: calibration rejected: {=str}", error.as_str());
    }
    let mut last_health = FeedbackHealth::Unavailable;

    loop {
        // Timestamp before the oldest channel; include both reads in its age.
        let sample_started_ms = Instant::now().as_millis();
        let readings = reader.read().await;
        let now_ms = Instant::now().as_millis();
        let update = monitor.update(now_ms, sample_started_ms, readings);
        state.publish_feedback(update.snapshot, update.interlock);
        if update.snapshot.health != last_health {
            info!("feedback: {=str}", update.snapshot.health.as_str());
            last_health = update.snapshot.health;
        }

        Timer::after(FEEDBACK_PERIOD).await;
    }
}
