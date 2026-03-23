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
        let updates: Vec<(&str, f64)> = names.iter()
            .map(|n| (n.as_str(), xorshift(&mut rng)))
            .collect();

        c.set(&updates)?;
        sleep(Duration::from_nanos(1_000_000_000 / 60));
    }
}
