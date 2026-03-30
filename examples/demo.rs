// Demo data generator for SR835 simulator.
// Connects to param_serv and writes fake measurement data.
//
// Usage: cargo run --example demo
// Requires param_serv to be running: param_serv ../params.txt

use std::time::Duration;

fn pseudo_random(seed: &mut u64) -> f64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    (*seed >> 33) as f64 / (1u64 << 31) as f64 - 0.5  // range [-0.5, 0.5)
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
        ("dem1_time_constant", "1 \u{00B5}s"), ("dem1_filter_slope", "3"),
        ("dem1_upper_qty", "0"), ("dem1_lower_qty", "1"),
        ("dem2_input", "1"),
        ("dem2_input_mode", "0"), ("dem2_coupling", "0"),
        ("dem2_harmonic", "1"), ("dem2_gain", "0"),
        ("dem2_time_constant", "1 \u{00B5}s"), ("dem2_filter_slope", "3"),
        ("dem2_upper_qty", "0"), ("dem2_lower_qty", "1"),
        ("led_status_com", "0"), ("led_status_err", "0"),
        ("led_status_ovld", "0"), ("led_status_trip", "0"),
        ("status", "Ready."),
    ];
    let _ = conn.set(&init);

    let mut t: f64 = 0.0;
    let mut rng_seed: u64 = 12345;
    let mut internal_freq: f64 = 1000.0;
    let mut is_external = false;

    loop {
        t += 0.033;

        // Read changed params
        let current = conn.get().unwrap_or_default();
        for (n, v) in &current {
            match n.as_str() {
                "ref_source" => is_external = v == "1",
                "ref_frequency" if !is_external => {
                    if let Ok(f) = v.parse::<f64>() { internal_freq = f; }
                }
                _ => {}
            }
        }

        // When external: jitter around the last known internal frequency
        if is_external {
            let jitter = pseudo_random(&mut rng_seed) * internal_freq * 0.01;
            let freq_s = (internal_freq + jitter).to_string();
            let _ = conn.set(&[("ref_frequency", &freq_s)]);
        }

        let d1x = (t).sin() * 0.01;
        let d1y = (t * 0.7).cos() * 0.01;
        let d1r = (d1x * d1x + d1y * d1y).sqrt();
        let d1t = d1y.atan2(d1x).to_degrees();

        let d2x = (t * 1.3 + 1.0).sin() * 0.005;
        let d2y = (t * 0.9 + 2.0).cos() * 0.005;
        let d2r = (d2x * d2x + d2y * d2y).sqrt();
        let d2t = d2y.atan2(d2x).to_degrees();

        let d1_phase = ((t * 0.3).sin() * 5.0).to_string();
        let d1x_s = d1x.to_string();
        let d1y_s = d1y.to_string();
        let d1r_s = d1r.to_string();
        let d1t_s = d1t.to_string();
        let d2_phase = ((t * 0.2).cos() * 3.0).to_string();
        let d2x_s = d2x.to_string();
        let d2y_s = d2y.to_string();
        let d2r_s = d2r.to_string();
        let d2t_s = d2t.to_string();

        let updates: Vec<(&str, &str)> = vec![
            ("dem1_phase", &d1_phase),
            ("dem1_x", &d1x_s), ("dem1_y", &d1y_s),
            ("dem1_r", &d1r_s), ("dem1_theta", &d1t_s),
            ("dem2_phase", &d2_phase),
            ("dem2_x", &d2x_s), ("dem2_y", &d2y_s),
            ("dem2_r", &d2r_s), ("dem2_theta", &d2t_s),
        ];
        if conn.set(&updates).is_err() {
            eprintln!("param_serv disconnected");
            return;
        }

        std::thread::sleep(Duration::from_millis(33));
    }
}
