// Demo data generator for SR835 simulator.
// Connects to param_serv and writes fake measurement data.
//
// Usage: cargo run --example demo
// Requires param_serv to be running: param_serv ../params.txt

use std::time::Duration;

/// BW_3dB factor for a synchronous FIR (rectangular window / sinc response).
/// The −3 dB point of sinc(π f T) is at f ≈ 0.886 / T.
const SINC_BW3DB_FACTOR: f64 = 0.886;

/// Given integration time (seconds) and detection frequency (Hz),
/// compute all four filter representations and push updates.
/// Synchronous FIR (rectangular window over N reference cycles):
///   T_int = n_cycles / f_det
///   ENBW  = 1 / T_int  (= f_det / n_cycles)
///   BW_3dB ≈ 0.886 / T_int
/// Push filter updates, skipping the parameter that triggered the recomputation
/// to avoid overwriting the user's keypad entry.
/// `skip`: 0=cycles, 1=enbw, 2=tint, 3=bw3db, -1=skip none (freq/harmonic change)
fn push_filter_updates(
    updates: &mut Vec<(&'static str, String)>,
    prefix_cycles: &'static str,
    prefix_enbw: &'static str,
    prefix_tint: &'static str,
    prefix_bw3db: &'static str,
    t_int: f64,
    f_det: f64,
    skip: i32,
) {
    let n_cycles = t_int * f_det;
    let enbw = 1.0 / t_int;
    let bw3db = SINC_BW3DB_FACTOR / t_int;
    if skip != 0 { updates.push((prefix_cycles, format!("{}", n_cycles.round() as i64))); }
    if skip != 1 { updates.push((prefix_enbw, format!("{}", enbw))); }
    if skip != 2 { updates.push((prefix_tint, format!("{}", t_int))); }
    if skip != 3 { updates.push((prefix_bw3db, format!("{}", bw3db))); }
}

fn pseudo_random(seed: &mut u64) -> f64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    (*seed >> 33) as f64 / (1u64 << 31) as f64 - 0.5  // range [-0.5, 0.5)
}

struct AutoPhaseState {
    running: bool,
    start_time: f64,
    duration: f64,
    target_phase: f64,
}

impl AutoPhaseState {
    fn new() -> Self {
        Self { running: false, start_time: 0.0, duration: 0.5, target_phase: 0.0 }
    }
}

fn main() {
    let mut conn = loop {
        match param_serv::Connection::new() {
            Ok(c) => break c,
            Err(_) => {
                eprintln!("waiting for param_serv...");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    };
    eprintln!("connected to param_serv");

    // Initial values
    let init: Vec<(&str, &str)> = vec![
        ("ref_source", "0"),
        ("ref_frequency", "1000"),
        ("src_amplitude", "1.0"),
        ("src_mode", "0"),
        ("dem1_input", "0"),
        ("dem1_input_mode", "0"), ("dem1_coupling", "0"),
        ("dem1_harmonic", "1"), ("dem1_gain", "0"),
        ("dem1_time_constant", "1 \u{00B5}s"),
        ("dem1_filter_display", "0"), ("dem1_filter_cycles", "10000"),
        ("dem1_upper_qty", "0"), ("dem1_lower_qty", "1"),
        ("dem2_input", "1"),
        ("dem2_input_mode", "0"), ("dem2_coupling", "0"),
        ("dem2_harmonic", "1"), ("dem2_gain", "0"),
        ("dem2_time_constant", "1 \u{00B5}s"),
        ("dem2_filter_display", "0"), ("dem2_filter_cycles", "10000"),
        ("dem2_upper_qty", "0"), ("dem2_lower_qty", "1"),
        ("led_status_com", "0"), ("led_status_err", "0"),
        ("led_status_ovld", "0"), ("led_status_trip", "0"),
        ("status", ""),
    ];
    let _ = conn.set(&init);

    let mut t: f64 = 0.0;
    let mut rng_seed: u64 = 12345;
    let mut internal_freq: f64 = 1000.0;
    let mut is_external = false;
    let mut dem1_gain: f64 = 0.0;
    let mut dem2_gain: f64 = 0.0;
    let mut dem1_input: usize = 0;
    let mut dem2_input: usize = 1;
    let mut dem1_harmonic: f64 = 1.0;
    let mut dem2_harmonic: f64 = 1.0;
    let mut dem1_filter_cycles: f64 = 10000.0;
    let mut dem2_filter_cycles: f64 = 10000.0;
    let mut dem1_filter_t_int: f64 = 10000.0 / 1_000_000.0; // cycles / det_freq
    let mut dem2_filter_t_int: f64 = 10000.0 / 1_000_000.0;
    let mut dem1_filter_display: usize = 0; // 0=cycles, 1=enbw, 2=tint, 3=bw3db
    let mut dem2_filter_display: usize = 0;
    // Track last values written by demo.rs to avoid feedback loops
    let mut last_written: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut dem1_auto_phase = AutoPhaseState::new();
    let mut dem2_auto_phase = AutoPhaseState::new();
    let mut dem1_phase: f64 = 0.0;
    let mut dem2_phase: f64 = 0.0;
    // Auto-gain state: 0=Single, 1=Persistent, 2=Off
    let mut dem1_auto_gain: usize = 2;
    let mut dem2_auto_gain: usize = 2;
    let mut dem1_gain_inc_time: f64 = 5.0;
    let mut dem1_gain_dec_time: f64 = 1.0;
    let mut dem2_gain_inc_time: f64 = 5.0;
    let mut dem2_gain_dec_time: f64 = 1.0;
    // Track how long signal has been too low/high for persistent auto-gain
    let mut dem1_too_low_since: Option<f64> = None;
    let mut dem1_too_high_since: Option<f64> = None;
    let mut dem2_too_low_since: Option<f64> = None;
    let mut dem2_too_high_since: Option<f64> = None;

    loop {
        t += 0.033;

        // Read changed params
        let current = conn.get().unwrap_or_default();
        for (n, v) in &current {
            match n.as_str() {
                "ref_source" => is_external = v != "0",
                "ref_frequency" if !is_external => {
                    if let Ok(f) = v.parse::<f64>() { internal_freq = f; }
                }
                "dem1_gain" => { if let Ok(g) = v.parse::<f64>() { dem1_gain = g; } }
                "dem2_gain" => { if let Ok(g) = v.parse::<f64>() { dem2_gain = g; } }
                "dem1_input" => { if let Ok(i) = v.parse::<usize>() { dem1_input = i; } }
                "dem2_input" => { if let Ok(i) = v.parse::<usize>() { dem2_input = i; } }
                "dem1_harmonic" => { if let Ok(h) = v.parse::<f64>() { dem1_harmonic = h; } }
                "dem2_harmonic" => { if let Ok(h) = v.parse::<f64>() { dem2_harmonic = h; } }
                "dem1_phase" => { if let Ok(p) = v.parse::<f64>() { dem1_phase = p; } }
                "dem2_phase" => { if let Ok(p) = v.parse::<f64>() { dem2_phase = p; } }
                "dem1_auto_gain" => { if let Ok(a) = v.parse::<usize>() { dem1_auto_gain = a; } }
                "dem2_auto_gain" => { if let Ok(a) = v.parse::<usize>() { dem2_auto_gain = a; } }
                "dem1_gain_inc_time" => { if let Ok(t) = v.parse::<f64>() { dem1_gain_inc_time = t; } }
                "dem1_gain_dec_time" => { if let Ok(t) = v.parse::<f64>() { dem1_gain_dec_time = t; } }
                "dem2_gain_inc_time" => { if let Ok(t) = v.parse::<f64>() { dem2_gain_inc_time = t; } }
                "dem2_gain_dec_time" => { if let Ok(t) = v.parse::<f64>() { dem2_gain_dec_time = t; } }
                "dem1_filter_cycles" => { if let Ok(c) = v.parse::<f64>() { dem1_filter_cycles = c; } }
                "dem2_filter_cycles" => { if let Ok(c) = v.parse::<f64>() { dem2_filter_cycles = c; } }
                "dem1_filter_display" => { if let Ok(d) = v.parse::<usize>() { dem1_filter_display = d.min(3); } }
                "dem2_filter_display" => { if let Ok(d) = v.parse::<usize>() { dem2_filter_display = d.min(3); } }
                "dem1_auto_phase" => {
                    if v == "start" && !dem1_auto_phase.running {
                        let f_ref_hz = internal_freq * 1000.0;
                        dem1_auto_phase.running = true;
                        dem1_auto_phase.start_time = t;
                        dem1_auto_phase.duration = if f_ref_hz >= 2.0 { 0.5 } else { 1.0 / f_ref_hz };
                        let _ = conn.set(&[("dem1_auto_phase", "running")]);
                    } else if v == "cancel" {
                        dem1_auto_phase.running = false;
                        let _ = conn.set(&[("dem1_auto_phase", "idle"), ("dem1_auto_phase_progress", "0")]);
                    }
                }
                "dem2_auto_phase" => {
                    if v == "start" && !dem2_auto_phase.running {
                        let f_ref_hz = internal_freq * 1000.0;
                        dem2_auto_phase.running = true;
                        dem2_auto_phase.start_time = t;
                        dem2_auto_phase.duration = if f_ref_hz >= 2.0 { 0.5 } else { 1.0 / f_ref_hz };
                        let _ = conn.set(&[("dem2_auto_phase", "running")]);
                    } else if v == "cancel" {
                        dem2_auto_phase.running = false;
                        let _ = conn.set(&[("dem2_auto_phase", "idle"), ("dem2_auto_phase_progress", "0")]);
                    }
                }
                _ => {}
            }
        }

        // Filter parameter conversions: when one changes, recompute the others.
        // Each demod has filter_cycles, filter_enbw, filter_tint, filter_bw3db.
        // All represent the same underlying integration time.
        // Also recompute when ref_frequency or harmonic changes (n_cycles stays
        // fixed but T_int/ENBW/BW_3dB change with the detection frequency).
        {
            let f_ref_hz = internal_freq * 1000.0; // ref_frequency is in kHz
            let freq_changed = current.iter().any(|(n, _)| n == "ref_frequency");
            let dem1_harm_changed = current.iter().any(|(n, _)| n == "dem1_harmonic");
            let dem2_harm_changed = current.iter().any(|(n, _)| n == "dem2_harmonic");
            struct DemodFilter {
                harmonic: f64,
                cycles: f64,
                t_int: f64,
                display: usize,
                freq_dep_changed: bool,
                cycles_key: &'static str,
                enbw_key: &'static str,
                tint_key: &'static str,
                bw3db_key: &'static str,
            }
            let mut demods = [
                DemodFilter {
                    harmonic: dem1_harmonic, cycles: dem1_filter_cycles,
                    t_int: dem1_filter_t_int, display: dem1_filter_display,
                    freq_dep_changed: freq_changed || dem1_harm_changed,
                    cycles_key: "dem1_filter_cycles", enbw_key: "dem1_filter_enbw",
                    tint_key: "dem1_filter_tint", bw3db_key: "dem1_filter_bw3db",
                },
                DemodFilter {
                    harmonic: dem2_harmonic, cycles: dem2_filter_cycles,
                    t_int: dem2_filter_t_int, display: dem2_filter_display,
                    freq_dep_changed: freq_changed || dem2_harm_changed,
                    cycles_key: "dem2_filter_cycles", enbw_key: "dem2_filter_enbw",
                    tint_key: "dem2_filter_tint", bw3db_key: "dem2_filter_bw3db",
                },
            ];
            let mut filter_sets: Vec<(&str, String)> = Vec::new();
            for dm in &mut demods {
                let det_freq = f_ref_hz * dm.harmonic;
                if det_freq <= 0.0 { continue; }
                // The keypad only targets the displayed param, so only check
                // that one for user edits.  Checking all four in a priority
                // chain caused false positives: stale SSE echoes for a
                // non-displayed param would misfire first and overwrite the
                // param the user was actually editing.
                let (active_key, skip_idx) = match dm.display {
                    0 => (dm.cycles_key, 0i32),
                    1 => (dm.enbw_key, 1),
                    2 => (dm.tint_key, 2),
                    _ => (dm.bw3db_key, 3),
                };
                let latest = current.iter().rev()
                    .find(|(n, _)| n.as_str() == active_key)
                    .map(|(_, v)| v.clone());
                let user_edit = latest.as_ref().and_then(|v| {
                    if last_written.get(active_key).map(|lw| lw == v).unwrap_or(false) {
                        None
                    } else {
                        Some(v.clone())
                    }
                });
                let (t_int, skip) = if let Some(v) = user_edit {
                    match skip_idx {
                        0 => {
                            if let Ok(nc) = v.parse::<f64>() {
                                dm.cycles = nc; dm.t_int = nc / det_freq;
                                (dm.t_int, 0)
                            } else { continue; }
                        }
                        1 => {
                            if let Ok(enbw) = v.parse::<f64>() {
                                if enbw <= 0.0 { continue; }
                                let ti = 1.0 / enbw;
                                dm.t_int = ti; dm.cycles = ti * det_freq;
                                (ti, 1)
                            } else { continue; }
                        }
                        2 => {
                            if let Ok(ti) = v.parse::<f64>() {
                                dm.t_int = ti; dm.cycles = ti * det_freq;
                                (ti, 2)
                            } else { continue; }
                        }
                        _ => {
                            if let Ok(bw) = v.parse::<f64>() {
                                if bw <= 0.0 { continue; }
                                let ti = SINC_BW3DB_FACTOR / bw;
                                dm.t_int = ti; dm.cycles = ti * det_freq;
                                (ti, 3)
                            } else { continue; }
                        }
                    }
                } else if dm.freq_dep_changed || t < 0.05 {
                    // Ref frequency or harmonic changed: keep the active
                    // parameter constant and recompute the rest.
                    // display=0 (cycles): keep cycles, t_int changes
                    // display=1,2,3: keep t_int, cycles changes
                    if dm.display == 0 {
                        let ti = dm.cycles / det_freq;
                        dm.t_int = ti;
                        (ti, -1)
                    } else {
                        dm.cycles = dm.t_int * det_freq;
                        (dm.t_int, -1)
                    }
                } else {
                    continue;
                };
                if t_int <= 0.0 { continue; }
                push_filter_updates(
                    &mut filter_sets,
                    dm.cycles_key, dm.enbw_key, dm.tint_key, dm.bw3db_key,
                    t_int, det_freq, skip,
                );
                // Record the user's raw value for the skipped param so echo
                // detection won't keep re-triggering on subsequent ticks.
                if skip >= 0 {
                    if let Some(v) = latest {
                        last_written.insert(active_key.to_string(), v);
                    }
                }
            }
            // Write back updated state
            dem1_filter_cycles = demods[0].cycles;
            dem1_filter_t_int = demods[0].t_int;
            dem2_filter_cycles = demods[1].cycles;
            dem2_filter_t_int = demods[1].t_int;
            if !filter_sets.is_empty() {
                for (n, v) in &filter_sets {
                    last_written.insert(n.to_string(), v.clone());
                }
                let refs: Vec<(&str, &str)> = filter_sets.iter()
                    .map(|(n, v)| (*n, v.as_str())).collect();
                let _ = conn.set(&refs);
            }
        }

        // When external: jitter around the last known internal frequency
        if is_external {
            let jitter = pseudo_random(&mut rng_seed) * internal_freq * 0.01;
            let freq_s = (internal_freq + jitter).to_string();
            let _ = conn.set(&[("ref_frequency", &freq_s)]);
        }

        // Simulate per-channel signals in µV (200-300 µV range for R)
        let ch_signals: [(f64, f64, f64); 4] = [
            ((t).sin() * 200.0,            (t * 0.7).cos() * 200.0,       (t * 0.3).sin() * 5.0),
            ((t * 1.3 + 1.0).sin() * 150.0, (t * 0.9 + 2.0).cos() * 150.0, (t * 0.2).cos() * 3.0),
            ((t * 0.8).sin() * 250.0,      (t * 1.1).cos() * 250.0,       (t * 0.5).sin() * 10.0),
            ((t * 0.6 + 3.0).sin() * 180.0, (t * 1.4 + 1.0).cos() * 180.0, (t * 0.4).cos() * 7.0),
        ];

        // Demod 1 reads from its selected channel, rotated by user phase
        let (d1x_raw, d1y_raw, _) = ch_signals[dem1_input.min(3)];
        let d1_cos = dem1_phase.to_radians().cos();
        let d1_sin = dem1_phase.to_radians().sin();
        let d1x = d1x_raw * d1_cos + d1y_raw * d1_sin;
        let d1y = -d1x_raw * d1_sin + d1y_raw * d1_cos;
        let d1r = (d1x * d1x + d1y * d1y).sqrt();
        let d1t = d1y.atan2(d1x).to_degrees();

        // Demod 2 reads from its selected channel, rotated by user phase
        let (d2x_raw, d2y_raw, _) = ch_signals[dem2_input.min(3)];
        let d2_cos = dem2_phase.to_radians().cos();
        let d2_sin = dem2_phase.to_radians().sin();
        let d2x = d2x_raw * d2_cos + d2y_raw * d2_sin;
        let d2y = -d2x_raw * d2_sin + d2y_raw * d2_cos;
        let d2r = (d2x * d2x + d2y * d2y).sqrt();
        let d2t = d2y.atan2(d2x).to_degrees();

        let d1x_s = d1x.to_string();
        let d1y_s = d1y.to_string();
        let d1r_s = d1r.to_string();
        let d1t_s = d1t.to_string();
        let d2x_s = d2x.to_string();
        let d2y_s = d2y.to_string();
        let d2r_s = d2r.to_string();
        let d2t_s = d2t.to_string();

        // ADC level: signal amplitude (µV) * gain / full_scale (1V = 1e6 µV)
        // ADC full scale ~9000 µV: gain=4 (16x) with R≈280 µV → ~0.5
        let d1_adc = (d1r * 2f64.powf(dem1_gain) / 9000.0).min(1.0);
        let d2_adc = (d2r * 2f64.powf(dem2_gain) / 9000.0).min(1.0);
        let d1_adc_s = d1_adc.to_string();
        let d2_adc_s = d2_adc.to_string();

        // Auto-gain processing
        // Target: ADC level ~0.5 (halfway). Too low: <0.15, too high: >0.85
        let mut gain_sets: Vec<(&str, String)> = Vec::new();
        for (adc, gain, auto_mode, inc_t, dec_t, too_low, too_high, gain_key, mode_key) in [
            (d1_adc, &mut dem1_gain, &mut dem1_auto_gain, dem1_gain_inc_time, dem1_gain_dec_time,
             &mut dem1_too_low_since, &mut dem1_too_high_since, "dem1_gain", "dem1_auto_gain"),
            (d2_adc, &mut dem2_gain, &mut dem2_auto_gain, dem2_gain_inc_time, dem2_gain_dec_time,
             &mut dem2_too_low_since, &mut dem2_too_high_since, "dem2_gain", "dem2_auto_gain"),
        ] {
            if *auto_mode == 0 {
                // Single: pick best gain for ~0.5 ADC reading
                // Current ADC = R * 2^gain / 9000, want ADC ≈ 0.5
                // So ideal gain_exp = log2(0.5 * 9000 / R)
                let r = adc * 9000.0 / 2f64.powf(*gain); // recover R
                if r > 0.0 {
                    let ideal = (0.5 * 9000.0 / r).log2();
                    let new_gain = ideal.round().clamp(0.0, 8.0);
                    *gain = new_gain;
                    gain_sets.push((gain_key, format!("{}", new_gain as i32)));
                }
                *auto_mode = 2; // back to Off after single shot
                gain_sets.push((mode_key, "2".to_owned()));
            } else if *auto_mode == 1 {
                // Persistent: adjust after timing thresholds
                if adc < 0.15 {
                    *too_high = None;
                    if too_low.is_none() { *too_low = Some(t); }
                    if t - too_low.unwrap() >= inc_t && *gain < 8.0 {
                        *gain += 1.0;
                        gain_sets.push((gain_key, format!("{}", *gain as i32)));
                        *too_low = None;
                    }
                } else if adc > 0.85 {
                    *too_low = None;
                    if too_high.is_none() { *too_high = Some(t); }
                    if t - too_high.unwrap() >= dec_t && *gain > 0.0 {
                        *gain -= 1.0;
                        gain_sets.push((gain_key, format!("{}", *gain as i32)));
                        *too_high = None;
                    }
                } else {
                    *too_low = None;
                    *too_high = None;
                }
            }
        }
        if !gain_sets.is_empty() {
            let refs: Vec<(&str, &str)> = gain_sets.iter()
                .map(|(n, v)| (*n, v.as_str())).collect();
            let _ = conn.set(&refs);
        }

        // Auto-phase processing
        let mut auto_sets: Vec<(&str, String)> = Vec::new();
        // Compute target phase from raw (unrotated) signal: angle that zeros Y
        if dem1_auto_phase.running {
            dem1_auto_phase.target_phase = d1y_raw.atan2(d1x_raw).to_degrees();
        }
        if dem2_auto_phase.running {
            dem2_auto_phase.target_phase = d2y_raw.atan2(d2x_raw).to_degrees();
        }
        for (ap, phase_val, phase_key, progress_key, state_key) in [
            (&mut dem1_auto_phase, &mut dem1_phase, "dem1_phase", "dem1_auto_phase_progress", "dem1_auto_phase"),
            (&mut dem2_auto_phase, &mut dem2_phase, "dem2_phase", "dem2_auto_phase_progress", "dem2_auto_phase"),
        ] {
            if ap.running {
                let elapsed = t - ap.start_time;
                let progress = (elapsed / ap.duration).min(1.0);
                auto_sets.push((progress_key, format!("{}", progress)));
                if progress >= 1.0 {
                    ap.running = false;
                    *phase_val = ap.target_phase;
                    auto_sets.push((phase_key, format!("{}", ap.target_phase)));
                    auto_sets.push((state_key, "idle".to_owned()));
                    auto_sets.push((progress_key, "0".to_owned()));
                }
            }
        }
        if !auto_sets.is_empty() {
            let refs: Vec<(&str, &str)> = auto_sets.iter()
                .map(|(n, v)| (*n, v.as_str())).collect();
            let _ = conn.set(&refs);
        }

        let updates: Vec<(&str, &str)> = vec![
            ("dem1_x", &d1x_s), ("dem1_y", &d1y_s),
            ("dem1_r", &d1r_s), ("dem1_theta", &d1t_s),
            ("dem1_adc_level", &d1_adc_s),
            ("dem2_x", &d2x_s), ("dem2_y", &d2y_s),
            ("dem2_r", &d2r_s), ("dem2_theta", &d2t_s),
            ("dem2_adc_level", &d2_adc_s),
        ];
        if conn.set(&updates).is_err() {
            eprintln!("param_serv disconnected");
            return;
        }

        std::thread::sleep(Duration::from_millis(33));
    }
}
