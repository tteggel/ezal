# Tracking

The device follows one explicitly armed pass at a time. It plans the pass in
the pure core, freezes the accepted plan before moving, executes it on the
firmware's output task independently of any browser, and reports pointing
deviation, recovery and a final summary to every viewer. Execution uses the
same output boundary as manual control, so the
[control contract](CONTROL_CONTRACT.md) applies unchanged. The policy is
validated on the host; board timing and installation measurements follow the
[commissioning checklist](CALIBRATION.md#bench-verification), and the
[known model limits](CALIBRATION.md#known-model-limits) record where execution
departs from prediction.

The installation is an 80 cm parabolic dish with a custom helical feed that
can track over the top. Every numerical default below is a provisional
engineering choice until commissioning replaces it; modeled support is never a
claim of receiver lock or RF alignment. Recording, receiver tuning and terrain
or building obstruction are outside this design.

## Selection and ownership

- The device keeps at most one selected pass. Manual mode may have no
  selection; Tracking mode requires exactly one. Selecting a forecast row
  previews its plan; entering Tracking arms it. There is no queue, no automatic
  next pass and no automatic park move. Selecting a pass never moves the dish.
- [`TrackingLifecycle`](../crates/ezal-core/src/tracking/lifecycle.rs) owns the
  Manual/Tracking mode, the five-second exclusive
  [`OperatorLease`](../crates/ezal-core/src/operator.rs) and the command
  mailbox. Another connection cannot displace a valid lease. Expiry is
  exclusive, so a heartbeat at the deadline cannot resurrect it; reads,
  repeated claims and expired grants never renew it. Only a fresh, single-use
  WebSocket Ping/Pong renews the original grant.
- The operator lease is separate from the run. Browser disconnection, page
  suspension or lease expiry releases operator authority and inhibits manual
  movement, but leaves an armed or executing pass running. A new lease holder
  manages the existing run; claiming the lease neither stops it nor changes
  the selection. An old socket's close or delayed request cannot release a
  newer grant.
- Only the lease holder may select, replace, arm, prepare early, cancel or
  change mode. Other viewers observe position, deviation and tracking state.
  Fault handling and physical power removal never need a lease.
- Tracking mode rejects manual movement and manual stop messages on the
  device, and the dashboard hides the manual controls. Cancel and Return to
  Manual stay available throughout planning, preparation and execution; either
  inhibits pending and active output, returns to idle Manual mode and keeps the
  last selection and result for inspection. Another run needs a fresh arm.
- Loss of the device's Wi-Fi link or DHCP address stops the run and
  invalidates queued output; network recovery never resumes it. A failed
  internet refresh does not stop a run while its cached orbital and clock data
  stay valid. Reset always boots in Manual mode with no selection, run or
  persisted permission.

### Operator requests

Every mutation carries the connection-bound lease generation and a strictly
increasing, nonzero request number. Select, Arm and Prepare now also carry the
current state revision and a telemetry proof younger than one second, and
Select must name a row the device currently offers. Cancel and Manual ignore
the revision and the proof's age, so automatic replanning or a stale view
cannot prevent a stop, but they still require the current lease and request
order. An exact retry of the last request returns its original outcome; older
requests are rejected without repeating their effect. Replacing a pass stops
the old run before admitting its replacement. The transport binds the token to
its actual connection identity: lease serials order requests, they do not
authenticate them.

### Pass identity

A selection gets an opaque event identity that survives forecast refreshes.
A refreshed row is the same event only when it has the same catalog number,
overlaps the reception window and has AOS and LOS within 300 seconds of the
**original** selected window, so repeated refreshes cannot walk the identity
to another orbit. The comparison uses a complete forecast and ignores row
order. A missing, ambiguous, invalid or over-capacity forecast blocks
preparation with an explicit reason; a different satellite or orbit is never
selected silently.

## Planning

The [planner](../crates/ezal-core/src/tracking/planner.rs) is an incremental,
finite search in the pure core. It searches 24 route candidates: conventional,
inverted, and over-the-top bridges entered at 80°, 85° or 88° from either
orientation, each at three azimuth turns. Azimuth is unwrapped continuously,
geometry is refined near zenith and the azimuth wrap, and interpolation stays
inside the installed mechanical range. Mechanical angles are kept separate
from sky bearings: the direction `(A, E)` is also `(A + 180°, 180° - E)` where
travel permits, but changing representation alone does not reduce the azimuth
rate of a near miss of zenith. An over-the-top bridge may offset the sky
direction on purpose, and every following-error check includes that offset.

The planner needs a serviced, settled copy of the output owner's actuator,
network generation and fresh feedback acquired after coast. It copies relay
availability, duty and supervision history and never mutates live outputs.
Each candidate is evaluated with the production follower and actuator across
nine motor and measurement scenarios: nominal, slow and fast rates, mixed axis
rates, initial gearbox engagement, fixed and alternating one-code offsets, 0 to
150 ms acquisition latency and up to 150 ms channel skew. The follower accounts
for minimum pulse completion, target velocity, startup and coast, requires a
further 0.15° predicted improvement before a pulse, and waits for post-coast
feedback before another correction. The scenarios share the
[provisional motor profile](HARDWARE.md#provisional-tracking-profile); they do
not cover every intermediate combination or prove hardware performance.

Checks are at most 20 ms apart, with earlier output and acquisition deadlines
and exact AOS/LOS boundaries. Between checks the error is bounded using the
axis-rate vector and an assumed sky-rate bound of 2°/s, plus a 0.002°
numerical allowance. An observed violation of the sky-rate bound rejects the
job; an unobserved one remains outside the guarantee. Among supported routes
the planner prefers the smaller bounded peak error, then fewer reversals, fewer
output transitions and a shorter preparation.

Support requires every scenario to complete through final coast. Any interval
not established within the pointing budget is a predicted gap; the report keeps
the worst scenario's total gap time and up to 16 intervals, flagging
truncation. An unsupported attempt can never become an armable plan. A source,
work or storage failure invalidates the whole job, including earlier
candidates. Exhausting the search means no supported route was found, not that
every physical route is impossible.

Work and memory are bounded: at most 2,048 route points, nine scenarios and 16
gap intervals; one step performs at most one propagation and one simulator
service, or bounded route bookkeeping; a job stops at 20 million steps.
Preparation probes are limited to 300 s and reception windows to 30 minutes.

The offline timing tool,
[`examples/tracking-plan.rs`](../crates/ezal-core/examples/tracking-plan.rs),
plans synthetic passes and a pass from the recorded orbital fixture on the
host, with a synthetic full-travel calibration, and prints each report and the
planner's storage. `--candidate-timing` evaluates the proposed relay timing; it
never configures the device:

```bash
HOST=$(rustc --print host-tuple)
cargo run --locked -p ezal-core --target "$HOST" --example tracking-plan
cargo run --locked -p ezal-core --target "$HOST" --example tracking-plan -- --candidate-timing
```

### Pointing budget

`70 × wavelength / diameter` gives about 15.4° full half-power beamwidth at
1.7 GHz for an 80 cm dish, and the simple main-lobe model gives about 0.45 dB
loss at 3° offset; the coefficient depends on feed illumination, so these are
models, not measurements of the custom feed. Both approximations are in
[JPL IPN report 42-216C, PDF page 9](https://www.nasa.gov/wp-content/uploads/2026/02/42-216c.pdf);
the keyhole rate relationship is in
[JPL antenna positioning, section 2.2](https://deepspace.jpl.nasa.gov/dsndocs/810-005/302/302E.pdf).

The operational target is 3° total pointing deviation. 1° is reserved for
alignment, orbital and clock uncertainty, and the modeled path offset plus
controller following error must fit the remaining 2° allocation, including
feedback latency, quantization, backlash and coast. Commissioning establishes
whether the reserve is sufficient.

### Preparation

Preparation is scheduled before AOS, with an optional Prepare now for an armed
pass:

```text
preparation_lead = max(60 s, estimated_preparation + 20 s)
preparation_start = AOS - preparation_lead
```

Preparation probes estimate the worst readiness time from the current position,
chosen route, backlash, startup, coast and relay state, including a 2 s
settled dwell. Each complete verification then simulates the scheduled
preparation and the pass in one run, keeping on-time and relay holds at AOS.
Far-future idle waiting is skipped with its actual rest credit. Prepare now
requests a separately evaluated `Immediate` plan, so it cannot spend rest that
the scheduled start would have gained. A ready dish may wait at the preparation
pose beyond the probe limit; the planning job keeps its fixed work bound, so a
long early wait can fail with an explicit work-limit reason. Late preparation,
duty exhaustion, motion faults and travel violations produce explicit
rejections naming the scenario and phase.

### Updating and freezing the plan

Before preparation the device replans the selected pass automatically, in
either mode, whenever orbital elements, the clock or the measured starting
state change. Each planning ticket binds the selection to orbital, clock and
starting-state revisions and the preparation schedule. Publishing newer inputs
immediately withdraws the previous plan's permission to prepare; only the
latest job can publish, even if an older job finishes later; an unsupported or
failed replacement never falls back to an older success. A change in measured
pose, feedback health, consumed duty or network generation invalidates a
preview's starting state, so manual movement does too; fresh ADC timestamps
alone do not. Noisy, changing starting positions can therefore keep a preview
from completing.

Each published plan carries an exclusive `freeze_before_ms` deadline, the
earliest of its clock's admission deadline, LOS and the plan's own admission
deadline: AOS, or one millisecond after a late-acquisition entry. The plan
stays armable only before that instant, while it is the current ticket and
while its preparation time still fits.

Preparation atomically accepts the latest complete, valid plan and freezes its
orbital elements, observer, clock mapping, motion profile and mechanical route.
Later forecast, observer and clock updates leave the frozen run intact. The
run compares the actuator's installed configuration with the frozen one by
value and stops with `InstallationChanged` when they differ.

### Lifecycle events

Normal phases are Armed, Preparing, Ready, Following, Reacquiring and Complete;
cancellation and faults end with an explicit terminal reason.

| Event | Behaviour |
|---|---|
| Scheduled preparation becomes due | Freeze the current supported plan and issue a new run token |
| Prepare now | Request an `Immediate` plan; start only after that schedule passes evaluation |
| Measured readiness | Enter Ready after 2 s of distinct fresh, settled samples at the preparation pose; keep the frozen route |
| AOS while Ready | Enter Following, independently of operator presence |
| AOS during preparation without readiness | Enter a late-acquisition recovery episode counted from AOS |
| AOS without a valid frozen plan | Stop with `LatePreparation`; never execute an older or unsupported route |
| LOS while Following | Inhibit, return to Manual and keep the result and selection |
| Cancel, replacement or interruption | Invalidate old jobs, callbacks and commands; preserve an unconsumed stop |

At LOS, and after a cancellation, the dish stays where it stops, allowing for
normal mechanical coast. The mailbox keeps inhibition ahead of refreshes.
Execution commands are bound to the current run and keep their original source
deadline of at most 750 ms; LOS and cancellation are checked again on drain.
The production actuator remains responsible for feedback, network, motion,
relay and duty constraints, so a change of command source never resets
physical accounting.

## Execution inputs

A planning snapshot ([`ExecutionInputs`](../crates/ezal-core/src/tracking/inputs.rs))
owns the selected satellite's validated SGP4 constants, observer geometry,
clock anchor, ticket, window and planning assumptions; there are no mutable
orbit or clock accessors, and the browser cannot supply an orbit or a plan.
Only a complete framed download publishes elements to the
[catalog](../crates/ezal-core/src/catalog.rs); each accepted entry carries a
device-owned revision and monotonic completion time, and reading or failing to
refresh the cache changes neither. Orbital freshness uses the **TLE epoch**:
the one-day future-epoch and seven-day age limits must cover now through LOS,
widened by the admitted timing error. Publishing a result requires the same
selection, revisions, satellite, fractional AOS/LOS mapping and configuration
as its snapshot.

[`TrackingRuntime`](../crates/ezal-core/src/tracking/runtime.rs) wraps the
lifecycle with this input boundary and enforces it before preparation, before
admitting execution commands and on every mailbox drain. It takes coherent
device observations, including the timestamped network gate, never a cached
connected flag. The firmware publishes complete source revisions before
servicing a simultaneous preparation deadline and supplies current clock and
network observations on every drain.

### Execution clock

[`ntp`](../crates/ezal-core/src/ntp.rs) keeps millisecond UTC, the original
monotonic receipt instant, a never-reused synchronization revision and an error
estimate: half the network delay after server processing, half the nonnegative
root delay, root dispersion, server precision and a rounding allowance
([RFC 5905](https://www.rfc-editor.org/rfc/rfc5905.html#section-7.3)). SNTP is
unauthenticated and assumes honest time service and bounded drift. A sample
announcing a leap second stays usable for display but cannot authorize
tracking.

[`ExecutionClock`](../crates/ezal-core/src/tracking/clock.rs) reserves part of
the planner's uncertainty allocation for time:

| Assumption | Default |
|---|---|
| Local oscillator drift bound | 100 ppm |
| Maximum sample age | Six hours, exclusive; angular error usually limits it earlier |
| Alignment/orbit share of the 1° reserve | 0.5° |
| Timing share of the 1° reserve | 0.5° |
| Assumed maximum sky rate | 2°/s |
| Resulting maximum timing error | 250 ms |

Error grows from the original sample by `elapsed × drift_ppm / 1,000,000`: 6 ms
per minute at 100 ppm, so a sample with 50 ms initial error covers about 33
minutes from receipt to LOS. A preview checks the sample's current validity, so
a pass can be armed hours ahead; immediately before preparation the full error
and age bounds must fit through LOS, or preparation is blocked with a timing
reason.

After preparation, propagation always uses the frozen millisecond anchor. A new
sample is compared with that mapping: its discrepancy plus its uncertainty and
drift through LOS must fit the timing share. An acceptable sample validates the
run without shifting targets or deadlines; repeated small corrections cannot
move the anchor. An excessive correction, missing time, a stale or reversed
clock publication, or unusable source data stops the run with an explicit
reason.

## Deviation and reacquisition

Fresh paired feedback is compared with the frozen ephemeris at the oldest
channel's acquisition time, as the spherical angle between the measured
boresight and the satellite direction, so the estimate holds through azimuth
overlap and over-the-top travel. Each reading carries an uncertainty: the
plan's alignment/orbit/time reserve, the calibration's ADC resolution and the
worst-case azimuth travel during the paired-read latency bound. Missing, stale
or failed feedback removes the current reading and stops execution; the
retained maximum stays in the summary.

| Condition | Behaviour |
|---|---|
| Estimated deviation exceeds 3° | Show the deviation warning immediately |
| Deviation stays above 3° for 1 s of fresh samples | Enter Reacquiring |
| Deviation at most 2° for 2 s of fresh samples after the validated route join | Return to Following |
| An episode reaches 30 s without recovery, or LOS arrives | Stop and retain the reason |
| Feedback, motion supervision, timing or another output permission fails | Inhibit and stop; reacquisition never clears a fault |

Repeated reads of one sample add no dwell, and unobserved intervals break it.
Each episode owns `min(entry + 30 s, LOS)`; retries, replanning and browser
lease expiry never move that deadline. The runtime keeps observed time outside
tolerance and a final summary bound to the exact run, event and termination
reason; selecting or executing another pass does not relabel it.

### Intercept procedure

[Recovery](../crates/ezal-core/src/tracking/recovery.rs) is a finite search on
the **same frozen mechanical route**:

1. Inhibit and wait for coast to end and a post-coast feedback pair. Copy the
   serviced actuator, keeping relay holds, supervision and duty.
2. Set a launch 2 s ahead while the search runs incrementally. Try future
   joins at 1 s intervals: slew toward each future point, then rejoin the route
   at its original time.
3. Evaluate every configured scenario through the remaining pass and final
   coast, one propagation and one simulation service per step, within the
   planner's step ceiling. From the join onward, the intersample pointing bound
   must fit the smaller of 2° and the plan's allocation, leaving time for the
   recovery dwell.
4. Accept the earliest qualifying candidate in this search. At launch, require
   the same measured pose, policy, network generation and uninterrupted idle
   history; a changed start, missed launch or missed control deadline discards
   the path.
5. Follow only that validated target with the existing follower and actuator.
   If measured recovery misses the predicted join plus the dwell and the
   feedback age allowance, inhibit and retry. A search that finds nothing
   retries after 1 s within the original episode; nothing resets duty or
   faults.

On return to Following, the recovery follower continues as the normal
follower, keeping its physical pulse and coast history.

## Partial-pass admission

When no complete route qualifies, or the pass is already past AOS, a second
search ([`planner/partial.rs`](../crates/ezal-core/src/tracking/planner/partial.rs))
may admit **one consolidated missed interval**: initial late acquisition, or a
supported prefix, a bounded planned gap and a supported suffix. For every
scenario it replays preparation and the prefix, inhibits and waits for coast and
settled feedback, then runs the intercept search from that actuator copy
through original LOS and final coast. Prefix duty and relay history carry into
recovery; a nominal failure is never relabelled as support. Complete routes are
always preferred; within the partial search the first qualifying route wins.
The nested work shares the job's step limit.

A [partial plan](../crates/ezal-core/src/tracking/reception.rs) keeps the
original pass identity, AOS, LOS and route, and exposes the missed interval
relative to AOS, a fixed recovery entry and its 30 s or LOS deadline, the
earliest and latest validated joins, and the usable prefix and suffix. The
missed interval ends at the latest join plus the recovery dwell and the
feedback age allowance. For late acquisition the entry is
`max(AOS, snapshot time + 5 s)`; arming allows the device to freeze and begin
recovery at that entry with no preparation movement, and a missed entry
expires the plan rather than shifting it. At a planned gap the run enters
Reacquiring, clears queued following commands and revalidates a live intercept
from actual settled feedback; an earlier unexpected recovery keeps its own
token and deadline. Summaries keep the original window and the complete or
partial prediction beside measured deviation; reaching LOS never turns a
partial forecast into a complete-reception claim.

## Firmware execution

All shared tracking and forecast users run on the core-0 thread executor;
thread-mode borrows keep interrupts enabled and never span an await or I/O.

The output task services preparation, readiness, following and recovery
independently of browsers, publishing feedback and motion health before it
issues or drains a command. Each turn drains current inhibition and
permissions, services and applies the actuator, generates a tracking command
from that coherent snapshot (`execution_command` produces both following and
recovery commands), then drains again with the actual current time. Every
intermediate output transition reaches the GPIOs and the active follower's
history, and only then is the watchdog fed. Idle service is every 100 ms;
armed and live tracking aim for 10 ms, shortened by relay, feedback, network,
preparation, recovery-launch and LOS deadlines. A recovery launch is generated
and drained at its scheduled millisecond. If normal following misses the
plan's 20 ms maximum interval the run stops with `ExecutionDeadline`, and a
stale command cannot start a pending relay on drain; recovery instead discards
late work and holds or retries within its episode. A stalled executor cannot
cut output until it resumes or the 500 ms watchdog resets the board, so 20 ms
is an execution requirement, not a physical release guarantee.

The separate `tracking` task owns a statically bounded preview planner and
steps preview and recovery work (`step_reacquisition`) cooperatively, yielding
after each bounded unit. It never touches GPIO or the watchdog. Downloads yield
after every bounded parsing chunk and forecasting keeps its own task; complete
forecasts and their orbital snapshots publish atomically. A once-per-minute
`tracking timing maxima` log reports output service time, active service gap,
planner and recovery step times, worker poll time and planner steps.

## Dashboard

The dashboard selects exactly one device-offered pass by NORAD number and its
complete AOS/LOS window; selection stops any held manual input and previews
feasibility. The preview shows the route, modeled error bound, pointing
allowance, predicted reception and missed intervals and the UTC preparation
time; a partial plan is also named on the arming button. **Enter Tracking**
arms, **Prepare now** requests the earlier schedule, and **Cancel tracking** or
**Return to Manual** stops the run.

Every viewer sees the selection, preview, phase, measured and desired pointing
at the same ADC sample time, spherical error and its uncertainty, peak error
and time outside tolerance. Feedback expiry removes the current deviation
rather than showing a stale value. Reacquisition shows the attempt count,
search or hold reason, the episode's remaining time and a predicted intercept
ETA, distinguished from measured recovery, in text rather than colour alone.
The last finished run's summary survives later previews and runs. Disconnect,
page hiding and telemetry loss never cancel tracking; reconnecting restores
device state and never resends an earlier intent, and the dashboard never
retries an action automatically. A rejected or unacknowledged action is
visible.

## Wire contract

The `/ws` connection carries manual and tracking traffic, and each `/api/passes`
row carries its `norad_id` as a decimal string. Tracking intent fits the
256-byte receive bound with full-width identities:

```text
track:<proof>:<lease>:<sequence>:<revision>:select:<norad>:<aos_unix_s>:<los_unix_s>
track:<proof>:<lease>:<sequence>:<revision>:arm
track:<proof>:<lease>:<sequence>:<revision>:prepare
track:<proof>:<lease>:<sequence>:<revision>:cancel
track:<proof>:<lease>:<sequence>:<revision>:manual
```

`proof` is `telemetry.freshness.token`; `lease` and `revision` are
`telemetry.tracking.operator` and `.revision`. Every identity is a decimal
string in JavaScript, and intent sequences use `BigInt`. The server derives the
connection identity from the socket, checks the live lease generation and
applies the [request rules](#operator-requests). Network recovery invalidates
earlier proofs.

A `tracking_result` acknowledgement carries `sequence`, `ok`, `revision` and
`error`, and no freshness proof. The next coherent telemetry frame is
authoritative for mode, selection and execution state, and the browser waits
for it before admitting further movement. Its `tracking` projection carries:

- `revision`, `operator`, `mode`, `phase`, `run` and at most one `selection`;
- `plan.status`, an explicit blocked `reason` and a small supported `preview`;
- `feedback_stale`, `feedback_age_ms`, `deviation`, `recovery`, `termination`
  and the retained `summary`.

Reception windows and missed intervals are milliseconds relative to their own
`aos_unix_s`; recovery times are remaining milliseconds at the device
observation. Reasons are closed Rust enums written in their `Debug` form and
explained by the browser; no browser-provided string reaches server JSON or
HTML. The projection copies less than 1 KiB and no route or orbital snapshot,
performs no propagation or transition and cannot renew a lease. It is captured
with the feedback and output telemetry under one thread-mode borrow and encoded
outside it into the bounded telemetry buffer; an encoding failure closes the
session without touching the running pass.
[`tracking-telemetry.json`](../crates/ezal-core/tests/fixtures/tracking-telemetry.json)
pins the format and feeds the dashboard tests.

## Verification

Host tests must exercise the production follower, actuator and tracking
runtime with synthetic and recorded passes covering:

- exact zenith passes and near misses, azimuth overlap and pre-positioning
  from both mechanical extremes;
- slower and faster motors, backlash, pulse supervision and stalls;
- bounded reacquisition that succeeds and that fails;
- automatic plan updates racing preparation or cancellation, and stale client
  commands;
- lease expiry and transfer with tracking continuing, clock steps, duty
  exhaustion and LOS.

Dashboard tests must preserve the deviation and recovery displays, stale
deviation removal and Cancel staying available. Commissioning repeats the
pass with downloads, forecasting and several viewers active, and compares
simulated and measured trajectories, in the
[bench verification](CALIBRATION.md#bench-verification).

The host acceptance matrix in
[`planner/acceptance.rs`](../crates/ezal-core/tests/planner/acceptance.rs) and
[`tracking_inputs/execution/acceptance.rs`](../crates/ezal-core/tests/tracking_inputs/execution/acceptance.rs)
runs with the host suite. To print its predicted and executed comparisons:

```bash
./scripts/test-host.sh --test planner --test tracking_inputs acceptance -- --nocapture
```

Synthetic routes run a separate 10 ms driver around the production follower
and actuator, with quantized ADC pairs, latency, skew, startup, coast, backlash
and duty, and check the whole route and post-LOS coast against the 20 ms
planning result. The recorded run drives the full coordination runtime and
leased API through a 751 s METOP-B window from the recorded catalog, with the
browser disconnecting after arming, an ADC pair every 200 ms arriving with
100 ms latency, and output every 10 ms. The motor model is shared with the
planner, so these are integration checks under shared physical assumptions,
not evidence for those assumptions. They use synthetic 450°/180° travel and
confirmed-AC duty; the installed travel, relay timing, tolerance and scenarios
are unchanged.
