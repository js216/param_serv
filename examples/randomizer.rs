use param_serv::Connection;
use std::thread::sleep;
use std::time::Duration;

fn xorshift(x: &mut u64) -> f64 {
    *x ^= *x << 13;
    *x ^= *x >> 7;
    *x ^= *x << 17;
    (*x >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
}

fn main() -> std::io::Result<()> {
    let mut c = Connection::new()?;
    let names = c.list()?;
    let mut rng = 1;

    loop {
        let vals: Vec<String> = names.iter()
            .map(|_| format!("{:.6}", xorshift(&mut rng)))
            .collect();
        let updates: Vec<(&str, &str)> = names.iter()
            .zip(vals.iter())
            .map(|(n, v)| (n.name.as_str(), v.as_str()))
            .collect();

        c.set(&updates)?;
        sleep(Duration::from_nanos(1_000_000_000 / 60));
    }
}
