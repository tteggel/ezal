use ezal_core::orbit::{
    Catalog, GroundStation, LookAngle, Observer, ObserverError, Orbit, Update, MAX_TLE_AGE_S,
    SATELLITES,
};
use ezal_core::passes::{
    needs_forecast, Config, ConfigError, Elements, Error, Forecast, Job, Outcome, Pass, Report,
    Search, Status, HORIZON_S, REFRESH_LEAD_S, SERVE_AHEAD_S, WINDOW_S,
};

const TLES: &str = include_str!("fixtures/weather-2026-10-07.tle");
const REPORT_JSON: &str = include_str!("fixtures/passes-report.json");
const START: i64 = 1_791_385_200; // 2026-10-07T15:00:00Z

fn catalog() -> Catalog {
    let mut catalog = Catalog::new();
    assert_eq!(catalog.update(TLES, START), SATELLITES.len());
    catalog
}

fn predict(catalog: &Catalog, start: i64, config: Config) -> Forecast {
    let station = GroundStation::new(config.observer).unwrap();
    let mut job: Job = Job::new(catalog, &station, config, start).unwrap();
    let mut steps = 0;
    while !job.step() {
        steps += 1;
        assert!(steps < 20_000, "bounded incremental search");
    }
    job.finish()
}

/// Passes a dashboard window starting at `start` would list.
fn window(forecast: &Forecast, start: i64) -> Vec<Pass> {
    forecast
        .passes
        .iter()
        .copied()
        .filter(|pass| pass.los_unix_s > start && pass.aos_unix_s < start + WINDOW_S)
        .collect()
}

fn index_of(norad_id: u64) -> usize {
    SATELLITES
        .iter()
        .position(|sat| sat.norad_id == norad_id)
        .unwrap()
}

/// Recompute a TLE line's final checksum digit after editing its fields.
fn with_checksum(line: &str) -> String {
    let sum: u32 = line[..68]
        .bytes()
        .map(|byte| match byte {
            b'0'..=b'9' => u32::from(byte - b'0'),
            b'-' => 1,
            _ => 0,
        })
        .sum();
    format!("{}{}", &line[..68], sum % 10)
}

fn fixture_lines(norad_id: u64) -> (String, String) {
    let lines: Vec<_> = TLES.lines().collect();
    let first = lines
        .iter()
        .position(|line| line.starts_with(&format!("1 {norad_id}")))
        .unwrap();
    (lines[first].to_string(), lines[first + 1].to_string())
}

#[test]
fn independent_satellite_js_topocentric_reference() {
    // satellite.js 6.0.1 (WGS72 SGP4 + WGS84 eciToEcf/ecfToLookAngles).
    let reference = [
        (32958, 342.25672168, -7.44688305),
        (37214, 277.76385436, -35.45401306),
        (38771, 320.55413717, -7.46160442),
        (39260, 166.02399587, -22.24599137),
        (43689, 63.91984032, -27.34411434),
        (57166, 261.71243920, -44.42103402),
        (59051, 0.16483677, -63.46937253),
    ];
    let station = GroundStation::new(Observer::CLEVEDON).unwrap();
    for (id, azimuth, elevation) in reference {
        let catalog = catalog();
        let orbit = catalog
            .orbits()
            .find(|orbit| orbit.satellite.norad_id == id)
            .unwrap();
        let angle = orbit.look_at(&station, START as f64).unwrap();
        assert!(
            (angle.azimuth_deg - azimuth).abs() < 0.001,
            "{id}: {angle:?}"
        );
        assert!(
            (angle.elevation_deg - elevation).abs() < 0.001,
            "{id}: {angle:?}"
        );
    }
}

#[test]
fn six_hour_windows_match_independent_one_second_reference() {
    // Independent satellite.js scan, first second at/above 5° and below 5°.
    // One-second quantization permits a 1 s difference from refined crossings.
    let expected = [
        (39260, 1791385771, 1791386548, 70.7520),
        (59051, 1791388788, 1791389340, 14.1805),
        (39260, 1791391996, 1791392504, 12.1114),
        (37214, 1791395402, 1791395907, 12.2987),
        (38771, 1791396286, 1791396761, 11.6582),
        (57166, 1791400829, 1791401517, 29.2957),
        (37214, 1791401229, 1791402004, 51.4116),
        (38771, 1791402087, 1791402838, 51.3351),
        (43689, 1791403405, 1791404142, 42.4308),
        (57166, 1791406775, 1791407533, 53.9498),
    ];
    let forecast = predict(
        &catalog(),
        START,
        Config {
            include_experimental: true,
            ..Config::DEFAULT
        },
    );
    assert_eq!(forecast.end_unix_s - forecast.start_unix_s, HORIZON_S);
    assert_eq!(forecast.outcomes, [Outcome::Predicted; SATELLITES.len()]);
    let passes = window(&forecast, START);
    assert_eq!(passes.len(), expected.len());
    for (pass, (id, aos, los, peak)) in passes.iter().zip(expected) {
        assert_eq!(pass.satellite.norad_id, id);
        assert!(pass.aos_unix_s.abs_diff(aos) <= 1, "{pass:?}");
        assert!(pass.los_unix_s.abs_diff(los) <= 1, "{pass:?}");
        assert!((pass.peak_elevation_deg - peak).abs() < 0.01, "{pass:?}");
    }
    assert!(
        passes.last().unwrap().los_unix_s > START + WINDOW_S,
        "LOS of a pass straddling the window boundary must not be clipped"
    );
    assert!(
        forecast
            .passes
            .windows(2)
            .all(|pair| pair[0].aos_unix_s <= pair[1].aos_unix_s),
        "assembled passes are sorted by acquisition"
    );
    let ongoing = predict(&catalog(), 1_791_403_800, Config::DEFAULT);
    assert!(ongoing
        .passes
        .iter()
        .any(|pass| pass.aos_unix_s < 1_791_403_800 && pass.los_unix_s > 1_791_403_800));
}

#[test]
fn default_catalog_filters_legacy_services_and_configuration_changes_results() {
    let catalog = catalog();
    let normal = predict(&catalog, START, Config::DEFAULT);
    assert!(normal
        .passes
        .iter()
        .all(|pass| !pass.satellite.experimental));
    assert_eq!(window(&normal, START).len(), 6);
    assert_eq!(normal.outcomes[index_of(32958)], Outcome::Skipped);
    let high = predict(
        &catalog,
        START,
        Config {
            min_peak_elevation_deg: 40.0,
            ..Config::DEFAULT
        },
    );
    assert_eq!(window(&high, START).len(), 3);
    let moved = predict(
        &catalog,
        START,
        Config {
            observer: Observer {
                latitude_deg: -33.86,
                longitude_deg: 151.2,
                altitude_m: 30.0,
            },
            ..Config::DEFAULT
        },
    );
    assert_ne!(normal.passes, moved.passes);
    let stale = predict(&catalog, START + MAX_TLE_AGE_S + 86_400, Config::DEFAULT);
    assert!(stale.passes.is_empty());
    assert_eq!(stale.outcomes, [Outcome::Skipped; SATELLITES.len()]);
}

#[test]
fn parser_handles_partial_crlf_and_bad_data_without_losing_good_elements() {
    let mut catalog = catalog();
    let before: Vec<_> = catalog.orbits().map(|orbit| orbit.epoch_unix_s).collect();
    assert_eq!(catalog.update("<html>rate limited</html>", START), 0);
    assert_eq!(
        catalog.update(&TLES.replace("1 38771", "1 99999"), START),
        6
    );
    assert_eq!(catalog.update(&TLES.replace('\n', "\r\n"), START), 7);
    assert_eq!(
        before,
        catalog
            .orbits()
            .map(|orbit| orbit.epoch_unix_s)
            .collect::<Vec<_>>()
    );
    let lines: Vec<_> = TLES.lines().collect();
    assert!(
        Orbit::from_tle(&SATELLITES[0], lines[1].as_bytes(), lines[2].as_bytes()).is_none(),
        "wrong satellite"
    );
    let mut corrupt = lines[1].as_bytes().to_vec();
    corrupt[68] = if corrupt[68] == b'0' { b'1' } else { b'0' };
    assert!(Orbit::from_tle(&SATELLITES[4], &corrupt, lines[2].as_bytes()).is_none());
    let mut empty = Catalog::new();
    assert_eq!(empty.update(TLES, START - 3 * 86_400), 0, "future epochs");
    assert_eq!(empty.update(TLES, START + 9 * 86_400), 0, "stale epochs");
    assert_eq!(empty.update(lines[1], START), 0, "truncated pair");
}

#[test]
fn every_non_digit_checksum_column_is_rejected_without_panicking() {
    // sgp4 2.4 computes `line[68] - b'0'` as u8; with overflow checks a byte
    // below '0' would panic inside the library, so it must never reach it.
    let (line1, line2) = fixture_lines(38771);
    for byte in 0..=u8::MAX {
        for line in [1, 2] {
            let mut first = line1.as_bytes().to_vec();
            let mut second = line2.as_bytes().to_vec();
            let target = if line == 1 { &mut first } else { &mut second };
            let valid = target[68];
            target[68] = byte;
            let orbit = Orbit::from_tle(&SATELLITES[0], &first, &second);
            assert_eq!(orbit.is_some(), byte == valid, "line {line} byte {byte}");
        }
    }
    // The same lines arriving through a download stage nothing either.
    let mut catalog = Catalog::new();
    let text = format!("METOP-B\n{}-\n{line2}\n", &line1[..68]);
    assert_eq!(catalog.update(&text, START), 0);
}

#[test]
fn non_numeric_or_impossible_elements_are_rejected_even_with_valid_checksums() {
    let (line1, line2) = fixture_lines(38771);
    // Rust float parsing accepts these words, and checksums ignore letters.
    for (start, field) in [
        (8, "     nan"),
        (8, "     inf"),
        (17, "     NaN"),
        (34, "infinity"),
        (43, "    -inf"),
        (52, "        nan"),
    ] {
        let edited = with_checksum(&format!(
            "{}{field}{}",
            &line2[..start],
            &line2[start + field.len()..]
        ));
        assert!(
            Orbit::from_tle(&SATELLITES[0], line1.as_bytes(), edited.as_bytes()).is_none(),
            "{edited}"
        );
        // A newer epoch is no excuse: a bad set never displaces a good one.
        let mut catalog = catalog();
        let good = catalog.orbit(0).unwrap().epoch_unix_s;
        let newer = with_checksum(&line1.replacen("26280.", "26281.", 1));
        assert_eq!(
            catalog.update(&format!("{newer}\n{edited}\n"), START + 86_400),
            0
        );
        assert_eq!(catalog.orbit(0).unwrap().epoch_unix_s, good);
    }
    for (start, field) in [(8, "190.0000"), (17, "400.0000"), (34, "361.0000")] {
        let edited = with_checksum(&format!(
            "{}{field}{}",
            &line2[..start],
            &line2[start + field.len()..]
        ));
        assert!(Orbit::from_tle(&SATELLITES[0], line1.as_bytes(), edited.as_bytes()).is_none());
    }
    // Non-ASCII bytes never reach the library's unchecked UTF-8 conversion.
    let mut wide = line2.as_bytes().to_vec();
    wide.splice(8..10, "é".bytes());
    assert!(Orbit::from_tle(&SATELLITES[0], line1.as_bytes(), &wide).is_none());
    assert!(Orbit::from_tle(&SATELLITES[0], line1.as_bytes(), line2.as_bytes()).is_some());
}

#[test]
fn streamed_updates_match_a_whole_body_at_every_split() {
    let reference: Vec<_> = catalog().epochs().to_vec();
    for chunk in [1, 2, 7, 68, 69, 70, 512] {
        let mut update = Update::new(START);
        for piece in TLES.replace('\n', "\r\n").as_bytes().chunks(chunk) {
            update.push(piece);
        }
        let mut streamed = Catalog::new();
        assert_eq!(streamed.commit(update), SATELLITES.len(), "chunk {chunk}");
        assert_eq!(streamed.epochs().to_vec(), reference, "chunk {chunk}");
    }
    // A final line without a newline, and an overlong line between sets.
    let (line1, line2) = fixture_lines(38771);
    let mut update = Update::new(START);
    update.push(format!("{}\n{line1}\n{line2}", "x".repeat(500)).as_bytes());
    assert_eq!(Catalog::new().commit(update), 1);
    // An overlong line between a pair breaks it.
    let mut update = Update::new(START);
    update.push(format!("{line1}\n{}\n{line2}\n", "2".repeat(500)).as_bytes());
    assert_eq!(Catalog::new().commit(update), 0);
}

#[test]
fn staged_elements_never_replace_newer_catalog_entries() {
    let mut catalog = catalog();
    let (line1, line2) = fixture_lines(38771);
    let older = with_checksum(&line1.replacen("26280.", "26279.", 1));
    assert_eq!(catalog.update(&format!("{older}\n{line2}\n"), START), 0);
    let newer = with_checksum(&line1.replacen("26280.", "26281.", 1));
    assert_eq!(
        catalog.update(&format!("{newer}\n{line2}\n"), START + 86_400),
        1
    );
    let epochs = catalog.epochs();
    assert!(epochs[0].unwrap() > START as f64);
    assert!(epochs.iter().all(Option::is_some));
}

fn synthetic(start: i64, peak_at: f64, maximum: f64, width: f64, config: Config) -> Vec<Pass> {
    let mut search = Search::new(
        start,
        start + WINDOW_S,
        config,
        &SATELLITES[0],
        |time: f64| {
            Some(LookAngle {
                azimuth_deg: 180.0,
                elevation_deg: maximum - ((time - peak_at) / width).powi(2),
            })
        },
    )
    .unwrap();
    let mut result = Vec::new();
    while !search.is_finished() {
        if let Some(pass) = search.step().unwrap() {
            result.push(pass);
        }
    }
    result
}

#[test]
fn finds_grazing_passes_between_samples_and_preserves_window_edges() {
    let config = Config {
        min_elevation_deg: 5.0,
        min_peak_elevation_deg: 5.0,
        ..Config::DEFAULT
    };
    // A 20-second pass entirely between samples at 60 and 90 seconds.
    let grazing = synthetic(0, 75.0, 6.0, 10.0, config);
    assert_eq!(grazing.len(), 1);
    assert_eq!((grazing[0].aos_unix_s, grazing[0].los_unix_s), (65, 85));
    assert!(synthetic(0, 75.0, 4.999, 10.0, config).is_empty());
    let ongoing = synthetic(1000, 950.0, 6.0, 100.0, config);
    assert_eq!((ongoing[0].aos_unix_s, ongoing[0].los_unix_s), (850, 1050));
    let boundary = synthetic(0, WINDOW_S as f64, 6.0, 100.0, config);
    assert_eq!(boundary[0].los_unix_s, WINDOW_S + 100);
    assert!(synthetic(0, (WINDOW_S + 500) as f64, 6.0, 100.0, config).is_empty());
}

#[test]
fn each_search_step_performs_at_most_two_propagations_even_during_refinement() {
    let calls = std::cell::Cell::new(0);
    let mut search = Search::new(
        1000,
        1000 + WINDOW_S,
        Config::DEFAULT,
        &SATELLITES[0],
        |time: f64| {
            calls.set(calls.get() + 1);
            Some(LookAngle {
                azimuth_deg: 180.0,
                elevation_deg: 40.0 - ((time - 1300.0) / 100.0).powi(2),
            })
        },
    )
    .unwrap();
    let mut found = 0;
    while !search.is_finished() {
        calls.set(0);
        if search.step().unwrap().is_some() {
            found += 1;
        }
        assert!(calls.get() <= 2);
    }
    assert_eq!(found, 1);
}

#[test]
fn invalid_configuration_and_failed_propagation_are_explicit_errors() {
    for (observer, error) in [
        (
            Observer {
                latitude_deg: f64::NAN,
                ..Observer::CLEVEDON
            },
            ObserverError::Latitude,
        ),
        (
            Observer {
                longitude_deg: 181.0,
                ..Observer::CLEVEDON
            },
            ObserverError::Longitude,
        ),
        (
            Observer {
                altitude_m: f64::INFINITY,
                ..Observer::CLEVEDON
            },
            ObserverError::Altitude,
        ),
    ] {
        assert!(GroundStation::new(observer).is_none());
        let config = Config {
            observer,
            ..Config::DEFAULT
        };
        assert_eq!(config.validate(), Err(ConfigError::Observer(error)));
    }
    assert_eq!(
        Config {
            min_elevation_deg: -1.0,
            ..Config::DEFAULT
        }
        .validate(),
        Err(ConfigError::MinElevation)
    );
    assert_eq!(
        Config {
            min_elevation_deg: 20.0,
            ..Config::DEFAULT
        }
        .validate(),
        Err(ConfigError::MinPeakElevation)
    );
    let mut failed = Search::new(
        START,
        START + WINDOW_S,
        Config::DEFAULT,
        &SATELLITES[0],
        |_| None,
    )
    .unwrap();
    assert_eq!(failed.step(), Err(Error::Propagation));
    assert!(failed.is_finished());
    for end in [START, START - 1, START + HORIZON_S + 1] {
        let search = Search::new(START, end, Config::DEFAULT, &SATELLITES[0], |_| None);
        assert!(search.is_err());
    }
}

#[test]
fn a_mask_above_the_default_peak_raises_the_default_peak() {
    // Configuring only the mask must never produce an invalid configuration.
    for mask in [0.0, 5.0, 10.0, 15.0, 89.0] {
        let config = Config {
            min_elevation_deg: mask,
            min_peak_elevation_deg: Config::default_min_peak(mask),
            ..Config::DEFAULT
        };
        assert_eq!(config.validate(), Ok(()), "mask {mask}");
        assert!(config.min_peak_elevation_deg >= 10.0);
    }
    assert_eq!(Config::default_min_peak(15.0), 15.0);
    assert_eq!(
        Config::default_min_peak(5.0),
        Config::DEFAULT.min_peak_elevation_deg
    );
    // Validation is usable in constant evaluation (the firmware relies on it).
    const VALID: bool = Config::DEFAULT.is_valid();
    const _: () = assert!(VALID);
}

#[test]
fn propagation_failure_and_quota_overflow_discard_only_that_candidate() {
    // Extreme drag (B* columns 53–60) decays METOP-B within the span, so
    // propagation fails partway through its search.
    let (line1, line2) = fixture_lines(38771);
    let decaying = with_checksum(&format!("{}  99999+3{}", &line1[..52], &line1[61..]));
    let mut catalog = catalog();
    assert_eq!(catalog.update(&format!("{decaying}\n{line2}\n"), START), 1);
    let forecast = predict(&catalog, START, Config::DEFAULT);
    assert_eq!(forecast.outcomes[0], Outcome::Failed);
    assert!(forecast
        .passes
        .iter()
        .all(|pass| pass.satellite.norad_id != 38771));
    assert_eq!(forecast.outcomes[index_of(43689)], Outcome::Predicted);

    // A quota of one pass discards every candidate with two or more.
    let station = GroundStation::new(Observer::CLEVEDON).unwrap();
    let catalog = self::catalog();
    let mut job = Job::<1>::new(&catalog, &station, Config::DEFAULT, START).unwrap();
    while !job.step() {}
    let small = job.finish();
    let full = predict(&catalog, START, Config::DEFAULT);
    for (index, outcome) in small.outcomes.iter().enumerate() {
        let count = full
            .passes
            .iter()
            .filter(|pass| pass.satellite == &SATELLITES[index])
            .count();
        let expected = match full.outcomes[index] {
            Outcome::Predicted if count > 1 => Outcome::Failed,
            other => other,
        };
        assert_eq!(*outcome, expected, "{}", SATELLITES[index].name);
    }
    assert!(small.passes.len() <= SATELLITES.len());
}

#[test]
fn coverage_rather_than_age_decides_when_to_recompute() {
    assert!(needs_forecast(None, START));
    let forecast = predict(&catalog(), START, Config::DEFAULT);
    let valid_until = forecast.valid_until_unix_s();
    assert_eq!(valid_until, START + HORIZON_S - WINDOW_S - SERVE_AHEAD_S);
    assert!(forecast.covers(START) && forecast.covers(valid_until));
    assert!(!forecast.covers(START - 1) && !forecast.covers(valid_until + 1));
    assert!(!needs_forecast(Some(&forecast), START));
    assert!(!needs_forecast(
        Some(&forecast),
        valid_until - REFRESH_LEAD_S
    ));
    assert!(needs_forecast(
        Some(&forecast),
        valid_until - REFRESH_LEAD_S + 1
    ));
    assert!(
        needs_forecast(Some(&forecast), START - 1),
        "clock stepped back"
    );
}

fn elements(catalog: &Catalog) -> Elements {
    Elements {
        epochs: catalog.epochs(),
        updated_unix_s: Some(START - 600),
        download_failed: false,
    }
}

#[test]
fn reports_serve_the_whole_window_for_as_long_as_coverage_lasts() {
    let catalog = catalog();
    let config = Config {
        min_peak_elevation_deg: 20.0,
        ..Config::DEFAULT
    };
    let forecast = predict(&catalog, START, config);
    // A pass acquired after the first window ends must appear once `now`
    // advances far enough, without recomputing.
    let late = forecast
        .passes
        .iter()
        .find(|pass| pass.aos_unix_s > START + WINDOW_S + SERVE_AHEAD_S)
        .copied()
        .unwrap();
    let now = late.aos_unix_s - WINDOW_S - SERVE_AHEAD_S + 1;
    assert!(forecast.covers(now));
    let report = Report::new(
        config,
        Some(&forecast),
        elements(&catalog),
        Some(now),
        Some(0),
    );
    assert_eq!(report.status(), Status::Ready);
    assert!(report.passes().contains(&late));
    assert!(report.offers(ezal_core::tracking::PassEvent::from_pass(late)));
    assert!(!report.offers(ezal_core::tracking::PassEvent {
        aos_unix_s: late.aos_unix_s + 1,
        ..ezal_core::tracking::PassEvent::from_pass(late)
    }));
    let expected: Vec<_> = forecast
        .passes
        .iter()
        .copied()
        .filter(|pass| pass.los_unix_s > now && pass.aos_unix_s < now + WINDOW_S + SERVE_AHEAD_S)
        .collect();
    assert_eq!(report.passes(), expected.as_slice());
    let json = report.to_string();
    assert!(json.contains(&format!("\"valid_until_unix_s\":{}", now + SERVE_AHEAD_S)));
    assert_eq!(report.json_len(), json.len());
    // Beyond coverage nothing is served as current.
    let after = forecast.valid_until_unix_s() + 1;
    let report = Report::new(
        config,
        Some(&forecast),
        elements(&catalog),
        Some(after),
        Some(0),
    );
    assert_eq!(report.status(), Status::Predicting);
    assert!(!report.offers(ezal_core::tracking::PassEvent::from_pass(late)));
    assert!(report.passes().is_empty());
    assert!(report.to_string().contains("\"valid_until_unix_s\":null"));
}

#[test]
fn availability_reflects_elements_even_while_a_forecast_is_calculated() {
    let catalog = catalog();
    let config = Config::DEFAULT;
    // Elements downloaded, first forecast still running.
    let calculating = Report::new(config, None, elements(&catalog), Some(START), Some(0));
    assert_eq!(calculating.status(), Status::Predicting);
    let json = calculating.to_string();
    assert!(json.contains("\"status\":\"predicting\""));
    assert!(json.contains("{\"name\":\"METOP-B\",\"available\":true,\"failed\":false"));
    assert!(!json.contains("\"available\":false"));
    // A forecast computed before those elements arrived is not current.
    let empty = predict(&Catalog::new(), START, config);
    let report = Report::new(
        config,
        Some(&empty),
        elements(&catalog),
        Some(START),
        Some(0),
    );
    assert_eq!(report.status(), Status::Predicting);
    // Precedence: no time, then no elements.
    let report = Report::new(config, Some(&empty), elements(&catalog), None, None);
    assert_eq!(report.status(), Status::WaitingForTime);
    let report = Report::new(config, Some(&empty), Elements::EMPTY, Some(START), Some(0));
    assert_eq!(report.status(), Status::WaitingForTles);
    let stale = START + MAX_TLE_AGE_S + 86_400;
    let report = Report::new(config, None, elements(&catalog), Some(stale), Some(0));
    assert_eq!(report.status(), Status::WaitingForTles);
    assert!(report.to_string().contains("\"available\":false"));
}

#[test]
fn failed_candidates_are_reported_and_full_lists_keep_the_earliest_passes() {
    let catalog = catalog();
    let config = Config::DEFAULT;
    let mut forecast = predict(&catalog, START, config);
    forecast.outcomes[0] = Outcome::Failed;
    forecast
        .passes
        .retain(|pass| pass.satellite.norad_id != 38771);
    let report = Report::new(
        config,
        Some(&forecast),
        elements(&catalog),
        Some(START),
        Some(0),
    );
    assert_eq!(report.status(), Status::Ready);
    let json = report.to_string();
    assert!(json.contains("{\"name\":\"METOP-B\",\"available\":true,\"failed\":true"));
    assert!(json.contains("\"truncated\":false"));

    // Flood the window with copies of the first pass, then one earlier pass.
    let first = forecast.passes[0];
    forecast.passes.clear();
    let early = Pass {
        aos_unix_s: first.aos_unix_s - 60,
        ..first
    };
    forecast.passes.push(early).unwrap();
    while forecast.passes.push(first).is_ok() {}
    let report = Report::new(
        config,
        Some(&forecast),
        elements(&catalog),
        Some(START),
        Some(0),
    );
    assert!(report.to_string().contains("\"truncated\":true"));
    assert_eq!(
        report.passes()[0],
        early,
        "truncation keeps the earliest rows"
    );
}

#[test]
fn report_json_matches_the_dashboard_contract_fixture() {
    // `fixtures/passes-report.json` is also the dashboard tests' server reply,
    // so the client is exercised against the exact format served here.
    let catalog = catalog();
    let forecast = predict(&catalog, START, Config::DEFAULT);
    let report = Report::new(
        Config::DEFAULT,
        Some(&forecast),
        elements(&catalog),
        Some(START),
        Some(60_000),
    );
    let json = report.to_string();
    // Set EZAL_UPDATE_FIXTURES=1 to regenerate after an intended change.
    if std::env::var_os("EZAL_UPDATE_FIXTURES").is_some() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/passes-report.json"
        );
        std::fs::write(path, format!("{json}\n")).unwrap();
        return;
    }
    assert_eq!(json, REPORT_JSON.trim_end());
}
