# Manual control contract

Manual movement is an expiring request. The output boundary decides what may
be energised using current feedback, operating permission and applied-motion
history. Forecast rows alone never authorize movement. Explicitly armed tracking
uses the same output boundary and exclusive operator lease; see
[the tracking contract](PASS_PREDICTION.md#planned-tracking).

## Ownership and recovery

| Rule | Owner | Effect | Recovery |
|---|---|---|---|
| ADC transaction deadline, 500 ms | ADC resource owner | Invalidate feedback; never reuse a cancelled bus | Board reset |
| Calibration, ADC rails and accepted read latency, at most 150 ms | Feedback validation | Publish raw diagnostics but no usable sample | A valid later sample; invalid installation requires corrected calibration |
| Feedback lifetime, age at most 500 ms | Actuator | Clear forbidden active and pending movement | Fresh feedback and a new movement request |
| Endpoint margin: quantization, sample-age travel and 500 ms coast | Actuator | Cut outward motion on the affected axis | Inward movement remains available |
| Session token age, less than 1 s, and increasing input sequence | Manual session | Reject stale/replayed input and abandon held intent | New telemetry token and operator press |
| Five-second exclusive operator lease; disconnect/expiry | Coordination through TrackingRuntime | Inhibit manual movement; device-owned tracking continues | Claim vacant control explicitly; fresh Ping/Pong renews only the original valid grant |
| Network observation age, less than 300 ms | Coordination publishes permission; actuator enforces it | Revoke ownership and clear active and pending movement | New recovery generation and fresh input |
| Command source deadline and unrefreshed lease, at most 750 ms | Admission stamps; actuator enforces | Clear active and pending movement | A new request, respecting relay off-time |
| Motion progress, direction and speed | Actuator using applied output | Latch both axes off | Board reset |
| Motor duty: 180 s per axis by default; unknown history on boot | Actuator using applied output | Refuse a new pulse without its minimum allowance; cut an exhausted axis and discard its pending request | 900 s continuously idle on that axis, then fresh operator input |
| Ordinary relay timing, 500 ms on and 2 s off | Actuator | Delay ordinary release, reversal and reactivation | Timer completion, subject to all current permissions |
| Operator release, reversal or inhibition | Actuator | Cut affected axes immediately; retain off-time | Fresh permitted request after off-time |
| Executor failure | Hardware watchdog, fed after GPIO service | Reset; boot holds outputs low after a firmware watchdog timeout | Explicit reset or power cycle |

## Ordering and time

- Account for the output applied during the elapsed interval before changing it.
- Enforce permissions and consume an arriving stop before advancing a pending
  activation at the same instant.
- Expire the old movement lease before admitting a late refresh.
- Preserve original command expiry through transport, mailbox replacement and
  consumption. An unconsumed inhibition takes priority over movement refreshes.
- Clear both active and pending requests on inhibition. Recovery alone never
  recreates previous motion. Preserve an unaffected axis's timers and lease.
- A relay activation requires 500 ms of remaining **command** lifetime.
  It also requires at least 500 ms of remaining motor duty.
  Feedback and network permission are independently revocable and may cut it
  short. Their deadlines must not be folded into the activation lifetime.
- Feedback age is inclusive at 500 ms; network and command deadlines are
  exclusive. Future timestamps and unrepresentable permission deadlines cannot
  grant movement. Keep these boundary semantics explicit.
- Repeated short jogs and reversals do not reset accumulated no-progress time.
  Fresh communication does not prove fresh feedback or physical movement.
- Direction evidence survives same-direction pulse gaps, and speed comparisons
  continue through startup, coast and idle. Reversal timing must cover coast
  plus maximum feedback age before establishing a new direction baseline.
- Duty history belongs to the actuator, across command sources and ownership
  changes. Only a full continuous rest replenishes it; reboot requires that
  rest again because there is no persistent duty history.

The dashboard reports remaining on-time and required rest separately for each
axis. It disables and releases only the affected hold; recovery requires a new
press. Control acknowledgements can update these values but cannot renew
telemetry freshness. Live relay timing remains 500 ms on / 2 s off; the shorter
candidate policy is available for offline simulation and commissioning.

The ADC task and output task remain separate. Output servicing wakes for state
changes and the earliest applicable deadline, with 100 ms idle service and
10 ms tracking service. Tracking commands keep the 750 ms relay-compatible
lease; a separate 20 ms execution cadence check inhibits stale targets before
pending activation. A stalled whole executor can only cut output on resumption
or hardware reset, so this is not a hard 20 ms physical release guarantee. Only the output task feeds the 500 ms hardware watchdog, after applying
the permitted GPIO state. All relay/command timing uses monotonic uptime; an
armed pass freezes its validated UTC-to-uptime mapping before execution.

## Verification

Exercise the production coordination and actuator path with independent
command, sensor and network stalls; an outage and recovery between service
passes; queued expiry; simultaneous stop and activation; endpoint refusal;
ownership transfer and old-owner disconnect; coalesced notifications; and
fault latches surviving recovery; repeated short pulses; per-axis duty exhaustion;
and unknown boot history. Invalid calibration must keep raw diagnostics
available. Dashboard tests must preserve fresh-press recovery, pointer release,
page suspension, and applied-output indicators.

Host tests do not establish physical output-release or stopping time. The board
checks in [DEVELOPMENT.md](DEVELOPMENT.md) cover GPIO release, relay timing,
watchdog reset and mechanical coast.
