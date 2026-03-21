use param_serv::{connect, param_list, param_set};
use std::io::BufReader;
use std::thread;
use std::time::Duration;

// ---- RNG --------------------------------------------------------------------

struct Xorshift64(u64);

impl Xorshift64 {
    fn next_f64(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
}

// ---- Main -------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let filter: Vec<&str> =
        args[1..].iter().map(String::as_str).collect();

    let stream = connect();
    let mut w = stream.try_clone().expect("clone");
    let mut r = BufReader::new(stream);

    let names: Vec<String> = param_list(&mut w, &mut r)
        .expect("param_list")
        .into_iter()
        .filter(|n| filter.is_empty() || filter.iter().any(|f| f == n))
        .collect();

    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos() as u64
        | 1;
    let mut rng = Xorshift64(seed);

    let mut updates: Vec<(&str, f64)> = Vec::with_capacity(names.len());

    loop {
        updates.clear();
        updates
            .extend(names.iter().map(|n| (n.as_str(), rng.next_f64())));

        if param_set(&mut w, &mut r, &updates).is_err() {
            break;
        }

        thread::sleep(Duration::from_nanos(1_000_000_000 / 60));
    }
}
