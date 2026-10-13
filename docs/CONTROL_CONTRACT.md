# Control contract

Manual movement is an expiring request. The output boundary decides what may
be energised using current feedback, operating permission and applied-motion
history. Forecast rows alone never authorize movement, and browser messages
never supply trajectory points or motor outputs. Explicitly armed tracking
uses the same output boundary and exclusive operator lease; see
[the tracking contract](TRACKING.md).

This document is the one home of the control timing numbers. Each appears once,
beside the constant that implements it; other documents name the constant and
link here, and only a bench or calibration procedure the operator performs
repeats a number.

## Ownership and recovery

| Rule | Owner | Effect | Recovery |
|---|---|---|---|
| ADC transaction deadline, 500 ms (`feedback::FEEDBACK_READ_TIMEOUT_MS`) | ADC resource owner | Invalidate feedback; never reuse a cancelled bus | Board reset |
| Calibration, ADC rails and accepted read latency, at most 150 ms (`feedback::FEEDBACK_MAX_LATENCY_MS`) | Feedback validation | Publish raw diagnostics but no usable sample | A valid later sample; invalid installation requires corrected calibration |
| Feedback lifetime, age at most 500 ms (`feedback::FEEDBACK_MAX_AGE_MS`) | Actuator | Clear forbidden active and pending movement | Fresh feedback and a new movement request |
| Endpoint margin: quantization, sample-age travel and 500 ms coast (`interlock::TravelProfile::DEFAULT`) | Actuator | Cut outward motion on the affected axis | Inward movement remains available |
| Session token age, less than 1 s (`session::COMMAND_MAX_AGE_MS`), and increasing input sequence | Manual session | Reject stale/replayed input and abandon held intent | New telemetry token and operator press |
| Exclusive operator lease, 5 s (`operator::OPERATOR_LEASE_MS`); disconnect/expiry | `TrackingLifecycle`, with coordination serializing claims and renewals | Inhibit manual movement; device-owned tracking continues | Claim vacant control explicitly; fresh Ping/Pong renews only the original valid grant |
| Tracking request: current lease generation and increasing intent number | Protocol admission, then `TrackingLifecycle` | Reject any tracking action from another connection, an expired grant or an old intent number | Claim the lease; send the next intent number |
| Tracking start or change: current telemetry proof and expected state revision; a selection names a currently offered forecast row | Protocol admission, then `TrackingLifecycle` | Reject Select, Arm and Prepare now; Cancel and Return to Manual skip this rule, so a stale view cannot block a stop | A new request from current telemetry |
| Tracking mode | `TrackingLifecycle` | Reject manual direction and stop messages | Cancel or Return to Manual |
| Network observation age, less than 300 ms (`network::NETWORK_HEALTH_TIMEOUT_MS`), renewed every 100 ms (`network::NETWORK_POLL_MS`) | Coordination publishes permission; actuator enforces it | Revoke ownership and clear active and pending movement | New recovery generation and fresh input |
| Command source deadline and unrefreshed lease, at most 750 ms (`drive::MOVEMENT_LEASE_MS`) | Admission stamps; actuator enforces | Clear active and pending movement | A new request, respecting relay off-time |
| Tracking execution interval, at most the plan's step, 20 ms by default (`tracking::PlanningConfig::step_ms`), outside recovery | `TrackingRuntime` | Stop the run with `ExecutionDeadline`; a stale command cannot start a pending relay | A fresh arm |
| Motion progress within 5 s of accumulated energized time (`motion::MotionConfig::DEFAULT`), direction and speed | Actuator using applied output | Latch both axes off | Board reset |
| Motor duty: 180 s per axis by default (`duty::DutyConfig::DEFAULT`); unknown history on boot | Actuator using applied output | Refuse a new pulse without its minimum allowance; cut an exhausted axis and discard its pending request | 900 s continuously idle on that axis, then fresh operator input |
| Ordinary relay timing, 500 ms on and 2 s off (`drive::RelayConfig::DEFAULT`) | Actuator | Delay ordinary release, reversal and reactivation | Timer completion, subject to all current permissions |
| Operator release, reversal or inhibition | Actuator | Cut affected axes immediately; retain off-time | Fresh permitted request after off-time |
| Executor failure: no output service for 500 ms (firmware `watchdog::WATCHDOG_TIMEOUT`), including a [stack overflow](DEVELOPMENT.md#debugging-tips) once the watchdog is armed | Hardware watchdog, fed after GPIO service | Reset; boot holds outputs low after a firmware watchdog timeout. A UF2 or `picotool` reboot also restarts through the watchdog and is not a firmware timeout | Explicit reset or power cycle |

## Ordering and time

- Account for the output applied during the elapsed interval before changing it.
- Enforce permissions and consume an arriving stop before advancing a pending
  activation at the same instant.
- Expire the old movement lease before admitting a late refresh.
- Preserve original command expiry through transport, mailbox replacement and
  consumption. An unconsumed inhibition takes priority over movement refreshes.
- Clear both active and pending requests on inhibition. Recovery alone never
  recreates previous motion. Preserve an unaffected axis's timers and lease.
- A relay activation requires remaining **command** lifetime and remaining
  motor duty that each cover the minimum on-time. Feedback and network
  permission are independently revocable and may cut it short. Their deadlines
  must not be folded into the activation lifetime.
- Feedback age is inclusive at its limit; network and command deadlines are
  exclusive. Future timestamps and unrepresentable permission deadlines cannot
  grant movement. Keep these boundary semantics explicit.
- Repeated short jogs and reversals do not reset accumulated no-progress time.
  Fresh communication does not prove fresh feedback or physical movement.
- Direction evidence survives same-direction pulse gaps, and speed comparisons
  continue through startup, coast and idle. Reversal timing must cover coast
  plus maximum feedback age before establishing a new direction baseline.
- Duty history belongs to the actuator, across command sources and ownership
  changes. Only a full continuous rest replenishes it; reboot requires that
  rest again because there is no persistent duty history. Front-panel motor
  use is not recorded, so it needs a full rest before returning to Pico control.

The ADC task and output task remain separate. The output task wakes for state
changes and the earliest applicable deadline, and otherwise services the
actuator every 100 ms while idle and every 10 ms while a pass is armed or
running (firmware `drive::SERVICE_PERIOD` and `drive::TRACKING_PERIOD`). Only
the output task feeds the watchdog, after applying the permitted GPIO state. A stalled whole
executor can only cut output on resumption or watchdog reset, so the tracking
execution interval is not a physical release guarantee.

All relay/command timing uses monotonic uptime; an armed pass freezes its
validated UTC-to-uptime mapping before execution.
Every SNTP attempt publishes the clock; reversed uptime or an exhausted
revision disarms it until reboot, which stops a run with
`Inputs(Timing(Unavailable))` and blocks preview.

`RelayConfig::TRACKING_CANDIDATE`, a shorter relay policy, is for offline
simulation and commissioning only; the firmware runs `RelayConfig::DEFAULT`.

## Dashboard

The dashboard reports remaining on-time and required rest separately for each
axis. It disables and releases only the affected hold; recovery requires a new
press. Control acknowledgements can update these values but cannot renew
telemetry freshness.

The dashboard waits for acknowledged device state, hides manual controls while
tracking, and never resends tracking intent after a reconnect. Closing or
hiding the browser cannot stop device-owned tracking. Viewers get the phase,
feasibility, timestamp-matched deviation, feedback age, recovery deadline/ETA and
retained final metrics without renewing any permission. A stale sample is never
shown as a current error measurement. See the
[tracking wire contract](TRACKING.md#wire-contract) for message fields.

## Verification

Host tests must exercise the production coordination and actuator path with:

- independent command, sensor and network stalls, and an outage and recovery
  between service passes;
- queued expiry, coalesced notifications, and a stop arriving with an
  activation at the same instant;
- endpoint refusal, repeated short pulses, per-axis duty exhaustion and
  unknown boot history;
- ownership transfer and old-owner disconnect;
- fault latches surviving recovery;
- raw diagnostics remaining available under an invalid calibration.

Dashboard tests must preserve fresh-press recovery, pointer release, page
suspension and the applied-output indicators.

Host tests do not establish physical output-release or stopping time; the
[bench verification](CALIBRATION.md#bench-verification) covers GPIO release,
relay timing, watchdog reset and mechanical coast.
