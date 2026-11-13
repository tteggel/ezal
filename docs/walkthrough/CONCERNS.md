# Questions and concerns log

Questions and known limits met while surveying and explaining the code for the walkthrough, grouped by the chapters that raised them. Each entry gives its source, its status, the consequence and what would settle it. None claims a demonstrated firmware defect. The walkthrough's last section is a digest of this file.

## Board and feedback questions log

Covers chapters 1 and 2. Every entry was re-checked on 2026-11-13 against local main at 79046c17e90caf0ed6f90e919111a353ba931085; line numbers are for that commit.

### B1 — How quickly an output is released after a freeze has not been measured
- Status: confirmed limitation (a validation gap, not a demonstrated defect).
- Source: [watchdog.rs L25–33](https://github.com/tteggel/ezal/blob/main/crates/ezal-firmware/src/watchdog.rs#L25-L33); bench step 4 in [CALIBRATION.md L363–369](https://github.com/tteggel/ezal/blob/main/docs/CALIBRATION.md?plain=1#L363-L369).
- Evidence: the code's own comment says RP2350 pad isolation latches can hold a pin's last level through a reset until the next boot claims the pin. The 500 ms watchdog setting is therefore a bound on when the chip restarts, not on when a switch opens.
- Consequence: a reader must not take 500 ms as the worst-case time a motor keeps running after the program freezes. Chapter 1 says this once, under "Then, a question about the last run".
- What would settle it: capture a direction pin on a scope or logic analyser through a forced stall and reset, as the bench checklist asks, and record reset-to-pin-low time on this board.

### B2 — A sensor fault at startup cannot be seen from the dashboard
- Status: question (a deliberate trade-off whose usability cost is not documented).
- Source: [main.rs L111–136](https://github.com/tteggel/ezal/blob/main/crates/ezal-firmware/src/main.rs#L111-L136).
- Evidence: a failed or timed-out self-test parks `main` before the radio is initialised or any web task is spawned. The only record is a `defmt` log line. The onboard light stays dark in this case, but it also stays dark while Wi-Fi is merely still joining.
- Consequence: someone who flashed by UF2 and has no debug probe cannot tell a dead sensor from a network that will not join. Chapter 1 states the fact under "Next, the sensor".
- What would settle it: the owner's decision on whether a debug probe is an accepted bring-up requirement, or whether a bounded local indication (a distinct light pattern, say) is wanted. The fail-closed order itself should not change.

### B3 — The sensor is assumed to have finished converting after a fixed 1 ms
- Status: question (an engineering assumption to verify, not a demonstrated defect).
- Source: [ads1015.rs L38–44](https://github.com/tteggel/ezal/blob/main/crates/ezal-firmware/src/ads1015.rs#L38-L44) and [L146–153](https://github.com/tteggel/ezal/blob/main/crates/ezal-firmware/src/ads1015.rs#L146-L153).
- Evidence: each single-shot read waits 1 ms for a conversion that nominally takes 625 µs at 1600 samples per second, rather than polling the converter's ready bit. The comment ties the wait to the chosen rate.
- Consequence: lowering the data rate without lengthening the wait would return the previous conversion; the result would look like a valid, fresh reading.
- What would settle it: the worst-case conversion time from the datasheet for the installed part, checked against the 1 ms wait, or a ready-bit poll.

### B4 — Calibration and travel limits describe one installation, and its ends are not the rotator's mechanical stops
- Status: confirmed limitation.
- Source: [position.rs L320–336](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/position.rs#L320-L336); [interlock.rs L51–64](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/interlock.rs#L51-L64).
- Evidence: the checked-in ends cover 0–360° azimuth and 0–90° elevation, with elevation's voltage falling as the angle rises. The end-of-travel guard assumes 7°/s, 3°/s and a 500 ms coast; the source calls these the manual's rates "plus margin" and a "provisional coast allowance". The G-5500 itself turns 450° and 180°.
- Consequence: the "end" that chapter 2's last figure guards is the end of calibrated travel, not a hard stop. An earlier draft of the chapter called it a stop where "the rotator and its cables can go no further"; the source does not support that, and the text now says "end of travel". A fresh reading from a miscalibrated installation would still look plausible.
- What would settle it: measured ends, speeds and coast for this dish, recorded in the worksheet in CALIBRATION.md.

### B5 — Are the checked-in calibration voltages measurements or placeholders?
- Status: question.
- Source: [CALIBRATION.md L22–30](https://github.com/tteggel/ezal/blob/main/docs/CALIBRATION.md?plain=1#L22-L30); [position.rs L320–336](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/position.rs#L320-L336); [HARDWARE.md L190–194](https://github.com/tteggel/ezal/blob/main/docs/HARDWARE.md?plain=1#L190-L194).
- Evidence: the table in CALIBRATION.md heads the column "placeholder ADC range", while the sentence under it and the doc comment in position.rs call the same numbers the current installed values. The numbers also disagree with the nominal model in HARDWARE.md, which expects 0.90–2.02 V at the sensor for the manual's 2.0–4.5 V: azimuth's 114 mV at 0° implies about 0.25 V at the control box, and elevation falls from 1281 mV to 58 mV as the angle rises, where the manual's signal rises.
- Consequence: chapter 2 shows the divider with the manual's 4.5 V and then a calibration line whose low end is far below what that divider would give. A careful reader will notice. The chapter says only "the numbers checked into the code say", which is true either way.
- What would settle it: the owner confirming whether these four voltages were measured on the installed rotator (and whether the control box's output trimmers explain the difference), then correcting whichever of the two documents is wrong.

### B6 — How long a healthy read takes is not known
- Status: confirmed limitation (unmeasured).
- Source: [ads1015.rs L44](https://github.com/tteggel/ezal/blob/main/crates/ezal-firmware/src/ads1015.rs#L44); worksheet row "Paired ADC latency and A0-to-A1 capture delay", still TBD, at [CALIBRATION.md L267](https://github.com/tteggel/ezal/blob/main/docs/CALIBRATION.md?plain=1#L267).
- Evidence: the source fixes only the two 1 ms conversion waits. The I²C transfers around them, and the time the task spends waiting for its turn, are not stated anywhere.
- Consequence: chapter 2's ageing figure opens with a 20 ms read, which is an illustration, not a measurement; the caption under the budget figure says only that a healthy read is "far quicker" than 150 ms. The interval between readings ("about ten times a second") is the 100 ms pause plus this unknown.
- What would settle it: the worksheet measurement.

### B7 — The step figure draws an ideal sensor, and "one sensor step" has two sizes
- Status: confirmed limitation (of the walkthrough's figures; no source defect).
- Source: [position.rs L165–176](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/position.rs#L165-L176); [interlock.rs L22](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/interlock.rs#L22) and [L147](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/interlock.rs#L147).
- Evidence: the figure under "From voltage to number to angle" rounds the true voltage to the nearest 2 mV and assumes the voltage follows the straight line exactly. A real converter adds offset, gain error and noise, and a real potentiometer is not perfectly linear. Separately, the text gives the plain step as 0.375° and 0.147°, while the end-of-travel guard allows 2.5 mV (one count plus a 0.5 mV rounding margin), which is 0.47° and 0.18°.
- Consequence: the figure's "firmware computes" value is exact only for an ideal sensor. The last figure labels its allowance "one sensor step, rounded up" to account for the larger number; a reader comparing the two sections may still wonder.
- What would settle it: the worksheet row "Feedback noise at rest and position residual" (TBD) would say how far the ideal is from this installation.

### B8 — Startup durations in chapter 1 are not measured
- Status: confirmed limitation (unmeasured).
- Source: [main.rs L88–101](https://github.com/tteggel/ezal/blob/main/crates/ezal-firmware/src/main.rs#L88-L101); nothing in the source or docs gives a time for the boot ROM or for joining Wi-Fi.
- Evidence: the chapter says the chip's loader runs "for the first few milliseconds" and that the dashboard is available "shortly afterwards". Both are qualitative. The pins are undriven for the first of these intervals and held open only by the interface board's pull-down resistors.
- Consequence: the startup figure is explicitly not to scale. Nobody should read a duration off it.
- What would settle it: a logic-analyser capture from power-on to the first driven-low pin, and a log timestamp at the first ready network.

### B9 — The watchdog is not yet running between taking the pins and the sensor test
- Status: confirmed limitation (benign as far as the source shows).
- Source: [main.rs L98–112](https://github.com/tteggel/ezal/blob/main/crates/ezal-firmware/src/main.rs#L98-L112); [drive.rs L111–119](https://github.com/tteggel/ezal/blob/main/crates/ezal-firmware/src/drive.rs#L111-L119).
- Evidence: `spawner.spawn` only queues the direction task. The task first runs, and calls `watchdog::start`, when `main` first awaits, which is inside the sensor self-test. In between, `main` runs synchronous setup with all four pins already driven low and owned by the queued task.
- Consequence: a hang in that short stretch would not be caught by the watchdog, but nothing can raise a pin during it. The startup figure shows the countdown starting at step 3 (the spawn); the note beside the excerpt says the task first runs a few lines later.
- What would settle it: nothing needs settling unless code that can block is added between the spawn and the first await.

## Control-path questions and concerns (chapters 3 and 4)

Re-verified 2026-11-13 against local main at 79046c1; every line reference below was re-read at that commit. No firmware was changed. C1–C5 carry over from 2026-11-05 with line references corrected; C6–C8 are new this round. Paths are under `crates/` unless they start with `docs/` or `design/`.

### C1 — Being the operator is not authentication

- Status: confirmed limitation (documented scope).
- Source: `ezal-core/src/session.rs:88–94` ("a bounded age check, not authentication"), `ezal-core/src/operator.rs:1–6`, `ezal-core/src/coordination.rs:171–182` (a connecting client claims vacant control).
- Consequence: a token proves a recent round trip and the operator lease stops two dashboards fighting; neither identifies a person. Anyone who can reach the board on the network can take control when it is free.
- What would settle it: nothing within the documented trusted-LAN scope. An untrusted network needs authentication and transport protection designed separately.
- In the walkthrough: said once, in chapter 3 under "Two things checked before any of that".

### C2 — Run-time accounting cannot see front-panel use

- Status: confirmed limitation.
- Source: `ezal-core/src/duty.rs:1–6`, `docs/CONTROL_CONTRACT.md:59–63`.
- Consequence: the 180 s / 900 s budget counts only time ezal's own pins were high. Use of the G-5500's own switches is invisible, and "Skip startup rest" is the operator's word, not a temperature.
- What would settle it: an operating procedure for the installation (full rest after front-panel use), and identifying the AC or DC variant before choosing `DutyConfig::CONFIRMED_AC`.
- In the walkthrough: said once, in chapter 4 after the tank figure.

### C3 — Transistor drive margin is asserted, not measured

- Status: question for the bench.
- Source: `docs/HARDWARE.md:146–156` (≈1.1 mA base current "for any reasonable sink current"), `design/circuit.py:132–210`.
- Consequence: a high pin and a lit LED show what was asked for. Whether the transistor pulls the G-5500 input low enough, and lets go cleanly, on this controller is unproven. The chapter 4 circuit figure labels its currents as design estimates.
- What would settle it: measure the input's sink current, the collector's low voltage and the release behaviour on the installed controller.

### C4 — Software deadlines need a running executor

- Status: confirmed limitation.
- Source: `ezal-firmware/src/drive.rs:124–172` (loop; watchdog fed at line 150 after both service passes), `docs/CONTROL_CONTRACT.md:65–71`.
- Consequence: the 300 ms network cut, lease expiry and every other timed stop are run by the direction task. If the executor stalls, only the 500 ms watchdog reset remains, and a pin can keep its level through that reset until startup takes it back (chapter 1's caveat).
- What would settle it: bench measurement of pad behaviour through a watchdog reset and of service latency under load.
- In the walkthrough: said once, in chapter 4 after the loop excerpt, pointing back to chapter 1.

### C5 — Motion-fault thresholds are starting points

- Status: confirmed limitation (provisional values).
- Source: `ezal-core/src/motion.rs:10–14, 40–47`, `ezal-core/src/actuator.rs:28–35`.
- Consequence: the 5 s no-progress timeout, the code tolerances and the 500 ms coast bound are engineering guesses until measured on the mechanism. The chapter 4 figures leave motion out, so they neither test nor illustrate these numbers.
- What would settle it: commissioning measurements of coast, speed and feedback noise.

### C6 — A token-capped lease can fall below the minimum on-time during a steady hold (new)

- Status: question.
- Source: `ezal-core/src/session.rs:56–61` (comment and compile-time checks), `session.rs:175` (`deadline?.min(now_ms + MOVEMENT_LEASE_MS)`), `ezal-core/src/drive.rs:17` and `drive.rs:208–214` (a start needs `now + 500 <= activation_limit_ms`), `ezal-core/src/dashboard.html` (a hold quotes the newest token received).
- Observation: the comment says a steadily held control "must also leave the minimum on-time on its lease between refreshes, or the actuator would defer an activation until the following refresh", and the assert `COMMAND_REFRESH_MS + OUTPUT_MIN_ACTIVE_MS <= MOVEMENT_LEASE_MS` secures that for a full 750 ms lease. But `admit` also caps the lease at the quoted token's expiry, and `COMMAND_MAX_AGE_MS - TELEMETRY_PERIOD_MS = 500 = OUTPUT_MIN_ACTIVE_MS`, with no margin. A hold quoting the token issued at time T is capped at T + 1000, so from T + 500 (when the next report goes out) until the first hold quoting the newer token is admitted, the lease in force has less than 500 ms left. That gap is about two one-way delays plus up to one 150 ms refresh, in every 500 ms telemetry period.
- Consequence: no safety effect. A start that falls due in that window (typically the end of an off-time during a held button) is deferred, by up to roughly 210 ms at a 30 ms one-way delay. As the one-way delay approaches 175–250 ms the window covers the whole period, and a held button whose holds are all accepted would rarely or never start the motor.
- What would settle it: a host test that steps `CommandFreshness` and `ActuatorGuard` through a steady hold with telemetry at its real phase and a non-zero delay and records the longest deferred start; or a decision that the deferral is acceptable, with the comment at `session.rs:56–58` reworded; or a margin such as `COMMAND_MAX_AGE_MS >= TELEMETRY_PERIOD_MS + COMMAND_REFRESH_MS + OUTPUT_MIN_ACTIVE_MS` plus a delay allowance.
- In the walkthrough: chapter 3's note on the three asserts says a token-capped lease can have less than 500 ms and that a start falling due in one waits for the next hold. The chapter 3 hold figure shows the dips. The chapter 4 live chart does not model the cap (see C8).

### C7 — The minimum on-time is not a floor for a move made by hand (new)

- Status: confirmed limitation (by design).
- Source: `ezal-core/src/drive.rs:16` ("safety inhibition and operator release bypass it"), `drive.rs:361–383` (`command_until`), `ezal-core/src/dashboard.html` (releasing the last button sends `stop`).
- Consequence: a quick tap on the dashboard can produce a pulse as short as the gap between the press and the stop being read, a few milliseconds, followed by the 2 s off-time. So the G-5500's relays can be asked for a very short click, at most once per two seconds per axis. The 500 ms rule only stops the firmware starting a pulse it already knows the lease cannot cover, and lets a tracker pulse finish.
- What would settle it: bench observation of how the controller's relays respond to a very short closure, and whether that matters to them.
- In the walkthrough: stated in chapter 4 ("This rule never holds a pin up against you") and visible in the live chart.

### C8 — What the chapter 3 and 4 figures simplify (new)

- Status: resolved (disclosed in each caption; listed here so the page's log has them in one place).
- Source: `docs/walkthrough/sections/control.js` (models between the `models` markers, ported from `session.rs:158–178`, `tracking/mailbox.rs:18–30`, `drive.rs:195–215` and `361–394`, `actuator.rs:123–168`, `duty.rs:95–109`).
- Token and hold figures (chapter 3): every message takes a fixed 30 ms; a stall holds messages back and delivers them together when it ends; the stall always begins at 1100 ms. The browser's side follows `dashboard.html` (holds every 150 ms quoting the newest token, a stale-report check that gives up and sends stop, release on `input_required`).
- Mailbox figure: every move note is given the full 750 ms; real notes carry whatever `admit` returned.
- Live chart and four cases (chapter 4): one axis; messages arrive instantly and every hold earns the full 750 ms, so the deferral in C6 never appears; position, run-time and motion checks always pass; the model is serviced exactly at each deadline, where the real loop wakes on a signal, a reported deadline or its 100 ms (10 ms while tracking) period.
- Tank figure: one motor, time jumps in whole button presses.
- Circuit figure: the inside of the G-5500 is a drawing of what it does, not of its circuit; currents are the estimates in `docs/HARDWARE.md:146–167`.
- Checked: the models reproduce, in a node script run against `control.js`, the four pin traces previously reviewed for the relay demo (short lease, tracker release, network loss at 300 ms exclusive, stop winning a tie) and the `admit` rules (lease capped by token, late hold refused, repeated sequence refused). They have not been compared with the Rust host tests' output directly.

## Orbit and pointing-geometry concerns

Surveyed 2026-11-13 against `crates/ezal-core/src/orbit.rs` and `passes.rs` at 79046c17e90caf0ed6f90e919111a353ba931085. No firmware changes made. Numbers marked "measured" come from a throwaway cargo project that depends on `ezal-core` by path (run with `--offline`, outside the repository) and from the model section of `sections/orbit.js` run under node on the same inputs. None of these entries claims a demonstrated firmware defect.

### O1 — The walkthrough's figures use a simpler orbit model than the firmware

- Status: confirmed limitation of the walkthrough, not of ezal. Stated once on the page.
- Source: [`Orbit::look_at`](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/orbit.rs#L394-L410) calls the `sgp4` crate; `sections/orbit.js` `propagate` moves the satellite round a Kepler ellipse and adds only the steady (secular J2) drift of the ring, its low point and its lap time.
- Evidence (measured, fixture `weather-2026-10-07.tle`, Clevedon): for the page's example pass (METOP-B, 2026-10-07 19:41–19:54 UTC) position differs from SGP4 by at most 11.4 km and the pointing direction by at most 0.44°; rise and set differ by 1 s and the peak by 0.02°. Over 24 h for all seven fixture satellites: position within 18.2 km, every pass found, rise and set within 2 s, peaks within 0.11°, direction while above the horizon within 0.66°.
- Evidence (measured) for everything after propagation: `sidereal`, `groundStation` and `lookAt` in `orbit.js` agree with `GroundStation::new` and `look_at` to 1.2 × 10⁻¹³ degrees on 400 random positions and times, including points within 30 km of the station, and on SGP4's own positions for every satellite.
- Consequence: figure readouts are not ezal's numbers to better than about half a degree. The prose quotes ezal's own pass times separately.
- What would settle it: nothing outstanding. Re-run the comparison if `orbit.rs`, the fixture or `orbit.js` changes.

### O2 — `from_tle` accepts lap rates no real orbit can have

- Status: question.
- Source: [`Orbit::from_tle` range checks](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/orbit.rs#L353-L371). Mean motion is accepted up to 18 laps a day (an 80-minute lap). A ring that small is about 6,150 km from Earth's centre, inside the planet (radius 6,378 km); a lap that grazes the surface is about 17 a day.
- Evidence (measured, METOP-B's record with only the mean motion and check digit rewritten): 18.1 is rejected. 17.9 is accepted, every `look_at` succeeds, the position is 6,168 km from the centre and elevation never exceeds −14°, so a search would report a successful forecast with no passes. 17.2 is accepted and 2,646 of 2,880 `look_at` calls over a day fail, which the search reports as a failed prediction. 16.5 behaves like a normal very low orbit.
- Consequence: none known in practice. The source comment says these are search limits, not validity limits, and a genuine feed will not carry such a record. A damaged or forged one that passed the checksum could show as "no passes" rather than as a failure.
- What would settle it: a decision on whether the upper bound is meant to reject impossible orbits (then about 16.5) or only to bound the search.

### O3 — Station height is an unsurveyed zero

- Status: confirmed limitation, documented in the source.
- Source: [`Observer::CLEVEDON`](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/orbit.rs#L121-L127); used in [`GroundStation::new`](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/orbit.rs#L197-L211).
- Evidence (measured with the page's model): 100 m of height changes the direction by at most 0.004° during the example pass. By contrast, treating the Earth as a ball of radius 6,371 km moves the station 21.7 km and the direction by up to 1.16°, which is why the flattening is worth its three lines.
- Consequence: negligible for pointing.
- What would settle it: a surveyed height, if anyone wants one.

### O4 — The direction is a straight line

- Status: confirmed limitation, documented in the source.
- Source: [`GroundStation::look_at` doc comment](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/orbit.rs#L219-L224): no atmospheric bending, no terrain mask, UT1 taken as UTC, polar motion ignored.
- Consequence: predicted rise and set are crossings of a geometric 5°, not moments of reception. The size of the difference at this site has not been measured. The walkthrough says this once, after the last `look_at` excerpt.
- What would settle it: comparing predicted and observed signal at rise and set on the installed dish.

### O5 — Azimuth has no meaning straight overhead

- Status: resolved by T11 in the tracking log.
- Source: [`look_at` doc comment and azimuth lines](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/orbit.rs#L222-L265). ezal's own search finds a 77° METOP-B pass at 09:33 UTC on 8 October 2026 in the same fixture.
- Evidence (measured, ezal's own output): around that peak the azimuth goes from 55.7° at 09:39:00 to 156.6° at 09:40:00, about 100° in one minute.
- Consequence: nothing in `orbit.rs` is wrong, but whatever consumes these directions must cope with a bearing that can change this fast. T11 shows that it does: the swing is a third of the assumed motor speed, and the follower lengthens its pulses.
- What would settle it: nothing outstanding; chapter 6 replays this pass.

### O6 — Downloaded records are checked for form, not for origin

- Status: confirmed limitation, accepted by the operator.
- Source: [`well_formed`](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/orbit.rs#L284-L317); [PASS_PREDICTION.md lines 98–101](https://github.com/tteggel/ezal/blob/main/docs/PASS_PREDICTION.md#L98-L101) (plain HTTP, accepted on 2026-10-11).
- Consequence: the walkthrough says the board "does not trust" the lines. That means layout, check digit, catalogue number, ranges and age. A well-formed forgery would pass all of them and move the forecast. What the motors may then do is bounded by the checks in chapters 3, 4 and 6, not by anything in this chapter.
- What would settle it: nothing within the accepted scope.

### O7 — The seven-day limit is a policy, not an accuracy bound

- Status: confirmed limitation, documented in the source.
- Source: [`MAX_TLE_AGE_S`](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/orbit.rs#L29-L32) and [`fresh_epoch`](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/orbit.rs#L319-L327).
- Evidence (measured): running SGP4 on METOP-B's record with and without its drag term moves the satellite 0.04 km after one day and 2.0 km after seven. The walkthrough quotes the second figure.
- Consequence: for this high orbit, drag inside the limit is small. A manoeuvre between records is not bounded by any number here.
- What would settle it: comparing positions from successive real records for the same satellite.

### O8 — Resolved: which sidereal-time formula

- Status: resolved.
- Source: [`look_at` line 231](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/orbit.rs#L231) uses `afspc_epoch_to_sidereal_time`; the `sgp4` crate's documentation calls its `iau_` variant the recommended one.
- Evidence (measured): at 2026-10-07 19:46:00 UTC the two differ by 4 × 10⁻⁹ degrees.
- Consequence: none.

## Tracking and bench concerns

Surveyed 2026-11-05 and re-verified 2026-11-13 against main 79046c17e90caf0ed6f90e919111a353ba931085; every line reference below was checked again on the second date. No firmware was changed. T6, T7 and T11 rest on running the repository's own `Planner` and `Simulation` from a scratch crate over the recorded fixture `crates/ezal-core/tests/fixtures/weather-2026-10-07.tle` for the observer `Observer::CLEVEDON`; nothing was run on a board.

### T1 — The rehearsal's rhythm changes its result

- Status: confirmed limitation (documented by the repository).
- Source: [CALIBRATION.md, known model limits](https://github.com/tteggel/ezal/blob/main/docs/CALIBRATION.md#L446-L455); [`PlanningConfig::default`, `step_ms: 20`](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/tracking/planner.rs#L80-L93); [`TRACKING_PERIOD`, 10 ms](https://github.com/tteggel/ezal/blob/main/crates/ezal-firmware/src/drive.rs#L24-L27).
- Consequence: the planner rehearses every 20 ms and the firmware runs a pass every 10 ms. In the recorded METOP-B window the planner predicts a 2.218° bounded peak and the 10 ms replay peaks at 2.345°. "Supported" is a statement about a model, not a guarantee about the dish.
- Would settle it: measured scheduling, motor and pointing errors on the installed rotator, and rehearsals at more than one timing phase.

### T2 — Whether the Pico can plan a recovery in time is unmeasured

- Status: confirmed limitation (documented by the repository).
- Source: [CALIBRATION.md, known model limits](https://github.com/tteggel/ezal/blob/main/docs/CALIBRATION.md#L456-L463); [`WORK_PER_TICK` in the acceptance test](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/tests/tracking_inputs/execution/acceptance.rs#L15-L17); [`INTERCEPT_PLANNING_LEAD_MS`](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/tracking/recovery.rs#L50-L56); [the firmware's planning worker](https://github.com/tteggel/ezal/blob/main/crates/ezal-firmware/src/tracking.rs#L158-L229).
- Consequence: the recorded successful recovery search takes 398,964 work units. The test grants 2,048 units per virtual 10 ms, which is a test setting and not a measurement of the board; at 128 units it misses every launch. A board too slow to finish a search inside the 2 s lead would stop at its deadline rather than recover.
- Would settle it: timing whole searches on the Pico with browsers connected and a forecast running.

### T3 — A run's clock is fixed, but a later time check can still stop the run

- Status: resolved (a question about wording; the code is clear).
- Source: [`ExecutionClock::validate`](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/tracking/clock.rs#L138-L160).
- Consequence: none for the firmware. A later answer from the time server never moves the run's schedule; if it disagrees by more than the 250 ms allowance (after its own uncertainty and 100 ppm of drift to the window's end) the run stops with `TimingError::Correction`. Chapter 6 excerpts the function and its figure applies the same sum.

### T4 — The board and rotator have not been through the bench checklist

- Status: confirmed limitation.
- Source: [README, validation status](https://github.com/tteggel/ezal/blob/main/README.md#L14-L17); [CALIBRATION.md, bench verification](https://github.com/tteggel/ezal/blob/main/docs/CALIBRATION.md#L328-L438); [HARDWARE.md, watchdog reset](https://github.com/tteggel/ezal/blob/main/docs/HARDWARE.md#L346-L360).
- Consequence: host tests and rehearsals say nothing about what a pin does while the watchdog resets the chip (RP2350 pads can hold their last level until boot takes them over), how far a pulse really carries the dish, how much the readings flicker, or where the beam points. The board has no separate motor-enable interlock, so a switch that fails closed is outside anything the firmware can stop. Chapter 7 says each of these once.
- Would settle it: the measurements in the existing procedure, including the pin during the reset window.

### T5 — "Ready" still moves the dish if it drifts

- Status: resolved (an earlier draft of the walkthrough was wrong; the code is clear).
- Source: [`TrackingRuntime::execute`, target while preparing or ready](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/tracking/runtime.rs#L529-L550).
- Consequence: none for the firmware. `Preparing` and `Ready` both run the follower against the first point of the route, so a ready dish that is pushed off is pulsed back. Chapter 6 excerpts these lines.

### T6 — With the numbers checked in, the planner blocks every recorded pass

- Status: confirmed limitation.
- Source: [`PlanningConfig::default`: 3° tolerance, 1° reserve, nine scenarios](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/tracking/planner.rs#L80-L160); [`RelayConfig::DEFAULT`: 500 ms on, 2 s off](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/drive.rs#L16-L21); [`MotorProfile::AC_50HZ`](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/tracking.rs#L138-L152); the repository's own test [`production_assumptions_and_exhausted_duty_refuse_the_recorded_pass_without_motion`](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/tests/tracking_inputs/execution/acceptance.rs#L267-L270) and example [`tracking-plan.rs`](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/examples/tracking-plan.rs#L85-L90), which reports the recorded FENGYUN 3C pass as unsupported.
- Evidence gathered here: the real `Planner`, given `PlanningConfig::default()`, `HARD_CODED_CALIBRATION` and `DutyConfig::DEFAULT`, blocked all 16 passes that the fixture's satellites make over Clevedon between 15:00 UTC on 7 October 2026 and 03:00 the next day, and the 77° METOP-B pass of T11. Eight failed in a rehearsal (T7), three on the pointing budget, and five cross north and have no route (T8).
- Consequence: the shortest pulse carries the azimuth 3.6° (0.7 s at 5.14° per second) and the follower waits for half of that plus 0.15°, so the staircase alone uses about 1.95° of the 2° left for it. On the dashboard every pass would read "Tracking blocked" until the placeholders are replaced. This is the planner refusing on cautious assumptions, not a defect, but a reader should not expect Enter Tracking to be available today.
- Would settle it: the measured motion profile from CALIBRATION.md, and a decision on [`RelayConfig::TRACKING_CANDIDATE`](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/drive.rs#L45-L50) (500 ms off, 1 s before reversing), which only simulations use at present.

### T7 — The fast-motor rehearsals run at exactly the speed the stall check calls implausible

- Status: question (possible defect in the provisional margins).
- Source: [`MotorProfile::AC_FAST`: 7.0 and 3.0° per second](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/tracking.rs#L167-L178); [`TravelProfile::DEFAULT`: `max_speed_deg_per_s` 7.0 and 3.0](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/interlock.rs#L51-L64); [the speed check](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/motion.rs#L169-L176); [the scenarios that mix a fast axis with alternating one-step noise](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/tracking/planner.rs#L107-L134).
- Evidence gathered here: of the 11 passes in T6 that have a route, 8 ended with `Simulation(Motion(ElevationImplausibleSpeed))` in scenario 3 (slow azimuth, fast elevation, readings alternating by one converter step) while following. The other 3 completed all nine scenarios and failed the pointing budget with bounded peaks of 3.6° to 3.7°. Given the full 450° and 180° of travel instead, 9 of the 16 ended with `AzimuthImplausibleSpeed` in scenario 4 and 7 with `ElevationImplausibleSpeed` in scenario 3. The repository's own example completes all nine scenarios for a synthetic pass through the zenith, so the fault is not universal.
- Consequence: a rehearsed motor that turns at exactly the supervisor's speed limit, read through noise, sits on a knife edge of that check, and which side it falls depends on the pass and the calibration. Passes are then refused for a reason that says nothing about pointing. The dashboard has no wording for it and would show the raw path, "Tracking blocked: Unsupported Some Simulation Motion Elevation Implausible Speed" ([fallback](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/dashboard.html#L717-L725)).
- Would settle it: the maintainer saying whether the equality is intended (the fast scenario as a deliberate edge case) or whether the supervisor's bound should sit above the fastest rehearsed motor; then a measured maximum speed.

### T8 — The checked-in travel leaves one usable route, and none for a pass that crosses north

- Status: confirmed limitation.
- Source: [`HARD_CODED_CALIBRATION`: 0–360° and 0–90°](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/position.rs#L320-L336); [`route::choice`, 24 candidates](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/tracking/route.rs#L96-L111); [the planner rejecting a route that leaves the calibrated travel](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/tracking/planner.rs#L653-L662).
- Consequence: a whole-turn offset needs azimuth beyond 360° and a flipped or over-the-top route needs elevation beyond 90°, so only the plain route can fit. Five of the 16 passes in T6 cross north and are rejected before any rehearsal; the dashboard would show "Tracking blocked: Unsupported None". The repository's acceptance tests reach the flipped route only by giving the simulated rotator 450° and 180°.
- Would settle it: calibrating the rotator's real overlap and elevation range, and a plain-language reason on the dashboard for "no route fits the travel".

### T9 — Two rules disagree by one millisecond about a reading that is exactly 500 ms old

- Status: question (harmless as far as can be seen).
- Source: [the interlock allows `age_ms <= FEEDBACK_MAX_AGE_MS`](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/interlock.rs#L201-L210); [a tracking run stops at `age >= FEEDBACK_MAX_AGE_MS`](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/tracking/runtime.rs#L795-L799).
- Consequence: at exactly 500 ms a manual movement may continue for that instant while a tracking run stops. The stricter rule is the one that ends a run, so nothing unsafe follows; the walkthrough says "more than 500 ms" for the pin and "the tracking run stops" without the boundary.
- Would settle it: a comment or shared helper saying which boundary is intended.

### T10 — Where the follower's decision lives

- Status: resolved (a note for whoever maintains the walkthrough).
- Source: [`AxisFollower::request`](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/tracking/controller.rs#L28-L77); [`TrackingRuntime::execute` choosing the target and wrapping the answer](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/tracking/runtime.rs#L529-L576).
- Consequence: the rule that turns a pointing error into a pulse is in `controller.rs`, not `runtime.rs` as the revision brief assumed. Chapter 6 excerpts both.

### T11 — A nearly overhead pass is followed, not refused for speed (answer to O5 in the orbit log)

- Status: resolved for the question asked; the pass itself is blocked today for the reasons in T6 and T7.
- Source: [the running branch of `AxisFollower::request`](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/tracking/controller.rs#L41-L59); [`MotorProfile::AC_50HZ`, azimuth 360° in 70 s](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/tracking.rs#L138-L152); [over-the-top routes start at 80°, 85° or 88°](https://github.com/tteggel/ezal/blob/main/crates/ezal-core/src/tracking/route.rs#L96-L111).
- Evidence gathered here, for METOP-B from 09:33:00 UTC on 8 October 2026 (773 s, peak 77.3°, azimuth from 16.5° round through east to 192.1°, and from 55.7° to 156.6° between 09:39 and 09:40): the fastest azimuth change is 2.3° per second and the fastest true motion across the sky 0.51° per second. The assumed azimuth motor turns 5.14° per second, 308° a minute, so the swing is a third of what the motor can do; and at 77° a degree of azimuth is 0.22° of sky. The real `Simulation` with the expected motors, starting settled at the rise point, follows the whole pass: 42 azimuth pulses, 11 of them longer than the 500 ms minimum and the longest 1.26 s at the peak, worst error 2.12°, 3.5 s beyond 2°. With slow motors the worst error is 1.96°; with fast motors 3.03°.
- What the planner does: with the checked-in limits only the plain route fits (46 of the 48 route attempts are rejected on geometry), and the pass peaks below the 80° at which the lowest over-the-top route begins. With nine scenarios it is blocked (`ElevationImplausibleSpeed`, scenario 3). Rehearsed with the expected motors alone it would be "Partial reception supported", missing 14 s about two minutes in, well before the peak.
- Consequence for the walkthrough: chapter 5 should not say this pass outruns the azimuth motor. Chapter 6 lets the reader replay it and says what changes (longer pulses) and what the dashboard would show (blocked, like every pass today).
