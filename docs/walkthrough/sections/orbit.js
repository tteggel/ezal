(() => {
  'use strict';
  if (!document.getElementById('orbit')) return;

  // ── Model ────────────────────────────────────────────────────────────────
  // Everything from `sidereal` down mirrors crates/ezal-core/src/orbit.rs line
  // for line. `propagate` does not: ezal calls the sgp4 crate, while this page
  // moves the satellite round a Kepler ellipse whose orientation drifts at the
  // steady rates Earth's bulge (J2) causes.
  const DEG = Math.PI / 180;
  const TAU = 2 * Math.PI;
  const { sin, cos, sqrt, atan2, floor, abs, round, min, max, PI } = Math;

  // METOP-B from crates/ezal-core/tests/fixtures/weather-2026-10-07.tle.
  const LINE1 = '1 38771U 12049A   26280.32152257  .00000049  00000+0  42027-4 0  9998';
  const LINE2 = '2 38771  98.6378 329.5145 0002825  45.3235 314.8172 14.21463054729191';

  // Read the fields this page uses from their fixed columns.
  const parseTle = (line1, line2) => {
    const year = 2000 + Number(line1.slice(18, 20));
    const day = Number(line1.slice(20, 32));
    return {
      // ezal keeps the epoch to the millisecond (Orbit::from_tle).
      epochUnix: floor((Date.UTC(year, 0, 1) / 1000 + (day - 1) * 86400) * 1000) / 1000,
      inclination: Number(line2.slice(8, 16)),
      raan: Number(line2.slice(17, 25)),
      eccentricity: Number('0.' + line2.slice(26, 33)),
      argPerigee: Number(line2.slice(34, 42)),
      meanAnomaly: Number(line2.slice(43, 51)),
      meanMotion: Number(line2.slice(52, 63)),
    };
  };

  // The checksum rule from `well_formed`: digits add their value, '-' adds 1.
  const checksum = (line) => {
    let sum = 0;
    for (const ch of line.slice(0, 68)) sum += ch === '-' ? 1 : ch >= '0' && ch <= '9' ? Number(ch) : 0;
    return sum % 10;
  };

  // SGP4's gravity constants (WGS72), in Earth radii and minutes.
  const RE_KM = 6378.135;
  const KE = 0.07436691613317342;
  const J2 = 0.001082616;

  // Turn the record's numbers into an ellipse and its three steady drift rates.
  const orbitFrom = (el) => {
    const i = el.inclination * DEG;
    const e = el.eccentricity;
    const n0 = el.meanMotion * TAU / 1440; // radians per minute
    const cosI = cos(i);
    const beta2 = 1 - e * e;
    const beta = sqrt(beta2);
    // The record's laps-per-day already includes the bulge's average effect;
    // undo that to get the ellipse's size, exactly as SGP4 starts.
    const a1 = Math.pow(KE / n0, 2 / 3);
    const k = 0.75 * J2 * (3 * cosI * cosI - 1) / (beta * beta2);
    const d1 = k / (a1 * a1);
    const a0 = a1 * (1 - d1 / 3 - d1 * d1 - 134 / 81 * d1 * d1 * d1);
    const d0 = k / (a0 * a0);
    const n = n0 / (1 + d0);
    const a = a0 / (1 - d0);
    const p2 = a * a * beta2 * beta2;
    return {
      epochUnix: el.epochUnix,
      i, e, a, n,
      raan0: el.raan * DEG,
      argp0: el.argPerigee * DEG,
      m0: el.meanAnomaly * DEG,
      // Radians per minute: the ring swings, its low point creeps, laps stretch.
      raanRate: -1.5 * J2 * n * cosI / p2,
      argpRate: 0.75 * J2 * n * (5 * cosI * cosI - 1) / p2,
      mRate: n * (1 + 0.75 * J2 * (3 * cosI * cosI - 1) / (a * a * beta * beta2)),
    };
  };

  // Position in kilometres from Earth's centre, in axes fixed to the stars.
  const propagate = (orbit, minutes) => {
    const raan = orbit.raan0 + orbit.raanRate * minutes;
    const argp = orbit.argp0 + orbit.argpRate * minutes;
    const m = orbit.m0 + orbit.mRate * minutes;
    let E = m;
    for (let k = 0; k < 8; k++) E -= (E - orbit.e * sin(E) - m) / (1 - orbit.e * cos(E));
    const nu = atan2(sqrt(1 - orbit.e * orbit.e) * sin(E), cos(E) - orbit.e);
    const r = orbit.a * (1 - orbit.e * cos(E)) * RE_KM;
    const u = argp + nu;
    const cu = cos(u), su = sin(u), cr = cos(raan), sr = sin(raan), ci = cos(orbit.i);
    return [r * (cu * cr - su * sr * ci), r * (cu * sr + su * cr * ci), r * su * sin(orbit.i)];
  };

  // sgp4::afspc_epoch_to_sidereal_time: how far Greenwich has turned, radians.
  const sidereal = (unixS) => {
    const epoch = (unixS - 946728000) / 31557600; // years since 2000-01-01 12:00
    const d1970 = (epoch + 30) * 365.25 + 1;
    const whole = floor(d1970 + 1e-8);
    const theta = 1.7321343856509374 + 1.72027916940703639e-2 * whole
      + (1.72027916940703639e-2 + TAU) * (d1970 - whole)
      + d1970 * d1970 * 5.07551419432269442e-15;
    return ((theta % TAU) + TAU) % TAU;
  };

  // WGS84 squared eccentricity, as written in GroundStation::new.
  const E2 = 6.69437999014e-3;

  // GroundStation::new. `e2` is a parameter only so one figure can exaggerate it.
  const groundStation = (latitudeDeg, longitudeDeg, altitudeM, e2 = E2) => {
    const lat = latitudeDeg * (PI / 180);
    const lon = longitudeDeg * (PI / 180);
    const sinLat = sin(lat), cosLat = cos(lat);
    const sinLon = sin(lon), cosLon = cos(lon);
    const n = 6378.137 / sqrt(1 - e2 * sinLat * sinLat);
    const height = altitudeM / 1000;
    return {
      position: [
        (n + height) * cosLat * cosLon,
        (n + height) * cosLat * sinLon,
        (n * (1 - e2) + height) * sinLat,
      ],
      sinLat, cosLat, sinLon, cosLon, n,
    };
  };

  // GroundStation::look_at, keeping the intermediate values the figures draw.
  const lookAt = (station, temeKm, unixS) => {
    const theta = sidereal(unixS);
    const s = sin(theta), c = cos(theta);
    const x = c * temeKm[0] + s * temeKm[1] - station.position[0];
    const y = -s * temeKm[0] + c * temeKm[1] - station.position[1];
    const z = temeKm[2] - station.position[2];
    const east = -station.sinLon * x + station.cosLon * y;
    const north = -station.sinLat * station.cosLon * x - station.sinLat * station.sinLon * y + station.cosLat * z;
    const up = station.cosLat * station.cosLon * x + station.cosLat * station.sinLon * y + station.sinLat * z;
    const azimuth = atan2(east, north) * (180 / PI);
    return {
      theta, arrow: [x, y, z], east, north, up,
      azimuth: azimuth < 0 ? azimuth + 360 : azimuth,
      elevation: atan2(up, sqrt(east * east + north * north)) * (180 / PI),
    };
  };

  // Observer::CLEVEDON and passes::Config::DEFAULT.
  const CLEVEDON = { latitude: 51.44266, longitude: -2.84053, altitude: 0 };
  const MASK_DEG = 5;
  const MIN_PEAK_DEG = 10;

  // The search in passes.rs, run to completion instead of in bounded steps:
  // sample every 30 s; where a sample beats both neighbours, narrow the peak
  // by thirds; then walk out a minute at a time and halve to find each edge.
  const findPasses = (elevationAt, start, end) => {
    const STEP = 30, MARGIN = 1800, found = [];
    let left = null, centre = null;
    for (let t = start - MARGIN; t <= end + MARGIN; t += STEP) {
      const e = elevationAt(t);
      if (left !== null && centre > left && centre >= e) {
        let low = t - 2 * STEP, high = t;
        while (high - low > 0.25) {
          const a = low + (high - low) / 3, b = high - (high - low) / 3;
          if (elevationAt(a) < elevationAt(b)) low = a; else high = b;
        }
        const peakTime = (low + high) / 2, peak = elevationAt(peakTime);
        if (peak >= MIN_PEAK_DEG && peak > MASK_DEG) {
          const edge = (direction) => {
            let inside = peakTime, outside = peakTime + direction * 60;
            while (elevationAt(outside) >= MASK_DEG) { inside = outside; outside += direction * 60; }
            while (abs(inside - outside) > 0.25) {
              const mid = (inside + outside) / 2;
              if (elevationAt(mid) < MASK_DEG) outside = mid; else inside = mid;
            }
            return (inside + outside) / 2;
          };
          const aos = edge(-1), los = edge(1);
          if (los > start && aos < end && round(los) > round(aos)) {
            found.push({ aos: round(aos), los: round(los), peakTime, peak });
          }
        }
      }
      left = centre;
      centre = e;
    }
    return found;
  };
  // ── End of model ─────────────────────────────────────────────────────────

  // The orbit, the station and the passes every figure shares.
  const RECORD = parseTle(LINE1, LINE2);
  const ORBIT = orbitFrom(RECORD);
  const STATION = groundStation(CLEVEDON.latitude, CLEVEDON.longitude, CLEVEDON.altitude);
  const NOON = Date.UTC(2026, 9, 7, 12) / 1000;
  const HORIZON_S = 12 * 3600; // passes::HORIZON_S
  const minutesAt = (unixS) => (unixS - RECORD.epochUnix) / 60; // as Orbit::look_at
  const lookFrom = (unixS) => lookAt(STATION, propagate(ORBIT, minutesAt(unixS)), unixS);
  const PASSES = findPasses((t) => lookFrom(t).elevation, NOON, NOON + HORIZON_S);
  const PASS = PASSES.reduce((best, pass) => (pass.peak > best.peak ? pass : best));

  // ── Drawing ──────────────────────────────────────────────────────────────
  // One small renderer: a camera that looks at the scene from far away (no
  // perspective), a globe that hides what is behind it, and lines that turn
  // faint where the globe is in the way.
  const C = {
    ink: '#20323e', navy: '#18354a', az: '#245bce', el: '#087e83', sat: '#9b3fb5', warn: '#925100',
    muted: '#506574', sea: '#eaf2f9', land: '#c6d7e4', coast: '#9db5c7', grid: 'rgba(24,53,74,.13)',
  };
  const FONT = "'Trebuchet MS','Segoe UI',sans-serif";
  const R_EARTH = 6378.137;
  const F_REAL = 1 / 298.257223563;
  const IDENTITY = [[1, 0, 0], [0, 1, 0], [0, 0, 1]];

  const add = (a, b) => [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
  const sub = (a, b) => [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
  const mul = (a, k) => [a[0] * k, a[1] * k, a[2] * k];
  const dot = (a, b) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
  const cross = (a, b) => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
  const unit = (a) => mul(a, 1 / Math.hypot(a[0], a[1], a[2]));
  const mix = (a, b, t) => [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
  const rotZ = (p, angle) => [cos(angle) * p[0] - sin(angle) * p[1], sin(angle) * p[0] + cos(angle) * p[1], p[2]];
  const zTurn = (angle) => [[cos(angle), -sin(angle), 0], [sin(angle), cos(angle), 0], [0, 0, 1]];
  const apply = (m, p) => [dot(m[0], p), dot(m[1], p), dot(m[2], p)];
  const applyT = (m, p) => [
    m[0][0] * p[0] + m[1][0] * p[1] + m[2][0] * p[2],
    m[0][1] * p[0] + m[1][1] * p[1] + m[2][1] * p[2],
    m[0][2] * p[0] + m[1][2] * p[1] + m[2][2] * p[2],
  ];
  const clamp = (v, low, high) => min(high, max(low, v));
  const sweep = (steps, at) => Array.from({ length: steps + 1 }, (_, k) => at(k / steps));
  const seg = (from, to, steps = 24) => sweep(steps, (t) => mix(from, to, t));
  const flat = (radius, angle) => [radius * cos(angle), radius * sin(angle), 0];

  // The whole ring at one moment, for drawing.
  const ringAt = (orbit, minutes) => {
    const raan = orbit.raan0 + orbit.raanRate * minutes, argp = orbit.argp0 + orbit.argpRate * minutes;
    const cr = cos(raan), sr = sin(raan), ci = cos(orbit.i), si = sin(orbit.i);
    return sweep(180, (t) => {
      const nu = t * TAU, u = argp + nu;
      const r = orbit.a * (1 - orbit.e * orbit.e) / (1 + orbit.e * cos(nu)) * RE_KM;
      return [r * (cos(u) * cr - sin(u) * sr * ci), r * (cos(u) * sr + sin(u) * cr * ci), r * sin(u) * si];
    });
  };

  // Natural Earth 110 m land (public domain) in 0.1° steps. Rings are split
  // by spaces. Each starts with four characters (longitude then latitude,
  // base 85) and continues with two per vertex (each step, offset by 42).
  // Rings keep land on their left, seen from outside the globe.
  const LAND = '4)#9EJ:R:SfLjRiQX] %K#AAX?TLNaKaOgS 5d#M=P9BIO;OPNaNlR|VtXLcEW +l#|PN]ObQ[UXUFP +@#}KObPgP .G%1ETLPWN]PzSZQLWGQ 3-%ABdIQMMQFOMGRKQGNRNZOkUMNaRkUWZ RF!^AT9U4PKUPS3TMW9UCUDUFUIVMVEVWUVVTW[WgScTfSNW?SNVPULVRVSWTUVVZVXUWVaTXVaSWWUUWWWUSVKUHTGTGTFRFRGSGRGSJVGTMVFSFSIUKUITKOGRHTDSETGUGTLUTVKUGSERHRERIQERGQITNVOVPVLUQNPMIQHQGRHRGPIPISISIRIQGSISIQLOJOGQFSFUKTGRKUMUJTKPFRLOIOHQGPIRHSEXKUKUKSFKKOMOKOHSIQGRIPJTJSKQIRHSHQIRITNWNUMKKOGSHQIQHSKPLQLNJPHPKNONMNHQHQGQLQLMPOLOLOONLNGPFSJUWWWUTVFRQVZV]TSWRTJ^HRHRITHSITINJPHRJUKUFSLUJUQWJTHSISIQHPJQIOQNIPIRKOIOISJPJOJPFPIQGOIPONGOKUKUHRITNUHTLOMLJOHQHQJOGQHPGRFRGRGPKUNUGSGSGRFRGQJNNVERHPNNFPLUKWHQJTLOJOGQGOGQGQHPGOMVGRFQEPFSOVGSMNRNFRMVFRKPMMJNHOGOHQKNNN^PONIOIODOGOFPEPDQBQKQEQDPINFOFOAPDPMPQJUNgRiTgRROVNZOBO:P;Q?P?P6QDOLN=U7S7RAO;O@OBOFOMVHV?VAU8T7T@UDU@U:SFVPVFURVSVbUeQ^ULUGUCTOVdRdSfQdSZVbT_TaTcUYVVUaTXUXVSWOVQVOVQ[OVMUOVPVNVJUJTMUNVTVXVXVXU[SYTMURVSVWUYUYTZSZPVT]UVVLTJQLOKOJQKPLOHRLOLONNJOKOLNLONNSNTNPNNMNOQNRNUNUNUOUNTNOMLOIPEQDPFQGOHPEQKVHTDQHVNNJNFUFSERJVEQGTJVIKEQHTEQEPEQDRNVHUFSERERJTVJTOXN_QWOYNMNDTDSCSFPCQGUDSFVHOLUNVLUGNFQETFPHODRCRDRDRCSESDPERDTDQKRJSFPGPDOEQDRFPCPLUEPOOVNRNCQGOEOEPBQJVBQAQIUMSUGZNaOYNgQfQiQbOaPVNJNAOAOCU9P8ScOZNdPTMOONNPMoPZNuP{QPN4O6O8U:S;S3O)U9U4TCVMSHT3PPRENDVQRPRQROQNQPQPQ@VJL 33)ENWKSHNOKMMEVDYITbFkG]QZO^QZUUVXO[RWWDUDX 48)UJUFLWM[WVNbUTUJW EQ)mQTITKUPLRLdS N7*nRMXJSKXJ[QYYUOSZVUQ_LSEOHU QS*pKMPMOOMKLMBILPEHMJTO^QXO[RaY[eZUZRNWWV]_RWPVJN Qd+MKULWNRWK_ERPVKRLQJKOQN^NULMJVQSOZV]bQWUUZQUXT[MSLNLSJUPZNUQKK] Pl-(B_MVMRTOXKbHWOVT R1-UNSNOSMWQYSSXOU RF-_RWLOJPSN !%-fPQRM Pr-gROUNUS Pl-tRJTOTSUQPZ C(.+L[OMQLMKNSSNOMJLLLNRHNMRQKOMRIWGQKMKRNMQQJRKVJSITMbL[VZU_v^wS[UTSVPZTUULTUSWOVQ_ N+.#QXOWQVPSQXOWNORONGSOQPPJSLROSNJ8KOIUPUMUORF[JUJXQUY]QVURVVUWMWPQNRLPLUOQJTLVLULPZORMIPMSLONLSONPMJTLFSLXKWHORPNSROMPONMMQKMTOMVMMPN[KIQIMMOMMMGOMRORPOLQJOPSMQJNNNIONKQYNKSLQMPPQLZDRMOVMUVIMVTNWJTJVNROVKRMTLWHSLQGPPMQRLXP]LbQ]XURXWbT[PcSUVTW[U]W]QlYfS]MYRaLPPWNXKRLZNVZXUZ[SKNMQLLL[TWYTKOM]QWNVEXLrJ[YWTQMXPZN^Z[V^RXVQUTYTYVWU[YaZVV]TYRWVbS[MZR_PZJZF]P^PPNWNPN]LYSTE[GWOYSWO[OSQWP[SVMVOVKOOY P9.GLTVLYROU KI.KIXIPaKVS P,.OOTKTQNTP^Q P3.TNZORTLYITT Kv.QgYUUOS?NQPHNNLQOSPZU K#.[MQNMXRfUPVLQNU KU.ZKRLUJPRNbQcUSW O{.aFYPQ[KZN Ob.kLVRPVNVPTS Iz.qNSJUFSKH[RTMfNVTYQ_N[PhR^M^VFXCTNY@VON?S M%.xPUNKSJWY OU.pMUNXOUQUPQTJWLVNUS O-0#RXPTLQSLMOONLRQWQRPMMQITQObLWR^WTTWS L009HQTNYPUU LJ0=ERPLaRZRZMOZ O500JZKVLUPPYO]JTNTLUU Lz0RBVMODPVM[PVLcRSOKRGOZMRNTNVSUWcHZRgIWJUGKONJaRTU]P[J^QWQZUQZXUZUaMXJaFgOTOYSSUGVUTKTRVLRL^IXPY[SOWBXPXF]?XAYGTBYIRGLJGGXOd Kv0lHKFRFSJUIKK=QKNKUKVRTHOIVOXTQnYUQHYLQNTO]VMJVOXURXI_TUHa[VVXWPRW@NLNI[TY[TdR]PYTZ] LB0kLSQXONPJUEVLUSNWSYWQSZ I_0,T_MYKSNWQYNRLVVZKWLZJYGRI^E^I[:USM]GZLWJ[KYBZIa;_DaJZI^R K*0sK[X[YVPWYRSVCXQVMWNRMJKKIMB@FPNPPJJPKULKQISIWJWOSEZQYRVM_VWOYQVL^VSOVcS][YR[UY_SG[ L(1^QWJWSJKRPMJOOWLNLONJTOYXWQUVXNOMUK_LUVOZVWVHU[ Fl1FT]M^MYKWNBTDVJ[T 4#1tJQRNOOTQ[S Kl1wJITXLRQLNLXKUUUYVU K51kYXPZMHLKKLMKa[ KU1yRHXUWWRVLRLU Kw2*ISXJRNLSTLUQRKWUOXRUZOO_ KM24IS]ET[ KH2kOIQBNVRHTNWLSVUOOPQNWO[TYLTVVLZMSWOTSXEWNQLSPZTZXVTZOYSVQUOOLU 3M2jESQQRMbSTT 282ePUMTISNRQOWOXPUT 2X2yPOXQRNWNNOAUQNXOVTYQVSYPRPUPVYUSTPVRSTZRWPVVMVMRSULRPUPRLUJRQP J62sVVPVLRGOMNSIZOZW %x2rVUNWKUROPOTNRPTP %p3(LTQQUOUR %c3,RQWRRS %[32QROQTOVR %L37PRPQUPTS 1e3BASHPINPNLOWQVSTUVRYW]QNPVPaPUP[PXRTLVOXRVOKMcSYQXRYTSTKVJRSULTOR@aHQNV 2*3NN[ORPLVOTLUS KS3OTXMUJJLISJWJWZ 2%3iFRSN^T 2%3kRQWNQMTQTY Ly4_NMLTMLRMXOUXWTVOXXPU A24jJRQOMRTNWQ]VQTYW ?z4i_PRPcSSULQSTJRJRMU =x5+GQGSPMgLZNTWQUUZ =75JMSTLRFVOVUWRSaLY Mk5,[]RZMbGPNLTFKF=DKWH@GT=OKKHMMLHOVKXOPIWNXWYdHZ_R^WfTRJYOb^fSaVWYPWVZ =@5aKNPOUKVPVZ N#5pG^L:GQMKTH^RW^dK[]bUP^KN 3a60NOUO_NVRWVIRJT 3w6IGVEUOQ[MaOWR +[6GHVIZISAVROUM^PSO[OUNeNWS 4_6^TUMRJOKLHBJLVPLNSOrR]PKMXR_[TPPJYPVSWZM_KPUYHVNOIUUULUKN *S6|ROSMXOUMXNVRJZU^HQ N%6dS[LeMPVNIMQDXIQDPKSGQHTJXYZLRYI^XbcOGe :P6rPYWYHXIQ;FWHJGbQfX =_76COSN^LXZ (@7GMULSCMTMWO :d7QJRJJOHWMRHXWUONNUO_PYLPLCSPKXMGOUNdPIPCHXPYU[QXWWPeUbRZWOWWUSXFTIdKSHaHS[[TZGRHQ^] #d7dJSHPaNXS 1p7yNRMNUOVQVX 1V7~DRKNTOYR_W !|8)TQ[S_NZUHSJULQLT 1+8CMAJMaSVMbW]WXNcO[UKVJQRW3[MPPW 9?8KBMEUCLCYBQLKiPRO@PjMHN}Mz[aYGYTY 2D8TSXJUIPNMTM^R ![8R5]8ZR%XVMW_TPNWO_S]QTMaOZO[RVTS[dS]W=[5TUJKU .r8kOTBVKNIObNZSaOYU RF9!ENTO^R !?8{STDUHQRKaS 0B8m[WD^HWESFKQG_N^LMHJRLJR[BVIPTLCRBPAWGQHXIT=QLN]NGMHXIP5Q@TbWEWHQDT9VBVFSNOFPQZDKB[KSOMKPKVBOCNCSFOINHSBW2VCUEPJS?S;UHQBTSTASNPJVCUCLIS?MHQHNOMFM@RLMbNeI_NZL;RSWJR+IaLVNbOcTYOUU^TXRKNWPTNKOJRJNKSJQOMJMNLYNSLcKaTXNPOSLXR]VVMUUXMXVUP_ULLQKHLDLMNFQGL=KSPeUp[YR_VUWcWUViY[ZKWbYYXaXUOGOMHTM^T[UeUPY^T_NgMbSoLiH_OmQXIaJTI[OXNRM_KTJ_MQLXIfNpDWBOOM[CVQPXETDP@NIUJRIPJWMTJ_FRPZD]HSLTP]QUPXQSOWQXLXD_=XOYKRMNNNSQQXLYPTOST_JTMQJaKXLULSRWVRTJ[MRRWL[O]LWOUOUPXQRPVDbP_SVXOUSZNQNVJWKSO]GXRTKXMWOQLSP[NWK]K[HUMSNUMROPOSOOQTMWL^LVN]OXQTO[N~CWSZUXSZOWO_GWNWPXRWPUPWRZOXQSUUQTOQQPTQP]JTOZKPPTQQKSPVQTPTTQUUPSOYOUPROQQTOWPSQSTVRYPTNUQSTTLWSVTMXSTXUTUWSWOTOTQPPTLYJTIPPTIPLVOJGMQOOSMNQSOFLSNNKPLOPSIPPXMVWTMMKJLOKWHONYNYKVKVMg)ZEQOVJZKfGgHSN]MTCSBO:O;P?LESFOJUCMCE0NQSGUHMLL.XPUaYOM;FVOAIJcOGJNHSAWLOLVK_M^K_NVST[aXaRLWO^VV[VX__XV[HTHYUYZX[RUWS]ZX[UOXMNKUQ]VU[N^TXUPWTZPX^QhTcW[aSWKVTYE[SWXLYS[MWSZP^XVXYXV]d_^fYWTWT]QXSY[]^Z^UZVcV^RTX[WS]]aTaUVVjQgUYUSYZY_bbWXVeMfNVIRKUE^CZCR?WFOTXMW3]JLR]@SOUZZQYMTJkNRQVGZGVHTJRQPFTHZNRRWLYGXMRQZJWJROWZU8QRPLOJSMV=RRUKWKSOYNNSNFNRJVOOJNRNZWWRXOVWSUXOUNRNNFJJSQPLRLMPFNQKKLSKWNTLSSSLROOHNMQOTQRNRRTF_NYQUTTRUSVSSR_SVTUQVTUOUHWLTNQLSNPMRHSPPNTQPOTTUSSTTT]QSTYPSRSTRUPV_PSTYXWRWOTLRJRGPKOPORJOLLNLPCOIONWGTNWPXKYPZOWPWTgTYV[PUQ_UVWW]V[Z[TXSYP[SYLYRVTVQTSPTRVPUVSZS[Q^SXPWMTQ]WVPZJTMPK_=VOSNWQUSUXR^N^RWMYOZPYTYVXXW^YSUWVWRYX]UYZOaOSR[KUYQPXTWSIWNOLSQZ_RVPTPWWOSOYZTYOSUURPYRbUOTCPZUfUWRXSSUOTSONROWRXSSXYiX]WQUoT@JSIYQ_Z[VkX_XLVQYHGAPEWLg[XIWCQ;JBEJPnkaVZYmSjPeS_[eUbYQ_NUKTNZKUBUIXEWVY0uLNHGDLJXITO_R[@ROV;[IP5VKOUCWIFG_KYISKLKGKBNYLWCMGJOC]K^O]SZ5TGVFUKWHV?VDPIeCSS_VZ]^aZ^TSXZVbS_Ycf^ObSgYSWKWZXQWEW3TQGKIGKIYUYFYDJS] ,^985LCGeOdR9NWNwShPEPDR:P_J[SaOXMjTiSgU^SxNUULVdSSV,]IeGVHSIQYHMNJ[IVFMEW9OWV -m9;EROQcK[X 1~9=9SNQQNbL^TiQRU 1,9?6OHNJIUHbRHNZNzNxOfTWUZN[R^KKPiOcMVNTNFHNMUO(PKK[NqTRUqHOO_MiLoNQV7bmJaPUYL]NTDWHXSXaSdH_Ne_UYCRC[CU8XfWI[IV8VHY?OQWDV:WHO?OSXA^6MIJJY .:99^K;SROmJYO[QeVT[BV]WOU@PFT N*9:DVEQINjPcR 0F95c[?T;RHPMMRMXJ^Q^Z ,!9(aYy];Y7RDT-Q]L=@kLWNnW N]9N@TOOgNdQ^V 009NIUDPMM_PbQXV Mp9V5REPMLhKdTyRZZ .Y9[DRCOGRSKdRRM^PdSWaJV -K9]GSINbIAR@X4VIODHbPjSkS;MTOcRmVvTXWPWHRIP Cg9!P[sgs]uVuWYUKV=QAN.Q4L4LUM8I[Q=JSLHPSMfOUNtPXS .n9cNN^NiQ]NWG~NmSmSaPbUYTOVHVAS=QBP4S@WUUIV=RGU ,E9fCR;LAJbPdTgV[RZVSY IR9bbUAU8NHNPM?R:MJT5QOP6P=MGRFLZL;Q8S@OSH[J@W;RANWKGSV]MXMQTK@KaJJHTM^QNKXMIMOMGONOGSgbWYGXTbOYKU]ZPX5TKPKI@IXGNN_QSO]MKM+^DU3VLN^LFLGXCN?QKOFS]XJS4K4KPLBPKURX_SMX4UYLMLZLLKIUIS9HXKJO8XKOXMQM;XQcBYYTjNkNdU^WRYJV)_5TAXHOGXaU1Y@P?RCJBS:M:H:H:B;A?H5IILU9VGaMbTh_XLZCZGUI_RXZ_QW[Ve^U[^HWKYYbxePX]YeUbMSN2EILNLVIPH]OXLdUfUfS]MGMGQHTEQEOSLYOUSODJRJZINMLRJTJDOPMHRQUHT4JKOMUGVMOGPSVGTRVPWYYUQTVMSQUTUSWJONPIQNLRHVMSLTNKMPTJRPPJRDNICMOANOJ9JIWUFAUEPSKcNZM]GP=ML8RGTDRFSDKVNRKTGPHNNQKTOWQSLPJWSWPVU]PUMUNWQVVXVkRYYYTTYXVNXVZXXSUaT[YRXSXcUgO[XWSWVWTaMUHdFZNYQWNXPUNVPYHPPPIVSWZWSSWJVWXZQXMTVQTIWBXUTOUJRGZMZIWOWSUQWZUZQPQRNUOUVXQRNWNPQZK[OXObLXLPHSNXNTL[HXCZNYRN[XSOW[PRWHZVTLWPXTTWMXRWSKX_T_QQPVN_Z_STTIUN]UYXVT_UQWTRUZYUX]SRNdPUPHNQP]OPNXPaW^TTVHQMURXZU]SXUeURODNWNJJKPZM_NdFXQXNTLQMFMISFPDVAYARFOFL?SNKCQGIXMMJYMXH^R]LYTTV^QZMcSYX[PXSNNUMPNSJMKMGPKOMSQOOMPJRJUPNOWKTIQNOJO8YEROUAUOVDTLQJMONUJMMMPGW9XM[EVJTNQFUNTPWLRPWYWS[OTRWWWQTJNRWKTGMKRNTHRHNMT.OHOLNGPHMDTHQLWKRHA@ILLMDTEJJMOJKIQMNKHLONKRMOLOQLKOKROOMNOQIXNTMQMUCPIPMRLKFNPWOVJQNTHUPRPTOVR[LTKWO[HSKTNWM]MXK[M[J]NWQeY_TXRYPVR[O[UXUlZZTdRYKVKWLZQVUVQ]VROTQTMWPVLM?TPMCPOROXHYI]HZHVGQPWLWAONRPWFUORLQKGCK9PITDZIZ?WJVNSLRFZ0VKYJa;XHRKOPVKQLTPRSWNUQVOWRXVZS^VVRXS^PWTXQSTWS^VYVge_e[XUWViPSQVVUh[WVSVPTSVS]PRPZMZSYVTYZVS^^i[XXYbPWRdPZS]PWNTKdRZUXQULUQXW_VVTXUVTYUSUVYVTUTW^_wn^a[^[c^lT^TUSbNRNOHOIRNPMQIOHQHOMRNWPVJVXURVPUMUDcD[MWO[L_IXN_Q]TTO]D^RYTSI^K^JdL]N^SSYCZJUTTWTZUVODWR^CVJUPUMRPULWPWNXGQLSKYHVQYKUJXIWORNUOTMRNQPSNPQUNVHQLULYRUSWRSUURTU^SYTWVZR[YdW]VRUTWYUWRXUXRWTRVVUXRTTSWXUWRSTQXSWTUWQVYVUW[RTOTJ[EULUNYQZTXQS89DSJPRUPQPVTXRXOUOQOMSKK^SVRVKVPVMTKdSTPWXQUVYOWSZEXI]O]J_O]WZSWQVE}LlTmSYKUJZP^IUONNaFYQbXTHRFY0[DW?ZD[8[IVUVX[UOUVZXSReV[OiUZWSbXRV]YYX__bYWWRZ_TYRSVYPSSWPUXQV[RVLTMSMUMYJXPPOZFTIOEXPXQf_VJTEUFVMQGVLOKSJNCSLTX]GTMVOTMQKVMSIXLSMiBYSRUO[NUP[SWRYNXD_LTMWP[N[KRRZZkS^[RPJ[R[NWHVLZQZLOKVL_[Z[_XZYSeNgLZIXAlTYZ[b[VSbOONUKWRV^]TbWXVVOXW^Spe[_clXSVdLUPXXTRZKZL[N]HWWY[WWXbTQXKRIVGKJTRWJTLXWW]SXX_YZUXMJLTNMM]UZVbUVPXQSPOLSQPPONWPSPUSSSWPYJJPWHPJSM[SZV[TVWR_OXI_MWNSSSRWWTXTXWWTRYUUVVVQSVbYXMXRbXuqVYcbVdS_Z]Q[D^GSLMHTMXBSxjwiuUrQaWcPRKyUJYmdnVZEn^X[_SJC@J,:BORNILMD]*VKb[U^cTV^eXNVVZ]SSbGTQV_]UYbP[WWNnZbKVVrb[SpYfPUUQVMULZIWaQaWR|DV3W;Q1UMNZKFO@[?Q@T@QDPFUSZJV=S%P6ZIX2T2T2TDNZIASKO?VAOBVHHBVFZXVMXBXBR;SQK!SPW0TBPMOMYIP5VZUyZTWJV:W3RHPPW Bv5SG_SYI[]][TWX[T_VZP]STKPIJSIPRKHSROXOVI_OTNONVISZZTUMYLJNHUPIYROKZOPGTKQPCOCTKWITOYRWUTV_ZSOTMS 039gKT=RTPgQ -;9kDTDQJNbOgU ?x9lNV;PXNKPfOjW -09sMRHNdR_T .y9oFV@SWJZPcT .C9s5V=SSN_PHOkQdOdQ IY9r.XJQ?EpTqT ?5:#CQNODRMU4OZJhIcOIOiL_RWZ[TWZmV ?t:,HODT1O^NbQTPqPqScX=U C::,CTCNPUCR=PkRLN^P_TSSjT H|9}9[=X;O6KaPZKeNpPiS 0r:(.VEX9QVPHQDKXM^OgRJOQO^MiQfSfYbUFV 2s:H;R3QMS1Q1PJLGUFSCN9P9PTOhKfNwSkQBNJS5SVLbNOO@PJNePaK6WKQWK@PSMvPqTnOoXRTARPTbUWVdU[UJWYTDSrSYThTeZ|[:R[TmTlWST?U 7):L,QEO,R-LQNlM2V3U6MGU1R1P8PWMCQ,KJNoONN)M*MQNkN~P=Q6OqInRmSiQoLRNaNn;GIZO]SnI=THRNMSK_O[SeWPJNNCOHFYNNKbITIWKeFYH[MhRcIbS]fNZdZWXRY[YvWjYb]]V^RrVoZoZ#S_XOXhIeTOZEXDTWUhNRVEYbRbTVUFVjSE[^RP[?WeUcSFXR_ha9StTWT}YLV2T8P8P6O^WJV?O1U,QMTsRrSsRgV0V1V';
  const landRings = (() => {
    const index = {};
    let n = 0;
    for (let c = 33; c < 127; c++) if (!'"\'\\`<>&/$'.includes(String.fromCharCode(c))) index[String.fromCharCode(c)] = n++;
    return LAND.split(' ').map((ring) => {
      let x = index[ring[0]] * 85 + index[ring[1]], y = index[ring[2]] * 85 + index[ring[3]];
      const points = [[(y / 10 - 90) * DEG, (x / 10 - 180) * DEG]];
      for (let k = 4; k < ring.length; k += 2) {
        x += index[ring[k]] - 42;
        y += index[ring[k + 1]] - 42;
        points.push([(y / 10 - 90) * DEG, (x / 10 - 180) * DEG]);
      }
      return points;
    });
  })();

  // Land and grid lines as points on a unit ball, for one flattening. A point
  // at latitude φ on a flattened Earth sits at the ball's latitude
  // atan(squash · tan φ); scaling the ball's z by `squash` puts it in place.
  const shapes = new Map();
  const shapeFor = (squash) => {
    const key = squash.toFixed(5);
    if (!shapes.has(key)) {
      if (shapes.size > 2) shapes.clear();
      const ball = (lat, lon) => {
        const beta = Math.atan(squash * Math.tan(lat));
        return [cos(beta) * cos(lon), cos(beta) * sin(lon), sin(beta)];
      };
      const grid = [];
      for (let lon = -150; lon <= 180; lon += 30) if (lon) grid.push(sweep(40, (t) => ball((t * 160 - 80) * DEG, lon * DEG)));
      for (const lat of [-60, -30, 30, 60]) grid.push(sweep(72, (t) => ball(lat * DEG, t * TAU)));
      shapes.set(key, {
        land: landRings.map((ring) => ring.map(([lat, lon]) => ball(lat, lon))),
        grid,
        equator: sweep(120, (t) => ball(0, t * TAU)),
        greenwich: sweep(60, (t) => ball((t * 180 - 90) * DEG, 0)),
      });
    }
    return shapes.get(key);
  };

  const makeView = (canvas, { yaw, pitch, fit, centre = [0, 0, 0], pitchRange = [-85, 85], draw }) => {
    const ctx = canvas.getContext('2d');
    const view = { ctx, yaw, pitch, yawOffset: 0, centre, earth: null, turned: false };

    view.render = () => {
      const w = canvas.clientWidth, h = canvas.clientHeight, dpr = window.devicePixelRatio || 1;
      if (!w || !h) return;
      if (canvas.width !== round(w * dpr) || canvas.height !== round(h * dpr)) {
        canvas.width = round(w * dpr);
        canvas.height = round(h * dpr);
      }
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, w, h);
      ctx.lineJoin = ctx.lineCap = 'round';
      const y = (view.yaw + view.yawOffset) * DEG, p = view.pitch * DEG;
      view.right = [-sin(y), cos(y), 0];
      view.up = [-sin(p) * cos(y), -sin(p) * sin(y), cos(p)];
      view.toward = [cos(p) * cos(y), cos(p) * sin(y), sin(p)];
      view.w = w;
      view.h = h;
      view.small = w < 520;
      view.k = min(w / 2 / fit[0], h / 2 / fit[1]); // pixels per kilometre
      view.earth = null;
      draw(view);
      if (!view.turned) view.text('Drag to turn', 10, h - 10, { color: C.muted, size: 13 });
    };

    view.project = (p) => {
      const q = sub(p, view.centre);
      return [view.w / 2 + view.k * dot(q, view.right), view.h / 2 - view.k * dot(q, view.up)];
    };

    // Put the Earth in the scene: world = m · earth + o, with its z squashed.
    view.placeEarth = (m, o = [0, 0, 0], squash = 1 - F_REAL) => {
      const d = applyT(m, view.toward);
      const ray = [d[0], d[1], d[2] / squash];
      view.earth = { m, o, squash, ray, normal: unit(ray) };
    };

    // Is the globe between this point and the camera? Solve for where the
    // line of sight meets a ball, after stretching the squashed Earth back
    // into one. The ball is drawn a shade small so the surface itself shows.
    view.hidden = (p) => {
      const e = view.earth;
      if (!e) return false;
      const q = applyT(e.m, sub(p, e.o));
      q[2] /= e.squash;
      const a = dot(e.ray, e.ray), b = dot(q, e.ray), c = dot(q, q) - (R_EARTH * 0.997) ** 2;
      const disc = b * b - a * c;
      return disc > 0 && sqrt(disc) > b;
    };

    // A line through 3D points: solid where it can be seen, faint where the
    // globe covers it. `hidden` is the faint part's opacity; 0 leaves it out.
    view.path = (points, { color, width = 1.5, dash = [], alpha = 1, hidden = 0.3, hiddenDash = dash }) => {
      const seen = [], covered = [];
      let previous = null, wasHidden = false, run = null;
      for (const p of points) {
        const isHidden = view.hidden(p);
        if (!previous || isHidden !== wasHidden) {
          const next = [];
          if (previous) {
            let a = previous, b = p; // find where the line goes behind the globe
            for (let k = 0; k < 6; k++) {
              const m = mix(a, b, 0.5);
              if (view.hidden(m) === wasHidden) a = m; else b = m;
            }
            const edge = view.project(a);
            run.push(edge);
            next.push(edge);
          }
          run = next;
          (isHidden ? covered : seen).push(run);
        }
        run.push(view.project(p));
        previous = p;
        wasHidden = isHidden;
      }
      const stroke = (runs, opacity, pattern) => {
        if (!opacity || !runs.length) return;
        ctx.beginPath();
        for (const r of runs) r.forEach((q, k) => (k ? ctx.lineTo(q[0], q[1]) : ctx.moveTo(q[0], q[1])));
        ctx.globalAlpha = opacity;
        ctx.setLineDash(pattern);
        ctx.strokeStyle = color;
        ctx.lineWidth = width;
        ctx.stroke();
        ctx.setLineDash([]);
        ctx.globalAlpha = 1;
      };
      stroke(covered, hidden * alpha, hiddenDash);
      stroke(seen, alpha, dash);
    };

    // A flat filled shape. The caller decides whether it goes in front of the globe.
    view.fill = (points, colour) => {
      ctx.beginPath();
      points.forEach((p, k) => {
        const q = view.project(p);
        k ? ctx.lineTo(q[0], q[1]) : ctx.moveTo(q[0], q[1]);
      });
      ctx.closePath();
      ctx.fillStyle = colour;
      ctx.fill();
    };

    view.head = (from, to, color, size = 10) => {
      const a = view.project(from), b = view.project(to);
      const angle = atan2(b[1] - a[1], b[0] - a[0]);
      ctx.save();
      ctx.translate(b[0], b[1]);
      ctx.rotate(angle);
      ctx.globalAlpha = view.hidden(to) ? 0.35 : 1;
      ctx.beginPath();
      ctx.moveTo(size * 0.3, 0);
      ctx.lineTo(-size * 0.8, size * 0.45);
      ctx.lineTo(-size * 0.8, -size * 0.45);
      ctx.closePath();
      ctx.fillStyle = color;
      ctx.fill();
      ctx.restore();
    };

    view.arrow = (from, to, style) => {
      view.path(seg(from, to), style);
      view.head(mix(from, to, 0.9), to, style.color, 6 + 2 * (style.width || 1.5));
    };

    view.dot = (p, radius, color) => {
      const q = view.project(p);
      ctx.globalAlpha = view.hidden(p) ? 0.4 : 1;
      ctx.beginPath();
      ctx.arc(q[0], q[1], radius, 0, TAU);
      ctx.fillStyle = color;
      ctx.fill();
      ctx.lineWidth = 1.5;
      ctx.strokeStyle = '#fff';
      ctx.stroke();
      ctx.globalAlpha = 1;
    };

    // Text with a white halo, so it stays readable over lines.
    view.text = (string, x, y, { color = C.ink, align = 'left', size = 14, weight = '', alpha = 1 } = {}) => {
      ctx.font = `${weight} ${size}px ${FONT}`;
      ctx.textAlign = align;
      ctx.textBaseline = 'middle';
      // Keep the whole string inside the canvas.
      const width = ctx.measureText(string).width, lead = align === 'left' ? 0 : align === 'right' ? width : width / 2;
      x = clamp(x, 4 + lead, view.w - 4 - (width - lead));
      y = clamp(y, size * 0.6, view.h - size * 0.6);
      ctx.globalAlpha = alpha;
      ctx.lineWidth = 4;
      ctx.strokeStyle = 'rgba(255,255,255,.9)';
      ctx.strokeText(string, x, y);
      ctx.fillStyle = color;
      ctx.fillText(string, x, y);
      ctx.globalAlpha = 1;
    };

    view.label = (p, string, { dx = 0, dy = 0, ...style } = {}) => {
      const q = view.project(p);
      view.text(string, q[0] + dx, q[1] + dy, { alpha: view.hidden(p) ? 0.55 : 1, ...style });
    };

    // The globe: sea, land, grid lines, equator and (optionally) the
    // Greenwich meridian. Land rings are cut where they pass over the edge
    // and the cut ends joined along the edge, so the fill is exact.
    view.globe = ({ greenwich = false } = {}) => {
      const e = view.earth, shape = shapeFor(e.squash), n = e.normal;
      const a = unit(cross(abs(n[2]) < 0.9 ? [0, 0, 1] : [1, 0, 0], n)), b = cross(n, a);
      const at = (u) => view.project(add(apply(e.m, [u[0] * R_EARTH, u[1] * R_EARTH, u[2] * R_EARTH * e.squash]), e.o));
      const edge = (phi) => at(add(mul(a, cos(phi)), mul(b, sin(phi))));
      const angle = (u) => atan2(dot(u, b), dot(u, a));
      const outline = new Path2D();
      for (let k = 0; k < 144; k++) {
        const q = edge(k / 144 * TAU);
        k ? outline.lineTo(q[0], q[1]) : outline.moveTo(q[0], q[1]);
      }
      outline.closePath();
      ctx.fillStyle = C.sea;
      ctx.fill(outline);
      ctx.save();
      ctx.clip(outline);

      const runs = [];
      ctx.beginPath();
      for (const ring of shape.land) {
        const count = ring.length, side = ring.map((u) => dot(u, n));
        const front = side.filter((s) => s > 0).length;
        if (!front) continue;
        if (front === count) {
          ring.forEach((u, k) => {
            const q = at(u);
            k ? ctx.lineTo(q[0], q[1]) : ctx.moveTo(q[0], q[1]);
          });
          ctx.closePath();
          continue;
        }
        let start = 0;
        while (side[start] > 0 || side[(start + 1) % count] <= 0) start++;
        let run = null;
        for (let j = 1; j <= count; j++) {
          const k = (start + j) % count, prev = (k + count - 1) % count;
          if (side[k] > 0 === side[prev] > 0) {
            if (run) run.points.push(ring[k]);
            continue;
          }
          const cut = unit(mix(ring[prev], ring[k], side[prev] / (side[prev] - side[k])));
          if (side[k] > 0) run = { points: [cut, ring[k]], from: angle(cut) };
          else {
            run.points.push(cut);
            run.to = angle(cut);
            runs.push(run);
            run = null;
          }
        }
      }
      runs.sort((p, q) => p.from - q.from);
      for (const first of runs) {
        if (first.used) continue;
        let run = first;
        for (;;) {
          run.used = true;
          run.points.forEach((u, k) => {
            const q = at(u);
            run === first && !k ? ctx.moveTo(q[0], q[1]) : ctx.lineTo(q[0], q[1]);
          });
          // Land lies counter-clockwise along the edge from where a coast
          // leaves the visible side, as far as the next coast coming back.
          const next = runs.find((r) => r.from > run.to) || runs[0];
          const end = next.from > run.to ? next.from : next.from + TAU;
          for (let phi = run.to + 0.04; phi < end; phi += 0.04) {
            const q = edge(phi);
            ctx.lineTo(q[0], q[1]);
          }
          if (next.used) break;
          run = next;
        }
        ctx.closePath();
      }
      ctx.fillStyle = C.land;
      ctx.fill();
      ctx.strokeStyle = C.coast;
      ctx.lineWidth = 0.75;
      ctx.stroke();

      const surfaceLine = (points, color, width) => {
        ctx.beginPath();
        let pen = false;
        for (const u of points) {
          if (dot(u, n) > 0) {
            const q = at(u);
            pen ? ctx.lineTo(q[0], q[1]) : ctx.moveTo(q[0], q[1]);
            pen = true;
          } else pen = false;
        }
        ctx.strokeStyle = color;
        ctx.lineWidth = width;
        ctx.stroke();
      };
      for (const line of shape.grid) surfaceLine(line, C.grid, 1);
      surfaceLine(shape.equator, 'rgba(24,53,74,.55)', 1.25);
      surfaceLine(shape.greenwich, greenwich ? C.navy : C.grid, greenwich ? 2.25 : 1);

      // A slight darkening toward the edge, so the disc reads as a ball.
      const middle = view.project(e.o), radius = R_EARTH * view.k;
      const shade = ctx.createRadialGradient(middle[0], middle[1], radius * 0.5, middle[0], middle[1], radius);
      shade.addColorStop(0, 'rgba(24,53,74,0)');
      shade.addColorStop(1, 'rgba(24,53,74,.14)');
      ctx.fillStyle = shade;
      ctx.fill(outline);
      ctx.restore();
      ctx.strokeStyle = C.navy;
      ctx.lineWidth = 1.25;
      ctx.stroke(outline);
    };

    // Drag, or arrow keys when focused, to turn the view.
    let drag = null;
    const turn = (dYaw, dPitch) => {
      view.yaw += dYaw;
      view.pitch = clamp(view.pitch + dPitch, pitchRange[0], pitchRange[1]);
      view.turned = true;
      view.render();
    };
    canvas.addEventListener('pointerdown', (event) => {
      if (event.button) return;
      drag = { id: event.pointerId, x: event.clientX, y: event.clientY };
      canvas.setPointerCapture(event.pointerId);
    });
    canvas.addEventListener('pointermove', (event) => {
      if (!drag || event.pointerId !== drag.id) return;
      turn((drag.x - event.clientX) * 0.35, (event.clientY - drag.y) * 0.35);
      drag.x = event.clientX;
      drag.y = event.clientY;
    });
    const release = (event) => {
      if (drag && event.pointerId === drag.id) drag = null;
    };
    canvas.addEventListener('pointerup', release);
    canvas.addEventListener('pointercancel', release);
    canvas.addEventListener('keydown', (event) => {
      const step = { ArrowLeft: [5, 0], ArrowRight: [-5, 0], ArrowUp: [0, -5], ArrowDown: [0, 5] }[event.key];
      if (!step) return;
      event.preventDefault();
      turn(step[0], step[1]);
    });
    new ResizeObserver(view.render).observe(canvas);
    return view;
  };

  // ── Shared pieces of the figures ─────────────────────────────────────────
  const $ = (id) => document.getElementById('orbit-' + id);
  const number = (value, digits = 0) => {
    const text = abs(value).toLocaleString('en-GB', { minimumFractionDigits: digits, maximumFractionDigits: digits });
    return (value < 0 && Number(abs(value).toFixed(digits)) > 0 ? '−' : '') + text;
  };
  const clock = (unixS, seconds = false) => new Date(round(unixS) * 1000).toISOString().slice(11, seconds ? 19 : 16);
  const colour = (name, text) => `<span class="c-${name}">${text}</span>`;
  // Show a slider's value beside its label, and say the same to a screen reader.
  const say = (id, text) => {
    $(`${id}-out`).textContent = text;
    $(id).setAttribute('aria-valuetext', text);
  };

  // Three legs from the centre to a point: along x, then y, then z.
  const legs = (view, p) => {
    const corners = [[0, 0, 0], [p[0], 0, 0], [p[0], p[1], 0], p];
    for (let k = 0; k < 3; k++) {
      view.path(seg(corners[k], corners[k + 1]), { color: C.navy, width: 2, hidden: 0.6, hiddenDash: [2, 5] });
    }
    view.dot(corners[1], 2.5, C.navy);
    view.dot(corners[2], 2.5, C.navy);
  };

  const axes = (view, length, names) => {
    [[length, 0, 0], [0, length, 0], [0, 0, length]].forEach((tip, k) => {
      view.arrow([0, 0, 0], tip, { color: C.navy, width: 1.25, hidden: 0.3 });
      view.label(mul(tip, 1.06), names[k], { align: 'center', weight: 'bold' });
    });
  };

  // A Play button that runs a slider to its end. With reduced motion asked
  // for, each press is one jump instead.
  const still = window.matchMedia('(prefers-reduced-motion: reduce)');
  const player = (button, slider, perSecond, show) => {
    let frame = 0, last = 0, value = 0;
    const name = () => {
      button.textContent = frame ? 'Pause' : still.matches ? 'Step' : 'Play';
    };
    const stop = () => {
      cancelAnimationFrame(frame);
      frame = 0;
      name();
    };
    const tick = (now) => {
      value += perSecond * min(0.1, (now - last) / 1000);
      last = now;
      const done = value >= Number(slider.max);
      if (done) value = Number(slider.max);
      slider.value = value;
      show(value);
      if (done) stop(); else frame = requestAnimationFrame(tick);
    };
    button.addEventListener('click', () => {
      if (frame) return stop();
      value = Number(slider.value) >= Number(slider.max) ? Number(slider.min) : Number(slider.value);
      if (still.matches) {
        slider.value = min(Number(slider.max), value + (Number(slider.max) - Number(slider.min)) / 12);
        return show(Number(slider.value));
      }
      last = performance.now();
      frame = requestAnimationFrame(tick);
      name();
    });
    slider.addEventListener('input', () => frame && stop());
    still.addEventListener('change', name);
    name();
  };

  // ── Figure 1: the record's numbers shape a ring ──────────────────────────
  const figRing = () => {
    const inputs = { tilt: $('ring-tilt'), facing: $('ring-facing'), where: $('ring-where'), laps: $('ring-laps') };
    const real = { tilt: RECORD.inclination, facing: RECORD.raan, where: RECORD.meanAnomaly, laps: RECORD.meanMotion };
    const s = { ...real };
    const theta = sidereal(RECORD.epochUnix);
    const PLATE = 8700;

    const view = makeView($('ring-canvas'), {
      yaw: -72, pitch: 22, fit: [10300, 9300],
      draw: (v) => {
        const orbit = orbitFrom({ ...RECORD, inclination: s.tilt, raan: s.facing, meanAnomaly: s.where, meanMotion: s.laps });
        const ring = ringAt(orbit, 0), sat = propagate(orbit, 0);
        const facing = s.facing * DEG, tilt = s.tilt * DEG, yaw = v.yaw * DEG;
        // The plane of the equator, as a plate: far half, globe, near half.
        const half = (from) => [...sweep(48, (t) => flat(PLATE, from + t * PI)), ...sweep(48, (t) => flat(R_EARTH, from + PI - t * PI))];
        v.placeEarth(zTurn(theta));
        v.fill(half(yaw + PI / 2), 'rgba(24,53,74,.06)');
        v.globe();
        v.fill(half(yaw - PI / 2), 'rgba(24,53,74,.06)');
        v.path(sweep(120, (t) => flat(PLATE, t * TAU)), { color: C.navy, width: 1, alpha: 0.3 });
        v.path(seg([0, 0, -7500], [0, 0, 7800]), { color: C.navy, width: 1, alpha: 0.7 });

        // Which way the ring faces: an angle round the equator from a fixed direction.
        v.arrow([0, 0, 0], flat(PLATE + 1000, 0), { color: C.navy, width: 1.25 });
        v.label(flat(PLATE + 1000, 0), v.small ? 'fixed' : 'fixed direction', { dx: 9, size: 13 });
        v.path(seg([0, 0, 0], flat(PLATE, facing)), { color: C.navy, width: 1, hiddenDash: [2, 4] });
        const swept = sweep(90, (t) => flat(PLATE, t * facing));
        v.path(swept, { color: C.navy, width: 3 });
        if (s.facing > 4) v.head(swept[88], swept[90], C.navy, 13);
        v.label(flat(PLATE, facing - min(0.4, facing / 2)), `facing ${s.facing.toFixed(1)}°`, { dy: 19, align: 'center', weight: 'bold' });

        v.path(ring, { color: C.sat, width: 2.5 });
        for (const k of [50, 140]) v.head(ring[k - 1], ring[k], C.sat, 11);

        // Tilt: the angle between the equator and the ring where they cross.
        const node = flat(orbit.a * RE_KM, facing), east = [-sin(facing), cos(facing), 0];
        const lean = (radius, angle) => add(node, add(mul(east, radius * cos(angle)), [0, 0, radius * sin(angle)]));
        v.path(seg(node, lean(2300, 0), 6), { color: C.navy, width: 1 });
        v.path(sweep(40, (t) => lean(2000, t * tilt)), { color: C.navy, width: 3 });
        v.label(lean(2900, tilt / 2), `tilt ${s.tilt.toFixed(1)}°`, { align: 'center', weight: 'bold' });

        // Where along the ring: counted from the ring's lowest point.
        v.path(seg(mul(ring[0], 0.96), mul(ring[0], 1.04), 2), { color: C.sat, width: 2.5 });
        v.label(mul(ring[0], 1.1), '0°', { align: 'center', color: C.sat, size: 13 });
        v.dot(sat, 6.5, C.sat);
        v.label(sat, 'METOP-B', { dx: 13, dy: 4, color: C.sat, weight: 'bold' });
      },
    });

    const cell = (value, width, digits) => value.toFixed(digits).padStart(width);
    const show = () => {
      const body = LINE2.slice(0, 8) + cell(s.tilt, 8, 4) + ' ' + cell(s.facing, 8, 4) + LINE2.slice(25, 43)
        + cell(s.where, 8, 4) + ' ' + cell(s.laps, 11, 8) + LINE2.slice(63, 68);
      const mark = (text, from, to, kind) => `${text.slice(0, from)}<b class="${kind}">${text.slice(from, to)}</b>${text.slice(to)}`;
      const one = mark(mark(LINE1, 68, 69, 'k'), 18, 32, 'd');
      let two = body + checksum(body);
      for (const [from, to, kind] of [[68, 69, 'k'], [52, 63, 'f'], [43, 51, 'f'], [17, 25, 'f'], [8, 16, 'f']]) two = mark(two, from, to, kind);
      const caption = (...labels) => {
        const row = Array(69).fill(' ');
        for (const [text, from] of labels) [...text].forEach((ch, k) => { row[from + k] = ch; });
        return `<i>${row.join('')}</i>`;
      };
      $('tle').innerHTML = `${caption(['the record’s date', 16], ['check', 64])}\n${one}\n${two}\n`
        + caption(['tilt', 10], ['facing', 18], ['where', 44], ['laps a day', 52], ['check', 64]);
      say('ring-tilt', `${s.tilt.toFixed(1)}°`);
      say('ring-facing', `${s.facing.toFixed(1)}°`);
      say('ring-where', `${s.where.toFixed(1)}°`);
      say('ring-laps', s.laps.toFixed(2));
      const orbit = orbitFrom({ ...RECORD, inclination: s.tilt, meanMotion: s.laps });
      const lap = TAU / orbit.mRate, height = orbit.a * RE_KM - R_EARTH;
      $('ring-out').innerHTML = `${s.laps.toFixed(2)} laps a day is one lap every <strong>${lap.toFixed(1)} minutes</strong>. `
        + `Gravity allows that only on a ring <strong>${number(orbit.a * RE_KM)} km</strong> from Earth’s centre: `
        + `about ${number(height)} km above the ground, at ${(TAU * orbit.a * RE_KM / lap / 60).toFixed(1)} km every second.`;
      view.render();
    };
    for (const [key, input] of Object.entries(inputs)) {
      input.addEventListener('input', () => {
        s[key] = Number(input.value);
        show();
      });
    }
    $('ring-reset').addEventListener('click', () => {
      Object.assign(s, real);
      for (const [key, input] of Object.entries(inputs)) input.value = real[key];
      show();
    });
    show();
  };

  // ── Figure 2: minutes in, x y z out ──────────────────────────────────────
  const figTime = () => {
    const minutes = $('time-minutes'), days = $('time-days');
    let t = 0;
    const view = makeView($('time-canvas'), {
      yaw: 38, pitch: 24, fit: [10600, 10000],
      draw: (v) => {
        const p = propagate(ORBIT, t);
        v.placeEarth(zTurn(sidereal(RECORD.epochUnix + t * 60)));
        v.globe();
        axes(v, 9200, ['x', 'y', 'z']);
        v.path(ringAt(ORBIT, 0), { color: C.muted, width: 1.25, dash: [5, 5], hidden: 0.35 });
        v.path(ringAt(ORBIT, t), { color: C.sat, width: 2.5 });
        legs(v, p);
        v.dot(p, 6.5, C.sat);
        v.label(p, 'METOP-B', { dx: 12, dy: -12, color: C.sat, weight: 'bold' });
      },
    });
    const show = (value = Number(minutes.value)) => {
      t = value + Number(days.value) * 1440;
      const p = propagate(ORBIT, t);
      say('time-minutes', number(value));
      say('time-days', days.value);
      $('time-out').innerHTML = `<strong>${number(t)}</strong> minutes after the record’s date: `
        + `x = <strong>${number(p[0])}</strong> km, y = <strong>${number(p[1])}</strong> km, z = <strong>${number(p[2])}</strong> km. `
        + `The ring has swung ${(ORBIT.raanRate * t / DEG).toFixed(2)}° since the record’s date.`;
      view.render();
    };
    minutes.addEventListener('input', () => show());
    days.addEventListener('input', () => show());
    player($('time-play'), minutes, 10, show);
    show();
  };

  // ── Figure 3: the same scene from the stars and from the ground ──────────
  const figSpin = () => {
    const slider = $('spin-time'), holds = document.querySelectorAll('input[name="orbit-spin-hold"]');
    // Start the slider at the moment Greenwich lines up with the fixed x axis.
    let low = NOON, high = NOON + 86000;
    while (high - low > 0.01) {
      const mid = (low + high) / 2;
      if (sidereal(mid) > sidereal(NOON)) low = mid; else high = mid;
    }
    const start = high;
    let t = start, hold = 'stars';
    // θ wraps to zero after one turn; near the slider's end, show the turn completed.
    const turned = () => sidereal(t) + (t - start > 80000 && sidereal(t) < PI ? TAU : 0);
    const view = makeView($('spin-canvas'), {
      yaw: 75, pitch: 28, fit: [10900, 9700],
      draw: (v) => {
        const theta = turned(), now = minutesAt(t);
        v.placeEarth(zTurn(theta));
        v.globe({ greenwich: true });
        v.path(seg([0, 0, -7500], [0, 0, 7500]), { color: C.navy, width: 1, alpha: 0.7 });
        // Two x axes; whichever tip is higher on screen has its label above it.
        const fixed = [9300, 0, 0], riding = flat(9300, theta);
        const lift = v.project(fixed)[1] < v.project(riding)[1] ? -16 : 17;
        v.arrow([0, 0, 0], fixed, { color: C.navy, width: 1.5 });
        v.arrow([0, 0, 0], riding, { color: C.navy, width: 1.5 });
        const arc = sweep(96, (k) => flat(8000, k * theta));
        v.path(arc, { color: C.navy, width: 2.5 });
        if (theta > 0.08) v.head(arc[94], arc[96], C.navy, 11);

        v.path(ringAt(ORBIT, now), { color: C.sat, width: 1.25, alpha: 0.6 });
        // Where the satellite has been for the last two laps, in the view held still.
        const trail = sweep(200, (k) => {
          const back = k * 200, p = propagate(ORBIT, now - back);
          return hold === 'earth' ? rotZ(p, theta - sidereal(t - back * 60)) : p;
        });
        v.path(trail, { color: C.sat, width: 2.5 });
        v.dot(trail[0], 6.5, C.sat);
        v.label(fixed, v.small ? 'x: stars' : 'x, fixed to the stars', { dy: lift, align: 'center', size: 13, alpha: 1 });
        v.label(riding, v.small ? 'x: Earth' : 'x, riding on the Earth', { dy: 1 - lift, align: 'center', size: 13, alpha: 1 });
        v.label(flat(6700, theta / 2), `θ = ${(theta / DEG).toFixed(1)}°`, { align: 'center', weight: 'bold', alpha: 1 });
      },
    });
    const show = (value = Number(slider.value)) => {
      t = start + value * 60;
      const theta = turned(), p = propagate(ORBIT, minutesAt(t)), q = rotZ(p, -theta);
      view.yawOffset = hold === 'earth' ? theta / DEG : 0;
      const turns = theta / DEG;
      const xyz = (v) => `x ${number(v[0])}, y ${number(v[1])}, z ${number(v[2])} km`;
      say('spin-time', `${clock(t)} UTC, ${t < NOON + 43200 ? 7 : 8} October`);
      $('spin-out').innerHTML = `Greenwich has turned <strong>θ = ${turns.toFixed(1)}°</strong> past the fixed x axis.<br>`
        + `Satellite, fixed to the stars: ${xyz(p)}.<br>Satellite, riding on the Earth: ${xyz(q)}.`;
      view.render();
    };
    slider.addEventListener('input', () => show());
    for (const radio of holds) {
      radio.addEventListener('change', () => {
        // Keep the picture as it is at the moment of the switch.
        const theta = sidereal(t) / DEG;
        view.yaw += radio.value === 'earth' ? -theta : theta;
        hold = radio.value;
        show();
      });
    }
    player($('spin-play'), slider, 30, show);
    show();
  };

  // ── Figure 4: where the dish stands, on a slightly flattened Earth ───────
  const figStation = () => {
    const latitude = $('station-lat'), factor = $('station-squash');
    const s = { latitude: CLEVEDON.latitude, factor: 1 };
    const shape = () => {
      const f = F_REAL * s.factor;
      return { f, e2: f * (2 - f), station: groundStation(s.latitude, CLEVEDON.longitude, CLEVEDON.altitude, f * (2 - f)) };
    };
    const view = makeView($('station-canvas'), {
      yaw: CLEVEDON.longitude - 64, pitch: 12, fit: [9700, 8900],
      draw: (v) => {
        const { f, e2, station } = shape();
        const p = station.position;
        const up = [station.cosLat * station.cosLon, station.cosLat * station.sinLon, station.sinLat];
        const east = [-station.sinLon, station.cosLon, 0], north = cross(up, east);
        const outward = unit(p), foot = [0, 0, -station.n * e2 * station.sinLat];
        v.placeEarth(IDENTITY, [0, 0, 0], 1 - f);
        v.globe({ greenwich: true });
        axes(v, 8100, ['x', 'y', 'z']);
        v.path(seg(foot, p), { color: C.el, width: 1.5, hidden: 0.75, hiddenDash: [2, 5] });
        v.path(seg([0, 0, 0], add(p, mul(outward, 2100))), { color: C.muted, width: 1.5, dash: [6, 5], hidden: 0.75 });
        legs(v, p);
        v.fill(sweep(48, (t) => add(p, add(mul(east, 900 * cos(t * TAU)), mul(north, 900 * sin(t * TAU))))), 'rgba(36,91,206,.22)');
        v.arrow(p, add(p, mul(up, 2100)), { color: C.el, width: 2.75 });
        v.label(add(p, mul(up, 2100)), 'up', { dx: 10, dy: -8, color: C.el, weight: 'bold' });
        if (s.factor > 8) v.label(add(p, mul(outward, 2100)), v.small ? 'from centre' : 'away from the centre', { dx: 10, dy: 10, color: C.muted, size: 13 });
        v.dot([0, 0, 0], 3.5, C.navy);
        v.dot(foot, 3.5, C.el);
        v.dot(p, 5.5, C.navy);
        v.label(p, 'dish', { dx: -10, dy: 14, align: 'right', weight: 'bold' });
      },
    });
    const show = () => {
      const { e2, station } = shape();
      const p = station.position;
      const up = [station.cosLat * station.cosLon, station.cosLat * station.sinLon, station.sinLat];
      const lean = Math.acos(min(1, dot(up, unit(p)))) / DEG;
      say('station-lat', `${number(s.latitude, 1)}°`);
      say('station-squash', s.factor === 1 ? 'the real Earth' : `${s.factor}× the real flattening`);
      $('station-out').innerHTML = `x = <strong>${number(p[0])}</strong> km, y = <strong>${number(p[1])}</strong> km, z = <strong>${number(p[2])}</strong> km`
        + `${s.factor === 1 ? '' : ' on this exaggerated Earth'}. `
        + `${colour('el', 'Up')} leans <strong>${lean.toFixed(2)}°</strong> from the line through the centre, `
        + `and meets Earth’s axis ${number(abs(station.n * e2 * station.sinLat))} km ${s.latitude < 0 ? 'above' : 'below'} the centre.`;
      view.render();
    };
    latitude.addEventListener('input', () => {
      s.latitude = Number(latitude.value);
      show();
    });
    factor.addEventListener('input', () => {
      s.factor = Number(factor.value);
      show();
    });
    $('station-reset').addEventListener('click', () => {
      s.latitude = CLEVEDON.latitude;
      s.factor = 1;
      latitude.value = s.latitude;
      factor.value = 1;
      show();
    });
    show();
  };

  // Figures 5 and 6 share the minutes around the example pass.
  const WINDOW = [floor((PASS.aos - 300) / 60) * 60, Math.ceil((PASS.los + 300) / 60) * 60];
  const passSlider = (slider) => {
    slider.max = WINDOW[1] - WINDOW[0];
    slider.value = round((PASS.peakTime - 100 - WINDOW[0]) / 5) * 5;
  };

  // ── Figure 5: subtract the dish from the satellite ───────────────────────
  const figArrow = () => {
    const slider = $('arrow-time');
    passSlider(slider);
    const riding = (unixS) => rotZ(propagate(ORBIT, minutesAt(unixS)), -sidereal(unixS));
    const track = sweep(120, (k) => riding(WINDOW[0] - 500 + k * (WINDOW[1] - WINDOW[0] + 1000)));
    let t = 0;
    const view = makeView($('arrow-canvas'), {
      yaw: -35, pitch: -15, fit: [4200, 3600], centre: mul(STATION.position, 0.58),
      draw: (v) => {
        const sat = riding(t), dish = STATION.position;
        v.placeEarth(IDENTITY);
        v.globe({ greenwich: true });
        axes(v, 2200, ['x', 'y', 'z']);
        v.path(track, { color: C.sat, width: 1.25, alpha: 0.6 });
        v.path(seg([0, 0, 0], dish), { color: C.navy, width: 2, hidden: 0.7, hiddenDash: [2, 5] });
        v.path(seg([0, 0, 0], sat), { color: C.sat, width: 2, hidden: 0.7, hiddenDash: [2, 5] });
        v.arrow(dish, sat, { color: C.sat, width: 3.5 });
        v.dot([0, 0, 0], 3.5, C.navy);
        v.dot(dish, 5.5, C.navy);
        v.label(dish, 'dish', { dx: -10, dy: 14, align: 'right', weight: 'bold' });
        v.dot(sat, 6.5, C.sat);
        v.label(sat, 'METOP-B', { dx: 12, dy: -12, color: C.sat, weight: 'bold' });
      },
    });
    const show = (value = Number(slider.value)) => {
      t = WINDOW[0] + value;
      const sat = riding(t), dish = STATION.position, arrow = lookFrom(t).arrow;
      say('arrow-time', `${clock(t, true)} UTC`);
      [sat, dish, arrow].forEach((row, r) => row.forEach((value, c) => {
        $(`arrow-cell-${r}${c}`).textContent = number(value);
      }));
      $('arrow-length').textContent = number(Math.hypot(...arrow));
      view.render();
    };
    slider.addEventListener('input', () => show());
    player($('arrow-play'), slider, 45, show);
    show();
  };

  // ── Figure 6: the arrow measured along east, north and up ────────────────
  const figLocal = () => {
    const slider = $('local-time');
    passSlider(slider);
    const east = [-STATION.sinLon, STATION.cosLon, 0];
    const north = [-STATION.sinLat * STATION.cosLon, -STATION.sinLat * STATION.sinLon, STATION.cosLat];
    const up = [STATION.cosLat * STATION.cosLon, STATION.cosLat * STATION.sinLon, STATION.sinLat];
    const rulers = [east, north, up], centre = mul(apply(rulers, STATION.position), -1);
    const local = (unixS) => {
      const look = lookFrom(unixS);
      return [look.east, look.north, look.up];
    };
    const track = sweep(160, (k) => local(WINDOW[0] - 240 + k * (WINDOW[1] - WINDOW[0] + 480)));
    const above = track.filter((p) => p[2] >= 0);
    const SHEET = 3300;
    let t = 0;
    const view = makeView($('local-canvas'), {
      yaw: 252, pitch: 26, fit: [3750, 2500], centre: [0, 0, 450], pitchRange: [6, 50],
      draw: (v) => {
        const look = lookFrom(t), sat = [look.east, look.north, look.up], shadow = [look.east, look.north, 0];
        const az = look.azimuth * DEG, el = look.elevation * DEG, risen = look.up >= 0;
        const bearing = (radius, angle) => [radius * sin(angle), radius * cos(angle), 0];
        const sight = () => {
          v.path(seg(shadow, sat, 8), { color: C.el, width: 1.5, dash: [4, 4], hidden: 0 });
          v.arrow([0, 0, 0], sat, { color: C.sat, width: 3.5, hidden: 0 });
          v.dot(sat, 6.5, C.sat);
          v.label(sat, 'METOP-B', { dx: 12, dy: -12, color: C.sat, weight: 'bold' });
        };
        v.placeEarth(rulers, centre);
        v.globe();
        // Below the horizon is drawn first, so the sheet tints it.
        v.path(track, { color: C.sat, width: 1.25, alpha: 0.7, hidden: 0 });
        if (!risen) sight();
        v.fill(sweep(96, (k) => bearing(SHEET, k * TAU)), 'rgba(36,91,206,.13)');
        v.path(sweep(96, (k) => bearing(SHEET, k * TAU)), { color: C.az, width: 1.25, alpha: 0.7, hidden: 1 });
        for (const [name, angle] of [['N', 0], ['E', 90], ['S', 180], ['W', 270]]) {
          v.label(bearing(SHEET + 260, angle * DEG), name, { align: 'center', color: C.az, weight: 'bold', alpha: 1 });
        }
        v.arrow([0, 0, 0], [0, 1500, 0], { color: C.az, width: 1.5, hidden: 1 });
        v.arrow([0, 0, 0], [1500, 0, 0], { color: C.az, width: 1.5, hidden: 1 });
        v.arrow([0, 0, 0], [0, 0, 1500], { color: C.el, width: 1.5, hidden: 1 });
        if (!v.small) {
          v.label([0, 1500, 0], 'north', { dx: 8, dy: -10, color: C.az, size: 13, alpha: 1 });
          v.label([1500, 0, 0], 'east', { dx: 8, dy: -10, color: C.az, size: 13, alpha: 1 });
        }
        v.label([0, 0, 1500], 'up', { dx: 8, dy: -6, color: C.el, size: 13, alpha: 1 });
        // East leg, north leg, then the arrow's shadow on the ground.
        v.path([[0, 0, 0], [look.east, 0, 0], shadow], { color: C.az, width: 3, alpha: 0.45, hidden: 1 });
        v.path([[0, 0, 0], shadow], { color: C.az, width: 2.5, hidden: 1 });
        v.path(sweep(48, (k) => bearing(700, k * az)), { color: C.az, width: 2.5, hidden: 1 });
        v.path(sweep(24, (k) => add(mul(bearing(700, az), cos(k * el)), [0, 0, 700 * sin(k * el)])), { color: C.el, width: 2.5, hidden: 1 });
        v.path(above, { color: C.sat, width: 2.5, hidden: 0 });
        if (risen) sight();
        v.dot([0, 0, 0], 5.5, C.navy);
      },
    });
    const show = (value = Number(slider.value)) => {
      t = WINDOW[0] + value;
      const look = lookFrom(t);
      const way = (value, plus, minus) => `${number(abs(value))} km ${value < 0 ? minus : plus}`;
      say('local-time', `${clock(t, true)} UTC`);
      $('local-out').innerHTML = `From the dish, the satellite is ${colour('az', way(look.east, 'east', 'west'))}, `
        + `${colour('az', way(look.north, 'north', 'south'))} and ${colour('el', way(look.up, 'up', 'down'))}.<br>`
        + `${colour('az', `Azimuth ${look.azimuth.toFixed(1)}°`)}, ${colour('el', `elevation ${number(look.elevation, 1)}°`)}`
        + `${look.elevation < 0 ? ': below the horizon' : ''}.`;
      view.render();
    };
    slider.addEventListener('input', () => show());
    player($('local-play'), slider, 45, show);
    show();
  };

  // ── Figure 7: elevation through twelve hours ─────────────────────────────
  const figDay = () => {
    const canvas = $('day-canvas'), ctx = canvas.getContext('2d'), slider = $('day-time');
    const STEP = 30; // passes.rs samples every 30 s
    const samples = sweep(HORIZON_S / STEP, (k) => lookFrom(NOON + k * HORIZON_S).elevation);
    let t = round(PASS.peakTime / STEP) * STEP, box = null;
    const render = () => {
      const w = canvas.clientWidth, h = canvas.clientHeight, dpr = window.devicePixelRatio || 1;
      if (!w || !h) return;
      canvas.width = round(w * dpr);
      canvas.height = round(h * dpr);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      const left = 42, top = 22, right = w - 12, bottom = h - 30;
      box = { left, right };
      const x = (unixS) => left + (unixS - NOON) / HORIZON_S * (right - left);
      const y = (degrees) => top + (90 - degrees) / 180 * (bottom - top);
      const text = (string, px, py, align = 'left', color = C.muted, weight = '') => {
        ctx.font = `${weight} 13px ${FONT}`;
        ctx.textAlign = align;
        ctx.textBaseline = 'middle';
        ctx.fillStyle = color;
        ctx.fillText(string, px, py);
      };
      ctx.fillStyle = '#edf1f5';
      ctx.fillRect(left, y(0), right - left, bottom - y(0));
      if (w >= 520) text('below the horizon', left + 8, bottom - 12);
      ctx.lineWidth = 1;
      ctx.strokeStyle = C.grid;
      ctx.beginPath();
      for (let hour = 0; hour <= 12; hour += w < 520 ? 3 : 2) {
        ctx.moveTo(x(NOON + hour * 3600), top);
        ctx.lineTo(x(NOON + hour * 3600), bottom);
        text(`${String((12 + hour) % 24 || 24).padStart(2, '0')}:00`, x(NOON + hour * 3600), bottom + 14, hour === 12 ? 'right' : hour ? 'center' : 'left');
      }
      for (const degrees of [-90, -45, 45, 90]) {
        ctx.moveTo(left, y(degrees));
        ctx.lineTo(right, y(degrees));
        text(`${number(degrees)}°`, left - 6, y(degrees), 'right');
      }
      ctx.stroke();
      text('0°', left - 6, y(0), 'right', C.navy, 'bold');
      ctx.strokeStyle = C.navy;
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.moveTo(left, y(0));
      ctx.lineTo(right, y(0));
      ctx.stroke();
      // The listed passes: the part of each bump above the 5° line.
      for (const pass of PASSES) {
        ctx.beginPath();
        ctx.moveTo(x(pass.aos), y(MASK_DEG));
        for (let u = pass.aos; u < pass.los; u += 10) ctx.lineTo(x(u), y(lookFrom(u).elevation));
        ctx.lineTo(x(pass.los), y(MASK_DEG));
        ctx.closePath();
        ctx.fillStyle = C.el;
        ctx.fill();
        text(`${pass.peak.toFixed(0)}°`, x(pass.peakTime), y(pass.peak) - 11, 'center', C.el, 'bold');
      }
      ctx.strokeStyle = C.warn;
      ctx.setLineDash([5, 4]);
      ctx.lineWidth = 1.25;
      ctx.beginPath();
      ctx.moveTo(left, y(MASK_DEG));
      ctx.lineTo(right, y(MASK_DEG));
      ctx.stroke();
      ctx.setLineDash([]);
      text('5°', right - 4, y(MASK_DEG) - 9, 'right', C.warn, 'bold');
      ctx.strokeStyle = C.el;
      ctx.lineWidth = 1.75;
      ctx.lineJoin = 'round';
      ctx.beginPath();
      samples.forEach((degrees, k) => (k ? ctx.lineTo(x(NOON + k * STEP), y(degrees)) : ctx.moveTo(x(NOON), y(degrees))));
      ctx.stroke();
      // The moment being read.
      const now = lookFrom(t).elevation;
      ctx.strokeStyle = C.navy;
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(x(t), top);
      ctx.lineTo(x(t), bottom);
      ctx.stroke();
      ctx.beginPath();
      ctx.arc(x(t), y(now), 5.5, 0, TAU);
      ctx.fillStyle = C.sat;
      ctx.fill();
      ctx.strokeStyle = '#fff';
      ctx.lineWidth = 1.5;
      ctx.stroke();
    };
    const show = () => {
      const look = lookFrom(t);
      const pass = PASSES.find((p) => t >= p.aos && t <= p.los);
      say('day-time', `${clock(t, true)} UTC`);
      $('day-out').innerHTML = `${colour('el', `Elevation ${number(look.elevation, 1)}°`)}: `
        + (pass ? `inside the pass from ${clock(pass.aos, true)} to ${clock(pass.los, true)}, which peaks at ${pass.peak.toFixed(1)}°.`
          : look.elevation < 0 ? 'the Earth is in the way.'
            : look.elevation < MASK_DEG ? 'above the horizon, but under the 5° line.' : 'above 5°, but this bump never reaches 10°, so it is not listed.');
      render();
    };
    slider.max = HORIZON_S;
    slider.value = t - NOON;
    slider.addEventListener('input', () => {
      t = NOON + Number(slider.value);
      show();
    });
    const point = (event) => {
      if (!box || !(event.buttons & 1)) return;
      const fraction = clamp((event.clientX - canvas.getBoundingClientRect().left - box.left) / (box.right - box.left), 0, 1);
      t = NOON + round(fraction * HORIZON_S / STEP) * STEP;
      slider.value = t - NOON;
      show();
    };
    canvas.addEventListener('pointerdown', (event) => {
      point(event);
      canvas.setPointerCapture(event.pointerId);
    });
    canvas.addEventListener('pointermove', point);
    new ResizeObserver(render).observe(canvas);
    show();
  };

  // ── Figure 8: one pass across the sky ────────────────────────────────────
  const figDome = () => {
    const slider = $('dome-time'), select = $('dome-pass');
    PASSES.forEach((pass, k) => {
      select.add(new Option(`${clock(pass.aos)} to ${clock(pass.los)} UTC, peak ${pass.peak.toFixed(0)}°`, k, pass === PASS, pass === PASS));
    });
    let pass = PASS, t = PASS.peakTime, track = [];
    // A direction as a point on a dome of radius 1: x east, y north, z up.
    const onDome = (azimuth, elevation, radius = 1) => [
      radius * cos(elevation * DEG) * sin(azimuth * DEG), radius * cos(elevation * DEG) * cos(azimuth * DEG), radius * sin(elevation * DEG),
    ];
    const view = makeView($('dome-canvas'), {
      yaw: 252, pitch: 26, fit: [1.32, 1.02], centre: [0, 0, 0.3], pitchRange: [5, 55],
      draw: (v) => {
        const look = lookFrom(t), aim = onDome(look.azimuth, look.elevation), ground = onDome(look.azimuth, 0);
        const rim = sweep(120, (k) => onDome(k * 360, 0));
        v.fill(rim, '#edf4fa');
        v.path(rim, { color: C.navy, width: 1.5 });
        for (let bearing = 0; bearing < 360; bearing += 30) {
          v.path([onDome(bearing, 0, 0.94), onDome(bearing, 0)], { color: C.navy, width: bearing % 90 ? 1 : 2 });
        }
        for (const [name, bearing] of [['N', 0], ['E', 90], ['S', 180], ['W', 270]]) {
          v.label(onDome(bearing, 0, 1.12), name, { align: 'center', weight: 'bold' });
        }
        const side = v.right, over = cross(v.toward, side);
        v.path(sweep(60, (k) => add(mul(side, cos(k * PI)), mul(over, sin(k * PI)))), { color: C.navy, width: 1.25, alpha: 0.5 });
        for (const elevation of [30, 60]) v.path(sweep(90, (k) => onDome(k * 360, elevation)), { color: C.navy, width: 1, alpha: 0.22 });
        for (const bearing of [0, 90]) v.path(sweep(60, (k) => onDome(bearing, k * 180)), { color: C.navy, width: 1, alpha: 0.22 });
        v.path(sweep(120, (k) => onDome(k * 360, MASK_DEG)), { color: C.warn, width: 1.25, dash: [5, 4] });
        // Label the 5° ring on the side nearest the camera (compass bearing 90° − yaw).
        v.label(onDome(128 - v.yaw, MASK_DEG), '5°', { dy: -10, align: 'center', color: C.warn, size: 13 });

        v.path(track, { color: C.sat, width: 2.5 });
        v.dot(track[0], 3.5, C.sat);
        v.dot(track[track.length - 1], 3.5, C.sat);
        v.label(track[0], `rises ${clock(pass.aos)}`, { dy: -14, align: 'center', color: C.sat, size: 13 });
        v.label(track[track.length - 1], `sets ${clock(pass.los)}`, { dy: -14, align: 'center', color: C.sat, size: 13 });
        if (look.elevation < 0) return;
        v.path([[0, 0, 0], ground], { color: C.az, width: 2.5 });
        v.path([ground, aim], { color: C.el, width: 1.25, dash: [4, 4] });
        v.path(sweep(60, (k) => onDome(k * look.azimuth, 0, 0.34)), { color: C.az, width: 2.5 });
        v.path(sweep(30, (k) => onDome(look.azimuth, k * look.elevation, 0.5)), { color: C.el, width: 2.5 });
        v.arrow([0, 0, 0], aim, { color: C.sat, width: 3.5 });
        v.dot(aim, 6.5, C.sat);
        v.dot([0, 0, 0], 5.5, C.navy);
      },
    });
    const show = (value = Number(slider.value)) => {
      t = pass.aos + value;
      const look = lookFrom(t);
      say('dome-time', `${clock(t, true)} UTC`);
      $('dome-out').innerHTML = `${colour('az', `Azimuth ${look.azimuth.toFixed(1)}°`)}, ${colour('el', `elevation ${number(look.elevation, 1)}°`)}.`;
      view.render();
    };
    const choose = () => {
      pass = PASSES[Number(select.value)];
      track = sweep(120, (k) => {
        const look = lookFrom(pass.aos + k * (pass.los - pass.aos));
        return onDome(look.azimuth, look.elevation);
      });
      slider.max = pass.los - pass.aos;
      slider.value = round((pass.peakTime - pass.aos) / 5) * 5;
      show();
    };
    select.addEventListener('change', choose);
    slider.addEventListener('input', () => show());
    player($('dome-play'), slider, 45, show);
    choose();
  };

  for (const figure of [figRing, figTime, figSpin, figStation, figArrow, figLocal, figDay, figDome]) figure();
})();
